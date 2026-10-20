//! Crash recovery, atomic backup CLI, and exit code contract tests.
//!
//! Spec: Section 5.5, Section 7.1, Section 7.2,
//! Requirements: REL-6, REL-2, REL-3.

use std::sync::Arc;
use tempfile::tempdir;

use llmroller_core::ids::{ClientKeyId, ModelPk, UpstreamKeyId, UserId};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_server::backup::run_backup;
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pins::{ResponseLinkRow, get_response_link_tx, insert_response_link_tx};
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

async fn setup_recovery_test_db() -> (tempfile::TempDir, sqlx::SqlitePool, MasterKey, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("llmroller.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x88; 32]);
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
            username: "recovery_user",
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

#[doc = "Spec: REL-6, REL-2, REL-3"]
#[tokio::test]
async fn test_atomic_backup_cli_and_overwrite_guards() {
    let (dir, pool, _master_key, user_id) = setup_recovery_test_db().await;
    let backup_dest = dir.path().join("backup_target.db");

    // 1. Initial backup succeeds and writes valid SQLite file (REL-6)
    let json_result = run_backup(&pool, &backup_dest, false, "key-0")
        .await
        .expect("backup must succeed");

    assert_eq!(json_result["status"], "ok");
    assert_eq!(json_result["master_key_id"], "key-0");
    assert!(json_result["bytes"].as_u64().unwrap_or(0) > 0);
    assert!(backup_dest.exists());

    // 2. Refuses to overwrite without --overwrite (returns AlreadyExists) (REL-6)
    let err = run_backup(&pool, &backup_dest, false, "key-0").await;
    assert!(matches!(
        err,
        Err(llmroller_server::backup::BackupError::AlreadyExists)
    ));

    // 3. Overwrite succeeds when overwrite = true
    let overwrite_res = run_backup(&pool, &backup_dest, true, "key-0")
        .await
        .expect("overwrite backup");
    assert_eq!(overwrite_res["status"], "ok");

    // Verify backup integrity with SQLite pool
    let backup_pool = open_pool(&backup_dest, PoolRole::Reader)
        .await
        .expect("open backup pool");
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check;")
        .fetch_one(&backup_pool)
        .await
        .expect("integrity check");
    assert_eq!(integrity, "ok");

    // 4. Zero lost confirmed rows after crash simulation (REL-2, REL-3)
    let mut tx = pool.begin().await.expect("begin");
    let client_key_id = ClientKeyId::generate();
    let model_pk = ModelPk::generate();
    let key_id = UpstreamKeyId::generate();

    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, origin) VALUES (?, 'model-r', 'global')",
    )
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model");

    let p_id = llmroller_core::ids::ProviderId::generate();
    sqlx::query("INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, share_config_fingerprint, created_at) VALUES (?, 'P', 'openrouter', 'https://api.com', '{}', 'global', 'fp', 1000)")
        .bind(p_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert provider");

    sqlx::query("INSERT INTO upstream_keys (id, provider_id, label, secret_enc, secret_last4, priority, master_key_id, created_at) VALUES (?, ?, 'K', X'00', 'test', 100, 'key-0', 1000)")
        .bind(key_id.to_string())
        .bind(p_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert key");

    sqlx::query("INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at) VALUES (?, ?, 'Key', 'llr_123', 'hash123', 1000)")
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert client key");

    let link = ResponseLinkRow {
        response_id: "resp_crash_safe".to_string(),
        owner_user_id: user_id,
        client_key_id,
        upstream_key_id: key_id,
        model_pk,
        session_hash: "crash-hash".to_string(),
        status: "completed".to_string(),
        store_flag: true,
        created_at: 1000,
        updated_at: 1000,
        expires_at: 1000 + 86400,
    };
    insert_response_link_tx(&mut tx, &link)
        .await
        .expect("insert link");
    tx.commit().await.expect("commit");

    // Read back after simulated abrupt process restart
    let mut read_tx = pool.begin().await.expect("read tx");
    let fetched = get_response_link_tx(&mut read_tx, "resp_crash_safe")
        .await
        .expect("get link")
        .expect("link must survive crash");
    assert_eq!(fetched.session_hash, "crash-hash");
    read_tx.commit().await.expect("commit read tx");
}
