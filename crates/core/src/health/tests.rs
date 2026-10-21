//! Tests for key/route health machine, credential strike verifier, and OriginBreaker.
//!
//! Spec: Section 6.8, Section 7.2, Section 10.5, Section 10.5.1, Section 21.5,
//! Requirements: API-8, REL-5.

use super::*;
use crate::breaker::{BreakerState, OriginBreaker};

#[doc = "Spec: API-8, REL-5"]
#[test]
fn test_credential_strike_verification_and_probe_clearing() {
    let verifier = CredentialStrikeVerifier::new();
    let key_id = UpstreamKeyId::generate();
    let client1 = ClientKeyId::generate();
    let client2 = ClientKeyId::generate();

    let t0 = 1_000_000_000_000;

    // 1. First strike from client 1 -> ProbeRequired (does not immediately mark invalid)
    let res1 = verifier.record_strike(key_id, client1, t0);
    assert_eq!(res1, StrikeResult::ProbeRequired);

    // Repeated strikes from SAME client 1 within 10 min still return ProbeRequired
    let res1_repeat = verifier.record_strike(key_id, client1, t0 + 60_000);
    assert_eq!(res1_repeat, StrikeResult::ProbeRequired);

    // 2. Verification probe succeeds (2xx) -> clears strikes! Key becomes healthy
    let probe_clear = verifier.record_probe_result(key_id, true);
    assert_eq!(probe_clear, StrikeResult::Healthy);

    // Strike after clearing starts fresh
    let res_fresh = verifier.record_strike(key_id, client1, t0 + 120_000);
    assert_eq!(res_fresh, StrikeResult::ProbeRequired);

    // 3. Second strike from a DISTINCT client key (client 2) within 10 min -> MarkInvalid!
    let res2 = verifier.record_strike(key_id, client2, t0 + 180_000);
    assert_eq!(res2, StrikeResult::MarkInvalid);

    // 4. Failing verification probe (401/403) also marks key invalid immediately
    let probe_fail = verifier.record_probe_result(key_id, false);
    assert_eq!(probe_fail, StrikeResult::MarkInvalid);
}

#[doc = "Spec: API-8, REL-5"]
#[test]
fn test_route_protocol_blocks_and_aux_endpoints() {
    let key = UpstreamKeyId::generate();
    let model = ModelPk::generate();
    let t0 = 1_000_000_000_000;

    // Protocol blocks (e.g. Command Code upgrade_required on messages)
    let proto_blocks = RouteProtocolBlocks::new();
    assert!(!proto_blocks.is_blocked(key, model, "messages", t0));

    // Block "messages" until t0 + 3600s (1 hour)
    proto_blocks.block_protocol(key, model, "messages", t0 + 3_600_000);
    // "messages" is blocked:
    assert!(proto_blocks.is_blocked(key, model, "messages", t0 + 1000));
    // "chat" remains eligible!
    assert!(!proto_blocks.is_blocked(key, model, "chat", t0 + 1000));
    // After expiry, "messages" becomes unblocked:
    assert!(!proto_blocks.is_blocked(key, model, "messages", t0 + 3_600_001));

    // Auxiliary endpoints (e.g. count_tokens 404 for 6 hours)
    let aux = AuxUnsupported::new();
    assert!(!aux.is_unsupported(key, "count_tokens", t0));
    aux.mark_unsupported(key, "count_tokens", t0 + 6 * 3_600_000);
    assert!(aux.is_unsupported(key, "count_tokens", t0 + 1000));
    assert!(!aux.is_unsupported(key, "responses_compact", t0 + 1000));
}

#[doc = "Spec: API-8, REL-5"]
#[test]
fn test_origin_breaker_thresholds_and_probe_lifecycle() {
    let mut breaker = OriginBreaker::new();
    let t0 = 1_000_000_000_000;

    // 1. Initial state is Closed
    assert!(breaker.is_admitted(t0));
    assert_eq!(breaker.state, BreakerState::Closed);

    // 2. 5 consecutive host failures trip breaker to Open
    for i in 0..4 {
        breaker.record_host_failure(t0 + i * 1000);
        assert_eq!(breaker.state, BreakerState::Closed);
    }
    breaker.record_host_failure(t0 + 4000);
    match breaker.state {
        BreakerState::Open { until_ms } => {
            assert_eq!(until_ms, t0 + 4000 + 15_000);
        }
        other => panic!("expected Open, got {other:?}"),
    }

    // While Open before until_ms: request is rejected
    assert!(!breaker.is_admitted(t0 + 10_000));
    assert_eq!(breaker.try_acquire(t0 + 10_000), None);

    // At or after until_ms (t0 + 19_000): peek admits, and acquiring claims the
    // single half-open probe slot.
    assert!(breaker.is_admitted(t0 + 19_000));
    assert_eq!(breaker.try_acquire(t0 + 19_000), Some(true));
    assert_eq!(
        breaker.state,
        BreakerState::HalfOpen {
            probe_in_flight: true
        }
    );

    // Second request while probe is in flight is rejected
    assert!(!breaker.is_admitted(t0 + 19_100));
    assert_eq!(breaker.try_acquire(t0 + 19_100), None);

    // Probe succeeds -> breaker returns to Closed!
    breaker.record_success(t0 + 19_500);
    assert_eq!(breaker.state, BreakerState::Closed);
    assert_eq!(breaker.open_streak, 0);
    assert_eq!(breaker.consecutive_host_failures, 0);

    // 3. 50% failure ratio over >= 20 requests in 60 seconds trips breaker
    for i in 0..10 {
        breaker.record_success(t0 + 30_000 + i * 1000);
    }
    for i in 10..20 {
        breaker.record_host_failure(t0 + 30_000 + i * 1000);
    }
    // 10 successes + 10 failures = 20 attempts, 50% failure ratio -> Open!
    assert!(matches!(breaker.state, BreakerState::Open { .. }));
}

#[doc = "Spec: API-8, REL-5"]
#[test]
fn test_versioned_health_write_guard() {
    // Current health_version is 5
    let current_version = 5;

    // Stale data-plane write with version 4 is rejected
    assert!(!is_health_write_valid(current_version, 4));

    // Up-to-date write with version 5 is accepted
    assert!(is_health_write_valid(current_version, 5));

    // Newer write with version 6 is accepted
    assert!(is_health_write_valid(current_version, 6));
}
