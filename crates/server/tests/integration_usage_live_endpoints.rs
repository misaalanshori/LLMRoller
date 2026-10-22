//! Live usage/key endpoints and caller-scoped snapshot model listing regressions.
//!
//! Spec: Section 9.5, Section 13.4
//! Requirements: API-1, MDL-7, USG-7

use std::io::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use bytes::Bytes;
use flate2::Compression;
use flate2::write::GzEncoder;
use futures_util::StreamExt as _;
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_core::ids::{ModelPk, ProviderId, UpstreamKeyId, UserId};
use llmroller_core::pricing::Rates;
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
use llmroller_store::prices::{UpsertPriceParams, upsert_price_tx};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::writer::{ReloadNotification, WriterHandle, flush_data_queues, start_writer};

const CHAT_JSON: &str = r#"{
    "id": "chatcmpl-live",
    "object": "chat.completion",
    "created": 1700000000,
    "model": "gpt-4o",
    "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
    "usage": {"prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500}
}"#;

async fn mock_chat(State(calls): State<Arc<std::sync::atomic::AtomicUsize>>) -> Response {
    calls.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        CHAT_JSON,
    )
        .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let app = Router::new()
        .route("/chat/completions", post(mock_chat))
        .with_state(Arc::new(std::sync::atomic::AtomicUsize::new(0)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

const GZIP_SSE: &str = concat!(
    "data: {\"id\":\"chatcmpl-gzip\",\"object\":\"chat.completion.chunk\",",
    "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-gzip\",\"object\":\"chat.completion.chunk\",",
    "\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,",
    "\"total_tokens\":10}}\n\n",
    "data: [DONE]\n\n",
);

fn gzip(payload: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(payload).expect("compress mock response");
    encoder.finish().expect("finish gzip")
}

async fn mock_gzip_chat() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CONTENT_ENCODING, "gzip"),
        ],
        gzip(CHAT_JSON.as_bytes()),
    )
        .into_response()
}

/// Emit one gzip stream split across three independent HTTP chunks so the proxy must keep
/// decoder state across chunk boundaries (TRN-3, audit F18).
async fn mock_gzip_sse() -> Response {
    let compressed = gzip(GZIP_SSE.as_bytes());
    let split = compressed.len() / 3;
    let parts = [
        Bytes::copy_from_slice(&compressed[..split]),
        Bytes::copy_from_slice(&compressed[split..split * 2]),
        Bytes::copy_from_slice(&compressed[split * 2..]),
    ];
    let stream = futures_util::stream::iter(parts.into_iter().map(Ok::<_, std::io::Error>)).then(
        |part| async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            part
        },
    );
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CONTENT_ENCODING, "gzip"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn spawn_gzip_mock(streaming: bool) -> SocketAddr {
    let app = if streaming {
        Router::new().route("/chat/completions", post(mock_gzip_sse))
    } else {
        Router::new().route("/chat/completions", post(mock_gzip_chat))
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind gzip mock listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
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
    let pool = open_pool(&dir.path().join("live_usage.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xCD; 32]);
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

async fn create_client_key(
    harness: &Harness,
    user_id: UserId,
    label: &str,
    routing_scope: Option<&str>,
    budget: Option<i64>,
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
            routing_scope: routing_scope.map(ToString::to_string),
            rate_limit_rpm: Some(60),
            budget_micro_usd_month: budget,
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
    ownership: OwnershipScope,
    owner: Option<UserId>,
) -> ProviderId {
    let mut tx = harness.pool.begin().await.expect("begin");
    let provider = create_provider_tx(
        &mut tx,
        &harness.system_actor,
        NewProvider {
            name: name.to_string(),
            kind: ProviderKind::Custom,
            base_url,
            config_json: "{}".to_string(),
            ownership_scope: ownership,
            owner_user_id: owner,
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
    owner: Option<UserId>,
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
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");
    tx.commit().await.expect("commit");
    if let Some(owner) = owner {
        sqlx::query("UPDATE upstream_keys SET owner_user_id = ? WHERE id = ?")
            .bind(owner.to_string())
            .bind(key.id.to_string())
            .execute(&harness.pool)
            .await
            .expect("set key owner");
    }
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
) {
    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, ?, '[\"chat\",\"responses\",\"messages\"]', 'active', 1, 1000)",
    )
    .bind(key_id.to_string())
    .bind(model_pk.to_string())
    .bind(upstream_model_id)
    .execute(&harness.pool)
    .await
    .expect("insert model route");
}

async fn seed_price(harness: &Harness) {
    let mut tx = harness.pool.begin().await.expect("begin");
    upsert_price_tx(
        &mut tx,
        UpsertPriceParams {
            provider_id: String::new(),
            provider_kind: ProviderKind::Custom.as_str().to_string(),
            upstream_model_id: "gpt-4o".to_string(),
            rates: Rates {
                input_per_mtok_micro_usd: 1_000_000,
                output_per_mtok_micro_usd: 2_000_000,
                cache_read_per_mtok_micro_usd: 0,
                cache_write_per_mtok_micro_usd: 0,
            },
            tiers: Vec::new(),
            schedules: Vec::new(),
            source: "test".to_string(),
            now_ms: 1000,
        },
    )
    .await
    .expect("upsert price");
    tx.commit().await.expect("commit");
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

async fn flush(harness: &Harness) {
    flush_data_queues(&harness.pool, &harness.writer.queues).await;
}

fn get(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request")
}

fn get_with_header(uri: &str, token: &str, name: &str, value: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(name, value)
        .body(Body::empty())
        .expect("request")
}

async fn response_json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body");
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test]
async fn test_usage_and_key_reflect_live_monthly_spend_and_openrouter_shape() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "live-spend").await;
    let token = create_client_key(&harness, user, "live-key", None, Some(10_000_000)).await;
    let addr = spawn_mock().await;
    let provider = create_provider(
        &harness,
        "live",
        format!("http://{addr}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key = create_upstream_key(&harness, provider, "live", None).await;
    let model = insert_model(&harness, "gpt-4o").await;
    insert_route(&harness, key, model, "gpt-4o").await;
    seed_price(&harness).await;

    let app = build_app(&harness).await;
    let inference = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("inference");
    assert_eq!(inference.status(), StatusCode::OK);
    let _ = to_bytes(inference.into_body(), usize::MAX)
        .await
        .expect("drain inference");
    flush(&harness).await;

    let usage = app
        .clone()
        .oneshot(get_with_header(
            "/v1/usage",
            &token,
            "x-llmroller-session-id",
            "live-session",
        ))
        .await
        .expect("usage");
    assert_eq!(usage.status(), StatusCode::OK);
    let usage_json = response_json(usage).await;
    assert_eq!(usage_json["session_id"], "live-session");
    assert_eq!(
        usage_json["client_key_allowance"]["used_this_month_micro_usd"], 2000,
        "usage must report the live monthly client-key spend"
    );
    assert_eq!(
        usage_json["client_key_allowance"]["monthly_cap_micro_usd"],
        10_000_000
    );

    let key_response = app
        .clone()
        .oneshot(get("/v1/key", &token))
        .await
        .expect("key");
    assert_eq!(key_response.status(), StatusCode::OK);
    let key_json = response_json(key_response).await;
    assert_eq!(key_json["object"], "client_key_info");
    assert_eq!(key_json["used_this_month_micro_usd"], 2000);
    assert_eq!(key_json["data"]["usage"], 0.002);
    assert_eq!(key_json["data"]["limit"], 10.0);
    assert_eq!(key_json["data"]["limit_remaining"], 9.998);
    assert_eq!(key_json["data"]["rate_limit"]["requests"], 60);
    assert_eq!(key_json["data"]["rate_limit"]["interval"], "1m");

    let auth_key = app
        .oneshot(get("/v1/auth/key", &token))
        .await
        .expect("auth key");
    assert_eq!(auth_key.status(), StatusCode::OK);
    let auth_json = response_json(auth_key).await;
    assert_eq!(auth_json["used_this_month_micro_usd"], 2000);
    assert_eq!(auth_json["data"]["usage"], 0.002);
}

#[tokio::test]
#[doc = "Spec: TRN-3 — F14 non-streaming gzip is decoded for the client and usage tap"]
async fn test_gzip_non_streaming_response_is_decoded_and_accounted() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "gzip-nonstream").await;
    let token = create_client_key(&harness, user, "gzip-nonstream-key", None, None).await;
    let addr = spawn_gzip_mock(false).await;
    let provider = create_provider(
        &harness,
        "gzip-nonstream",
        format!("http://{addr}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key = create_upstream_key(&harness, provider, "gzip-nonstream", None).await;
    let model = insert_model(&harness, "gpt-4o").await;
    insert_route(&harness, key, model, "gpt-4o").await;

    let app = build_app(&harness).await;
    let inference = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("inference");
    assert_eq!(inference.status(), StatusCode::OK);
    assert!(
        inference.headers().get(header::CONTENT_ENCODING).is_none(),
        "the proxy must return identity bytes after transparent decoding"
    );
    let body = to_bytes(inference.into_body(), usize::MAX)
        .await
        .expect("collect inference body");
    let response: serde_json::Value =
        serde_json::from_slice(&body).expect("decoded response must be JSON");
    assert_eq!(response["usage"]["total_tokens"], 1500);

    flush(&harness).await;
    let (input_tokens, output_tokens): (i64, i64) = sqlx::query_as(
        "SELECT input_uncached_tokens, output_tokens FROM usage_events ORDER BY ts DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event");
    assert_eq!((input_tokens, output_tokens), (1000, 500));
}

#[tokio::test]
#[doc = "Spec: TRN-3 — F14 gzip SSE is decoded across chunk boundaries"]
async fn test_gzip_streaming_response_is_decoded_and_accounted() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "gzip-stream").await;
    let token = create_client_key(&harness, user, "gzip-stream-key", None, None).await;
    let addr = spawn_gzip_mock(true).await;
    let provider = create_provider(
        &harness,
        "gzip-stream",
        format!("http://{addr}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let key = create_upstream_key(&harness, provider, "gzip-stream", None).await;
    let model = insert_model(&harness, "gpt-4o").await;
    insert_route(&harness, key, model, "gpt-4o").await;

    let app = build_app(&harness).await;
    let inference = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-4o","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("inference");
    assert_eq!(inference.status(), StatusCode::OK);
    assert!(
        inference.headers().get(header::CONTENT_ENCODING).is_none(),
        "the proxy must not forward the upstream content-encoding"
    );
    let body = to_bytes(inference.into_body(), usize::MAX)
        .await
        .expect("collect inference body");
    let text = String::from_utf8(body.to_vec()).expect("SSE must be plain text");
    assert!(
        text.contains("data: "),
        "decoded SSE frames must reach the client"
    );
    assert!(text.contains("chat.completion.chunk"));
    assert!(!body.starts_with(&[0x1f, 0x8b]), "gzip magic must not leak");

    flush(&harness).await;
    let (input_tokens, output_tokens): (i64, i64) = sqlx::query_as(
        "SELECT input_uncached_tokens, output_tokens FROM usage_events ORDER BY ts DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event");
    assert_eq!((input_tokens, output_tokens), (7, 3));
}

#[tokio::test]
async fn test_models_are_caller_scoped_from_snapshot_for_personal_only() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "personal-only").await;
    let token = create_client_key(
        &harness,
        user,
        "personal-only-key",
        Some("personal_only"),
        None,
    )
    .await;
    let addr = spawn_mock().await;

    let personal_provider = create_provider(
        &harness,
        "personal",
        format!("http://{addr}"),
        OwnershipScope::Personal,
        Some(user),
    )
    .await;
    let personal_key =
        create_upstream_key(&harness, personal_provider, "personal", Some(user)).await;
    let personal_model = insert_model(&harness, "personal-model").await;
    insert_route(&harness, personal_key, personal_model, "personal-model").await;

    let global_provider = create_provider(
        &harness,
        "global",
        format!("http://{addr}"),
        OwnershipScope::Global,
        None,
    )
    .await;
    let global_key = create_upstream_key(&harness, global_provider, "global", None).await;
    let global_model = insert_model(&harness, "global-model").await;
    insert_route(&harness, global_key, global_model, "global-model").await;

    let app = build_app(&harness).await;
    let response = app
        .clone()
        .oneshot(get("/v1/models", &token))
        .await
        .expect("models");
    assert_eq!(response.status(), StatusCode::OK);
    let json = response_json(response).await;
    let ids: Vec<&str> = json["data"]
        .as_array()
        .expect("data array")
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .collect();
    assert!(
        ids.contains(&"personal-model"),
        "the caller's own model must be listed"
    );
    assert!(
        !ids.contains(&"global-model"),
        "personal_only must hide global-only models"
    );

    let anthropic = app
        .oneshot(get_with_header(
            "/v1/models",
            &token,
            "anthropic-version",
            "2023-06-01",
        ))
        .await
        .expect("anthropic models");
    assert_eq!(anthropic.status(), StatusCode::OK);
    let anthropic_json = response_json(anthropic).await;
    assert_eq!(anthropic_json["data"][0]["type"], "model");
}

#[tokio::test]
async fn test_usage_rejects_inaccessible_upstream_key_without_leaking_it() {
    let harness = harness().await;
    let owner = insert_user(&harness.pool, "usage-owner").await;
    let stranger = insert_user(&harness.pool, "usage-stranger").await;
    let stranger_token = create_client_key(&harness, stranger, "stranger-key", None, None).await;
    let addr = spawn_mock().await;

    let personal_provider = create_provider(
        &harness,
        "owned",
        format!("http://{addr}"),
        OwnershipScope::Personal,
        Some(owner),
    )
    .await;
    let personal_key = create_upstream_key(&harness, personal_provider, "owned", Some(owner)).await;

    let app = build_app(&harness).await;
    let response = app
        .oneshot(get(
            &format!("/v1/usage?upstream_key_id={personal_key}"),
            &stranger_token,
        ))
        .await
        .expect("usage");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "not_found_error"
    );
}
