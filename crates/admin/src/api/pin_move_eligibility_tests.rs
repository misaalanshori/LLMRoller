//! Unit tests for the pin-move target eligibility decision.
//!
//! These drive [`judge`] directly rather than through SQL, because the decision table is a pure
//! function of six row values and the interesting cases are combinations a database fixture would
//! take far longer to build. The endpoint-level behaviour (that the handlers call this, and that a
//! rejection writes nothing) is covered by
//! `crates/server/tests/integration_pin_move_eligibility.rs`, which uses real rows.
//!
//! Both halves are needed: these pin the *decision*, that file pins the *wiring*. Cycle 39 showed
//! that a perfect extracted function with no caller passes every unit test.

use super::*;

const NOW: i64 = 1_000_000;

fn active_route() -> Option<(bool, String)> {
    Some((true, "active".to_string()))
}

#[test]
fn a_routed_enabled_healthy_key_is_eligible() {
    assert_eq!(judge(active_route(), true, "healthy", None, NOW), Ok(()));
}

#[test]
fn no_route_row_is_rejected_before_anything_else() {
    // Route existence is checked first so the message is the operator's most likely problem.
    // Even a disabled, quarantined key reports the missing route, not the key state.
    assert_eq!(
        judge(None, false, "invalid", None, NOW),
        Err(TargetIneligibility::NoRouteForModel)
    );
}

#[test]
fn a_disabled_key_is_rejected() {
    assert_eq!(
        judge(active_route(), false, "healthy", None, NOW),
        Err(TargetIneligibility::KeyDisabled)
    );
}

#[test]
fn a_quarantined_key_is_rejected_regardless_of_windows() {
    // `health = 'invalid'` carries no reset time: `KeyHealthEntry::is_routable` returns false
    // unconditionally, so a `health_until` in the past must NOT make it usable.
    assert_eq!(
        judge(active_route(), true, "invalid", Some(NOW - 1), NOW),
        Err(TargetIneligibility::KeyQuarantined)
    );
}

#[test]
fn an_exhausted_key_is_rejected_until_its_window_passes() {
    assert_eq!(
        judge(active_route(), true, "exhausted", Some(NOW + 5_000), NOW),
        Err(TargetIneligibility::KeyExhausted)
    );
    // A window that has already passed is genuinely routable again, matching `is_routable`.
    assert_eq!(
        judge(active_route(), true, "exhausted", Some(NOW - 1), NOW),
        Ok(())
    );
}

#[test]
fn an_exhausted_key_with_no_reset_time_is_rejected() {
    // `health_until IS NULL` with `exhausted` means "no known reset"; treating that as healthy
    // would be the same failure mode as a fabricated claim.
    assert_eq!(
        judge(active_route(), true, "exhausted", None, NOW),
        Err(TargetIneligibility::KeyExhausted)
    );
}

#[test]
fn a_cooling_down_key_is_rejected_until_its_window_passes() {
    assert_eq!(
        judge(active_route(), true, "cooling_down", Some(NOW + 1), NOW),
        Err(TargetIneligibility::KeyCoolingDown)
    );
    assert_eq!(
        judge(active_route(), true, "cooling_down", Some(NOW), NOW),
        Ok(())
    );
}

#[test]
fn a_disabled_route_is_rejected() {
    assert_eq!(
        judge(
            Some((false, "active".to_string())),
            true,
            "healthy",
            None,
            NOW
        ),
        Err(TargetIneligibility::RouteDisabled)
    );
}

#[test]
fn a_non_active_route_is_rejected() {
    for status in ["missing", "retired"] {
        assert_eq!(
            judge(Some((true, status.to_string())), true, "healthy", None, NOW),
            Err(TargetIneligibility::RouteNotActive),
            "route status {status} must fail C1 (route status must be active)"
        );
    }
}

#[test]
fn every_rejection_names_its_condition_and_carries_the_specified_code() {
    // `14.t.4` requires `422 target_route_ineligible`; each variant must also say *which*
    // condition failed, because a generic message sends the operator hunting.
    for variant in [
        TargetIneligibility::NoRouteForModel,
        TargetIneligibility::RouteDisabled,
        TargetIneligibility::RouteNotActive,
        TargetIneligibility::KeyDisabled,
        TargetIneligibility::KeyQuarantined,
        TargetIneligibility::KeyExhausted,
        TargetIneligibility::KeyCoolingDown,
    ] {
        let (status, body) = variant.reject();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body.0["error"]["code"], "target_route_ineligible");
        let message = body.0["error"]["message"]
            .as_str()
            .expect("message is a string");
        assert!(
            message.contains("target upstream key") || message.contains("route"),
            "message for {variant:?} must name the target or its route, got: {message}"
        );
    }
}

#[test]
fn the_not_checkable_list_is_not_empty_and_names_its_reasons() {
    // This constant is the honesty mechanism: it is what stops a later reader treating this
    // module as a full C1-C8 implementation. If it were ever emptied without the real predicate
    // becoming reachable, the module's doc comment would be a lie.
    assert!(!NOT_CHECKABLE_HERE.is_empty());
    for entry in NOT_CHECKABLE_HERE {
        assert!(
            entry.contains("C1")
                || entry.contains("C2")
                || entry.contains("C4")
                || entry.contains("C5")
                || entry.contains("C6")
                || entry.contains("C7")
                || entry.contains("C8"),
            "each unverifiable condition must name its check, got: {entry}"
        );
    }
}
