//! Cycle 27 — `instance_id` must never be substituted with a zero HKDF salt
//! (`ISSUE-0229`, follow-up to `ISSUE-0137`).
//!
//! `store/src/crypto.rs:303` derives sealer subkeys with
//! `Hkdf::<Sha256>::new(Some(instance_id), master_key)`, so `instance_id` is the **salt**: a zero
//! substitute is a *different key*, not a different AAD. A seal that "succeeds" under it produces
//! bytes nothing else can ever open.
//!
//! The three readers that used to substitute `[0u8; 16]` are covered by
//! `tests/instance_id_salt.rs` (reloader, shared `load_sealer` accessor) and by the tests here for
//! `AuthAppState::load_instance_id`, which is the unit-level seam behind `verify_totp_code`.

use llmroller_store::crypto::MasterKey;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};

use crate::auth::{AuthAppState, AuthConfig};

async fn pool_with_instance_meta(
    instance_id: Option<Vec<u8>>,
) -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("auth-c27.db");
    let pool = open_pool(&path, PoolRole::Writer).await.expect("pool");
    migrate(&pool).await.expect("migrate");

    // `instance_id` is `BLOB NOT NULL CHECK (length(instance_id) = 16)` in the schema, so a NULL or
    // short value can only be written with the constraint disabled — which is exactly what a
    // partially-restored database or an out-of-band writer does. Disabling it here is how we reach
    // the states the old fallback silently accepted.
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&pool)
        .await
        .expect("ignore checks");

    match instance_id {
        Some(bytes) => {
            sqlx::query(
                "INSERT INTO instance_meta (singleton, instance_id, active_master_key_id,
                    rotation_state, config_version, updated_at)
                 VALUES (1, ?, 'key-0', 'idle', 1, 1)",
            )
            .bind(bytes)
            .execute(&pool)
            .await
            .expect("instance_meta");
        }
        None => {
            // No row at all: the reachable "missing" case.
        }
    }
    (dir, pool)
}

fn state_with(pool: sqlx::SqlitePool) -> AuthAppState {
    let mut state = AuthAppState::new(
        pool,
        AuthConfig {
            dev_insecure_cookies: true,
            require_tls: false,
            trusted_proxies: vec![],
        },
    );
    state.master_key = Some(MasterKey::from_bytes([0x5A; 32]));
    state.master_key_id = "key-0".to_string();
    state
}

/// A missing `instance_meta` row must be an error, not a zero salt.
///
/// Pre-fix this returned `Ok([0u8; 16])`, and every TOTP operation then hashed its sealer key
/// against a publicly-known constant.
#[tokio::test]
#[doc = "Spec: AUTH-9, SEC-5; ISSUE-0229"]
async fn a_missing_instance_meta_row_is_an_error() {
    let (_dir, pool) = pool_with_instance_meta(None).await;
    let state = state_with(pool);

    let result = state.load_instance_id().await;
    assert!(
        result.is_err(),
        "no instance_meta row must be an internal error; got {result:?}"
    );
}

/// A short id must be an error too.
///
/// The old code swallowed this with `try_into().unwrap_or([0_u8; 16])`. Reaching this state needs
/// the schema's length CHECK bypassed, which is why the fixture disables it — a direct writer or a
/// partially-restored dump can produce exactly this.
#[tokio::test]
#[doc = "Spec: AUTH-9, SEC-5; ISSUE-0229"]
async fn a_short_instance_id_is_an_error() {
    let (_dir, pool) = pool_with_instance_meta(Some(vec![0x11; 8])).await;
    let state = state_with(pool);

    let result = state.load_instance_id().await;
    assert!(
        result.is_err(),
        "an 8-byte instance_id must be an internal error, not a zero substitution; got {result:?}"
    );
}

/// A correctly-sized id is returned verbatim — the non-regression that stops "always error" from
/// passing the two tests above.
#[tokio::test]
#[doc = "Spec: AUTH-9, SEC-5; ISSUE-0229"]
async fn a_correct_instance_id_is_returned_verbatim() {
    let id = [0x33u8; 16];
    let (_dir, pool) = pool_with_instance_meta(Some(id.to_vec())).await;
    let state = state_with(pool);

    let result = state.load_instance_id().await.expect("must succeed");
    assert_eq!(result, id);
}

/// The verification path must **not** succeed when the id is missing.
///
/// Honest note on what this can and cannot prove: `verify_totp_code` returns
/// `Err(AuthError::Internal)` both when the id lookup fails and when a zero-salt sealer fails to
/// decrypt (the `Err(_)` arm maps a decryption error the same way), so asserting the error variant
/// alone does **not** discriminate the two implementations. What it does pin is that the path never
/// returns `Ok(true)` — no login succeeds on a substituted salt — which is the security property.
/// The discriminating assertions for the id lookup itself are the three tests above, which call the
/// seam directly.
#[tokio::test]
#[doc = "Spec: AUTH-9, SEC-5; ISSUE-0229"]
async fn totp_verification_never_succeeds_without_a_real_instance_id() {
    let (_dir, pool) = pool_with_instance_meta(None).await;
    let state = state_with(pool);
    let user_id: llmroller_core::ids::UserId = "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().expect("user");

    let result = state
        .verify_totp_code_for_test(user_id, "123456", 1_700_000_000_000)
        .await;
    assert!(
        !matches!(result, Ok(true)),
        "no TOTP verification may succeed when instance_meta is unreadable; got {result:?}"
    );
}
