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
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use ipnet::IpNet;
use llmroller_core::client_key::{ClientKeyRpmLimiter, GateFailure, evaluate_request_gates};
use llmroller_core::snapshot::ConfigSnapshot;
use parking_lot::Mutex;
use sha2::Digest as _;
use sqlx::SqlitePool;

/// Maximum authentication failures allowed per client IP per minute before throttling per SEC-12.
pub const MAX_AUTH_FAILURES_PER_IP_MINUTE: usize = 20;

/// Maximum number of distinct client IPs tracked at once (`SEC-12`, `ISSUE-0059`).
///
/// The tracking map is keyed by attacker-controlled source address, so without a
/// bound an unauthenticated attacker can grow it without limit by spraying distinct
/// IPs. At capacity, expired entries are swept first; if the map is still full, one
/// existing entry is evicted rather than the new one dropped — dropping the new
/// entry would mean a flooding attacker is never rate-limited at all, which is worse
/// than recycling an old slot.
pub const MAX_TRACKED_FAILURE_IPS: usize = 10_000;

/// Sliding window for the per-IP authentication failure counter (`SEC-12`).
const IP_FAILURE_WINDOW_MS: i64 = 60_000;

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
    pub audit_hmac_key: Option<llmroller_core::seams::SecretBytes>,
    pub writer: Option<llmroller_store::writer::WriterHandle>,
    pub master_key: Option<llmroller_store::crypto::MasterKey>,
    pub master_key_id: Option<String>,
    pub instance_id: Option<[u8; 16]>,
    pub sealer_pool: Option<Arc<llmroller_store::traces::SealerPool>>,
    pub http_client_public: reqwest::Client,
    pub http_client_private_allowed: reqwest::Client,
    pub token_manager: crate::token_manager::TokenManager,
    pub clock: Arc<dyn llmroller_core::seams::Clock>,
    pub entropy: Arc<dyn llmroller_core::seams::EntropySource>,
    pub discovery_tx:
        Option<tokio::sync::mpsc::UnboundedSender<llmroller_proto::discovery::DiscoveryTarget>>,
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
            audit_hmac_key: None,
            writer: None,
            master_key: None,
            master_key_id: None,
            instance_id: None,
            sealer_pool: None,
            http_client_public: crate::ssrf::create_guarded_client(false)
                .expect("the public HTTP client requires a working TLS backend"),
            http_client_private_allowed: crate::ssrf::create_guarded_client(true)
                .expect("the private-network HTTP client requires a working TLS backend"),
            token_manager: crate::token_manager::TokenManager::new(),
            clock: Arc::new(llmroller_core::seams::SystemClock::new()),
            entropy: Arc::new(llmroller_core::seams::OsEntropy),
            discovery_tx: None,
        }
    }

    #[must_use]
    pub fn with_discovery_tx(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<llmroller_proto::discovery::DiscoveryTarget>,
    ) -> Self {
        self.discovery_tx = Some(tx);
        self
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
            // Bound the map before inserting: it is keyed by attacker-controlled
            // source address, so an unauthenticated attacker could otherwise grow it
            // without limit by spraying distinct IPs (`SEC-12`, `ISSUE-0059`).
            if failures.len() >= MAX_TRACKED_FAILURE_IPS {
                self.sweep_failure_ips_at_capacity(&mut failures, now_ms);
            }
            let entry = failures.entry(ip).or_default();
            entry.retain(|&ts| now_ms - ts < IP_FAILURE_WINDOW_MS);
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

        if let Some(writer) = &self.writer {
            writer.queues.durable_state.lock().insert(
                llmroller_store::writer::DurableKey::AuthFailureCounter(
                    minute_ms,
                    reason.to_string(),
                    prefix_str.clone(),
                ),
                llmroller_store::writer::DurableOp::AuthFailureCounterIncrement {
                    minute_ms,
                    reason: reason.to_string(),
                    ip_prefix: prefix_str,
                },
            );
        } else {
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
    }

    /// Keep `failures` within [`MAX_TRACKED_FAILURE_IPS`] while making room for one
    /// more entry.
    ///
    /// Runs only when the map is at capacity, never on the read path: an O(n) sweep
    /// per request would be a DoS amplifier. Two stages:
    ///
    /// 1. Drop every IP whose window holds no failure in the last 60 s. Cheap, and
    ///    sufficient whenever the flood is not entirely inside one minute.
    /// 2. If the map is still full — a genuine `MAX_TRACKED_FAILURE_IPS`-distinct-IP
    ///    flood inside a single window — evict expired-vector entries until there is
    ///    room. Evicting a live attacker entry resets that attacker's counter, which
    ///    is strictly better than unbounded memory. Dropping the *new* entry instead
    ///    would mean a flooding attacker is never rate-limited at all, which is worse.
    fn sweep_failure_ips_at_capacity(&self, failures: &mut HashMap<IpAddr, Vec<i64>>, now_ms: i64) {
        failures.retain(|_, attempts| {
            attempts.retain(|&ts| now_ms - ts < IP_FAILURE_WINDOW_MS);
            !attempts.is_empty()
        });
        // Stage 2: still full, so recycle slots. `HashMap` iteration order is
        // unspecified, so this evicts an arbitrary subset rather than asserting which
        // entries lose; every choice is a live entry whose counter resets.
        if failures.len() >= MAX_TRACKED_FAILURE_IPS {
            let excess = failures.len() - MAX_TRACKED_FAILURE_IPS + 1;
            let victims: Vec<IpAddr> = failures.keys().take(excess).copied().collect();
            for victim in victims {
                failures.remove(&victim);
            }
        }
    }

    /// Number of distinct client IPs currently tracked (`ISSUE-0059`).
    ///
    /// Exposed so the bound can be asserted. A bound with no observable is how this
    /// defect survived: nothing could see the map grow.
    #[must_use]
    pub fn tracked_failure_ip_count(&self) -> usize {
        self.ip_failures.lock().len()
    }

    /// Check if the given client IP has exceeded the bad-token rate limit.
    #[must_use]
    pub fn is_ip_rate_limited(&self, ip: IpAddr, now_ms: i64) -> bool {
        let mut failures = self.ip_failures.lock();
        if let Some(entry) = failures.get_mut(&ip) {
            entry.retain(|&ts| now_ms - ts < IP_FAILURE_WINDOW_MS);
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
        let Ok(ip) = entry.parse::<IpAddr>() else {
            break;
        };
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

    peer_ip
}

/// Authentication middleware enforcing request gates G1 through G6.
pub async fn client_key_auth_middleware(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Response {
    let now_ms = state.clock.now_ms();

    // The raw peer address, or `None` when the router was built without `ConnectInfo`
    // (in-process tests, unusual embeddings). Kept as an `Option` because the two readers below
    // want different things from its absence: rate limiting needs *an* address and defaults to
    // loopback, while geo attribution must default to **untrusted** (`#158`) — an unknown peer
    // is not an edge we configured, and assuming loopback would let any in-process caller be
    // attributed as if Caddy had spoken.
    let connect_info_peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0);
    let peer_addr =
        connect_info_peer.unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080));

    let client_ip = extract_client_ip(peer_addr, &headers, &state.trusted_proxies);
    let path = request.uri().path().to_string();
    let is_messages_path = path.starts_with("/v1/messages")
        || path.starts_with("/v1/v1/messages")
        || path.starts_with("/messages");

    // 1. Check IP failure rate limit (SEC-12)
    if state.is_ip_rate_limited(client_ip, now_ms) {
        return crate::orchestrator::build_protocol_error_response(
            is_messages_path,
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "rate_limit_error",
            "too many authentication failures, retry in 60s",
            None,
            Some(60),
        );
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
        return crate::orchestrator::build_protocol_error_response(
            is_messages_path,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "authentication_error",
            "missing bearer token or x-api-key",
            None,
            None,
        );
    };

    let hash: [u8; 32] = sha2::Sha256::digest(token.as_bytes()).into();
    let snapshot = state.snapshot.load();

    // 3. Evaluate gates G1 through G6.
    // RPM rate limiting (G5) applies strictly to inference endpoints (CLK-1, API-2);
    // administrative/discovery routes like /v1/models, /v1/usage, /v1/key do not consume RPM.
    // `SEC-6`, `USG-9` (`F-011`): gate-rejection events must still carry the
    // sanitized allow-list of diagnostic headers. `Authorization`/`x-api-key`
    // are never allow-listed, so the bearer secret cannot leak here.
    let request_headers_json = Some(llmroller_proto::headers::extract_safe_diagnostic_headers(
        &headers,
    ));
    // `#158`: geo attribution is decided here, where both the peer address and the headers are
    // available, and carried to the writer in `metadata_json`. Gate rejections happen before
    // `admit_inbound_request`, so this path needs its own copy of the same decision.
    let metadata_json = crate::orchestrator::client_location_metadata_from(
        &headers,
        connect_info_peer,
        &state.trusted_proxies,
    );
    let is_inference = path.starts_with("/v1/chat/completions")
        || is_messages_path
        || path.starts_with("/v1/responses");
    let rpm_limiter = if is_inference {
        Some(&*state.rpm_limiter)
    } else {
        None
    };

    let gate_result = evaluate_request_gates(
        &hash,
        Some(client_ip),
        None,
        None,
        &snapshot,
        rpm_limiter,
        now_ms,
    );

    match gate_result {
        Ok((key_record, hash_role)) => {
            request.extensions_mut().insert(key_record);
            request.extensions_mut().insert(hash_role);
            next.run(request).await
        }
        Err(failure) => {
            let matched_key = snapshot
                .client_keys_by_hash
                .get(&hash)
                .map(|(record, _role)| record);
            let record_gate_usage_event = |status_u16: u16, err_code: &str| {
                if let (Some(ck), Some(writer)) = (matched_key, &state.writer) {
                    let ep = if is_messages_path {
                        "messages"
                    } else if path.starts_with("/v1/responses") {
                        "responses"
                    } else {
                        "chat"
                    };
                    writer.queues.usage_writer.enqueue(
                        llmroller_store::usage_writer::UsageEventRecord {
                            id: llmroller_core::ids::UsageEventId::generate().to_string(),
                            ts: now_ms,
                            request_id: llmroller_core::ids::RequestId::generate(),
                            client_key_id: ck.id,
                            owner_user_id: ck.owner_user_id,
                            upstream_key_id: None,
                            provider_id: None,
                            provider_scope: Some("none".to_string()),
                            share_id: None,
                            canonical_id: None,
                            upstream_model_id: None,
                            endpoint: ep.to_string(),
                            protocol: Some(ep.to_string()),
                            session_hash: None,
                            status: i64::from(status_u16),
                            stream: false,
                            incomplete: false,
                            termination: "completed".to_string(),
                            trace_dropped: false,
                            input_uncached_tokens: 0,
                            cache_read_tokens: 0,
                            cache_write_tokens: 0,
                            output_tokens: 0,
                            reasoning_tokens: 0,
                            cost_micro_usd: 0,
                            cost_source: llmroller_core::pricing::CostSource::PriceBook,
                            latency_ms: Some(0),
                            ttfb_ms: None,
                            pin_outcome: None,
                            failovers: 0,
                            error_code: Some(err_code.to_string()),
                            list_cost_micro_usd: 0,
                            plan_multiplier_bps: 10_000,
                            billing_coverage_mode: "pay_as_you_go".to_string(),
                            service_account_id: ck
                                .service_account_id
                                .clone()
                                .map(|sa| sa.to_string()),
                            stop_reason: None,
                            request_headers_json: request_headers_json.clone(),
                            metadata_json: metadata_json.clone(),
                        },
                        Vec::new(),
                    );
                }
            };

            match failure {
                GateFailure::G1AuthenticationError => {
                    state.record_failure(client_ip, "unknown_key", now_ms).await;
                    crate::orchestrator::build_protocol_error_response(
                        is_messages_path,
                        StatusCode::UNAUTHORIZED,
                        "authentication_error",
                        "authentication_error",
                        "invalid client key",
                        None,
                        None,
                    )
                }
                GateFailure::G2KeyDisabledOrExpired => {
                    let g2_reason = if matched_key.is_some_and(|ck| ck.revoked_at.is_some()) {
                        "revoked"
                    } else if matched_key
                        .and_then(|ck| ck.expires_at)
                        .is_some_and(|exp| now_ms >= exp)
                    {
                        "expired"
                    } else {
                        "disabled"
                    };
                    state.record_failure(client_ip, g2_reason, now_ms).await;
                    crate::orchestrator::build_protocol_error_response(
                        is_messages_path,
                        StatusCode::UNAUTHORIZED,
                        "authentication_error",
                        "authentication_error",
                        "client key is disabled, revoked, or expired",
                        None,
                        None,
                    )
                }
                GateFailure::G2ClientKeyPaused => {
                    state.record_failure(client_ip, "disabled", now_ms).await;
                    crate::orchestrator::build_protocol_error_response(
                        is_messages_path,
                        StatusCode::UNAUTHORIZED,
                        "authentication_error",
                        "client_key_paused",
                        "client key is paused; resume it in the control plane",
                        None,
                        None,
                    )
                }
                GateFailure::G2ServiceAccountDisabled => {
                    state.record_failure(client_ip, "disabled", now_ms).await;
                    crate::orchestrator::build_protocol_error_response(
                        is_messages_path,
                        StatusCode::UNAUTHORIZED,
                        "authentication_error",
                        "service_account_disabled",
                        "parent service account is disabled",
                        None,
                        None,
                    )
                }
                GateFailure::G3OwnerDisabled => {
                    state
                        .record_failure(client_ip, "owner_disabled", now_ms)
                        .await;
                    crate::orchestrator::build_protocol_error_response(
                        is_messages_path,
                        StatusCode::UNAUTHORIZED,
                        "authentication_error",
                        "authentication_error",
                        "account is disabled",
                        None,
                        None,
                    )
                }
                GateFailure::G4CidrForbidden => {
                    state
                        .record_failure(client_ip, "cidr_forbidden", now_ms)
                        .await;
                    record_gate_usage_event(403, "permission_error");
                    crate::orchestrator::build_protocol_error_response(
                        is_messages_path,
                        StatusCode::FORBIDDEN,
                        "permission_error",
                        "permission_error",
                        "client IP not allowed by key CIDR policy",
                        None,
                        None,
                    )
                }
                GateFailure::G5RateLimitExceeded { retry_after_secs } => {
                    record_gate_usage_event(429, "rate_limit_error");
                    crate::orchestrator::build_protocol_error_response(
                        is_messages_path,
                        StatusCode::TOO_MANY_REQUESTS,
                        "rate_limit_error",
                        "rate_limit_error",
                        "client key rate limit exceeded",
                        None,
                        Some(retry_after_secs),
                    )
                }
                GateFailure::G6ModelNotFound { model } => {
                    record_gate_usage_event(404, "model_not_found");
                    crate::orchestrator::build_protocol_error_response(
                        is_messages_path,
                        StatusCode::NOT_FOUND,
                        "not_found_error",
                        "model_not_found",
                        &format!("model {model} not found"),
                        None,
                        None,
                    )
                }
                GateFailure::G6ModelForbidden { model } => {
                    record_gate_usage_event(403, "permission_error");
                    crate::orchestrator::build_protocol_error_response(
                        is_messages_path,
                        StatusCode::FORBIDDEN,
                        "permission_error",
                        "permission_error",
                        &format!("model {model} not permitted on this key"),
                        None,
                        None,
                    )
                }
            }
        }
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

        // 5. Unparseable token in XFF stops right-to-left traversal immediately (F091, SEC-12)
        headers.insert(
            "x-forwarded-for",
            "1.1.1.1, malformed_ip, 172.16.0.1"
                .parse()
                .expect("parse header"),
        );
        assert_eq!(
            extract_client_ip(peer_addr, &headers, &trusted_proxies),
            "10.0.0.1".parse::<IpAddr>().expect("parse ip")
        );
    }
}

/// Bounded failure-tracking map tests (`SEC-12`, `ISSUE-0059`).
///
/// The map is bounded so an unauthenticated attacker cannot grow it without limit
/// by spraying distinct source addresses. Every case here drives an explicit
/// `now_ms`, so nothing depends on wall-clock time or scheduling.
#[cfg(test)]
mod failure_bound_tests {
    use super::*;

    fn state() -> ProxyAuthState {
        ProxyAuthState::new(
            Arc::new(ArcSwap::from_pointee(ConfigSnapshot::default())),
            sqlx::SqlitePool::connect_lazy("sqlite::memory:").expect("lazy pool"),
            Vec::new(),
        )
    }

    fn ip(index: u32) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::from(index.to_be_bytes()))
    }

    /// A flood of distinct source IPs must not grow the tracking map without limit.
    ///
    /// The flood is deliberately only slightly larger than the bound. Each insert at
    /// capacity runs an O(n) sweep, so a much larger flood would make this test
    /// quadratic for no extra signal — the invariant is "never exceeds the bound",
    /// which a 1-span overshoot exercises just as well as a 5-span one.
    #[tokio::test]
    #[doc = "Spec: SEC-12 — ISSUE-0059 failure map is bounded under a distinct-IP flood"]
    async fn failure_map_is_bounded_under_a_distinct_ip_flood() {
        let state = state();
        let now = 1_700_000_000_000;

        let flood = MAX_TRACKED_FAILURE_IPS + 500;
        for index in 0..flood {
            state
                .record_failure(ip(index as u32), "invalid_key", now)
                .await;
        }

        assert!(
            state.tracked_failure_ip_count() <= MAX_TRACKED_FAILURE_IPS,
            "the failure map tracked {} distinct IPs, exceeding the {} bound; an \
             unauthenticated attacker can grow this without limit",
            state.tracked_failure_ip_count(),
            MAX_TRACKED_FAILURE_IPS
        );
    }

    /// Filling the map to its bound and then recording a failure in a later window
    /// must sweep the now-expired entries and admit the new IP.
    #[tokio::test]
    #[doc = "Spec: SEC-12 — ISSUE-0059 expired entries are swept when the map is full"]
    async fn expired_ip_entries_are_swept_when_the_map_is_full() {
        let state = state();
        let t0 = 1_700_000_000_000;

        for index in 0..MAX_TRACKED_FAILURE_IPS {
            state
                .record_failure(ip(index as u32), "invalid_key", t0)
                .await;
        }
        assert_eq!(
            state.tracked_failure_ip_count(),
            MAX_TRACKED_FAILURE_IPS,
            "the map must be exactly full before the sweep is exercised"
        );

        // 120 s later every existing entry is outside the 60 s window.
        let later = t0 + 120_000;
        let new_ip = ip(u32::MAX);
        state.record_failure(new_ip, "invalid_key", later).await;

        assert!(
            state.tracked_failure_ip_count() <= MAX_TRACKED_FAILURE_IPS,
            "the map must stay within its bound after a sweep; tracked {}",
            state.tracked_failure_ip_count()
        );
        assert!(
            state.tracked_failure_ip_count() < MAX_TRACKED_FAILURE_IPS,
            "expired entries must actually be removed, not merely tolerated; the map \
             still holds {} entries",
            state.tracked_failure_ip_count()
        );

        // The new IP's own counter must be live: reaching the threshold with further
        // failures in the same window has to trip the limiter. A single failure cannot
        // trip it (the threshold is 20), so drive it to the threshold rather than
        // asserting on one sample.
        for _ in 1..MAX_AUTH_FAILURES_PER_IP_MINUTE {
            state.record_failure(new_ip, "invalid_key", later).await;
        }
        assert!(
            state.is_ip_rate_limited(new_ip, later),
            "the newly recorded IP must be tracked and rate-limited once it reaches the \
             threshold; its entry was lost by the sweep"
        );
    }

    /// The bound must not break rate limiting: a single IP's own counter must still
    /// trip at the threshold.
    ///
    /// The threshold is inclusive: `is_ip_rate_limited` fires once the window holds
    /// `MAX_AUTH_FAILURES_PER_IP_MINUTE` entries, so the last-but-one is the final
    /// admitted attempt.
    #[tokio::test]
    #[doc = "Spec: SEC-12 — ISSUE-0059 the bound does not evict the attacker's own entry"]
    async fn rate_limiting_still_fires_at_the_threshold() {
        let state = state();
        let now = 1_700_000_000_000;
        let attacker = ip(4242);

        for recorded in 0..MAX_AUTH_FAILURES_PER_IP_MINUTE {
            assert!(
                !state.is_ip_rate_limited(attacker, now),
                "must not be limited after {recorded} failures, below the {} threshold",
                MAX_AUTH_FAILURES_PER_IP_MINUTE
            );
            state.record_failure(attacker, "invalid_key", now).await;
        }

        assert!(
            state.is_ip_rate_limited(attacker, now),
            "{} failures inside one minute must rate-limit the IP",
            MAX_AUTH_FAILURES_PER_IP_MINUTE
        );
    }
}
