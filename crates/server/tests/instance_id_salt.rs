//! Cycle 27 — a zero `instance_id` derives the **wrong encryption key**
//! (`ISSUE-0229`, follow-up to `ISSUE-0137`).
//!
//! `instance_id` is not merely AAD. In `crates/store/src/crypto.rs:303`:
//!
//! ```ignore
//! let hkdf = Hkdf::<Sha256>::new(Some(instance_id), master_key.as_bytes());
//! ```
//!
//! It is the **HKDF salt**, so a zero salt derives a *different* set of subkeys rather than
//! merely tagging the same key differently. Data sealed under a zero-salt fallback is therefore
//! permanently unreadable once the real id loads — lost upstream credentials, locked-out TOTP
//! accounts — and nothing reports an error.
//!
//! Three readers substituted `[0u8; 16]` when the id was missing or malformed:
//!
//! | Site | Behaviour before |
//! |---|---|
//! | `reloader::load_instance_id` | `.ok().flatten()` then `unwrap_or([0; 16])` — swallows the query error *and* the length mismatch |
//! | `providers.rs` | `unwrap_or_else(\|\| vec![0; 16])` then `try_into().unwrap_or([0; 16])` |
//! | `auth.rs::verify_totp_code` | the same pair |
//!
//! Spec: `SEC-5`, `SEC-13`, `ADR-11`, `REL-4`.

use std::sync::Arc;

use llmroller_core::seams::OsEntropy;
use llmroller_store::crypto::{MasterKey, SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use tempfile::tempdir;

const DOMAIN: &str = "upstream_key";

async fn fresh_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("c27.db");
    let pool = open_pool(&path, PoolRole::Writer).await.expect("pool");
    migrate(&pool).await.expect("migrate");
    (dir, pool)
}

/// Real 16-byte instance ids. Fixed dummies — never anything derived from a real key.
const ID_A: [u8; 16] = [0x11; 16];
const ID_B: [u8; 16] = [0x22; 16];

// ---------------------------------------------------------------------------
// The characterization test: why a zero salt is catastrophic
// ---------------------------------------------------------------------------

/// Two sealers with the **same master key** but different `instance_id`s cannot read each other.
///
/// This is the executable form of the reason the zero fallback matters. If `instance_id` were only
/// AAD, the two would interoperate and the fallback would be cosmetic; because it is the HKDF salt,
/// they derive different subkeys and the sealed bytes are permanently unreadable across the change.
///
/// Writing it as a test means a future reader cannot mistake the fallback for a harmless default.
#[test]
#[doc = "Spec: SEC-5, SEC-13; ISSUE-0229"]
fn instance_id_is_the_hkdf_salt_not_just_aad() {
    let master = MasterKey::from_bytes([0x5A; 32]);
    let sealer_a = XChaCha20Poly1305Sealer::new(master.clone(), ID_A, Arc::new(OsEntropy));
    let sealer_b = XChaCha20Poly1305Sealer::new(master, ID_B, Arc::new(OsEntropy));

    let sealed = sealer_a
        .try_seal(DOMAIN, &[b"key-id"], b"super-secret-upstream-credential")
        .expect("seal under id A");

    assert!(
        sealer_b.open(DOMAIN, &[b"key-id"], &sealed).is_err(),
        "a sealer built with a different instance_id must NOT decrypt — proving instance_id is \
         the HKDF salt (a different key), not merely AAD (the same key, different tag)"
    );

    // And the origin still reads its own output, so the failure above is the id and not the seal.
    let plain = sealer_a
        .open(DOMAIN, &[b"key-id"], &sealed)
        .expect("round trip under the same id");
    assert_eq!(
        llmroller_core::seams::ExposeSecret::expose_secret(&plain),
        b"super-secret-upstream-credential"
    );
}

/// A zero id behaves exactly like any other distinct id: mutually unreadable with a real one.
///
/// The specific combination the fallback created.
#[test]
#[doc = "Spec: SEC-5, SEC-13; ISSUE-0229"]
fn a_zero_instance_id_cannot_read_data_sealed_under_the_real_id() {
    let master = MasterKey::from_bytes([0x5A; 32]);
    let zero = [0u8; 16];
    let real = ID_A;

    let real_sealer = XChaCha20Poly1305Sealer::new(master.clone(), real, Arc::new(OsEntropy));
    let zero_sealer = XChaCha20Poly1305Sealer::new(master, zero, Arc::new(OsEntropy));

    let sealed_by_real = real_sealer
        .try_seal(DOMAIN, &[b"key-id"], b"credential")
        .expect("seal");

    assert!(
        zero_sealer
            .open(DOMAIN, &[b"key-id"], &sealed_by_real)
            .is_err(),
        "the zero-salt fallback cannot read data sealed under the real instance_id — this is the \
         permanent data loss the fallback risked"
    );
}

// ---------------------------------------------------------------------------
// #1 — the reloader must propagate, not substitute
// ---------------------------------------------------------------------------

/// A missing `instance_id` row must make `load_initial_snapshot` fail.
///
/// Pre-fix it returns `Ok` with a snapshot whose `instance_id` is all zeros, which every sealer
/// built from that snapshot then uses as the HKDF salt.
/// What can actually be missing is the **row**, not the column.
///
/// The schema forbids a null or short id:
///
/// ```sql
/// instance_id BLOB NOT NULL CHECK (length(instance_id) = 16)
/// ```
///
/// (`0001_initial.sql:11`). So the only reachable trigger for the reloader fallback is an absent
/// `instance_meta` row — an uninitialised or partially-restored database — plus a query error.
/// Both must be errors rather than a zero-salt snapshot.
#[tokio::test]
#[doc = "Spec: SEC-5, REL-4; ISSUE-0229"]
async fn reloader_rejects_a_database_with_no_instance_meta_row() {
    let (_dir, pool) = fresh_pool().await;

    // No `instance_meta` row at all: `fetch_optional` returns `None`, which the old code turned
    // into `[0u8; 16]`.
    let result = llmroller_server::reloader::load_initial_snapshot(&pool).await;

    assert!(
        result.is_err(),
        "a database with no instance_meta row must not yield a snapshot sealed under a zero salt; \
         got instance_id {:?}",
        result.as_ref().map(|s| s.instance_id).ok()
    );
}

/// The snapshot must never be published with an all-zero `instance_id`, whatever the cause.
///
/// Stated as a separate property because it is the invariant that matters: every sealer built from
/// that snapshot would use the zero id as its HKDF salt.
#[tokio::test]
#[doc = "Spec: SEC-5, REL-4; ISSUE-0229"]
async fn reloader_never_publishes_a_zero_instance_id() {
    let (_dir, pool) = fresh_pool().await;

    match llmroller_server::reloader::load_initial_snapshot(&pool).await {
        Err(_) => {}
        Ok(snapshot) => assert_ne!(
            snapshot.instance_id, [0u8; 16],
            "a snapshot carrying an all-zero instance_id would key every sealer on a constant"
        ),
    }
}

/// The non-regression: a correct id still loads, and the snapshot carries it verbatim.
///
/// Without this, "always return an error" would satisfy both tests above.
#[tokio::test]
#[doc = "Spec: SEC-5, REL-4; ISSUE-0229"]
async fn reloader_accepts_a_correct_instance_id() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("c27-ok.db");
    let pool = open_pool(&path, PoolRole::Writer).await.expect("pool");
    migrate(&pool).await.expect("migrate");
    llmroller_store::settings::seed_defaults(&pool, None, 1)
        .await
        .expect("seed");

    let master = MasterKey::from_bytes([0x5A; 32]);
    let state = llmroller_store::instance::load_or_initialize(
        &pool,
        "key-0",
        &master,
        Arc::new(OsEntropy),
        1,
    )
    .await
    .expect("initialize");

    let snapshot = llmroller_server::reloader::load_initial_snapshot(&pool)
        .await
        .expect("a correctly initialised database must load");
    assert_eq!(
        snapshot.instance_id, state.instance_id,
        "the snapshot must carry the real instance_id, not a substitute"
    );
}

// ---------------------------------------------------------------------------
// #2 — the two admin sites must error rather than seal with a zero salt
// ---------------------------------------------------------------------------

/// A helper that mirrors what the admin handlers must NOT do: build a sealer from a zero id.
///
/// Asserted here so the defect's shape is executable: if a handler falls back to zeros it will
/// produce sealed bytes that `a_zero_instance_id_cannot_read_data_sealed_under_the_real_id` proves
/// are unreadable.
#[test]
#[doc = "Spec: SEC-5, SEC-13; ISSUE-0229"]
fn a_zero_salt_sealer_produces_bytes_the_real_sealer_cannot_open() {
    let master = MasterKey::from_bytes([0x5A; 32]);
    let zero_sealer = XChaCha20Poly1305Sealer::new(master.clone(), [0u8; 16], Arc::new(OsEntropy));
    let real_sealer = XChaCha20Poly1305Sealer::new(master, ID_A, Arc::new(OsEntropy));

    let wrongly_sealed = zero_sealer
        .try_seal(DOMAIN, &[b"key-id"], b"totp-secret")
        .expect("the fallback seal *succeeds* — that is the problem");

    assert!(
        real_sealer
            .open(DOMAIN, &[b"key-id"], &wrongly_sealed)
            .is_err(),
        "a seal that succeeded under the zero-salt fallback is unreadable once the real \
         instance_id loads: silent, permanent data loss"
    );
}

/// `ApiAppState::sealer()` must error when `instance_meta.instance_id` is absent.
///
/// This is the shared accessor the two admin handlers now use, so both are covered by one place.
#[tokio::test]
#[doc = "Spec: SEC-5, SEC-13; ISSUE-0229"]
async fn admin_sealer_errors_when_there_is_no_instance_meta_row() {
    let (_dir, pool) = fresh_pool().await;
    // No row: the case `providers.rs` turned into `vec![0u8; 16]`.

    let master = MasterKey::from_bytes([0x5A; 32]);
    let auth = llmroller_admin::AuthAppState::new(
        pool.clone(),
        llmroller_admin::auth::AuthConfig {
            dev_insecure_cookies: true,
            require_tls: false,
            trusted_proxies: vec![],
        },
    );
    let state = llmroller_admin::ApiAppState::new(auth, master, "key-0".to_string());

    let result = llmroller_admin::api::load_sealer(&state).await;
    assert!(
        result.is_err(),
        "no instance_id must be an error, not a sealer built with a zero HKDF salt"
    );
}

/// The non-regression for the admin accessor.
#[tokio::test]
#[doc = "Spec: SEC-5, SEC-13; ISSUE-0229"]
async fn admin_sealer_works_with_a_correct_instance_id() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("c27-admin.db");
    let pool = open_pool(&path, PoolRole::Writer).await.expect("pool");
    migrate(&pool).await.expect("migrate");
    let master = MasterKey::from_bytes([0x5A; 32]);
    let instance = llmroller_store::instance::load_or_initialize(
        &pool,
        "key-0",
        &master,
        Arc::new(OsEntropy),
        1,
    )
    .await
    .expect("initialize");

    let auth = llmroller_admin::AuthAppState::new(
        pool.clone(),
        llmroller_admin::auth::AuthConfig {
            dev_insecure_cookies: true,
            require_tls: false,
            trusted_proxies: vec![],
        },
    );
    let state = llmroller_admin::ApiAppState::new(auth, master, "key-0".to_string());

    let sealer = llmroller_admin::api::load_sealer(&state)
        .await
        .expect("a correctly initialised database must produce a sealer");

    // Round-trip proves the sealer is usable, and that the id it used was the real one: a sealer
    // built from zeros would still round-trip against itself, so also compare against a sealer
    // built independently from the real id.
    let sealed = sealer
        .try_seal(DOMAIN, &[b"k"], b"value")
        .expect("seal via accessor");
    let real = XChaCha20Poly1305Sealer::new(
        MasterKey::from_bytes([0x5A; 32]),
        instance.instance_id,
        Arc::new(OsEntropy),
    );
    let opened = real
        .open(DOMAIN, &[b"k"], &sealed)
        .expect("open by real id");
    assert_eq!(
        llmroller_core::seams::ExposeSecret::expose_secret(&opened),
        b"value",
        "the accessor must have used the real instance_id as the HKDF salt"
    );
}
