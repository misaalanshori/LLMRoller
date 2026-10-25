//! Cross-tenant routing isolation and fail-closed key unsealing regressions.
//!
//! Spec: Section 10.1, Section 10.2, Section 11.1, Section 5.4
//! Requirements: SEC-2, SEC-17, SHR-1, RTR-1, RTE-1, USR-3, PERF-1

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_core::ids::{ModelPk, UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, SystemSeal, SystemTask};
use llmroller_core::seams::{ExposeSecret as _, OsEntropy, secret_string};
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_server::reloader::load_initial_snapshot;
use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;

#[derive(Clone)]
struct MockUpstreamState {
    calls: Arc<AtomicUsize>,
    status: Arc<std::sync::atomic::AtomicU16>,
}

impl Default for MockUpstreamState {
    fn default() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            status: Arc::new(std::sync::atomic::AtomicU16::new(200)),
        }
    }
}

async fn mock_chat(State(state): State<MockUpstreamState>, _body: axum::body::Bytes) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    let code = state.status.load(Ordering::SeqCst);
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
    if !status.is_success() {
        return (
            status,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "error": { "message": "mock error", "code": "upstream_error" }
            })),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        Json(serde_json::json!({
            "id": "chatcmpl-isolation",
            "object": "chat.completion",
            "created": 1_700_000_000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        })),
    )
        .into_response()
}

async fn spawn_mock_upstream() -> (SocketAddr, MockUpstreamState) {
    let state = MockUpstreamState::default();
    let app = Router::new()
        .route("/chat/completions", post(mock_chat))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, state)
}

async fn spawn_mock_upstream_status(status: StatusCode) -> (SocketAddr, MockUpstreamState) {
    let state = MockUpstreamState {
        calls: Arc::new(AtomicUsize::new(0)),
        status: Arc::new(std::sync::atomic::AtomicU16::new(status.as_u16())),
    };
    let app = Router::new()
        .route("/chat/completions", post(mock_chat))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, state)
}

struct Harness {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    master_key: MasterKey,
    system_actor: ActorScope,
}

async fn harness() -> Harness {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("isolation.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xAA; 32]);
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&pool, None, 1).await.expect("seed settings");
    Harness {
        _dir: dir,
        pool,
        master_key,
        system_actor: ActorScope::system(SystemTask::Writer, SystemSeal::store()),
    }
}

async fn insert_user(pool: &sqlx::SqlitePool, username: &str) -> UserId {
    let user_id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, can_use_global_providers, created_at) \
         VALUES (?, ?, 'h', 'user', 1, 1000)",
    )
    .bind(user_id.to_string())
    .bind(username)
    .execute(pool)
    .await
    .expect("insert user");
    user_id
}

async fn create_client_key(
    harness: &Harness,
    user_id: UserId,
    label: &str,
    budget_micro_usd_month: Option<i64>,
    routing_scope: Option<&str>,
) -> String {
    let mut tx = harness.pool.begin().await.expect("begin");
    let created = create_client_key_tx(
        &mut tx,
        &harness.system_actor,
        user_id,
        NewClientKey {
            label: label.to_string(),
            service_account_id: None,
            allowed_models: None,
            routing_scope: routing_scope.map(str::to_string),
            rate_limit_rpm: None,
            budget_micro_usd_month,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &OsEntropy,
        1000,
    )
    .await
    .expect("create client key");
    tx.commit().await.expect("commit");
    created.raw_token.expose_secret().to_string()
}

async fn create_provider(
    harness: &Harness,
    name: &str,
    base_url: String,
    ownership_scope: OwnershipScope,
    owner_user_id: Option<UserId>,
) -> llmroller_core::ids::ProviderId {
    let mut tx = harness.pool.begin().await.expect("begin");
    let provider = create_provider_tx(
        &mut tx,
        &harness.system_actor,
        NewProvider {
            name: name.to_string(),
            kind: ProviderKind::Custom,
            base_url,
            config_json: "{}".to_string(),
            ownership_scope,
            owner_user_id,
            usage_url_cross_origin_ack: false,
            allow_private_network: true,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("create provider");
    tx.commit().await.expect("commit");
    provider.id
}

async fn create_upstream_key(
    harness: &Harness,
    provider_id: llmroller_core::ids::ProviderId,
    label: &str,
    priority: i64,
) -> UpstreamKeyId {
    create_upstream_key_with_tier(harness, provider_id, label, priority, 1).await
}

async fn create_upstream_key_with_tier(
    harness: &Harness,
    provider_id: llmroller_core::ids::ProviderId,
    label: &str,
    priority: i64,
    strict_tier: i64,
) -> UpstreamKeyId {
    let mut tx = harness.pool.begin().await.expect("begin");
    let key = create_upstream_key_tx(
        &mut tx,
        &harness.system_actor,
        &harness.master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id,
            label: label.to_string(),
            secret: secret_string(format!("sk-{label}")),
            priority: Some(priority),
            strict_tier: Some(strict_tier),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");
    tx.commit().await.expect("commit");
    key.id
}

async fn insert_model(harness: &Harness, canonical_id: &str, strategy: Option<&str>) -> ModelPk {
    let model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) VALUES (?, ?, 1, ?)",
    )
    .bind(model_pk.to_string())
    .bind(canonical_id)
    .bind(strategy)
    .execute(&harness.pool)
    .await
    .expect("insert model");
    model_pk
}

async fn insert_route(
    harness: &Harness,
    key_id: UpstreamKeyId,
    model_pk: ModelPk,
    upstream_model_id: &str,
    enabled: bool,
) {
    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, ?, '[\"chat\",\"responses\",\"messages\"]', 'active', ?, 1000)",
    )
    .bind(key_id.to_string())
    .bind(model_pk.to_string())
    .bind(upstream_model_id)
    .bind(i64::from(enabled))
    .execute(&harness.pool)
    .await
    .expect("insert model route");
}

async fn build_app(
    harness: &Harness,
    proxy_master_key: MasterKey,
) -> (Router, Arc<llmroller_core::runtime_state::RuntimeState>) {
    let snapshot = load_initial_snapshot(&harness.pool)
        .await
        .expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let state = ProxyAuthState::new(shared_snapshot, harness.pool.clone(), vec![])
        .with_master_key(proxy_master_key);
    state
        .is_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let runtime_state = state.runtime_state.clone();
    (data_plane_router(state), runtime_state)
}

fn chat_request(token: &str, model: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            r#"{{"model":"{model}","messages":[{{"role":"user","content":"hello"}}]}}"#
        )))
        .expect("request")
}

#[tokio::test]
async fn test_personal_key_isolated_across_users_without_share_grant() {
    let harness = harness().await;
    let alice = insert_user(&harness.pool, "alice").await;
    let bob = insert_user(&harness.pool, "bob").await;
    let (mock_addr, mock_state) = spawn_mock_upstream().await;

    let alice_provider = create_provider(
        &harness,
        "Alice personal",
        format!("http://{mock_addr}"),
        OwnershipScope::Personal,
        Some(alice),
    )
    .await;
    let alice_key = create_upstream_key(&harness, alice_provider, "alice-key", 100).await;
    let model_pk = insert_model(&harness, "gpt-4o", None).await;
    insert_route(&harness, alice_key, model_pk, "gpt-4o", true).await;

    let bob_token = create_client_key(&harness, bob, "bob-key", None, None).await;
    let (app, _runtime) = build_app(&harness, harness.master_key.clone()).await;

    let response = app
        .oneshot(chat_request(&bob_token, "gpt-4o"))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a foreign personal key without a share grant must be invisible"
    );
    assert_eq!(
        mock_state.calls.load(Ordering::SeqCst),
        0,
        "no upstream call may reach Alice's personal key"
    );
}

#[tokio::test]
async fn test_disabled_model_route_does_not_fallback_to_unrelated_upstream_keys() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "carol").await;
    let (mock_a, state_a) = spawn_mock_upstream().await;
    let (mock_b, state_b) = spawn_mock_upstream().await;

    let provider_a = create_provider(
        &harness,
        "Provider A",
        format!("http://{mock_a}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_a = create_upstream_key(&harness, provider_a, "key-a", 100).await;
    let provider_b = create_provider(
        &harness,
        "Provider B",
        format!("http://{mock_b}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_b = create_upstream_key(&harness, provider_b, "key-b", 100).await;

    let model_gpt = insert_model(&harness, "gpt-4o", None).await;
    let model_claude = insert_model(&harness, "claude-sonnet-4-5", None).await;
    insert_route(&harness, key_a, model_gpt, "gpt-4o", false).await;
    insert_route(&harness, key_b, model_claude, "claude-sonnet-4-5", true).await;

    let token = create_client_key(&harness, user, "carol-key", None, None).await;
    let (app, _runtime) = build_app(&harness, harness.master_key.clone()).await;

    let response = app
        .oneshot(chat_request(&token, "gpt-4o"))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a disabled route must not be replaced by an unrelated key"
    );
    assert_eq!(
        state_a.calls.load(Ordering::SeqCst),
        0,
        "the disabled route's provider must not be called"
    );
    assert_eq!(
        state_b.calls.load(Ordering::SeqCst),
        0,
        "another model's upstream key must never be queried"
    );
}

#[tokio::test]
async fn test_wrong_master_key_fails_closed_and_quarantines_key() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "dave").await;
    let (mock_addr, mock_state) = spawn_mock_upstream().await;

    let provider = create_provider(
        &harness,
        "Global",
        format!("http://{mock_addr}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_id = create_upstream_key(&harness, provider, "sealed-key", 100).await;
    let model_pk = insert_model(&harness, "gpt-4o", None).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o", true).await;

    let token = create_client_key(&harness, user, "dave-key", None, None).await;
    let wrong_master_key = MasterKey::from_bytes([0xBA; 32]);
    let (app, runtime) = build_app(&harness, wrong_master_key).await;

    let response = app
        .oneshot(chat_request(&token, "gpt-4o"))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "AEAD failure must fail closed with no default secret"
    );
    assert_eq!(
        mock_state.calls.load(Ordering::SeqCst),
        0,
        "an unsealed key must never be dispatched"
    );
    let health = runtime.key_health_entry(key_id);
    assert_eq!(
        health.quarantine_reason.as_deref(),
        Some("aead_unseal_failed"),
        "the key must be quarantined for operators"
    );
}

#[tokio::test]
async fn test_sticky_session_pin_keeps_requests_on_first_key() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "erin").await;
    let (mock_a, state_a) = spawn_mock_upstream().await;
    let (mock_b, state_b) = spawn_mock_upstream().await;

    let provider_a = create_provider(
        &harness,
        "Sticky A",
        format!("http://{mock_a}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_a = create_upstream_key(&harness, provider_a, "sticky-a", 100).await;
    let provider_b = create_provider(
        &harness,
        "Sticky B",
        format!("http://{mock_b}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_b = create_upstream_key(&harness, provider_b, "sticky-b", 100).await;

    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_a, model_pk, "gpt-4o", true).await;
    insert_route(&harness, key_b, model_pk, "gpt-4o", true).await;

    let token = create_client_key(&harness, user, "erin-key", None, None).await;
    let (app, _runtime) = build_app(&harness, harness.master_key.clone()).await;

    let request_with_session = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-llmroller-session-id", "sticky-session-1")
            .body(Body::from(
                r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
            ))
            .expect("request")
    };

    let first = app
        .clone()
        .oneshot(request_with_session())
        .await
        .expect("first response");
    assert_eq!(first.status(), StatusCode::OK);
    let a_after_first = state_a.calls.load(Ordering::SeqCst);
    let b_after_first = state_b.calls.load(Ordering::SeqCst);
    assert_eq!(
        a_after_first + b_after_first,
        1,
        "exactly one upstream call"
    );

    let second = app
        .oneshot(request_with_session())
        .await
        .expect("second response");
    assert_eq!(second.status(), StatusCode::OK);
    let a_after_second = state_a.calls.load(Ordering::SeqCst);
    let b_after_second = state_b.calls.load(Ordering::SeqCst);
    if a_after_first == 1 {
        assert_eq!(a_after_second, 2, "the second request must follow key A");
        assert_eq!(
            b_after_second, 0,
            "the pinned session must not move to key B"
        );
    } else {
        assert_eq!(b_after_second, 2, "the second request must follow key B");
        assert_eq!(
            a_after_second, 0,
            "the pinned session must not move to key A"
        );
    }
}

#[tokio::test]
async fn test_naive_round_robin_distributes_across_healthy_keys() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "frank").await;
    let (mock_a, state_a) = spawn_mock_upstream().await;
    let (mock_b, state_b) = spawn_mock_upstream().await;

    let provider_a = create_provider(
        &harness,
        "RR A",
        format!("http://{mock_a}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_a = create_upstream_key(&harness, provider_a, "rr-a", 100).await;
    let provider_b = create_provider(
        &harness,
        "RR B",
        format!("http://{mock_b}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_b = create_upstream_key(&harness, provider_b, "rr-b", 100).await;

    let model_pk = insert_model(&harness, "gpt-4o", Some("naive_rr")).await;
    insert_route(&harness, key_a, model_pk, "gpt-4o", true).await;
    insert_route(&harness, key_b, model_pk, "gpt-4o", true).await;

    let token = create_client_key(&harness, user, "frank-key", None, None).await;
    let (app, _runtime) = build_app(&harness, harness.master_key.clone()).await;

    for turn in 0..4 {
        // Distinct sessions must be distributed by naive RR; identical openings
        // would share a Step 8 fingerprint and legitimately stick to one key.
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-llmroller-session-id", format!("rr-session-{turn}"))
            .body(Body::from(
                r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
            ))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    assert_eq!(
        state_a.calls.load(Ordering::SeqCst),
        2,
        "naive RR alternates"
    );
    assert_eq!(
        state_b.calls.load(Ordering::SeqCst),
        2,
        "naive RR alternates"
    );
}

#[tokio::test]
async fn test_client_monthly_cap_rejects_before_upstream_dispatch() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "grace").await;
    let (mock_addr, mock_state) = spawn_mock_upstream().await;

    let provider = create_provider(
        &harness,
        "Capped",
        format!("http://{mock_addr}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_id = create_upstream_key(&harness, provider, "capped-key", 100).await;
    let model_pk = insert_model(&harness, "gpt-4o", None).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o", true).await;

    let token = create_client_key(&harness, user, "grace-key", Some(1), None).await;
    let (app, _runtime) = build_app(&harness, harness.master_key.clone()).await;

    let response = app
        .oneshot(chat_request(&token, "gpt-4o"))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "monthly cap must reject before upstream dispatch"
    );
    assert_eq!(
        mock_state.calls.load(Ordering::SeqCst),
        0,
        "no upstream request may be made after the cap is exceeded"
    );
}

#[doc = "Spec: RTE-6, F031 - Strict tier prevents failover from Tier 1 to Tier 2 when Tier 1 key fails"]
#[tokio::test]
async fn regression_test_f031_strict_tier_blocks_fallback_to_lower_tiers() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "f031-user").await;
    let (mock_a, state_a) = spawn_mock_upstream_status(StatusCode::INTERNAL_SERVER_ERROR).await;
    let (mock_b, state_b) = spawn_mock_upstream().await;

    let provider_a = create_provider(
        &harness,
        "Provider Tier 1",
        format!("http://{mock_a}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_a = create_upstream_key_with_tier(&harness, provider_a, "key-tier-1", 100, 1).await;

    let provider_b = create_provider(
        &harness,
        "Provider Tier 2",
        format!("http://{mock_b}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key_b = create_upstream_key_with_tier(&harness, provider_b, "key-tier-2", 100, 2).await;

    let model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy, distribution) VALUES (?, 'gpt-4o-strict', 1, 'smooth_weighted_rr', 'strict')",
    )
    .bind(model_pk.to_string())
    .execute(&harness.pool)
    .await
    .expect("insert strict model");

    insert_route(&harness, key_a, model_pk, "gpt-4o-strict", true).await;
    insert_route(&harness, key_b, model_pk, "gpt-4o-strict", true).await;

    let token = create_client_key(&harness, user, "f031-client-key", None, None).await;
    let (app, _runtime) = build_app(&harness, harness.master_key.clone()).await;

    let response = app
        .oneshot(chat_request(&token, "gpt-4o-strict"))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::BAD_GATEWAY,
        "when Tier 1 fails and strict tier is active, request must not fall back to Tier 2"
    );
    assert_eq!(
        state_a.calls.load(Ordering::SeqCst),
        1,
        "Tier 1 key was attempted"
    );
    assert_eq!(
        state_b.calls.load(Ordering::SeqCst),
        0,
        "Tier 2 key must NEVER be queried because strict tier forbids cross-tier fallback"
    );
}
