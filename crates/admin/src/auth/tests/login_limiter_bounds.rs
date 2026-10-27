//! Bounded failure-map tests for the three-tier login limiter (`AUTH-8`, `SEC-12`,
//! `ISSUE-0059`).
//!
//! All three maps are keyed by attacker-controlled values — source IP, arbitrary
//! `(username, IP)` pairs, arbitrary usernames — so all three must be bounded.
//! `check()` had a weak size guard that dropped only empty vectors;
//! `record_failure()` had none at all, which is the hole these tests pin.
//!
//! Every case drives an explicit `now_ms`, so nothing depends on wall-clock time.

use std::net::{IpAddr, Ipv4Addr};

use crate::auth::{
    AuthError, LimiterOutcome, MAX_FAILURES_PER_IP_15M, MAX_FAILURES_PER_USER_IP_15M,
    MAX_TRACKED_LIMITER_KEYS, ThreeTierLoginLimiter, USERNAME_ATTACK_THRESHOLD_1H,
};

const T0: i64 = 1_700_000_000_000;

fn ip(index: u32) -> IpAddr {
    IpAddr::V4(Ipv4Addr::from(index.to_be_bytes()))
}

/// Fill all three maps past the bound using only `record_failure`.
///
/// `record_failure` is the unconditional insertion point and is reachable after the
/// CSRF gate, so an attacker can grow every map without ever calling `check`.
#[test]
#[doc = "Spec: AUTH-8, SEC-12 — ISSUE-0059 login limiter bounds all three maps under a flood"]
fn login_limiter_bounds_all_three_maps_under_a_flood() {
    let limiter = ThreeTierLoginLimiter::new();
    let flood = MAX_TRACKED_LIMITER_KEYS + 500;

    for index in 0..flood {
        let index = index as u32;
        // Distinct IP, distinct username, so every map sees distinct keys.
        limiter.record_failure(&format!("user_{index}"), ip(index), T0);
    }

    let (ip_count, user_ip_count, username_count) = limiter.tracked_key_counts();
    assert!(
        ip_count <= MAX_TRACKED_LIMITER_KEYS,
        "ip_failures tracked {ip_count} keys, exceeding the {MAX_TRACKED_LIMITER_KEYS} bound"
    );
    assert!(
        user_ip_count <= MAX_TRACKED_LIMITER_KEYS,
        "user_ip_failures tracked {user_ip_count} keys, exceeding the {MAX_TRACKED_LIMITER_KEYS} bound"
    );
    assert!(
        username_count <= MAX_TRACKED_LIMITER_KEYS,
        "username_failures tracked {username_count} keys, exceeding the {MAX_TRACKED_LIMITER_KEYS} bound"
    );
}

/// Recording failures with no preceding `check` must still respect the bound.
///
/// This is the specific hole: `check()` carried the only guard, and
/// `record_failure()` is reachable without it.
#[test]
#[doc = "Spec: AUTH-8, SEC-12 — ISSUE-0059 record_failure alone sweeps without a preceding check"]
fn record_failure_alone_sweeps_without_a_preceding_check() {
    let limiter = ThreeTierLoginLimiter::new();
    let flood = MAX_TRACKED_LIMITER_KEYS + 250;

    for index in 0..flood {
        limiter.record_failure("shared_username", ip(index as u32), T0);
    }

    let (ip_count, user_ip_count, username_count) = limiter.tracked_key_counts();
    assert!(
        ip_count <= MAX_TRACKED_LIMITER_KEYS,
        "ip_failures grew to {ip_count} with no check() call, exceeding the bound"
    );
    assert!(
        user_ip_count <= MAX_TRACKED_LIMITER_KEYS,
        "user_ip_failures grew to {user_ip_count} with no check() call, exceeding the bound"
    );
    assert!(
        username_count <= 1,
        "a single repeated username must occupy exactly one key, got {username_count}"
    );
}

/// Driving `check()` with distinct keys must also respect the bound.
#[test]
#[doc = "Spec: AUTH-8, SEC-12 — ISSUE-0059 check bounds every map it touches"]
fn check_also_bounds_all_three_maps() {
    let limiter = ThreeTierLoginLimiter::new();
    let flood = MAX_TRACKED_LIMITER_KEYS + 250;

    for index in 0..flood {
        let index = index as u32;
        // Errors are the expected outcome for a fresh key only if the map is
        // saturated; either verdict is fine, the bound is what is asserted.
        let _ = limiter.check(&format!("user_{index}"), ip(index), T0);
    }

    let (ip_count, user_ip_count, username_count) = limiter.tracked_key_counts();
    assert!(
        ip_count <= MAX_TRACKED_LIMITER_KEYS,
        "ip_failures tracked {ip_count}"
    );
    assert!(
        user_ip_count <= MAX_TRACKED_LIMITER_KEYS,
        "user_ip_failures tracked {user_ip_count}"
    );
    assert!(
        username_count <= MAX_TRACKED_LIMITER_KEYS,
        "username_failures tracked {username_count}"
    );
}

/// Expired entries must be swept so the bound does not simply freeze the maps.
#[test]
#[doc = "Spec: AUTH-8, SEC-12 — ISSUE-0059 expired limiter entries are swept when full"]
fn expired_limiter_entries_are_swept_when_full() {
    let limiter = ThreeTierLoginLimiter::new();

    for index in 0..MAX_TRACKED_LIMITER_KEYS {
        limiter.record_failure(&format!("old_{index}"), ip(index as u32), T0);
    }
    assert!(
        limiter.tracked_key_counts().0 >= MAX_TRACKED_LIMITER_KEYS,
        "the IP map must be full before the sweep is exercised"
    );

    // 2 hours later every existing entry is outside both the 15 min and 1 h windows.
    let later = T0 + 2 * 60 * 60 * 1000;
    limiter.record_failure("fresh_user", ip(u32::MAX), later);

    let (ip_count, _user_ip_count, username_count) = limiter.tracked_key_counts();
    assert!(
        ip_count < MAX_TRACKED_LIMITER_KEYS,
        "expired IP entries must be swept, not merely tolerated; the map still holds \
         {ip_count} keys"
    );
    assert!(
        username_count <= 2,
        "expired username entries must be swept too; the map holds {username_count}"
    );
}

/// The bound must not change any threshold.
///
/// Tier 1: the 20th failure inside 15 minutes on one IP is rejected.
/// Tier 2: the 5th failure inside 15 minutes on one `(username, IP)` is rejected.
/// Tier 3: 50 failures for one username inside an hour yields `EnforceTotpOrDelay`.
#[test]
#[doc = "Spec: AUTH-8 — ISSUE-0059 bounding the maps does not change any threshold"]
fn login_thresholds_are_unchanged() {
    // Tier 1 — 19 recorded failures from one IP pass, the 20th trips it.
    //
    // Each failure uses a distinct username so tier 2 (5 per username+IP) never
    // accumulates: tier 2 would otherwise fire first and this would not test tier 1.
    let tier1 = ThreeTierLoginLimiter::new();
    let attacker_ip = ip(7001);
    for index in 0..MAX_FAILURES_PER_IP_15M - 1 {
        tier1.record_failure(&format!("spray_{index}"), attacker_ip, T0);
    }
    assert_eq!(
        tier1.check("victim", attacker_ip, T0).expect("allowed"),
        LimiterOutcome::Allowed,
        "{} failures must still be below the tier-1 threshold",
        MAX_FAILURES_PER_IP_15M - 1
    );
    tier1.record_failure("spray_final", attacker_ip, T0);
    assert!(
        matches!(
            tier1.check("victim", attacker_ip, T0),
            Err(AuthError::RateLimited)
        ),
        "{} failures must trip tier 1",
        MAX_FAILURES_PER_IP_15M
    );

    // Tier 2 — one IP, one username, threshold is per (username, IP).
    let tier2 = ThreeTierLoginLimiter::new();
    let ip2 = ip(7002);
    for _ in 0..MAX_FAILURES_PER_USER_IP_15M {
        tier2.record_failure("target", ip2, T0);
    }
    assert!(
        matches!(tier2.check("target", ip2, T0), Err(AuthError::RateLimited)),
        "{} failures must trip tier 2",
        MAX_FAILURES_PER_USER_IP_15M
    );

    // Tier 3 — 50 failures for one username across distinct IPs.
    let tier3 = ThreeTierLoginLimiter::new();
    for index in 0..USERNAME_ATTACK_THRESHOLD_1H {
        // Distinct IPs so tier 1 is never the reason.
        tier3.record_failure("victim", ip(10_000 + index as u32), T0);
    }
    assert_eq!(
        tier3.check("victim", ip(90_000), T0).expect("checked",),
        LimiterOutcome::EnforceTotpOrDelay,
        "{} failures for one username must escalate to TOTP-or-delay",
        USERNAME_ATTACK_THRESHOLD_1H
    );
}

/// A successful login must clear the username tier as well as the `(username, IP)`
/// tier, so a legitimate user is not escalated by their own earlier typos.
#[test]
#[doc = "Spec: AUTH-8 — ISSUE-0059 record_success clears the username tier too"]
fn record_success_clears_the_username_tier() {
    let limiter = ThreeTierLoginLimiter::new();
    let user_ip = ip(7003);

    for _ in 0..USERNAME_ATTACK_THRESHOLD_1H {
        limiter.record_failure("legit_user", user_ip, T0);
    }
    assert_eq!(
        limiter
            .check("legit_user", ip(90_001), T0)
            .expect("checked"),
        LimiterOutcome::EnforceTotpOrDelay,
        "the escalation must be in place before the successful login"
    );

    limiter.record_success("legit_user", user_ip);

    assert_eq!(
        limiter
            .check("legit_user", ip(90_002), T0)
            .expect("checked"),
        LimiterOutcome::Allowed,
        "a successful login must clear the username tier, or a user is permanently \
         escalated by their own earlier typos"
    );
}
