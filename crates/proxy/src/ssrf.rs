#![forbid(unsafe_code)]
//! Socket-level SSRF filter, IP range validation, and GuardedConnector.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#21-sub-phase-21-socket-level-ssrf-guardedconnector-and-url-validation

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};

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
    #[error("HTTP client error: {0}")]
    Reqwest(#[from] reqwest::Error),
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
        // When private networks are explicitly allowed by admin, only multicast,
        // broadcast, and unspecified addresses remain blocked.
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
///
/// Ensures:
/// 1. URL parses and has a host.
/// 2. Scheme is `https://` (or `http://` if `allow_private_network = 1`).
/// 3. Rejects non-standard integer, octal, or hex IP representations.
/// 4. Checks IP literals against special-purpose blocked ranges.
pub fn validate_url(url_str: &str, allow_private_network: bool) -> Result<Url, SsrfError> {
    // Check raw host string before WHATWG URL normalization resolves octal/hex
    if let Some(raw_host) = extract_raw_host(url_str) {
        // (a) Single decimal integer host (e.g. 2130706433)
        if !raw_host.is_empty() && raw_host.chars().all(|c| c.is_ascii_digit()) {
            return Err(SsrfError::NonStandardIpEncoding(raw_host.to_owned()));
        }

        // (b) Single hex host (e.g. 0x7f000001)
        if raw_host.starts_with("0x") || raw_host.starts_with("0X") {
            return Err(SsrfError::NonStandardIpEncoding(raw_host.to_owned()));
        }

        // (c) Octal or hex notation in dot-separated host components
        let parts: Vec<&str> = raw_host.split('.').collect();
        if parts.len() == 4 {
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

fn extract_raw_host(url_str: &str) -> Option<&str> {
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

/// Socket-level connector service that validates every target IP before connecting.
#[derive(Clone, Debug)]
pub struct GuardedConnector {
    pub allow_private_network: bool,
}

impl GuardedConnector {
    #[must_use]
    pub fn new(allow_private_network: bool) -> Self {
        Self {
            allow_private_network,
        }
    }

    /// Resolve host to socket addresses and validate that no resolved IP is blocked.
    pub async fn resolve_and_validate(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<SocketAddr>, SsrfError> {
        let unbracketed = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = unbracketed.parse::<IpAddr>() {
            if is_ip_blocked(ip, self.allow_private_network) {
                return Err(SsrfError::BlockedIp(ip));
            }
            return Ok(vec![SocketAddr::new(ip, port)]);
        }

        // Host is a domain name: resolve DNS
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(SsrfError::Dns)?
            .collect();

        if addrs.is_empty() {
            return Err(SsrfError::NoAddressesResolved(host.to_owned()));
        }

        for addr in &addrs {
            if is_ip_blocked(addr.ip(), self.allow_private_network) {
                return Err(SsrfError::BlockedIp(addr.ip()));
            }
        }

        Ok(addrs)
    }

    /// Establish a TCP connection to the validated destination.
    pub async fn connect_tcp(
        &self,
        host: &str,
        port: u16,
    ) -> Result<tokio::net::TcpStream, SsrfError> {
        let addrs = self.resolve_and_validate(host, port).await?;
        tokio::net::TcpStream::connect(&addrs[..])
            .await
            .map_err(SsrfError::Dns)
    }
}

impl tower::Service<http::Uri> for GuardedConnector {
    type Response = tokio::net::TcpStream;
    type Error = SsrfError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        let connector = self.clone();
        Box::pin(async move {
            let scheme = uri.scheme_str().unwrap_or("https");
            if scheme != "https" && !(connector.allow_private_network && scheme == "http") {
                return Err(SsrfError::InsecureScheme);
            }
            let host = uri
                .host()
                .ok_or_else(|| SsrfError::Validation("missing host in URI".to_owned()))?
                .to_owned();
            let port = uri
                .port_u16()
                .unwrap_or(if scheme == "http" { 80 } else { 443 });
            connector.connect_tcp(&host, port).await
        })
    }
}

/// Create a hardened outbound `reqwest::Client` disabling proxies, redirects, and enforcing TLS 1.2+ per SEC-7.
pub fn create_guarded_client(allow_private_network: bool) -> Result<reqwest::Client, SsrfError> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());

    if !allow_private_network {
        builder = builder.min_tls_version(reqwest::tls::Version::TLS_1_2);
    }

    builder.build().map_err(SsrfError::Reqwest)
}

#[cfg(test)]
mod tests;
