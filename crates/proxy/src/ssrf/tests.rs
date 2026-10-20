//! SSRF IP range and URL validation tests.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#21-sub-phase-21-socket-level-ssrf-guardedconnector-and-url-validation

use std::net::IpAddr;
use std::str::FromStr;

use crate::ssrf::{SsrfError, is_ip_blocked, validate_url};

/// Spec: PRV-7, SEC-7
/// Verify that all special-purpose, private, loopback, link-local, multicast,
/// IPv4-mapped IPv6, 6to4, and NAT64 IP ranges are blocked by is_ip_blocked.
#[doc = "Spec: PRV-7, SEC-7"]
#[test]
fn test_ssrf_ip_ranges_blocked() {
    // IPv4 special-purpose ranges
    let blocked_ipv4 = [
        "0.0.0.0", // 0.0.0.0/8
        "0.1.2.3",
        "10.0.0.1", // 10.0.0.0/8 (RFC 1918)
        "10.255.255.254",
        "100.64.0.1", // 100.64.0.0/10 (CGNAT RFC 6598)
        "100.127.255.254",
        "127.0.0.1", // 127.0.0.0/8 (Loopback)
        "127.255.255.255",
        "169.254.169.254", // 169.254.0.0/16 (Link-local / AWS metadata)
        "169.254.0.1",
        "172.16.0.1", // 172.16.0.0/12 (RFC 1918)
        "172.31.255.254",
        "192.0.0.1",   // 192.0.0.0/24 (IETF Protocol)
        "192.0.2.1",   // 192.0.2.0/24 (TEST-NET-1)
        "192.88.99.1", // 192.88.99.0/24 (6to4 Relay)
        "192.168.1.1", // 192.168.0.0/16 (RFC 1918)
        "192.168.254.254",
        "198.18.0.1", // 198.18.0.0/15 (Benchmarking)
        "198.19.255.254",
        "198.51.100.1", // 198.51.100.0/24 (TEST-NET-2)
        "203.0.113.1",  // 203.0.113.0/24 (TEST-NET-3)
        "224.0.0.1",    // 224.0.0.0/4 (Multicast)
        "239.255.255.250",
        "240.0.0.1",       // 240.0.0.0/4 (Reserved / Future Use)
        "255.255.255.255", // Limited broadcast
    ];

    for ip_str in blocked_ipv4 {
        let ip = IpAddr::from_str(ip_str).expect("parse ipv4");
        assert!(
            is_ip_blocked(ip, false),
            "IPv4 {ip_str} must be blocked by default"
        );
    }

    // Public IPv4 addresses must NOT be blocked
    let allowed_ipv4 = [
        "8.8.8.8",
        "1.1.1.1",
        "93.184.216.34", // example.com
        "104.244.42.1",
    ];
    for ip_str in allowed_ipv4 {
        let ip = IpAddr::from_str(ip_str).expect("parse allowed ipv4");
        assert!(
            !is_ip_blocked(ip, false),
            "Public IPv4 {ip_str} must NOT be blocked"
        );
    }

    // IPv6 special-purpose ranges
    let blocked_ipv6 = [
        "::",          // ::/128 (Unspecified)
        "::1",         // ::1/128 (Loopback)
        "100::1",      // 100::/64 (Discard)
        "2001::1",     // 2001::/23 (IETF Protocol / Teredo)
        "2001:db8::1", // 2001:db8::/32 (Documentation)
        "fc00::1",     // fc00::/7 (ULA)
        "fd12:3456:789a::1",
        "fe80::1",                // fe80::/10 (Link-local)
        "fec0::1",                // fec0::/10 (Site-local deprecated)
        "ff02::1",                // ff00::/8 (Multicast)
        "::ffff:127.0.0.1",       // IPv4-mapped loopback
        "::ffff:10.0.0.1",        // IPv4-mapped private
        "::ffff:169.254.169.254", // IPv4-mapped metadata
        "2002:a9fe:a9fe::1",      // 6to4 embedding 169.254.169.254
        "2002:7f00:0001::",       // 6to4 embedding 127.0.0.1
        "2002:0a00:0001::",       // 6to4 embedding 10.0.0.1
        "64:ff9b::127.0.0.1",     // NAT64 embedding 127.0.0.1
        "64:ff9b:1::",            // NAT64 local-use 64:ff9b:1::/48
    ];

    for ip_str in blocked_ipv6 {
        let ip = IpAddr::from_str(ip_str).expect("parse ipv6");
        assert!(
            is_ip_blocked(ip, false),
            "IPv6 {ip_str} must be blocked by default"
        );
    }

    // Public IPv6 must NOT be blocked
    let allowed_ipv6 = [
        "2606:4700:4700::1111", // Cloudflare DNS
        "2001:4860:4860::8888", // Google DNS
        "2600:1408:ec00::",
    ];
    for ip_str in allowed_ipv6 {
        let ip = IpAddr::from_str(ip_str).expect("parse allowed ipv6");
        assert!(
            !is_ip_blocked(ip, false),
            "Public IPv6 {ip_str} must NOT be blocked"
        );
    }
}

/// Spec: PRV-7, SEC-7
/// Verify URL validation enforces https, rejects non-standard IP encodings,
/// and honors allow_private_network flag.
#[doc = "Spec: PRV-7, SEC-7"]
#[test]
fn test_ssrf_url_validation() {
    // 1. Insecure http scheme rejected by default
    let err = validate_url("http://api.openai.com/v1", false)
        .expect_err("http must be rejected without allow_private_network");
    assert!(matches!(err, SsrfError::InsecureScheme));

    // http permitted when allow_private_network is true
    let url = validate_url("http://127.0.0.1:8080/v1", true)
        .expect("http allowed with allow_private_network");
    assert_eq!(url.scheme(), "http");

    // 2. IP literals in URL checked against blocked ranges
    let err =
        validate_url("https://127.0.0.1/v1", false).expect_err("127.0.0.1 literal must be blocked");
    assert!(matches!(err, SsrfError::BlockedIp(_)));

    let err = validate_url("https://[::ffff:127.0.0.1]/v1", false)
        .expect_err("mapped IPv6 literal must be blocked");
    assert!(matches!(err, SsrfError::BlockedIp(_)));

    let err = validate_url("https://[2002:a9fe:a9fe::1]/v1", false)
        .expect_err("6to4 metadata literal must be blocked");
    assert!(matches!(err, SsrfError::BlockedIp(_)));

    // 3. Non-standard integer/octal/hex IP hosts rejected at save time
    let err = validate_url("https://2130706433/v1", false)
        .expect_err("decimal integer IP must be rejected");
    assert!(matches!(err, SsrfError::NonStandardIpEncoding(_)));

    let err = validate_url("https://0177.0.0.1/v1", false).expect_err("octal IP must be rejected");
    assert!(matches!(err, SsrfError::NonStandardIpEncoding(_)));

    let err = validate_url("https://0x7f.0.0.1/v1", false).expect_err("hex IP must be rejected");
    assert!(matches!(err, SsrfError::NonStandardIpEncoding(_)));

    // 4. Valid domain URL succeeds
    let url = validate_url("https://api.anthropic.com/v1", false).expect("valid https URL");
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("api.anthropic.com"));
}
