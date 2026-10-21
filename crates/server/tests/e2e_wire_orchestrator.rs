//! End-to-end wire tests for data-plane proxy orchestrator with mock upstream server.
//!
//! Spec: Section 5.3, Section 10.0-10.5, Section 11.1-11.2, Section 12.1-12.4
//! Requirements: API-1, API-3, API-4, API-5, API-6, API-7, RTE-1, RTE-3, RTE-9, TRN-1, TRN-2, TRN-3, USG-1

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt as _;
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
use llmroller_store::writer::start_writer;

#[derive(Clone, Default)]
struct MockUpstreamState {
    pub chat_calls: Arc<AtomicUsize>,
    pub responses_calls: Arc<AtomicUsize>,
    pub messages_calls: Arc<AtomicUsize>,
}

async fn mock_chat_handler(
    State(state): State<MockUpstreamState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    state.chat_calls.fetch_add(1, Ordering::SeqCst);

    if headers.contains_key("x-fail-429") {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "rate limited" })),
        )
            .into_response();
    }

    let body_str = String::from_utf8_lossy(&body);
    if body_str.contains("\"stream\":true") || body_str.contains("\"stream\": true") {
        let stream_body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello from \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"mock upstream!\"}}]}\n\ndata: [DONE]\n\n";
        (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            stream_body,
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "id": "chatcmpl-mock-upstream-123",
                "object": "chat.completion",
                "created": 1_700_000_000,
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "mock upstream answer" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 15, "total_tokens": 25 }
            })),
        )
            .into_response()
    }
}

async fn mock_responses_handler(
    State(state): State<MockUpstreamState>,
    _body: axum::body::Bytes,
) -> Response {
    state.responses_calls.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        Json(serde_json::json!({
            "id": "resp-mock-1",
            "object": "response",
            "output": "mock responses output"
        })),
    )
        .into_response()
}

async fn mock_messages_handler(
    State(state): State<MockUpstreamState>,
    _body: axum::body::Bytes,
) -> Response {
    state.messages_calls.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        Json(serde_json::json!({
            "id": "msg-mock-1",
            "type": "message",
            "content": [{ "type": "text", "text": "mock messages output" }]
        })),
    )
        .into_response()
}

async fn spawn_mock_upstream() -> (SocketAddr, MockUpstreamState) {
    let mock_state = MockUpstreamState::default();
    let app = Router::new()
        .route("/chat/completions", post(mock_chat_handler))
        .route("/responses", post(mock_responses_handler))
        .route("/messages", post(mock_messages_handler))
        .with_state(mock_state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (addr, mock_state)
}

#[tokio::test]
#[doc = "Spec: API-1, Spec: RTE-9, Spec: TRN-3, Spec: USG-1"]
async fn test_orchestrator_forwards_to_mock_upstream_and_enforces_gates() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;

    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("wire_test.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xBB; 32]);
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let mut tx = pool.begin().await.expect("begin");
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user_id = UserId::generate();

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, 'wire_user', 'h', 'user', 1000)",
    )
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert user");

    let client_key = create_client_key_tx(
        &mut tx,
        &system_actor,
        user_id,
        NewClientKey {
            label: "wire_key".to_string(),
            allowed_models: Some(vec!["gpt-4o".to_string()]), // Gate G6: only gpt-4o allowed
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &entropy,
        1000,
    )
    .await
    .expect("create client key");

    let client_token = client_key.raw_token.expose_secret().to_string();

    let provider = create_provider_tx(
        &mut tx,
        &system_actor,
        NewProvider {
            name: "Mock Provider".to_string(),
            kind: ProviderKind::Custom,
            base_url: format!("http://{}", mock_addr),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: true,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("create provider");

    let upstream_key = create_upstream_key_tx(
        &mut tx,
        &system_actor,
        &master_key,
        "key-0",
        Arc::new(entropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Mock Key".to_string(),
            secret: secret_string("sk-mock-key"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");

    let model_pk = ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id, exposed) VALUES (?, 'gpt-4o', 1)")
        .bind(model_pk.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert model gpt-4o");

    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, discovered_at) \
         VALUES (?, ?, 'gpt-4o', '[\"chat\",\"responses\",\"messages\"]', 'active', 1000)",
    )
    .bind(upstream_key.id.to_string())
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert gpt-4o route");

    let forbidden_model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed) VALUES (?, 'claude-3-5-sonnet', 1)",
    )
    .bind(forbidden_model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model claude");

    tx.commit().await.expect("commit");

    let (reloader_tx, _reloader_rx) = tokio::sync::mpsc::channel(100);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx);

    let snapshot = load_initial_snapshot(&pool).await.expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));

    let mut proxy_auth_state = ProxyAuthState::new(shared_snapshot.clone(), pool.clone(), vec![])
        .with_master_key(master_key.clone());
    proxy_auth_state.is_ready.store(true, Ordering::SeqCst);
    proxy_auth_state.writer = Some(writer);

    let app = data_plane_router(proxy_auth_state);

    // 1. Gate G6 test: model not found in catalog returns HTTP 404
    let req_unknown = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"definitely-not-in-catalog","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("req");
    let res_unknown = app.clone().oneshot(req_unknown).await.expect("res");
    assert_eq!(
        res_unknown.status(),
        StatusCode::NOT_FOUND,
        "Unknown model must return 404 model_not_found"
    );

    // 2. Gate G6 test: model forbidden by client_key.allowed_models returns HTTP 403
    let req_forbidden = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"claude-3-5-sonnet","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("req");
    let res_forbidden = app.clone().oneshot(req_forbidden).await.expect("res");
    assert_eq!(
        res_forbidden.status(),
        StatusCode::FORBIDDEN,
        "Disallowed model must return 403 permission_error"
    );

    // 3. Non-streaming request forwards to mock upstream and returns real response (API-1)
    let req_chat = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("req");
    let res_chat = app.clone().oneshot(req_chat).await.expect("res");
    assert_eq!(res_chat.status(), StatusCode::OK);
    let chat_body = res_chat
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let chat_json: serde_json::Value = serde_json::from_slice(&chat_body).expect("json");
    assert_eq!(
        chat_json["choices"][0]["message"]["content"],
        "mock upstream answer"
    );
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 1);

    // 4. Streaming SSE request receives forwarded chunks from mock upstream (API-6)
    let req_stream = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"stream me"}],"stream":true}"#))
        .expect("req");
    let res_stream = app.clone().oneshot(req_stream).await.expect("res");
    assert_eq!(res_stream.status(), StatusCode::OK);
    let stream_body = res_stream
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let stream_text = String::from_utf8_lossy(&stream_body);
    assert!(stream_text.contains("Hello from "));
    assert!(stream_text.contains("mock upstream!"));
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 2);
}

/// Inserts a user plus an allowed-models client key and returns the raw token.
async fn seed_user_with_client_key(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    username: &str,
    global_budget_micro_usd_month: Option<i64>,
    entropy: &OsEntropy,
) -> (UserId, String) {
    let user_id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, global_budget_micro_usd_month, created_at)
         VALUES (?, ?, 'h', 'user', ?, 1000)",
    )
    .bind(user_id.to_string())
    .bind(username)
    .bind(global_budget_micro_usd_month)
    .execute(&mut **tx)
    .await
    .expect("insert user");

    let client_key = create_client_key_tx(
        tx,
        &ActorScope::system(SystemTask::Writer, SystemSeal::store()),
        user_id,
        NewClientKey {
            label: format!("{username}_key"),
            allowed_models: Some(vec!["gpt-4o".to_string()]),
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        entropy,
        1000,
    )
    .await
    .expect("create client key");
    let token = client_key.raw_token.expose_secret().to_string();
    (user_id, token)
}

/// Seeds a provider/key/model/route triple and returns their identifiers.
async fn seed_provider_route(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    master_key: &MasterKey,
    _entropy: &OsEntropy,
    mock_addr: SocketAddr,
    ownership_scope: OwnershipScope,
    owner_user_id: Option<UserId>,
) -> (llmroller_core::ids::ProviderId, UpstreamKeyId) {
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let provider = create_provider_tx(
        tx,
        &system_actor,
        NewProvider {
            name: "Mock Provider".to_string(),
            kind: ProviderKind::Custom,
            base_url: format!("http://{}", mock_addr),
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

    let upstream_key = create_upstream_key_tx(
        tx,
        &system_actor,
        master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Mock Key".to_string(),
            secret: secret_string("sk-mock-key"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");

    let model_pk = ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id, exposed) VALUES (?, 'gpt-4o', 1)")
        .bind(model_pk.to_string())
        .execute(&mut **tx)
        .await
        .expect("insert model");
    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, discovered_at) \
         VALUES (?, ?, 'gpt-4o', '[\"chat\",\"responses\",\"messages\"]', 'active', 1000)",
    )
    .bind(upstream_key.id.to_string())
    .bind(model_pk.to_string())
    .execute(&mut **tx)
    .await
    .expect("insert route");

    (provider.id, upstream_key.id)
}

async fn build_wire_app(pool: &sqlx::SqlitePool, master_key: &MasterKey) -> Router {
    let (reloader_tx, _reloader_rx) = tokio::sync::mpsc::channel(100);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx);
    let snapshot = load_initial_snapshot(pool).await.expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let mut proxy_auth_state = ProxyAuthState::new(shared_snapshot, pool.clone(), vec![])
        .with_master_key(master_key.clone());
    proxy_auth_state.is_ready.store(true, Ordering::SeqCst);
    proxy_auth_state.writer = Some(writer);
    data_plane_router(proxy_auth_state)
}

fn chat_request(token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("request")
}

async fn seed_base(pool: &sqlx::SqlitePool, master_key: &MasterKey) {
    let entropy = OsEntropy;
    load_or_initialize(pool, "key-0", master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(pool, None, 1).await.expect("seed settings");
}

#[tokio::test]
#[doc = "Spec: USR-5 — F05 own-tier requests bypass a zero user-global budget"]
async fn test_own_tier_bypasses_zero_user_global_budget() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("usr5.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xCC; 32]);
    seed_base(&pool, &master_key).await;

    let entropy = OsEntropy;
    let mut tx = pool.begin().await.expect("begin");
    let (user_id, token) = seed_user_with_client_key(&mut tx, "usr5_user", Some(0), &entropy).await;
    let (_provider_id, _key_id) = seed_provider_route(
        &mut tx,
        &master_key,
        &entropy,
        mock_addr,
        OwnershipScope::Personal,
        Some(user_id),
    )
    .await;
    tx.commit().await.expect("commit");

    let app = build_wire_app(&pool, &master_key).await;
    let response = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "own personal routes must not be blocked by the user-global cap"
    );
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[doc = "Spec: SHR-1 — F05 shared-tier requests enforce the grantor's zero share cap before egress"]
async fn test_shared_tier_zero_cap_returns_429_without_upstream_call() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("shr1.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xDD; 32]);
    seed_base(&pool, &master_key).await;

    let entropy = OsEntropy;
    let mut tx = pool.begin().await.expect("begin");
    let (grantor_id, _grantor_token) =
        seed_user_with_client_key(&mut tx, "shr1_grantor", None, &entropy).await;
    let (grantee_id, grantee_token) =
        seed_user_with_client_key(&mut tx, "shr1_grantee", None, &entropy).await;
    let (provider_id, _key_id) = seed_provider_route(
        &mut tx,
        &master_key,
        &entropy,
        mock_addr,
        OwnershipScope::Personal,
        Some(grantor_id),
    )
    .await;
    let fingerprint: String =
        sqlx::query_scalar("SELECT share_config_fingerprint FROM providers WHERE id = ?")
            .bind(provider_id.to_string())
            .fetch_one(&mut *tx)
            .await
            .expect("provider fingerprint");

    sqlx::query(
        "INSERT INTO provider_shares (
             id, provider_id, owner_user_id, grantee_user_id, max_monthly_micro_usd,
             accepted_at, accepted_config_fingerprint, admin_approved_at, enabled, created_at
         ) VALUES (?, ?, ?, ?, 0, 1000, ?, 1000, 1, 1000)",
    )
    .bind(llmroller_core::ids::ShareId::generate().to_string())
    .bind(provider_id.to_string())
    .bind(grantor_id.to_string())
    .bind(grantee_id.to_string())
    .bind(fingerprint)
    .execute(&mut *tx)
    .await
    .expect("insert share");
    tx.commit().await.expect("commit");

    let app = build_wire_app(&pool, &master_key).await;
    let response = app
        .oneshot(chat_request(&grantee_token))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "an exhausted share cap must fail closed with 429"
    );
    assert_eq!(
        mock_state.chat_calls.load(Ordering::SeqCst),
        0,
        "no upstream request may be issued when the share cap is exhausted"
    );
}
