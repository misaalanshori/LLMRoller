//! Shared fixture for the state-bound Responses tests.
//!
//! Extracted from `state_bound_decode.rs` so `state_bound_billing.rs` can drive the same
//! harness instead of copying it. A second copy of a 250-line fixture is how two tests
//! quietly stop testing the same thing.
//!
//! The mock upstream answers `/responses` and `/responses/{id}`; `corrupt_after` lets the
//! link-creating `POST /v1/responses` succeed while the state-bound read fails to decode.

use super::*;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

// Re-exported so both consumers see what `state_bound_decode.rs` used to import directly.
pub(super) use axum::Router;
pub(super) use axum::body::{Body, to_bytes};
pub(super) use axum::extract::State;
pub(super) use axum::http::{Request, StatusCode, header};
pub(super) use axum::response::{IntoResponse, Response};
pub(super) use axum::routing::get;
pub(super) use llmroller_core::ids::{ModelPk, UpstreamKeyId, UserId};
pub(super) use llmroller_core::provider::ProviderKind;
pub(super) use llmroller_core::rbac::{ActorScope, OwnershipScope, SystemSeal, SystemTask};
pub(super) use llmroller_core::seams::{OsEntropy, secret_string};
pub(super) use llmroller_proxy::auth::ProxyAuthState;
pub(super) use llmroller_proxy::listeners::data_plane_router;
pub(super) use llmroller_server::reloader::load_initial_snapshot;
pub(super) use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
pub(super) use llmroller_store::crypto::MasterKey;
pub(super) use llmroller_store::instance::load_or_initialize;
pub(super) use llmroller_store::migrations::migrate;
pub(super) use llmroller_store::pool::{Role as PoolRole, open_pool};
pub(super) use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
pub(super) use llmroller_store::settings::seed_defaults;
pub(super) use llmroller_store::writer::{ReloadNotification, WriterHandle, start_writer};

pub(super) const UPSTREAM_RESPONSE: &str = r#"{
    "id": "resp_up_1",
    "object": "response",
    "status": "completed",
    "output": [{"type": "message", "content": [{"type": "output_text", "text": "hi"}]}]
}"#;

// ---------------------------------------------------------------------------
// Mock: answer the responses endpoints, optionally with a corrupt gzip body
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(super) struct MockState {
    corrupt_gzip: bool,
    /// When set, only responses *after* this many calls are corrupted. The link-creating
    /// `POST /v1/responses` must succeed; the state-bound read is what must fail.
    corrupt_after: usize,
    pub(super) calls: Arc<AtomicUsize>,
}

async fn mock_responses(State(state): State<MockState>) -> Response {
    let n = state.calls.fetch_add(1, Ordering::SeqCst);
    let corrupt = state.corrupt_gzip && n >= state.corrupt_after;
    if corrupt {
        // Claims gzip, delivers bytes that are not gzip: the decode must fail.
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(b"this is definitely not gzip".to_vec()))
            .expect("corrupt gzip response");
    }
    let path = "/responses";
    let _ = path;
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        UPSTREAM_RESPONSE,
    )
        .into_response()
}

pub(super) async fn spawn_mock(corrupt_gzip: bool) -> (String, MockState) {
    let state = MockState {
        corrupt_gzip,
        // Creation is the first call; everything after it is the state-bound read.
        corrupt_after: 1,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route("/responses/{id}", get(mock_responses))
        .route("/responses/{id}/input_items", get(mock_responses))
        .route(
            "/responses/{id}/cancel",
            axum::routing::post(mock_responses),
        )
        .route("/responses", axum::routing::post(mock_responses))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state)
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

pub(super) struct Harness {
    pub(super) _dir: tempfile::TempDir,
    pub(super) pool: sqlx::SqlitePool,
    pub(super) master_key: MasterKey,
    pub(super) system_actor: ActorScope,
    pub(super) writer: WriterHandle,
    pub(super) _reload_rx: tokio::sync::mpsc::Receiver<ReloadNotification>,
}

pub(super) async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("state_bound_decode.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xBF; 32]);
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

pub(super) async fn insert_user(pool: &sqlx::SqlitePool, username: &str) -> UserId {
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

pub(super) async fn create_client_key(harness: &Harness, user_id: UserId, label: &str) -> String {
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

/// Create the provider, key, model and route, then create the response link through a
/// real `POST /v1/responses` so `response_links` holds a row the state-bound op can find.
pub(super) async fn create_link(
    harness: &Harness,
    token: &str,
    corrupt_gzip: bool,
) -> (String, UpstreamKeyId, MockState) {
    let (base_url, mock) = spawn_mock(corrupt_gzip).await;
    let mut tx = harness.pool.begin().await.expect("begin");
    let provider = create_provider_tx(
        &mut tx,
        &harness.system_actor,
        NewProvider {
            name: "bound-decode".to_string(),
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
    let key = create_upstream_key_tx(
        &mut tx,
        &harness.system_actor,
        &harness.master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "bound-decode".to_string(),
            secret: secret_string("sk-bound-decode"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");
    tx.commit().await.expect("commit");

    let model: ModelPk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) \
         VALUES (?, 'gpt-4o', 1, 'smooth_weighted_rr')",
    )
    .bind(model.to_string())
    .execute(&harness.pool)
    .await
    .expect("insert model");
    sqlx::query(
        "INSERT INTO model_routes \
         (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, 'gpt-4o', '[\"chat\",\"responses\",\"messages\"]', 'active', 1, 1000)",
    )
    .bind(key.id.to_string())
    .bind(model.to_string())
    .execute(&harness.pool)
    .await
    .expect("insert model route");

    let app = build_app(harness).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"gpt-4o","input":"hello"}"#))
                .expect("create request"),
        )
        .await
        .expect("create response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "link creation must succeed"
    );
    let json: serde_json::Value = serde_json::from_slice(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("create body"),
    )
    .expect("create json");
    let id = json["id"].as_str().expect("response id").to_string();
    (id, key.id, mock)
}

pub(super) async fn build_app(harness: &Harness) -> Router {
    build_app_with_state(harness).await.0
}

pub(super) async fn build_app_with_state(harness: &Harness) -> (Router, ProxyAuthState) {
    let snapshot = load_initial_snapshot(&harness.pool)
        .await
        .expect("load snapshot");
    let shared_snapshot = Arc::new(arc_swap::ArcSwap::from_pointee(snapshot));
    let state = ProxyAuthState::new(shared_snapshot, harness.pool.clone(), vec![])
        .with_master_key(harness.master_key.clone())
        .with_writer(harness.writer.clone());
    state.is_ready.store(true, Ordering::SeqCst);
    (data_plane_router(state.clone()), state)
}

pub(super) async fn get_response(app: Router, token: &str, id: &str) -> (StatusCode, Vec<u8>) {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/responses/{id}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("get request"),
        )
        .await
        .expect("state-bound GET");
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec();
    (status, body)
}

/// Drain the writer queues so `usage_events` is readable.
pub(super) async fn flush(harness: &Harness) {
    llmroller_store::writer::flush_data_queues(&harness.pool, &harness.writer.queues).await;
}
