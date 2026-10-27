//! `count_tokens` must fail over, learn from failures, and never fake a 200
//! (`ISSUE-0146`, `API-1`, `API-8`, `F040`).
//!
//! Cycle 9 gave this handler the shared pre-candidate gates. What was left inside the
//! candidate loop was worse than the old miniature, because it *looked* finished:
//!
//! 1. A send timeout or transport error hit `_ => continue` — no failure recorded, no
//!    key-health write, no slow metric. The key kept its perfect health record while
//!    being unusable, so inference kept selecting it.
//! 2. A `401`/`403`/`429`/`5xx` from upstream was *returned to the client*, not failed
//!    over. Only `404`/`405`/`501` caused a `continue`.
//! 3. `decode_upstream_body(..).unwrap_or_default()` turned a corrupt compressed body
//!    into **`200 OK` with an empty body**. A client reads that as "your prompt is
//!    empty" and sends the real request; this is the defect with the worst consequence.
//! 4. `record_request_metric` was never called, so `count_tokens` traffic did not exist
//!    in `llmroller_requests_total`.
//!
//! These tests assert what *we* do: which mock was called, what the client saw, what the
//! health entry says, and a label-scoped metric delta. Nothing here asserts tokio or
//! hyper internals.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use tower::ServiceExt as _;

use llmroller_core::breaker::OriginKey;
use llmroller_core::ids::{ModelPk, ProviderId, UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, SystemSeal, SystemTask};
use llmroller_core::runtime_state::RuntimeState;
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
use llmroller_store::writer::{
    ReloadNotification, WriterHandle, flush_data_queues, start_writer,
};

/// A short `first_activity_timeout_ms`, so the transport-failure test's hang is measured
/// in milliseconds instead of the 60 s default.
const SHORT_TIMEOUT_CONFIG: &str =
    r#"{"timeouts":{"first_activity_timeout_ms":300,"nonstream_response_timeout_ms":2000}}"#;

const COUNT_BODY: &str = r#"{"input_tokens":42}"#;

// ---------------------------------------------------------------------------
// Mocks: one behaviour per mock, selected by constructor
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MockState {
    kind: MockKind,
    calls: Arc<AtomicUsize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MockKind {
    /// Answers `200` with a real token count.
    Ok,
    /// Answers with an upstream error status and a plain body.
    Status(u16),
    /// Never answers — the client must hit its send timeout.
    Hang,
    /// Answers `200` with `content-encoding: gzip` and bytes that are not gzip.
    CorruptGzip,
}

impl MockState {
    fn new(kind: MockKind) -> Self {
        Self {
            kind,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

async fn mock_count_tokens(State(state): State<MockState>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    match state.kind {
        MockKind::Ok => (
            StatusCode::OK,
            [("content-type", "application/json")],
            COUNT_BODY,
        )
            .into_response(),
        MockKind::Status(code) => {
            let status = StatusCode::from_u16(code).expect("valid mock status");
            (
                status,
                [("content-type", "application/json")],
                format!(r#"{{"error":{{"code":"upstream_{code}"}}}}"#),
            )
                .into_response()
        }
        MockKind::Hang => {
            tokio::time::sleep(Duration::from_secs(30)).await;
            StatusCode::OK.into_response()
        }
        MockKind::CorruptGzip => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(b"this is not gzip".to_vec()))
            .expect("corrupt gzip response"),
    }
}

async fn spawn_mock(kind: MockKind) -> (String, MockState) {
    let state = MockState::new(kind);
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

// ---------------------------------------------------------------------------
// Harness (mirrors the parent test module, reduced to what these tests need)
// ---------------------------------------------------------------------------

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
    let pool = open_pool(&dir.path().join("count_tokens_failover.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xBD; 32]);
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

async fn create_provider(
    harness: &Harness,
    name: &str,
    base_url: String,
    config_json: &str,
) -> ProviderId {
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

async fn insert_route(harness: &Harness, key_id: UpstreamKeyId, model_pk: ModelPk) {
    sqlx::query(
        "INSERT INTO model_routes \
         (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, 'claude-native', '[\"messages\"]', 'active', 1, 1000)",
    )
    .bind(key_id.to_string())
    .bind(model_pk.to_string())
    .execute(&harness.pool)
    .await
    .expect("insert model route");
}

/// One upstream candidate: a provider, a key, and a route for `model`.
async fn add_candidate(
    harness: &Harness,
    label: &str,
    base_url: String,
    model: ModelPk,
    priority: i64,
    config_json: &str,
) -> (ProviderId, UpstreamKeyId, String) {
    let provider = create_provider(harness, label, base_url.clone(), config_json).await;
    let key = create_upstream_key(harness, provider, label, priority).await;
    insert_route(harness, key, model).await;
    (provider, key, base_url)
}

async fn build_app_with_runtime(harness: &Harness) -> (Router, Arc<RuntimeState>) {
    let snapshot = load_initial_snapshot(&harness.pool)
        .await
        .expect("load snapshot");
    let shared_snapshot = Arc::new(arc_swap::ArcSwap::from_pointee(snapshot));
    let state = ProxyAuthState::new(shared_snapshot, harness.pool.clone(), vec![])
        .with_master_key(harness.master_key.clone())
        .with_writer(harness.writer.clone());
    state.is_ready.store(true, Ordering::SeqCst);
    let runtime = Arc::clone(&state.runtime_state);
    (data_plane_router(state), runtime)
}

async fn flush(harness: &Harness) {
    flush_data_queues(&harness.pool, &harness.writer.queues).await;
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

async fn body_bytes(response: Response) -> (StatusCode, Vec<u8>) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body");
    (status, bytes.to_vec())
}

/// The origin a provider's base URL resolves to — the coordinate
/// `record_upstream_failure` keys on.
fn origin_of(base_url: &str) -> OriginKey {
    OriginKey::from_base_url(base_url, true)
}

// ---------------------------------------------------------------------------
// Defect 2: retryable upstream statuses must fail over
// ---------------------------------------------------------------------------

/// A `500` from the first candidate must not reach the client: the loop has to treat it as
/// a host failure, record it, and try the next candidate. Pre-fix the client got A's `500`
/// even though B was available and healthy.
#[tokio::test]
#[doc = "Spec: API-1, API-8 — ISSUE-0146 count_tokens fails over on a retryable status"]
async fn count_tokens_fails_over_on_a_retryable_status() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-500-user").await;
    let token = create_client_key(&harness, user, "ct-500-key").await;
    let model = insert_model(&harness, "ct-failover-500").await;

    let (bad_url, bad_mock) = spawn_mock(MockKind::Status(500)).await;
    let (_p_a, _k_a, _) = add_candidate(&harness, "ct-500-a", bad_url, model, 200, "{}").await;
    let (good_url, good_mock) = spawn_mock(MockKind::Ok).await;
    let (_p_b, _k_b, _) = add_candidate(&harness, "ct-500-b", good_url, model, 100, "{}").await;

    let (app, _runtime) = build_app_with_runtime(&harness).await;
    let response = app
        .oneshot(count_request(&token, "ct-failover-500"))
        .await
        .expect("count tokens");
    let (status, body) = body_bytes(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "the healthy candidate's 200 must reach the client; body was {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(bad_mock.calls(), 1, "candidate A must be tried exactly once");
    assert_eq!(
        good_mock.calls(),
        1,
        "candidate B must be tried after A's 500, so exactly two upstream calls happen"
    );
}

/// `401` is `CredentialInvalid`, which the inference path fails over on. Same here.
#[tokio::test]
#[doc = "Spec: API-8 — ISSUE-0146 count_tokens fails over on a 401"]
async fn count_tokens_fails_over_on_a_credential_failure() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-401-user").await;
    let token = create_client_key(&harness, user, "ct-401-key").await;
    let model = insert_model(&harness, "ct-failover-401").await;

    let (bad_url, bad_mock) = spawn_mock(MockKind::Status(401)).await;
    add_candidate(&harness, "ct-401-a", bad_url, model, 200, "{}").await;
    let (good_url, good_mock) = spawn_mock(MockKind::Ok).await;
    add_candidate(&harness, "ct-401-b", good_url, model, 100, "{}").await;

    let (app, _runtime) = build_app_with_runtime(&harness).await;
    let (status, _body) = body_bytes(
        app.oneshot(count_request(&token, "ct-failover-401"))
            .await
            .expect("count tokens"),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "a revoked key must fail over, not 401 the client");
    assert_eq!(bad_mock.calls(), 1);
    assert_eq!(good_mock.calls(), 1);
}

// ---------------------------------------------------------------------------
// Defect 1 + the "learn from failures" half
// ---------------------------------------------------------------------------

/// A send timeout must fail over (defect 1's visible half).
#[tokio::test]
#[doc = "Spec: API-1, API-8 — ISSUE-0146 count_tokens fails over on a send timeout"]
async fn count_tokens_fails_over_on_a_transport_failure() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-hang-user").await;
    let token = create_client_key(&harness, user, "ct-hang-key").await;
    let model = insert_model(&harness, "ct-failover-hang").await;

    let (hang_url, hang_mock) = spawn_mock(MockKind::Hang).await;
    add_candidate(
        &harness,
        "ct-hang-a",
        hang_url,
        model,
        200,
        SHORT_TIMEOUT_CONFIG,
    )
    .await;
    let (good_url, good_mock) = spawn_mock(MockKind::Ok).await;
    add_candidate(&harness, "ct-hang-b", good_url, model, 100, "{}").await;

    let (app, _runtime) = build_app_with_runtime(&harness).await;
    // Bounded by the test, not just by the provider timeout: if the handler ever stopped
    // failing over, this would otherwise wait out the mock's own 30 s sleep.
    let (status, _body) = tokio::time::timeout(
        Duration::from_secs(20),
        async {
            body_bytes(
                app.oneshot(count_request(&token, "ct-failover-hang"))
                    .await
                    .expect("count tokens"),
            )
            .await
        },
    )
    .await
    .expect("count_tokens must not hang when an upstream stalls");

    assert_eq!(status, StatusCode::OK, "the healthy candidate must answer");
    assert_eq!(hang_mock.calls(), 1);
    assert_eq!(good_mock.calls(), 1);
}

/// The half with production consequence: an abandoned key must be recorded as failed.
///
/// Pre-fix the transport error was swallowed by `_ => continue`, so the key kept
/// `consecutive_failures == 0` and `healthy == true` while being unusable. Inference reads
/// the same tracker, so a dead key that `count_tokens` never reports keeps getting chosen.
#[tokio::test]
#[doc = "Spec: API-8, REL-5 — ISSUE-0146 count_tokens records a failure on the abandoned key"]
async fn count_tokens_records_a_failure_on_the_key_it_abandoned() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-health-user").await;
    let token = create_client_key(&harness, user, "ct-health-key").await;
    let model = insert_model(&harness, "ct-failover-health").await;

    let (hang_url, _hang_mock) = spawn_mock(MockKind::Hang).await;
    let (_p_a, key_a, base_a) = add_candidate(
        &harness,
        "ct-health-a",
        hang_url,
        model,
        200,
        SHORT_TIMEOUT_CONFIG,
    )
    .await;
    let (good_url, _good_mock) = spawn_mock(MockKind::Ok).await;
    add_candidate(&harness, "ct-health-b", good_url, model, 100, "{}").await;

    let (app, runtime) = build_app_with_runtime(&harness).await;
    let before = runtime.key_health_entry(key_a);
    assert_eq!(
        before.consecutive_failures, 0,
        "precondition: the key starts with a clean record"
    );

    let (status, _body) = tokio::time::timeout(
        Duration::from_secs(20),
        async {
            body_bytes(
                app.oneshot(count_request(&token, "ct-failover-health"))
                    .await
                    .expect("count tokens"),
            )
            .await
        },
    )
    .await
    .expect("count_tokens must not hang");

    assert_eq!(status, StatusCode::OK, "failover must still serve the request");
    let after = runtime.key_health_entry(key_a);
    assert!(
        after.consecutive_failures > 0,
        "the abandoned key must be recorded as failed; got {after:?} for origin {}",
        origin_of(&base_a).host
    );
}

/// A retryable *status* must also be recorded, not just a transport error.
#[tokio::test]
#[doc = "Spec: API-8 — ISSUE-0146 count_tokens records a failure on a 5xx key"]
async fn count_tokens_records_a_failure_on_a_status_failure() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-health5-user").await;
    let token = create_client_key(&harness, user, "ct-health5-key").await;
    let model = insert_model(&harness, "ct-failover-health5").await;

    let (bad_url, _bad_mock) = spawn_mock(MockKind::Status(503)).await;
    let (_p_a, key_a, _) = add_candidate(&harness, "ct-health5-a", bad_url, model, 200, "{}").await;
    let (good_url, _good_mock) = spawn_mock(MockKind::Ok).await;
    add_candidate(&harness, "ct-health5-b", good_url, model, 100, "{}").await;

    let (app, runtime) = build_app_with_runtime(&harness).await;
    let before = runtime.key_health_entry(key_a);
    assert_eq!(before.consecutive_failures, 0);

    let (status, _body) = body_bytes(
        app.oneshot(count_request(&token, "ct-failover-health5"))
            .await
            .expect("count tokens"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let after = runtime.key_health_entry(key_a);
    assert!(
        after.consecutive_failures > 0,
        "a 503 must count against the key that produced it; got {after:?}"
    );
}

// ---------------------------------------------------------------------------
// Defect 3: a decode failure is a 502, never an empty 200
// ---------------------------------------------------------------------------

/// A corrupt compressed upstream body must surface as `502` with a non-empty, actionable
/// body. Pre-fix the client saw `200 OK` with zero bytes, i.e. "your prompt is empty".
#[tokio::test]
#[doc = "Spec: API-1 — ISSUE-0146 count_tokens returns 502 on a corrupt upstream body"]
async fn count_tokens_returns_502_on_a_corrupt_compressed_upstream_body() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-gzip-user").await;
    let token = create_client_key(&harness, user, "ct-gzip-key").await;
    let model = insert_model(&harness, "ct-corrupt-gzip").await;

    let (url, mock) = spawn_mock(MockKind::CorruptGzip).await;
    add_candidate(&harness, "ct-gzip", url, model, 100, "{}").await;

    let (app, _runtime) = build_app_with_runtime(&harness).await;
    let (status, body) = body_bytes(
        app.oneshot(count_request(&token, "ct-corrupt-gzip"))
            .await
            .expect("count tokens"),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "an undecodable upstream body is a gateway fault, not a successful count"
    );
    assert!(
        !body.is_empty(),
        "the 502 must carry an actionable error body; an empty body would repeat the bug"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("the 502 body must be JSON the client can read");
    assert_eq!(
        json["error"]["type"], "upstream_decode_error",
        "the error type must name the decode failure; got {json}"
    );
    assert_eq!(mock.calls(), 1, "the corrupt body is discovered after one call");
}

// ---------------------------------------------------------------------------
// Defect 4: count_tokens traffic must be visible in the request metric
// ---------------------------------------------------------------------------

/// Read the summed value of `name`, keeping only series carrying every label pair.
///
/// **Label-scoped by construction** — the lesson from cycle 14fix (`89d7e80`). An unscoped
/// read is satisfied by any parallel test in this binary emitting the same metric, so it
/// cannot tell whether *this* handler emitted anything.
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

    let (url, _mock) = spawn_mock(MockKind::Ok).await;
    add_candidate(&harness, "ct-metric", url, model, 100, "{}").await;

    let (app, _runtime) = build_app_with_runtime(&harness).await;
    let scoped = || read_metric_by_label("llmroller_requests_total", &[("endpoint", "count_tokens")]);
    let before = scoped();

    let (status, _body) = body_bytes(
        app.oneshot(count_request(&token, "ct-metric"))
            .await
            .expect("count tokens"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let after = scoped();
    assert_eq!(
        after - before,
        1.0,
        "one served count_tokens request must add exactly one sample under \
         endpoint=\"count_tokens\"; got {before} -> {after}"
    );
}

/// The metric must also be emitted on the 502 path, so a failing endpoint is visible too.
#[tokio::test]
#[doc = "Spec: OBS-1 — ISSUE-0146 count_tokens emits a request metric on failure"]
async fn count_tokens_emits_a_request_metric_on_a_decode_failure() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-metric5-user").await;
    let token = create_client_key(&harness, user, "ct-metric5-key").await;
    let model = insert_model(&harness, "ct-metric-fail").await;

    let (url, _mock) = spawn_mock(MockKind::CorruptGzip).await;
    add_candidate(&harness, "ct-metric-fail", url, model, 100, "{}").await;

    let (app, _runtime) = build_app_with_runtime(&harness).await;
    let scoped = || read_metric_by_label("llmroller_requests_total", &[("endpoint", "count_tokens")]);
    let before = scoped();

    let (status, _body) = body_bytes(
        app.oneshot(count_request(&token, "ct-metric-fail"))
            .await
            .expect("count tokens"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);

    assert_eq!(
        scoped() - before,
        1.0,
        "a failed count_tokens request must also be counted, or the 5xx rate is invisible"
    );
}

// ---------------------------------------------------------------------------
// Non-regressions
// ---------------------------------------------------------------------------

/// `404`/`405`/`501` keep their distinct meaning: the key does not implement the endpoint,
/// which is *not* a health problem, so it is cached as unsupported and the key's health is
/// left alone.
#[tokio::test]
#[doc = "Spec: F040 — ISSUE-0146 unsupported aux endpoints still short-circuit locally"]
async fn count_tokens_still_marks_unsupported_aux_endpoints() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-unsup-user").await;
    let token = create_client_key(&harness, user, "ct-unsup-key").await;
    let model = insert_model(&harness, "ct-unsupported").await;

    let (url, mock) = spawn_mock(MockKind::Status(404)).await;
    let (_p, key_a, _) = add_candidate(&harness, "ct-unsup", url, model, 100, "{}").await;

    let (app, runtime) = build_app_with_runtime(&harness).await;
    let (status, _body) = body_bytes(
        app.clone()
            .oneshot(count_request(&token, "ct-unsupported"))
            .await
            .expect("count tokens"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unsupported aux endpoint stays a local not_found, not a 502"
    );

    // Cached: the second request must not reach upstream.
    let (status2, _body2) = body_bytes(
        app.oneshot(count_request(&token, "ct-unsupported"))
            .await
            .expect("count tokens"),
    )
    .await;
    assert_eq!(status2, StatusCode::NOT_FOUND);
    assert_eq!(
        mock.calls(),
        1,
        "the negative probe must be cached; a 404 is not a retryable failure"
    );

    let health = runtime.key_health_entry(key_a);
    assert_eq!(
        health.consecutive_failures, 0,
        "an unsupported endpoint is not a key-health problem; got {health:?}"
    );
}

/// A non-retryable status (`400 RequestRejected`) still passes through to the client
/// unchanged — the fix must not turn every 4xx into a failover.
#[tokio::test]
#[doc = "Spec: API-1 — ISSUE-0146 count_tokens passes a non-retryable 400 through"]
async fn count_tokens_still_passes_through_a_non_retryable_400() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "ct-400-user").await;
    let token = create_client_key(&harness, user, "ct-400-key").await;
    let model = insert_model(&harness, "ct-passthrough-400").await;

    let (url_a, mock_a) = spawn_mock(MockKind::Status(400)).await;
    add_candidate(&harness, "ct-400-a", url_a, model, 200, "{}").await;
    let (url_b, mock_b) = spawn_mock(MockKind::Ok).await;
    add_candidate(&harness, "ct-400-b", url_b, model, 100, "{}").await;

    let (app, _runtime) = build_app_with_runtime(&harness).await;
    let (status, _body) = body_bytes(
        app.oneshot(count_request(&token, "ct-passthrough-400"))
            .await
            .expect("count tokens"),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a genuine 400 is the client's problem and must not be retried elsewhere"
    );
    assert_eq!(mock_a.calls(), 1);
    assert_eq!(
        mock_b.calls(),
        0,
        "a non-retryable status must not fail over to candidate B"
    );
}
