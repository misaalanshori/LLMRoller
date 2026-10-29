//! A user pin move naming a `canonical_id` must move — and validate — only that model
//! (`ISSUE-0235`).
//!
//! # The gap
//!
//! The **admin** handler got a `canonical_id` filter in cycle 36 (`(? IS NULL OR model_pk IN (…))`).
//! The **user** handler did not: its `UPDATE` matched every pin in the session, so a caller naming
//! one model silently repointed all of them onto the target key — which may not serve them.
//!
//! Cycle 44 then added `verify_move_target_for_owned_pins`, which validated the target against
//! **every** model the caller had pinned in that session. So one move produced two wrong outcomes at
//! once: an over-broad write, and a false rejection when any *other* model's pin could not be served.
//!
//! # Why these drive the real endpoint
//!
//! The behaviour under test is which rows the handler's SQL changes and which models it validates.
//! Both are invisible to a unit test of a helper, and the second is the half that turns a corrected
//! filter into a still-broken handler.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::{ApiAppState, SetupAppState, control_plane_router_full};
use llmroller_core::ids::{ClientKeyId, ModelPk, UpstreamKeyId, UserId};
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

const SESSION_TOKEN: &str = "canon_filter_token_0123456789abcdef";
const CSRF_TOKEN: &str = "canon_filter_csrf_0123456789abcdef";
const SESSION: &str = "session-with-two-model-pins";
/// The model the caller names in the move request.
const NAMED_CANONICAL: &str = "named-model";
/// A second model the caller has pinned in the same session.
const OTHER_CANONICAL: &str = "other-model";

struct Harness {
    _dir: tempfile::TempDir,
    app: Router,
    pool: sqlx::SqlitePool,
    store: Arc<PinStore>,
    client_key: ClientKeyId,
    /// The key both pins currently sit on.
    start_key: UpstreamKeyId,
    /// A key that serves BOTH models: the named-model move must succeed onto it.
    both_key: UpstreamKeyId,
    /// A key that serves ONLY the named model: `OTHER_CANONICAL` is unservable by it.
    named_only_key: UpstreamKeyId,
    named_model_pk: ModelPk,
    other_model_pk: ModelPk,
}

#[allow(clippy::too_many_arguments)]
async fn seed_key(
    pool: &sqlx::SqlitePool,
    actor: &ActorScope,
    master_key: &MasterKey,
    owner: UserId,
    label: &str,
    models: &[(&str, &str)],
) -> (UpstreamKeyId, Option<ClientKeyId>) {
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
            secret: secret_string(format!("sk-{label}-canon-test-secret")),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        100,
    )
    .await
    .expect("create upstream key");

    for (model_pk, _) in models {
        sqlx::query(
            "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols,
                                       status, enabled, discovered_at)
             VALUES (?, ?, ?, '[\"chat\"]', 'active', 1, 100)",
        )
        .bind(key.id.to_string())
        .bind(model_pk)
        .bind(format!("native-{label}"))
        .execute(pool)
        .await
        .expect("insert route");
    }

    (key.id, None)
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("canon_filter.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let master_key = MasterKey::from_bytes([0x4D; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance");
    seed_defaults(&pool, None, 1).await.expect("settings");

    let named_model_pk = ModelPk::generate();
    let other_model_pk = ModelPk::generate();
    for (pk, canonical) in [
        (named_model_pk, NAMED_CANONICAL),
        (other_model_pk, OTHER_CANONICAL),
    ] {
        sqlx::query(
            "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy)
             VALUES (?, ?, 1, 'smooth_weighted_rr')",
        )
        .bind(pk.to_string())
        .bind(canonical)
        .execute(&pool)
        .await
        .expect("insert model");
    }

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "canon_filter_user",
            password: "canon-filter-password-123",
            role: Role::User,
            must_change_password: false,
        },
        &OsEntropy,
        1,
    )
    .await
    .expect("create user");
    let actor = ActorScope::user(user.id, Role::User);

    // The starting key serves both models.
    let (start_key, _) = seed_key(
        &pool,
        &actor,
        &master_key,
        user.id,
        "start",
        &[
            (&named_model_pk.to_string(), NAMED_CANONICAL),
            (&other_model_pk.to_string(), OTHER_CANONICAL),
        ],
    )
    .await;
    let (both_key, _) = seed_key(
        &pool,
        &actor,
        &master_key,
        user.id,
        "both",
        &[
            (&named_model_pk.to_string(), NAMED_CANONICAL),
            (&other_model_pk.to_string(), OTHER_CANONICAL),
        ],
    )
    .await;
    // Serves ONLY the named model — this is the key that exposes the false rejection.
    let (named_only_key, _) = seed_key(
        &pool,
        &actor,
        &master_key,
        user.id,
        "namedonly",
        &[(&named_model_pk.to_string(), NAMED_CANONICAL)],
    )
    .await;

    let client_key = create_client_key(
        &pool,
        &actor,
        user.id,
        NewClientKey {
            label: "canon-filter-client".to_string(),
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

    // Two pins in one session, one per model, both on `start_key`, hydrated in SQLite and the store.
    let store = Arc::new(PinStore::new(1_000));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    for (pk, canonical) in [
        (named_model_pk, NAMED_CANONICAL),
        (other_model_pk, OTHER_CANONICAL),
    ] {
        sqlx::query(
            "INSERT INTO pins (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier,
                               created_at, last_used_at, expires_at)
             VALUES (?, ?, ?, ?, 0, 100, 100, ?)",
        )
        .bind(client_key.to_string())
        .bind(SESSION)
        .bind(pk.to_string())
        .bind(start_key.to_string())
        .bind(i64::MAX / 2)
        .execute(&pool)
        .await
        .expect("insert pin");
        store.upsert_confirmed(
            llmroller_core::pins::PinKey {
                client_key_id: client_key,
                canonical_id: canonical.to_string(),
                session_hash: SESSION.to_string(),
            },
            start_key,
            llmroller_core::eligibility::CandidatePoolTier::Own,
            now_ms,
            86_400_000,
        );
    }

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
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x2B; 32]));
    api_state.pin_store = Some(store.clone());

    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    let csrf_hash = llmroller_store::crypto::sha256_hex(CSRF_TOKEN.as_bytes());
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(SESSION_TOKEN.as_bytes()),
            user_id: user.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("canon-filter-test"),
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
        client_key,
        start_key,
        both_key,
        named_only_key,
        named_model_pk,
        other_model_pk,
    }
}

impl Harness {
    /// `POST /api/user/pins/move`, optionally naming a `canonical_id`.
    async fn move_pin(
        &self,
        target: UpstreamKeyId,
        canonical_id: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut payload = serde_json::json!({
            "upstream_key_id": target.to_string(),
            "session_hash": SESSION,
        });
        if let Some(canonical) = canonical_id {
            payload["canonical_id"] = serde_json::json!(canonical);
        }
        let request = Request::builder()
            .method("POST")
            .uri("/api/user/pins/move")
            .header("cookie", format!("llmroller_session={SESSION_TOKEN}"))
            .header("content-type", "application/json")
            .header("x-csrf-token", CSRF_TOKEN)
            .body(Body::from(payload.to_string()))
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

    /// The upstream key an event's pin row points at, read from SQLite.
    async fn key_in_sql(&self, model_pk: &ModelPk) -> Option<UpstreamKeyId> {
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT upstream_key_id FROM pins
              WHERE client_key_id = ? AND session_hash = ? AND model_pk = ?",
        )
        .bind(self.client_key.to_string())
        .bind(SESSION)
        .bind(model_pk.to_string())
        .fetch_optional(&self.pool)
        .await
        .expect("read pin");
        stored.map(|raw| UpstreamKeyId::parse(&raw).expect("stored key parses"))
    }

    /// The upstream key the live `PinStore` routes that model to.
    fn key_in_store(&self, canonical_id: &str) -> Option<UpstreamKeyId> {
        self.store.find_active_pin(
            &self.client_key,
            SESSION,
            Some(canonical_id),
            llmroller_core::seams::SystemClock::new().now_ms(),
        )
    }
}

// ---------------------------------------------------------------------------

#[doc = "Spec: RTE-11, Section 14.t.4 — a named model move touches one model"]
#[doc = "Spec: ISSUE-0235"]
#[tokio::test]
async fn a_user_move_naming_a_canonical_id_moves_only_that_model() {
    let h = harness().await;

    let (status, body) = h.move_pin(h.both_key, Some(NAMED_CANONICAL)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a move onto a key serving the named model must succeed: {body}"
    );

    assert_eq!(
        h.key_in_sql(&h.named_model_pk).await,
        Some(h.both_key),
        "the named model's pin must move"
    );
    assert_eq!(
        h.key_in_store(NAMED_CANONICAL),
        Some(h.both_key),
        "the named model's live pin must move too"
    );
    assert_eq!(
        h.key_in_sql(&h.other_model_pk).await,
        Some(h.start_key),
        "the OTHER model's pin must be untouched in SQLite — the move named one model, not the \
         session"
    );
    assert_eq!(
        h.key_in_store(OTHER_CANONICAL),
        Some(h.start_key),
        "the OTHER model's live pin must be untouched"
    );
}

#[doc = "Spec: RTE-11, Section 14.t.4 — validation follows the named model only"]
#[doc = "Spec: ISSUE-0235"]
#[tokio::test]
async fn a_user_move_naming_a_canonical_id_is_validated_only_for_that_model() {
    let h = harness().await;

    // `named_only_key` serves NAMED_CANONICAL but not OTHER_CANONICAL. The caller names the model
    // it serves, so the move must succeed — validating every model the caller owns in the session
    // would reject it, which is the false rejection this issue records.
    let (status, body) = h.move_pin(h.named_only_key, Some(NAMED_CANONICAL)).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "the target serves the model the caller named, so the move must succeed even though it \
         cannot serve another model pinned in the same session: {body}"
    );
    assert_eq!(
        h.key_in_sql(&h.named_model_pk).await,
        Some(h.named_only_key)
    );
    assert_eq!(
        h.key_in_sql(&h.other_model_pk).await,
        Some(h.start_key),
        "the unservable-by-target model must not be moved onto that key"
    );
}

#[doc = "Spec: RTE-11 — no canonical_id still moves the whole session"]
#[doc = "Spec: ISSUE-0235"]
#[tokio::test]
async fn a_user_move_without_a_canonical_id_still_moves_every_pin() {
    let h = harness().await;

    let (status, body) = h.move_pin(h.both_key, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a session-wide move must keep working: {body}"
    );
    assert_eq!(
        h.key_in_sql(&h.named_model_pk).await,
        Some(h.both_key),
        "both models' pins must move when no model is named"
    );
    assert_eq!(h.key_in_sql(&h.other_model_pk).await, Some(h.both_key));
    assert_eq!(h.key_in_store(NAMED_CANONICAL), Some(h.both_key));
    assert_eq!(h.key_in_store(OTHER_CANONICAL), Some(h.both_key));
}

#[doc = "Spec: RTE-11 — a session-wide move is still validated against every model it moves"]
#[doc = "Spec: ISSUE-0235"]
#[tokio::test]
async fn a_session_wide_move_is_still_rejected_when_a_model_is_unservable() {
    let h = harness().await;

    // No canonical_id, and the target cannot serve OTHER_CANONICAL. A session-wide move would move
    // a pin onto a key that cannot serve it, so it must be rejected — that is cycle 44's guard
    // still doing its job, not the false rejection this issue fixes.
    let (status, body) = h.move_pin(h.named_only_key, None).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a session-wide move onto a key that cannot serve one of the session's models must be \
         rejected: {body}"
    );
    assert_eq!(body["error"]["code"], "target_route_ineligible");
    assert_eq!(
        h.key_in_sql(&h.named_model_pk).await,
        Some(h.start_key),
        "a rejected move must not move anything"
    );
    assert_eq!(h.key_in_sql(&h.other_model_pk).await, Some(h.start_key));
}

#[doc = "Spec: RTE-11, USR-3 — an unknown canonical_id moves nothing and reports 404"]
#[doc = "Spec: ISSUE-0235"]
#[tokio::test]
async fn a_move_naming_an_unknown_canonical_id_moves_nothing() {
    let h = harness().await;

    let (status, body) = h.move_pin(h.both_key, Some("no-such-model")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "naming a model with no pin in the session matches nothing, which is the existing \
         zero-row 404: {body}"
    );
    assert_eq!(
        h.key_in_sql(&h.named_model_pk).await,
        Some(h.start_key),
        "nothing may move when the named model has no pin"
    );
    assert_eq!(h.key_in_sql(&h.other_model_pk).await, Some(h.start_key));
}
