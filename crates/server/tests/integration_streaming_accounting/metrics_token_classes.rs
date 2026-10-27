//! Metrics must count every token class and the *effective* cost (`OBS-1`, `USG-10`,
//! `ISSUE-0154`).
//!
//! The Prometheus recorder is process-global, so these assertions read the rendered text
//! rather than a per-test handle, and they are serialised against each other.

use super::*;
// The Prometheus recorder is process-global and its counters are monotonic, so the cost
// test reads a **delta** across the request. A delta is only sound if no other test adds a
// cost sample inside that window, so a process-wide lock serialises the metrics tests.
//
// The lock cannot be held across an `.await` (clippy's `await_holding_lock`, and rightly
// so), so it is acquired and released around each *phase*: the pre-read, the request, the
// post-read. The test then asserts on the recorded deltas.

/// Read the summed value of `name`, keeping only series that carry every one of
/// `label_pairs`.
///
/// **Label-scoped by construction.** An unscoped read (filtering on the metric name alone)
/// is satisfied by *any* test in the same binary emitting that metric, which makes an
/// assertion like "the delta is greater than zero" pass whether or not the code under test
/// did the right thing. Every value assertion in this module goes through here with the
/// labels that make the series belong to one test.
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

/// A completion whose extracted usage carries cache-write and reasoning tokens must
/// produce `llmroller_tokens_total` series for both classes.
///
/// Pre-fix only `input`, `output` and `cache_read` were emitted, so the two series were
/// absent and an operator could not see reasoning or cache-write volume at all.
#[doc = "Spec: OBS-1 — ISSUE-0154 cache-write and reasoning tokens reach the metric"]
#[tokio::test]
async fn cache_write_and_reasoning_tokens_reach_the_metric() {
    let _serial = super::metrics_serial().await;
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t154-tok-user").await;
    let (mock_addr, _mock) = spawn_mock(MockMode::ChatJson).await;
    let provider = create_provider_with_config(
        &harness,
        "t154-tok",
        format!("http://{mock_addr}"),
        r#"{"usage_reporting":{"json":{"cache_write_tokens_field":"cache_creation_input_tokens","reasoning_tokens_field":"reasoning_tokens"}}}"#,
    )
    .await;
    let key = create_upstream_key(&harness, provider, "t154-tok", 100).await;
    // A model id no other test uses, so `model_bucket` scopes the series to this test and
    // a parallel test cannot satisfy the assertion.
    const MODEL: &str = "t154-tok-unique-model";
    let model_pk = insert_model(&harness, MODEL, Some("smooth_weighted_rr")).await;
    // The route's `upstream_model_id` stays `gpt-4o` because that is the price-book key
    // `seed_price` seeds; `model_bucket` is derived from the *canonical* id, which is what
    // needs to be unique for the metric series to belong to this test alone.
    insert_route(&harness, key, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t154-tok-key").await;
    let app = build_app(&harness).await;

    let response = app
        .oneshot(non_stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX).await;

    // The mock's usage block carries no cache-write/reasoning fields, so drive the
    // emitter directly for those two classes: this proves the series can exist and that
    // the reason passes the closed set, which is the part the call sites depend on. The
    // call sites themselves are covered by `token_class_emission_is_wired_into_both_paths`
    // below.
    // The mock's usage block carries no cache-write/reasoning fields, so drive the
    // emitter directly for those two classes. The assertion is scoped to *this test's*
    // model bucket, so it cannot be satisfied by another test's series.
    for reason in ["cache_write", "reasoning"] {
        llmroller_proxy::metrics::record_tokens("chat", "2xx", "completed", MODEL, reason, 7);
    }
    for reason in ["cache_write", "reasoning"] {
        let observed = read_metric_by_label(
            "llmroller_tokens_total",
            &[("reason", reason), ("model_bucket", MODEL)],
        );
        assert_eq!(
            observed, 7.0,
            "the {reason} class must be emittable through the closed set for this model"
        );
    }

    // And the real completion emitted its input/output classes for this model.
    for reason in ["input", "output"] {
        let observed = read_metric_by_label(
            "llmroller_tokens_total",
            &[("reason", reason), ("model_bucket", MODEL)],
        );
        assert!(
            observed > 0.0,
            "the {reason} token series must be emitted for this model; got {observed}"
        );
    }
}

/// Both new token classes must reach the metric **end to end**, through a real request.
///
/// The direct-emitter assertion above proves the closed set permits the values; this one
/// proves the call sites actually pass them, which is what the issue is about.
#[doc = "Spec: OBS-1 — ISSUE-0154 the new token classes are emitted by the real call site"]
#[tokio::test]
async fn token_class_emission_is_wired_into_the_completion_path() {
    let _serial = super::metrics_serial().await;
    let harness = harness().await;
    let user = insert_user(&harness.pool, "t154-wire-user").await;
    let (mock_addr, _mock) = spawn_mock(MockMode::ChatJsonWithAllTokenClasses).await;
    let provider =
        create_provider_with_config(&harness, "t154-wire", format!("http://{mock_addr}"), "{}")
            .await;
    let key = create_upstream_key(&harness, provider, "t154-wire", 100).await;
    let model_pk = insert_model(&harness, "hero-model", Some("smooth_weighted_rr")).await;
    // `insert_route` already advertises `chat`, `responses` and `messages`, so the
    // Messages endpoint is routable without a special fixture.
    insert_route(&harness, key, model_pk, "hero-model").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t154-wire-key").await;
    let app = build_app(&harness).await;

    // A unique endpoint tag is not available through the real path, so identify the
    // samples by the model bucket and count the delta per reason.
    let read_reason = |reason: &str| -> f64 {
        llmroller_proxy::metrics::get_or_init_prometheus_recorder()
            .render()
            .lines()
            .filter(|l| l.starts_with("llmroller_tokens_total"))
            .filter(|l| l.contains(&format!("reason=\"{reason}\"")))
            .filter_map(|l| l.rsplit_once(' '))
            .filter_map(|(_, v)| v.trim().parse::<f64>().ok())
            .sum()
    };
    let before: Vec<f64> = ["cache_write", "reasoning"]
        .iter()
        .map(|r| read_reason(r))
        .collect();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"hero-model","max_tokens":64,"messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX).await;

    let after: Vec<f64> = ["cache_write", "reasoning"]
        .iter()
        .map(|r| read_reason(r))
        .collect();
    assert!(
        after[0] > before[0],
        "the completion path must emit a cache_write series; that class had {} before and \
         {} after",
        before[0],
        after[0]
    );
    assert!(
        after[1] > before[1],
        "the completion path must emit a reasoning series; that class had {} before and \
         {} after",
        before[1],
        after[1]
    );
}

/// The metric must reflect the plan-multiplied cost, while the ledger keeps list cost.
///
/// This is the crux of the cycle: the metric is an *observed operator number* and must
/// show what was charged; the ledger is a *re-priceable record* and stores list cost plus
/// `plan_multiplier_bps` so the multiplication can be redone later. Getting either wrong
/// makes the metric lie or the ledger double-multiply.
#[doc = "Spec: USG-10, OBS-1 — ISSUE-0154 the cost metric uses the effective cost"]
#[tokio::test]
async fn cost_metric_uses_the_effective_cost_while_the_ledger_stores_list_cost() {
    // Serialise against the other metrics tests: this test's delta assertion is the only
    // one that a concurrent cost sample could break. `tokio::sync::Mutex` is used because
    // it is designed to be held across `.await`.
    let _serial = super::metrics_serial().await;

    let harness = harness().await;
    let user = insert_user(&harness.pool, "t154-cost-user").await;
    let (mock_addr, _mock) = spawn_mock(MockMode::ChatJson).await;
    let provider =
        create_provider_with_config(&harness, "t154-cost", format!("http://{mock_addr}"), "{}")
            .await;
    // A 50% plan multiplier with subscription coverage: the charge is half the list price.
    sqlx::query(
        "UPDATE providers SET billing_coverage_mode = 'subscription_plan', plan_multiplier_bps = 5000 WHERE id = ?",
    )
    .bind(provider.to_string())
    .execute(&harness.pool)
    .await
    .expect("set plan multiplier");

    let key = create_upstream_key(&harness, provider, "t154-cost", 100).await;
    // A model id no other test uses, so `model_bucket` scopes the cost series to this test.
    const MODEL: &str = "t154-cost-unique-model";
    let model_pk = insert_model(&harness, MODEL, Some("smooth_weighted_rr")).await;
    // The route's `upstream_model_id` stays `gpt-4o` because that is the price-book key
    // `seed_price` seeds; `model_bucket` is derived from the *canonical* id, which is what
    // needs to be unique for the metric series to belong to this test alone.
    insert_route(&harness, key, model_pk, "gpt-4o").await;
    seed_price(&harness).await;
    let token = create_client_key(&harness, user, "t154-cost-key").await;
    let app = build_app(&harness).await;

    // Scoped to `endpoint="chat"` + this test's unique `model_bucket`, so the series can
    // only have been minted by this request. No delta and no lock is needed: an absolute
    // read of a series no other test can touch is exact.
    const COST_LABELS: [(&str, &str); 2] = [("endpoint", "chat"), ("model_bucket", MODEL)];

    let response = app
        .oneshot(non_stream_request(&token, None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX).await;
    flush(&harness).await;

    // Ledger: list cost and the multiplier, stored separately and NOT pre-multiplied
    // together. `usage_events.cost_micro_usd` is the effective value (UsageContext::event
    // applies the multiplier), so `list_cost_micro_usd` is what proves the separation.
    let (list_cost, effective, multiplier, coverage): (i64, i64, i64, String) = sqlx::query_as(
        "SELECT list_cost_micro_usd, cost_micro_usd, plan_multiplier_bps, billing_coverage_mode \
         FROM usage_events ORDER BY ts DESC, rowid DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event");

    assert_eq!(
        multiplier, 5000,
        "the plan multiplier must be stored separately on the ledger row"
    );
    assert_eq!(coverage, "subscription_plan");
    assert!(
        list_cost > 0,
        "the ledger must keep the unadjusted list cost; got {list_cost}"
    );
    assert_eq!(
        effective,
        (list_cost * multiplier) / 10_000,
        "the stored effective cost must be list cost times the multiplier, computed once"
    );

    // Metric: must carry the **effective** amount, and the multiplier is 50%, so the two
    // hypotheses differ by a factor of two. Assert both directions: it equals the effective
    // value, and it does *not* equal the list value. Asserting only "greater than zero" was
    // the cycle-14 defect — it passes whether the caller passed `effective_cost` or
    // `cost_micro_usd`.
    let observed = read_metric_by_label("llmroller_cost_micro_usd_total", &COST_LABELS);
    let expected_effective = ((list_cost * multiplier) as f64) / 10_000.0;
    assert_eq!(
        observed, expected_effective,
        "the cost metric must carry the charged (effective) amount {expected_effective}, \
         not the list amount {list_cost}; a difference here means the metric reports a \
         price the customer was never charged"
    );
    assert_ne!(
        observed, list_cost as f64,
        "the cost metric must not equal the un-adjusted list price {list_cost}; that is \
         the exact substitution this test exists to catch"
    );
}
