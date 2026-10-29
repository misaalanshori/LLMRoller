//! A pin move must only succeed onto a target key that can actually serve the pinned model
//! (`ISSUE-0167`).
//!
//! # The defect
//!
//! Both move handlers verified only that the caller **owns** the target key:
//!
//! ```text
//! let key_record = get_upstream_key(&state.auth.pool, &actor, target_key).await...;
//! if !actor.is_admin() && key_record.owner_user_id != Some(user_id) { return 404; }
//! ```
//!
//! Nothing checked that the target key is enabled, is not quarantined/exhausted, or has a
//! `model_routes` row for the pin's `canonical_id`. So a move onto a key with **no route behind it**
//! returned `200 {"moved": true}` while writing both SQLite and `PinStore`. The next request on that
//! session then had no eligible route — the operator's pin was worse than the one it replaced.
//!
//! This is the same family as `ISSUE-0232`: a successful write that produces a broken pin. Every
//! test that expects a refusal therefore also asserts the pin **did not move**, in both places,
//! because a rejection that has already half-written is not a rejection.
//!
//! # The specification says which conditions
//!
//! `14.t.4` is explicit: the handler "verifies that `target_upstream_key_id` passes candidate checks
//! `C1` through `C8` for `(caller, canonical_id)` (returning HTTP `422 target_route_ineligible`
//! otherwise)". The static subset of C1/C4/C6 is checkable from this handler's SQL; the parts
//! that live in `RuntimeState` (C7 breaker, C8 budget, the memory half of C6) are not, because
//! `ApiAppState` exposes no `RuntimeState`. See `CYCLE-44-REPORT-20261029.md` §3-§4.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::{ApiAppState, SetupAppState, control_plane_router_full};
use llmroller_core::ids::{ClientKeyId, UpstreamKeyId, UserId};
use llmroller_core::pins::PinStore;
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

const SESSION_TOKEN: &str = "pin_eligible_session_token_0123456789ab";
const CSRF_TOKEN: &str = "pin_eligible_csrf_token_0123456789abc";
const CANONICAL: &str = "eligible-target-model";
const OWN_SESSION: &str = "session-pinned-for-eligibility";

struct Harness {
    _dir: tempfile::TempDir,
    app: Router,
    pool: sqlx::SqlitePool,
    store: Arc<PinStore>,
    /// The key the pin currently sits on: enabled, healthy, and routed for `CANONICAL`.
    alice_key: UpstreamKeyId,
    alice_client_key: ClientKeyId,
    /// A second key Alice owns that is **not** routed for the model.
    unrouted_key: UpstreamKeyId,
    /// A third key Alice owns that **is** routed, but has been disabled.
    disabled_key: UpstreamKeyId,
    /// A fourth key Alice owns that is routed and enabled, but carries a quarantined health state.
    invalid_key: UpstreamKeyId,
    /// A fifth key Alice owns that is routed and enabled, but whose route is `retired`.
    retired_route_key: UpstreamKeyId,
    /// A sixth key Alice owns that is routed, enabled and healthy — a legitimate move target.
    good_key: UpstreamKeyId,
}

/// Create a provider, an upstream key, and a route for `model_pk`.
///
/// `enabled`, `health` and `route_status` are parameters so a test can build a key that is
/// "there" but cannot serve the model — which is the whole point of this file.
#[allow(clippy::too_many_arguments)]
async fn seed_key(
    pool: &sqlx::SqlitePool,
    actor: &ActorScope,
    master_key: &MasterKey,
    owner: UserId,
    label: &str,
    model_pk: &str,
    enabled: bool,
    health: &str,
    route_status: &str,
    has_route: bool,
) -> (UpstreamKeyId, ClientKeyId) {
    let provider = create_provider(
        pool,
        actor,
        NewProvider {
            name: format!("{label}-provider"),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(owner),
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
        actor,
        master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: format!("{label}-key"),
            secret: secret_string(format!("sk-{label}-eligible-test-secret")),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(enabled),
        },
        100,
    )
    .await
    .expect("create upstream key");

    // `create_upstream_key` always writes `enabled` from the input, but health and route status
    // have no creation parameter, so they are set directly. Doing it here keeps every key in a
    // test in one state, rather than each test patching its own.
    sqlx::query("UPDATE upstream_keys SET health = ? WHERE id = ?")
        .bind(health)
        .bind(key.id.to_string())
        .execute(pool)
        .await
        .expect("set key health");

    if has_route {
        sqlx::query(
            "INSERT INTO model_routes \
             (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, \
              discovered_at) \
             VALUES (?, ?, ?, '[\"chat\"]', ?, 1, 100)",
        )
        .bind(key.id.to_string())
        .bind(model_pk)
        .bind(format!("native-{label}"))
        .bind(route_status)
        .execute(pool)
        .await
        .expect("insert route");
    }

    let client_key = create_client_key(
        pool,
        actor,
        owner,
        NewClientKey {
            label: format!("{label}-client"),
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

    (key.id, client_key)
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("pin_eligible.db");
    let pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let master_key = MasterKey::from_bytes([0xC9; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance");
    seed_defaults(&pool, None, 1).await.expect("settings");

    let model_pk = "01JC44ELIGIBLE00000000000".to_string();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) \
         VALUES (?, ?, 1, 'smooth_weighted_rr')",
    )
    .bind(&model_pk)
    .bind(CANONICAL)
    .execute(&pool)
    .await
    .expect("insert model");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "alice_eligible",
            password: "alice-eligible-password",
            role: Role::User,
            must_change_password: false,
        },
        &OsEntropy,
        1,
    )
    .await
    .expect("create user");
    let actor = ActorScope::user(user.id, Role::User);

    // The key the pin starts on: routed, enabled, healthy.
    let (alice_key, alice_client_key) = seed_key(
        &pool,
        &actor,
        &master_key,
        user.id,
        "alice",
        &model_pk,
        true,
        "healthy",
        "active",
        true,
    )
    .await;
    // No `model_routes` row at all: the key exists and is owned, but nothing says it serves
    // this model.
    let (unrouted_key, _) = seed_key(
        &pool,
        &actor,
        &master_key,
        user.id,
        "unrouted",
        &model_pk,
        true,
        "healthy",
        "active",
        false,
    )
    .await;
    let (disabled_key, _) = seed_key(
        &pool,
        &actor,
        &master_key,
        user.id,
        "disabled",
        &model_pk,
        false,
        "healthy",
        "active",
        true,
    )
    .await;
    let (invalid_key, _) = seed_key(
        &pool,
        &actor,
        &master_key,
        user.id,
        "invalid",
        &model_pk,
        true,
        "invalid",
        "active",
        true,
    )
    .await;
    let (retired_route_key, _) = seed_key(
        &pool,
        &actor,
        &master_key,
        user.id,
        "retired",
        &model_pk,
        true,
        "healthy",
        "retired",
        true,
    )
    .await;
    let (good_key, _) = seed_key(
        &pool,
        &actor,
        &master_key,
        user.id,
        "good",
        &model_pk,
        true,
        "healthy",
        "active",
        true,
    )
    .await;

    // One pin, on `alice_key`, hydrated in both places so a move that touches only one is visible.
    let store = Arc::new(PinStore::new(1_000));
    sqlx::query(
        "INSERT INTO pins \
         (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier, \
          created_at, last_used_at, expires_at) \
         VALUES (?, ?, ?, ?, 1, 100, 100, ?)",
    )
    .bind(alice_client_key.to_string())
    .bind(OWN_SESSION)
    .bind(&model_pk)
    .bind(alice_key.to_string())
    .bind(i64::MAX / 2)
    .execute(&pool)
    .await
    .expect("insert pin");
    store.upsert_confirmed(
        llmroller_core::pins::PinKey {
            client_key_id: alice_client_key,
            canonical_id: CANONICAL.to_string(),
            session_hash: OWN_SESSION.to_string(),
        },
        alice_key,
        llmroller_core::eligibility::CandidatePoolTier::Own,
        llmroller_core::seams::SystemClock::new().now_ms(),
        86_400_000,
    );

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
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x44; 32]));
    api_state.pin_store = Some(store.clone());

    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let csrf_hash = llmroller_store::crypto::sha256_hex(CSRF_TOKEN.as_bytes());
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(SESSION_TOKEN.as_bytes()),
            user_id: user.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("pin-eligible-test"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("insert session");

    Harness {
        _dir: dir,
        app,
        pool,
        store,
        alice_key,
        alice_client_key,
        unrouted_key,
        disabled_key,
        invalid_key,
        retired_route_key,
        good_key,
    }
}

impl Harness {
    /// `POST /api/user/pins/move` as Alice, onto `target`.
    async fn user_move(&self, target: UpstreamKeyId) -> (StatusCode, serde_json::Value) {
        let body = serde_json::json!({
            "upstream_key_id": target.to_string(),
            "session_hash": OWN_SESSION,
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

    /// `POST /admin/api/pins/move` as the admin session created by [`Harness::admin_move`].
    async fn admin_move_with(
        &self,
        cookie: &str,
        csrf: &str,
        session_hash: &str,
        target: UpstreamKeyId,
    ) -> (StatusCode, serde_json::Value) {
        let body = serde_json::json!({
            "upstream_key_id": target.to_string(),
            "session_hash": session_hash,
        })
        .to_string();
        let request = Request::builder()
            .method("POST")
            .uri("/admin/api/pins/move")
            .header("cookie", format!("llmroller_session={cookie}"))
            .header("content-type", "application/json")
            .header("x-csrf-token", csrf)
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

    /// Create an admin user and a web session for them; returns `(cookie_token, csrf_token)`.
    async fn admin_move(&self, token: &str, csrf: &str) -> (String, String) {
        let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
        let admin = create_user(
            &self.pool,
            &system_actor,
            NewUser {
                username: "admin_eligible",
                password: "admin-eligible-password",
                role: Role::Admin,
                must_change_password: false,
            },
            &OsEntropy,
            1,
        )
        .await
        .expect("create admin");
        let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
        let csrf_hash = llmroller_store::crypto::sha256_hex(csrf.as_bytes());
        insert_web_session(
            &self.pool,
            NewWebSession {
                id_hash: &llmroller_store::crypto::sha256_hex(token.as_bytes()),
                user_id: admin.id,
                csrf_hash: &csrf_hash,
                ip_addr: Some("127.0.0.1"),
                user_agent: Some("pin-eligible-admin"),
                created_at: now_ms,
                expires_at: now_ms + 86_400_000,
            },
        )
        .await
        .expect("insert admin session");
        (token.to_string(), csrf.to_string())
    }

    /// The pin's `upstream_key_id` in SQLite, or `None` if the row is absent.
    async fn pin_key_in_sql(&self) -> Option<UpstreamKeyId> {
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT upstream_key_id FROM pins WHERE client_key_id = ? AND session_hash = ?",
        )
        .bind(self.alice_client_key.to_string())
        .bind(OWN_SESSION)
        .fetch_optional(&self.pool)
        .await
        .expect("read pin");
        stored.map(|raw| UpstreamKeyId::parse(&raw).expect("stored key parses"))
    }

    /// The pin's `upstream_key_id` in the live store, or `None`.
    fn pin_key_in_store(&self) -> Option<UpstreamKeyId> {
        self.store.find_active_pin(
            &self.alice_client_key,
            OWN_SESSION,
            Some(CANONICAL),
            llmroller_core::seams::SystemClock::new().now_ms(),
        )
    }

    /// Assert both places still point at `expected` — the "a rejection wrote nothing" check.
    async fn assert_pin_unmoved(&self, expected: UpstreamKeyId, context: &str) {
        assert_eq!(
            self.pin_key_in_sql().await,
            Some(expected),
            "{context}: the SQLite pin row must not have moved"
        );
        assert_eq!(
            self.pin_key_in_store(),
            Some(expected),
            "{context}: the live PinStore entry must not have moved"
        );
    }
}

// ---------------------------------------------------------------------------

#[doc = "Spec: RTE-11, 14.t.4 — target key must have a route for the pinned model"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn moving_a_pin_onto_a_key_with_no_route_for_the_model_is_rejected() {
    let h = harness().await;
    let before = h.alice_key;

    let (status, body) = h.user_move(h.unrouted_key).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a target key with no route for the pinned model is an ineligible target, not a server \
         error and not a success; body: {body}"
    );
    assert_eq!(
        body["error"]["code"], "target_route_ineligible",
        "the error code must be the one `14.t.4` names"
    );
    h.assert_pin_unmoved(before, "no-route rejection").await;
}

#[doc = "Spec: RTE-11, 14.t.4 (C1) — a disabled key cannot serve anything"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn moving_a_pin_onto_a_disabled_key_is_rejected() {
    let h = harness().await;
    let before = h.alice_key;

    let (status, body) = h.user_move(h.disabled_key).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a disabled key is an ineligible target (C1); body: {body}"
    );
    assert_eq!(body["error"]["code"], "target_route_ineligible");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|d| d.contains("disabled")),
        "the message must name the failed condition, got: {body}"
    );
    h.assert_pin_unmoved(before, "disabled-key rejection").await;
}

#[doc = "Spec: RTE-11, 14.t.4 (C6) — a quarantined key is not routable"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn moving_a_pin_onto_an_unhealthy_key_is_rejected() {
    let h = harness().await;
    let before = h.alice_key;

    let (status, body) = h.user_move(h.invalid_key).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a key whose health is `invalid` is not routable (C6); body: {body}"
    );
    assert_eq!(body["error"]["code"], "target_route_ineligible");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|d| d.contains("health")),
        "the message must name the failed condition, got: {body}"
    );
    h.assert_pin_unmoved(before, "unhealthy-key rejection")
        .await;
}

#[doc = "Spec: RTE-11, 14.t.4 (C1) — a retired route is not a route"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn moving_a_pin_onto_a_key_whose_route_is_retired_is_rejected() {
    let h = harness().await;
    let before = h.alice_key;

    let (status, body) = h.user_move(h.retired_route_key).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a route with status `retired` fails C1 (route status must be `active`); body: {body}"
    );
    assert_eq!(body["error"]["code"], "target_route_ineligible");
    h.assert_pin_unmoved(before, "retired-route rejection")
        .await;
}

#[doc = "Spec: RTE-11, 14.t.4 — the non-regression: a genuinely eligible key still moves"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn moving_a_pin_onto_a_valid_key_still_succeeds() {
    let h = harness().await;

    let (status, body) = h.user_move(h.good_key).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "an enabled, healthy key with an active route for the model must still accept a move, \
         otherwise the eligibility check is over-strict; body: {body}"
    );
    assert_eq!(body["moved"], true);
    assert_eq!(
        h.pin_key_in_sql().await,
        Some(h.good_key),
        "the SQLite row must move to the target"
    );
    assert_eq!(
        h.pin_key_in_store(),
        Some(h.good_key),
        "the live PinStore entry must move with it"
    );
}

#[doc = "Spec: RTE-11, 14.t.4 — the admin handler is the same operation, same guard"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn admin_move_onto_a_key_with_no_route_for_the_model_is_rejected() {
    let h = harness().await;
    let (admin_cookie, admin_csrf) = h
        .admin_move("admin_eligible_token_0123456789", "admin-csrf-0123456789ab")
        .await;
    let before = h.alice_key;

    let (status, body) = h
        .admin_move_with(&admin_cookie, &admin_csrf, OWN_SESSION, h.unrouted_key)
        .await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the admin move must apply the same eligibility guard — leaving one handler unguarded is \
         how the family survives; body: {body}"
    );
    assert_eq!(body["error"]["code"], "target_route_ineligible");
    h.assert_pin_unmoved(before, "admin no-route rejection")
        .await;
}

#[doc = "Spec: RTE-11, 14.t.4 — the non-regression for the admin handler"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn admin_move_onto_a_valid_key_still_succeeds() {
    let h = harness().await;
    let (admin_cookie, admin_csrf) = h
        .admin_move("admin_eligible_token_0123456789", "admin-csrf-0123456789ab")
        .await;

    let (status, body) = h
        .admin_move_with(&admin_cookie, &admin_csrf, OWN_SESSION, h.good_key)
        .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "the admin handler must still accept an eligible target; body: {body}"
    );
    assert_eq!(h.pin_key_in_sql().await, Some(h.good_key));
    assert_eq!(h.pin_key_in_store(), Some(h.good_key));
}

#[doc = "Spec: RTE-11, 14.t.4 — a rejected move writes nothing at all"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn a_rejected_move_does_not_partially_write() {
    let h = harness().await;
    let before = h.alice_key;

    // Reject for three different reasons, each of which must leave both stores alone. The point
    // is the observable outcome, not the mechanism: `execute_atomic_mutation` could roll back or
    // the check could run before the transaction, and either is acceptable as long as nothing
    // moved.
    for (target, label) in [
        (h.unrouted_key, "no route"),
        (h.disabled_key, "disabled"),
        (h.invalid_key, "unhealthy"),
        (h.retired_route_key, "retired route"),
    ] {
        let (status, _) = h.user_move(target).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{label} must be rejected"
        );
        h.assert_pin_unmoved(before, label).await;
    }

    // And the pin is still movable afterwards: a rejection must not poison the row.
    let (status, _) = h.user_move(h.good_key).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "after four rejections the pin must still be movable onto a valid target"
    );
    assert_eq!(h.pin_key_in_sql().await, Some(h.good_key));
}

#[doc = "Spec: RTE-11, 14.t.4 — the guard is per model, not per session"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn eligibility_is_checked_for_the_pinned_model_not_any_model() {
    let h = harness().await;

    // A second model that `unrouted_key` *does* serve. The key is therefore a real key with real
    // routes — just none for the model this pin is on. A check that asked only "does this key
    // have any route at all" would wrongly accept the move.
    let other_model_pk = "01JC44ELIGIBLEOTHER0000000".to_string();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) \
         VALUES (?, 'some-other-model', 1, 'smooth_weighted_rr')",
    )
    .bind(&other_model_pk)
    .execute(&h.pool)
    .await
    .expect("insert other model");
    sqlx::query(
        "INSERT INTO model_routes \
         (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, 'native-other', '[\"chat\"]', 'active', 1, 100)",
    )
    .bind(h.unrouted_key.to_string())
    .bind(&other_model_pk)
    .execute(&h.pool)
    .await
    .expect("insert other-model route");

    let before = h.alice_key;
    let (status, body) = h.user_move(h.unrouted_key).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the target key routes a *different* model; the check must be for the pinned model's \
         model_pk, not merely for the existence of any route; body: {body}"
    );
    h.assert_pin_unmoved(before, "wrong-model rejection").await;
}

#[doc = "Spec: USR-3, SEC-17 — ownership is checked before eligibility, and does not leak"]
#[doc = "Spec: ISSUE-0167"]
#[tokio::test]
async fn a_foreign_key_is_still_404_not_422() {
    let h = harness().await;

    // A key that does not exist at all. `14.t.4`'s 422 is for an *ineligible* target; an
    // unknown or foreign key stays 404 so a caller cannot probe another tenant's key space by
    // distinguishing "not yours" from "yours but ineligible" (`USR-3`).
    let nonexistent = UpstreamKeyId::generate();
    let (status, _) = h.user_move(nonexistent).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown key must remain indistinguishable from a foreign one (404), not 422"
    );

    h.assert_pin_unmoved(h.alice_key, "unknown-key rejection")
        .await;
}
