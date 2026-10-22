//! End-to-end wire tests for data-plane proxy orchestrator with mock upstream server.
//!
//! Spec: Section 5.3, Section 10.0-10.5, Section 11.1-11.2, Section 12.1-12.4
//! Requirements: API-1, API-3, API-4, API-5, API-6, API-7, RTE-1, RTE-3, RTE-9, TRN-1, TRN-2, TRN-3, USG-1

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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
use llmroller_core::seams::{ExposeSecret as _, OsEntropy, secret_bytes, secret_string};
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_proxy::orchestrator::PreflightBudgetSemaphore;
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

#[derive(Clone, Debug)]
struct RecordedRequest {
    pub path: &'static str,
    pub headers: HeaderMap,
    pub body: String,
}

#[derive(Clone, Default)]
struct MockUpstreamState {
    pub chat_calls: Arc<AtomicUsize>,
    pub responses_calls: Arc<AtomicUsize>,
    pub messages_calls: Arc<AtomicUsize>,
    pub recorded: Arc<Mutex<Vec<RecordedRequest>>>,
}

fn record_request(state: &MockUpstreamState, path: &'static str, headers: &HeaderMap, body: &[u8]) {
    state
        .recorded
        .lock()
        .expect("recorded requests")
        .push(RecordedRequest {
            path,
            headers: headers.clone(),
            body: String::from_utf8_lossy(body).to_string(),
        });
}

fn configured_upstream_error(headers: &HeaderMap) -> Option<Response> {
    let secret = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();

    if headers.contains_key("x-fail-moderation") {
        return Some(
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": {
                        "code": "content_filter",
                        "message": "Prompt blocked by moderation"
                    }
                })),
            )
                .into_response(),
        );
    }
    if headers.contains_key("x-fail-plan") || secret == "sk-plan" {
        return Some(
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": {
                        "code": "upgrade_required",
                        "message": "not available on your plan"
                    }
                })),
            )
                .into_response(),
        );
    }
    if secret.starts_with("sk-quota") {
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": {
                    "code": "insufficient_quota",
                    "message": "exceeded your current quota"
                }
            })),
        )
            .into_response();
        response
            .headers_mut()
            .insert("retry-after", axum::http::HeaderValue::from_static("7"));
        return Some(response);
    }
    if secret == "sk-503" {
        return Some(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "upstream unavailable" })),
            )
                .into_response(),
        );
    }
    if secret == "sk-401" {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": { "code": "invalid_api_key" } })),
            )
                .into_response(),
        );
    }
    None
}

async fn mock_chat_handler(
    State(state): State<MockUpstreamState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    state.chat_calls.fetch_add(1, Ordering::SeqCst);
    record_request(&state, "/chat/completions", &headers, &body);

    if let Some(response) = configured_upstream_error(&headers) {
        return response;
    }

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
            [
                ("content-type", "text/event-stream"),
                ("openai-organization", "org-secret-admin-id"),
                ("x-ratelimit-remaining-requests", "999"),
            ],
            stream_body,
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            [
                ("content-type", "application/json"),
                ("x-upstream-custom", "yes"),
                ("openai-organization", "org-secret-admin-id"),
                ("x-ratelimit-remaining-requests", "999"),
            ],
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
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    state.responses_calls.fetch_add(1, Ordering::SeqCst);
    record_request(&state, "/responses", &headers, &body);

    if body_requests_stream(&body) {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-mock-1\"}}\n\ndata: [DONE]\n\n",
        )
            .into_response();
    }

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
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    state.messages_calls.fetch_add(1, Ordering::SeqCst);
    record_request(&state, "/messages", &headers, &body);

    if body_requests_stream(&body) {
        return (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-mock-1\"}}\n\ndata: [DONE]\n\n",
        )
            .into_response();
    }

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

fn body_requests_stream(body: &[u8]) -> bool {
    let body = String::from_utf8_lossy(body);
    body.contains("\"stream\":true") || body.contains("\"stream\": true")
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

    // An admin caller on the same Global key must still receive diagnostic headers.
    let admin_user_id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, 'wire_admin', 'h', 'admin', 1000)",
    )
    .bind(admin_user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert admin user");
    let admin_client_key = create_client_key_tx(
        &mut tx,
        &system_actor,
        admin_user_id,
        NewClientKey {
            label: "wire_admin_key".to_string(),
            allowed_models: Some(vec!["gpt-4o".to_string()]),
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
    .expect("create admin client key");
    let admin_token = admin_client_key.raw_token.expose_secret().to_string();

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
    assert!(
        !res_chat.headers().contains_key("openai-organization"),
        "non-admin non-streaming responses must not leak org identity"
    );
    assert!(
        !res_chat
            .headers()
            .contains_key("x-ratelimit-remaining-requests"),
        "non-admin non-streaming responses must not leak pooled rate limits"
    );
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
    assert!(
        !res_stream.headers().contains_key("openai-organization"),
        "non-admin streaming responses must not leak org identity"
    );
    assert!(
        !res_stream
            .headers()
            .contains_key("x-ratelimit-remaining-requests"),
        "non-admin streaming responses must not leak pooled rate limits"
    );
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

    // Admin callers keep diagnostic rate-limit headers on both paths.
    let req_admin_stream = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"admin stream"}],"stream":true}"#,
        ))
        .expect("req");
    let res_admin_stream = app.clone().oneshot(req_admin_stream).await.expect("res");
    assert_eq!(res_admin_stream.status(), StatusCode::OK);
    assert_eq!(
        res_admin_stream
            .headers()
            .get("x-ratelimit-remaining-requests")
            .and_then(|value| value.to_str().ok()),
        Some("999")
    );
    let _ = res_admin_stream
        .into_body()
        .collect()
        .await
        .expect("admin stream body");

    let req_admin_plain = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"admin plain"}]}"#,
        ))
        .expect("req");
    let res_admin_plain = app.clone().oneshot(req_admin_plain).await.expect("res");
    assert_eq!(res_admin_plain.status(), StatusCode::OK);
    assert_eq!(
        res_admin_plain
            .headers()
            .get("x-ratelimit-remaining-requests")
            .and_then(|value| value.to_str().ok()),
        Some("999")
    );
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
    config_json: &str,
) -> (llmroller_core::ids::ProviderId, UpstreamKeyId) {
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let provider = create_provider_tx(
        tx,
        &system_actor,
        NewProvider {
            name: format!(
                "Mock Provider {}",
                llmroller_core::ids::ProviderId::generate()
            ),
            kind: ProviderKind::Custom,
            base_url: format!("http://{}", mock_addr),
            config_json: config_json.to_string(),
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
    build_wire_app_with_max_body(pool, master_key, 32).await
}

async fn build_wire_app_with_max_body(
    pool: &sqlx::SqlitePool,
    master_key: &MasterKey,
    max_body_mb: usize,
) -> Router {
    let (reloader_tx, _reloader_rx) = tokio::sync::mpsc::channel(100);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx);
    let snapshot = load_initial_snapshot(pool).await.expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let mut proxy_auth_state = ProxyAuthState::new(shared_snapshot, pool.clone(), vec![])
        .with_master_key(master_key.clone());
    proxy_auth_state.is_ready.store(true, Ordering::SeqCst);
    proxy_auth_state.writer = Some(writer);
    proxy_auth_state.session_hmac_secret = Some(secret_bytes(vec![0x5A; 32]));
    proxy_auth_state.budget_semaphore = Arc::new(PreflightBudgetSemaphore::new(819, max_body_mb));
    data_plane_router(proxy_auth_state)
}

#[tokio::test]
#[doc = "Spec: SEC-8 - data-plane requests up to the configured maximum are read, and larger bodies are rejected before routing"]
async fn test_data_plane_body_limit_matches_configured_maximum() {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("body_limit.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xDD; 32]);
    seed_base(&pool, &master_key).await;

    let entropy = OsEntropy;
    let mut tx = pool.begin().await.expect("begin");
    let (_user_id, token) =
        seed_user_with_client_key(&mut tx, "body_limit_user", None, &entropy).await;
    tx.commit().await.expect("commit");

    let prefix = r#"{"model":"not-in-catalog","messages":[{"role":"user","content":""#;
    let suffix = r#""}]}"#;
    let padding = 2 * 1024 * 1024 - prefix.len() - suffix.len();
    let body = format!("{prefix}{}{suffix}", "x".repeat(padding));
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.clone()))
            .expect("request")
    };

    let app = build_wire_app_with_max_body(&pool, &master_key, 32).await;
    let response = app.oneshot(request()).await.expect("2 MiB response");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a 2 MiB body must pass the default 32 MiB data-plane limit and reach model resolution"
    );

    let app = build_wire_app_with_max_body(&pool, &master_key, 1).await;
    let response = app.oneshot(request()).await.expect("oversize response");
    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body above the configured maximum must be rejected by the data-plane limit"
    );
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
        "{}",
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
        "{}",
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

async fn seed_single_global_route(
    pool: &sqlx::SqlitePool,
    master_key: &MasterKey,
    mock_addr: SocketAddr,
    config_json: &str,
) -> String {
    let entropy = OsEntropy;
    seed_base(pool, master_key).await;
    let mut tx = pool.begin().await.expect("begin");
    let (_user_id, token) = seed_user_with_client_key(&mut tx, "f03_user", None, &entropy).await;
    seed_provider_route(
        &mut tx,
        master_key,
        &entropy,
        mock_addr,
        OwnershipScope::Global,
        None,
        config_json,
    )
    .await;
    tx.commit().await.expect("commit");
    token
}

/// Seeds one global provider, model and one active route per `(secret, priority)` pair.
async fn seed_global_route_with_secrets(
    pool: &sqlx::SqlitePool,
    master_key: &MasterKey,
    mock_addr: SocketAddr,
    secrets: &[(&str, i64)],
    config_json: &str,
) -> String {
    let entropy = OsEntropy;
    seed_base(pool, master_key).await;
    let mut tx = pool.begin().await.expect("begin");
    let (_user_id, token) =
        seed_user_with_client_key(&mut tx, "f03_err_user", None, &entropy).await;
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    let provider = create_provider_tx(
        &mut tx,
        &system_actor,
        NewProvider {
            name: format!(
                "Mock Error Provider {}",
                llmroller_core::ids::ProviderId::generate()
            ),
            kind: ProviderKind::Custom,
            base_url: format!("http://{}", mock_addr),
            config_json: config_json.to_string(),
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

    let model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) \
         VALUES (?, 'gpt-4o', 1, 'naive_rr')",
    )
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model");

    for (secret, priority) in secrets {
        let upstream_key = create_upstream_key_tx(
            &mut tx,
            &system_actor,
            master_key,
            "key-0",
            Arc::new(OsEntropy),
            NewUpstreamKey {
                provider_id: provider.id,
                label: format!("Mock Error Key {secret}"),
                secret: secret_string(*secret),
                priority: Some(*priority),
                strict_tier: Some(1),
                enabled: Some(true),
            },
            1000,
        )
        .await
        .expect("create upstream key");
        sqlx::query(
            "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, discovered_at) \
             VALUES (?, ?, 'gpt-4o', '[\"chat\",\"responses\",\"messages\"]', 'active', 1000)",
        )
        .bind(upstream_key.id.to_string())
        .bind(model_pk.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert route");
    }

    tx.commit().await.expect("commit");
    token
}

fn latest_request(mock_state: &MockUpstreamState, path: &str) -> RecordedRequest {
    mock_state
        .recorded
        .lock()
        .expect("recorded requests")
        .iter()
        .rev()
        .find(|request| request.path == path)
        .cloned()
        .expect("upstream request recorded")
}

fn streaming_request(uri: &str, token: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

#[tokio::test]
#[doc = "Spec: API-4, TRN-2 — F03 only Chat streaming may inject stream_options"]
async fn test_non_chat_streaming_does_not_inject_chat_stream_options() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_body.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE1; 32]);
    let token = seed_single_global_route(&pool, &master_key, mock_addr, "{}").await;
    let app = build_wire_app(&pool, &master_key).await;

    let messages = streaming_request(
        "/v1/messages",
        &token,
        r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
    );
    let messages_response = app
        .clone()
        .oneshot(messages)
        .await
        .expect("messages response");
    assert_eq!(messages_response.status(), StatusCode::OK);
    let messages_body = latest_request(&mock_state, "/messages").body;
    assert!(
        !messages_body.contains("stream_options"),
        "Anthropic /v1/messages must not receive stream_options: {messages_body}"
    );

    let responses = streaming_request(
        "/v1/responses",
        &token,
        r#"{"model":"gpt-4o","input":"hi","stream":true}"#,
    );
    let responses_response = app.oneshot(responses).await.expect("responses response");
    assert_eq!(responses_response.status(), StatusCode::OK);
    let responses_body = latest_request(&mock_state, "/responses").body;
    assert!(
        !responses_body.contains("stream_options"),
        "OpenAI /v1/responses must not receive chat stream_options: {responses_body}"
    );
}

#[tokio::test]
#[doc = "Spec: API-4 — F03 provider-native chat_stream mode suppresses injection"]
async fn test_provider_native_chat_stream_mode_preserves_body() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_native.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE2; 32]);
    let config = r#"{"usage_reporting":{"chat_stream":"native"}}"#;
    let token = seed_single_global_route(&pool, &master_key, mock_addr, config).await;
    let app = build_wire_app(&pool, &master_key).await;

    let request = streaming_request(
        "/v1/chat/completions",
        &token,
        r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
    );
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = latest_request(&mock_state, "/chat/completions").body;
    assert!(
        !body.contains("stream_options"),
        "provider-native usage reporting must not be mutated: {body}"
    );

    let _ = mock_state;
}

#[tokio::test]
#[doc = "Spec: API-2, API-3, PRV-3 — F03 honors provider header, UA, auth and response profiles"]
async fn test_provider_profile_applies_headers_ua_auth_and_response_allowlist() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_profile.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE3; 32]);
    let config = r#"{
        "add_headers": {"x-custom-provider": "1"},
        "strip_headers": ["x-client-secret"],
        "strip_header_tokens": ["beta-drop"],
        "ua_policy": "label",
        "response_header_allow": ["x-upstream-custom"],
        "auth": {"chat": {"header": "authorization", "prefix": "Bearer "}}
    }"#;
    let token = seed_single_global_route(&pool, &master_key, mock_addr, config).await;
    let app = build_wire_app(&pool, &master_key).await;

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-client-secret", "must-not-leak")
        .header("anthropic-beta", "keep, beta-drop")
        .header(header::USER_AGENT, "curl/8.5.0")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-upstream-custom")
            .map(|v| v.as_bytes()),
        Some(&b"yes"[..]),
        "response_header_allow must forward the configured upstream header"
    );

    let recorded = latest_request(&mock_state, "/chat/completions");
    assert_eq!(
        recorded
            .headers
            .get("x-custom-provider")
            .map(|v| v.as_bytes()),
        Some(&b"1"[..]),
        "add_headers must be applied to the upstream request"
    );
    assert!(
        !recorded.headers.contains_key("x-client-secret"),
        "strip_headers must remove configured deny-listed headers"
    );
    assert_eq!(
        recorded.headers.get("anthropic-beta").map(|v| v.as_bytes()),
        Some(&b"keep"[..]),
        "strip_header_tokens must remove only the configured tokens"
    );
    assert_eq!(
        recorded
            .headers
            .get(header::USER_AGENT)
            .map(|v| v.as_bytes()),
        Some(&b"llmroller/1.0"[..]),
        "ua_policy=label must replace generic library User-Agent values"
    );
    assert_eq!(
        recorded
            .headers
            .get(header::AUTHORIZATION)
            .map(|v| v.as_bytes()),
        Some(&b"Bearer sk-mock-key"[..]),
        "the configured auth scheme must receive the decrypted upstream secret"
    );
}

#[tokio::test]
#[doc = "Spec: API-2, PRV-3 — F03 supports per-protocol x-api-key authentication"]
async fn test_messages_auth_scheme_uses_configured_x_api_key() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_messages_auth.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE4; 32]);
    let config = r#"{"auth":{"messages":{"header":"x-api-key","prefix":""}}}"#;
    let token = seed_single_global_route(&pool, &master_key, mock_addr, config).await;
    let app = build_wire_app(&pool, &master_key).await;

    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = latest_request(&mock_state, "/messages");
    assert_eq!(
        recorded.headers.get("x-api-key").map(|v| v.as_bytes()),
        Some(&b"sk-mock-key"[..]),
        "the Messages protocol must use its configured auth header"
    );
    assert!(
        !recorded.headers.contains_key(header::AUTHORIZATION),
        "client credentials must not leak through the Authorization header"
    );
    assert_eq!(
        recorded
            .headers
            .get("anthropic-version")
            .map(|v| v.as_bytes()),
        Some(&b"2023-06-01"[..]),
        "native Anthropic Messages requests require anthropic-version"
    );
}

fn chat_request_with_header(token: &str, name: &'static str, value: &'static str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(name, value)
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("request")
}

#[tokio::test]
#[doc = "Spec: API-8 — F03 moderation 403 is returned verbatim without failover"]
async fn test_moderation_403_does_not_fail_over_or_forward_as_credential_error() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_moderation.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE5; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-moderate", 1), ("sk-ok", 2)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let response = app
        .oneshot(chat_request_with_header(&token, "x-fail-moderation", "1"))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("moderation"),
        "the upstream moderation body must be forwarded verbatim: {text}"
    );
    assert_eq!(
        mock_state.chat_calls.load(Ordering::SeqCst),
        1,
        "a RequestRejected 403 must never fail over to another key"
    );
}

#[tokio::test]
#[doc = "Spec: API-8 — F03 moderation 403 leaves key health untouched"]
async fn test_moderation_403_does_not_quarantine_key() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(
        &dir.path().join("f03_moderation_health.db"),
        PoolRole::Writer,
    )
    .await
    .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE6; 32]);
    let token =
        seed_global_route_with_secrets(&pool, &master_key, mock_addr, &[("sk-moderate", 1)], "{}")
            .await;
    let app = build_wire_app(&pool, &master_key).await;

    let rejected = app
        .clone()
        .oneshot(chat_request_with_header(&token, "x-fail-moderation", "1"))
        .await
        .expect("response");
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let recovered = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(
        recovered.status(),
        StatusCode::OK,
        "a content-moderation 403 must not quarantine the upstream key"
    );
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[doc = "Spec: API-8 — F03 PlanRestricted blocks only that key/model/protocol route"]
async fn test_plan_restricted_blocks_route_until_next_discovery() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_plan.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE7; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-plan", 1), ("sk-ok", 2)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let first = app
        .clone()
        .oneshot(chat_request(&token))
        .await
        .expect("response");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        mock_state.chat_calls.load(Ordering::SeqCst),
        2,
        "PlanRestricted must fail over to the next candidate"
    );

    let second = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        mock_state.chat_calls.load(Ordering::SeqCst),
        3,
        "the blocked (key, model, protocol) route must be skipped after PlanRestricted"
    );
}

#[tokio::test]
#[doc = "Spec: API-8 — F03 terminal selector is used for mixed failures"]
async fn test_mixed_quota_and_server_failure_returns_terminal_502() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_terminal_mixed.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE8; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-quota", 1), ("sk-503", 2)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let response = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"], "upstream_unreachable");
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 2);
}

fn chat_request_with_body(token: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn recorded_authorizations(state: &MockUpstreamState) -> Vec<String> {
    state
        .recorded
        .lock()
        .expect("recorded requests")
        .iter()
        .filter_map(|request| {
            request
                .headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect()
}

#[tokio::test]
#[doc = "Spec: RTE-1 - F06 Step 7 body session fields pin the same upstream key"]
async fn test_body_session_fields_pin_across_distinct_openings() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f06_body_fields.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF1; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-session-a", 100), ("sk-session-b", 100)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let prompt_cache_key_bodies = [
        r#"{"model":"gpt-4o","prompt_cache_key":"cache-key-1","messages":[{"role":"system","content":"system one"},{"role":"user","content":"question one"}]}"#,
        r#"{"model":"gpt-4o","prompt_cache_key":"cache-key-1","messages":[{"role":"system","content":"system two"},{"role":"user","content":"question two"}]}"#,
    ];
    for body in prompt_cache_key_bodies {
        let response = app
            .clone()
            .oneshot(chat_request_with_body(&token, body))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let metadata_session_bodies = [
        r#"{"model":"gpt-4o","metadata":{"session_id":"meta-session-1"},"messages":[{"role":"system","content":"meta system one"},{"role":"user","content":"meta question one"}]}"#,
        r#"{"model":"gpt-4o","metadata":{"session_id":"meta-session-1"},"messages":[{"role":"system","content":"meta system two"},{"role":"user","content":"meta question two"}]}"#,
    ];
    for body in metadata_session_bodies {
        let response = app
            .clone()
            .oneshot(chat_request_with_body(&token, body))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let authorizations = recorded_authorizations(&mock_state);
    assert_eq!(authorizations.len(), 4);
    assert_eq!(
        authorizations[0], authorizations[1],
        "prompt_cache_key must pin distinct openings to one upstream key"
    );
    assert_eq!(
        authorizations[2], authorizations[3],
        "metadata.session_id must pin distinct openings to one upstream key"
    );
}

#[tokio::test]
#[doc = "Spec: RTE-1 - F06 Step 8 fingerprint pins matching opening prompts"]
async fn test_step8_fingerprint_pins_matching_openings() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f06_step8.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF2; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-step8-a", 100), ("sk-step8-b", 100)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let first_turn = r#"{"model":"gpt-4o","messages":[{"role":"system","content":"stable system prompt"},{"role":"user","content":"first question"}]}"#;
    let second_turn = r#"{"model":"gpt-4o","messages":[{"role":"system","content":"stable system prompt"},{"role":"user","content":"first question"},{"role":"assistant","content":"answer"},{"role":"user","content":"follow-up question"}]}"#;

    for body in [first_turn, second_turn] {
        let response = app
            .clone()
            .oneshot(chat_request_with_body(&token, body))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let authorizations = recorded_authorizations(&mock_state);
    assert_eq!(authorizations.len(), 2);
    assert_eq!(
        authorizations[0], authorizations[1],
        "matching SYS and USER1 must produce one Step 8 pin"
    );
}

#[tokio::test]
#[doc = "Spec: RTE-1 - F06 fingerprint_generic_ids replaces generic IDs with Step 8"]
async fn test_constant_session_mode_fingerprint_generic_ids_uses_step8() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f06_constant_mode.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF3; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-mode-a", 100), ("sk-mode-b", 100)],
        "{}",
    )
    .await;
    sqlx::query("UPDATE client_keys SET constant_session_mode = 'fingerprint_generic_ids'")
        .execute(&pool)
        .await
        .expect("set constant session mode");
    let app = build_wire_app(&pool, &master_key).await;

    let first_opening = r#"{"model":"gpt-4o","session_id":"static-generic-id","messages":[{"role":"system","content":"opening one"},{"role":"user","content":"question one"}]}"#;
    let second_opening = r#"{"model":"gpt-4o","session_id":"static-generic-id","messages":[{"role":"system","content":"opening two"},{"role":"user","content":"question two"}]}"#;

    for body in [first_opening, second_opening, first_opening] {
        let response = app
            .clone()
            .oneshot(chat_request_with_body(&token, body))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let authorizations = recorded_authorizations(&mock_state);
    assert_eq!(authorizations.len(), 3);
    assert_ne!(
        authorizations[0], authorizations[1],
        "fingerprint_generic_ids must ignore the static body session ID"
    );
    assert_eq!(
        authorizations[0], authorizations[2],
        "the same opening must return to its Step 8 pin"
    );
}

#[tokio::test]
#[doc = "Spec: API-8 - F03 all-quota failures select 429 with Retry-After"]
async fn test_all_quota_failures_return_429_with_retry_after() {
    let (mock_addr, _mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_terminal_quota.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE9; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-quota", 1), ("sk-quota-2", 2)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let response = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .map(|value| value.to_str().expect("retry-after")),
        Some("7")
    );
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"], "upstream_rate_limited");
}
