//! Billing attribution on the state-bound pass-through path (`ISSUE-0130`).
//!
//! # The defect
//!
//! `forward_state_bound_response_op` attributes usage on two paths:
//!
//! - **decode failure** — cycle 16's fix (`ISSUE-0156`), which calls
//!   `provider_billing(&snapshot, &key.provider_id)`;
//! - **success / upstream-error pass-through** — which used the *defaults*
//!   `plan_multiplier_bps: 10_000` and `billing_coverage_mode: "pay_as_you_go"`.
//!
//! So one handler honoured provider billing on a corrupt body and ignored it on a normal
//! upstream error. That is worse than a plain hardcode: a reader of the decode branch would
//! conclude the handler honours the provider's plan, and be wrong about the common path.
//!
//! `USG-10` requires `plan_multiplier_bps` and `billing_coverage_mode` to reflect the
//! serving provider. Both paths must agree, which is what
//! [`state_bound_decode_and_pass_through_agree_on_billing`] pins.

use super::state_bound_fixture::*;
use llmroller_store::providers::set_provider_billing_plan;

/// A provider on a subscription plan at 0.7321x list.
///
/// Both values are deliberately *implausible as constants*: `5_000` (a clean "0.5x") turned
/// out to let a mutant survive, because a hardcoded `5_000` is indistinguishable from a
/// correctly-read `5_000`. A value nobody would type kills that class of mutant.
const PLAN_MULTIPLIER_BPS: i64 = 7_321;
const PLAN_COVERAGE_MODE: &str = "subscription_plan";

/// A second, deliberately different plan, used to prove the value is read *per provider*
/// rather than being one shared constant.
const OTHER_PLAN_MULTIPLIER_BPS: i64 = 3_117;
const OTHER_PLAN_COVERAGE_MODE: &str = "free_tier";

/// Put the harness's provider on a subscription plan.
async fn set_subscription_plan(harness: &Harness, provider_id: llmroller_core::ids::ProviderId) {
    set_provider_billing_plan(
        &harness.pool,
        &harness.system_actor,
        provider_id,
        PLAN_COVERAGE_MODE,
        PLAN_MULTIPLIER_BPS,
    )
    .await
    .expect("set provider billing plan");
}

/// Read the newest usage row's billing columns for the `responses` endpoint.
async fn newest_billing(harness: &Harness) -> (String, i64, Option<String>) {
    let row: (String, i64, Option<String>) = sqlx::query_as(
        "SELECT billing_coverage_mode, plan_multiplier_bps, error_code \
         FROM usage_events WHERE endpoint = 'responses' ORDER BY ts DESC, rowid DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("usage event row");
    row
}

/// Primary red for #130. Pre-fix: `plan_multiplier_bps = 10_000`, mode `pay_as_you_go`.
#[tokio::test]
#[doc = "Spec: USG-10 — ISSUE-0130 the pass-through path uses the provider's billing plan"]
async fn state_bound_pass_through_uses_the_provider_billing_mode() {
    let harness = harness().await;
    let user = insert_user(&harness.pool, "billing-pt-user").await;
    let token = create_client_key(&harness, user, "billing-pt-key").await;

    // `create_link` builds the provider; take its id back so the plan can be set before the
    // snapshot is loaded (the snapshot is what `provider_billing` reads).
    let (id, key_id, _mock) = create_link(&harness, &token, false).await;
    let provider_id: String =
        sqlx::query_scalar("SELECT provider_id FROM upstream_keys WHERE id = ?")
            .bind(key_id.to_string())
            .fetch_one(&harness.pool)
            .await
            .expect("provider id");
    let provider_id = provider_id.parse().expect("provider id parses");
    set_subscription_plan(&harness, provider_id).await;

    let app = build_app(&harness).await;

    // The mock answers `200` with a valid body for the state-bound read, so the request
    // succeeds and takes the pass-through path this test is about.
    let (status, _body) = get_response(app, &token, &id).await;
    assert_eq!(status, StatusCode::OK, "the pass-through must succeed");
    flush(&harness).await;

    let (mode, multiplier, error_code) = newest_billing(&harness).await;
    assert_eq!(
        multiplier, PLAN_MULTIPLIER_BPS,
        "the row must carry the provider's plan multiplier, not the 10_000 default; \
         row: mode={mode} multiplier={multiplier} error_code={error_code:?}"
    );
    assert_eq!(
        mode, PLAN_COVERAGE_MODE,
        "the row must carry the provider's coverage mode, not `pay_as_you_go`"
    );
}

/// The cross-check that stops the asymmetry from reappearing.
///
/// Both failure shapes from the *same* provider must produce the same billing fields. A fix
/// applied to one path and not the other is a passing state without this test — which is
/// exactly how the defect survived cycle 16.
#[tokio::test]
#[doc = "Spec: USG-10 — ISSUE-0130 both state-bound paths agree on billing"]
async fn state_bound_decode_and_pass_through_agree_on_billing() {
    // --- Pass-through: upstream answers 500 with a decodable body. ---
    let pass_through = {
        let harness = harness().await;
        let user = insert_user(&harness.pool, "billing-agree-user").await;
        let token = create_client_key(&harness, user, "billing-agree-key").await;
        let (id, key_id, _mock) = create_link(&harness, &token, false).await;
        let provider_id: String =
            sqlx::query_scalar("SELECT provider_id FROM upstream_keys WHERE id = ?")
                .bind(key_id.to_string())
                .fetch_one(&harness.pool)
                .await
                .expect("provider id");
        set_subscription_plan(&harness, provider_id.parse().expect("parses")).await;

        let app = build_app(&harness).await;
        let (status, _body) = get_response(app, &token, &id).await;
        assert_eq!(status, StatusCode::OK);
        flush(&harness).await;
        let billing = newest_billing(&harness).await;
        (format!("pass-through({status})"), billing)
    };

    // --- Decode failure: upstream claims gzip and delivers non-gzip bytes. ---
    let decode_failure = {
        let harness = harness().await;
        let user = insert_user(&harness.pool, "billing-agree-df-user").await;
        let token = create_client_key(&harness, user, "billing-agree-df-key").await;
        let (id, key_id, _mock) = create_link(&harness, &token, true).await;
        let provider_id: String =
            sqlx::query_scalar("SELECT provider_id FROM upstream_keys WHERE id = ?")
                .bind(key_id.to_string())
                .fetch_one(&harness.pool)
                .await
                .expect("provider id");
        set_subscription_plan(&harness, provider_id.parse().expect("parses")).await;

        let app = build_app(&harness).await;
        let (status, _body) = get_response(app, &token, &id).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        flush(&harness).await;
        let billing = newest_billing(&harness).await;
        (format!("decode-failure({status})"), billing)
    };

    let (pass_through_label, (pass_mode, pass_multiplier, pass_error)) = pass_through;
    let (decode_label, (decode_mode, decode_multiplier, decode_error)) = decode_failure;

    // Sanity: the two shapes really took different branches, so this is not comparing a
    // path against itself.
    assert_ne!(
        pass_error, decode_error,
        "the two fixtures must reach different error branches or the agreement is vacuous"
    );

    assert_eq!(
        pass_mode, decode_mode,
        "{pass_through_label} and {decode_label} disagree on billing_coverage_mode"
    );
    assert_eq!(
        pass_multiplier, decode_multiplier,
        "{pass_through_label} and {decode_label} disagree on plan_multiplier_bps"
    );
    assert_eq!(
        pass_multiplier, PLAN_MULTIPLIER_BPS,
        "both paths must report the provider's plan, not the default"
    );
}

/// The billing fields must come from the serving provider's row, not from any constant.
///
/// This is the test that separates "reads the setting" from "happens to write the right
/// number". Two providers are given different plans; each request's row must carry its own
/// provider's plan. A single shared constant — even a plausible one — fails here.
#[tokio::test]
#[doc = "Spec: USG-10 — ISSUE-0130 billing is read per provider, not from a constant"]
async fn state_bound_billing_is_read_per_provider() {
    let mut observed = Vec::new();

    for (index, (mode, multiplier)) in [
        (PLAN_COVERAGE_MODE, PLAN_MULTIPLIER_BPS),
        (OTHER_PLAN_COVERAGE_MODE, OTHER_PLAN_MULTIPLIER_BPS),
    ]
    .into_iter()
    .enumerate()
    {
        let harness = harness().await;
        let user = insert_user(&harness.pool, &format!("billing-per-prov-{index}")).await;
        let token =
            create_client_key(&harness, user, &format!("billing-per-prov-key-{index}")).await;
        let (id, key_id, _mock) = create_link(&harness, &token, false).await;
        let provider_id: String =
            sqlx::query_scalar("SELECT provider_id FROM upstream_keys WHERE id = ?")
                .bind(key_id.to_string())
                .fetch_one(&harness.pool)
                .await
                .expect("provider id");
        set_provider_billing_plan(
            &harness.pool,
            &harness.system_actor,
            provider_id.parse().expect("parses"),
            mode,
            multiplier,
        )
        .await
        .expect("set plan");

        let app = build_app(&harness).await;
        let (status, _body) = get_response(app, &token, &id).await;
        assert_eq!(status, StatusCode::OK);
        flush(&harness).await;
        let (got_mode, got_multiplier, _err) = newest_billing(&harness).await;
        observed.push((mode, multiplier, got_mode, got_multiplier));
    }

    for (want_mode, want_multiplier, got_mode, got_multiplier) in &observed {
        assert_eq!(
            got_multiplier, want_multiplier,
            "provider on {want_mode} at {want_multiplier} bps was recorded at {got_multiplier} bps"
        );
        assert_eq!(
            got_mode, want_mode,
            "provider on {want_mode} was recorded as {got_mode}"
        );
    }

    // The two providers really were told apart, so this cannot pass with one shared value.
    assert_ne!(
        observed[0].3, observed[1].3,
        "the two providers' multipliers must differ in the ledger, or the value is a constant"
    );
}
