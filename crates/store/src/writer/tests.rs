//! Writer actor tests.
//!
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#72-reliability-and-availability-rel
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#92-storage-invariants-dual-queue-write-batching-and-retention

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use llmroller_core::ids::{ClientKeyId, ModelPk, ProviderId, RequestId, UpstreamKeyId, UserId};
use llmroller_core::seams::DeterministicRng;
use sqlx::{Sqlite, Transaction};
use tempfile::TempDir;
use tokio::sync::mpsc;

use super::{
    ControlOp, DataQueues, DurableKey, DurableOp, RAW_EVENTS_MAX_ITEMS, WriterError,
    flush_data_queues, start_writer,
};
use crate::crypto::MasterKey;
use crate::instance::load_or_initialize;
use crate::migrations::migrate;
use crate::pool::{Role, open_pool};
use crate::usage_writer::UsageEventRecord;

async fn setup_writer_db() -> (TempDir, sqlx::SqlitePool) {
    let directory = tempfile::tempdir().expect("tempdir");
    let pool = open_pool(&directory.path().join("llmroller.db"), Role::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    load_or_initialize(
        &pool,
        "mk_test",
        &MasterKey::from_bytes([0x33; 32]),
        Arc::new(DeterministicRng::new(0x33)),
        1_000,
    )
    .await
    .expect("bootstrap");
    (directory, pool)
}

struct InsertSettingOp(&'static str, &'static str);

impl ControlOp for InsertSettingOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), WriterError>> + Send + 'a>> {
        Box::pin(async move {
            sqlx::query("INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, 1)")
                .bind(self.0)
                .bind(self.1)
                .execute(&mut **tx)
                .await
                .map_err(WriterError::from)?;
            Ok(())
        })
    }
}

struct UpdateSettingOp(&'static str, &'static str);

impl ControlOp for UpdateSettingOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), WriterError>> + Send + 'a>> {
        Box::pin(async move {
            sqlx::query("UPDATE settings SET value_json = ? WHERE key = ?")
                .bind(self.1)
                .bind(self.0)
                .execute(&mut **tx)
                .await
                .map_err(WriterError::from)?;
            Ok(())
        })
    }
}

#[tokio::test]
#[doc = "Spec: REL-4"]
async fn writer_control_transaction_increments_config_version_and_notifies_reloader() {
    let (_dir, pool) = setup_writer_db().await;
    let (reloader_tx, mut reloader_rx) = mpsc::channel(10);
    let (writer, _handle) = start_writer(pool.clone(), reloader_tx);

    let v1 = writer
        .execute_control(vec![], vec![], InsertSettingOp("test_k", "\"test_v\""))
        .await
        .expect("execute control 1");

    assert_eq!(v1, 1);
    let notif1 = reloader_rx.recv().await.expect("reloader notification");
    assert_eq!(notif1.min_version, 1);

    let v2 = writer
        .execute_control(vec![], vec![], UpdateSettingOp("test_k", "\"updated\""))
        .await
        .expect("execute control 2");

    assert_eq!(v2, 2);
    let notif2 = reloader_rx.recv().await.expect("reloader notification");
    assert_eq!(notif2.min_version, 2);
}

#[test]
#[doc = "Spec: REL-3"]
fn data_queues_raw_events_bounds_and_evicts_oldest() {
    let queues = DataQueues::new();
    for i in 0..RAW_EVENTS_MAX_ITEMS + 500 {
        queues.push_raw(format!("item_{i}"));
    }

    let lock = queues.raw_events.lock();
    assert_eq!(lock.len(), RAW_EVENTS_MAX_ITEMS);
    // Oldest 500 were evicted: front is item_500
    assert_eq!(lock.front().expect("front"), "item_500");
    assert_eq!(
        lock.back().expect("back"),
        &format!("item_{}", RAW_EVENTS_MAX_ITEMS + 499)
    );
}

#[tokio::test]
#[doc = "Spec: REL-2, Spec: REL-3, Spec: USG-1"]
async fn test_writer_flushes_durable_ops_rollups_and_raw_events() {
    let (_dir, pool) = setup_writer_db().await;
    let (reloader_tx, _reloader_rx) = mpsc::channel(10);
    let (writer, _handle) = start_writer(pool.clone(), reloader_tx);

    let client_key_id = ClientKeyId::generate();
    let user_id = UserId::generate();
    let upstream_key_id = UpstreamKeyId::generate();
    let provider_id = ProviderId::generate();
    let request_id = RequestId::generate();

    // Seed database with required foreign key entities
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, 'user1', 'h', 'user', 1000)",
    )
    .bind(user_id.to_string())
    .execute(&pool)
    .await
    .expect("insert user");

    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, label, key_hash, prefix, created_at) VALUES (?, ?, 'k', 'hash1', 'llr_k', 1000)",
    )
    .bind(client_key_id.to_string())
    .bind(user_id.to_string())
    .execute(&pool)
    .await
    .expect("insert client_key");

    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, share_config_fingerprint, created_at) VALUES (?, 'p', 'custom', 'https://api.openai.com/v1', '{}', 'global', 'fp', 1000)",
    )
    .bind(provider_id.to_string())
    .execute(&pool)
    .await
    .expect("insert provider");

    let model_pk = ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id) VALUES (?, 'gpt-4o')")
        .bind(model_pk.to_string())
        .execute(&pool)
        .await
        .expect("insert model");

    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, label, secret_enc, secret_last4, priority, master_key_id, created_at) VALUES (?, ?, 'uk', X'00', '1234', 100, 'mk_test', 1000)",
    )
    .bind(upstream_key_id.to_string())
    .bind(provider_id.to_string())
    .execute(&pool)
    .await
    .expect("insert upstream_key");

    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, discovered_at) \
         VALUES (?, ?, 'gpt-4o', '[\"chat\"]', 'active', 1000)",
    )
    .bind(upstream_key_id.to_string())
    .bind(model_pk.to_string())
    .execute(&pool)
    .await
    .expect("insert model_route");

    // 1. Enqueue DurableOp (ResponseLinkUpsert)
    writer.queues.durable_state.lock().insert(
        DurableKey::ResponseLink("resp-test-1".to_string()),
        DurableOp::ResponseLinkUpsert(crate::pins::ResponseLinkRow {
            response_id: "resp-test-1".to_string(),
            owner_user_id: user_id,
            client_key_id,
            upstream_key_id,
            model_pk,
            session_hash: "sess-hash-test".to_string(),
            status: "in_progress".to_string(),
            store_flag: true,
            created_at: 1_000,
            updated_at: 1_000,
            expires_at: 10_000,
        }),
    );

    // 1b. Enqueue a confirmed pin with a non-zero repin count.
    writer.queues.durable_state.lock().insert(
        DurableKey::Pin(
            client_key_id.to_string(),
            "sess-hash-test".to_string(),
            model_pk.to_string(),
        ),
        DurableOp::PinUpsert {
            client_key_id: client_key_id.to_string(),
            session_hash: "sess-hash-test".to_string(),
            model_pk: model_pk.to_string(),
            upstream_key_id: upstream_key_id.to_string(),
            pool_tier: 1,
            created_at: 1_000,
            last_used_at: 2_000,
            expires_at: 10_000,
            repin_count: 2,
        },
    );

    // 1c. Enqueue route health and a protocol block.
    writer.queues.durable_state.lock().insert(
        DurableKey::RouteHealth(upstream_key_id.to_string(), model_pk.to_string()),
        DurableOp::RouteHealthUpdate {
            upstream_key_id: upstream_key_id.to_string(),
            model_pk: model_pk.to_string(),
            route_health: "cooling_down".to_string(),
            route_health_until: Some(5_000),
            health_version: 7,
            last_error: Some("upstream_5xx".to_string()),
        },
    );
    writer.queues.durable_state.lock().insert(
        DurableKey::RouteProtocolBlock(
            upstream_key_id.to_string(),
            model_pk.to_string(),
            "chat".to_string(),
        ),
        DurableOp::RouteProtocolBlockUpsert {
            upstream_key_id: upstream_key_id.to_string(),
            model_pk: model_pk.to_string(),
            protocol: "chat".to_string(),
            reason: "plan_restricted".to_string(),
            until_ms: 9_000,
        },
    );

    // 2. Enqueue UsageEventRecord and Rollup via usage_writer
    writer.queues.usage_writer.enqueue(
        UsageEventRecord {
            id: "ue-writer-test-1".to_string(),
            ts: 1_000,
            request_id,
            client_key_id,
            owner_user_id: user_id,
            upstream_key_id: Some(upstream_key_id.to_string()),
            provider_id: Some(provider_id.to_string()),
            provider_scope: Some("global".to_string()),
            share_id: None,
            canonical_id: Some("gpt-4o".to_string()),
            upstream_model_id: Some("gpt-4o".to_string()),
            endpoint: "chat".to_string(),
            protocol: Some("chat".to_string()),
            session_hash: Some("sess-hash-test".to_string()),
            status: 200,
            stream: false,
            incomplete: false,
            termination: "completed".to_string(),
            trace_dropped: false,
            input_uncached_tokens: 10,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            output_tokens: 20,
            reasoning_tokens: 0,
            cost_micro_usd: 150,
            cost_source: llmroller_core::pricing::CostSource::Provider,
            latency_ms: Some(100),
            ttfb_ms: Some(50),
            pin_outcome: None,
            failovers: 0,
            error_code: None,
        },
        vec![],
    );

    // Trigger flush
    flush_data_queues(&pool, &writer.queues).await;

    // 3. Assertions: response_link, usage_rollups_5min, and usage_events must be persisted!
    let link_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM response_links WHERE response_id = 'resp-test-1'")
            .fetch_one(&pool)
            .await
            .expect("count response_links");
    assert_eq!(link_count, 1, "response_link must be persisted in SQLite");

    let rollup_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_rollups_5min")
        .fetch_one(&pool)
        .await
        .expect("count usage_rollups_5min");
    assert_eq!(
        rollup_count, 1,
        "usage_rollups_5min must be persisted in SQLite"
    );

    let event_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_events WHERE id = 'ue-writer-test-1'")
            .fetch_one(&pool)
            .await
            .expect("count usage_events");
    assert_eq!(event_count, 1, "usage_events must be persisted in SQLite");

    // 4. Pins, route health, and protocol blocks use the same durable queue.
    let repin_count: i64 =
        sqlx::query_scalar("SELECT repin_count FROM pins WHERE session_hash = 'sess-hash-test'")
            .fetch_one(&pool)
            .await
            .expect("persisted pin");
    assert_eq!(repin_count, 2, "PinUpsert must persist repin_count");

    let (route_health, until, version): (String, Option<i64>, i64) = sqlx::query_as(
        "SELECT route_health, route_health_until, health_version
           FROM model_routes WHERE upstream_key_id = ? AND model_pk = ?",
    )
    .bind(upstream_key_id.to_string())
    .bind(model_pk.to_string())
    .fetch_one(&pool)
    .await
    .expect("route health");
    assert_eq!(route_health, "cooling_down");
    assert_eq!(until, Some(5_000));
    assert_eq!(version, 7);

    let block_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM route_protocol_blocks")
        .fetch_one(&pool)
        .await
        .expect("protocol block count");
    assert_eq!(block_count, 1);

    writer.queues.durable_state.lock().insert(
        DurableKey::RouteProtocolClear(upstream_key_id.to_string()),
        DurableOp::RouteProtocolBlockClear {
            upstream_key_id: upstream_key_id.to_string(),
        },
    );
    flush_data_queues(&pool, &writer.queues).await;
    let block_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM route_protocol_blocks")
        .fetch_one(&pool)
        .await
        .expect("protocol block count after clear");
    assert_eq!(
        block_count, 0,
        "a discovery refresh must clear persisted blocks"
    );
}
