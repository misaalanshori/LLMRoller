#![forbid(unsafe_code)]
//! Socket-level SSRF filter, IP range validation, and URL validation.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use thiserror::Error;
use url::Url;

/// SSRF errors.
#[derive(Debug, Error)]
pub enum SsrfError {
    #[error("blocked IP address: {0}")]
    BlockedIp(IpAddr),
    #[error("insecure scheme: http:// is not allowed unless allow_private_network is enabled")]
    InsecureScheme,
    #[error("non-standard IP encoding: {0}")]
    NonStandardIpEncoding(String),
    #[error("DNS resolution error: {0}")]
    Dns(#[from] std::io::Error),
    #[error("no IP addresses resolved for host: {0}")]
    NoAddressesResolved(String),
    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),
    #[error("validation error: {0}")]
    Validation(String),
}

/// Check whether an IPv4 address belongs to any special-purpose or blocked range per SEC-7.
#[must_use]
pub fn is_ipv4_blocked(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    // 0.0.0.0/8: "This host on this network"
    if octets[0] == 0 {
        return true;
    }
    // 10.0.0.0/8: Private-Use (RFC 1918)
    if octets[0] == 10 {
        return true;
    }
    // 100.64.0.0/10: Shared Address Space (RFC 6598)
    if octets[0] == 100 && (octets[1] & 0xC0) == 64 {
        return true;
    }
    // 127.0.0.0/8: Loopback
    if octets[0] == 127 {
        return true;
    }
    // 169.254.0.0/16: Link Local (RFC 3927)
    if octets[0] == 169 && octets[1] == 254 {
        return true;
    }
    // 172.16.0.0/12: Private-Use (RFC 1918)
    if octets[0] == 172 && (octets[1] & 0xF0) == 16 {
        return true;
    }
    // 192.0.0.0/24: IETF Protocol Assignments (RFC 6890)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
        return true;
    }
    // 192.0.2.0/24: Documentation (TEST-NET-1)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 2 {
        return true;
    }
    // 192.88.99.0/24: 6to4 Relay Anycast (RFC 7526)
    if octets[0] == 192 && octets[1] == 88 && octets[2] == 99 {
        return true;
    }
    // 192.168.0.0/16: Private-Use (RFC 1918)
    if octets[0] == 192 && octets[1] == 168 {
        return true;
    }
    // 198.18.0.0/15: Benchmarking (RFC 2544)
    if octets[0] == 198 && (octets[1] & 0xFE) == 18 {
        return true;
    }
    // 198.51.100.0/24: Documentation (TEST-NET-2)
    if octets[0] == 198 && octets[1] == 51 && octets[2] == 100 {
        return true;
    }
    // 203.0.113.0/24: Documentation (TEST-NET-3)
    if octets[0] == 203 && octets[1] == 0 && octets[2] == 113 {
        return true;
    }
    // 224.0.0.0/4: Multicast (RFC 5771)
    if (octets[0] & 0xF0) == 224 {
        return true;
    }
    // 240.0.0.0/4: Reserved / Future Use (RFC 1112)
    if (octets[0] & 0xF0) == 240 {
        return true;
    }
    // 255.255.255.255/32: Limited Broadcast
    if octets == [255, 255, 255, 255] {
        return true;
    }
    false
}

/// Check whether an IPv6 address belongs to any special-purpose or blocked range per SEC-7.
#[must_use]
pub fn is_ipv6_blocked(addr: Ipv6Addr) -> bool {
    let seg = addr.segments();

    // ::/128: Unspecified
    if addr.is_unspecified() {
        return true;
    }
    // ::1/128: Loopback
    if addr.is_loopback() {
        return true;
    }
    // IPv4-mapped IPv6: ::ffff:0:0/96
    if seg[0] == 0 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 && seg[4] == 0 && seg[5] == 0xffff {
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            seg[6] as u8,
            (seg[7] >> 8) as u8,
            seg[7] as u8,
        );
        return is_ipv4_blocked(v4);
    }
    // 6to4: 2002::/16 - unwrap embedded IPv4
    if seg[0] == 0x2002 {
        let v4 = Ipv4Addr::new(
            (seg[1] >> 8) as u8,
            seg[1] as u8,
            (seg[2] >> 8) as u8,
            seg[2] as u8,
        );
        return is_ipv4_blocked(v4);
    }
    // NAT64 Well-Known Prefix: 64:ff9b::/96 - unwrap embedded IPv4
    if seg[0] == 0x0064
        && seg[1] == 0xff9b
        && seg[2] == 0
        && seg[3] == 0
        && seg[4] == 0
        && seg[5] == 0
    {
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            seg[6] as u8,
            (seg[7] >> 8) as u8,
            seg[7] as u8,
        );
        return is_ipv4_blocked(v4);
    }
    // NAT64 Local-Use: 64:ff9b:1::/48
    if seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2] == 1 {
        return true;
    }
    // Discard prefix: 100::/64
    if seg[0] == 0x0100 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0 {
        return true;
    }
    // 2001::/23: IETF Protocol Assignments (includes Teredo 2001::/32, AMT, etc.)
    if seg[0] == 0x2001 && (seg[1] & 0xfe00) == 0 {
        return true;
    }
    // 2001:db8::/32: Documentation
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return true;
    }
    // Unique Local Address: fc00::/7
    if (seg[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Link-Local Unicast: fe80::/10
    if (seg[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // Site-Local Unicast (deprecated): fec0::/10
    if (seg[0] & 0xffc0) == 0xfec0 {
        return true;
    }
    // Multicast: ff00::/8
    if (seg[0] & 0xff00) == 0xff00 {
        return true;
    }
    false
}

/// Check whether an IP address is blocked under the given allow_private_network policy per SEC-7.
#[must_use]
pub fn is_ip_blocked(ip: IpAddr, allow_private_network: bool) -> bool {
    if allow_private_network {
        match ip {
            IpAddr::V4(v4) => {
                let octets = v4.octets();
                (octets[0] & 0xF0) == 224 || octets == [255, 255, 255, 255] || v4.is_unspecified()
            }
            IpAddr::V6(v6) => (v6.segments()[0] & 0xff00) == 0xff00 || v6.is_unspecified(),
        }
    } else {
        match ip {
            IpAddr::V4(v4) => is_ipv4_blocked(v4),
            IpAddr::V6(v6) => is_ipv6_blocked(v6),
        }
    }
}

/// Validate a provider URL at save time per PRV-7 and SEC-7.
pub fn validate_url(url_str: &str, allow_private_network: bool) -> Result<Url, SsrfError> {
    if let Some(raw_host) = extract_raw_host(url_str) {
        if !raw_host.is_empty() && raw_host.chars().all(|c| c.is_ascii_digit()) {
            return Err(SsrfError::NonStandardIpEncoding(raw_host.to_owned()));
        }

        if raw_host.starts_with("0x") || raw_host.starts_with("0X") {
            return Err(SsrfError::NonStandardIpEncoding(raw_host.to_owned()));
        }

        let parts: Vec<&str> = raw_host.split('.').collect();
        if (2..=4).contains(&parts.len())
            && parts.iter().all(|part| {
                !part.is_empty()
                    && (part.chars().all(|c| c.is_ascii_digit())
                        || part.starts_with("0x")
                        || part.starts_with("0X"))
            })
        {
            if parts.len() != 4 {
                return Err(SsrfError::NonStandardIpEncoding(raw_host.to_owned()));
            }
            for part in &parts {
                if part.starts_with("0x") || part.starts_with("0X") {
                    return Err(SsrfError::NonStandardIpEncoding(raw_host.to_owned()));
                }
                if part.len() > 1
                    && part.starts_with('0')
                    && part.chars().all(|c| c.is_ascii_digit())
                {
                    return Err(SsrfError::NonStandardIpEncoding(raw_host.to_owned()));
                }
            }
        }
    }

    let parsed = Url::parse(url_str)?;

    let scheme = parsed.scheme();
    if scheme != "https" && !(allow_private_network && scheme == "http") {
        return Err(SsrfError::InsecureScheme);
    }

    match parsed.host() {
        Some(url::Host::Ipv4(v4)) => {
            if is_ip_blocked(IpAddr::V4(v4), allow_private_network) {
                return Err(SsrfError::BlockedIp(IpAddr::V4(v4)));
            }
        }
        Some(url::Host::Ipv6(v6)) => {
            if is_ip_blocked(IpAddr::V6(v6), allow_private_network) {
                return Err(SsrfError::BlockedIp(IpAddr::V6(v6)));
            }
        }
        Some(url::Host::Domain(host_str)) => {
            let unbracketed = host_str.trim_start_matches('[').trim_end_matches(']');
            if let Ok(ip) = IpAddr::from_str(unbracketed)
                && is_ip_blocked(ip, allow_private_network)
            {
                return Err(SsrfError::BlockedIp(ip));
            }
        }
        None => return Err(SsrfError::Validation("missing host in URL".to_owned())),
    }

    Ok(parsed)
}

/// Extract raw host string before URL normalization.
#[must_use]
pub fn extract_raw_host(url_str: &str) -> Option<&str> {
    let after_scheme = url_str.split("://").nth(1)?;
    let host_and_rest = after_scheme.split('/').next()?;
    let host_and_port = host_and_rest.split('?').next()?;
    let host = host_and_port.split('#').next()?;
    if host.starts_with('[') {
        let end_bracket = host.find(']')?;
        return Some(&host[..=end_bracket]);
    }
    Some(host.split(':').next().unwrap_or(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: SEC-7, PRV-7"]
    fn rejects_two_and_three_part_shorthand_and_octal_hex_ipv4_urls() {
        assert!(matches!(
            validate_url("https://127.1/v1", false),
            Err(SsrfError::NonStandardIpEncoding(_))
        ));
        assert!(matches!(
            validate_url("https://127.0.1/v1", false),
            Err(SsrfError::NonStandardIpEncoding(_))
        ));
        assert!(matches!(
            validate_url("https://169.254.1/v1", true),
            Err(SsrfError::NonStandardIpEncoding(_))
        ));
        assert!(matches!(
            validate_url("https://0177.0.0.1/v1", false),
            Err(SsrfError::NonStandardIpEncoding(_))
        ));
        assert!(matches!(
            validate_url("https://2130706433/v1", false),
            Err(SsrfError::NonStandardIpEncoding(_))
        ));
    }
}
