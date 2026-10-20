//! Instance metadata and sealed instance-secret bootstrap tests.
//!
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#91-per-connection-pragma-builder-and-complete-31-table-sqlite-ddl-schema

use std::sync::Arc;

use llmroller_core::seams::{DeterministicRng, EntropySource, ExposeSecret};
use tempfile::TempDir;

use super::{InstanceError, load_or_initialize};
use crate::crypto::MasterKey;
use crate::migrations::migrate;
use crate::pool::{Role, open_pool};

async fn database() -> (TempDir, sqlx::SqlitePool) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("instance.db");
    let pool = open_pool(&path, Role::Writer)
        .await
        .expect("writer pool must open");
    migrate(&pool).await.expect("schema must migrate");
    (directory, pool)
}

fn entropy(seed: u64) -> Arc<dyn EntropySource> {
    Arc::new(DeterministicRng::new(seed))
}

#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn bootstrap_creates_instance_meta_and_two_sealed_256_bit_secrets() {
    let (_directory, pool) = database().await;
    let key = MasterKey::from_bytes([0x11; 32]);
    let entropy = entropy(0x1234);

    let state = load_or_initialize(&pool, "mk_one", &key, Arc::clone(&entropy), 1_000)
        .await
        .expect("first bootstrap");

    assert_eq!(state.active_master_key_id, "mk_one");
    assert_eq!(state.session_hmac_secret.expose_secret().len(), 32);
    assert_eq!(state.audit_hmac_key.expose_secret().len(), 32);

    let meta_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_meta")
        .fetch_one(&pool)
        .await
        .expect("meta count");
    let secret_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_secrets")
        .fetch_one(&pool)
        .await
        .expect("secret count");
    assert_eq!(meta_count, 1);
    assert_eq!(secret_count, 2);

    let stored_instance_id: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_one(&pool)
            .await
            .expect("instance id");
    assert_eq!(stored_instance_id, state.instance_id.to_vec());

    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM instance_secrets ORDER BY name")
        .fetch_all(&pool)
        .await
        .expect("secret names");
    assert_eq!(names, vec!["audit_hmac_key", "session_hmac_secret"]);

    let master_ids: Vec<String> = sqlx::query_scalar("SELECT master_key_id FROM instance_secrets")
        .fetch_all(&pool)
        .await
        .expect("secret master ids");
    assert!(master_ids.iter().all(|id| id == "mk_one"));

    drop(entropy);
    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn reload_preserves_secret_values_and_rejects_mismatches() {
    let (_directory, pool) = database().await;
    let key = MasterKey::from_bytes([0x22; 32]);
    let first = load_or_initialize(&pool, "mk_one", &key, entropy(7), 1_000)
        .await
        .expect("first bootstrap");

    let second = load_or_initialize(&pool, "mk_one", &key, entropy(999), 2_000)
        .await
        .expect("reload");
    assert_eq!(first.instance_id, second.instance_id);
    assert_eq!(
        first.session_hmac_secret.expose_secret(),
        second.session_hmac_secret.expose_secret()
    );
    assert_eq!(
        first.audit_hmac_key.expose_secret(),
        second.audit_hmac_key.expose_secret()
    );

    let id_mismatch = load_or_initialize(&pool, "mk_other", &key, entropy(1), 3_000)
        .await
        .expect_err("key id mismatch");
    assert!(matches!(
        id_mismatch,
        InstanceError::MasterKeyIdMismatch { .. }
    ));
    assert_eq!(id_mismatch.exit_code(), 3);

    let wrong_key = MasterKey::from_bytes([0x33; 32]);
    let auth_failure = load_or_initialize(&pool, "mk_one", &wrong_key, entropy(1), 3_000)
        .await
        .expect_err("wrong key");
    assert!(matches!(auth_failure, InstanceError::Sealed(_)));
    assert_eq!(auth_failure.exit_code(), 3);

    sqlx::query(
        "UPDATE instance_meta SET rotation_state = 'in_progress', \
         rotation_from_key_id = 'mk_one', rotation_to_key_id = 'mk_two', \
         rotation_cursor_table = 'instance_secrets' WHERE singleton = 1",
    )
    .execute(&pool)
    .await
    .expect("mark rotation");
    let rotating = load_or_initialize(&pool, "mk_one", &key, entropy(1), 4_000)
        .await
        .expect_err("rotation in progress");
    assert!(matches!(rotating, InstanceError::RotationInProgress));
    assert_eq!(rotating.exit_code(), 3);

    pool.close().await;
}
