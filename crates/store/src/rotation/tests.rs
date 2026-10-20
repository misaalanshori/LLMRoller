//! Offline resumable master-key rotation tests.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#922-canonical-aead-associated-data-aad-specification

use std::sync::Arc;

use llmroller_core::seams::{DeterministicRng, EntropySource, ExposeSecret};
use sqlx::SqlitePool;
use tempfile::TempDir;

use super::{RotationError, rotate_master_key};
use crate::audit::{AuditEntry, GENESIS_HASH, entry_hash};
use crate::crypto::{MasterKey, SecretSealer, XChaCha20Poly1305Sealer};
use crate::instance::{InstanceState, load_or_initialize};
use crate::migrations::migrate;
use crate::pool::{Role, open_pool};

const USER_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const PROVIDER_GLOBAL: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const SNAPSHOT_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const CLIENT_KEY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const USAGE_EVENT_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
const TRACE_REQUEST_ID: &str = "request-rotation";
const TRACE_SESSION_HASH: &str = "0123456789abcdef0123456789abcdef";
const TRACE_BLOCK_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn entropy(seed: u64) -> Arc<dyn EntropySource> {
    Arc::new(DeterministicRng::new(seed))
}

fn old_key() -> MasterKey {
    MasterKey::from_bytes([0x51; 32])
}

fn new_key() -> MasterKey {
    MasterKey::from_bytes([0x52; 32])
}

fn upstream_id(index: usize) -> String {
    format!("01ARZ3NDEKTSV4RRFFQ69G{index:04}")
}

async fn setup() -> (
    TempDir,
    SqlitePool,
    InstanceState,
    XChaCha20Poly1305Sealer,
    Arc<dyn EntropySource>,
) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("rotation.db");
    let pool = open_pool(&path, Role::Writer)
        .await
        .expect("writer pool must open");
    migrate(&pool).await.expect("schema must migrate");
    let entropy = entropy(0x5151);
    let state = load_or_initialize(&pool, "mk_old", &old_key(), Arc::clone(&entropy), 1_000)
        .await
        .expect("instance bootstrap");
    let sealer = XChaCha20Poly1305Sealer::new(old_key(), state.instance_id, Arc::clone(&entropy));
    (directory, pool, state, sealer, entropy)
}

fn seal_totp(sealer: &XChaCha20Poly1305Sealer, key_id: &str, user_id: &str) -> Vec<u8> {
    sealer
        .try_seal(
            "totp_secret",
            &[key_id.as_bytes(), user_id.as_bytes()],
            b"totp-test-secret",
        )
        .expect("totp seal")
}

async fn seed_user(pool: &SqlitePool, sealer: &XChaCha20Poly1305Sealer, key_id: &str) {
    let totp = seal_totp(sealer, key_id, USER_A);
    sqlx::query(
        "INSERT INTO users \
         (id, username, password_hash, role, totp_secret_enc, totp_master_key_id, created_at) \
         VALUES (?, 'rotation-user', 'hash', 'user', ?, ?, 1)",
    )
    .bind(USER_A)
    .bind(totp)
    .bind(key_id)
    .execute(pool)
    .await
    .expect("user insert");
}

async fn seed_provider(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO providers \
         (id, ownership_scope, kind, name, base_url, config_json, created_at) \
         VALUES (?, 'global', 'custom', 'rotation-provider', 'https://example.test', '{}', 1)",
    )
    .bind(PROVIDER_GLOBAL)
    .execute(pool)
    .await
    .expect("provider insert");
}

async fn seed_upstream_keys(
    pool: &SqlitePool,
    sealer: &XChaCha20Poly1305Sealer,
    key_id: &str,
    count: usize,
) {
    for index in 0..count {
        let id = upstream_id(index);
        let plaintext = format!("upstream-secret-{index:04}");
        let sealed = sealer
            .try_seal(
                "upstream_key",
                &[key_id.as_bytes(), id.as_bytes(), PROVIDER_GLOBAL.as_bytes()],
                plaintext.as_bytes(),
            )
            .expect("upstream seal");
        sqlx::query(
            "INSERT INTO upstream_keys \
             (id, provider_id, label, secret_enc, master_key_id, secret_last4, created_at) \
             VALUES (?, ?, ?, ?, ?, '0000', 1)",
        )
        .bind(&id)
        .bind(PROVIDER_GLOBAL)
        .bind(format!("key-{index}"))
        .bind(sealed)
        .bind(key_id)
        .execute(pool)
        .await
        .expect("upstream key insert");
    }
}

async fn seed_usage_snapshot(pool: &SqlitePool, sealer: &XChaCha20Poly1305Sealer, key_id: &str) {
    let sealed = sealer
        .try_seal(
            "usage_raw",
            &[key_id.as_bytes(), SNAPSHOT_ID.as_bytes()],
            br#"{"plan":"raw-test"}"#,
        )
        .expect("snapshot seal");
    sqlx::query(
        "INSERT INTO upstream_usage_snapshots \
         (id, upstream_key_id, fetched_at, http_status, windows_json, raw_json_enc, \
          master_key_id, sanitized_json) \
         VALUES (?, ?, 1, 200, '[]', ?, ?, '{}')",
    )
    .bind(SNAPSHOT_ID)
    .bind(upstream_id(0))
    .bind(sealed)
    .bind(key_id)
    .execute(pool)
    .await
    .expect("snapshot insert");
}

async fn seed_trace_block(pool: &SqlitePool, sealer: &XChaCha20Poly1305Sealer, key_id: &str) {
    let sealed = sealer
        .try_seal(
            "trace_block",
            &[
                key_id.as_bytes(),
                USER_A.as_bytes(),
                TRACE_BLOCK_HASH.as_bytes(),
            ],
            b"trace-block-plaintext",
        )
        .expect("trace block seal");
    sqlx::query(
        "INSERT INTO trace_blocks \
         (block_hash, owner_user_id, payload_enc, master_key_id, byte_len, last_referenced_at) \
         VALUES (?, ?, ?, ?, 20, 1)",
    )
    .bind(TRACE_BLOCK_HASH)
    .bind(USER_A)
    .bind(sealed)
    .bind(key_id)
    .execute(pool)
    .await
    .expect("trace block insert");
}

async fn seed_trace_record(pool: &SqlitePool, sealer: &XChaCha20Poly1305Sealer, key_id: &str) {
    let sealed = sealer
        .try_seal(
            "trace_record",
            &[
                key_id.as_bytes(),
                TRACE_REQUEST_ID.as_bytes(),
                USER_A.as_bytes(),
                TRACE_SESSION_HASH.as_bytes(),
            ],
            b"trace-record-plaintext",
        )
        .expect("trace record seal");
    sqlx::query(
        "INSERT INTO trace_records \
         (usage_event_id, request_id, ts, owner_user_id, client_key_id, session_hash, \
          canonical_id, protocol, payload_enc, master_key_id, expires_at) \
         VALUES (?, ?, 1, ?, ?, ?, 'vendor/model', 'chat', ?, ?, 999999)",
    )
    .bind(USAGE_EVENT_ID)
    .bind(TRACE_REQUEST_ID)
    .bind(USER_A)
    .bind(CLIENT_KEY_ID)
    .bind(TRACE_SESSION_HASH)
    .bind(sealed)
    .bind(key_id)
    .execute(pool)
    .await
    .expect("trace record insert");
}

async fn seed_usage_event(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO client_keys \
         (id, owner_user_id, label, prefix, key_hash, created_at) \
         VALUES (?, ?, 'rotation-key', 'llr_rot', 'hash-rotation', 1)",
    )
    .bind(CLIENT_KEY_ID)
    .bind(USER_A)
    .execute(pool)
    .await
    .expect("client key insert");

    sqlx::query(
        "INSERT INTO usage_events \
         (id, ts, request_id, client_key_id, owner_user_id, endpoint, status, stream, \
          cost_micro_usd, cost_source) \
         VALUES (?, 1, 'request-usage', ?, ?, 'chat', 200, 0, 0, 'provider')",
    )
    .bind(USAGE_EVENT_ID)
    .bind(CLIENT_KEY_ID)
    .bind(USER_A)
    .execute(pool)
    .await
    .expect("usage event insert");
}

async fn seed_all(
    pool: &SqlitePool,
    sealer: &XChaCha20Poly1305Sealer,
    key_id: &str,
    upstream_count: usize,
) {
    seed_user(pool, sealer, key_id).await;
    seed_provider(pool).await;
    seed_upstream_keys(pool, sealer, key_id, upstream_count).await;
    seed_usage_snapshot(pool, sealer, key_id).await;
    seed_usage_event(pool).await;
    seed_trace_block(pool, sealer, key_id).await;
    seed_trace_record(pool, sealer, key_id).await;
}

async fn count_stale_rows(pool: &SqlitePool, old_key_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT \
         (SELECT COUNT(*) FROM instance_secrets WHERE master_key_id = ?) + \
         (SELECT COUNT(*) FROM users WHERE totp_master_key_id = ?) + \
         (SELECT COUNT(*) FROM upstream_keys WHERE master_key_id = ?) + \
         (SELECT COUNT(*) FROM upstream_usage_snapshots WHERE master_key_id = ?) + \
         (SELECT COUNT(*) FROM trace_blocks WHERE master_key_id = ?) + \
         (SELECT COUNT(*) FROM trace_records WHERE master_key_id = ?)",
    )
    .bind(old_key_id)
    .bind(old_key_id)
    .bind(old_key_id)
    .bind(old_key_id)
    .bind(old_key_id)
    .bind(old_key_id)
    .fetch_one(pool)
    .await
    .expect("stale row count")
}

fn new_sealer(state: &InstanceState, entropy: Arc<dyn EntropySource>) -> XChaCha20Poly1305Sealer {
    XChaCha20Poly1305Sealer::new(new_key(), state.instance_id, entropy)
}

#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn rotation_reseals_all_six_tables_in_500_row_batches_and_preserves_secrets() {
    let (_directory, pool, state, old_sealer, entropy) = setup().await;
    seed_all(&pool, &old_sealer, "mk_old", 501).await;

    let report = rotate_master_key(
        &pool,
        old_key(),
        new_key(),
        "mk_new",
        Arc::clone(&entropy),
        10_000,
    )
    .await
    .expect("rotation");

    assert!(!report.resumed);
    assert_eq!(report.rows_re_sealed, 507);
    assert_eq!(
        report.batches, 7,
        "500-row batch boundary must split upstream keys"
    );
    assert_eq!(count_stale_rows(&pool, "mk_old").await, 0);

    let header: (String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT active_master_key_id, rotation_state, rotation_cursor_table, \
         rotation_cursor_id FROM instance_meta WHERE singleton = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("instance meta");
    assert_eq!(header.0, "mk_new");
    assert_eq!(header.1, "idle");
    assert_eq!(header.2, None);
    assert_eq!(header.3, None);

    let reloaded = load_or_initialize(&pool, "mk_new", &new_key(), Arc::clone(&entropy), 20_000)
        .await
        .expect("reload with new key");
    assert_eq!(
        reloaded.session_hmac_secret.expose_secret(),
        state.session_hmac_secret.expose_secret(),
        "rotation must preserve session_hmac_secret"
    );
    assert_eq!(
        reloaded.audit_hmac_key.expose_secret(),
        state.audit_hmac_key.expose_secret(),
        "rotation must preserve audit_hmac_key"
    );

    let sealer = new_sealer(&state, Arc::clone(&entropy));
    let last_id = upstream_id(500);
    let last_sealed: Vec<u8> =
        sqlx::query_scalar("SELECT secret_enc FROM upstream_keys WHERE id = ?")
            .bind(&last_id)
            .fetch_one(&pool)
            .await
            .expect("last upstream blob");
    let last_plain = sealer
        .open(
            "upstream_key",
            &[b"mk_new", last_id.as_bytes(), PROVIDER_GLOBAL.as_bytes()],
            &last_sealed,
        )
        .expect("open last upstream key");
    assert_eq!(
        last_plain.expose_secret(),
        b"upstream-secret-0500".as_slice()
    );

    let audit: (
        i64,
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT seq, id, action, target_id, prev_hash, hash, detail_json \
             FROM audit_log ORDER BY seq DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("rotation audit row");
    assert_eq!(audit.0, 1);
    assert_eq!(audit.2, "master_key.rotated");
    assert_eq!(audit.3.as_deref(), Some("mk_new"));
    assert_eq!(audit.4, GENESIS_HASH);
    let entry = AuditEntry {
        id: &audit.1,
        ts: 10_000,
        actor_user_id: None,
        ip_addr: None,
        action: "master_key.rotated",
        target_type: "instance_meta",
        target_id: Some("mk_new"),
        detail_json: audit.6.as_deref(),
    };
    assert_eq!(
        entry_hash(
            state.audit_hmac_key.expose_secret(),
            &entry,
            1,
            GENESIS_HASH
        ),
        audit.5
    );

    pool.close().await;
}

async fn reseal_instance_secrets(
    pool: &SqlitePool,
    old_sealer: &XChaCha20Poly1305Sealer,
    new_sealer: &XChaCha20Poly1305Sealer,
) {
    let rows: Vec<(String, Vec<u8>)> =
        sqlx::query_as("SELECT name, sealed FROM instance_secrets ORDER BY name")
            .fetch_all(pool)
            .await
            .expect("instance secrets");
    for (name, sealed) in rows {
        let plain = old_sealer
            .open("instance_secret", &[b"mk_old", name.as_bytes()], &sealed)
            .expect("open old instance secret");
        let resealed = new_sealer
            .try_seal(
                "instance_secret",
                &[b"mk_new", name.as_bytes()],
                plain.expose_secret(),
            )
            .expect("reseal instance secret");
        sqlx::query(
            "UPDATE instance_secrets SET sealed = ?, master_key_id = 'mk_new' WHERE name = ?",
        )
        .bind(resealed)
        .bind(name)
        .execute(pool)
        .await
        .expect("update instance secret");
    }
}

async fn reseal_user_totp(
    pool: &SqlitePool,
    old_sealer: &XChaCha20Poly1305Sealer,
    new_sealer: &XChaCha20Poly1305Sealer,
) {
    let sealed: Vec<u8> = sqlx::query_scalar("SELECT totp_secret_enc FROM users WHERE id = ?")
        .bind(USER_A)
        .fetch_one(pool)
        .await
        .expect("old totp");
    let plain = old_sealer
        .open("totp_secret", &[b"mk_old", USER_A.as_bytes()], &sealed)
        .expect("open old totp");
    let resealed = new_sealer
        .try_seal(
            "totp_secret",
            &[b"mk_new", USER_A.as_bytes()],
            plain.expose_secret(),
        )
        .expect("reseal totp");
    sqlx::query("UPDATE users SET totp_secret_enc = ?, totp_master_key_id = 'mk_new' WHERE id = ?")
        .bind(resealed)
        .bind(USER_A)
        .execute(pool)
        .await
        .expect("update totp");
}

async fn reseal_first_upstream(
    pool: &SqlitePool,
    old_sealer: &XChaCha20Poly1305Sealer,
    new_sealer: &XChaCha20Poly1305Sealer,
) {
    let id = upstream_id(0);
    let sealed: Vec<u8> = sqlx::query_scalar("SELECT secret_enc FROM upstream_keys WHERE id = ?")
        .bind(&id)
        .fetch_one(pool)
        .await
        .expect("old upstream");
    let plain = old_sealer
        .open(
            "upstream_key",
            &[b"mk_old", id.as_bytes(), PROVIDER_GLOBAL.as_bytes()],
            &sealed,
        )
        .expect("open old upstream");
    let resealed = new_sealer
        .try_seal(
            "upstream_key",
            &[b"mk_new", id.as_bytes(), PROVIDER_GLOBAL.as_bytes()],
            plain.expose_secret(),
        )
        .expect("reseal upstream");
    sqlx::query("UPDATE upstream_keys SET secret_enc = ?, master_key_id = 'mk_new' WHERE id = ?")
        .bind(resealed)
        .bind(id)
        .execute(pool)
        .await
        .expect("update upstream");
}

#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn rotation_resumes_from_the_saved_table_and_row_cursor() {
    let (_directory, pool, state, old_sealer, entropy) = setup().await;
    seed_all(&pool, &old_sealer, "mk_old", 3).await;

    let new = new_sealer(&state, Arc::clone(&entropy));
    reseal_instance_secrets(&pool, &old_sealer, &new).await;
    reseal_user_totp(&pool, &old_sealer, &new).await;
    reseal_first_upstream(&pool, &old_sealer, &new).await;

    sqlx::query(
        "UPDATE instance_meta SET rotation_state = 'in_progress', \
         rotation_from_key_id = 'mk_old', rotation_to_key_id = 'mk_new', \
         rotation_cursor_table = 'upstream_keys', rotation_cursor_id = ? \
         WHERE singleton = 1",
    )
    .bind(upstream_id(0))
    .execute(&pool)
    .await
    .expect("simulate interrupted rotation");

    let report = rotate_master_key(
        &pool,
        old_key(),
        new_key(),
        "mk_new",
        Arc::clone(&entropy),
        11_000,
    )
    .await
    .expect("resumed rotation");

    assert!(report.resumed);
    assert_eq!(report.rows_re_sealed, 5);
    assert_eq!(count_stale_rows(&pool, "mk_old").await, 0);
    let active: (String, String) = sqlx::query_as(
        "SELECT active_master_key_id, rotation_state FROM instance_meta WHERE singleton = 1",
    )
    .fetch_one(&pool)
    .await
    .expect("meta");
    assert_eq!(active, ("mk_new".to_owned(), "idle".to_owned()));

    pool.close().await;
}

#[tokio::test]
#[doc = "Partial: SEC-2"]
async fn resume_rejects_a_different_new_key_identifier() {
    let (_directory, pool, _state, _old_sealer, entropy) = setup().await;
    sqlx::query(
        "UPDATE instance_meta SET rotation_state = 'in_progress', \
         rotation_from_key_id = 'mk_old', rotation_to_key_id = 'mk_new', \
         rotation_cursor_table = 'instance_secrets' WHERE singleton = 1",
    )
    .execute(&pool)
    .await
    .expect("simulate interrupted rotation");

    let error = rotate_master_key(
        &pool,
        old_key(),
        new_key(),
        "mk_other",
        Arc::clone(&entropy),
        12_000,
    )
    .await
    .expect_err("wrong new key id");
    assert!(matches!(error, RotationError::ResumeMismatch { .. }));
    assert_eq!(error.exit_code(), 3);

    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: SEC-2"]
async fn rotation_rejects_a_wrong_old_key_without_starting_state() {
    let (_directory, pool, _state, old_sealer, entropy) = setup().await;
    seed_all(&pool, &old_sealer, "mk_old", 1).await;

    let error = rotate_master_key(
        &pool,
        MasterKey::from_bytes([0x99; 32]),
        new_key(),
        "mk_new",
        Arc::clone(&entropy),
        13_000,
    )
    .await
    .expect_err("wrong old key");
    assert!(matches!(
        error,
        RotationError::InstanceAuthenticatedFailure { .. }
    ));
    assert_eq!(error.exit_code(), 3);

    let state: String =
        sqlx::query_scalar("SELECT rotation_state FROM instance_meta WHERE singleton = 1")
            .fetch_one(&pool)
            .await
            .expect("rotation state");
    assert_eq!(state, "idle");

    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: SEC-1"]
async fn rotation_reports_aead_tampering_with_exit_code_7() {
    let (_directory, pool, _state, old_sealer, entropy) = setup().await;
    seed_all(&pool, &old_sealer, "mk_old", 1).await;
    sqlx::query("UPDATE upstream_keys SET secret_enc = X'00' WHERE id = ?")
        .bind(upstream_id(0))
        .execute(&pool)
        .await
        .expect("tamper upstream key");

    let error = rotate_master_key(
        &pool,
        old_key(),
        new_key(),
        "mk_new",
        Arc::clone(&entropy),
        14_000,
    )
    .await
    .expect_err("tampered row");
    assert!(matches!(
        error,
        RotationError::Authentication {
            table: "upstream_keys",
            ..
        }
    ));
    assert_eq!(error.exit_code(), 7);

    let state: String =
        sqlx::query_scalar("SELECT rotation_state FROM instance_meta WHERE singleton = 1")
            .fetch_one(&pool)
            .await
            .expect("rotation state");
    assert_eq!(state, "in_progress");

    pool.close().await;
}
