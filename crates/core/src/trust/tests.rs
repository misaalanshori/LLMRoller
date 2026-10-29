//! Trusted-edge predicate tests (`SEC-12`, `#158`).

use super::*;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr as _;

fn v4(addr: &str) -> IpAddr {
    IpAddr::V4(Ipv4Addr::from_str(addr).expect("valid IPv4 literal"))
}

fn v6(addr: &str) -> IpAddr {
    IpAddr::V6(Ipv6Addr::from_str(addr).expect("valid IPv6 literal"))
}

fn list(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn an_empty_trusted_list_trusts_nobody() {
    assert!(!peer_is_trusted_proxy(v4("10.0.0.1"), &[]));
    assert!(!peer_is_trusted_proxy(v4("127.0.0.1"), &[]));
}

#[test]
fn an_exact_peer_address_matches() {
    assert!(peer_is_trusted_proxy(
        v4("203.0.113.9"),
        &list(&["203.0.113.9"])
    ));
    assert!(!peer_is_trusted_proxy(
        v4("203.0.113.10"),
        &list(&["203.0.113.9"])
    ));
}

#[test]
fn a_cidr_prefix_matches_only_inside_the_prefix() {
    let trusted = list(&["10.0.0.0/8"]);
    assert!(peer_is_trusted_proxy(v4("10.0.0.1"), &trusted));
    assert!(peer_is_trusted_proxy(v4("10.255.255.254"), &trusted));
    assert!(!peer_is_trusted_proxy(v4("11.0.0.1"), &trusted));
}

#[test]
fn ipv6_prefixes_and_addresses_match() {
    assert!(peer_is_trusted_proxy(
        v6("2001:db8::1"),
        &list(&["2001:db8::/32"])
    ));
    assert!(!peer_is_trusted_proxy(
        v6("2001:db9::1"),
        &list(&["2001:db8::/32"])
    ));
    assert!(peer_is_trusted_proxy(v6("::1"), &list(&["::1"])));
}

/// An IPv4 peer must not match an IPv6 prefix or the reverse: the two families are disjoint
/// address spaces, and a cross-family "match" could only ever come from a bug.
#[test]
fn address_families_never_cross_match() {
    assert!(!peer_is_trusted_proxy(
        v4("10.0.0.1"),
        &list(&["2001:db8::/32"])
    ));
    assert!(!peer_is_trusted_proxy(v6("::1"), &list(&["0.0.0.0/0"])));
}

/// A typo must deny, not broaden. `parse_ip_or_cidr` rejects `10.0.0.0/33`, so the entry
/// contributes no match and the peer stays untrusted.
#[test]
fn a_malformed_entry_matches_nothing_rather_than_everything() {
    assert!(!peer_is_trusted_proxy(
        v4("10.0.0.1"),
        &list(&["10.0.0.0/33", "/8", "", "not-an-ip"])
    ));
}

/// Surrounding whitespace is tolerated: operators write these lists in hand-edited env vars
/// (`LLMROLLER_TRUSTED_PROXIES="10.0.0.0/8, 172.16.0.0/12"`) and the space after the comma
/// must not silently disable the second entry.
#[test]
fn entries_are_trimmed_before_matching() {
    let trusted = list(&[" 10.0.0.0/8 ", "\t172.16.0.0/12\n"]);
    assert!(peer_is_trusted_proxy(v4("10.1.2.3"), &trusted));
    assert!(peer_is_trusted_proxy(v4("172.16.5.5"), &trusted));
}

/// The zero-length prefix matches the whole family it is written for; that is the operator
/// explicitly asking for it, not an accident of parsing.
#[test]
fn a_zero_length_prefix_matches_its_own_family() {
    assert!(peer_is_trusted_proxy(v4("8.8.8.8"), &list(&["0.0.0.0/0"])));
    assert!(peer_is_trusted_proxy(v6("2606:4700::1"), &list(&["::/0"])));
}
