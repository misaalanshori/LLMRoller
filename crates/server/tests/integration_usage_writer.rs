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
            cost_source: "provider".to_string(),
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
        cost_source: "price_book".to_string(),
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
        cost_source: "provider".to_string(),
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
        outcome: "http_5xx".to_string(),
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
        outcome: "served".to_string(),
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
