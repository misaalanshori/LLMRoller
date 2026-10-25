use super::*;

#[derive(Clone, Debug)]
pub(crate) struct RecordedRequest {
    pub path: &'static str,
    pub headers: HeaderMap,
    pub body: String,
}

#[derive(Clone, Default)]
pub(crate) struct MockUpstreamState {
    pub chat_calls: Arc<AtomicUsize>,
    pub responses_calls: Arc<AtomicUsize>,
    pub messages_calls: Arc<AtomicUsize>,
    pub recorded: Arc<Mutex<Vec<RecordedRequest>>>,
}

pub(crate) fn record_request(state: &MockUpstreamState, path: &'static str, headers: &HeaderMap, body: &[u8]) {
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

pub(crate) fn configured_upstream_error(headers: &HeaderMap) -> Option<Response> {
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

pub(crate) async fn mock_chat_handler(
    State(state): State<MockUpstreamState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    state.chat_calls.fetch_add(1, Ordering::SeqCst);
    record_request(&state, "/chat/completions", &headers, &body);

    if let Some(response) = configured_upstream_error(&headers) {
        return response;
    }

    if headers.contains_key("x-fail-429-once-then-200")
        && state.chat_calls.load(Ordering::SeqCst) == 1
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "1"), ("content-type", "application/json")],
            Json(serde_json::json!({ "error": "transient rate limit" })),
        )
            .into_response();
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

pub(crate) async fn mock_responses_handler(
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

pub(crate) async fn mock_messages_handler(
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

pub(crate) fn body_requests_stream(body: &[u8]) -> bool {
    let body = String::from_utf8_lossy(body);
    body.contains("\"stream\":true") || body.contains("\"stream\": true")
}

pub(crate) async fn spawn_mock_upstream() -> (SocketAddr, MockUpstreamState) {
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

/// Inserts a user plus an allowed-models client key and returns the raw token.
pub(crate) async fn seed_user_with_client_key(
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
            service_account_id: None,
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
pub(crate) async fn seed_provider_route(
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

pub(crate) async fn build_wire_app(pool: &sqlx::SqlitePool, master_key: &MasterKey) -> Router {
    build_wire_app_with_max_body(pool, master_key, 32).await
}

pub(crate) async fn build_wire_app_with_max_body(
    pool: &sqlx::SqlitePool,
    master_key: &MasterKey,
    max_body_mb: usize,
) -> Router {
    build_wire_app_and_writer(pool, master_key, max_body_mb)
        .await
        .0
}

/// Builds the data-plane router and returns the writer handle so a test can
/// drain the write-behind queues deterministically.
pub(crate) async fn build_wire_app_and_writer(
    pool: &sqlx::SqlitePool,
    master_key: &MasterKey,
    max_body_mb: usize,
) -> (Router, WriterHandle) {
    let (reloader_tx, _reloader_rx) = tokio::sync::mpsc::channel(100);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx);
    let snapshot = load_initial_snapshot(pool).await.expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let mut proxy_auth_state = ProxyAuthState::new(shared_snapshot, pool.clone(), vec![])
        .with_master_key(master_key.clone());
    proxy_auth_state.is_ready.store(true, Ordering::SeqCst);
    proxy_auth_state.writer = Some(writer.clone());
    proxy_auth_state.session_hmac_secret = Some(secret_bytes(vec![0x5A; 32]));
    proxy_auth_state.budget_semaphore = Arc::new(PreflightBudgetSemaphore::new(819, max_body_mb));
    (data_plane_router(proxy_auth_state), writer)
}

pub(crate) fn chat_request(token: &str) -> Request<Body> {
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

pub(crate) async fn seed_base(pool: &sqlx::SqlitePool, master_key: &MasterKey) {
    let entropy = OsEntropy;
    load_or_initialize(pool, "key-0", master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(pool, None, 1).await.expect("seed settings");
}

pub(crate) async fn seed_single_global_route(
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
pub(crate) async fn seed_global_route_with_secrets(
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

pub(crate) fn latest_request(mock_state: &MockUpstreamState, path: &str) -> RecordedRequest {
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

pub(crate) fn streaming_request(uri: &str, token: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

pub(crate) fn chat_request_with_header(token: &str, name: &'static str, value: &'static str) -> Request<Body> {
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

pub(crate) fn chat_request_with_body(token: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

pub(crate) fn recorded_authorizations(state: &MockUpstreamState) -> Vec<String> {
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

