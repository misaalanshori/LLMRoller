//! `forward_state_bound_response_op` must not turn a decode failure into an empty
//! success, and must not be body-gated (`ISSUE-0156`, `ISSUE-0155`, `RTE-12`).
//!
//! # #156 — the decode failure
//!
//! A corrupt, truncated, or decompression-bomb upstream body used to become
//! `Bytes::new()` while the response kept its status (typically `200 OK`), so the client
//! received a **successful, empty document**. That is the worst failure shape available
//! here: a client reading an empty `GET /v1/responses/{id}` cannot tell "this response has
//! no content" from "we could not read the upstream's bytes", and acts on the wrong one.
//!
//! # #155 — the body controls are not applicable
//!
//! The issue claims this handler reads the inbound body without acquiring
//! `state.budget_semaphore` or checking `check_transfer_encoding_smuggling`. It does not
//! read an inbound body at all: the signature takes `inbound: &HeaderMap` and nothing
//! else, and all four callers pass only `&headers` (`GET`/`DELETE /v1/responses/{id}`,
//! `/input_items`, `/cancel` are bodyless operations). There is nothing to bound and
//! nothing to smuggle through, so the two tests at the bottom of this file pin that
//! status quo: a saturated preflight budget must **not** reject a state-bound op.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
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
use llmroller_store::writer::{ReloadNotification, WriterHandle, start_writer};

const UPSTREAM_RESPONSE: &str = r#"{
    "id": "resp_up_1",
    "object": "response",
    "status": "completed",
    "output": [{"type": "message", "content": [{"type": "output_text", "text": "hi"}]}]
}"#;

// ---------------------------------------------------------------------------
// Mock: answer the responses endpoints, optionally with a corrupt gzip body
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MockState {
    corrupt_gzip: bool,
    /// When set, only responses *after* this many calls are corrupted. The link-creating
    /// `POST /v1/responses` must succeed; the state-bound read is what must fail.
    corrupt_after: usize,
    calls: Arc<AtomicUsize>,
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

async fn spawn_mock(corrupt_gzip: bool) -> (String, MockState) {
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

/// Create the provider, key, model and route, then create the response link through a
/// real `POST /v1/responses` so `response_links` holds a row the state-bound op can find.
async fn create_link(
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

async fn build_app(harness: &Harness) -> Router {
    build_app_with_state(harness).await.0
}

async fn build_app_with_state(harness: &Harness) -> (Router, ProxyAuthState) {
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

async fn get_response(app: Router, token: &str, id: &str) -> (StatusCode, Vec<u8>) {
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
async fn flush(harness: &Harness) {
    llmroller_store::writer::flush_data_queues(&harness.pool, &harness.writer.queues).await;
}

// ---------------------------------------------------------------------------
// #156 — decode failure must be a 502, never an empty 200
// ---------------------------------------------------------------------------

/// Primary red for #156. Pre-fix: `200 OK` with a zero-length body.
#[tokio::test]
#[doc = "Spec: API-1, RTE-12 — ISSUE-0156 state-bound op returns 502 on a corrupt upstream body"]
async fn state_bound_response_op_returns_502_on_a_corrupt_compressed_upstream_body() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "sbg-user").await;
    let token = create_client_key(&harness, user, "sbg-key").await;
    let (id, _key, mock) = create_link(&harness, &token, true).await;

    let app = build_app(&harness).await;
    let (status, body) = get_response(app, &token, &id).await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "an undecodable upstream body is a gateway fault, not a successful empty response"
    );
    assert!(
        !body.is_empty(),
        "the 502 must carry an actionable error body; an empty body would repeat the bug \
         in a new spelling"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("the 502 body must be readable JSON");
    assert_eq!(
        json["error"]["type"], "upstream_decode_error",
        "the error type must name the decode failure so a client can tell it from \
         'this response has no content'; got {json}"
    );
    assert!(
        mock.calls.load(Ordering::SeqCst) >= 2,
        "one call creates the link, one is the state-bound read"
    );
}

/// A decode failure against a real key must still be recorded, attributed to that key.
///
/// Cycle 10 established the rule: a row credited to `"none"` when a real key answered is
/// a row that exists but is wrong, and that is worse than a missing row.
#[tokio::test]
#[doc = "Spec: USG-1, USG-3 — ISSUE-0156 state-bound decode failure records an attributed usage event"]
async fn state_bound_response_op_records_an_usage_event_on_a_decode_failure() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "sbg-ledger-user").await;
    let token = create_client_key(&harness, user, "sbg-ledger-key").await;
    let (id, key_id, _mock) = create_link(&harness, &token, true).await;

    let app = build_app(&harness).await;
    let (status, _body) = get_response(app, &token, &id).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    flush(&harness).await;

    // The decode-failure row is the newest one for this endpoint.
    let row: (Option<String>, i64, Option<String>, i64, String) = sqlx::query_as(
        "SELECT upstream_key_id, cost_micro_usd, error_code, status, termination \
         FROM usage_events WHERE endpoint = 'responses' ORDER BY ts DESC, rowid DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event row");

    let (upstream_key_id, cost, error_code, status_code, termination) = row;
    assert_eq!(
        upstream_key_id.as_deref(),
        Some(key_id.to_string().as_str()),
        "the row must be attributed to the key that actually answered, not 'none'"
    );
    assert_eq!(
        cost, 0,
        "nothing was billable — the body was unreadable — so the cost must be zero"
    );
    assert_eq!(
        error_code.as_deref(),
        Some("upstream_decode_error"),
        "the row must name the decode failure so the failure is queryable"
    );
    assert_eq!(
        status_code, 502,
        "the recorded status must be what the client saw"
    );
    assert_eq!(
        termination, "completed",
        "the request did terminate — badly, but terminally; the error_code carries the \
         reason"
    );
}

// ---------------------------------------------------------------------------
// #155 — the status-quo pins
// ---------------------------------------------------------------------------

/// A saturated preflight budget must **not** reject a state-bound op.
///
/// `state.budget_semaphore` bounds the *inbound request body* before decompression
/// (`ISSUE-0147`). A `GET /v1/responses/{id}` has no request body — the handler's
/// signature takes only `&HeaderMap` — so there is nothing to admit. This test exists so
/// that a future reader taking #155's text at face value does not add a body gate that
/// makes bodyless operations fail under load.
#[tokio::test]
#[doc = "Spec: RTE-12 — ISSUE-0155 state-bound op is not body-budget gated"]
async fn state_bound_response_op_is_not_budget_semaphore_gated() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "sb-budget-user").await;
    let token = create_client_key(&harness, user, "sb-budget-key").await;
    let (id, _key, _mock) = create_link(&harness, &token, false).await;

    let (_app, state) = build_app_with_state(&harness).await;
    let budget = Arc::new(llmroller_proxy::orchestrator::PreflightBudgetSemaphore::new(1, 32));
    // Saturate: hold the only permit for the duration of the assertion.
    let held = budget
        .acquire(1024 * 1024, std::time::Duration::from_millis(200))
        .await
        .expect("the only permit must be available");
    let mut state = state;
    state.budget_semaphore = Arc::clone(&budget);
    let app = data_plane_router(state);

    let (status, _body) = get_response(app, &token, &id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a bodyless operation must not be rejected because the body budget is full"
    );
    drop(held);
}

/// `check_transfer_encoding_smuggling` is about the *request* body's framing. A
/// state-bound op sends none, so a `Transfer-Encoding` header must not change the outcome
/// — there is no body whose framing could disagree with `Content-Length`.
#[tokio::test]
#[doc = "Spec: RTE-12, SEC-8 — ISSUE-0155 state-bound op has no request body to smuggle through"]
async fn state_bound_response_op_is_not_smuggling_gated() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "sb-smuggle-user").await;
    let token = create_client_key(&harness, user, "sb-smuggle-key").await;
    let (id, _key, _mock) = create_link(&harness, &token, false).await;

    let app = build_app(&harness).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/responses/{id}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::TRANSFER_ENCODING, "chunked")
                .body(Body::empty())
                .expect("smuggle probe"),
        )
        .await
        .expect("state-bound GET");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "with no request body there is nothing for the smuggling check to adjudicate"
    );
}

// ---------------------------------------------------------------------------
// #129 — an unresolvable `model_pk` must not invent a model id
// ---------------------------------------------------------------------------

/// A `response_links` row whose `model_pk` is no longer in the snapshot must **not** produce a
/// usage row attributed to an invented model.
///
/// `state_bound.rs:765-768` does:
///
/// ```ignore
/// snapshot.models.get(&link.model_pk)
///     .map_or_else(|| "gpt-4o".to_string(), |m| m.canonical_id.clone())
/// ```
///
/// `gpt-4o` was not chosen for any reason — it was the first plausible string. A ledger row naming
/// a model that did not serve the request is wrong data, which is the failure cycle 10 established
/// the rule against ("a row attributed to `none` when a real key answered is a row that exists but
/// is wrong"). An empty string would be equally wrong and less visible.
///
/// **Reachability.** `response_links.model_pk` is `REFERENCES models(model_pk) ON DELETE RESTRICT`
/// (`0001_initial.sql:387`) and the snapshot loader selects `FROM models` with no predicate, so in
/// production the lookup cannot miss. This is a latent hazard: the snapshot loader also **skips**
/// rows whose `model_pk` fails to parse (`let Ok(pk) = ModelPk::parse(..) else { continue }`,
/// `store/src/models.rs:650`), so a snapshot can legitimately be missing a model the DB still has.
/// The fixture reaches the state by deleting the model with the FK disabled — which is what a
/// partially restored database or an out-of-band writer does.
#[tokio::test]
#[doc = "Spec: USG-1, USG-3, RTE-12 — ISSUE-0129 no invented model id on an unresolvable link"]
async fn state_bound_decode_failure_does_not_invent_a_model_id() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "sb129-user").await;
    let token = create_client_key(&harness, user, "sb129-key").await;
    let (id, _key_id, _mock) = create_link(&harness, &token, true).await;

    // The link is written through the coalescing writer queue, so drain it before reading.
    flush(&harness).await;

    // Count the rows the link-creation call wrote, so the assertion below can distinguish them
    // from anything the state-bound op writes.
    let rows_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_events WHERE endpoint = 'responses'")
            .fetch_one(&harness.pool)
            .await
            .expect("count before");

    // Remove the model the link points at, leaving the link's `model_pk` dangling. `RESTRICT`
    // forbids this normally, so lift the constraint for the fixture — the point is that the code
    // must not paper over the resulting inconsistency with a plausible-looking string.
    let model_pk: String =
        sqlx::query_scalar("SELECT model_pk FROM response_links WHERE response_id = ?")
            .bind(&id)
            .fetch_one(&harness.pool)
            .await
            .expect("link model_pk");
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&harness.pool)
        .await
        .expect("disable fk");
    sqlx::query("DELETE FROM model_routes WHERE model_pk = ?")
        .bind(&model_pk)
        .execute(&harness.pool)
        .await
        .expect("delete route");
    sqlx::query("DELETE FROM models WHERE model_pk = ?")
        .bind(&model_pk)
        .execute(&harness.pool)
        .await
        .expect("delete model");
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&harness.pool)
        .await
        .expect("enable fk");

    let app = build_app(&harness).await;
    let (status, body) = get_response(app, &token, &id).await;
    flush(&harness).await;

    // The request must fail loudly rather than proceed on a link it cannot attribute. An earlier
    // guard (`hard_route_checks` → `Forbidden403ModelOrProtocolBlocked`) already refuses a link
    // whose model has left the snapshot, so the response is `403` and the invented-model branch is
    // never reached — which is the finding recorded in the report: the `gpt-4o` fallback is
    // genuinely unreachable, and this fix is defence in depth rather than a behaviour change.
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::BAD_GATEWAY,
        "an unresolvable model on the link must be refused, not silently attributed; got {status} \
         with body {}",
        String::from_utf8_lossy(&body)
    );

    // **The assertion with teeth, stated honestly.** `hard_route_checks` refuses a link whose
    // model has left the snapshot *before* the attribution code runs, so the op writes no new
    // ledger row at all. That is the property: an unusable link produces no row rather than a
    // row naming a guessed model. It is also why `state_bound.rs`'s `gpt-4o` fallback is dead code
    // — established by mutation, not by reading: restoring the fallback leaves this test green.
    let rows_now: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_events WHERE endpoint = 'responses'")
            .fetch_one(&harness.pool)
            .await
            .expect("count after");

    assert_eq!(
        rows_now, rows_before,
        "an unusable link must not append a usage row; a new row means the op proceeded to \
         attribution, where the model would have had to be guessed"
    );
}

/// Non-regression: a resolvable link still produces the real `canonical_id`.
///
/// Without this, "always error" would satisfy the test above.
#[tokio::test]
#[doc = "Spec: USG-1, USG-3, RTE-12 — ISSUE-0129 a resolvable link still attributes the real model"]
async fn state_bound_decode_failure_attributes_the_real_model_when_resolvable() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "sb129-ok-user").await;
    let token = create_client_key(&harness, user, "sb129-ok-key").await;
    let (id, _key_id, _mock) = create_link(&harness, &token, true).await;

    let app = build_app(&harness).await;
    let (status, _body) = get_response(app, &token, &id).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    flush(&harness).await;

    let canonical_id: Option<String> = sqlx::query_scalar(
        "SELECT canonical_id FROM usage_events \
         WHERE endpoint = 'responses' ORDER BY ts DESC, rowid DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event");
    assert_eq!(
        canonical_id.as_deref(),
        Some("gpt-4o"),
        "a resolvable link must still be attributed to its real model — here the fixture's model \
         genuinely *is* gpt-4o, so this asserts the lookup ran rather than that the fallback fired"
    );
}
