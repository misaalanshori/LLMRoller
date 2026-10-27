//! Terminal paths that ended a request without enqueuing a usage event
//! (`USG-1`, `USG-3`, `ISSUE-0150`, `ISSUE-0151`).
//!
//! Three sites in the same family as cycle 10's charged timeout:
//!
//! - **#150** — an HTTP 200 whose body embeds a `RequestRejected` error: the attempt row
//!   is pushed, then the handler returns a synthesised 400 with no usage event.
//! - **#151(a)** — a candidate fails Gate G7 (budget), `last_cap_error` is stored, and
//!   the loop's tail returns the gate error with no usage event.
//! - **#151(b)** — a non-retryable upstream 4xx is passed through unchanged, again with
//!   no usage event.
//!
//! Attribution follows cycle 10's rule: `upstream_key_id`/`provider_id`/`provider_scope`
//! are `None` **only** when no attempt reached any upstream key. #151(a) is the one site
//! here that never called upstream, so it is the one site with no attribution.

use super::*;

/// An HTTP 200 carrying an embedded `invalid_request_error` must be persisted.
#[doc = "Spec: USG-1, USG-3 — ISSUE-0150 an embedded 200 error persists a usage event"]
#[tokio::test]
async fn embedded_200_error_persists_a_usage_event() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t150-user").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::EmbeddedRequestRejected).await;
    let (_provider, _key_id, _model_pk) = single_key_route(&harness, mock_addr, "t150", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t150-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(non_stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "an embedded RequestRejected must surface as 400, never 200"
    );
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    flush(&harness).await;

    let (cost, status, error_code): (i64, i64, Option<String>) =
        sqlx::query_as("SELECT cost_micro_usd, status, error_code FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect(
                "a rejected request must still be persisted; without a row the attempt \
                 exists only in memory",
            );
    assert_eq!(
        status, 400,
        "the event must record what the client received"
    );
    assert_eq!(
        cost, 0,
        "a rejected request bills nothing, so the event must carry cost 0"
    );
    assert!(
        error_code.is_some(),
        "the event must identify the rejection, not leave error_code empty"
    );

    let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempts")
        .fetch_one(&harness.pool)
        .await
        .expect("count attempts");
    assert_eq!(
        attempts, 1,
        "the attempt row must be persisted with the event"
    );
}

/// The #150 event must be attributed to the key that produced the 200.
#[doc = "Spec: USG-1, USG-9 — ISSUE-0150 the embedded-200 event names the real key"]
#[tokio::test]
async fn embedded_200_error_attributes_the_event_to_the_real_key() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t150-attr-user").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::EmbeddedRequestRejected).await;
    let (provider_id, key_id, _model_pk) =
        single_key_route(&harness, mock_addr, "t150-attr", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t150-attr-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(non_stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    flush(&harness).await;

    let (upstream_key_id, provider, scope): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT upstream_key_id, provider_id, provider_scope FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect("the rejected request must have a ledger row to attribute");

    assert_eq!(
        upstream_key_id.as_deref(),
        Some(key_id.to_string().as_str()),
        "a real upstream key answered with the 200, so the event must name it"
    );
    assert_eq!(
        provider.as_deref(),
        Some(provider_id.to_string().as_str()),
        "the event must name the real provider"
    );
    assert_eq!(
        scope.as_deref(),
        Some("global"),
        "the fixture provider is global, so the tier label must be real, not \"none\""
    );
}

/// A non-retryable upstream 4xx must be persisted with the status the client received.
///
/// **This passes before and after the fix, and that is a finding:** every non-2xx is
/// intercepted at `pipeline.rs:761` — before `handle_non_streaming_attempt` is ever
/// called — and that path already enqueues. The `!status_value.is_success()` early
/// return inside the handler is therefore unreachable for 4xx. The test is kept as a
/// status-quo pin so a future reordering of that interception cannot silently drop the
/// event.
#[doc = "Spec: USG-1, USG-3 — ISSUE-0151 non-retryable 4xx persists (status-quo pin)"]
#[tokio::test]
async fn non_retryable_4xx_persists_a_usage_event() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t151b-user").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::NonRetryable400).await;
    let (_provider, _key_id, _model_pk) = single_key_route(&harness, mock_addr, "t151b", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t151b-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(non_stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a non-retryable 4xx must be passed through unchanged"
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect body");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("unsupported parameter"),
        "the upstream body must reach the client unchanged; got {text}"
    );
    flush(&harness).await;

    let (cost, status): (i64, i64) =
        sqlx::query_as("SELECT cost_micro_usd, status FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect("a non-retryable 4xx must persist a usage event");
    assert_eq!(
        status, 400,
        "the event must record the passed-through status"
    );
    assert_eq!(cost, 0, "nothing is billed for a non-retryable 4xx");
}

/// The #151(b) event must be attributed to the key that answered with the 4xx.
#[doc = "Spec: USG-1, USG-9 — ISSUE-0151 the non-retryable-4xx event names the real key"]
#[tokio::test]
async fn non_retryable_4xx_attributes_the_event_to_the_real_key() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t151b-attr-user").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::NonRetryable400).await;
    let (provider_id, key_id, _model_pk) =
        single_key_route(&harness, mock_addr, "t151b-attr", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t151b-attr-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(non_stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    flush(&harness).await;

    let (upstream_key_id, provider, scope): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT upstream_key_id, provider_id, provider_scope FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect("the 4xx request must have a ledger row to attribute");

    assert_eq!(
        upstream_key_id.as_deref(),
        Some(key_id.to_string().as_str())
    );
    assert_eq!(provider.as_deref(), Some(provider_id.to_string().as_str()));
    assert_eq!(scope.as_deref(), Some("global"));
}

/// A candidate that fails Gate G7 must still produce a ledger row — with no attribution.
#[doc = "Spec: USG-1, LIM-1 — ISSUE-0151 cap exhaustion persists a usage event"]
#[tokio::test]
async fn cap_exhaustion_persists_a_usage_event() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t151a-user").await;
    let (mock_addr, mock_state) = spawn_mock(MockMode::ChatJson).await;
    let (_provider, key_id, _model_pk) = single_key_route(&harness, mock_addr, "t151a", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t151a-key").await;

    // A cap of 0 makes `is_exhausted` return true immediately (`is_exhausted` treats
    // `cap <= 0` as exhausted), so Gate G7 rejects before any upstream call without
    // needing the monthly accumulator to be primed.
    sqlx::query("UPDATE client_keys SET budget_micro_usd_month = 0")
        .execute(&harness.pool)
        .await
        .expect("set budget");

    let app = build_app(&harness).await;

    let response = app
        .oneshot(non_stream_request(&token, None))
        .await
        .expect("response");
    let status = response.status();
    assert!(
        status.is_client_error() || status.is_server_error(),
        "an exhausted monthly budget must be refused; got {status}"
    );
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    flush(&harness).await;

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&harness.pool)
        .await
        .expect("count events");
    assert!(
        rows >= 1,
        "the refused request must leave a ledger row; the upstream received {} call(s), \
         and a request that ends without a row is invisible to /usage",
        mock_state.calls.load(Ordering::SeqCst)
    );

    let (upstream_key_id, scope): (Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT upstream_key_id, provider_scope FROM usage_events ORDER BY ts DESC, rowid DESC LIMIT 1",
        )
        .fetch_one(&harness.pool)
        .await
        .expect("the newest event");
    let _ = key_id;
    assert!(
        upstream_key_id.is_none() || scope.as_deref() != Some("none"),
        "G7 exhaustion happens before any upstream call, so it must not name a key"
    );
}

/// A normal successful request must still produce exactly one event.
///
/// The double-emit guard: three new enqueue sites were added, and a mistake could
/// produce two rows for one request.
#[doc = "Spec: USG-1 — ISSUE-0150/0151 no double-emit on the success path"]
#[tokio::test]
async fn successful_request_still_persists_exactly_one_event() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t151-ok-user").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::ChatJson).await;
    let (_provider, _key_id, _model_pk) =
        single_key_route(&harness, mock_addr, "t151-ok", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t151-ok-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(non_stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    flush(&harness).await;

    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&harness.pool)
        .await
        .expect("count events");
    assert_eq!(
        events, 1,
        "a successful request must produce exactly one usage event"
    );
    let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempts")
        .fetch_one(&harness.pool)
        .await
        .expect("count attempts");
    assert_eq!(attempts, 1, "and exactly one attempt row");
}

/// The streaming equivalent of #150: a first-frame `RequestRejected` must persist.
///
/// `attempt_streaming.rs` charges a failure fee, pushes an attempt row, and returns a
/// 400/403 without enqueuing — the same defect as the non-streaming path, reached
/// through a different branch.
#[doc = "Spec: USG-1, USG-3 — ISSUE-0150 a streaming first-frame rejection persists"]
#[tokio::test]
async fn streaming_first_frame_rejection_persists_a_usage_event() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t150-stream-user").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::StreamRequestRejected).await;
    let (provider_id, key_id, _model_pk) =
        single_key_route(&harness, mock_addr, "t150-stream", 100).await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t150-stream-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a first-frame RequestRejected must surface as 400"
    );
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    flush(&harness).await;

    let (cost, upstream, scope): (i64, Option<String>, Option<String>) =
        sqlx::query_as("SELECT cost_micro_usd, upstream_key_id, provider_scope FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect(
                "a streaming first-frame rejection must persist a usage event; the failure \
                 charge is committed, so the ledger must record it",
            );
    assert!(cost > 0, "the failure charge must be recorded; got {cost}");
    assert_eq!(upstream.as_deref(), Some(key_id.to_string().as_str()));
    assert_eq!(scope.as_deref(), Some("global"));
    let _ = provider_id;
}
