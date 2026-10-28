//! Instance metadata and sealed instance-secret bootstrap tests.
//!
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#91-per-connection-pragma-builder-and-complete-31-table-sqlite-ddl-schema

use std::sync::Arc;

use llmroller_core::seams::{DeterministicRng, EntropySource, ExposeSecret};
use tempfile::TempDir;

use super::{InstanceError, load_instance_state, load_or_initialize};
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

// ---------------------------------------------------------------------------
// ISSUE-0183 — the read-only loader must never bootstrap
// ---------------------------------------------------------------------------

/// `load_instance_state` must refuse an uninitialised database instead of creating the row.
///
/// This is the store-level guarantee `llmroller doctor` depends on. `load_or_initialize`
/// bootstraps on the same input; that difference is the whole point of having two functions.
#[tokio::test]
#[doc = "Spec: SEC-2"]
async fn read_only_loader_refuses_an_uninitialised_database() {
    let (_directory, pool) = database().await;
    let key = MasterKey::from_bytes([0x22; 32]);

    let err = load_instance_state(&pool, "mk_one", &key, entropy(0x55))
        .await
        .expect_err("an uninitialised database must be refused");
    assert!(
        matches!(err, InstanceError::NotInitialized),
        "expected NotInitialized, got {err:?}"
    );
    assert_eq!(err.exit_code(), 10, "uninitialised is a 10, not a crypto 3");

    // Nothing may have been written.
    let meta_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_meta")
        .fetch_one(&pool)
        .await
        .expect("meta count");
    let secret_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_secrets")
        .fetch_one(&pool)
        .await
        .expect("secret count");
    assert_eq!(
        meta_count, 0,
        "the read-only loader must not create instance_meta"
    );
    assert_eq!(
        secret_count, 0,
        "the read-only loader must not create instance_secrets"
    );
}

/// On an initialised database the read-only loader returns the same state as the initialiser.
#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn read_only_loader_returns_the_same_state_as_the_initialiser() {
    let (_directory, pool) = database().await;
    let key = MasterKey::from_bytes([0x33; 32]);

    let initialized = load_or_initialize(&pool, "mk_one", &key, entropy(0x77), 1_000)
        .await
        .expect("bootstrap");
    let read_back = load_instance_state(&pool, "mk_one", &key, entropy(0x99))
        .await
        .expect("read-only load");

    assert_eq!(read_back.instance_id, initialized.instance_id);
    assert_eq!(read_back.active_master_key_id, "mk_one");
    assert_eq!(
        read_back.session_hmac_secret.expose_secret(),
        initialized.session_hmac_secret.expose_secret()
    );
    assert_eq!(
        read_back.audit_hmac_key.expose_secret(),
        initialized.audit_hmac_key.expose_secret()
    );
}

/// A master-key id disagreement is a `3`, not an "uninitialised" `10`.
#[tokio::test]
#[doc = "Spec: SEC-2"]
async fn read_only_loader_keeps_the_master_key_id_guard() {
    let (_directory, pool) = database().await;
    let key = MasterKey::from_bytes([0x44; 32]);
    load_or_initialize(&pool, "mk_one", &key, entropy(0x11), 1_000)
        .await
        .expect("bootstrap");

    let err = load_instance_state(&pool, "mk_two", &key, entropy(0x12))
        .await
        .expect_err("a key-id mismatch must be refused");
    assert!(
        matches!(err, InstanceError::MasterKeyIdMismatch { .. }),
        "expected MasterKeyIdMismatch, got {err:?}"
    );
    assert_eq!(err.exit_code(), 3);
}

// ---------------------------------------------------------------------------
// The delegation must be provably neutral: one implementation, one difference.
//
// `load_or_initialize` delegates the whole verification body to `load_instance_state`.
// These tests are what make that safe to keep. Without them the two paths could drift
// again, which is the defect the delegation removes.
// ---------------------------------------------------------------------------

/// Populate `instance_secrets` while leaving `instance_meta` empty.
///
/// This is the state a partially restored or hand-edited database can reach: sealed
/// secrets survive, the meta row does not.
async fn seed_orphaned_secrets(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO instance_secrets (name, sealed, master_key_id, created_at) \
         VALUES ('session_hmac_secret', X'00', 'mk_one', 1)",
    )
    .execute(pool)
    .await
    .expect("orphan secret");
}

/// Orphaned secrets on a fresh meta must be `CorruptMetadata`, never a second bootstrap.
///
/// The `instance_secrets != 0` guard is the only thing standing between this state and a
/// silent second identity: `initialize` would happily insert a *new* `instance_meta` row
/// and two more secrets, leaving the database with two generations of key material and no
/// way to tell which one the existing rows belong to.
///
/// A refactor that moves the absent-row branch must keep this check. If this test fails,
/// the delegation dropped the guard.
#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn load_or_initialize_refuses_orphaned_secrets_on_a_fresh_meta() {
    let (_directory, pool) = database().await;
    let key = MasterKey::from_bytes([0x55; 32]);
    seed_orphaned_secrets(&pool).await;

    let err = load_or_initialize(&pool, "mk_one", &key, entropy(0x31), 1_000)
        .await
        .expect_err("orphaned secrets must block bootstrap, not be overwritten");
    assert!(
        matches!(err, InstanceError::CorruptMetadata),
        "expected CorruptMetadata, got {err:?}"
    );
    assert_eq!(err.exit_code(), 3);

    // No second identity may have been created.
    let meta_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_meta")
        .fetch_one(&pool)
        .await
        .expect("meta count");
    assert_eq!(meta_count, 0, "bootstrap must not create instance_meta");
    let secret_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_secrets")
        .fetch_one(&pool)
        .await
        .expect("secret count");
    assert_eq!(
        secret_count, 1,
        "bootstrap must not add secrets beside the orphans"
    );
}

/// A fresh database still bootstraps through the delegating entry point.
///
/// `load_or_initialize` is now a wrapper around `load_instance_state`; the empty-database
/// branch is the only thing it still owns, so it is pinned directly rather than incidentally.
#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn load_or_initialize_still_bootstraps_an_empty_database() {
    let (_directory, pool) = database().await;
    let key = MasterKey::from_bytes([0x66; 32]);

    let state = load_or_initialize(&pool, "mk_new", &key, entropy(0x41), 4_242)
        .await
        .expect("a genuinely empty database must bootstrap");

    assert_eq!(state.active_master_key_id, "mk_new");
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
}

/// The variant name without its payload, so table failures read cleanly.
fn variant_name(error: &InstanceError) -> &'static str {
    match error {
        InstanceError::Sqlx(_) => "Sqlx",
        InstanceError::RotationInProgress => "RotationInProgress",
        InstanceError::MasterKeyIdMismatch { .. } => "MasterKeyIdMismatch",
        InstanceError::NotInitialized => "NotInitialized",
        InstanceError::CorruptMetadata => "CorruptMetadata",
        InstanceError::MissingSecret { .. } => "MissingSecret",
        InstanceError::InvalidSecretLength { .. } => "InvalidSecretLength",
        InstanceError::SecretKeyIdMismatch { .. } => "SecretKeyIdMismatch",
        InstanceError::Sealed(_) => "Sealed",
    }
}

/// Every reachable verification failure must produce the **same error variant** from both
/// entry points.
///
/// This is the test that makes the duplication fix durable. `load_or_initialize` now
/// delegates to `load_instance_state`, so the paths cannot disagree by construction — but
/// only a test proves that stays true if someone re-inlines the body. Table-driven, because
/// separate tests would let a future edit fix one case and leave another drifting, which is
/// exactly how duplicated validation fails in practice.
///
/// Cases the schema forbids are deliberately absent: `instance_meta.instance_id` carries
/// `CHECK (length(instance_id) = 16)` and `instance_secrets.name` carries
/// `CHECK (name IN ('session_hmac_secret', 'audit_hmac_key'))`, so a short instance id and
/// an unrecognised secret name cannot be stored. `decode_instance_id`'s error arm and the
/// unknown-name arm are therefore unreachable from a real database and are not asserted
/// here rather than faked.
#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn load_or_initialize_and_load_instance_state_agree_on_every_verification_failure() {
    type Fixture = fn(sqlx::SqlitePool) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

    async fn set_rotation(pool: sqlx::SqlitePool) {
        sqlx::query("UPDATE instance_meta SET rotation_state = 'in_progress' WHERE singleton = 1")
            .execute(&pool)
            .await
            .expect("mark rotation");
    }
    async fn set_secret_key_id(pool: sqlx::SqlitePool) {
        sqlx::query("UPDATE instance_secrets SET master_key_id = 'mk_other'")
            .execute(&pool)
            .await
            .expect("repoint secret key id");
    }
    async fn drop_audit_secret(pool: sqlx::SqlitePool) {
        sqlx::query("DELETE FROM instance_secrets WHERE name = 'audit_hmac_key'")
            .execute(&pool)
            .await
            .expect("drop audit secret");
    }
    async fn corrupt_sealed_blob(pool: sqlx::SqlitePool) {
        sqlx::query("UPDATE instance_secrets SET sealed = X'DEADBEEF'")
            .execute(&pool)
            .await
            .expect("corrupt sealed blob");
    }

    let cases: [(&str, Fixture, &str); 4] = [
        (
            "rotation in progress",
            |p| Box::pin(set_rotation(p)),
            "RotationInProgress",
        ),
        (
            "secret written under another key",
            |p| Box::pin(set_secret_key_id(p)),
            "SecretKeyIdMismatch",
        ),
        (
            "missing required secret",
            |p| Box::pin(drop_audit_secret(p)),
            "MissingSecret",
        ),
        (
            "unauthentic sealed blob",
            |p| Box::pin(corrupt_sealed_blob(p)),
            "Sealed",
        ),
    ];

    for (label, corrupt, expected) in cases {
        // A fresh database per case: each corruption is destructive.
        let (_directory, pool) = database().await;
        let key = MasterKey::from_bytes([0x77; 32]);
        load_or_initialize(&pool, "mk_one", &key, entropy(0x21), 1_000)
            .await
            .expect("bootstrap");
        corrupt(pool.clone()).await;

        let via_initializer = load_or_initialize(&pool, "mk_one", &key, entropy(0x22), 2_000)
            .await
            .expect_err("the initialiser must reject this fixture");
        let via_read_only = load_instance_state(&pool, "mk_one", &key, entropy(0x22))
            .await
            .expect_err("the read-only loader must reject the same fixture");

        assert_eq!(
            variant_name(&via_initializer),
            expected,
            "fixture {label:?} produced the wrong variant: {via_initializer:?}"
        );
        assert_eq!(
            variant_name(&via_initializer),
            variant_name(&via_read_only),
            "the entry points disagree on {label:?}: initialiser gave {via_initializer:?}, \
             read-only gave {via_read_only:?}"
        );
        assert_eq!(
            via_initializer.exit_code(),
            via_read_only.exit_code(),
            "the entry points must agree on the exit code for {label:?}"
        );

        pool.close().await;
    }
}

/// A secret that authenticates but is the wrong length is `InvalidSecretLength` from both.
///
/// Kept apart from the table above because it needs a *correctly sealed* blob rather than a
/// corrupted one: the length check runs after `open` succeeds. A sealed 16-byte value
/// authenticates fine and still must be rejected, which is the only way to reach that arm.
#[tokio::test]
#[doc = "Spec: SEC-2"]
async fn load_or_initialize_and_load_instance_state_agree_on_a_wrong_length_secret() {
    use crate::crypto::{SecretSealer, XChaCha20Poly1305Sealer};

    let (_directory, pool) = database().await;
    let key = MasterKey::from_bytes([0x99; 32]);
    let state = load_or_initialize(&pool, "mk_one", &key, entropy(0x61), 1_000)
        .await
        .expect("bootstrap");

    // Re-seal the audit key as 16 bytes instead of 32, under the same key id and AAD the
    // reader will use, so only the length check can reject it.
    let sealer = XChaCha20Poly1305Sealer::new(key.clone(), state.instance_id, entropy(0x62));
    let short = sealer.seal(
        "instance_secret",
        &[b"mk_one", b"audit_hmac_key"],
        &[0xAA; 16],
    );
    sqlx::query("UPDATE instance_secrets SET sealed = ? WHERE name = 'audit_hmac_key'")
        .bind(short)
        .execute(&pool)
        .await
        .expect("reseal audit key short");

    let via_initializer = load_or_initialize(&pool, "mk_one", &key, entropy(0x63), 2_000)
        .await
        .expect_err("a 16-byte secret must be rejected");
    let via_read_only = load_instance_state(&pool, "mk_one", &key, entropy(0x63))
        .await
        .expect_err("a 16-byte secret must be rejected");

    match (&via_initializer, &via_read_only) {
        (
            InstanceError::InvalidSecretLength {
                name: n1,
                length: l1,
                ..
            },
            InstanceError::InvalidSecretLength {
                name: n2,
                length: l2,
                ..
            },
        ) => {
            assert_eq!(n1, "audit_hmac_key");
            assert_eq!(n1, n2);
            assert_eq!(l1, &16);
            assert_eq!(l1, l2);
        }
        _ => panic!(
            "expected InvalidSecretLength from both, got {via_initializer:?} and {via_read_only:?}"
        ),
    }
}
