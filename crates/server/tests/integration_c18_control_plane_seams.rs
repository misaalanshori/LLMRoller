//! Three control-plane seam fixes (`ISSUE-0182`, `ISSUE-0192`, `ISSUE-0193`).
//!
//! All three sit at the same seam: a handler writes a row, and the runtime (pins,
//! snapshot) must be made to agree. Each issue gets its own red plus the guard test that
//! stops the obvious lazy fix.
//!
//! The harness attaches a **real** `WriterHandle` and reloader, unlike
//! `integration_admin_and_user_api.rs`. That matters for #182: `publish_control_change`
//! silently does nothing without a writer, so a test built on a writer-less state would
//! pass whether or not the handler published — it would pin nothing.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tempfile::TempDir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::ids::{ClientKeyId, ModelPk, UpstreamKeyId, UserId};
use llmroller_core::pins::{PinKey, PinStore};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, DeterministicRng, SystemClock};
use llmroller_server::reloader::{load_initial_snapshot, start_reloader};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};
use llmroller_store::writer::start_writer;
use tokio::sync::mpsc;

const ADMIN_SESSION: &str = "c18_admin_session_0123456789abcdef";
const ADMIN_CSRF: &str = "c18_admin_csrf_0123456789abcdef";
const USER_SESSION: &str = "c18_user_session_0123456789abcdef0";
const USER_CSRF: &str = "c18_user_csrf_0123456789abcdef0";

const MODEL_CANONICAL: &str = "gpt-4o";

struct Harness {
    _dir: TempDir,
    pool: sqlx::SqlitePool,
    app: axum::Router,
    /// The live snapshot the reloader writes into — what the data plane reads.
    snapshot: Arc<ArcSwap<llmroller_core::snapshot::ConfigSnapshot>>,
    pin_store: Arc<PinStore>,
    user_id: UserId,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("c18.db"), PoolRole::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let master_key = MasterKey::from_bytes([0xD2; 32]);
    let instance_state = load_or_initialize(
        &pool,
        "key-0",
        &master_key,
        Arc::new(DeterministicRng::new(7)),
        1,
    )
    .await
    .expect("instance load");

    let now = SystemClock::new().now_ms();
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let mk_user = |name: &'static str, role: Role, pw: &'static str, now: i64| {
        let pool = pool.clone();
        async move {
            create_user(
                &pool,
                &system_actor,
                NewUser {
                    username: name,
                    password: pw,
                    role,
                    must_change_password: false,
                },
                &DeterministicRng::new(7),
                now,
            )
            .await
            .expect("create user")
        }
    };
    let admin = mk_user("admin_c18", Role::Admin, "supersecretadminpass123", now).await;
    let user = mk_user("user_c18", Role::User, "supersecretuserpass123", now).await;

    for (raw, uid, csrf) in [
        (ADMIN_SESSION, admin.id, ADMIN_CSRF),
        (USER_SESSION, user.id, USER_CSRF),
    ] {
        insert_web_session(
            &pool,
            NewWebSession {
                id_hash: &llmroller_store::crypto::sha256_hex(raw.as_bytes()),
                user_id: uid,
                csrf_hash: &llmroller_store::crypto::sha256_hex(csrf.as_bytes()),
                ip_addr: Some("127.0.0.1"),
                user_agent: Some("c18"),
                created_at: now,
                expires_at: now + 86_400_000,
            },
        )
        .await
        .expect("session");
    }

    let model_pk = ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id) VALUES (?, ?)")
        .bind(model_pk.to_string())
        .bind(MODEL_CANONICAL)
        .execute(&pool)
        .await
        .expect("insert model");

    let initial = load_initial_snapshot(&pool).await.expect("snapshot");
    let snapshot = Arc::new(ArcSwap::new(Arc::new(initial)));
    let runtime = Arc::new(llmroller_core::runtime_state::RuntimeState::new());
    let (reloader_tx, reloader_rx) = mpsc::channel(32);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx.clone());
    let (published, _reloader_task) = start_reloader(
        pool.clone(),
        Arc::clone(&snapshot),
        runtime,
        reloader_rx,
        reloader_tx,
    );

    let mut auth_state = AuthAppState::new(
        pool.clone(),
        AuthConfig {
            trusted_proxies: Vec::new(),
            dev_insecure_cookies: true,
            require_tls: false,
        },
    );
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();

    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: None,
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(SystemClock::new()),
        entropy: Arc::new(DeterministicRng::new(8)),
        audit_hmac_key: None,
    };
    let mut api_state =
        ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    api_state.writer = Some(writer);
    api_state.attach_reloader(
        published.notification_sender(),
        published.version_receiver(),
    );
    let pin_store = Arc::new(PinStore::new(10_000));
    api_state.attach_pin_store(Arc::clone(&pin_store));

    Harness {
        _dir: dir,
        pool,
        app: control_plane_router_full(setup_state, auth_state, api_state),
        snapshot,
        pin_store,
        user_id: user.id,
    }
}

impl Harness {
    async fn send(&self, request: Request<Body>) -> (StatusCode, Vec<u8>) {
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes()
            .to_vec();
        (status, body)
    }

    fn user_default_scope(&self) -> String {
        self.snapshot
            .load()
            .users
            .get(&self.user_id)
            .map(|u| format!("{:?}", u.default_routing_scope))
            .unwrap_or_else(|| "missing".to_string())
    }

    fn config_version(&self) -> i64 {
        self.snapshot.load().config_version
    }
}

fn admin_delete_price(provider_id: &str, model_id: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(format!(
            "/admin/api/prices?provider_id={provider_id}&upstream_model_id={model_id}"
        ))
        .header(header::COOKIE, format!("llmroller_session={ADMIN_SESSION}"))
        .header("x-csrf-token", ADMIN_CSRF)
        .body(Body::empty())
        .expect("delete price request")
}

fn user_patch_preferences(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri("/api/user/preferences")
        .header(header::COOKIE, format!("llmroller_session={USER_SESSION}"))
        .header("x-csrf-token", USER_CSRF)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("patch preferences request")
}

/// Insert a price row directly, so the delete endpoint has something to remove.
async fn seed_price(harness: &Harness, provider_id: &str, model_id: &str) {
    sqlx::query(
        "INSERT INTO prices (provider_id, provider_kind, upstream_model_id,
            input_per_mtok_micro_usd, output_per_mtok_micro_usd, source, updated_at)
         VALUES (?, 'custom', ?, 1000, 2000, 'manual', 1000)",
    )
    .bind(provider_id)
    .bind(model_id)
    .execute(&harness.pool)
    .await
    .expect("seed price");
}

async fn price_row_count(harness: &Harness, provider_id: &str, model_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM prices WHERE provider_id = ? AND upstream_model_id = ?",
    )
    .bind(provider_id)
    .bind(model_id)
    .fetch_one(&harness.pool)
    .await
    .expect("count prices")
}

// ---------------------------------------------------------------------------
// #192 — DELETE /admin/api/prices must 404 when nothing matched
// ---------------------------------------------------------------------------

/// Pre-fix the handler discards `delete_price_tx`'s `bool` and returns 204 unconditionally,
/// so an operator deleting a price that is not there is told it worked.
#[tokio::test]
#[doc = "Spec: MDL-4, API-1 — ISSUE-0192 delete price returns 404 when no row matched"]
async fn delete_price_returns_404_when_no_row_matches() {
    let h = harness().await;
    let (status, body) = h
        .send(admin_delete_price(
            "provider-that-has-no-price",
            "model-never-priced",
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "deleting a price that does not exist must not report success; body was {}",
        String::from_utf8_lossy(&body)
    );
}

/// The positive half: without it, "return 404 unconditionally" passes the test above.
#[tokio::test]
#[doc = "Spec: MDL-4, API-1 — ISSUE-0192 delete price returns 204 and removes the row"]
async fn delete_price_returns_204_and_removes_the_row() {
    let h = harness().await;
    seed_price(&h, "prov-192", "model-192").await;
    assert_eq!(price_row_count(&h, "prov-192", "model-192").await, 1);

    let (status, body) = h.send(admin_delete_price("prov-192", "model-192")).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "deleting a price that exists must succeed; body was {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        price_row_count(&h, "prov-192", "model-192").await,
        0,
        "the row must actually be gone"
    );

    // And a second delete of the same pair is now a genuine miss.
    let (status2, _) = h.send(admin_delete_price("prov-192", "model-192")).await;
    assert_eq!(
        status2,
        StatusCode::NOT_FOUND,
        "a repeated delete has nothing left to remove"
    );
}

// ---------------------------------------------------------------------------
// #193 — deleting a service account must evict its child keys' pins
// ---------------------------------------------------------------------------

fn pin_key(client_key_id: ClientKeyId) -> PinKey {
    PinKey {
        client_key_id,
        canonical_id: MODEL_CANONICAL.to_string(),
        session_hash: "c18-session".to_string(),
    }
}

const SESSION_HASH: &str = "c18-session";

/// Whether this client key still has a live pin, read through the **same** accessor the
/// data plane uses (`find_active_pin`), so the assertion tracks real routing behaviour
/// rather than store internals.
fn has_live_pin(store: &PinStore, client_key_id: ClientKeyId) -> bool {
    store
        .find_active_pin(
            &client_key_id,
            SESSION_HASH,
            Some(MODEL_CANONICAL),
            ChainClockNow::now(),
        )
        .is_some()
}

/// A far-future "now" so pins created with `ttl_ms = i64::MAX` are unambiguously live.
struct ChainClockNow;
impl ChainClockNow {
    fn now() -> i64 {
        2_000
    }
}

async fn create_service_account_with_child_key(
    harness: &Harness,
    name: &str,
) -> (String, ClientKeyId) {
    let now = SystemClock::new().now_ms();
    let sa = llmroller_store::service_accounts::create_service_account(
        &harness.pool,
        &ActorScope::user(harness.user_id, Role::User),
        harness.user_id,
        llmroller_store::service_accounts::NewServiceAccount {
            name: name.to_string(),
            description: None,
            workspace_scope: Some("personal".to_string()),
            allowed_cidrs: None,
        },
        now,
    )
    .await
    .expect("create service account");

    let client_key_id = ClientKeyId::generate();
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, service_account_id, prefix, key_hash, label,
            routing_scope, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 'personal_first', ?)",
    )
    .bind(client_key_id.to_string())
    .bind(harness.user_id.to_string())
    .bind(&sa.id)
    .bind(format!("llr_{name}"))
    .bind(format!("hash-{name}"))
    .bind(format!("{name}-key"))
    .bind(now)
    .execute(&harness.pool)
    .await
    .expect("insert child client key");

    (sa.id, client_key_id)
}

/// Pre-fix the DB row is deleted (child keys cascade away) but the in-memory pin survives,
/// so sessions that were mid-flight keep being routed on a key that no longer exists.
#[tokio::test]
#[doc = "Spec: CLK-6, RTE-2 — ISSUE-0193 deleting a service account evicts its child key pins"]
async fn deleting_a_service_account_evicts_its_child_key_pins() {
    let h = harness().await;
    let (sa_id, child_key) = create_service_account_with_child_key(&h, "c18-sa").await;

    // A live pin for the child key.
    h.pin_store.upsert_confirmed(
        pin_key(child_key),
        UpstreamKeyId::generate(),
        llmroller_core::eligibility::CandidatePoolTier::Own,
        1000,
        i64::MAX,
    );
    assert!(
        has_live_pin(&h.pin_store, child_key),
        "precondition: the child key has a live pin"
    );

    let response = h
        .send(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/user/service-accounts/{sa_id}"))
                .header(header::COOKIE, format!("llmroller_session={USER_SESSION}"))
                .header("x-csrf-token", USER_CSRF)
                .body(Body::empty())
                .expect("delete sa request"),
        )
        .await;
    assert_eq!(response.0, StatusCode::NO_CONTENT, "delete must succeed");

    assert!(
        !has_live_pin(&h.pin_store, child_key),
        "the child key's pin must be evicted, or its sessions keep routing on a key the \
         cascade just deleted"
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM client_keys WHERE service_account_id = ?")
            .bind(&sa_id)
            .fetch_one(&h.pool)
            .await
            .expect("count child keys");
    assert_eq!(remaining, 0, "the child key row must be cascaded away");
}

/// The over-eviction guard: an unrelated key's pin must survive. Without this, "clear the
/// whole pin store" passes the test above.
#[tokio::test]
#[doc = "Spec: CLK-6, RTE-2 — ISSUE-0193 deleting a service account does not evict other keys' pins"]
async fn deleting_a_service_account_does_not_evict_other_keys_pins() {
    let h = harness().await;
    let (sa_id, child_key) = create_service_account_with_child_key(&h, "c18-sa-iso").await;
    // The unrelated key needs a real `client_keys` row with NO `service_account_id`, or the
    // guard is unfalsifiable: a query that forgot its `WHERE service_account_id = ?` filter
    // would still never return a key that is not in the table at all, so the test would
    // pass for a fix that evicts every key. This was a real weakness in the first version.
    let unrelated = ClientKeyId::generate();
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, service_account_id, prefix, key_hash,
            label, routing_scope, created_at)
         VALUES (?, ?, NULL, 'llr_unrelated', 'hash-unrelated', 'unrelated-key',
            'personal_first', 1000)",
    )
    .bind(unrelated.to_string())
    .bind(h.user_id.to_string())
    .execute(&h.pool)
    .await
    .expect("insert unrelated client key");

    h.pin_store.upsert_confirmed(
        pin_key(child_key),
        UpstreamKeyId::generate(),
        llmroller_core::eligibility::CandidatePoolTier::Own,
        1000,
        i64::MAX,
    );
    h.pin_store.upsert_confirmed(
        pin_key(unrelated),
        UpstreamKeyId::generate(),
        llmroller_core::eligibility::CandidatePoolTier::Own,
        1000,
        i64::MAX,
    );

    let (status, _) = h
        .send(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/user/service-accounts/{sa_id}"))
                .header(header::COOKIE, format!("llmroller_session={USER_SESSION}"))
                .header("x-csrf-token", USER_CSRF)
                .body(Body::empty())
                .expect("delete sa request"),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(
        has_live_pin(&h.pin_store, unrelated),
        "an unrelated client key's pin must survive: eviction must be scoped to the \
         service account's children"
    );
}

/// Disabling a service account must behave the same way. The data plane blocks a disabled
/// SA at gate G2, so this is defence in depth — but leaving pins live for a principal that
/// is supposed to be off is exactly the stale state the issue is about.
#[tokio::test]
#[doc = "Spec: CLK-6, RTE-2 — ISSUE-0193 disabling a service account evicts its child key pins"]
async fn disabling_a_service_account_evicts_its_child_key_pins() {
    let h = harness().await;
    let (sa_id, child_key) = create_service_account_with_child_key(&h, "c18-sa-disable").await;
    h.pin_store.upsert_confirmed(
        pin_key(child_key),
        UpstreamKeyId::generate(),
        llmroller_core::eligibility::CandidatePoolTier::Own,
        1000,
        i64::MAX,
    );

    let (status, body) = h
        .send(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/user/service-accounts/{sa_id}"))
                .header(header::COOKIE, format!("llmroller_session={USER_SESSION}"))
                .header("x-csrf-token", USER_CSRF)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"disabled": true}).to_string(),
                ))
                .expect("patch sa request"),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "disable must succeed; body was {}",
        String::from_utf8_lossy(&body)
    );

    assert!(
        !has_live_pin(&h.pin_store, child_key),
        "a disabled service account's child key pins must be evicted"
    );
}

// ---------------------------------------------------------------------------
// #182 — PATCH /api/user/preferences publishes only what matters
// ---------------------------------------------------------------------------

/// The issue's stated intent: a UI-only preference change must not rebuild the snapshot.
///
/// This may already pass, because the handler currently publishes nothing at all. It is a
/// **status-quo pin** — its job is to stay green while the fix adds publishing for the
/// routing-scope case, and to fail if that fix publishes on every payload.
#[tokio::test]
#[doc = "Spec: UI-5, REL-4 — ISSUE-0182 a UI-only preference update does not bump the snapshot"]
async fn ui_only_preference_update_does_not_bump_the_snapshot_version() {
    let h = harness().await;
    let before = h.config_version();

    let (status, body) = h
        .send(user_patch_preferences(
            serde_json::json!({"ui_theme": "dark"}),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the update must succeed; body was {}",
        String::from_utf8_lossy(&body)
    );

    assert_eq!(
        h.config_version(),
        before,
        "a ui_theme change is pure presentation and must not rebuild the snapshot"
    );
}

/// The real defect: `default_routing_scope` drives authorization in the data plane
/// (`resolve_caller_scope` reads `snapshot.users[..].default_routing_scope`), so writing it
/// without publishing leaves the data plane on the stale value until something unrelated
/// bumps the snapshot.
#[tokio::test]
#[doc = "Spec: CLK-3, REL-4 — ISSUE-0182 a routing-scope update reaches the data plane"]
async fn routing_scope_update_reaches_the_data_plane() {
    let h = harness().await;
    let before = h.user_default_scope();

    let (status, body) = h
        .send(user_patch_preferences(
            serde_json::json!({"default_routing_scope": "own_only"}),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the update must succeed; body was {}",
        String::from_utf8_lossy(&body)
    );

    // The DB write is unconditional; the snapshot is what the data plane reads.
    let stored: String = sqlx::query_scalar("SELECT default_routing_scope FROM users WHERE id = ?")
        .bind(h.user_id.to_string())
        .fetch_one(&h.pool)
        .await
        .expect("stored scope");
    assert_eq!(stored, "own_only", "the row must be written");

    let after = h.user_default_scope();
    assert_ne!(
        after, before,
        "the handler wrote {stored} but the published snapshot still says {before}: the data \
         plane would keep using the old scope until an unrelated change bumped the snapshot"
    );
    assert!(
        after.to_lowercase().contains("own"),
        "the published snapshot must reflect own_only; got {after}"
    );
}

/// Publishing must be conditioned on the *field being present*, not on the handler being
/// called: a payload that carries both still publishes, and a payload with no
/// `default_routing_scope` in it must not.
#[tokio::test]
#[doc = "Spec: REL-4 — ISSUE-0182 only a routing-scope payload publishes"]
async fn locale_only_update_does_not_bump_the_snapshot_version() {
    let h = harness().await;
    let before = h.config_version();
    let (status, _) = h
        .send(user_patch_preferences(
            serde_json::json!({"ui_locale": "id", "ui_nav_layout": "sidebar"}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        h.config_version(),
        before,
        "locale and nav-layout are presentation only and must not publish"
    );
}
