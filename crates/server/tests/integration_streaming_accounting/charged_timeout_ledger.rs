//! A charged timeout must land in the usage ledger (`USG-1`, `USG-3`, `ISSUE-0149`).
//!
//! On the `timeout_failover == false` branch the handler commits `charged_micro_usd`
//! to the client-key and cap guards and then returns 504 — without enqueuing a usage
//! event or its attempts. Spend therefore existed with no ledger entry, so `/usage`
//! under-reported and the per-key spend view diverged permanently from what was
//! charged.
//!
//! Split out of `integration_streaming_accounting.rs` to stay under the 1,600-line
//! ceiling (`MNT-3`); the harness is imported from the parent module.

use super::*;

/// Configure a provider whose sends time out quickly, with `timeout_failover`
/// controlled by the caller.
///
/// Both timeout knobs are set because the two request shapes use different ones: the
/// non-streaming send is bounded by `first_activity_timeout_ms`, while the streaming
/// first-frame wait is bounded by `first_valid_frame_timeout_ms`
/// (`attempt_streaming.rs`'s `frame_timeout`).
async fn timeout_provider(
    harness: &Harness,
    label: &str,
    addr: SocketAddr,
    timeout_failover: bool,
) -> (llmroller_core::ids::ProviderId, UpstreamKeyId) {
    let provider = create_provider_with_config(
        harness,
        label,
        format!("http://{addr}"),
        &format!(
            r#"{{"timeouts":{{"first_activity_timeout_ms":100,"first_valid_frame_timeout_ms":100,"timeout_failover":{timeout_failover}}}}}"#
        ),
    )
    .await;
    let key = create_upstream_key(harness, provider, label, 100).await;
    (provider, key)
}

/// A timed-out request must produce a usage event carrying the charged amount.
#[doc = "Spec: USG-1, USG-3 — ISSUE-0149 a charged timeout persists a usage event"]
#[tokio::test]
async fn timeout_with_failover_disabled_persists_a_usage_event() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t149-user").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::SleepDelay(2000)).await;
    let (_, key_id) = timeout_provider(&harness, "t149-prov", mock_addr, false).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t149-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "a first-activity timeout with failover disabled must abort with 504"
    );
    flush(&harness).await;

    let (cost, termination, status): (i64, String, i64) =
        sqlx::query_as("SELECT cost_micro_usd, termination, status FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect(
                "a charged timeout must persist a usage event; without one the spend exists \
                 with no ledger entry",
            );
    assert_eq!(status, 504);
    assert!(
        cost > 0,
        "the persisted event must carry the amount actually charged, not 0; got {cost}"
    );
    assert_eq!(
        termination, "idle_timeout",
        "the event must record the real termination, not a generic truncation"
    );

    let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_attempts")
        .fetch_one(&harness.pool)
        .await
        .expect("count attempts");
    assert_eq!(
        attempts, 1,
        "the attempt row for the timed-out request must be persisted too"
    );
}

/// The charge must be attributed to the real key and provider, not to `"none"`.
#[doc = "Spec: USG-1, USG-9 — ISSUE-0149 a charged timeout is attributed to the real key"]
#[tokio::test]
async fn timeout_with_failover_disabled_attributes_the_charge_to_the_real_key() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t149-attr-user").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::SleepDelay(2000)).await;
    let (provider_id, key_id) = timeout_provider(&harness, "t149-attr", mock_addr, false).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t149-attr-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    flush(&harness).await;

    let (upstream_key_id, provider, scope): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT upstream_key_id, provider_id, provider_scope FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect("the charged timeout must have a ledger row to attribute");

    assert_eq!(
        upstream_key_id.as_deref(),
        Some(key_id.to_string().as_str()),
        "the charge must be attributed to the upstream key that was actually called"
    );
    assert_eq!(
        provider.as_deref(),
        Some(provider_id.to_string().as_str()),
        "the charge must name the real provider"
    );
    assert_eq!(
        scope.as_deref(),
        Some("global"),
        "the fixture provider is created global (`create_provider`), so the label must be \
         the real tier \"global\", not \"none\""
    );
}

/// With failover enabled the request still fails over and emits exactly one event.
///
/// This is the double-emit guard: now that the abort path also enqueues, a mistake
/// could produce two rows for one request.
#[doc = "Spec: USG-1, RTE-9 — ISSUE-0149 failover enabled still emits exactly one event"]
#[tokio::test]
async fn timeout_with_failover_enabled_still_fails_over_and_persists_once() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t149-fo-user").await;
    let (slow_addr, slow_state) = spawn_mock(MockMode::SleepDelay(2000)).await;
    let (fast_addr, fast_state) = spawn_mock(MockMode::StreamOk).await;
    let (_, slow_key) = timeout_provider(&harness, "t149-slow", slow_addr, true).await;
    let fast_provider = create_provider(&harness, "t149-fast", format!("http://{fast_addr}")).await;
    let fast_key = create_upstream_key(&harness, fast_provider, "t149-fast", 1).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, slow_key, model_pk, "gpt-4o").await;
    insert_route(&harness, fast_key, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t149-fo-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "with timeout_failover enabled the request must fail over and succeed"
    );
    // A streaming usage event is enqueued when the stream *finishes*, so the body has
    // to be drained before the ledger can be inspected.
    let _ = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("drain stream");
    flush(&harness).await;

    assert_eq!(
        slow_state.calls.load(Ordering::SeqCst),
        1,
        "the slow provider must be attempted once"
    );
    assert_eq!(
        fast_state.calls.load(Ordering::SeqCst),
        1,
        "the request must fail over to the fast provider"
    );

    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&harness.pool)
        .await
        .expect("count events");
    assert_eq!(
        events, 1,
        "a failed-over request must produce exactly one usage event, not one per attempt"
    );
}

/// A streaming request that times out must also land in the ledger.
///
/// With this fixture the abort happens inside `send()` — `first_activity_timeout_ms` is
/// 100 ms and the mock sleeps 2000 ms before answering — so the request is refused
/// before `attempt_streaming.rs`'s first-frame loop ever runs. This test therefore
/// exercises the **same** branch as the non-streaming cases, which is why it goes red
/// when the fix is reverted. The streaming first-frame abort at
/// `attempt_streaming.rs` remains a separate path that this mock cannot reach; it is
/// reported rather than claimed as covered.
#[doc = "Spec: USG-1, USG-3 — ISSUE-0149 a charged streaming timeout persists a usage event"]
#[tokio::test]
async fn streaming_timeout_with_failover_disabled_persists_a_usage_event() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t149-stream-user").await;
    let (mock_addr, _mock_state) = spawn_mock(MockMode::SleepDelay(2000)).await;
    let (_, key_id) = timeout_provider(&harness, "t149-stream", mock_addr, false).await;
    let model_pk = insert_model(&harness, "gpt-4o", Some("smooth_weighted_rr")).await;
    insert_route(&harness, key_id, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t149-stream-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "a streaming timeout with failover disabled must abort with 504"
    );
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    flush(&harness).await;

    let (cost, upstream, scope): (i64, Option<String>, Option<String>) =
        sqlx::query_as("SELECT cost_micro_usd, upstream_key_id, provider_scope FROM usage_events")
            .fetch_one(&harness.pool)
            .await
            .expect(
                "a charged streaming timeout must persist a usage event; without one the spend \
                 exists with no ledger entry",
            );
    assert!(
        cost > 0,
        "the event must carry the charged amount; got {cost}"
    );
    assert_eq!(
        upstream.as_deref(),
        Some(key_id.to_string().as_str()),
        "the charge must be attributed to the key that was actually called"
    );
    assert_eq!(scope.as_deref(), Some("global"));
}
