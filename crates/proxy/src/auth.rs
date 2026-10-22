#![forbid(unsafe_code)]
//! Client key authentication middleware, request gate dispatch, and auth failure accounting.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#100-ordered-request-gates-g1-through-g7
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-header-handling-policy

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arc_swap::ArcSwap;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use ipnet::IpNet;
use llmroller_core::client_key::{ClientKeyRpmLimiter, GateFailure, evaluate_request_gates};
use llmroller_core::snapshot::ConfigSnapshot;
use parking_lot::Mutex;
use sha2::Digest as _;
use sqlx::SqlitePool;

/// Maximum authentication failures allowed per client IP per minute before throttling per SEC-12.
pub const MAX_AUTH_FAILURES_PER_IP_MINUTE: usize = 20;

/// Shared state for client key authentication middleware.
#[derive(Clone)]
pub struct ProxyAuthState {
    pub snapshot: Arc<ArcSwap<ConfigSnapshot>>,
    pub pool: SqlitePool,
    /// Eight-connection read-only pool for data-plane reads (`REL-2`, F036).
    pub reader_pool: SqlitePool,
    pub rpm_limiter: Arc<ClientKeyRpmLimiter>,
    pub ip_failures: Arc<Mutex<HashMap<IpAddr, Vec<i64>>>>,
    pub trusted_proxies: Vec<String>,
    pub is_ready: Arc<std::sync::atomic::AtomicBool>,
    /// Number of live SSE response bodies, drained during graceful shutdown (REL-1).
    pub open_streams: Arc<std::sync::atomic::AtomicUsize>,
    /// Set by the shutdown supervisor once the drain deadline expires so streams
    /// that were not parked on the notify cannot miss the cut (REL-1).
    pub shutdown_cut: Arc<AtomicBool>,
    /// Wakes parked SSE streams when the shutdown supervisor cuts them (REL-1).
    pub shutdown_notify: Option<Arc<tokio::sync::Notify>>,
    pub budget_semaphore: Arc<crate::orchestrator::PreflightBudgetSemaphore>,
    pub pin_store: Arc<llmroller_core::pins::PinStore>,
    pub runtime_state: Arc<llmroller_core::runtime_state::RuntimeState>,
    /// Persistent tenant salt for HMAC-SHA256 `session_hash` derivation (`RTE-1`).
    pub session_hmac_secret: Option<llmroller_core::seams::SecretBytes>,
    pub writer: Option<llmroller_store::writer::WriterHandle>,
    pub master_key: Option<llmroller_store::crypto::MasterKey>,
    pub master_key_id: Option<String>,
    pub instance_id: Option<[u8; 16]>,
    pub sealer_pool: Option<Arc<llmroller_store::traces::SealerPool>>,
    pub http_client_public: reqwest::Client,
    pub http_client_private_allowed: reqwest::Client,
}

impl std::fmt::Debug for ProxyAuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyAuthState")
            .field("trusted_proxies", &self.trusted_proxies)
            .finish()
    }
}

impl ProxyAuthState {
    #[must_use]
    pub fn new(
        snapshot: Arc<ArcSwap<ConfigSnapshot>>,
        pool: SqlitePool,
        trusted_proxies: Vec<String>,
    ) -> Self {
        Self {
            snapshot,
            reader_pool: pool.clone(),
            pool,
            rpm_limiter: Arc::new(ClientKeyRpmLimiter::new()),
            ip_failures: Arc::new(Mutex::new(HashMap::new())),
            trusted_proxies,
            is_ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            open_streams: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shutdown_cut: Arc::new(AtomicBool::new(false)),
            shutdown_notify: None,
            budget_semaphore: Arc::new(crate::orchestrator::PreflightBudgetSemaphore::new(819, 32)),
            pin_store: Arc::new(llmroller_core::pins::PinStore::new(250_000)),
            runtime_state: Arc::new(llmroller_core::runtime_state::RuntimeState::new()),
            session_hmac_secret: None,
            writer: None,
            master_key: None,
            master_key_id: None,
            instance_id: None,
            sealer_pool: None,
            http_client_public: crate::ssrf::create_guarded_client(false)
                .expect("the public HTTP client requires a working TLS backend"),
            http_client_private_allowed: crate::ssrf::create_guarded_client(true)
                .expect("the private-network HTTP client requires a working TLS backend"),
        }
    }

    /// Select the pooled outbound client matching the provider's egress policy.
    #[must_use]
    pub fn select_http_client(&self, allow_private_network: bool) -> &reqwest::Client {
        if allow_private_network {
            &self.http_client_private_allowed
        } else {
            &self.http_client_public
        }
    }

    #[must_use]
    pub fn with_master_key(mut self, master_key: llmroller_store::crypto::MasterKey) -> Self {
        self.master_key = Some(master_key);
        self
    }

    #[must_use]
    pub fn with_runtime_state(
        mut self,
        runtime_state: Arc<llmroller_core::runtime_state::RuntimeState>,
    ) -> Self {
        self.runtime_state = runtime_state;
        self
    }

    #[must_use]
    pub fn with_writer(mut self, writer: llmroller_store::writer::WriterHandle) -> Self {
        self.writer = Some(writer);
        self
    }

    /// Attach the read-only connection pool used by control-plane and usage reads.
    #[must_use]
    pub fn with_reader_pool(mut self, reader_pool: SqlitePool) -> Self {
        self.reader_pool = reader_pool;
        self
    }

    /// Attach the graceful-shutdown cut signal observed by live SSE bodies (REL-1).
    #[must_use]
    pub fn with_shutdown_signal(
        mut self,
        cut: Arc<AtomicBool>,
        notify: Arc<tokio::sync::Notify>,
    ) -> Self {
        self.shutdown_cut = cut;
        self.shutdown_notify = Some(notify);
        self
    }

    /// Record an authentication failure for rate limiting and flush to auth_failure_counters table.
    pub async fn record_failure(&self, ip: IpAddr, reason: &'static str, now_ms: i64) {
        // In-memory 60s sliding window
        {
            let mut failures = self.ip_failures.lock();
            let entry = failures.entry(ip).or_default();
            entry.retain(|&ts| now_ms - ts < 60_000);
            entry.push(now_ms);
        }

        // Persist to auth_failure_counters table (minute resolution, /24 or /56 prefix)
        let minute_ms = (now_ms / 60_000) * 60_000;
        let prefix_str = match ip {
            IpAddr::V4(v4) => {
                let oct = v4.octets();
                format!("{}.{}.{}.0/24", oct[0], oct[1], oct[2])
            }
            IpAddr::V6(v6) => {
                let seg = v6.segments();
                format!(
                    "{:x}:{:x}:{:x}:{:x}::/56",
                    seg[0],
                    seg[1],
                    seg[2],
                    seg[3] & 0xff00
                )
            }
        };

        let _ = sqlx::query(
            "INSERT INTO auth_failure_counters (minute_ms, reason, ip_prefix, count)
             VALUES (?, ?, ?, 1)
             ON CONFLICT(minute_ms, reason, ip_prefix) DO UPDATE SET count = count + 1",
        )
        .bind(minute_ms)
        .bind(reason)
        .bind(prefix_str)
        .execute(&self.pool)
        .await;
    }

    /// Check if the given client IP has exceeded the bad-token rate limit.
    #[must_use]
    pub fn is_ip_rate_limited(&self, ip: IpAddr, now_ms: i64) -> bool {
        let mut failures = self.ip_failures.lock();
        if let Some(entry) = failures.get_mut(&ip) {
            entry.retain(|&ts| now_ms - ts < 60_000);
            entry.len() >= MAX_AUTH_FAILURES_PER_IP_MINUTE
        } else {
            false
        }
    }
}

/// Extract the client IP honoring trusted reverse proxies per SEC-9 / SEC-12.
#[must_use]
pub fn extract_client_ip(
    peer_addr: SocketAddr,
    headers: &HeaderMap,
    trusted_proxies: &[String],
) -> IpAddr {
    let peer_ip = peer_addr.ip();
    if trusted_proxies.is_empty() {
        return peer_ip;
    }

    let peer_str = peer_ip.to_string();
    let is_trusted = trusted_proxies.iter().any(|cidr| {
        cidr.trim() == peer_str
            || cidr
                .trim()
                .parse::<IpNet>()
                .is_ok_and(|net| net.contains(&peer_ip))
    });

    if !is_trusted {
        return peer_ip;
    }

    let Some(forwarded_for) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) else {
        return peer_ip;
    };

    let entries: Vec<&str> = forwarded_for.split(',').map(str::trim).collect();
    for entry in entries.into_iter().rev() {
        if let Ok(ip) = entry.parse::<IpAddr>() {
            let is_proxy = trusted_proxies.iter().any(|cidr| {
                cidr.trim() == entry
                    || cidr
                        .trim()
                        .parse::<IpNet>()
                        .is_ok_and(|net| net.contains(&ip))
            });
            if !is_proxy {
                return ip;
            }
        }
    }

    peer_ip
}

/// Authentication middleware enforcing request gates G1 through G6.
pub async fn client_key_auth_middleware(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Response {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);

    let peer_addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map_or_else(
            || SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080),
            |ci| ci.0,
        );

    let client_ip = extract_client_ip(peer_addr, &headers, &state.trusted_proxies);

    // 1. Check IP failure rate limit (SEC-12)
    if state.is_ip_rate_limited(client_ip, now_ms) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": {
                    "code": "rate_limit_error",
                    "message": "too many authentication failures, retry in 60s"
                }
            })),
        )
            .into_response();
    }

    // 2. Extract client bearer key from Authorization or x-api-key per API-2
    let token = if let Some(auth_header) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok())
    {
        if let Some(bearer) = auth_header.strip_prefix("Bearer ") {
            bearer.trim()
        } else {
            auth_header.trim()
        }
    } else if let Some(api_key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        api_key.trim()
    } else {
        state.record_failure(client_ip, "unknown_key", now_ms).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": {
                    "code": "authentication_error",
                    "message": "missing bearer token or x-api-key"
                }
            })),
        )
            .into_response();
    };

    let hash: [u8; 32] = sha2::Sha256::digest(token.as_bytes()).into();
    let snapshot = state.snapshot.load();

    // 3. Evaluate gates G1 through G6
    let gate_result = evaluate_request_gates(
        &hash,
        Some(client_ip),
        None,
        None,
        &snapshot,
        Some(&state.rpm_limiter),
        now_ms,
    );

    match gate_result {
        Ok((key_record, hash_role)) => {
            request.extensions_mut().insert(key_record);
            request.extensions_mut().insert(hash_role);
            next.run(request).await
        }
        Err(failure) => match failure {
            GateFailure::G1AuthenticationError => {
                state.record_failure(client_ip, "unknown_key", now_ms).await;
                (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": {
                            "code": "authentication_error",
                            "message": "invalid client key"
                        }
                    })),
                )
                    .into_response()
            }
            GateFailure::G2KeyDisabledOrExpired => {
                state.record_failure(client_ip, "disabled", now_ms).await;
                (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": {
                            "code": "authentication_error",
                            "message": "client key is disabled, revoked, or expired"
                        }
                    })),
                )
                    .into_response()
            }
            GateFailure::G2ClientKeyPaused => (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": {
                        "code": "client_key_paused",
                        "message": "client key is paused; resume it in the control plane"
                    }
                })),
            )
                .into_response(),
            GateFailure::G2ServiceAccountDisabled => (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": {
                        "code": "service_account_disabled",
                        "message": "parent service account is disabled"
                    }
                })),
            )
                .into_response(),
            GateFailure::G3OwnerDisabled => {
                state
                    .record_failure(client_ip, "owner_disabled", now_ms)
                    .await;
                (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": {
                            "code": "authentication_error",
                            "message": "account is disabled"
                        }
                    })),
                )
                    .into_response()
            }
            GateFailure::G4CidrForbidden => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": {
                        "code": "permission_error",
                        "message": "client IP not allowed by key CIDR policy"
                    }
                })),
            )
                .into_response(),
            GateFailure::G5RateLimitExceeded { retry_after_secs } => (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", retry_after_secs.to_string())],
                Json(serde_json::json!({
                    "error": {
                        "code": "rate_limit_error",
                        "message": "client key rate limit exceeded"
                    }
                })),
            )
                .into_response(),
            GateFailure::G6ModelNotFound { model } => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": {
                        "code": "model_not_found",
                        "message": format!("model {model} not found")
                    }
                })),
            )
                .into_response(),
            GateFailure::G6ModelForbidden { model } => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": {
                        "code": "permission_error",
                        "message": format!("model {model} not permitted on this key")
                    }
                })),
            )
                .into_response(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: AUTH-8, Spec: SEC-12, Spec: CLK-4"]
    fn test_extract_client_ip_right_to_left_traversal() {
        let peer_addr: SocketAddr = "10.0.0.1:12345".parse().expect("parse socket addr");
        let trusted_proxies = vec!["10.0.0.0/8".to_string(), "172.16.0.1".to_string()];

        // 1. Untrusted peer -> returns peer IP, ignores XFF
        let untrusted_peer: SocketAddr = "192.168.1.1:12345".parse().expect("parse socket addr");
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "1.2.3.4, 5.6.7.8".parse().expect("parse header"),
        );
        assert_eq!(
            extract_client_ip(untrusted_peer, &headers, &trusted_proxies),
            "192.168.1.1".parse::<IpAddr>().expect("parse ip")
        );

        // 2. Trusted peer with multiple XFF -> traverses right-to-left
        headers.insert(
            "x-forwarded-for",
            "1.1.1.1, 203.0.113.195, 172.16.0.1"
                .parse()
                .expect("parse header"),
        );
        assert_eq!(
            extract_client_ip(peer_addr, &headers, &trusted_proxies),
            "203.0.113.195".parse::<IpAddr>().expect("parse ip")
        );

        // 3. Trusted peer with single external IP
        headers.insert(
            "x-forwarded-for",
            "198.51.100.42".parse().expect("parse header"),
        );
        assert_eq!(
            extract_client_ip(peer_addr, &headers, &trusted_proxies),
            "198.51.100.42".parse::<IpAddr>().expect("parse ip")
        );

        // 4. Empty trusted proxies -> returns peer IP even if XFF present
        assert_eq!(
            extract_client_ip(peer_addr, &headers, &[]),
            "10.0.0.1".parse::<IpAddr>().expect("parse ip")
        );
    }
}
