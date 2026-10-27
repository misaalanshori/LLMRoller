//! The 429 short-retry failure branch (`ISSUE-0152`) and terminal-event attribution
//! (`ISSUE-0153`).
//!
//! The retry fixtures hang on the second call, so every test is wrapped in a bound: a
//! hanging mock is the canonical hang-prone setup and hitting the bound is a measurement,
//! not a flake.

use super::*;

/// Build a provider whose send timeout is short enough to interrupt a hanging retry.
///
/// `first_activity_timeout_ms` bounds the send; 150 ms keeps the test fast while leaving
/// the 1 s `Retry-After` sleep comfortably dominant.
async fn short_timeout_provider(
    harness: &Harness,
    label: &str,
    addr: SocketAddr,
) -> (llmroller_core::ids::ProviderId, UpstreamKeyId) {
    let provider = create_provider_with_config(
        harness,
        label,
        format!("http://{addr}"),
        r#"{"timeouts":{"first_activity_timeout_ms":150,"nonstream_response_timeout_ms":150}}"#,
    )
    .await;
    let key = create_upstream_key(harness, provider, label, 100).await;
    (provider, key)
}

/// A retry that times out must surface as a timeout, not as the stale 429.
///
/// Pre-fix the `if let Ok(Ok(...))` has no `else`, so a timed-out retry leaves
/// `response`/`status` pointing at the **first** 429 and the client is told to back off
/// a key that is actually unresponsive.
#[doc = "Spec: RTE-8, USG-1 — ISSUE-0152 a failed short retry returns a timeout, not the stale 429"]
#[tokio::test]
async fn failed_short_retry_returns_a_timeout_not_the_stale_429() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t152-user").await;
    let (mock_addr, mock) = spawn_mock(MockMode::Retry429ThenHang).await;
    let (_provider, key_id) = short_timeout_provider(&harness, "t152", mock_addr).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t152-key").await;
    let app = build_app(&harness).await;

    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(non_stream_request(&token, None)),
    )
    .await
    .expect("the request must terminate; hitting this bound is a finding")
    .expect("response");

    assert_ne!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a timed-out retry must not be reported as the stale 429 — that mislabels a \
         hang as a rate limit and tells the client to back off an unresponsive key"
    );
    assert!(
        response.status().is_server_error(),
        "the retry timed out, so the client must see a 5xx; got {}",
        response.status()
    );
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        2,
        "the retry must have reached upstream exactly once"
    );
}

/// The failed retry must be recorded, so the ledger does not show one row for two calls.
#[doc = "Spec: USG-1, USG-3 — ISSUE-0152 a failed short retry records a second attempt"]
#[tokio::test]
async fn failed_short_retry_records_a_second_attempt() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t152-att-user").await;
    let (mock_addr, _mock) = spawn_mock(MockMode::Retry429ThenHang).await;
    let (_provider, key_id) = short_timeout_provider(&harness, "t152-att", mock_addr).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t152-att-key").await;
    let app = build_app(&harness).await;

    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(non_stream_request(&token, None)),
    )
    .await
    .expect("the request must terminate")
    .expect("response");
    let _ = response.into_body();
    flush(&harness).await;

    let mut outcomes: Vec<String> =
        sqlx::query_scalar("SELECT outcome FROM usage_attempts ORDER BY attempt_no")
            .fetch_all(&harness.pool)
            .await
            .expect("list attempts");
    outcomes.sort();
    assert_eq!(
        outcomes.len(),
        2,
        "two upstream calls must produce two attempt rows; got {outcomes:?}"
    );
    assert!(
        outcomes.iter().any(|o| o == "wait_timeout"),
        "the failed retry must be recorded as a timeout; got {outcomes:?}"
    );
}

/// A successful retry must still use the second response.
#[doc = "Spec: RTE-8 — ISSUE-0152 a successful short retry uses the second response"]
#[tokio::test]
async fn successful_short_retry_still_uses_the_second_response() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t152-ok-user").await;
    let (mock_addr, mock) = spawn_mock(MockMode::Retry429ThenOk).await;
    let (_provider, key_id) = short_timeout_provider(&harness, "t152-ok", mock_addr).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t152-ok-key").await;
    let app = build_app(&harness).await;

    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(non_stream_request(&token, None)),
    )
    .await
    .expect("the request must terminate")
    .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "429 then 200 must yield the 200"
    );
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    flush(&harness).await;
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        2,
        "the retry reached upstream exactly once"
    );

    let (failovers,): (i64,) = sqlx::query_as("SELECT failovers FROM usage_events")
        .fetch_one(&harness.pool)
        .await
        .expect("usage event");
    assert_eq!(
        failovers, 0,
        "a same-key retry is not a failover, so the counter must stay 0"
    );
}

/// The retry must not loop: 429 then 429 is two calls, not three.
#[doc = "Spec: RTE-8 — ISSUE-0152 the short retry is bounded to one retry (status-quo pin)"]
#[tokio::test]
async fn short_retry_is_still_bounded_to_one_attempt() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t152-pin-user").await;
    let (mock_addr, mock) = spawn_mock(MockMode::Retry429Always).await;
    let (_provider, key_id) = short_timeout_provider(&harness, "t152-pin", mock_addr).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t152-pin-key").await;
    let app = build_app(&harness).await;

    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(non_stream_request(&token, None)),
    )
    .await
    .expect("the request must terminate")
    .expect("response");
    let _ = response.into_body();
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        2,
        "429 then 429 is one retry; a third call would mean the retry loops"
    );
}

/// The terminal all-failed event must carry the resolved session hash.
#[doc = "Spec: USG-1, TRC-1 — ISSUE-0153 the terminal event carries the resolved session hash"]
#[tokio::test]
async fn terminal_event_carries_the_resolved_session_hash() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t153-user").await;
    let (mock_addr, _mock) = spawn_mock(MockMode::Retry429Always).await;
    let (_provider, key_id) = short_timeout_provider(&harness, "t153", mock_addr).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t153-key").await;
    let app = build_app(&harness).await;

    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(non_stream_request(&token, Some("t153-session-abc"))),
    )
    .await
    .expect("the request must terminate")
    .expect("response");
    let _ = response.into_body();
    flush(&harness).await;

    let session_hash: Option<String> = sqlx::query_scalar(
        "SELECT session_hash FROM usage_events ORDER BY ts DESC, rowid DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event");
    assert!(
        session_hash.is_some(),
        "a request that carried a session header must record the resolved hash, not NULL"
    );
}

/// Every terminal path must still emit exactly one event.
#[doc = "Spec: USG-1 — ISSUE-0152/0153 no double-emit on the all-failed path"]
#[tokio::test]
async fn all_attempts_failed_still_persists_exactly_one_event() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t153-dup-user").await;
    let (mock_addr, _mock) = spawn_mock(MockMode::Retry429Always).await;
    let (_provider, key_id) = short_timeout_provider(&harness, "t153-dup", mock_addr).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t153-dup-key").await;
    let app = build_app(&harness).await;

    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(non_stream_request(&token, None)),
    )
    .await
    .expect("the request must terminate")
    .expect("response");
    let _ = response.into_body();
    flush(&harness).await;

    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&harness.pool)
        .await
        .expect("count events");
    assert_eq!(
        events, 1,
        "an all-attempts-failed request must produce exactly one event"
    );
}

/// The terminal all-failed event must also carry the real pin outcome.
#[doc = "Spec: USG-1, TRC-1 — ISSUE-0153 the terminal event carries the pin outcome"]
#[tokio::test]
async fn terminal_event_carries_the_pin_outcome() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t153-pin-user").await;
    let (mock_addr, _mock) = spawn_mock(MockMode::Retry429Always).await;
    let (_provider, key_id) = short_timeout_provider(&harness, "t153-pin", mock_addr).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t153-pin-key").await;
    let app = build_app(&harness).await;

    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(non_stream_request(&token, Some("t153-pin-session-abc"))),
    )
    .await
    .expect("the request must terminate")
    .expect("response");
    let _ = response.into_body();
    flush(&harness).await;

    let pin_outcome: Option<String> = sqlx::query_scalar(
        "SELECT pin_outcome FROM usage_events ORDER BY ts DESC, rowid DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event");
    // The vocabulary is `new_pin | followed | repin | repin_recovery |
    // unpinned_capacity | response_link` (`PinOutcome::as_str`). A fresh session must
    // produce exactly one of those, never a synthesised string.
    let outcome = pin_outcome
        .expect("a request whose session acquired a pin must record the outcome, not NULL");
    const VOCABULARY: [&str; 6] = [
        "new_pin",
        "followed",
        "repin",
        "repin_recovery",
        "unpinned_capacity",
        "response_link",
    ];
    assert!(
        VOCABULARY.contains(&outcome.as_str()),
        "pin_outcome must come from the real vocabulary, got {outcome:?}"
    );
}
