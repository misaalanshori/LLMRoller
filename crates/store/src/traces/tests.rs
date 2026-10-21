//! Tests for content-addressed trace_blocks, sealer pool, and reference-aware trace purge.
//!
//! Spec: Section 6.10, Section 7.1, Section 8.2, Section 9.1, Section 9.2,
//! Requirements: TRC-4, TRC-7, PERF-5, SEC-19.

use super::*;
use std::sync::Arc;
use tempfile::tempdir;

use crate::crypto::{MasterKey, XChaCha20Poly1305Sealer};
use crate::instance::load_or_initialize;
use crate::migrations::migrate;
use crate::pool::{Role as PoolRole, open_pool};
use crate::settings::seed_defaults;
use crate::users::{NewUser, create_user};
use llmroller_core::ids::{ClientKeyId, RequestId, UserId};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;

async fn setup_traces_test_db() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    Arc<XChaCha20Poly1305Sealer>,
    UserId,
) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("traces_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xEE; 32]);
    let instance = load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let sealer = Arc::new(XChaCha20Poly1305Sealer::new(
        master_key,
        instance.instance_id,
        Arc::new(entropy),
    ));

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "trace_user",
            password: "supersecretpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("user");

    (dir, writer_pool, sealer, user.id)
}

#[doc = "Spec: TRC-4, TRC-7, PERF-5, SEC-19"]
#[tokio::test]
async fn test_content_addressed_deduplication_and_reference_aware_purge() {
    let (_dir, pool, sealer, user_id) = setup_traces_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let client_key_id = ClientKeyId::generate();
    sqlx::query("INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at) VALUES (?, ?, 'Key', 'llr_123', 'hash123', 1000)")
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert client key");

    // Insert dummy usage_events rows so foreign key succeeds
    for i in 1..=3 {
        sqlx::query(
            r#"INSERT INTO usage_events (
                   id, ts, request_id, client_key_id, owner_user_id,
                   endpoint, status, stream, incomplete, termination,
                   trace_dropped, cost_micro_usd, cost_source
               )
               VALUES (?, 1000, ?, ?, ?, 'chat', 200, 0, 0, 'completed', 0, 1000, 'provider')"#,
        )
        .bind(format!("event-{i}"))
        .bind(format!("req-{i}"))
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert usage_event");
    }

    // Session 1: uses blocks B1 ("block-1-content") and B2 ("shared-block-content")
    let b1_hash = "b1_hash_hex".to_string();
    let b2_hash = "b2_shared_hash_hex".to_string();
    let b3_hash = "b3_hash_hex".to_string();

    let job_sess1 = SealJob {
        master_key_id: "key-0".to_string(),
        usage_event_id: "event-1".to_string(),
        request_id: RequestId::generate(),
        ts: 1000,
        owner_user_id: user_id,
        client_key_id,
        session_hash: "session-1".to_string(),
        thread_id: "main".to_string(),
        canonical_id: "gpt-4o".to_string(),
        protocol: "chat".to_string(),
        truncated_content: false,
        turn_trace: StoredTurnTraceV2 {
            v: 2,
            request_skeleton: serde_json::json!({ "model": "gpt-4o" }),
            block_refs: vec![b1_hash.clone(), b2_hash.clone()],
            response: serde_json::json!({ "text": "turn 1" }),
            response_raw: None,
            truncation: None,
        },
        blocks: vec![
            (b1_hash.clone(), b"block 1 content".to_vec()),
            (b2_hash.clone(), b"shared block 2 content".to_vec()),
        ],
        expires_at: 1000 + 86400 * 14 * 1000,
    };

    let sealed_sess1 = seal_turn_sync(&job_sess1, &sealer).expect("seal turn 1");
    insert_sealed_trace_tx(&mut tx, &sealed_sess1, "key-0")
        .await
        .expect("insert trace 1");

    // Session 2: uses blocks B2 (shared!) and B3
    let job_sess2 = SealJob {
        master_key_id: "key-0".to_string(),
        usage_event_id: "event-2".to_string(),
        request_id: RequestId::generate(),
        ts: 2000,
        owner_user_id: user_id,
        client_key_id,
        session_hash: "session-2".to_string(),
        thread_id: "main".to_string(),
        canonical_id: "gpt-4o".to_string(),
        protocol: "chat".to_string(),
        truncated_content: false,
        turn_trace: StoredTurnTraceV2 {
            v: 2,
            request_skeleton: serde_json::json!({ "model": "gpt-4o" }),
            block_refs: vec![b2_hash.clone(), b3_hash.clone()],
            response: serde_json::json!({ "text": "turn 2" }),
            response_raw: None,
            truncation: None,
        },
        blocks: vec![
            (b2_hash.clone(), b"shared block 2 content".to_vec()),
            (b3_hash.clone(), b"block 3 content".to_vec()),
        ],
        expires_at: 2000 + 86400 * 14 * 1000,
    };

    let sealed_sess2 = seal_turn_sync(&job_sess2, &sealer).expect("seal turn 2");
    insert_sealed_trace_tx(&mut tx, &sealed_sess2, "key-0")
        .await
        .expect("insert trace 2");

    // Verify 3 distinct blocks in trace_blocks (B2 was deduplicated!) (TRC-4)
    let total_blocks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace_blocks")
        .fetch_one(&mut *tx)
        .await
        .expect("count blocks");
    assert_eq!(total_blocks, 3); // B1, B2, B3

    // 2. Reference-aware trace purge: DELETE session 1 (TRC-7)
    let (deleted_records, deleted_blocks) = delete_session_trace_tx(&mut tx, &user_id, "session-1")
        .await
        .expect("delete session 1");

    assert_eq!(deleted_records, 1);
    assert_eq!(deleted_blocks, 1); // Exactly B1 deleted! B2 is PRESERVED because session 2 uses it!

    // Verify B2 and B3 still exist in trace_blocks
    let b2_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM trace_blocks WHERE block_hash = ? AND owner_user_id = ?",
    )
    .bind(&b2_hash)
    .bind(user_id.to_string())
    .fetch_one(&mut *tx)
    .await
    .expect("check b2");
    assert_eq!(b2_exists, 1, "shared block B2 must be preserved");

    let b1_exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM trace_blocks WHERE block_hash = ? AND owner_user_id = ?",
    )
    .bind(&b1_hash)
    .bind(user_id.to_string())
    .fetch_one(&mut *tx)
    .await
    .expect("check b1");
    assert_eq!(b1_exists, 0, "unreferenced block B1 must be deleted");

    // Usage events rows are completely preserved!
    let events_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&mut *tx)
        .await
        .expect("check events");
    assert_eq!(events_count, 3);

    tx.commit().await.expect("commit");
}

#[doc = "Spec: TRC-4, TRC-7, PERF-5, SEC-19"]
#[test]
fn test_sealer_pool_bounded_queue_and_drop_on_full() {
    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xEE; 32]);
    let sealer = Arc::new(XChaCha20Poly1305Sealer::new(
        master_key,
        [0x42; 16],
        Arc::new(entropy),
    ));

    let (pool, _rx) = SealerPool::new(sealer);

    let dummy_job = SealJob {
        master_key_id: "key-0".to_string(),
        usage_event_id: "ev".to_string(),
        request_id: RequestId::generate(),
        ts: 1000,
        owner_user_id: UserId::generate(),
        client_key_id: ClientKeyId::generate(),
        session_hash: "sess".to_string(),
        thread_id: "main".to_string(),
        canonical_id: "m".to_string(),
        protocol: "chat".to_string(),
        truncated_content: false,
        turn_trace: StoredTurnTraceV2 {
            v: 2,
            request_skeleton: serde_json::json!({}),
            block_refs: vec![],
            response: serde_json::json!({}),
            response_raw: None,
            truncation: None,
        },
        blocks: vec![],
        expires_at: 1000,
    };

    // Rapid submissions fill the 64-item bounded channel and overflow
    let mut dropped = false;
    for _ in 0..200 {
        if !pool.try_submit(dummy_job.clone()) {
            dropped = true;
            break;
        }
    }
    assert!(
        dropped,
        "rapid submissions must overflow the 64-item channel"
    );
    assert!(pool.dropped_jobs_count.load(Ordering::SeqCst) >= 1);
}

/// Spec: REL-1, PERF-5; graceful shutdown must quiesce accepted seal jobs.
#[tokio::test]
async fn test_sealer_pool_flush_drains_accepted_jobs_and_closes_submissions() {
    let (_dir, _db_pool, sealer, user_id) = setup_traces_test_db().await;
    let (pool, results) = SealerPool::new(sealer);
    let job = SealJob {
        master_key_id: "key-0".to_string(),
        usage_event_id: "event-flush".to_string(),
        request_id: RequestId::generate(),
        ts: 1000,
        owner_user_id: user_id,
        client_key_id: ClientKeyId::generate(),
        session_hash: "session-flush".to_string(),
        thread_id: "main".to_string(),
        canonical_id: "m".to_string(),
        protocol: "chat".to_string(),
        truncated_content: false,
        turn_trace: StoredTurnTraceV2 {
            v: 2,
            request_skeleton: serde_json::json!({}),
            block_refs: vec![],
            response: serde_json::json!({}),
            response_raw: None,
            truncation: None,
        },
        blocks: vec![],
        expires_at: 1000,
    };

    for _ in 0..3 {
        assert!(pool.try_submit(job.clone()), "accepted submission");
    }

    let collector = std::thread::spawn(move || {
        let mut sealed = 0;
        while sealed < 3 {
            match results.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(_) => sealed += 1,
                Err(_) => break,
            }
        }
        sealed
    });

    assert!(
        pool.flush(std::time::Duration::from_secs(5)).await,
        "flush must observe a quiescent pool"
    );
    assert_eq!(collector.join().expect("collector"), 3);
    assert!(!pool.try_submit(job), "flush must close submissions");
    assert_eq!(pool.in_flight(), 0);
    assert!(pool.dropped_jobs_count.load(Ordering::SeqCst) >= 1);
}
