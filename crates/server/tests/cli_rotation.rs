//! End-to-end `rotate-master-key` CLI tests.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use llmroller_core::ids::{ClientKeyId, RequestId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, SystemSeal, SystemTask};
use llmroller_core::seams::{DeterministicRng, ExposeSecret, secret_string};
use llmroller_store::crypto::{MasterKey, SecretSealer, XChaCha20Poly1305Sealer};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::traces::{
    SealJob, StoredTurnTraceV2, insert_sealed_trace_tx, seal_turn_sync,
};
use llmroller_store::usage_snapshots::{NewUsageSnapshot, insert_usage_snapshot_tx};

fn encoded(byte: u8) -> String {
    STANDARD.encode([byte; 32])
}

async fn seed_database(data_dir: &Path) {
    let pool = open_pool(&data_dir.join("llmroller.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migration");
    load_or_initialize(
        &pool,
        "mk_old",
        &MasterKey::from_bytes([0x61; 32]),
        Arc::new(DeterministicRng::new(0x61)),
        1_000,
    )
    .await
    .expect("bootstrap");
    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2, Spec: PRV-5, Spec: SEC-18, Spec: SEC-19"]
async fn rotate_master_key_cli_rotates_all_sealed_tables_successfully() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pool = open_pool(&directory.path().join("llmroller.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migration");
    let old_master_key = MasterKey::from_bytes([0x61; 32]);
    load_or_initialize(
        &pool,
        "mk_old",
        &old_master_key,
        Arc::new(DeterministicRng::new(0x61)),
        1_000,
    )
    .await
    .expect("bootstrap");

    let mut tx = pool.begin().await.expect("begin");
    let actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let _entropy = Arc::new(DeterministicRng::new(0x61));

    // Create user and client key for trace records foreign keys
    let user_id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, 'alice', 'dummy-hash', 'user', 1000)",
    )
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert user");

    let client_key_id = ClientKeyId::generate();
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, label, key_hash, prefix, created_at) VALUES (?, ?, 'alice_key', 'test_key_hash', 'llr_test', 1000)",
    )
    .bind(client_key_id.to_string())
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert client key");

    let provider = create_provider_tx(
        &mut tx,
        &actor,
        NewProvider {
            name: "test_provider".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://api.openai.com/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1_000,
    )
    .await
    .expect("create provider");

    let upstream_key = create_upstream_key_tx(
        &mut tx,
        &actor,
        &old_master_key,
        "mk_old",
        Arc::new(DeterministicRng::new(0x61)),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "test_key".to_string(),
            secret: secret_string("sk-test-secret-value-12345"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1_000,
    )
    .await
    .expect("create upstream key");

    let instance_id_vec: Vec<u8> = sqlx::query_scalar(
        "SELECT instance_id FROM instance_meta WHERE singleton = 1",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("fetch instance_id");
    let instance_id: [u8; 16] = instance_id_vec.try_into().expect("16-byte instance_id");

    let old_sealer = XChaCha20Poly1305Sealer::new(
        old_master_key.clone(),
        instance_id,
        Arc::new(DeterministicRng::new(0x61)),
    );

    // Seed usage snapshot
    let snap_id = insert_usage_snapshot_tx(
        &mut tx,
        &old_sealer,
        NewUsageSnapshot {
            key_id: &upstream_key.id,
            now_ms: 1_000,
            http_status: 200,
            plan_name: Some("pay-per-use"),
            windows_json: "[]",
            balance_micro_usd: None,
            master_key_id: "mk_old",
            sanitized_json: "{}",
            raw_json_bytes: b"{\"raw_usage\": 123}",
        },
    )
    .await
    .expect("insert usage snapshot");

    // Seed usage event for trace foreign key
    let request_id = RequestId::generate();
    sqlx::query(
        r#"INSERT INTO usage_events (
               id, ts, request_id, client_key_id, owner_user_id,
               endpoint, status, stream, incomplete, termination,
               trace_dropped, cost_micro_usd, cost_source
           )
           VALUES ('ue-01', 1000, ?, ?, ?, 'chat', 200, 0, 0, 'completed', 0, 1000, 'provider')"#,
    )
    .bind(request_id.to_string())
    .bind(client_key_id.to_string())
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert usage event");

    // Seed trace blocks and records
    let trace_job = SealJob {
        master_key_id: "mk_old".to_string(),
        usage_event_id: "ue-01".to_string(),
        request_id,
        ts: 1_000,
        owner_user_id: user_id,
        client_key_id,
        session_hash: "sess-hash-1".to_string(),
        thread_id: "th-1".to_string(),
        canonical_id: "canon-1".to_string(),
        protocol: "chat".to_string(),
        truncated_content: false,
        turn_trace: StoredTurnTraceV2 {
            v: 2,
            request_skeleton: serde_json::json!({ "model": "gpt-4o" }),
            block_refs: vec!["block-hash-1".to_string()],
            response: serde_json::json!({ "text": "hello rotated" }),
            response_raw: None,
            truncation: None,
        },
        blocks: vec![("block-hash-1".to_string(), b"block 1 payload".to_vec())],
        expires_at: 1_000 + 86400 * 14 * 1000,
    };
    let sealed_turn = seal_turn_sync(&trace_job, &old_sealer).expect("seal turn");
    insert_sealed_trace_tx(&mut tx, &sealed_turn, "mk_old")
        .await
        .expect("insert sealed trace");

    tx.commit().await.expect("commit");
    pool.close().await;

    // Run rotate-master-key CLI
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("rotate-master-key")
        .arg("--old-key-env")
        .arg("LLMROLLER_TEST_OLD_KEY")
        .arg("--new-key-env")
        .arg("LLMROLLER_TEST_NEW_KEY")
        .arg("--new-key-id")
        .arg("mk_new")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_TEST_OLD_KEY", encoded(0x61))
        .env("LLMROLLER_TEST_NEW_KEY", encoded(0x62))
        .output()
        .await
        .expect("run llmroller");
    assert!(
        output.status.success(),
        "rotate-master-key failed with exit code {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify unsealing under mk_new for all tables
    let pool = open_pool(&directory.path().join("llmroller.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    let new_sealer = XChaCha20Poly1305Sealer::new(
        MasterKey::from_bytes([0x62; 32]),
        instance_id,
        Arc::new(DeterministicRng::new(0x62)),
    );

    // 1. Verify upstream key unsealing
    let key_secret_enc: Vec<u8> = sqlx::query_scalar(
        "SELECT secret_enc FROM upstream_keys WHERE id = ?",
    )
    .bind(upstream_key.id.to_string())
    .fetch_one(&pool)
    .await
    .expect("fetch secret_enc");

    let unsealed_key = new_sealer
        .open(
            "upstream_key",
            &[
                b"mk_new",
                upstream_key.id.to_string().as_bytes(),
                provider.id.to_string().as_bytes(),
            ],
            &key_secret_enc,
        )
        .expect("unseal key with mk_new");
    assert_eq!(unsealed_key.expose_secret(), b"sk-test-secret-value-12345");

    // 2. Verify usage snapshot unsealing
    let snap_raw_enc: Vec<u8> = sqlx::query_scalar(
        "SELECT raw_json_enc FROM upstream_usage_snapshots WHERE id = ?",
    )
    .bind(&snap_id)
    .fetch_one(&pool)
    .await
    .expect("fetch raw_json_enc");

    let unsealed_snap = new_sealer
        .open(
            "usage_raw",
            &[b"mk_new", snap_id.as_bytes()],
            &snap_raw_enc,
        )
        .expect("unseal usage snapshot with mk_new");
    assert_eq!(unsealed_snap.expose_secret(), b"{\"raw_usage\": 123}");

    // 3. Verify trace block unsealing
    let block_payload_enc: Vec<u8> = sqlx::query_scalar(
        "SELECT payload_enc FROM trace_blocks WHERE owner_user_id = ? AND block_hash = ?",
    )
    .bind(user_id.to_string())
    .bind("block-hash-1")
    .fetch_one(&pool)
    .await
    .expect("fetch block payload_enc");

    let unsealed_block = new_sealer
        .open(
            "trace_block",
            &[b"mk_new", user_id.to_string().as_bytes(), b"block-hash-1"],
            &block_payload_enc,
        )
        .expect("unseal trace block with mk_new");
    let decompressed_block = zstd::bulk::decompress(unsealed_block.expose_secret(), 1024)
        .expect("decompress block");
    assert_eq!(decompressed_block, b"block 1 payload");

    // 4. Verify trace record unsealing
    let record_payload_enc: Vec<u8> = sqlx::query_scalar(
        "SELECT payload_enc FROM trace_records WHERE usage_event_id = ?",
    )
    .bind("ue-01")
    .fetch_one(&pool)
    .await
    .expect("fetch record payload_enc");

    let unsealed_record = new_sealer
        .open(
            "trace_record",
            &[
                b"mk_new",
                request_id.to_string().as_bytes(),
                user_id.to_string().as_bytes(),
                b"sess-hash-1",
            ],
            &record_payload_enc,
        )
        .expect("unseal trace record with mk_new");
    let decompressed_record = zstd::bulk::decompress(unsealed_record.expose_secret(), 4096)
        .expect("decompress record");
    assert!(decompressed_record.starts_with(b"{\"v\":2"));

    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn rotate_master_key_cli_completes_and_flips_the_active_key() {
    let directory = tempfile::tempdir().expect("temporary directory");
    seed_database(directory.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("rotate-master-key")
        .arg("--old-key-env")
        .arg("LLMROLLER_TEST_OLD_KEY")
        .arg("--new-key-env")
        .arg("LLMROLLER_TEST_NEW_KEY")
        .arg("--new-key-id")
        .arg("mk_new")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_TEST_OLD_KEY", encoded(0x61))
        .env("LLMROLLER_TEST_NEW_KEY", encoded(0x62))
        .output()
        .await
        .expect("run llmroller");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("rotation complete"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pool = open_pool(&directory.path().join("llmroller.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    let state = load_or_initialize(
        &pool,
        "mk_new",
        &MasterKey::from_bytes([0x62; 32]),
        Arc::new(DeterministicRng::new(0x62)),
        2_000,
    )
    .await
    .expect("reload with new key");
    assert_eq!(state.active_master_key_id, "mk_new");
    assert_eq!(state.session_hmac_secret.expose_secret().len(), 32);
    assert_eq!(state.audit_hmac_key.expose_secret().len(), 32);

    let audit_action: String =
        sqlx::query_scalar("SELECT action FROM audit_log ORDER BY seq DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("audit row");
    assert_eq!(audit_action, "master_key.rotated");

    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: SEC-2"]
async fn rotate_master_key_cli_exits_3_when_a_key_variable_is_missing() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("rotate-master-key")
        .arg("--old-key-env")
        .arg("LLMROLLER_TEST_DEFINITELY_MISSING_OLD_KEY")
        .arg("--new-key-env")
        .arg("LLMROLLER_TEST_NEW_KEY")
        .arg("--new-key-id")
        .arg("mk_new")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_TEST_NEW_KEY", encoded(0x63))
        .env_remove("LLMROLLER_TEST_DEFINITELY_MISSING_OLD_KEY")
        .output()
        .await
        .expect("run llmroller");
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is not set"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[doc = "Partial: SEC-2"]
async fn rotate_master_key_cli_exits_4_when_the_lock_is_held() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let lock_path = directory.path().join("llmroller.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("lock file");
    lock.try_lock().expect("test must acquire lock");

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("rotate-master-key")
        .arg("--old-key-env")
        .arg("LLMROLLER_TEST_OLD_KEY")
        .arg("--new-key-env")
        .arg("LLMROLLER_TEST_NEW_KEY")
        .arg("--new-key-id")
        .arg("mk_new")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_TEST_OLD_KEY", encoded(0x61))
        .env("LLMROLLER_TEST_NEW_KEY", encoded(0x62))
        .output()
        .await
        .expect("run llmroller");
    assert_eq!(output.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("held by another process"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
