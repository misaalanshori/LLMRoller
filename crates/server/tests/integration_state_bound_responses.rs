//! State-bound Responses forwarding, previous_response_id HardKey affinity, and
//! auxiliary count_tokens forwarding regressions.
//!
//! Spec: Section 10.4.1, Section 10.5, Section 11.1.2
//! Requirements: API-1, RTE-12

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_core::ids::{ModelPk, ProviderId, UpstreamKeyId, UserId};
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
use llmroller_store::writer::{ReloadNotification, WriterHandle, flush_data_queues, start_writer};

const UPSTREAM_RESPONSE: &str = r#"{
    "id": "resp_up_1",
    "object": "response",
    "status": "completed",
    "output": [{
        "type": "message",
        "content": [{"type": "output_text", "text": "Real upstream output"}]
    }]
}"#;

#[derive(Clone, Default)]
struct RecordedRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    api_key: Option<String>,
    anthropic_version: Option<String>,
    headers: axum::http::HeaderMap,
    body: String,
}

#[derive(Clone)]
struct MockState {
    count_tokens_unsupported: bool,
    calls: Arc<Mutex<Vec<RecordedRequest>>>,
    create_calls: Arc<AtomicUsize>,
}

impl MockState {
    fn new(count_tokens_unsupported: bool) -> Self {
        Self {
            count_tokens_unsupported,
            calls: Arc::new(Mutex::new(Vec::new())),
            create_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

async fn record(state: &MockState, request: &mut Request<Body>) -> String {
    let method = request.method().as_str().to_string();
    let path = request.uri().path_and_query().map_or_else(
        || request.uri().path().to_string(),
        |pq| pq.as_str().to_string(),
    );
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let api_key = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let anthropic_version = request
        .headers()
        .get("anthropic-version")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let headers = request.headers().clone();
    let body = std::mem::replace(request.body_mut(), Body::empty());
    let bytes = to_bytes(body, usize::MAX).await.expect("mock body");
    let body = String::from_utf8_lossy(&bytes).to_string();
    state
        .calls
        .lock()
        .expect("mock calls")
        .push(RecordedRequest {
            method,
            path,
            authorization,
            api_key,
            anthropic_version,
            headers,
            body: body.clone(),
        });
    body
}

async fn mock_create(State(state): State<MockState>, mut request: Request<Body>) -> Response {
    state.create_calls.fetch_add(1, Ordering::SeqCst);
    let _ = record(&state, &mut request).await;
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        UPSTREAM_RESPONSE,
    )
        .into_response()
}

async fn mock_get(State(state): State<MockState>, mut request: Request<Body>) -> Response {
    let body = record(&state, &mut request).await;
    if request.uri().path().ends_with("/input_items") {
        return (
            StatusCode::OK,
            [("content-type", "application/json")],
            r#"{"object":"list","data":[{"id":"msg_up_1","type":"message"}]}"#,
        )
            .into_response();
    }
    let _ = body;
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        UPSTREAM_RESPONSE,
    )
        .into_response()
}

async fn mock_cancel(State(state): State<MockState>, mut request: Request<Body>) -> Response {
    let _ = record(&state, &mut request).await;
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"id":"resp_up_1","object":"response","status":"cancelled"}"#,
    )
        .into_response()
}

async fn mock_delete(State(state): State<MockState>, mut request: Request<Body>) -> Response {
    let _ = record(&state, &mut request).await;
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"id":"resp_up_1","object":"response","deleted":true}"#,
    )
        .into_response()
}

async fn mock_count_tokens(State(state): State<MockState>, mut request: Request<Body>) -> Response {
    let _ = record(&state, &mut request).await;
    if state.count_tokens_unsupported {
        return (
            StatusCode::NOT_FOUND,
            [("content-type", "application/json")],
            r#"{"error":{"code":"not_found"}}"#,
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"input_tokens":42}"#,
    )
        .into_response()
}

async fn spawn_mock(unsupported: bool) -> (SocketAddr, MockState) {
    let state = MockState::new(unsupported);
    let app = Router::new()
        .route("/responses", post(mock_create))
        .route("/responses/{id}", get(mock_get).delete(mock_delete))
        .route("/responses/{id}/input_items", get(mock_get))
        .route("/responses/{id}/cancel", post(mock_cancel))
        .route("/messages/count_tokens", post(mock_count_tokens))
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
    writer: WriterHandle,
    _reload_rx: tokio::sync::mpsc::Receiver<ReloadNotification>,
}

async fn harness() -> Harness {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("state_bound.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xBC; 32]);
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&pool, None, 1).await.expect("seed settings");
    let (reload_tx, reload_rx) = tokio::sync::mpsc::channel(8);
    let (writer, _join) = start_writer(pool.clone(), reload_tx);
    Harness {
        _dir: dir,
        pool,
        master_key,
        system_actor: ActorScope::system(SystemTask::Writer, SystemSeal::store()),
        writer,
        _reload_rx: reload_rx,
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

async fn create_client_key(harness: &Harness, user_id: UserId, label: &str) -> String {
    let mut tx = harness.pool.begin().await.expect("begin");
    let created = create_client_key_tx(
        &mut tx,
        &harness.system_actor,
        user_id,
        NewClientKey {
            label: label.to_string(),
            service_account_id: None,
            allowed_models: None,
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
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

async fn create_provider(harness: &Harness, name: &str, base_url: String) -> ProviderId {
    let mut tx = harness.pool.begin().await.expect("begin");
    let provider = create_provider_tx(
        &mut tx,
        &harness.system_actor,
        NewProvider {
            name: name.to_string(),
            kind: ProviderKind::Custom,
            base_url,
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
    tx.commit().await.expect("commit");
    provider.id
}

async fn create_upstream_key(
    harness: &Harness,
    provider_id: ProviderId,
    label: &str,
    priority: i64,
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
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");
    tx.commit().await.expect("commit");
    key.id
}

async fn insert_model(harness: &Harness, canonical_id: &str) -> ModelPk {
    let model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) VALUES (?, ?, 1, 'smooth_weighted_rr')",
    )
    .bind(model_pk.to_string())
    .bind(canonical_id)
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
    protocols: &str,
) {
    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, ?, ?, 'active', 1, 1000)",
    )
    .bind(key_id.to_string())
    .bind(model_pk.to_string())
    .bind(upstream_model_id)
    .bind(protocols)
    .execute(&harness.pool)
    .await
    .expect("insert model route");
}

async fn build_app(harness: &Harness) -> Router {
    let snapshot = load_initial_snapshot(&harness.pool)
        .await
        .expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let state = ProxyAuthState::new(shared_snapshot, harness.pool.clone(), vec![])
        .with_master_key(harness.master_key.clone())
        .with_writer(harness.writer.clone());
    state.is_ready.store(true, Ordering::SeqCst);
    data_plane_router(state)
}

/// Build the router with a client-key TPM limit applied to every key.
///
/// `rate_limit_tpm` has no `client_keys` column; it lives only on the snapshot's
/// `ClientKeyRecord`, so the limit is injected by rebuilding the snapshot
/// (`ISSUE-0145`).
async fn build_app_with_tpm(harness: &Harness, tpm: i64) -> Router {
    let mut snapshot = load_initial_snapshot(&harness.pool)
        .await
        .expect("load snapshot");
    let mut keys_map = (*snapshot.client_keys_by_hash).clone();
    for (record, _) in keys_map.values_mut() {
        record.rate_limit_tpm = Some(tpm);
    }
    snapshot.client_keys_by_hash = Arc::new(keys_map);

    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let state = ProxyAuthState::new(shared_snapshot, harness.pool.clone(), vec![])
        .with_master_key(harness.master_key.clone())
        .with_writer(harness.writer.clone());
    state.is_ready.store(true, Ordering::SeqCst);
    data_plane_router(state)
}

async fn flush(harness: &Harness) {
    flush_data_queues(&harness.pool, &harness.writer.queues).await;
}

fn request(method: &str, uri: &str, token: &str, body: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("request")
}

async fn response_json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body");
    serde_json::from_slice(&bytes).expect("json body")
}

async fn create_link(harness: &Harness, token: &str) -> (serde_json::Value, MockState) {
    let (addr, mock) = spawn_mock(false).await;
    let provider = create_provider(harness, "bound", format!("http://{addr}")).await;
    let key = create_upstream_key(harness, provider, "bound", 100).await;
    let model = insert_model(harness, "gpt-4o").await;
    insert_route(
        harness,
        key,
        model,
        "gpt-4o",
        r#"["chat","responses","messages"]"#,
    )
    .await;

    let app = build_app(harness).await;
    let response = app
        .oneshot(request(
            "POST",
            "/v1/responses",
            token,
            Some(r#"{"model":"gpt-4o","input":"hello"}"#),
        ))
        .await
        .expect("create response");
    assert_eq!(response.status(), StatusCode::OK);
    let json = response_json(response).await;
    flush(harness).await;
    (json, mock)
}

#[tokio::test]
async fn test_state_bound_get_forwards_to_bound_upstream_key_verbatim() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "state-bound").await;
    let token = create_client_key(&harness, user, "bound-key").await;
    let (_created, mock) = create_link(&harness, &token).await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(request(
            "GET",
            "/v1/responses/resp_up_1?include=step_details",
            &token,
            None,
        ))
        .await
        .expect("get response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().contains_key("x-llmroller-request-id"),
        "response header pipeline must insert x-llmroller-request-id"
    );
    let json = response_json(response).await;
    assert_eq!(
        json["output"][0]["content"][0]["text"], "Real upstream output",
        "the upstream payload must be returned verbatim"
    );

    {
        let calls = mock.calls.lock().expect("calls");
        let get_call = calls
            .iter()
            .find(|call| {
                call.method == "GET" && call.path == "/responses/resp_up_1?include=step_details"
            })
            .expect("upstream GET with query string");
        assert_eq!(
            get_call.authorization.as_deref(),
            Some("Bearer sk-bound"),
            "the state-bound call must use the exact bound upstream key"
        );
    }

    flush(&harness).await;
    let events_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_events WHERE endpoint = 'responses'")
            .fetch_one(&harness.pool)
            .await
            .expect("count usage events");
    assert!(
        events_count >= 1,
        "state-bound response operation must emit a usage_events row"
    );
}

// Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-request-and-response-header-policies | Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer
#[tokio::test]
#[doc = "Spec: TRN-0, API-3 - state-bound GET/DELETE forward the client's headers and add no proxy marker"]
async fn test_state_bound_calls_forward_client_headers_without_proxy_markers() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "state-bound-headers").await;
    let token = create_client_key(&harness, user, "bound-headers-key").await;
    let (_created, mock) = create_link(&harness, &token).await;
    let app = build_app(&harness).await;

    let request = Request::builder()
        .method("GET")
        .uri("/v1/responses/resp_up_1")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::USER_AGENT, "opencode/1.2.15")
        .header(header::ACCEPT_ENCODING, "gzip, br")
        .header("x-future-agent-header", "kept")
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("get response");
    assert_eq!(response.status(), StatusCode::OK);

    let calls = mock.calls.lock().expect("calls");
    let get_call = calls
        .iter()
        .find(|call| call.method == "GET" && call.path == "/responses/resp_up_1")
        .expect("upstream GET");
    let headers = &get_call.headers;
    assert_eq!(
        headers.get(header::USER_AGENT).map(|v| v.as_bytes()),
        Some(&b"opencode/1.2.15"[..])
    );
    assert_eq!(
        headers.get(header::ACCEPT_ENCODING).map(|v| v.as_bytes()),
        Some(&b"gzip, br"[..])
    );
    assert_eq!(
        headers.get("x-future-agent-header").map(|v| v.as_bytes()),
        Some(&b"kept"[..])
    );
    assert!(
        !headers.contains_key("x-request-id"),
        "LLMRoller must not add its own request id upstream"
    );
    for (name, value) in headers {
        let line = format!("{name}: {}", value.to_str().unwrap_or_default()).to_ascii_lowercase();
        assert!(
            !line.contains("llmroller"),
            "proxy marker leaked upstream: {line}"
        );
    }
}

#[tokio::test]
async fn test_state_bound_input_items_and_cancel_use_upstream_and_mark_incomplete() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "state-bound-items").await;
    let token = create_client_key(&harness, user, "items-key").await;
    let (_created, mock) = create_link(&harness, &token).await;

    let app = build_app(&harness).await;
    let items = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/responses/resp_up_1/input_items",
            &token,
            None,
        ))
        .await
        .expect("input items");
    assert_eq!(items.status(), StatusCode::OK);
    let items_json = response_json(items).await;
    assert_eq!(items_json["data"][0]["id"], "msg_up_1");

    let cancel = app
        .oneshot(request(
            "POST",
            "/v1/responses/resp_up_1/cancel",
            &token,
            None,
        ))
        .await
        .expect("cancel");
    assert_eq!(cancel.status(), StatusCode::OK);
    // State-bound mutations are write-behind through the durable queue (`REL-2`).
    flush(&harness).await;

    let status: String =
        sqlx::query_scalar("SELECT status FROM response_links WHERE response_id = 'resp_up_1'")
            .fetch_one(&harness.pool)
            .await
            .expect("response link status");
    assert_eq!(status, "incomplete");

    let calls = mock.calls.lock().expect("calls");
    assert!(
        calls
            .iter()
            .any(|call| call.method == "GET" && call.path == "/responses/resp_up_1/input_items")
    );
    assert!(
        calls
            .iter()
            .any(|call| call.method == "POST" && call.path == "/responses/resp_up_1/cancel")
    );
}

#[tokio::test]
async fn test_state_bound_delete_forwards_and_removes_link() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "state-bound-delete").await;
    let token = create_client_key(&harness, user, "delete-key").await;
    let (_created, mock) = create_link(&harness, &token).await;

    let app = build_app(&harness).await;
    let response = app
        .oneshot(request("DELETE", "/v1/responses/resp_up_1", &token, None))
        .await
        .expect("delete response");
    assert_eq!(response.status(), StatusCode::OK);
    let json = response_json(response).await;
    assert_eq!(json["deleted"], true);
    // The delete is queued in `durable_state` and committed by the writer actor.
    flush(&harness).await;

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM response_links WHERE response_id = 'resp_up_1'")
            .fetch_one(&harness.pool)
            .await
            .expect("count links");
    assert_eq!(count, 0);
    assert!(
        mock.calls
            .lock()
            .expect("calls")
            .iter()
            .any(|call| call.method == "DELETE" && call.path == "/responses/resp_up_1")
    );
}

#[tokio::test]
async fn test_state_bound_cross_tenant_and_unknown_ids_return_local_404() {
    let harness = harness().await;
    let owner = insert_user(&harness.pool, "owner").await;
    let stranger = insert_user(&harness.pool, "stranger").await;
    let owner_token = create_client_key(&harness, owner, "owner-key").await;
    let stranger_token = create_client_key(&harness, stranger, "stranger-key").await;
    let (_created, mock) = create_link(&harness, &owner_token).await;

    let app = build_app(&harness).await;
    let cross = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/responses/resp_up_1",
            &stranger_token,
            None,
        ))
        .await
        .expect("cross tenant");
    assert_eq!(cross.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(cross).await["error"]["code"],
        "response_not_found"
    );

    let unknown = app
        .oneshot(request(
            "GET",
            "/v1/responses/does-not-exist",
            &owner_token,
            None,
        ))
        .await
        .expect("unknown");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(unknown).await["error"]["code"],
        "response_not_found"
    );

    let calls = mock.calls.lock().expect("calls");
    assert!(
        !calls.iter().any(|call| call.method == "GET"),
        "local 404 paths must never reach upstream"
    );
}

#[tokio::test]
async fn test_state_bound_disabled_key_returns_permission_error_without_upstream() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "disabled-bound").await;
    let token = create_client_key(&harness, user, "disabled-key").await;
    let (_created, mock) = create_link(&harness, &token).await;

    sqlx::query("UPDATE upstream_keys SET enabled = 0")
        .execute(&harness.pool)
        .await
        .expect("disable key");
    let app = build_app(&harness).await;

    let response = app
        .oneshot(request("GET", "/v1/responses/resp_up_1", &token, None))
        .await
        .expect("disabled get");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let json = response_json(response).await;
    assert_eq!(json["error"]["code"], "permission_error");
    assert!(
        !mock
            .calls
            .lock()
            .expect("calls")
            .iter()
            .any(|call| call.method == "GET")
    );
}

#[tokio::test]
async fn test_previous_response_id_routes_to_linked_key_without_failover() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "affinity").await;
    let token = create_client_key(&harness, user, "affinity-key").await;

    let (bound_addr, bound_mock) = spawn_mock(false).await;
    let (backup_addr, backup_mock) = spawn_mock(false).await;
    let bound_provider =
        create_provider(&harness, "affinity-bound", format!("http://{bound_addr}")).await;
    let bound_key = create_upstream_key(&harness, bound_provider, "bound", 100).await;
    let backup_provider =
        create_provider(&harness, "affinity-backup", format!("http://{backup_addr}")).await;
    let backup_key = create_upstream_key(&harness, backup_provider, "backup", 1).await;
    let model = insert_model(&harness, "gpt-4o").await;
    insert_route(
        &harness,
        bound_key,
        model,
        "gpt-4o",
        r#"["chat","responses","messages"]"#,
    )
    .await;
    insert_route(
        &harness,
        backup_key,
        model,
        "gpt-4o",
        r#"["chat","responses","messages"]"#,
    )
    .await;

    let app = build_app(&harness).await;
    let first = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/responses",
            &token,
            Some(r#"{"model":"gpt-4o","input":"first"}"#),
        ))
        .await
        .expect("first turn");
    assert_eq!(first.status(), StatusCode::OK);
    let _ = to_bytes(first.into_body(), usize::MAX)
        .await
        .expect("first body");
    flush(&harness).await;
    assert_eq!(bound_mock.create_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backup_mock.create_calls.load(Ordering::SeqCst), 0);

    let second = app
        .oneshot(request(
            "POST",
            "/v1/responses",
            &token,
            Some(r#"{"model":"gpt-4o","previous_response_id":"resp_up_1","input":"second"}"#),
        ))
        .await
        .expect("second turn");
    assert_eq!(second.status(), StatusCode::OK);
    let _ = to_bytes(second.into_body(), usize::MAX)
        .await
        .expect("second body");

    assert_eq!(
        bound_mock.create_calls.load(Ordering::SeqCst),
        2,
        "previous_response_id must stay on the linked key"
    );
    assert_eq!(
        backup_mock.create_calls.load(Ordering::SeqCst),
        0,
        "HardKey affinity disables failover to other keys"
    );
    let calls = bound_mock.calls.lock().expect("calls");
    let last = calls.last().expect("last create");
    assert!(
        last.body.contains("\"previous_response_id\":\"resp_up_1\""),
        "the client payload must be forwarded unchanged"
    );
}

#[tokio::test]
async fn test_count_tokens_forwards_native_anthropic_request() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "counter").await;
    let token = create_client_key(&harness, user, "counter-key").await;
    let (addr, mock) = spawn_mock(false).await;
    let provider = create_provider(&harness, "anthropic", format!("http://{addr}")).await;
    let key = create_upstream_key(&harness, provider, "anthropic", 100).await;
    let model = insert_model(&harness, "claude-sonnet").await;
    insert_route(&harness, key, model, "claude-native", r#"["messages"]"#).await;

    let app = build_app(&harness).await;
    let response = app
        .oneshot(request(
            "POST",
            "/v1/messages/count_tokens",
            &token,
            Some(r#"{"model":"claude-sonnet","messages":[{"role":"user","content":"hi"}]}"#),
        ))
        .await
        .expect("count tokens");
    assert_eq!(response.status(), StatusCode::OK);
    let json = response_json(response).await;
    assert_eq!(json["input_tokens"], 42);

    let calls = mock.calls.lock().expect("calls");
    let call = calls
        .iter()
        .find(|call| call.path == "/messages/count_tokens")
        .expect("upstream count_tokens");
    assert_eq!(call.method, "POST");
    assert_eq!(call.api_key.as_deref(), Some("sk-anthropic"));
    assert_eq!(call.anthropic_version.as_deref(), Some("2023-06-01"));
    assert!(
        call.body.contains("claude-native"),
        "the upstream model id must be rewritten from the catalog route"
    );
}

#[tokio::test]
async fn test_count_tokens_unsupported_upstream_returns_local_not_implemented_and_caches() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "counter-unsupported").await;
    let token = create_client_key(&harness, user, "counter-unsupported-key").await;
    let (addr, mock) = spawn_mock(true).await;
    let provider = create_provider(&harness, "anthropic-404", format!("http://{addr}")).await;
    let key = create_upstream_key(&harness, provider, "anthropic-404", 100).await;
    let model = insert_model(&harness, "claude-sonnet").await;
    insert_route(&harness, key, model, "claude-native", r#"["messages"]"#).await;

    let app = build_app(&harness).await;
    let body = r#"{"model":"claude-sonnet","messages":[{"role":"user","content":"hi"}]}"#;
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/messages/count_tokens",
            &token,
            Some(body),
        ))
        .await
        .expect("count tokens");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "not_found_error"
    );

    // The negative probe is cached: a second request must not reach upstream.
    let response = app
        .oneshot(request(
            "POST",
            "/v1/messages/count_tokens",
            &token,
            Some(body),
        ))
        .await
        .expect("count tokens");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        mock.calls
            .lock()
            .expect("calls")
            .iter()
            .filter(|call| call.path == "/messages/count_tokens")
            .count(),
        1,
        "a cached unsupported probe must short-circuit before dispatch"
    );
}

// ---------------------------------------------------------------------------
// `count_tokens` must pass the same pre-candidate gates as inference
// (`API-1`, `SEC-8`, `F040`, `ISSUE-0145`)
// ---------------------------------------------------------------------------
//
// Before the fix `post_messages_count_tokens` was a hand-rolled miniature of the
// inference path: it never touched the preflight budget, never decompressed, and
// never consumed client-key TPM. Each test here pins one of those gates and asserts
// the upstream received **zero** calls, because a status-code-only assertion could
// pass for an unrelated reason.

fn gzip_json(payload: &str) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write as _;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(payload.as_bytes()).expect("compress");
    encoder.finish().expect("finish")
}

fn gzip_request(token: &str, body: &[u8], content_type: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_ENCODING, "gzip")
        .body(Body::from(body.to_vec()))
        .expect("request")
}

/// A saturated preflight budget must reject `count_tokens` before it reaches upstream.
#[tokio::test]
#[doc = "Spec: API-1, SEC-8 — ISSUE-0145 count_tokens respects the preflight budget"]
async fn count_tokens_respects_the_preflight_budget() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "budget-counter").await;
    let token = create_client_key(&harness, user, "budget-counter-key").await;
    let (addr, mock) = spawn_mock(false).await;
    let provider = create_provider(&harness, "anthropic", format!("http://{addr}")).await;
    let key = create_upstream_key(&harness, provider, "anthropic", 100).await;
    let model = insert_model(&harness, "claude-sonnet").await;
    insert_route(&harness, key, model, "claude-native", r#"["messages"]"#).await;

    // A one-megabyte budget whose only permit is already held.
    let snapshot = load_initial_snapshot(&harness.pool)
        .await
        .expect("load snapshot");
    let shared = Arc::new(ArcSwap::from_pointee(snapshot));
    let mut state = ProxyAuthState::new(shared, harness.pool.clone(), vec![])
        .with_master_key(harness.master_key.clone())
        .with_writer(harness.writer.clone());
    let budget = Arc::new(llmroller_proxy::orchestrator::PreflightBudgetSemaphore::new(1, 32));
    state.budget_semaphore = Arc::clone(&budget);
    state.is_ready.store(true, Ordering::SeqCst);
    let app = data_plane_router(state);

    // Hold every permit so count_tokens cannot get one.
    let held = budget
        .acquire(1024 * 1024, std::time::Duration::from_millis(500))
        .await
        .expect("hold the entire budget");

    let response = app
        .oneshot(request(
            "POST",
            "/v1/messages/count_tokens",
            &token,
            Some(r#"{"model":"claude-sonnet","messages":[{"role":"user","content":"hi"}]}"#),
        ))
        .await
        .expect("count tokens");

    assert!(
        response.status().is_client_error() || response.status().is_server_error(),
        "count_tokens must be refused when the preflight budget is saturated; got {}",
        response.status()
    );
    let calls = mock.calls.lock().expect("calls");
    assert!(
        calls.is_empty(),
        "a budget-refused count_tokens must never reach upstream; upstream saw {} call(s)",
        calls.len()
    );
    drop(calls);
    drop(held);
}

/// A gzip-compressed count_tokens body must be decompressed before forwarding.
///
/// Pre-fix the raw gzip bytes were forwarded, so the upstream mock saw binary
/// garbage — an interop bug independent of the security framing.
#[tokio::test]
#[doc = "Spec: API-1, SEC-8 — ISSUE-0145 count_tokens decompresses a compressed body"]
async fn count_tokens_decompresses_a_compressed_body() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "gzip-counter").await;
    let token = create_client_key(&harness, user, "gzip-counter-key").await;
    let (addr, mock) = spawn_mock(false).await;
    let provider = create_provider(&harness, "anthropic", format!("http://{addr}")).await;
    let key = create_upstream_key(&harness, provider, "anthropic", 100).await;
    let model = insert_model(&harness, "claude-sonnet").await;
    insert_route(&harness, key, model, "claude-native", r#"["messages"]"#).await;

    let app = build_app(&harness).await;
    // Repetitive content so gzip genuinely shrinks it; a short unique string gzips
    // *larger* than its input and the assertion below would be meaningless.
    let filler = "compress me ".repeat(200);
    let payload = format!(
        r#"{{"model":"claude-sonnet","messages":[{{"role":"user","content":"{filler}"}}]}}"#
    );
    let compressed = gzip_json(&payload);
    assert!(
        compressed.len() < payload.len(),
        "the fixture must actually be compressed for this test to mean anything: \
         {} compressed vs {} raw",
        compressed.len(),
        payload.len()
    );

    let response = app
        .oneshot(gzip_request(&token, &compressed, "application/json"))
        .await
        .expect("count tokens");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a valid gzipped count_tokens request must succeed"
    );

    let calls = mock.calls.lock().expect("calls");
    let call = calls
        .iter()
        .find(|call| call.path == "/messages/count_tokens")
        .expect("upstream count_tokens");
    assert!(
        call.body.contains("compress me"),
        "the upstream must receive the decompressed JSON; it saw {} bytes starting {:?}",
        call.body.len(),
        call.body.chars().take(24).collect::<String>()
    );
    assert!(
        call.body.trim_start().starts_with('{'),
        "the forwarded body must be JSON, not gzip bytes"
    );
}

/// An exhausted client-key TPM budget must reject `count_tokens`.
#[tokio::test]
#[doc = "Spec: F040 — ISSUE-0145 count_tokens is rejected when client-key TPM is exhausted"]
async fn count_tokens_is_rejected_when_the_client_key_tpm_is_exhausted() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "tpm-counter").await;
    let token = create_client_key(&harness, user, "tpm-counter-key").await;
    let (addr, mock) = spawn_mock(false).await;
    let provider = create_provider(&harness, "anthropic", format!("http://{addr}")).await;
    let key = create_upstream_key(&harness, provider, "anthropic", 100).await;
    let model = insert_model(&harness, "claude-sonnet").await;
    insert_route(&harness, key, model, "claude-native", r#"["messages"]"#).await;

    // `rate_limit_tpm` is not a client_keys column — it is carried only on the
    // snapshot's `ClientKeyRecord`, so the limit is set by rebuilding the snapshot,
    // which is the same pattern `integration_client_keys.rs` uses.
    //
    // The limiter only rejects once something has already been consumed in this
    // minute (`entry.1 > 0 && entry.1 + add > limit`), so a 1-TPM key needs a first
    // request to prime the window and a second to be refused.
    let app = build_app_with_tpm(&harness, 1).await;
    let body =
        r#"{"model":"claude-sonnet","messages":[{"role":"user","content":"count me please"}]}"#;
    let first = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/messages/count_tokens",
            &token,
            Some(body),
        ))
        .await
        .expect("first count tokens");
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "the first request of the minute must be admitted"
    );

    let second = app
        .oneshot(request(
            "POST",
            "/v1/messages/count_tokens",
            &token,
            Some(body),
        ))
        .await
        .expect("second count tokens");
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "an exhausted client-key TPM must reject count_tokens with 429"
    );

    let calls = mock.calls.lock().expect("calls");
    let upstream_calls = calls
        .iter()
        .filter(|call| call.path == "/messages/count_tokens")
        .count();
    assert_eq!(
        upstream_calls, 1,
        "the TPM-rejected request must never reach upstream; it saw {upstream_calls} call(s)"
    );
}

/// A successful count_tokens must not reserve budget windows or create pins.
///
/// This protects the auxiliary-endpoint design: C8 is skipped for `CountTokens`
/// (SRDD 10.1), so the handler must share the *gates* without inheriting the
/// reservations and pin machinery. It is the guard against a future
/// "just call `admit_inbound_request`" simplification.
#[tokio::test]
#[doc = "Spec: API-1, SRDD 10.1 C8 — ISSUE-0145 count_tokens reserves no budget or pins"]
async fn count_tokens_does_not_reserve_budget_windows_or_create_pins() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "aux-counter").await;
    let token = create_client_key(&harness, user, "aux-counter-key").await;
    let (addr, _mock) = spawn_mock(false).await;
    let provider = create_provider(&harness, "anthropic", format!("http://{addr}")).await;
    let key = create_upstream_key(&harness, provider, "anthropic", 100).await;
    let model = insert_model(&harness, "claude-sonnet").await;
    insert_route(&harness, key, model, "claude-native", r#"["messages"]"#).await;

    let app = build_app(&harness).await;
    let response = app
        .oneshot(request(
            "POST",
            "/v1/messages/count_tokens",
            &token,
            Some(r#"{"model":"claude-sonnet","messages":[{"role":"user","content":"hi"}]}"#),
        ))
        .await
        .expect("count tokens");
    assert_eq!(response.status(), StatusCode::OK);
    flush(&harness).await;

    let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempts")
        .fetch_one(&harness.pool)
        .await
        .expect("count attempts");
    assert_eq!(
        attempts, 0,
        "count_tokens is auxiliary and must not enqueue usage attempts"
    );

    let pins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pins")
        .fetch_one(&harness.pool)
        .await
        .expect("count pins");
    assert_eq!(pins, 0, "count_tokens must not create session pins");
}

/// Status-quo pin: count_tokens is already RPM-limited via the auth middleware.
///
/// This passes before and after the change. It exists so a later router-wiring
/// "simplification" cannot quietly drop the G5 coverage count_tokens already has.
#[tokio::test]
#[doc = "Spec: CLK-1, API-2 — ISSUE-0145 count_tokens stays RPM limited (status-quo pin)"]
async fn count_tokens_is_still_rpm_limited() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "rpm-counter").await;
    let token = create_client_key(&harness, user, "rpm-counter-key").await;
    let (addr, mock) = spawn_mock(false).await;
    let provider = create_provider(&harness, "anthropic", format!("http://{addr}")).await;
    let key = create_upstream_key(&harness, provider, "anthropic", 100).await;
    let model = insert_model(&harness, "claude-sonnet").await;
    insert_route(&harness, key, model, "claude-native", r#"["messages"]"#).await;

    sqlx::query("UPDATE client_keys SET rate_limit_rpm = 1")
        .execute(&harness.pool)
        .await
        .expect("set rpm");

    let app = build_app(&harness).await;
    let body = r#"{"model":"claude-sonnet","messages":[{"role":"user","content":"hi"}]}"#;
    let first = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/messages/count_tokens",
            &token,
            Some(body),
        ))
        .await
        .expect("first");
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(request(
            "POST",
            "/v1/messages/count_tokens",
            &token,
            Some(body),
        ))
        .await
        .expect("second");
    assert_eq!(
        second.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a 1-RPM key must be refused on its second count_tokens call"
    );

    let calls = mock.calls.lock().expect("calls");
    let upstream_calls = calls
        .iter()
        .filter(|call| call.path == "/messages/count_tokens")
        .count();
    assert_eq!(
        upstream_calls, 1,
        "only the admitted request may reach upstream; the RPM-rejected one must not"
    );
}

// `count_tokens` failover, failure learning, decode-failure and metric coverage
// (`ISSUE-0146`). Kept in a submodule so this file stays under the MNT-3 ceiling.
#[path = "integration_state_bound_responses/count_tokens_failover.rs"]
mod count_tokens_failover;

// `forward_state_bound_response_op`: decode failure must not fake a 200 (`ISSUE-0156`),
// and the body controls #155 names are not applicable to a bodyless op (`ISSUE-0155`).
#[path = "integration_state_bound_responses/state_bound_billing.rs"]
mod state_bound_billing;
#[path = "integration_state_bound_responses/state_bound_decode.rs"]
mod state_bound_decode;
#[path = "integration_state_bound_responses/state_bound_fixture.rs"]
pub(crate) mod state_bound_fixture;
