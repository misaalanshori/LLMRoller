//! Web authentication, rate limiter, and security tests.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;

use super::{
    AuthError, LimiterOutcome, ThreeTierLoginLimiter, check_admin_listener_address,
    extract_client_ip,
};

#[test]
#[doc = "Spec: AUTH-8"]
fn three_tier_limiter_enforces_ip_user_ip_and_username_penalties() {
    let limiter = ThreeTierLoginLimiter::new();
    let ip1: IpAddr = "192.0.2.1".parse().expect("valid IP");
    let ip2: IpAddr = "192.0.2.2".parse().expect("valid IP");
    let now = 1_700_000_000_000;

    // 1. Five failures for (admin, ip1) triggers Tier 2
    for _ in 0..5 {
        assert_eq!(
            limiter.check("admin", ip1, now).expect("allowed"),
            LimiterOutcome::Allowed
        );
        limiter.record_failure("admin", ip1, now);
    }
    assert!(matches!(
        limiter.check("admin", ip1, now),
        Err(AuthError::RateLimited)
    ));

    // Admin from ip2 is still allowed (not locked out of whole account)
    assert_eq!(
        limiter.check("admin", ip2, now).expect("allowed"),
        LimiterOutcome::Allowed
    );

    // 2. Twenty failures from ip2 across distinct usernames triggers Tier 1 (IP limit)
    for i in 0..20 {
        let user = format!("user_{i}");
        limiter.record_failure(&user, ip2, now);
    }
    assert!(matches!(
        limiter.check("other_user", ip2, now),
        Err(AuthError::RateLimited)
    ));

    // 3. Across 60 distinct IPs, 50+ failures for 'victim' triggers Tier 3 without hard lockout
    let limiter_victim = ThreeTierLoginLimiter::new();
    for i in 1..=55 {
        let ip: IpAddr = format!("198.51.100.{i}").parse().expect("valid IP");
        limiter_victim.record_failure("victim", ip, now);
    }
    let new_ip: IpAddr = "203.0.113.1".parse().expect("valid IP");
    assert_eq!(
        limiter_victim
            .check("victim", new_ip, now)
            .expect("checked"),
        LimiterOutcome::EnforceTotpOrDelay
    );
}

#[test]
#[doc = "Spec: AUTH-8, Partial: SEC-12"]
fn client_ip_extraction_honors_trusted_proxies() {
    let trusted = vec!["10.0.0.0/8".to_owned(), "172.28.1.10/32".to_owned()];
    let peer = SocketAddr::new("172.28.1.10".parse().expect("ip"), 8081);

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "203.0.113.195, 10.1.2.3".parse().expect("valid header"),
    );

    let extracted = extract_client_ip(peer, &headers, &trusted);
    assert_eq!(
        extracted,
        "203.0.113.195".parse::<IpAddr>().expect("parsed")
    );

    // Untrusted peer cannot spoof X-Forwarded-For
    let untrusted_peer = SocketAddr::new("198.51.100.5".parse().expect("ip"), 8081);
    let extracted_untrusted = extract_client_ip(untrusted_peer, &headers, &trusted);
    assert_eq!(
        extracted_untrusted,
        "198.51.100.5".parse::<IpAddr>().expect("parsed")
    );

    // Unparseable token in XFF stops traversal immediately (F091, SEC-12)
    let mut bad_headers = HeaderMap::new();
    bad_headers.insert(
        "x-forwarded-for",
        "203.0.113.195, bad_hop, 10.1.2.3"
            .parse()
            .expect("valid header"),
    );
    let extracted_bad = extract_client_ip(peer, &bad_headers, &trusted);
    assert_eq!(
        extracted_bad,
        "172.28.1.10".parse::<IpAddr>().expect("parsed")
    );
}

#[test]
#[doc = "Spec: SEC-5, Partial: SEC-9"]
fn non_loopback_control_plane_bind_refuses_without_tls_or_trusted_proxies() {
    // Non-loopback plain HTTP without TLS and without trusted proxies fails with exit code 2
    assert_eq!(
        check_admin_listener_address("0.0.0.0:8081", false, ""),
        Err(2)
    );
    assert_eq!(
        check_admin_listener_address("192.168.1.50:8081", false, ""),
        Err(2)
    );

    // Loopback succeeds without TLS
    assert_eq!(
        check_admin_listener_address("127.0.0.1:8081", false, ""),
        Ok(())
    );
    assert_eq!(
        check_admin_listener_address("[::1]:8081", false, ""),
        Ok(())
    );

    // Non-loopback with TLS succeeds
    assert_eq!(
        check_admin_listener_address("0.0.0.0:8081", true, ""),
        Ok(())
    );

    // Non-loopback with trusted proxies succeeds
    assert_eq!(
        check_admin_listener_address("0.0.0.0:8081", false, "172.28.1.10/32"),
        Ok(())
    );
}

// ---------------------------------------------------------------------------
// Login concurrency harness (`AUTH-7`, `SEC-5`, ISSUE-0001)
// ---------------------------------------------------------------------------

mod login_concurrency;

// ---------------------------------------------------------------------------
// Session CSRF derivation (`SEC-5`, `FND-041`, ISSUE-0002)
// ---------------------------------------------------------------------------

mod session_csrf;
