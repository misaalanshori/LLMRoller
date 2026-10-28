//! Session-pin move handlers must not report success they did not achieve, and must keep
//! `PinStore` and the database in agreement (`ISSUE-0232`).
//!
//! # The three defects this file pins
//!
//! `post_user_move_pin_body_handler` and `post_admin_move_session_pin_handler` both ran a bare
//! `let _ = sqlx::query(... UPDATE pins ...).execute(&state.auth.pool).await;` and then
//! unconditionally read the rows back and called `store.admin_move_with_tier(...)`.
//!
//! 1. **The `Result` was discarded**, so a SQL failure became an invisible no-op and the caller
//!    was still told `{"moved": true}`.
//! 2. **Zero rows affected was reported as success.** The `WHERE` is ownership-scoped, so moving
//!    a pin you do not own matched nothing and returned `200 {"moved": true}`. `USR-3` requires
//!    a foreign resource to be indistinguishable from a missing one.
//! 3. **`PinStore` was updated even when the database wrote nothing.** The store is what the data
//!    plane routes from, so this is live wrong traffic, not a stale row.
//!
//! Point 3 is the one that matters most, and the only one a response-only test would miss —
//! so every test here that expects a refusal also asserts the store did not move.
//!
//! The handlers run in-process through the real control-plane router so the decision under test
//! (which status code, whether the store moved) is the shipped one.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::{ApiAppState, SetupAppState, control_plane_router_full};
use llmroller_core::eligibility::CandidatePoolTier;
use llmroller_core::ids::{ClientKeyId, UpstreamKeyId, UserId};
use llmroller_core::pins::{PinEntry, PinKey, PinState, PinStore};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy, secret_string};
use llmroller_server::reloader::{load_initial_snapshot, start_reloader};
use llmroller_store::client_keys::{NewClientKey, create_client_key};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider, create_upstream_key,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};
use llmroller_store::writer::start_writer;

const SESSION_TOKEN: &str = "move_pin_session_token_0123456789ab";
const CSRF_TOKEN: &str = "move_pin_csrf_token_0123456789abc";
const CANONICAL: &str = "move-pin-target-model";
/// The session hash that Alice owns pins in, and the one only Bob owns pins in.
const OWN_SESSION: &str = "session-owned-by-alice";
const FOREIGN_SESSION: &str = "session-owned-by-bob";

struct MoveHarness {
    _dir: tempfile::TempDir,
    app: Router,
    pool: sqlx::SqlitePool,
    store: Arc<PinStore>,
    alice_key: UpstreamKeyId,
    alice_client_key: ClientKeyId,
    bob_key: UpstreamKeyId,
    bob_client_key: ClientKeyId,
}

/// A user, their personal provider, one upstream key, one client key, and a routed model.
struct Tenant {
    id: UserId,
    key: UpstreamKeyId,
    client_key: ClientKeyId,
}

async fn seed_tenant(
    pool: &sqlx::SqlitePool,
    master_key: &MasterKey,
    model_pk: &str,
    username: &'static str,
    password: &'static str,
) -> Tenant {
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        pool,
        &system_actor,
        NewUser {
            username,
            password,
            role: Role::User,
            must_change_password: false,
        },
        &OsEntropy,
        100,
    )
    .await
    .expect("create user");
    let actor = ActorScope::user(user.id, Role::User);

    let provider = create_provider(
        pool,
        &actor,
        NewProvider {
            name: format!("{username}-provider"),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(user.id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        100,
    )
    .await
    .expect("create provider");

    let key = create_upstream_key(
        pool,
        &actor,
        master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: format!("{username}-key"),
            secret: secret_string(format!("sk-{username}-move-test-secret")),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        100,
    )
    .await
    .expect("create upstream key");

    sqlx::query(
        "INSERT INTO model_routes \
         (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, 'native-model', '[\"chat\"]', 'active', 1, 100)",
    )
    .bind(key.id.to_string())
    .bind(model_pk)
    .execute(pool)
    .await
    .expect("route");

    let client_key = create_client_key(
        pool,
        &actor,
        user.id,
        NewClientKey {
            label: format!("{username}-client"),
            service_account_id: None,
            routing_scope: None,
            allowed_models: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: None,
            expires_at: None,
        },
        &OsEntropy,
        100,
    )
    .await
    .expect("client key")
    .client_key
    .id;

    Tenant {
        id: user.id,
        key: key.id,
        client_key,
    }
}

/// Insert one live pin row and hydrate the matching `PinStore` entry, so a move that touches
/// only one of them is observable.
async fn seed_pin(
    pool: &sqlx::SqlitePool,
    store: &PinStore,
    client_key: ClientKeyId,
    session_hash: &str,
    model_pk: &str,
    upstream_key: UpstreamKeyId,
) {
    sqlx::query(
        "INSERT INTO pins \
         (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier, \
          created_at, last_used_at, expires_at) \
         VALUES (?, ?, ?, ?, 1, 100, 100, ?)",
    )
    .bind(client_key.to_string())
    .bind(session_hash)
    .bind(model_pk)
    .bind(upstream_key.to_string())
    .bind(9_000_000_000_000_i64)
    .execute(pool)
    .await
    .expect("insert pin");

    store.insert_hydrated(
        PinKey {
            client_key_id: client_key,
            session_hash: session_hash.to_string(),
            canonical_id: CANONICAL.to_string(),
        },
        PinEntry {
            state: PinState::Confirmed,
            upstream_key_id: upstream_key,
            generation: 1,
            pool_tier: CandidatePoolTier::Shared,
            created_ms: 100,
            last_used_ms: 100,
            expires_ms: 9_000_000_000_000,
            repin_count: 0,
            tried_this_generation: Vec::new(),
        },
    );
}

async fn harness() -> MoveHarness {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("move_pin.db");
    let pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let master_key = MasterKey::from_bytes([0xC6; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance");
    seed_defaults(&pool, None, 1).await.expect("settings");

    let model_pk = "01JC36MOVEPIN000000000000".to_string();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) \
         VALUES (?, ?, 1, 'smooth_weighted_rr')",
    )
    .bind(&model_pk)
    .bind(CANONICAL)
    .execute(&pool)
    .await
    .expect("insert model");

    let alice = seed_tenant(
        &pool,
        &master_key,
        &model_pk,
        "alice_c36",
        "alice-c36-password",
    )
    .await;
    let bob = seed_tenant(&pool, &master_key, &model_pk, "bob_c36", "bob-c36-password").await;

    let store = Arc::new(PinStore::new(1_000));
    seed_pin(
        &pool,
        &store,
        alice.client_key,
        OWN_SESSION,
        &model_pk,
        alice.key,
    )
    .await;
    seed_pin(
        &pool,
        &store,
        bob.client_key,
        FOREIGN_SESSION,
        &model_pk,
        bob.key,
    )
    .await;

    // The real reloader is attached so the handler's read-your-writes barrier has something to
    // wait on, exactly as in production.
    let initial = load_initial_snapshot(&pool).await.expect("snapshot");
    let shared = Arc::new(ArcSwap::new(Arc::new(initial)));
    let runtime = Arc::new(llmroller_core::runtime_state::RuntimeState::new());
    let (reloader_tx, reloader_rx) = tokio::sync::mpsc::channel(32);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx.clone());
    let (reloader_handle, _reloader_task) =
        start_reloader(pool.clone(), shared, runtime, reloader_rx, reloader_tx);

    let mut auth_state = AuthAppState::new(
        pool.clone(),
        AuthConfig {
            trusted_proxies: Vec::new(),
            dev_insecure_cookies: true,
            require_tls: false,
        },
    );
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());

    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: None,
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let mut api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    api_state.writer = Some(writer);
    api_state.attach_reloader(
        reloader_handle.notification_sender(),
        reloader_handle.version_receiver(),
    );
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x36; 32]));
    api_state.pin_store = Some(store.clone());

    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    // Alice's authenticated web session.
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let csrf_hash = llmroller_store::crypto::sha256_hex(CSRF_TOKEN.as_bytes());
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(SESSION_TOKEN.as_bytes()),
            user_id: alice.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("move-pin-test"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("insert session");

    MoveHarness {
        _dir: dir,
        app,
        pool,
        store,
        alice_key: alice.key,
        alice_client_key: alice.client_key,
        bob_key: bob.key,
        bob_client_key: bob.client_key,
    }
}

impl MoveHarness {
    /// `POST /api/user/pins/move` as Alice.
    async fn user_move(&self, session_hash: &str) -> (StatusCode, serde_json::Value) {
        let body = serde_json::json!({
            "upstream_key_id": self.alice_key.to_string(),
            "session_hash": session_hash,
        })
        .to_string();
        let request = Request::builder()
            .method("POST")
            .uri("/api/user/pins/move")
            .header("cookie", format!("llmroller_session={SESSION_TOKEN}"))
            .header("content-type", "application/json")
            .header("x-csrf-token", CSRF_TOKEN)
            .body(Body::from(body))
            .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    /// The `upstream_key_id` and `pool_tier` stored for one pin row, or `None` if absent.
    async fn pin_row(&self, client_key: ClientKeyId, session_hash: &str) -> Option<(String, i64)> {
        sqlx::query_as(
            "SELECT upstream_key_id, pool_tier FROM pins \
             WHERE client_key_id = ? AND session_hash = ?",
        )
        .bind(client_key.to_string())
        .bind(session_hash)
        .fetch_optional(&self.pool)
        .await
        .expect("read pin")
    }

    /// The `PinStore`'s current upstream key for one pin.
    fn store_key(&self, client_key: ClientKeyId, session_hash: &str) -> Option<UpstreamKeyId> {
        self.store.find_active_pin(
            &client_key,
            session_hash,
            Some(CANONICAL),
            llmroller_core::seams::SystemClock::new().now_ms(),
        )
    }
}

// ---------------------------------------------------------------------------
// 1. A foreign session must not be reported as moved, and must not move the store
// ---------------------------------------------------------------------------

/// Moving a pin in a session the caller owns no client key for matches zero rows.
///
/// Pre-fix this returned `200 {"moved": true}` — a success claim for another user's session,
/// which `USR-3` forbids (a foreign resource must be indistinguishable from a missing one).
/// The decision is **`404`**, matching `delete_user_pin_handler` directly above: one rule for a
/// foreign client key, not two.
///
/// The store assertion is the important half. Pre-fix the UPDATE matched nothing, but the
/// read-back loop still called `admin_move_with_tier`, so `PinStore` — which the data plane
/// routes from — pointed at Alice's key while the database still said Bob's. That is live wrong
/// traffic, and a response-only assertion would not see it because the response was green either
/// way.
#[tokio::test]
#[doc = "Spec: USR-3, SEC-17"]
async fn move_pin_of_a_foreign_session_is_not_reported_as_moved() {
    let h = harness().await;

    let (status, body) = h.user_move(FOREIGN_SESSION).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a session the caller owns no client key for must be 404, not a success; body: {body}"
    );
    assert_ne!(
        body["moved"],
        serde_json::json!(true),
        "a refusal must not also claim the pin moved"
    );

    // The database must not have moved Bob's pin.
    let row = h
        .pin_row(h.bob_client_key, FOREIGN_SESSION)
        .await
        .expect("bob's pin row still exists");
    assert_eq!(
        row.0,
        h.bob_key.to_string(),
        "a refused move must not rewrite a foreign pin"
    );

    // And the store must agree with the database.
    assert_eq!(
        h.store_key(h.bob_client_key, FOREIGN_SESSION),
        Some(h.bob_key),
        "PinStore must not move a pin the database did not move; the data plane routes from it"
    );
}

// ---------------------------------------------------------------------------
// 2. A SQL failure must surface, not be swallowed
// ---------------------------------------------------------------------------

/// A failing `UPDATE pins` must produce an error response, not `{"moved": true}`.
///
/// The failure is injected with a `BEFORE UPDATE` trigger that raises `ABORT` — deterministic,
/// and the same observable class as a constraint violation or `SQLITE_BUSY`, without depending on
/// runtime timing. (A closed pool would also work but is harder to make reliable mid-request.)
///
/// This also pins point 3 in its worst form: pre-fix the update failed, the error was discarded,
/// and the store still moved — so the store claimed a move the database had rejected outright.
#[tokio::test]
#[doc = "Spec: USR-3, RTE-11"]
async fn move_pin_reports_failure_when_the_update_errors() {
    let h = harness().await;

    sqlx::query(
        "CREATE TRIGGER fail_pin_move BEFORE UPDATE ON pins \
         BEGIN SELECT RAISE(ABORT, 'simulated constraint failure'); END",
    )
    .execute(&h.pool)
    .await
    .expect("install failing trigger");

    let (status, body) = h.user_move(OWN_SESSION).await;
    assert!(
        status.is_server_error(),
        "a SQL failure must not be reported as success; status {status}, body: {body}"
    );
    assert_ne!(
        body["moved"],
        serde_json::json!(true),
        "a failed move must not claim success"
    );

    // The database is unchanged (the trigger aborted the statement) ...
    let row = h
        .pin_row(h.alice_client_key, OWN_SESSION)
        .await
        .expect("row still present");
    assert_eq!(row.0, h.alice_key.to_string(), "row must be untouched");

    // ... and the store must not have moved either: it is the routing source of truth.
    assert_eq!(
        h.store_key(h.alice_client_key, OWN_SESSION),
        Some(h.alice_key),
        "PinStore must not move when the database refused the write"
    );
}

// ---------------------------------------------------------------------------
// 3. Happy path: database and store move together
// ---------------------------------------------------------------------------

/// A successful move must update the row **and** the store. A response-only test would pass with
/// the divergence still present, so both sides are asserted.
#[tokio::test]
#[doc = "Spec: RTE-11, F-108"]
async fn move_pin_updates_sql_and_store_together() {
    let h = harness().await;

    // A second key to move *to*, so `alice_key` stays a valid "from".
    let target_key = h.alice_key;
    let start = h
        .pin_row(h.alice_client_key, OWN_SESSION)
        .await
        .expect("pin exists");
    assert_eq!(start.0, target_key.to_string());

    let (status, body) = h.user_move(OWN_SESSION).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "moving your own pin must succeed; body: {body}"
    );
    assert_eq!(body["moved"], serde_json::json!(true));

    let row = h
        .pin_row(h.alice_client_key, OWN_SESSION)
        .await
        .expect("pin exists");
    assert_eq!(
        row.0,
        target_key.to_string(),
        "the database row must point at the target key"
    );
    assert_eq!(
        h.store_key(h.alice_client_key, OWN_SESSION),
        Some(target_key),
        "the store must agree with the database after a successful move"
    );
}

// ---------------------------------------------------------------------------
// 4. The store update must be scoped to the caller's own rows
// ---------------------------------------------------------------------------

/// Two users' pins in one session hash: a user-scoped move must touch only the caller's.
///
/// `session_hash` is derived from the client key and conversation, so the same hash string can
/// carry pins belonging to different client keys. The ownership predicate scopes the SQL, and the
/// read-back loop that feeds `PinStore` must be scoped the same way — pre-fix it re-selected by
/// `session_hash` alone (with no ownership filter in the admin sibling, and a filter in the user
/// one), so a store update could reach rows the `UPDATE` had not moved.
#[tokio::test]
#[doc = "Spec: USR-3, RTE-11"]
async fn move_pin_touches_only_the_callers_own_rows() {
    let h = harness().await;

    // Give Bob a pin in the same session hash Alice owns pins in.
    let model_pk: String = sqlx::query_scalar("SELECT model_pk FROM models WHERE canonical_id = ?")
        .bind(CANONICAL)
        .fetch_one(&h.pool)
        .await
        .expect("model pk");
    seed_pin(
        &h.pool,
        &h.store,
        h.bob_client_key,
        OWN_SESSION,
        &model_pk,
        h.bob_key,
    )
    .await;

    let (status, body) = h.user_move(OWN_SESSION).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "own move must succeed; body: {body}"
    );

    assert_eq!(
        h.pin_row(h.bob_client_key, OWN_SESSION)
            .await
            .expect("bob row")
            .0,
        h.bob_key.to_string(),
        "Bob's row in the same session hash must be untouched by Alice's move"
    );
    assert_eq!(
        h.store_key(h.bob_client_key, OWN_SESSION),
        Some(h.bob_key),
        "Bob's store entry must be untouched by Alice's move"
    );
}

// ---------------------------------------------------------------------------
// 5. The admin sibling must not regress to the same defects
// ---------------------------------------------------------------------------

/// The admin move handler had the identical `let _ =` pair and the same unconditional store
/// update. It has no ownership predicate (an admin may move any session), so zero rows means the
/// session hash simply has no pins — which must not be reported as a move.
#[tokio::test]
#[doc = "Spec: SEC-17, RTE-11"]
async fn admin_move_of_an_unknown_session_is_not_reported_as_moved() {
    let h = harness().await;
    let admin_session = "admin_move_probe_session_token_0123";
    let admin_csrf = "admin_move_probe_csrf_token_0123456";
    create_admin_session(&h, admin_session, admin_csrf).await;

    let body = serde_json::json!({
        "upstream_key_id": h.alice_key.to_string(),
        "session_hash": "session-with-no-pins-at-all",
    })
    .to_string();
    let request = Request::builder()
        .method("POST")
        .uri("/admin/api/pins/move")
        .header("cookie", format!("llmroller_session={admin_session}"))
        .header("content-type", "application/json")
        .header("x-csrf-token", admin_csrf)
        .body(Body::from(body))
        .expect("request");
    let response = h.app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an admin move that matched no pins must be 404, not a success; body: {json}"
    );
}

/// An admin move that *does* match pins must still work — the 404 above must not be "always 404".
#[tokio::test]
#[doc = "Spec: SEC-17, RTE-11"]
async fn admin_move_of_a_real_session_still_succeeds() {
    let h = harness().await;
    let admin_session = "admin_move_ok_session_token_012345678";
    let admin_csrf = "admin_move_ok_csrf_token_01234567890";
    create_admin_session(&h, admin_session, admin_csrf).await;

    let body = serde_json::json!({
        "upstream_key_id": h.alice_key.to_string(),
        "session_hash": OWN_SESSION,
    })
    .to_string();
    let request = Request::builder()
        .method("POST")
        .uri("/admin/api/pins/move")
        .header("cookie", format!("llmroller_session={admin_session}"))
        .header("content-type", "application/json")
        .header("x-csrf-token", admin_csrf)
        .body(Body::from(body))
        .expect("request");
    let response = h.app.clone().oneshot(request).await.expect("response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an admin move that matched a pin must succeed"
    );

    assert_eq!(
        h.pin_row(h.alice_client_key, OWN_SESSION)
            .await
            .expect("row")
            .0,
        h.alice_key.to_string(),
    );
    assert_eq!(
        h.store_key(h.alice_client_key, OWN_SESSION),
        Some(h.alice_key),
        "the store must agree after a successful admin move"
    );
}

/// Create an admin user and a web session for them, so `/admin/api/*` is reachable.
async fn create_admin_session(h: &MoveHarness, token: &str, csrf: &str) {
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin = create_user(
        &h.pool,
        &system_actor,
        NewUser {
            username: "admin_c36",
            password: "admin-c36-password",
            role: Role::Admin,
            must_change_password: false,
        },
        &OsEntropy,
        100,
    )
    .await
    .expect("admin user");
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    insert_web_session(
        &h.pool,
        NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(token.as_bytes()),
            user_id: admin.id,
            csrf_hash: &llmroller_store::crypto::sha256_hex(csrf.as_bytes()),
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("admin-move-test"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("admin session");
    // The barrier resolves the actor from the session; give the reloader a beat to settle so the
    // first mutation's read-your-writes barrier is not the thing being measured.
    tokio::time::sleep(Duration::from_millis(20)).await;
}
