//! Streaming accounting, pre-first-frame failover, usage-chunk suppression, and
//! client-drop finalization regressions.
//!
//! Spec: Section 10.5.3, Section 12.4, Section 12.4.1, Section 9.1
//! Requirements: API-5, API-7, USG-1, USG-2, USG-3, RTE-8

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::stream;
use tempfile::tempdir;
use tokio::sync::Notify;
use tower::ServiceExt as _;

use llmroller_core::ids::{ModelPk, UpstreamKeyId, UserId};
use llmroller_core::pricing::Rates;
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, SystemSeal, SystemTask};
use llmroller_core::seams::{ExposeSecret as _, OsEntropy, secret_string};
use llmroller_core::strategy::{RouteId, RouteLoadTracker};
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

const SSE_OK: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",",
    "\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,",
    "\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},",
    "\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",",
    "\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[],",
    "\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":500,",
    "\"total_tokens\":1500}}\n\n",
    "data: [DONE]\n\n",
);

const SSE_OVERLOADED: &str = concat!(
    ": keep-alive\n\n",
    "data: {\"error\":{\"message\":\"overloaded\",\"code\":529}}\n\n",
);

/// One complete frame, then the upstream never sends another byte or closes.
const SSE_STALL_AFTER_FIRST_FRAME: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",",
    "\"created\":1700000000,\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,",
    "\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},",
    "\"finish_reason\":null}]}\n\n",
);

#[derive(Clone, Copy)]
enum MockMode {
    StreamOk,
    StreamOverloaded,
    StreamStall,
    ChatJson,
    Chat429,
}

#[derive(Clone)]
struct MockUpstreamState {
    mode: MockMode,
    calls: Arc<AtomicUsize>,
}

impl MockUpstreamState {
    fn new(mode: MockMode) -> Self {
        Self {
            mode,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

async fn mock_chat(State(state): State<MockUpstreamState>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    match state.mode {
        MockMode::StreamOk => sse_response(SSE_OK),
        MockMode::StreamOverloaded => sse_response(SSE_OVERLOADED),
        MockMode::StreamStall => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(
                stream::once(async {
                    Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        SSE_STALL_AFTER_FIRST_FRAME.as_bytes(),
                    ))
                })
                .chain(stream::pending()),
            ))
            .expect("stall response"),
        MockMode::ChatJson => (
            StatusCode::OK,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "id": "chatcmpl-json",
                "object": "chat.completion",
                "created": 1_700_000_000,
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "ok" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
            })),
        )
            .into_response(),
        MockMode::Chat429 => (
            StatusCode::TOO_MANY_REQUESTS,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "error": { "message": "rate limited", "code": "rate_limit_exceeded" }
            })),
        )
            .into_response(),
    }
}

fn sse_response(body: &'static str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("sse response")
}

async fn spawn_mock(mode: MockMode) -> (SocketAddr, MockUpstreamState) {
    let state = MockUpstreamState::new(mode);
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
    writer: WriterHandle,
    _reload_rx: tokio::sync::mpsc::Receiver<ReloadNotification>,
}

async fn harness() -> Harness {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("streaming.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xAA; 32]);
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

async fn create_provider(
    harness: &Harness,
    name: &str,
    base_url: String,
) -> llmroller_core::ids::ProviderId {
    create_provider_with_config(harness, name, base_url, "{}").await
}

async fn create_provider_with_config(
    harness: &Harness,
    name: &str,
    base_url: String,
    config_json: &str,
) -> llmroller_core::ids::ProviderId {
    let mut tx = harness.pool.begin().await.expect("begin");
    let provider = create_provider_tx(
        &mut tx,
        &harness.system_actor,
        NewProvider {
            name: name.to_string(),
            kind: ProviderKind::Custom,
            base_url,
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
    tx.commit().await.expect("commit");
    provider.id
}

async fn create_upstream_key(
    harness: &Harness,
    provider_id: llmroller_core::ids::ProviderId,
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
    build_app_with_runtime(harness).await.0
}

/// Build the data-plane router and expose the runtime state for hot-path assertions.
async fn build_app_with_runtime(
    harness: &Harness,
) -> (Router, Arc<llmroller_core::runtime_state::RuntimeState>) {
    let snapshot = load_initial_snapshot(&harness.pool)
        .await
        .expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let state = ProxyAuthState::new(shared_snapshot, harness.pool.clone(), vec![])
        .with_master_key(harness.master_key.clone())
        .with_writer(harness.writer.clone());
    state.is_ready.store(true, Ordering::SeqCst);
    let runtime = state.runtime_state.clone();
    (data_plane_router(state), runtime)
}

async fn build_app_with_shutdown(
    harness: &Harness,
    cut: Option<Arc<AtomicBool>>,
    notify: Option<Arc<Notify>>,
) -> Router {
    let snapshot = load_initial_snapshot(&harness.pool)
        .await
        .expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let mut state = ProxyAuthState::new(shared_snapshot, harness.pool.clone(), vec![])
        .with_master_key(harness.master_key.clone())
        .with_writer(harness.writer.clone());
    if let (Some(cut), Some(notify)) = (cut, notify) {
        state = state.with_shutdown_signal(cut, notify);
    }
    state.is_ready.store(true, Ordering::SeqCst);
    data_plane_router(state)
}

fn stream_request(token: &str, session: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(session) = session {
        builder = builder.header("x-llmroller-session-id", session);
    }
    builder
        .body(Body::from(
            r#"{"model":"gpt-4o","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("request")
}

async fn flush(harness: &Harness) {
    flush_data_queues(&harness.pool, &harness.writer.queues).await;
}

async fn single_key_route(
    harness: &Harness,
    addr: SocketAddr,
    label: &str,
    priority: i64,
) -> (llmroller_core::ids::ProviderId, UpstreamKeyId, ModelPk) {
    single_key_route_with_config(harness, addr, label, priority, "{}").await
}

async fn single_key_route_with_config(
    harness: &Harness,
    addr: SocketAddr,
    label: &str,
    priority: i64,
    config_json: &str,
) -> (llmroller_core::ids::ProviderId, UpstreamKeyId, ModelPk) {
    let provider =
        create_provider_with_config(harness, label, format!("http://{addr}"), config_json).await;
    let key = create_upstream_key(harness, provider, label, priority).await;
    let model_pk = insert_model(harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(harness, key, model_pk, "gpt-4o").await;
    (provider, key, model_pk)
}

#[tokio::test]
async fn test_in_flight_tracks_active_stream_lifetime() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "in-flight").await;
    let (mock_addr, _) = spawn_mock(MockMode::StreamStall).await;
    let (_provider, key_id, model_pk) =
        single_key_route(&harness, mock_addr, "in-flight", 100).await;
    let token = create_client_key(&harness, user, "in-flight-key").await;
    let (app, runtime) = build_app_with_runtime(&harness).await;
    let route_id = RouteId {
        upstream_key_id: key_id,
        model_pk,
    };

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        runtime.load_tracker.in_flight(&route_id),
        1,
        "P2C must see the active streaming route while its body is open (RTE-3)"
    );

    drop(response);
    for _ in 0..100 {
        if runtime.load_tracker.in_flight(&route_id) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        runtime.load_tracker.in_flight(&route_id),
        0,
        "dropping the client response must release the in-flight slot"
    );
}

#[tokio::test]
async fn test_streaming_sse_emits_usage_event_and_attempt_to_sqlite() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "streamer").await;
    let (mock_addr, mock_state) = spawn_mock(MockMode::StreamOk).await;
    let (_provider, key_id, _model) = single_key_route(&harness, mock_addr, "stream-a", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "stream-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect stream");
    let text = String::from_utf8(body.to_vec()).expect("utf8 body");
    assert!(
        text.contains("\"content\":\"hello\""),
        "delta must be forwarded"
    );
    assert!(
        text.contains("[DONE]"),
        "completion sentinel must be forwarded"
    );
    assert_eq!(mock_state.calls.load(Ordering::SeqCst), 1);
    flush(&harness).await;

    let (stream, termination, incomplete, input, output, cost, status): (
        i64,
        String,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT stream, termination, incomplete, input_uncached_tokens, output_tokens, \
                cost_micro_usd, status FROM usage_events",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event");
    assert_eq!(stream, 1, "streaming request must be recorded as stream");
    assert_eq!(termination, "completed");
    assert_eq!(incomplete, 0);
    assert_eq!(input, 1000, "prompt tokens from the final SSE frame");
    assert_eq!(output, 500, "completion tokens from the final SSE frame");
    assert_eq!(cost, 2000, "1000 input + 500 output at the seeded price");
    assert_eq!(status, 200);

    let (outcome, attempt_no, key, attempt_cost, charge_basis): (String, i64, String, i64, String) =
        sqlx::query_as(
            "SELECT outcome, attempt_no, upstream_key_id, charged_micro_usd, charge_basis \
             FROM usage_attempts",
        )
        .fetch_one(&harness.pool)
        .await
        .expect("usage attempt");
    assert_eq!(outcome, "served");
    assert_eq!(attempt_no, 1);
    assert_eq!(key, key_id.to_string());
    assert_eq!(attempt_cost, 2000, "the served attempt carries actual cost");
    assert_eq!(charge_basis, "actual");
}

#[tokio::test]
async fn test_pre_first_frame_sse_error_triggers_failover_to_backup_key() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "failover").await;
    let (primary_addr, primary_state) = spawn_mock(MockMode::StreamOverloaded).await;
    let (backup_addr, backup_state) = spawn_mock(MockMode::StreamOk).await;

    let provider_a = create_provider(&harness, "primary", format!("http://{primary_addr}")).await;
    let key_a = create_upstream_key(&harness, provider_a, "primary", 100).await;
    let provider_b = create_provider(&harness, "backup", format!("http://{backup_addr}")).await;
    let key_b = create_upstream_key(&harness, provider_b, "backup", 1).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_a, model_pk, "gpt-4o").await;
    insert_route(&harness, key_b, model_pk, "gpt-4o").await;

    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "failover-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the error frame must be intercepted before headers commit"
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect stream");
    let text = String::from_utf8(body.to_vec()).expect("utf8 body");
    assert!(
        text.contains("\"content\":\"hello\""),
        "backup frames must reach the client"
    );
    assert!(
        !text.contains("overloaded"),
        "the terminal error frame must never be flushed"
    );
    assert_eq!(primary_state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(backup_state.calls.load(Ordering::SeqCst), 1);
    flush(&harness).await;

    let (termination, failovers, key): (String, i64, String) =
        sqlx::query_as("SELECT termination, failovers, upstream_key_id FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect("usage event");
    assert_eq!(termination, "completed");
    assert_eq!(failovers, 1);
    assert_eq!(
        key,
        key_b.to_string(),
        "accounting follows the served backup key"
    );

    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT attempt_no, upstream_key_id, outcome FROM usage_attempts ORDER BY attempt_no",
    )
    .fetch_all(&harness.pool)
    .await
    .expect("attempts");
    assert_eq!(
        rows.len(),
        2,
        "the failed and served attempts are both recorded"
    );
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[0].1, key_a.to_string());
    assert_eq!(rows[0].2, "error_frame");
    assert_eq!(rows[1].0, 2);
    assert_eq!(rows[1].1, key_b.to_string());
    assert_eq!(rows[1].2, "served");
}

#[tokio::test]
async fn test_proxy_injected_include_usage_chunk_stripped_when_client_omitted_flag() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "stripper").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::StreamOk).await;
    single_key_route(&harness, mock_addr, "strip-a", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "strip-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect stream");
    let text = String::from_utf8(body.to_vec()).expect("utf8 body");
    assert!(text.contains("\"content\":\"hello\""));
    assert!(
        !text.contains("\"usage\""),
        "the proxy-injected usage-only chunk must be suppressed"
    );
    flush(&harness).await;

    let (input, output, cost): (i64, i64, i64) = sqlx::query_as(
        "SELECT input_uncached_tokens, output_tokens, cost_micro_usd FROM usage_events",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event");
    assert_eq!(
        input, 1000,
        "suppressed frames must still feed the usage tap"
    );
    assert_eq!(output, 500);
    assert_eq!(cost, 2000);
}

#[tokio::test]
async fn test_sticky_pin_follows_backup_after_pre_first_byte_failure() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "pinner").await;
    let (primary_addr, primary_state) = spawn_mock(MockMode::StreamOverloaded).await;
    let (backup_addr, backup_state) = spawn_mock(MockMode::StreamOk).await;

    let provider_a =
        create_provider(&harness, "pin-primary", format!("http://{primary_addr}")).await;
    let key_a = create_upstream_key(&harness, provider_a, "pin-primary", 100).await;
    let provider_b = create_provider(&harness, "pin-backup", format!("http://{backup_addr}")).await;
    let key_b = create_upstream_key(&harness, provider_b, "pin-backup", 1).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_a, model_pk, "gpt-4o").await;
    insert_route(&harness, key_b, model_pk, "gpt-4o").await;

    let token = create_client_key(&harness, user, "pin-key").await;
    let app = build_app(&harness).await;

    let first = app
        .clone()
        .oneshot(stream_request(&token, Some("session-abc")))
        .await
        .expect("first response");
    assert_eq!(first.status(), StatusCode::OK);
    let _ = to_bytes(first.into_body(), usize::MAX)
        .await
        .expect("drain first stream");

    let second = app
        .oneshot(stream_request(&token, Some("session-abc")))
        .await
        .expect("second response");
    assert_eq!(second.status(), StatusCode::OK);
    let _ = to_bytes(second.into_body(), usize::MAX)
        .await
        .expect("drain second stream");

    assert_eq!(
        primary_state.calls.load(Ordering::SeqCst),
        1,
        "the failed key must not be retried once the pin moves"
    );
    assert_eq!(
        backup_state.calls.load(Ordering::SeqCst),
        2,
        "the replacement ticket must pin the session to the backup"
    );
    flush(&harness).await;

    let (last_key, pin_outcome): (String, Option<String>) = sqlx::query_as(
        "SELECT upstream_key_id, pin_outcome FROM usage_events ORDER BY ts DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("latest event");
    assert_eq!(last_key, key_b.to_string());
    assert!(
        pin_outcome.is_some(),
        "a pinned session must record its outcome"
    );
}

#[tokio::test]
async fn test_client_drop_finalizes_stream_as_client_gone() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "dropper").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::StreamOk).await;
    single_key_route(&harness, mock_addr, "drop-a", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "drop-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    {
        let mut stream = response.into_body().into_data_stream();
        let first = stream.next().await;
        assert!(first.is_some(), "the first SSE frame must arrive");
        // Dropping the stream here simulates the client going away before EOF.
    }

    flush(&harness).await;
    let (termination, incomplete, stream_flag): (String, i64, i64) =
        sqlx::query_as("SELECT termination, incomplete, stream FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect("usage event");
    assert_eq!(termination, "client_gone");
    assert_eq!(incomplete, 1, "aborted streams must be marked incomplete");
    assert_eq!(stream_flag, 1);
}

#[tokio::test]
async fn test_non_streaming_served_attempt_records_actual_charge_basis() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "jsoner").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::ChatJson).await;
    single_key_route(&harness, mock_addr, "json-a", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "json-key").await;
    let app = build_app(&harness).await;

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body");
    flush(&harness).await;

    let (charge_basis, outcome, cost): (String, String, i64) =
        sqlx::query_as("SELECT charge_basis, outcome, charged_micro_usd FROM usage_attempts")
            .fetch_one(&harness.pool)
            .await
            .expect("attempt");
    assert_eq!(outcome, "served");
    assert_eq!(charge_basis, "actual");
    assert_eq!(cost, 2000);

    let (stream, cost): (i64, i64) =
        sqlx::query_as("SELECT stream, cost_micro_usd FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect("event");
    assert_eq!(stream, 0);
    assert_eq!(cost, 2000);
}

#[tokio::test]
async fn test_non_streaming_429_then_served_persists_both_attempts_and_real_cost() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ratelimited").await;
    let (primary_addr, primary_state) = spawn_mock(MockMode::Chat429).await;
    let (backup_addr, backup_state) = spawn_mock(MockMode::ChatJson).await;

    let provider_a =
        create_provider(&harness, "rate-primary", format!("http://{primary_addr}")).await;
    let key_a = create_upstream_key(&harness, provider_a, "rate-primary", 100).await;
    let provider_b =
        create_provider(&harness, "rate-backup", format!("http://{backup_addr}")).await;
    let key_b = create_upstream_key(&harness, provider_b, "rate-backup", 1).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_a, model_pk, "gpt-4o").await;
    insert_route(&harness, key_b, model_pk, "gpt-4o").await;
    seed_price(&harness).await;

    let token = create_client_key(&harness, user, "rate-key").await;
    let app = build_app(&harness).await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body");
    assert_eq!(primary_state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(backup_state.calls.load(Ordering::SeqCst), 1);
    flush(&harness).await;

    let attempts: Vec<(i64, String, String, i64, String)> = sqlx::query_as(
        "SELECT attempt_no, upstream_key_id, outcome, charged_micro_usd, charge_basis \
         FROM usage_attempts ORDER BY attempt_no",
    )
    .fetch_all(&harness.pool)
    .await
    .expect("attempts");
    assert_eq!(
        attempts.len(),
        2,
        "valid outcomes must survive the batch flush"
    );
    assert_eq!(attempts[0].0, 1);
    assert_eq!(attempts[0].1, key_a.to_string());
    assert_eq!(attempts[0].2, "http_429");
    assert_eq!(attempts[0].3, 0);
    assert_eq!(attempts[0].4, "none");
    assert_eq!(attempts[1].0, 2);
    assert_eq!(attempts[1].1, key_b.to_string());
    assert_eq!(attempts[1].2, "served");
    assert_eq!(attempts[1].3, 2000);

    let events: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT upstream_key_id, cost_micro_usd, input_uncached_tokens, output_tokens \
         FROM usage_events",
    )
    .fetch_all(&harness.pool)
    .await
    .expect("events");
    assert_eq!(
        events.len(),
        1,
        "a single usage event is written for the served attempt"
    );
    assert_eq!(events[0].0, key_b.to_string());
    assert_eq!(
        events[0].1, 2000,
        "cost comes from the price book, not a constant"
    );
    assert_eq!(events[0].2, 1000);
    assert_eq!(events[0].3, 500);
}

#[doc = "Spec: API-6, API-7, REL-1 - a stalled upstream after the first frame must close and record idle_timeout"]
#[tokio::test]
async fn test_post_first_frame_upstream_stall_records_idle_timeout() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "stalled").await;
    let (mock_addr, mock_state) = spawn_mock(MockMode::StreamStall).await;
    single_key_route_with_config(
        &harness,
        mock_addr,
        "stall-a",
        100,
        r#"{"timeouts":{"stream_idle_timeout_ms":200}}"#,
    )
    .await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "stall-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(response.into_body(), usize::MAX),
    )
    .await
    .expect("the idle timeout must close the response body")
    .expect("collect stream");
    let text = String::from_utf8(body.to_vec()).expect("utf8 body");
    assert!(
        text.contains("\"content\":\"hello\""),
        "the frame sent before the stall must reach the client"
    );
    assert!(
        !text.contains("[DONE]"),
        "a stalled upstream never sends a completion sentinel"
    );
    assert_eq!(
        mock_state.calls.load(Ordering::SeqCst),
        1,
        "a stream that already committed must not fail over"
    );
    flush(&harness).await;

    let (termination, incomplete, stream_flag): (String, i64, i64) =
        sqlx::query_as("SELECT termination, incomplete, stream FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect("usage event");
    assert_eq!(termination, "idle_timeout");
    assert_eq!(incomplete, 1, "idle timeouts are incomplete streams");
    assert_eq!(stream_flag, 1);
}

#[doc = "Spec: API-6, REL-1 - a live stream must observe the shutdown cut and record shutdown_cut"]
#[tokio::test]
async fn test_shutdown_cut_terminates_live_stream_and_records_shutdown_cut() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "draining").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::StreamStall).await;
    single_key_route_with_config(
        &harness,
        mock_addr,
        "drain-a",
        100,
        r#"{"timeouts":{"stream_idle_timeout_ms":60000}}"#,
    )
    .await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "drain-key").await;

    let cut = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let app = build_app_with_shutdown(&harness, Some(cut.clone()), Some(notify.clone())).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let first = stream.next().await.expect("first chunk").expect("chunk");
    assert!(
        String::from_utf8_lossy(&first).contains("\"content\":\"hello\""),
        "the first frame must arrive before the cut"
    );

    // Mirrors ShutdownSupervisor step 4: set the flag, then wake parked streams.
    cut.store(true, Ordering::SeqCst);
    notify.notify_waiters();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(chunk) = stream.next().await {
            chunk.expect("stream chunk");
        }
    })
    .await
    .expect("the shutdown cut must close the stream");

    flush(&harness).await;
    let (termination, incomplete): (String, i64) =
        sqlx::query_as("SELECT termination, incomplete FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect("usage event");
    assert_eq!(termination, "shutdown_cut");
    assert_eq!(incomplete, 1);
}

// Keep the writer handle reachable for the duration of each test.
#[allow(dead_code)]
fn _writer_handle(_writer: &WriterHandle) {}
