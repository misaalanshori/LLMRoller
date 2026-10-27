//! `count_tokens` request-metric coverage (`ISSUE-0146`, `OBS-1`).
//!
//! A separate integration-test binary on purpose. `llmroller_requests_total` carries only
//! `endpoint`/`status_class`/`outcome` — there is no per-test dimension to scope on — and
//! the Prometheus registry is process-global. Two assertions on an absolute count of the
//! same label set are therefore only sound while the process belongs to them. Cargo gives
//! each integration-test file its own binary, so this file is the isolation boundary.
//!
//! The sibling `count_tokens_failover` module deliberately asserts only the 502 status and
//! the absence of an empty body; the exact sample count is asserted here.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
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
use llmroller_store::writer::{ReloadNotification, WriterHandle, start_writer};

const COUNT_BODY: &str = r#"{"input_tokens":42}"#;

#[derive(Clone)]
struct MockState {
    corrupt_gzip: bool,
    calls: Arc<AtomicUsize>,
}

async fn mock_count_tokens(State(state): State<MockState>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    if state.corrupt_gzip {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(b"this is not gzip".to_vec()))
            .expect("corrupt gzip response");
    }
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        COUNT_BODY,
    )
        .into_response()
}

async fn spawn_mock(corrupt_gzip: bool) -> (String, MockState) {
    let state = MockState {
        corrupt_gzip,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route("/messages/count_tokens", post(mock_count_tokens))
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

struct Harness {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    master_key: MasterKey,
    system_actor: ActorScope,
    writer: WriterHandle,
    _reload_rx: tokio::sync::mpsc::Receiver<ReloadNotification>,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("ct_metrics.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xBE; 32]);
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

/// Add one candidate provider/key/route for `model`, answering with `corrupt_gzip`.
async fn add_candidate(
    harness: &Harness,
    label: &str,
    model: ModelPk,
    corrupt_gzip: bool,
) -> (ProviderId, UpstreamKeyId, MockState) {
    let (base_url, mock) = spawn_mock(corrupt_gzip).await;
    let mut tx = harness.pool.begin().await.expect("begin");
    let provider = create_provider_tx(
        &mut tx,
        &harness.system_actor,
        NewProvider {
            name: label.to_string(),
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
    sqlx::query(
        "INSERT INTO model_routes \
         (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, 'claude-native', '[\"messages\"]', 'active', 1, 1000)",
    )
    .bind(key.id.to_string())
    .bind(model.to_string())
    .execute(&harness.pool)
    .await
    .expect("insert model route");
    (provider.id, key.id, mock)
}

async fn insert_model(harness: &Harness, canonical_id: &str) -> ModelPk {
    let model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) \
         VALUES (?, ?, 1, 'smooth_weighted_rr')",
    )
    .bind(model_pk.to_string())
    .bind(canonical_id)
    .execute(&harness.pool)
    .await
    .expect("insert model");
    model_pk
}

async fn build_app(harness: &Harness) -> Router {
    let snapshot = load_initial_snapshot(&harness.pool)
        .await
        .expect("load snapshot");
    let shared_snapshot = Arc::new(arc_swap::ArcSwap::from_pointee(snapshot));
    let state = ProxyAuthState::new(shared_snapshot, harness.pool.clone(), vec![])
        .with_master_key(harness.master_key.clone())
        .with_writer(harness.writer.clone());
    state.is_ready.store(true, Ordering::SeqCst);
    data_plane_router(state)
}

fn count_request(token: &str, model: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            r#"{{"model":"{model}","messages":[{{"role":"user","content":"hi"}}]}}"#
        )))
        .expect("count_tokens request")
}

/// Read the summed value of `name`, keeping only series carrying every label pair.
///
/// **Label-scoped by construction** — the lesson from cycle 14fix (`89d7e80`): an unscoped
/// read is satisfied by any other emitter in the same process and cannot tell whether
/// *this* handler recorded anything.
fn read_metric_by_label(name: &str, label_pairs: &[(&str, &str)]) -> f64 {
    llmroller_proxy::metrics::get_or_init_prometheus_recorder()
        .render()
        .lines()
        .filter(|line| line.starts_with(name))
        .filter(|line| {
            label_pairs
                .iter()
                .all(|(key, value)| line.contains(&format!("{key}=\"{value}\"")))
        })
        .filter_map(|line| line.rsplit_once(' '))
        .filter_map(|(_, value)| value.trim().parse::<f64>().ok())
        .sum()
}

/// The served-request series, scoped on the **full** label triple.
///
/// `endpoint` alone is not enough: both tests in this file hit `endpoint="count_tokens"`
/// and run in parallel, so an endpoint-only read would let one satisfy the other. The
/// success and failure paths emit different `status_class`/`outcome` pairs, which makes
/// each series belong to exactly one test.
fn served_samples() -> f64 {
    read_metric_by_label(
        "llmroller_requests_total",
        &[
            ("endpoint", "count_tokens"),
            ("status_class", "2xx"),
            ("outcome", "completed"),
        ],
    )
}

/// The failed-request series, same reasoning as `served_samples`.
fn failed_samples() -> f64 {
    read_metric_by_label(
        "llmroller_requests_total",
        &[
            ("endpoint", "count_tokens"),
            ("status_class", "5xx"),
            ("outcome", "error"),
        ],
    )
}

/// A served `count_tokens` request must increment `llmroller_requests_total` under
/// `endpoint="count_tokens"`. Pre-fix the handler never called `record_request_metric`, so
/// this was zero and the endpoint was invisible to every dashboard.
#[tokio::test]
#[doc = "Spec: OBS-1 — ISSUE-0146 count_tokens emits a request metric"]
async fn count_tokens_emits_a_request_metric() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-metric-user").await;
    let token = create_client_key(&harness, user, "ct-metric-key").await;
    let model = insert_model(&harness, "ct-metric").await;
    let (_p, _k, mock) = add_candidate(&harness, "ct-metric", model, false).await;

    let app = build_app(&harness).await;
    let before = served_samples();
    let response = app
        .oneshot(count_request(&token, "ct-metric"))
        .await
        .expect("count tokens");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX).await;

    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        served_samples() - before,
        1.0,
        "one served count_tokens request must add exactly one 2xx/completed sample under \
         endpoint=\"count_tokens\""
    );
}

/// The metric must also be emitted on the 502 path, so a failing endpoint is visible too.
/// Pre-fix the decode branch returned a bare status with no recording at all.
#[tokio::test]
#[doc = "Spec: OBS-1 — ISSUE-0146 count_tokens emits a request metric on failure"]
async fn count_tokens_emits_a_request_metric_on_a_decode_failure() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-metric-fail-user").await;
    let token = create_client_key(&harness, user, "ct-metric-fail-key").await;
    let model = insert_model(&harness, "ct-metric-fail").await;
    let (_p, _k, _mock) = add_candidate(&harness, "ct-metric-fail", model, true).await;

    let app = build_app(&harness).await;
    let before = failed_samples();
    let response = app
        .oneshot(count_request(&token, "ct-metric-fail"))
        .await
        .expect("count tokens");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let _ = to_bytes(response.into_body(), usize::MAX).await;

    assert_eq!(
        failed_samples() - before,
        1.0,
        "a failed count_tokens request must also be counted under 5xx/error, or its 5xx \
         rate is invisible"
    );
}
