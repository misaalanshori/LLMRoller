//! Runtime health, credential-strike, and origin-breaker regressions.
//!
//! Spec: Section 6.3, Section 10.5.1, Section 21.5,
//! Requirements: API-8, REL-5. Regression: F04.

use super::*;
use crate::breaker::BreakerState;
use crate::health::StrikeResult;
use crate::ids::{ClientKeyId, UpstreamKeyId};
use crate::usage::AttemptOutcome;

const T0: i64 = 1_000_000_000_000;

fn origin() -> OriginKey {
    OriginKey::from_base_url("https://api.example.com/v1", false)
}

#[test]
#[doc = "Spec: API-8 — F04 a single 401/403 only cools the key down"]
fn single_strike_applies_cooldown_not_permanent_quarantine() {
    let runtime = RuntimeState::new();
    let origin = origin();
    let key = UpstreamKeyId::generate();
    let client = ClientKeyId::generate();

    let decision =
        runtime.record_upstream_failure(&origin, key, client, AttemptOutcome::Http403, None, T0);

    assert_eq!(decision, Some(StrikeResult::ProbeRequired));
    let health = runtime.key_health_entry(key);
    assert!(health.healthy, "a first strike must not quarantine the key");
    assert_eq!(health.quarantine_reason, None);
    assert_eq!(health.cooldown_until_ms, Some(T0 + 15_000));
    // Even a day later the key is routable; the cooldown has simply expired.
    assert!(runtime.is_key_routable(key, T0 + 86_400_000));
}

#[test]
#[doc = "Spec: API-8 — F04 two distinct-client strikes quarantine the key"]
fn two_strikes_from_distinct_clients_quarantine_the_key() {
    let runtime = RuntimeState::new();
    let origin = origin();
    let key = UpstreamKeyId::generate();
    let first = ClientKeyId::generate();
    let second = ClientKeyId::generate();

    let first_decision =
        runtime.record_upstream_failure(&origin, key, first, AttemptOutcome::Http401, None, T0);
    assert_eq!(first_decision, Some(StrikeResult::ProbeRequired));

    let second_decision = runtime.record_upstream_failure(
        &origin,
        key,
        second,
        AttemptOutcome::Http401,
        None,
        T0 + 30_000,
    );
    assert_eq!(second_decision, Some(StrikeResult::MarkInvalid));
    let health = runtime.key_health_entry(key);
    assert!(!health.healthy);
    assert_eq!(
        health.quarantine_reason.as_deref(),
        Some("credential_rejected")
    );
}

#[test]
#[doc = "Spec: API-8 — F04 a failing probe quarantines; a passing probe clears the cooldown"]
fn verification_probe_outcomes_drive_key_health() {
    let runtime = RuntimeState::new();
    let origin = origin();
    let failed_key = UpstreamKeyId::generate();
    let recovered_key = UpstreamKeyId::generate();
    let client = ClientKeyId::generate();

    let _ = runtime.record_upstream_failure(
        &origin,
        failed_key,
        client,
        AttemptOutcome::Http401,
        None,
        T0,
    );
    assert_eq!(
        runtime.record_credential_probe_result(failed_key, false),
        StrikeResult::MarkInvalid
    );
    assert!(!runtime.key_health_entry(failed_key).healthy);
    assert_eq!(
        runtime
            .key_health_entry(failed_key)
            .quarantine_reason
            .as_deref(),
        Some("credential_rejected")
    );

    let _ = runtime.record_upstream_failure(
        &origin,
        recovered_key,
        client,
        AttemptOutcome::Http403,
        None,
        T0,
    );
    assert_eq!(
        runtime.record_credential_probe_result(recovered_key, true),
        StrikeResult::Healthy
    );
    let health = runtime.key_health_entry(recovered_key);
    assert!(health.healthy);
    assert_eq!(health.cooldown_until_ms, None);
}

#[test]
#[doc = "Spec: REL-5, Section 21.5 — F04 dropping an unfinished probe releases the slot"]
fn aborted_half_open_probe_releases_slot_via_raii_guard() {
    let runtime = RuntimeState::new();
    let origin = origin();
    let key = UpstreamKeyId::generate();
    let client = ClientKeyId::generate();

    for _ in 0..5 {
        let _ = runtime.record_upstream_failure(
            &origin,
            key,
            client,
            AttemptOutcome::ConnectError,
            None,
            T0,
        );
    }
    let breaker = runtime
        .origin_breakers
        .get(&origin)
        .expect("breaker")
        .clone();
    assert!(matches!(breaker.lock().state, BreakerState::Open { .. }));

    // Eligibility peeking must not claim the probe slot.
    assert!(runtime.is_origin_admitted(&origin, T0 + 15_001));
    assert!(matches!(breaker.lock().state, BreakerState::Open { .. }));

    // Acquire the probe and drop it without recording any outcome, simulating a
    // client disconnect or task cancellation.
    let permit = runtime
        .acquire_origin_permit(&origin, T0 + 15_001)
        .expect("probe slot must be claimable");
    drop(permit);
    assert_eq!(
        breaker.lock().state,
        BreakerState::HalfOpen {
            probe_in_flight: false
        }
    );

    // The next request can probe again instead of wedging forever.
    let retry = runtime
        .acquire_origin_permit(&origin, T0 + 15_002)
        .expect("released probe slot must be reusable");
    runtime.record_upstream_success(&origin, key, None, None, T0 + 15_003);
    drop(retry);
    assert_eq!(breaker.lock().state, BreakerState::Closed);
}

/// The client-key TPM window must not reset mid-burst.
///
/// `try_consume_client_key_tpm` keys its window on `now_ms.div_euclid(60_000)`, so a
/// request that lands just before a minute boundary and one that lands just after are
/// counted in *different* windows. Two requests in the same burst then both pass a limit
/// they should jointly exceed.
///
/// This is a wall-clock race, so it surfaces as an intermittent CI failure rather than a
/// deterministic one — cycle 17 hit it on `aarch64-apple-darwin` in
/// `regression_test_f040_client_key_tpm_rate_limiting`, where the second of two 25-token
/// requests against a 30 TPM limit returned 503 (no eligible route) instead of 429,
/// because the TPM gate had reset between them. Driving `now_ms` explicitly makes the
/// race reproducible instead of luck-dependent.
#[test]
fn client_key_tpm_window_must_not_reset_at_a_minute_boundary() {
    let state = RuntimeState::new();
    let key = ClientKeyId::generate();
    let limit = Some(30);
    // 1 ms before a minute boundary, and 1 ms after it: the same burst.
    let before_boundary = 59_999;
    let after_boundary = 60_001;

    assert_eq!(
        state.try_consume_client_key_tpm(key, 25, limit, before_boundary),
        Ok(()),
        "the first request of the burst is under the limit"
    );
    assert_eq!(
        state.try_consume_client_key_tpm(key, 25, limit, after_boundary),
        Err(60),
        "25 + 25 exceeds a 30 TPM limit even across a minute boundary; a window keyed on \
         div_euclid(60_000) forgets the first request and lets the burst through"
    );
}
