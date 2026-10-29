#![forbid(unsafe_code)]
//! The single trusted-edge predicate (`SEC-12`, `#158`).
//!
//! Two separate decisions ask the same question — *is this connecting peer an edge I
//! configured, whose forwarded claims I may believe?* — and before `#158` each answered it
//! with its own copy of the CIDR arithmetic:
//!
//! - the control plane, to decide whether `X-Forwarded-Proto: https` may make a cookie
//!   `Secure` (`crates/admin/src/auth.rs`);
//! - the data plane, to decide which `X-Forwarded-For` entry is the real client.
//!
//! `#158` adds a third caller that must agree with both: whether a geo header may set
//! `usage_events.client_location`. Three copies of a trust decision can drift, and a
//! drifted trust decision is a silent authentication bypass rather than a visible bug.
//! This module is therefore the one implementation, and `admin` delegates to it.
//!
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-header-handling-policy

use std::net::IpAddr;

use crate::client_key::parse_ip_or_cidr;

/// Whether `peer_ip` is listed in `trusted_proxies` (`SEC-12`).
///
/// A `trusted_proxies` entry matches either as a bare IPv4/IPv6 address (`203.0.113.9`) or as
/// a CIDR prefix (`10.0.0.0/8`, `2001:db8::/32`). An empty list trusts nobody — the default
/// deployment has no trusted edge, and "no configuration" must not mean "trust everything".
///
/// An unparseable entry matches nothing rather than being skipped silently into a broader
/// result; a typo in the operator's configuration can then only ever *deny*, never grant.
#[must_use]
pub fn peer_is_trusted_proxy(peer_ip: IpAddr, trusted_proxies: &[String]) -> bool {
    if trusted_proxies.is_empty() {
        return false;
    }
    let peer_str = peer_ip.to_string();
    trusted_proxies.iter().any(|entry| {
        let entry = entry.trim();
        entry == peer_str || parse_ip_or_cidr(entry).is_some_and(|net| net.contains(&peer_ip))
    })
}

#[cfg(test)]
mod tests;
