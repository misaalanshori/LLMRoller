#![forbid(unsafe_code)]
//! Socket-level SSRF filter, IP range validation, and GuardedConnector.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#21-sub-phase-21-socket-level-ssrf-guardedconnector-and-url-validation

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
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

pub use llmroller_core::ssrf::{extract_raw_host, is_ip_blocked, is_ipv4_blocked, is_ipv6_blocked};

pub fn validate_url(url_str: &str, allow_private_network: bool) -> Result<Url, SsrfError> {
    llmroller_core::ssrf::validate_url(url_str, allow_private_network).map_err(Into::into)
}

impl From<llmroller_core::ssrf::SsrfError> for SsrfError {
    fn from(err: llmroller_core::ssrf::SsrfError) -> Self {
        match err {
            llmroller_core::ssrf::SsrfError::BlockedIp(ip) => SsrfError::BlockedIp(ip),
            llmroller_core::ssrf::SsrfError::InsecureScheme => SsrfError::InsecureScheme,
            llmroller_core::ssrf::SsrfError::NonStandardIpEncoding(s) => {
                SsrfError::NonStandardIpEncoding(s)
            }
            llmroller_core::ssrf::SsrfError::Dns(e) => SsrfError::Dns(e),
            llmroller_core::ssrf::SsrfError::NoAddressesResolved(s) => {
                SsrfError::NoAddressesResolved(s)
            }
            llmroller_core::ssrf::SsrfError::Url(e) => SsrfError::Url(e),
            llmroller_core::ssrf::SsrfError::Validation(s) => SsrfError::Validation(s),
        }
    }
}

/// DNS resolver that rejects any hostname resolving to a blocked address (SEC-7).
#[derive(Debug, Clone)]
pub struct GuardedDnsResolver {
    allow_private_network: bool,
}

impl GuardedDnsResolver {
    #[must_use]
    pub const fn new(allow_private_network: bool) -> Self {
        Self {
            allow_private_network,
        }
    }
}

impl Resolve for GuardedDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private_network = self.allow_private_network;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();

            if addrs.is_empty() {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no DNS records found for host '{host}'"),
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }

            for addr in &addrs {
                if is_ip_blocked(addr.ip(), allow_private_network) {
                    return Err(Box::new(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "SSRF guard blocked host '{host}' resolving to {}",
                            addr.ip()
                        ),
                    ))
                        as Box<dyn std::error::Error + Send + Sync>);
                }
            }

            let addrs: Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// Create a hardened outbound `reqwest::Client` disabling proxies and redirects,
/// resolving every hostname through [`GuardedDnsResolver`], and enforcing TLS 1.2+
/// per SEC-7.
pub fn create_guarded_client(allow_private_network: bool) -> Result<reqwest::Client, SsrfError> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(GuardedDnsResolver::new(allow_private_network)))
        .pool_max_idle_per_host(32)
        .connect_timeout(Duration::from_secs(10));

    if !allow_private_network {
        builder = builder.min_tls_version(reqwest::tls::Version::TLS_1_2);
    }

    builder.build().map_err(SsrfError::Reqwest)
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

#[cfg(test)]
mod tests;
