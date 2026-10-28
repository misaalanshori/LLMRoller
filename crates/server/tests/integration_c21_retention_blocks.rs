//! Retention reduction must actually purge, and must leave no orphaned trace blocks
//! (`ISSUE-0194`, `TRC-1`, `TRC-4`, `ADR-9`).
//!
//! # What the code actually does
//!
//! The issue says the handler "purges `trace_records` when retention is reduced, but leaves
//! `trace_blocks` rows that nothing references any more". **There is no purge at all.**
//! `put_admin_traceability_handler` counts the rows a purge *would* affect (to fill
//! `affected_rows` in the 409 confirmation body) and then, when `confirm_purge: true`, only
//! rewrites the `traceability_config` settings row. Zero `DELETE` statements execute, so no
//! records are removed and nothing becomes orphaned.
//!
//! That is worse than the filed defect in one way and better in another: expired records linger
//! indefinitely after retention is lowered (the periodic sweep only removes rows whose
//! `expires_at` has already passed, and lowering retention does not retroactively set it), but
//! blocks cannot be orphaned *by this path* because it deletes nothing.
//!
//! # References are indexable, which decides the design
//!
//! `trace_record_blocks(usage_event_id, owner_user_id, block_hash)` is a join table with
//! `idx_trace_record_blocks_ref(owner_user_id, block_hash)`, populated at insert time
//! (`store/src/traces.rs:295-300`). So "is this block still referenced?" is an indexed
//! `NOT EXISTS`, **not** a JSON walk — the cheap branch of the brief's fork.
//!
//! A correct sweep already exists and is batched: `retention::sweep_orphaned_trace_blocks`
//! deletes in `LIMIT chunk_limit` chunks, guarded by exactly that `NOT EXISTS`. The fix reuses
//! it rather than writing a second one.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tempfile::TempDir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy, SystemClock};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

const ADMIN_SESSION: &str = "c21_trc_admin_session_0123456789ab";
const ADMIN_CSRF: &str = "c21_trc_admin_csrf_0123456789abcde";

const DAY_MS: i64 = 86_400_000;

struct Harness {
    _dir: TempDir,
    pool: sqlx::SqlitePool,
    app: axum::Router,
    admin_id: UserId,
    client_key_id: String,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("c21_retention.db"), PoolRole::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    seed_defaults(&pool, None, 1).await.expect("seed");

    let master_key = MasterKey::from_bytes([0xF2; 32]);
    let instance = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance");

    let now = SystemClock::new().now_ms();
    let admin = create_user(
        &pool,
        &ActorScope::system(SystemTask::Writer, SystemSeal::store()),
        NewUser {
            username: "admin_c21_trc",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &OsEntropy,
        now,
    )
    .await
    .expect("admin");

    // A real client key: `trace_records.client_key_id` and `usage_events.client_key_id` both
    // carry foreign keys, so a synthetic id is rejected.
    let client_key_id = llmroller_core::ids::ClientKeyId::generate().to_string();
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, prefix, key_hash, label, routing_scope,
            created_at)
         VALUES (?, ?, 'llr_c21', 'hash_c21', 'c21-trace-key', 'personal_first', ?)",
    )
    .bind(&client_key_id)
    .bind(admin.id.to_string())
    .bind(now)
    .execute(&pool)
    .await
    .expect("client key");

    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(ADMIN_SESSION.as_bytes()),
            user_id: admin.id,
            csrf_hash: &llmroller_store::crypto::sha256_hex(ADMIN_CSRF.as_bytes()),
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("c21-trc"),
            created_at: now,
            expires_at: now + DAY_MS,
        },
    )
    .await
    .expect("session");

    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance.session_hmac_secret.clone());
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: None,
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    Harness {
        _dir: dir,
        pool,
        app,
        admin_id: admin.id,
        client_key_id,
    }
}

impl Harness {
    /// Lower retention to `days` with `confirm_purge: true`.
    async fn reduce_retention(&self, days: i64) -> StatusCode {
        let body = serde_json::json!({
            "enabled": true,
            "match_filters": { "user_ids": [], "client_key_ids": [], "models": [] },
            "retention_days": days,
            "auto_disable_at": null,
            "confirm_purge": true
        });
        let request = Request::builder()
            .method("PUT")
            .uri("/admin/api/settings/traceability")
            .header(header::COOKIE, format!("llmroller_session={ADMIN_SESSION}"))
            .header("x-csrf-token", ADMIN_CSRF)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");
        self.app
            .clone()
            .oneshot(request)
            .await
            .expect("response")
            .status()
    }
}

/// Insert one trace record with a given age, plus its block references.
///
/// Returns the `usage_event_id` used. Written with direct SQL because the sealed-payload path
/// needs a real sealer and this cycle is about the *reference graph*, not encryption.
async fn insert_record(
    h: &Harness,
    id: &str,
    age_days: i64,
    block_hashes: &[&str],
    now: i64,
) -> String {
    let usage_event_id = format!("ue_{id}");
    let ts = now - age_days * DAY_MS;
    // `trace_records.usage_event_id` references `usage_events`, so the parent row must exist.
    sqlx::query(
        "INSERT INTO usage_events (id, ts, request_id, client_key_id, owner_user_id,
            endpoint, status, stream, incomplete, termination, cost_micro_usd, cost_source,
            plan_multiplier_bps, billing_coverage_mode, list_cost_micro_usd)
         VALUES (?, ?, ?, ?, ?, 'responses', 200, 0, 0, 'completed', 0, 'price_book',
            10000, 'pay_as_you_go', 0)",
    )
    .bind(&usage_event_id)
    .bind(ts)
    .bind(format!("req_{id}"))
    .bind(&h.client_key_id)
    .bind(h.admin_id.to_string())
    .execute(&h.pool)
    .await
    .expect("usage event");

    sqlx::query(
        "INSERT INTO trace_records (usage_event_id, request_id, ts, owner_user_id,
            client_key_id, session_hash, thread_id, canonical_id, protocol, truncated_content,
            payload_enc, master_key_id, expires_at)
         VALUES (?, ?, ?, ?, ?, 'sess_c21', 'main', 'gpt-4o', 'responses', 0,
            X'00', 'key-0', ?)",
    )
    .bind(&usage_event_id)
    .bind(format!("req_{id}"))
    .bind(ts)
    .bind(h.admin_id.to_string())
    .bind(&h.client_key_id)
    .bind(ts + 14 * DAY_MS)
    .execute(&h.pool)
    .await
    .expect("trace record");

    for hash in block_hashes {
        sqlx::query(
            "INSERT OR IGNORE INTO trace_blocks (block_hash, owner_user_id, payload_enc,
                master_key_id, byte_len, last_referenced_at)
             VALUES (?, ?, X'00', 'key-0', 1, ?)",
        )
        .bind(hash)
        .bind(h.admin_id.to_string())
        .bind(ts)
        .execute(&h.pool)
        .await
        .expect("trace block");

        sqlx::query(
            "INSERT INTO trace_record_blocks (usage_event_id, owner_user_id, block_hash)
             VALUES (?, ?, ?)",
        )
        .bind(&usage_event_id)
        .bind(h.admin_id.to_string())
        .bind(hash)
        .execute(&h.pool)
        .await
        .expect("block reference");
    }
    usage_event_id
}

async fn record_count(h: &Harness) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM trace_records")
        .fetch_one(&h.pool)
        .await
        .expect("count")
}

async fn block_exists(h: &Harness, hash: &str) -> bool {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM trace_blocks WHERE block_hash = ?")
            .bind(hash)
            .fetch_one(&h.pool)
            .await
            .expect("count");
    count > 0
}

// ---------------------------------------------------------------------------
// The real defect: reducing retention purges nothing
// ---------------------------------------------------------------------------

/// Pre-fix the handler rewrote the settings row and deleted nothing, so records older than the
/// new window survived. `confirm_purge: true` is the operator saying "yes, delete them".
#[tokio::test]
#[doc = "Spec: TRC-1 — ISSUE-0194 reducing retention purges expired trace records"]
async fn reducing_retention_purges_expired_trace_records() {
    let h = harness().await;
    let now = SystemClock::new().now_ms();
    // 20 days old: outside a 7-day window, inside the 14-day default.
    insert_record(&h, "old", 20, &["blk_old_only"], now).await;
    insert_record(&h, "fresh", 1, &["blk_fresh"], now).await;
    assert_eq!(record_count(&h).await, 2);

    assert_eq!(h.reduce_retention(7).await, StatusCode::OK);

    assert_eq!(
        record_count(&h).await,
        1,
        "the record older than the new 7-day window must be purged; confirm_purge: true is \
         the operator consenting to exactly that"
    );
    let remaining: String = sqlx::query_scalar("SELECT request_id FROM trace_records")
        .fetch_one(&h.pool)
        .await
        .expect("remaining row");
    assert_eq!(remaining, "req_fresh", "only the in-window record survives");
}

/// The block sweep half: a block referenced only by the purged record must go too.
#[tokio::test]
#[doc = "Spec: TRC-4, ADR-9 — ISSUE-0194 reducing retention purges orphaned trace blocks"]
async fn reducing_retention_purges_orphaned_trace_blocks() {
    let h = harness().await;
    let now = SystemClock::new().now_ms();
    insert_record(&h, "old", 20, &["blk_orphan"], now).await;
    insert_record(&h, "fresh", 1, &["blk_live"], now).await;
    assert!(block_exists(&h, "blk_orphan").await);

    assert_eq!(h.reduce_retention(7).await, StatusCode::OK);

    assert!(
        !block_exists(&h, "blk_orphan").await,
        "a block referenced only by the purged record must be collected, or the table grows \
         without bound every time retention is reduced"
    );
    assert!(
        block_exists(&h, "blk_live").await,
        "a block still referenced by a live record must survive"
    );
}

/// **The guard that matters.** `trace_blocks` is content-addressed (`ADR-9`), so one block may be
/// shared by an expired and a live record. A blanket delete passes the test above and silently
/// corrupts every live trace that happens to share a block.
#[tokio::test]
#[doc = "Spec: TRC-4, ADR-9 — ISSUE-0194 reducing retention keeps shared blocks"]
async fn reducing_retention_keeps_blocks_still_referenced() {
    let h = harness().await;
    let now = SystemClock::new().now_ms();
    // The same block hash under both records: exactly the content-addressing case.
    insert_record(&h, "old", 20, &["blk_shared"], now).await;
    insert_record(&h, "fresh", 1, &["blk_shared"], now).await;
    assert_eq!(record_count(&h).await, 2);

    assert_eq!(h.reduce_retention(7).await, StatusCode::OK);

    assert_eq!(record_count(&h).await, 1, "the expired record is gone");
    assert!(
        block_exists(&h, "blk_shared").await,
        "a block still referenced by the surviving record must NOT be collected — deleting it \
         would corrupt a live trace that shares the content"
    );
}

/// Raising retention must not purge anything: there is no window to enforce downwards.
#[tokio::test]
#[doc = "Spec: TRC-1 — ISSUE-0194 raising retention purges nothing"]
async fn raising_retention_purges_nothing() {
    let h = harness().await;
    let now = SystemClock::new().now_ms();
    insert_record(&h, "aged", 30, &["blk_aged"], now).await;

    // Default retention is 14 days; raising it is not a shrinking window.
    assert_eq!(h.reduce_retention(30).await, StatusCode::OK);

    assert_eq!(
        record_count(&h).await,
        1,
        "raising retention must never delete; a 30-day window keeps a 30-day-old record"
    );
    assert!(block_exists(&h, "blk_aged").await);
}
