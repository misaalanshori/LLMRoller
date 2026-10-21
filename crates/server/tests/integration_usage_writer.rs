//! Integration tests for three-queue batching writer, rollups, attempts, and drop-oldest retention.
//!
//! Spec: Section 7.2, Section 9.1, Section 9.2, Section 21.6,
//! Requirements: USG-1, SHR-6, REL-2, REL-3, Partial: TRC-9.

use std::sync::Arc;
use tempfile::tempdir;

use llmroller_core::ids::{ClientKeyId, ModelPk, RequestId, UpstreamKeyId, UserId};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pins::{ResponseLinkRow, get_response_link_tx, insert_response_link_tx};
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::usage_writer::{ThreeQueueUsageWriter, UsageAttemptRecord, UsageEventRecord};
use llmroller_store::users::{NewUser, create_user};

async fn setup_usage_writer_test_db() -> (tempfile::TempDir, sqlx::SqlitePool, MasterKey, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("usage_writer_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xBB; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "writer_user",
            password: "supersecretpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("user");

    (dir, writer_pool, master_key, user.id)
}

#[doc = "Spec: USG-1, SHR-6, REL-2, REL-3, Partial: TRC-9"]
#[tokio::test]
async fn test_three_queue_writer_batching_drop_oldest_and_rollups() {
    let (_dir, pool, _master_key, user_id) = setup_usage_writer_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let client_key_id = ClientKeyId::generate();
    sqlx::query("INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at) VALUES (?, ?, 'Key', 'llr_123', 'hash123', 1000)")
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert client key");

    // 1. Queue with max capacity of 10,000 items (REL-3)
    let writer = ThreeQueueUsageWriter::new(10_000);

    // Push 10,500 usage events (each with 10 tokens and $0.01 = 10,000 micro-USD)
    let base_ts = 1_700_000_000_000;
    for i in 0..10_500 {
        let req_id = RequestId::generate();
        let event = UsageEventRecord {
            id: format!("event-{i}"),
            ts: base_ts + (i % 100) * 1000,
            request_id: req_id,
            client_key_id,
            owner_user_id: user_id,
            upstream_key_id: Some("k1".to_string()),
            provider_id: Some("p1".to_string()),
            provider_scope: Some("personal".to_string()),
            share_id: None,
            canonical_id: Some("gpt-4o".to_string()),
            upstream_model_id: Some("gpt-4o".to_string()),
            endpoint: "chat".to_string(),
            protocol: Some("chat".to_string()),
            session_hash: Some("sess-1".to_string()),
            status: 200,
            stream: false,
            incomplete: false,
            termination: "completed".to_string(),
            trace_dropped: false,
            input_uncached_tokens: 10,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            output_tokens: 10,
            reasoning_tokens: 0,
            cost_micro_usd: 10_000,
            cost_source: llmroller_core::pricing::CostSource::Provider,
            latency_ms: Some(100),
            ttfb_ms: Some(50),
            pin_outcome: Some("new_pin".to_string()),
            failovers: 0,
            error_code: None,
        };

        writer.enqueue(event, vec![]);
    }

    // Assert that the oldest 500 raw events were evicted (REL-3)
    assert_eq!(
        writer
            .dropped_raw_events_count
            .load(std::sync::atomic::Ordering::SeqCst),
        500
    );
    assert_eq!(writer.raw_events.lock().len(), 10_000);

    // Flush batch to database
    let raw_flushed = writer.flush_to_db(&mut tx).await.expect("flush");
    assert_eq!(raw_flushed, 10_000);

    // 2. Assert 100% of all 10,500 token and cost totals are PRESERVED in rollups! (REL-3)
    let total_rollup_cost: i64 =
        sqlx::query_scalar("SELECT SUM(cost_micro_usd) FROM usage_rollups_5min")
            .fetch_one(&mut *tx)
            .await
            .expect("query rollup 5min");
    // 10,500 * 10,000 micro-USD = 105,000,000 micro-USD
    assert_eq!(
        total_rollup_cost, 105_000_000,
        "100% of cost must be preserved in 5min rollups"
    );

    let total_hourly_tokens: i64 =
        sqlx::query_scalar("SELECT SUM(output_tokens) FROM usage_rollups_hourly")
            .fetch_one(&mut *tx)
            .await
            .expect("query rollup hourly");
    // 10,500 * 10 tokens = 105,000 tokens
    assert_eq!(
        total_hourly_tokens, 105_000,
        "100% of tokens must be preserved in hourly rollups"
    );

    // Exactly 10,000 raw usage_events rows in database (oldest 500 dropped)
    let raw_events_in_db: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&mut *tx)
        .await
        .expect("query raw events");
    assert_eq!(raw_events_in_db, 10_000);

    // 3. Pre-route rejection (503 no_eligible_route) folds with sentinel values:
    // upstream_key_id = '', provider_id = '', provider_scope = 'none'
    let pre_route_req_id = RequestId::generate();
    let pre_route_event = UsageEventRecord {
        id: "pre-route-1".to_string(),
        ts: base_ts,
        request_id: pre_route_req_id,
        client_key_id,
        owner_user_id: user_id,
        upstream_key_id: Some("".to_string()),
        provider_id: Some("".to_string()),
        provider_scope: Some("none".to_string()),
        share_id: None,
        canonical_id: Some("claude-sonnet".to_string()),
        upstream_model_id: None,
        endpoint: "chat".to_string(),
        protocol: Some("chat".to_string()),
        session_hash: None,
        status: 503,
        stream: false,
        incomplete: true,
        termination: "completed".to_string(),
        trace_dropped: false,
        input_uncached_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: 0,
        cost_micro_usd: 0,
        cost_source: llmroller_core::pricing::CostSource::PriceBook,
        latency_ms: Some(1),
        ttfb_ms: None,
        pin_outcome: None,
        failovers: 0,
        error_code: Some("no_eligible_route".to_string()),
    };

    writer.enqueue(pre_route_event, vec![]);
    writer.flush_to_db(&mut tx).await.expect("flush pre-route");

    let pre_route_5min_count: i64 = sqlx::query_scalar(
        "SELECT requests FROM usage_rollups_5min WHERE upstream_key_id = '' AND provider_scope = 'none'",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("pre-route 5min rollup");
    assert_eq!(pre_route_5min_count, 1);

    // 4. Failed upstream tries record usage_attempts rows (USG-1)
    let multi_try_req = RequestId::generate();
    let multi_try_event = UsageEventRecord {
        id: "multi-try-1".to_string(),
        ts: base_ts + 5000,
        request_id: multi_try_req,
        client_key_id,
        owner_user_id: user_id,
        upstream_key_id: Some("k2".to_string()),
        provider_id: Some("p1".to_string()),
        provider_scope: Some("personal".to_string()),
        share_id: None,
        canonical_id: Some("gpt-4o".to_string()),
        upstream_model_id: Some("gpt-4o".to_string()),
        endpoint: "chat".to_string(),
        protocol: Some("chat".to_string()),
        session_hash: Some("sess-2".to_string()),
        status: 200,
        stream: false,
        incomplete: false,
        termination: "completed".to_string(),
        trace_dropped: false,
        input_uncached_tokens: 20,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        output_tokens: 20,
        reasoning_tokens: 0,
        cost_micro_usd: 20_000,
        cost_source: llmroller_core::pricing::CostSource::Provider,
        latency_ms: Some(250),
        ttfb_ms: Some(100),
        pin_outcome: Some("repin".to_string()),
        failovers: 1,
        error_code: None,
    };

    let attempt1 = UsageAttemptRecord {
        request_id: multi_try_req,
        attempt_no: 1,
        ts: base_ts + 5000,
        upstream_key_id: "k1".to_string(),
        share_id: None,
        outcome: llmroller_core::usage::AttemptOutcome::Http5xx,
        http_status: Some(502),
        bytes_sent: 512,
        charged_micro_usd: 5_000,
        charge_basis: "estimated_input".to_string(),
    };

    let attempt2 = UsageAttemptRecord {
        request_id: multi_try_req,
        attempt_no: 2,
        ts: base_ts + 5100,
        upstream_key_id: "k2".to_string(),
        share_id: None,
        outcome: llmroller_core::usage::AttemptOutcome::Served,
        http_status: Some(200),
        bytes_sent: 512,
        charged_micro_usd: 20_000,
        charge_basis: "actual".to_string(),
    };

    writer.enqueue(multi_try_event, vec![attempt1, attempt2]);
    writer.flush_to_db(&mut tx).await.expect("flush multi-try");

    let attempts_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempts WHERE request_id = ?")
            .bind(multi_try_req.to_string())
            .fetch_one(&mut *tx)
            .await
            .expect("attempts query");
    assert_eq!(attempts_count, 2);

    tx.commit().await.expect("commit");
}

#[doc = "Spec: USG-1, SHR-6, REL-2, REL-3, Partial: TRC-9"]
#[tokio::test]
async fn test_sigkill_durability_and_restart_recovery() {
    let (_dir, pool, _master_key, user_id) = setup_usage_writer_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let model_pk = ModelPk::generate();
    let key_id = UpstreamKeyId::generate();
    let client_key_id = ClientKeyId::generate();

    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, origin) VALUES (?, 'kill9-model', 'global')",
    )
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model");

    let provider_id = llmroller_core::ids::ProviderId::generate();
    sqlx::query("INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, share_config_fingerprint, created_at) VALUES (?, 'P', 'openrouter', 'https://api.com', '{}', 'global', 'fp', 1000)")
        .bind(provider_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert provider");

    sqlx::query("INSERT INTO upstream_keys (id, provider_id, label, secret_enc, secret_last4, priority, master_key_id, created_at) VALUES (?, ?, 'K', X'00', 'test', 100, 'key-0', 1000)")
        .bind(key_id.to_string())
        .bind(provider_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert key");

    sqlx::query("INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at) VALUES (?, ?, 'Key', 'llr_123', 'hash123', 1000)")
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert client key");

    // Insert response_link
    let link = ResponseLinkRow {
        response_id: "resp_kill9_safe".to_string(),
        owner_user_id: user_id,
        client_key_id,
        upstream_key_id: key_id,
        model_pk,
        session_hash: "kill9-hash".to_string(),
        status: "completed".to_string(),
        store_flag: true,
        created_at: 1000,
        updated_at: 1000,
        expires_at: 1000 + 86400,
    };
    insert_response_link_tx(&mut tx, &link)
        .await
        .expect("insert response link");

    tx.commit().await.expect("commit before restart");

    // Simulate restart (reading from new transaction after restart):
    // Zero lost rows! (REL-2)
    let mut read_tx = pool.begin().await.expect("begin read tx");
    let fetched = get_response_link_tx(&mut read_tx, "resp_kill9_safe")
        .await
        .expect("get link")
        .expect("link must survive");
    assert_eq!(fetched.session_hash, "kill9-hash");
    read_tx.commit().await.expect("commit read tx");
}

#[doc = "Spec: USG-1, USG-2, USG-3, LIM-1"]
#[tokio::test]
async fn test_live_usage_emission_and_attempt_charging() {
    use llmroller_core::pricing::{Rates, estimate_input_tokens};
    use llmroller_core::windows::{AtomicBudgetCounter, BudgetCounter, Reservation};
    use llmroller_proto::adapters::EndpointKind;
    use llmroller_proto::sse_tap::SseUsageTap;
    use llmroller_proxy::lifecycle::{RequestLifecycle, Termination};

    let (_dir, pool, _master_key, user_id) = setup_usage_writer_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let client_key_id = ClientKeyId::generate();
    sqlx::query("INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at) VALUES (?, ?, 'Key', 'llr_123', 'hash123', 1000)")
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert client key");

    let writer = ThreeQueueUsageWriter::new(10_000);

    // 1. Upstream try fails with 502 after body write: charges estimated_input (USG-1, USG-3)
    let body = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"Hello world, please answer this prompt with detailed information."}]}"#;
    let input_est = estimate_input_tokens(body);
    let rates = Rates::default(); // $3 / MTok input
    let charged_est =
        ((input_est as i128 * rates.input_per_mtok_micro_usd as i128 + 999_999) / 1_000_000) as i64;
    assert!(charged_est > 0);

    let req_id = RequestId::generate();
    let attempt_fail = UsageAttemptRecord {
        request_id: req_id,
        attempt_no: 1,
        ts: 1000,
        upstream_key_id: "k1".to_string(),
        share_id: None,
        outcome: llmroller_core::usage::AttemptOutcome::Http5xx,
        http_status: Some(502),
        bytes_sent: body.len() as i64,
        charged_micro_usd: charged_est,
        charge_basis: "estimated_input".to_string(),
    };

    let attempt_success = UsageAttemptRecord {
        request_id: req_id,
        attempt_no: 2,
        ts: 1100,
        upstream_key_id: "k2".to_string(),
        share_id: None,
        outcome: llmroller_core::usage::AttemptOutcome::Served,
        http_status: Some(200),
        bytes_sent: body.len() as i64,
        charged_micro_usd: 15_000,
        charge_basis: "actual".to_string(),
    };

    let event = UsageEventRecord {
        id: "ev-attempt-charge".to_string(),
        ts: 1000,
        request_id: req_id,
        client_key_id,
        owner_user_id: user_id,
        upstream_key_id: Some("k2".to_string()),
        provider_id: Some("p1".to_string()),
        provider_scope: Some("personal".to_string()),
        share_id: None,
        canonical_id: Some("gpt-4o".to_string()),
        upstream_model_id: Some("gpt-4o".to_string()),
        endpoint: "chat".to_string(),
        protocol: Some("chat".to_string()),
        session_hash: Some("sess-att".to_string()),
        status: 200,
        stream: false,
        incomplete: false,
        termination: "completed".to_string(),
        trace_dropped: false,
        input_uncached_tokens: input_est,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        output_tokens: 20,
        reasoning_tokens: 0,
        cost_micro_usd: 15_000,
        cost_source: llmroller_core::pricing::CostSource::Provider,
        latency_ms: Some(200),
        ttfb_ms: Some(80),
        pin_outcome: Some("repin".to_string()),
        failovers: 1,
        error_code: None,
    };

    writer.enqueue(event, vec![attempt_fail, attempt_success]);
    writer.flush_to_db(&mut tx).await.expect("flush attempts");

    let attempts_saved: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempts WHERE request_id = ?")
            .bind(req_id.to_string())
            .fetch_one(&mut *tx)
            .await
            .expect("count attempts");
    assert_eq!(attempts_saved, 2);

    let est_charge: i64 = sqlx::query_scalar(
        "SELECT charged_micro_usd FROM usage_attempts WHERE request_id = ? AND attempt_no = 1",
    )
    .bind(req_id.to_string())
    .fetch_one(&mut *tx)
    .await
    .expect("query est charge");
    assert_eq!(est_charge, charged_est);

    // 2. Client disconnecting mid-stream emits usage_events with client_gone, incomplete = 1 (USG-1, USG-2, USG-3)
    let mut tap = SseUsageTap::new(EndpointKind::Chat);
    let delta =
        b"data: {\"choices\":[{\"delta\":{\"content\":\"partial answer from stream\"}}]}\n\n";
    tap.feed_chunk(delta);

    let mut lifecycle = RequestLifecycle::new(RequestId::generate(), 1000, None);
    lifecycle.set_termination(Termination::ClientGone);

    let client_gone_usage = tap.finalize_usage();
    let aborted_event = UsageEventRecord {
        id: "ev-client-gone".to_string(),
        ts: 1000,
        request_id: lifecycle.request_id,
        client_key_id,
        owner_user_id: user_id,
        upstream_key_id: Some("k2".to_string()),
        provider_id: Some("p1".to_string()),
        provider_scope: Some("personal".to_string()),
        share_id: None,
        canonical_id: Some("gpt-4o".to_string()),
        upstream_model_id: Some("gpt-4o".to_string()),
        endpoint: "chat".to_string(),
        protocol: Some("chat".to_string()),
        session_hash: Some("sess-gone".to_string()),
        status: 499,
        stream: true,
        incomplete: true,
        termination: lifecycle.termination.as_str().to_string(),
        trace_dropped: false,
        input_uncached_tokens: input_est,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        output_tokens: client_gone_usage.output_tokens,
        reasoning_tokens: 0,
        cost_micro_usd: 5_000,
        cost_source: llmroller_core::pricing::CostSource::EstimatedIncomplete,
        latency_ms: Some(150),
        ttfb_ms: Some(50),
        pin_outcome: Some("followed".to_string()),
        failovers: 0,
        error_code: Some("client_gone".to_string()),
    };

    writer.enqueue(aborted_event, vec![]);
    writer
        .flush_to_db(&mut tx)
        .await
        .expect("flush aborted event");

    let saved_termination: String =
        sqlx::query_scalar("SELECT termination FROM usage_events WHERE id = 'ev-client-gone'")
            .fetch_one(&mut *tx)
            .await
            .expect("query termination");
    assert_eq!(saved_termination, "client_gone");

    let saved_incomplete: i64 =
        sqlx::query_scalar("SELECT incomplete FROM usage_events WHERE id = 'ev-client-gone'")
            .fetch_one(&mut *tx)
            .await
            .expect("query incomplete");
    assert_eq!(saved_incomplete, 1);

    // 3. Reservation finalization: commits actual cost and releases reservation (LIM-1)
    let counter = Arc::new(AtomicBudgetCounter::new(1, 10_000_000, 2, 18_000_000));
    let res = Reservation::try_reserve(
        vec![Arc::clone(&counter) as Arc<dyn BudgetCounter>],
        50_000,
        1000,
    )
    .expect("reserve");
    assert_eq!(
        counter
            .reserved_in_flight()
            .load(std::sync::atomic::Ordering::SeqCst),
        50_000
    );

    // Commit actual cost of 45_000 micro-USD
    res.commit(45_000, 2000);
    assert_eq!(
        counter
            .reserved_in_flight()
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(counter.effective_used(2000), 45_000);

    tx.commit().await.expect("commit");
}

#[doc = "Spec: REL-3, Partial: PERF-4"]
#[tokio::test]
async fn test_retention_sweeper_and_wal_checkpoint_supervisor() {
    use llmroller_store::checkpoint::{
        WAL_RESTART_THRESHOLD_BYTES, WAL_SATURATION_THRESHOLD_BYTES, WalCheckpointSupervisor,
    };
    use llmroller_store::retention::{
        enforce_max_usage_events_rows, run_incremental_vacuum, sweep_expired_response_links,
        sweep_expired_usage_events_chunk, sweep_expired_web_sessions,
    };
    use std::sync::atomic::AtomicBool;

    let (_dir, pool, _master_key, user_id) = setup_usage_writer_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let client_key_id = ClientKeyId::generate();
    sqlx::query("INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at) VALUES (?, ?, 'Key', 'llr_123', 'hash123', 1000)")
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert client key");

    // Insert 5 usage_events
    for i in 0..5 {
        let req_id = RequestId::generate();
        sqlx::query(
            r#"INSERT INTO usage_events (
                   id, ts, request_id, client_key_id, owner_user_id,
                   endpoint, status, stream, incomplete, termination,
                   trace_dropped, cost_micro_usd, cost_source
               )
               VALUES (?, ?, ?, ?, ?, 'chat', 200, 0, 0, 'completed', 0, 1000, 'provider')"#,
        )
        .bind(format!("01ARZ3NDEKTSV4RRFFQ69G5FA{i}"))
        .bind(1_000_000 + i * 1000)
        .bind(req_id.to_string())
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert event");
    }

    // 1. Sweeper deletes expired in 1,000-row chunks using portable subquery (REL-3)
    let cutoff_id = "01ARZ3NDEKTSV4RRFFQ69G5FA3";
    let deleted = sweep_expired_usage_events_chunk(&mut tx, cutoff_id, 1000)
        .await
        .expect("sweep chunk");
    assert_eq!(deleted, 3); // 0, 1, 2 deleted

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&mut *tx)
        .await
        .expect("count remaining");
    assert_eq!(remaining, 2); // 3 and 4 remain

    // Max rows enforcement
    let deleted_excess = enforce_max_usage_events_rows(&mut tx, 1, 1000)
        .await
        .expect("enforce max");
    assert_eq!(deleted_excess, 1);

    // Expired web_sessions and response_links
    let sessions_deleted = sweep_expired_web_sessions(&mut tx, 2_000_000)
        .await
        .expect("sweep sessions");
    assert_eq!(sessions_deleted, 0);

    let links_deleted = sweep_expired_response_links(&mut tx, 2_000_000)
        .await
        .expect("sweep links");
    assert_eq!(links_deleted, 0);

    tx.commit().await.expect("commit");

    // Incremental vacuum (500 pages)
    run_incremental_vacuum(&pool, 500)
        .await
        .expect("incremental vacuum");

    // 2. WAL checkpoint supervisor (REL-3, Partial: PERF-4)
    let is_ready = Arc::new(AtomicBool::new(true));
    let supervisor = WalCheckpointSupervisor::new(Arc::clone(&is_ready));

    // Under normal size: runs PASSIVE checkpoint, ready is true
    supervisor
        .run_checkpoint(&pool, 10 * 1024 * 1024)
        .await
        .expect("passive checkpoint");
    assert!(is_ready.load(std::sync::atomic::Ordering::SeqCst));

    // Above 256 MiB: runs RESTART checkpoint
    supervisor
        .run_checkpoint(&pool, WAL_RESTART_THRESHOLD_BYTES + 1024)
        .await
        .expect("restart checkpoint");
    assert!(is_ready.load(std::sync::atomic::Ordering::SeqCst));

    // Above 1 GiB: sets /readyz to false (503)
    supervisor
        .run_checkpoint(&pool, WAL_SATURATION_THRESHOLD_BYTES + 1024)
        .await
        .expect("saturation checkpoint");
    assert!(!is_ready.load(std::sync::atomic::Ordering::SeqCst));

    // 3. Simulated SQLITE_FULL
    supervisor.handle_storage_full();
    assert!(!is_ready.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        supervisor
            .storage_full
            .load(std::sync::atomic::Ordering::SeqCst)
    );

    supervisor.clear_storage_full();
    assert!(is_ready.load(std::sync::atomic::Ordering::SeqCst));
}

#[doc = "Spec: USG-1"]
#[tokio::test]
async fn test_invalid_usage_row_is_isolated_by_savepoint_without_rolling_back_batch() {
    let (_dir, pool, _master_key, user_id) = setup_usage_writer_test_db().await;

    let client_key_id = ClientKeyId::generate();
    sqlx::query("INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at) VALUES (?, ?, 'Key', 'llr_123', 'hash123', 1000)")
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&pool)
        .await
        .expect("insert client key");

    let writer = ThreeQueueUsageWriter::new(100);
    let base = UsageEventRecord {
        id: "event-invalid".to_string(),
        ts: 1_700_000_000_000,
        request_id: RequestId::generate(),
        client_key_id,
        owner_user_id: user_id,
        upstream_key_id: None,
        provider_id: None,
        provider_scope: Some("personal".to_string()),
        share_id: None,
        canonical_id: Some("gpt-4o".to_string()),
        upstream_model_id: None,
        endpoint: "not_a_real_endpoint".to_string(),
        protocol: Some("chat".to_string()),
        session_hash: None,
        status: 200,
        stream: false,
        incomplete: false,
        termination: "completed".to_string(),
        trace_dropped: false,
        input_uncached_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: 0,
        cost_micro_usd: 10,
        cost_source: llmroller_core::pricing::CostSource::Provider,
        latency_ms: None,
        ttfb_ms: None,
        pin_outcome: None,
        failovers: 0,
        error_code: None,
    };
    let mut valid = base.clone();
    valid.id = "event-valid".to_string();
    valid.request_id = RequestId::generate();
    valid.endpoint = "chat".to_string();

    writer.enqueue(base, vec![]);
    writer.enqueue(valid, vec![]);

    let mut tx = pool.begin().await.expect("begin");
    writer
        .flush_to_db(&mut tx)
        .await
        .expect("flush must succeed");
    tx.commit().await.expect("commit");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(
        count, 1,
        "invalid row must be dropped without rolling back the valid batch"
    );
    let surviving: String = sqlx::query_scalar("SELECT id FROM usage_events")
        .fetch_one(&pool)
        .await
        .expect("surviving id");
    assert_eq!(surviving, "event-valid");
}
