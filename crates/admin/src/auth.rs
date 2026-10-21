#![forbid(unsafe_code)]
//! Web authentication, sessions, CSRF, and three-tier rate limiting.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::header::COOKIE;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use llmroller_core::ids::UserId;
use llmroller_core::rbac::Role;
use llmroller_core::seams::{Clock, EntropySource, OsEntropy, SystemClock};
use llmroller_store::crypto::sha256_hex;
use llmroller_store::users::{
    NewWebSession, SessionWithUser, User, count_users, delete_web_session, find_login_record,
    insert_web_session, load_web_session, touch_web_session, verify_password,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::sync::Semaphore;

/// Cookie name for authenticated web sessions.
pub const SESSION_COOKIE_NAME: &str = "__Host-llmroller_session";
/// Fallback cookie name when `--dev-insecure-cookies` is active.
pub const DEV_SESSION_COOKIE_NAME: &str = "llmroller_session";

/// Cookie name for unauthenticated login CSRF tokens.
pub const LOGIN_CSRF_COOKIE_NAME: &str = "__Host-llmroller_login_csrf";
/// Fallback cookie name for login CSRF when `--dev-insecure-cookies` is active.
pub const DEV_LOGIN_CSRF_COOKIE_NAME: &str = "llmroller_login_csrf";

/// Idle timeout for web sessions (12 hours in milliseconds).
pub const SESSION_IDLE_TIMEOUT_MS: i64 = 12 * 3600 * 1000;
/// Absolute expiry for web sessions (7 days in milliseconds).
pub const SESSION_ABSOLUTE_TIMEOUT_MS: i64 = 7 * 86400 * 1000;
/// Login CSRF cookie validity (10 minutes in seconds).
pub const LOGIN_CSRF_MAX_AGE_SECS: i64 = 600;

const RATE_LIMIT_WINDOW_15M_MS: i64 = 15 * 60 * 1000;
const RATE_LIMIT_WINDOW_1H_MS: i64 = 60 * 60 * 1000;
const MAX_FAILURES_PER_IP_15M: usize = 20;
const MAX_FAILURES_PER_USER_IP_15M: usize = 5;
const USERNAME_ATTACK_THRESHOLD_1H: usize = 50;

/// Authentication errors with deterministic HTTP mappings.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("missing or invalid session")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("account disabled")]
    AccountDisabled,
    #[error("too many requests")]
    RateLimited,
    #[error("login busy")]
    LoginBusy,
    #[error("invalid CSRF token")]
    InvalidCsrf,
    #[error("TOTP verification required")]
    TotpRequired,
    #[error("internal error")]
    Internal,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "authentication required",
            ),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "permission denied"),
            Self::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "username or password incorrect",
            ),
            Self::AccountDisabled => (
                StatusCode::FORBIDDEN,
                "account_disabled",
                "account is suspended",
            ),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many failed login attempts; try again later",
            ),
            Self::LoginBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "login_busy",
                "login verifier busy; retry shortly",
            ),
            Self::InvalidCsrf => (
                StatusCode::FORBIDDEN,
                "invalid_csrf",
                "invalid or missing CSRF token",
            ),
            Self::TotpRequired => (
                StatusCode::UNAUTHORIZED,
                "totp_required",
                "TOTP second factor required",
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "internal server error",
            ),
        };
        (
            status,
            Json(serde_json::json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}

/// Normalized client IP for rate-limiting (exact IPv4, IPv6 `/64` prefix) per `AUTH-8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NormalizedClientIp {
    V4(Ipv4Addr),
    V6(u64),
}

impl From<IpAddr> for NormalizedClientIp {
    fn from(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(addr) => Self::V4(addr),
            IpAddr::V6(addr) => {
                let segments = addr.segments();
                let prefix = (u64::from(segments[0]) << 48)
                    | (u64::from(segments[1]) << 32)
                    | (u64::from(segments[2]) << 16)
                    | u64::from(segments[3]);
                Self::V6(prefix)
            }
        }
    }
}

/// Three-tier in-memory login rate limiter per `AUTH-8`.
#[derive(Debug)]
pub struct ThreeTierLoginLimiter {
    ip_failures: Mutex<HashMap<NormalizedClientIp, Vec<i64>>>,
    user_ip_failures: Mutex<HashMap<(String, NormalizedClientIp), Vec<i64>>>,
    username_failures: Mutex<HashMap<String, Vec<i64>>>,
}

impl Default for ThreeTierLoginLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Evaluation outcome from the three-tier login limiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimiterOutcome {
    Allowed,
    EnforceTotpOrDelay,
}

impl ThreeTierLoginLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ip_failures: Mutex::new(HashMap::new()),
            user_ip_failures: Mutex::new(HashMap::new()),
            username_failures: Mutex::new(HashMap::new()),
        }
    }

    /// Check whether a login attempt is admitted under the three-tier policy.
    pub fn check(
        &self,
        username: &str,
        ip: IpAddr,
        now_ms: i64,
    ) -> Result<LimiterOutcome, AuthError> {
        let client_key = NormalizedClientIp::from(ip);

        // Tier 1: 20 failures per 15 minutes per IP
        {
            let mut ip_map = self.ip_failures.lock();
            let attempts = ip_map.entry(client_key).or_default();
            attempts.retain(|&ts| now_ms - ts < RATE_LIMIT_WINDOW_15M_MS);
            if attempts.len() >= MAX_FAILURES_PER_IP_15M {
                return Err(AuthError::RateLimited);
            }
        }

        // Tier 2: 5 failures per 15 minutes per (username, IP)
        {
            let mut user_ip_map = self.user_ip_failures.lock();
            let key = (username.to_ascii_lowercase(), client_key);
            let attempts = user_ip_map.entry(key).or_default();
            attempts.retain(|&ts| now_ms - ts < RATE_LIMIT_WINDOW_15M_MS);
            if attempts.len() >= MAX_FAILURES_PER_USER_IP_15M {
                return Err(AuthError::RateLimited);
            }
        }

        // Tier 3: Across all IPs for this username, > 50 failures per 1 hour
        {
            let mut user_map = self.username_failures.lock();
            let attempts = user_map.entry(username.to_ascii_lowercase()).or_default();
            attempts.retain(|&ts| now_ms - ts < RATE_LIMIT_WINDOW_1H_MS);
            if attempts.len() >= USERNAME_ATTACK_THRESHOLD_1H {
                return Ok(LimiterOutcome::EnforceTotpOrDelay);
            }
        }

        Ok(LimiterOutcome::Allowed)
    }

    /// Record a failed login attempt.
    pub fn record_failure(&self, username: &str, ip: IpAddr, now_ms: i64) {
        let client_key = NormalizedClientIp::from(ip);
        let normalized = username.to_ascii_lowercase();

        {
            let mut ip_map = self.ip_failures.lock();
            ip_map.entry(client_key).or_default().push(now_ms);
        }
        {
            let mut user_ip_map = self.user_ip_failures.lock();
            user_ip_map
                .entry((normalized.clone(), client_key))
                .or_default()
                .push(now_ms);
        }
        {
            let mut user_map = self.username_failures.lock();
            user_map.entry(normalized).or_default().push(now_ms);
        }
    }

    /// Reset failure counters for a username and IP on successful authentication.
    pub fn record_success(&self, username: &str, ip: IpAddr) {
        let client_key = NormalizedClientIp::from(ip);
        let normalized = username.to_ascii_lowercase();

        let mut user_ip_map = self.user_ip_failures.lock();
        user_ip_map.remove(&(normalized, client_key));
    }
}

/// Configuration for trusted proxy CIDRs and cookie security.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub trusted_proxies: Vec<String>,
    pub dev_insecure_cookies: bool,
    pub require_tls: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            trusted_proxies: Vec::new(),
            dev_insecure_cookies: false,
            require_tls: true,
        }
    }
}

impl AuthConfig {
    #[must_use]
    pub fn session_cookie_name(&self) -> &'static str {
        if self.dev_insecure_cookies {
            DEV_SESSION_COOKIE_NAME
        } else {
            SESSION_COOKIE_NAME
        }
    }

    #[must_use]
    pub fn login_csrf_cookie_name(&self) -> &'static str {
        if self.dev_insecure_cookies {
            DEV_LOGIN_CSRF_COOKIE_NAME
        } else {
            LOGIN_CSRF_COOKIE_NAME
        }
    }
}

/// Verify control-plane listener address rules per `SEC-5` and `SEC-9`.
///
/// Refuses to start on a non-loopback plain-HTTP address without TLS unless
/// `LLMROLLER_TRUSTED_PROXIES` is configured (returns exit code 2).
pub fn check_admin_listener_address(
    listen_addr: &str,
    has_tls: bool,
    trusted_proxies: &str,
) -> Result<(), u8> {
    if has_tls || !trusted_proxies.trim().is_empty() {
        return Ok(());
    }
    let parsed: SocketAddr = listen_addr.parse().map_err(|_| 2)?;
    if parsed.ip().is_loopback() {
        Ok(())
    } else {
        Err(2)
    }
}

/// Extract the effective client IP honoring trusted proxies per `SEC-12`.
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

    // Verify peer is in trusted proxies
    let peer_str = peer_ip.to_string();
    let is_trusted_peer = trusted_proxies
        .iter()
        .any(|p| p.trim() == peer_str || cidr_contains(p.trim(), peer_ip));
    if !is_trusted_peer {
        return peer_ip;
    }

    // Parse X-Forwarded-For from right to left
    let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) else {
        return peer_ip;
    };

    let entries: Vec<&str> = xff.split(',').map(str::trim).collect();
    for entry in entries.into_iter().rev() {
        if let Ok(ip) = entry.parse::<IpAddr>() {
            let is_proxy = trusted_proxies
                .iter()
                .any(|p| p.trim() == entry || cidr_contains(p.trim(), ip));
            if !is_proxy {
                return ip;
            }
        }
    }

    peer_ip
}

fn cidr_contains(cidr: &str, ip: IpAddr) -> bool {
    let mut parts = cidr.split('/');
    let Some(network_str) = parts.next() else {
        return false;
    };
    let Some(prefix_len_str) = parts.next() else {
        return network_str.parse::<IpAddr>() == Ok(ip);
    };
    let Ok(prefix_len) = prefix_len_str.parse::<u8>() else {
        return false;
    };

    match (network_str.parse::<IpAddr>(), ip) {
        (Ok(IpAddr::V4(net_v4)), IpAddr::V4(ip_v4)) if prefix_len <= 32 => {
            let net_u32 = u32::from(net_v4);
            let ip_u32 = u32::from(ip_v4);
            let mask = if prefix_len == 0 {
                0
            } else {
                !((1_u32 << (32 - prefix_len)) - 1)
            };
            (net_u32 & mask) == (ip_u32 & mask)
        }
        (Ok(IpAddr::V6(net_v6)), IpAddr::V6(ip_v6)) if prefix_len <= 128 => {
            let net_u128 = u128::from(net_v6);
            let ip_u128 = u128::from(ip_v6);
            let mask = if prefix_len == 0 {
                0
            } else {
                !((1_u128 << (128 - prefix_len)) - 1)
            };
            (net_u128 & mask) == (ip_u128 & mask)
        }
        _ => false,
    }
}

/// Shared state for authentication handlers and middleware.
#[derive(Clone)]
pub struct AuthAppState {
    pub pool: SqlitePool,
    pub limiter: Arc<ThreeTierLoginLimiter>,
    pub config: AuthConfig,
    pub clock: Arc<dyn Clock>,
    pub entropy: Arc<dyn EntropySource>,
    pub argon2_semaphore: Arc<Semaphore>,
    /// Master key used to open sealed TOTP secrets during login.
    pub master_key: Option<llmroller_store::crypto::MasterKey>,
    /// Identifier of the active master key, bound into the TOTP AAD.
    pub master_key_id: String,
}

impl std::fmt::Debug for AuthAppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthAppState")
            .field("config", &self.config)
            .finish()
    }
}

impl AuthAppState {
    pub fn new(pool: SqlitePool, config: AuthConfig) -> Self {
        let parallelism = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .max(2);
        Self {
            pool,
            limiter: Arc::new(ThreeTierLoginLimiter::new()),
            config,
            clock: Arc::new(SystemClock::new()),
            entropy: Arc::new(OsEntropy::new()),
            argon2_semaphore: Arc::new(Semaphore::new(parallelism)),
            master_key: None,
            master_key_id: String::new(),
        }
    }

    /// Verify a login TOTP or single-use recovery code, advancing replay state.
    async fn verify_totp_code(
        &self,
        user_id: llmroller_core::ids::UserId,
        code: &str,
        now_ms: i64,
    ) -> Result<bool, AuthError> {
        let master_key = self.master_key.clone().ok_or(AuthError::Internal)?;
        let instance_id: Vec<u8> =
            sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(|_| AuthError::Internal)?
                .unwrap_or_else(|| vec![0_u8; 16]);
        let instance_id: [u8; 16] = instance_id.try_into().unwrap_or([0_u8; 16]);
        let sealer = llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
            master_key,
            instance_id,
            Arc::new(OsEntropy::new()),
        );
        match llmroller_store::users::verify_login_totp(
            &self.pool,
            user_id,
            code,
            &sealer,
            &self.master_key_id,
            now_ms,
        )
        .await
        {
            Ok(_) => Ok(true),
            Err(llmroller_store::users::UserError::InvalidTotp)
            | Err(llmroller_store::users::UserError::TotpReplay) => Ok(false),
            Err(_) => Err(AuthError::Internal),
        }
    }
}

/// Login request payload.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    pub totp_code: Option<String>,
}

/// Login form submission payload for browser form POSTs.
#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
    pub totp_code: Option<String>,
    pub csrf_token: Option<String>,
}

/// Login success response with user projection and CSRF token.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub user_id: UserId,
    pub username: String,
    pub role: Role,
    pub csrf_token: String,
}

/// Build the authentication router.
pub fn router(state: AuthAppState) -> Router {
    Router::new()
        .route("/", get(get_root_redirect))
        .route("/login", get(get_login_page))
        .route("/api/auth/login", post(post_login))
        .route("/api/auth/logout", post(post_logout))
        .route("/api/auth/me", get(get_me))
        .with_state(state)
}

#[doc = "Spec: AUTH-1, Spec: AUTH-3, Spec: UI-1"]
async fn get_root_redirect(State(state): State<AuthAppState>, headers: HeaderMap) -> Response {
    let count = count_users(&state.pool).await.unwrap_or(0);
    if count == 0 {
        return (StatusCode::SEE_OTHER, [("location", "/setup")]).into_response();
    }
    if let Ok(session) = authenticate_session(&state, &headers).await {
        if session.user.role == Role::Admin {
            return (StatusCode::SEE_OTHER, [("location", "/admin")]).into_response();
        } else {
            return (StatusCode::SEE_OTHER, [("location", "/app")]).into_response();
        }
    }
    (StatusCode::SEE_OTHER, [("location", "/login")]).into_response()
}

#[doc = "Spec: SEC-5"]
async fn get_login_page(State(state): State<AuthAppState>) -> Response {
    let mut raw_csrf = [0_u8; 32];
    state.entropy.fill_bytes(&mut raw_csrf);
    let csrf_token = URL_SAFE_NO_PAD.encode(raw_csrf);

    let cookie_name = state.config.login_csrf_cookie_name();
    let secure_flag = if state.config.dev_insecure_cookies {
        ""
    } else {
        "; Secure"
    };
    let cookie_header = format!(
        "{cookie_name}={csrf_token}; HttpOnly{secure_flag}; SameSite=Strict; Path=/; Max-Age={LOGIN_CSRF_MAX_AGE_SECS}"
    );

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta name="csrf-token" content="{csrf_token}">
    <title>LLMRoller - Sign In</title>
    <style>
        :root {{ --bg: #0f172a; --card: #1e293b; --border: #334155; --text: #f8fafc; --muted: #94a3b8; --primary: #3b82f6; --primary-hover: #2563eb; --error: #ef4444; }}
        * {{ box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; }}
        body {{ background: var(--bg); color: var(--text); min-height: 100vh; display: flex; align-items: center; justify-content: center; padding: 1rem; }}
        .card {{ background: var(--card); border: 1px solid var(--border); border-radius: 12px; width: 100%; max-width: 400px; padding: 2rem; box-shadow: 0 10px 25px rgba(0,0,0,0.5); }}
        h1 {{ font-size: 1.5rem; font-weight: 700; margin-bottom: 0.5rem; text-align: center; }}
        p.subtitle {{ color: var(--muted); font-size: 0.875rem; margin-bottom: 1.5rem; text-align: center; }}
        .form-group {{ margin-bottom: 1.25rem; }}
        label {{ display: block; font-size: 0.8125rem; font-weight: 600; margin-bottom: 0.375rem; color: #cbd5e1; }}
        input {{ width: 100%; padding: 0.625rem 0.875rem; background: #0f172a; border: 1px solid var(--border); border-radius: 6px; color: #fff; font-size: 0.9375rem; outline: none; transition: border-color 0.2s; }}
        input:focus {{ border-color: var(--primary); }}
        button {{ width: 100%; padding: 0.75rem; background: var(--primary); color: #fff; border: none; border-radius: 6px; font-weight: 600; font-size: 0.9375rem; cursor: pointer; transition: background 0.2s; margin-top: 0.5rem; }}
        button:hover {{ background: var(--primary-hover); }}
        .alert {{ display: none; background: rgba(239, 68, 68, 0.15); border: 1px solid var(--error); color: #fca5a5; padding: 0.75rem; border-radius: 6px; font-size: 0.875rem; margin-bottom: 1.25rem; }}
    </style>
</head>
<body>
    <div class="card">
        <h1>LLMRoller</h1>
        <p class="subtitle">Sign in to your control workspace</p>
        <div id="error-box" class="alert"></div>
        <form id="login-form" method="POST" action="/api/auth/login">
            <input type="hidden" name="csrf_token" value="{csrf_token}">
            <div class="form-group">
                <label for="username">Username</label>
                <input id="username" name="username" type="text" required autofocus placeholder="username">
            </div>
            <div class="form-group">
                <label for="password">Password</label>
                <input id="password" name="password" type="password" required placeholder="••••••••••••">
            </div>
            <div class="form-group">
                <label for="totp_code">2FA Code (Optional)</label>
                <input id="totp_code" name="totp_code" type="text" inputmode="numeric" autocomplete="one-time-code" placeholder="6-digit code">
            </div>
            <button type="submit" id="submit-btn">Sign In</button>
        </form>
    </div>
    <script>
        const form = document.getElementById('login-form');
        const errBox = document.getElementById('error-box');
        const btn = document.getElementById('submit-btn');

        form.addEventListener('submit', async (e) => {{
            e.preventDefault();
            errBox.style.display = 'none';
            btn.disabled = true;
            btn.textContent = 'Signing in...';

            const payload = {{
                username: form.username.value.trim(),
                password: form.password.value,
                totp_code: form.totp_code.value.trim() || null
            }};

            try {{
                const res = await fetch('/api/auth/login', {{
                    method: 'POST',
                    headers: {{
                        'Content-Type': 'application/json',
                        'Accept': 'application/json',
                        'X-CSRF-Token': '{csrf_token}'
                    }},
                    body: JSON.stringify(payload)
                }});
                const data = await res.json().catch(() => ({{}}));
                if (res.ok) {{
                    if (data.role === 'admin') {{
                        window.location.href = '/admin';
                    }} else {{
                        window.location.href = '/app';
                    }}
                }} else {{
                    errBox.textContent = data.error || ('Authentication failed with status ' + res.status);
                    errBox.style.display = 'block';
                    btn.disabled = false;
                    btn.textContent = 'Sign In';
                }}
            }} catch (err) {{
                // Fallback to native form post
                form.submit();
            }}
        }});
    </script>
</body>
</html>"#
    );

    (
        StatusCode::OK,
        [
            ("content-type", "text/html; charset=utf-8"),
            ("set-cookie", &cookie_header),
            ("x-csrf-token", &csrf_token),
        ],
        html,
    )
        .into_response()
}

#[doc = "Spec: AUTH-7, Spec: AUTH-8, Spec: SEC-4, Spec: SEC-5"]
async fn post_login(
    State(state): State<AuthAppState>,
    parts: Parts,
    body: Bytes,
) -> Result<Response, AuthError> {
    let now_ms = state.clock.now_ms();
    let peer_addr = parts
        .extensions
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map_or_else(
            || SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8081),
            |ci| ci.0,
        );
    let client_ip = extract_client_ip(peer_addr, &parts.headers, &state.config.trusted_proxies);

    let content_type = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let (payload, form_csrf): (LoginRequest, Option<String>) =
        if content_type.contains("application/json") {
            let req: LoginRequest =
                serde_json::from_slice(&body).map_err(|_| AuthError::InvalidCredentials)?;
            (req, None)
        } else {
            let params: HashMap<String, String> =
                url::form_urlencoded::parse(&body).into_owned().collect();
            (
                LoginRequest {
                    username: params.get("username").cloned().unwrap_or_default(),
                    password: params.get("password").cloned().unwrap_or_default(),
                    totp_code: params
                        .get("totp_code")
                        .cloned()
                        .filter(|s| !s.trim().is_empty()),
                },
                params
                    .get("csrf_token")
                    .cloned()
                    .filter(|s| !s.trim().is_empty()),
            )
        };

    // 1. Validate Login CSRF token per SEC-5
    let login_csrf_cookie_name = state.config.login_csrf_cookie_name();
    let cookie_header = parts
        .headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected_csrf = parse_cookie_value(cookie_header, login_csrf_cookie_name);

    let header_csrf = parts
        .headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or(form_csrf);

    let (Some(expected), Some(header_token)) = (expected_csrf, header_csrf) else {
        return Err(AuthError::InvalidCsrf);
    };
    if expected.is_empty() || !bool::from(expected.as_bytes().ct_eq(header_token.as_bytes())) {
        return Err(AuthError::InvalidCsrf);
    }

    // 2. Evaluate three-tier rate limiter before Argon2id
    let limiter_outcome = state.limiter.check(&payload.username, client_ip, now_ms)?;

    // 3. Acquire Argon2id concurrency semaphore (bounded permits, 5s timeout -> 503)
    let _permit = tokio::time::timeout(Duration::from_secs(5), state.argon2_semaphore.acquire())
        .await
        .map_err(|_| AuthError::LoginBusy)?
        .map_err(|_| AuthError::Internal)?;

    // 4. Lookup user and verify password
    let record = find_login_record(&state.pool, &payload.username)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::InvalidCredentials)?;

    if record.user.disabled {
        state
            .limiter
            .record_failure(&payload.username, client_ip, now_ms);
        return Err(AuthError::AccountDisabled);
    }

    if !verify_password(&payload.password, &record.password_hash) {
        state
            .limiter
            .record_failure(&payload.username, client_ip, now_ms);
        return Err(AuthError::InvalidCredentials);
    }

    // 5. Enforce TOTP 2FA whenever the account has it enabled (AUTH-9, SEC-4)
    if record.user.totp_enabled {
        let Some(code) = payload.totp_code.as_deref().map(str::trim) else {
            return Err(AuthError::TotpRequired);
        };
        if code.is_empty() {
            return Err(AuthError::TotpRequired);
        }
        if !state.verify_totp_code(record.user.id, code, now_ms).await? {
            state
                .limiter
                .record_failure(&payload.username, client_ip, now_ms);
            return Err(AuthError::InvalidCredentials);
        }
    } else if limiter_outcome == LimiterOutcome::EnforceTotpOrDelay {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Record login success in rate limiter
    state.limiter.record_success(&payload.username, client_ip);

    // 6. Issue 256-bit session token and random 256-bit CSRF token per AUTH-7 / SEC-5
    let mut raw_session = [0_u8; 32];
    state.entropy.fill_bytes(&mut raw_session);
    let session_token = URL_SAFE_NO_PAD.encode(raw_session);
    let session_id_hash = sha256_hex(session_token.as_bytes());

    let csrf_token = derive_session_csrf(&session_token);
    let csrf_hash = sha256_hex(csrf_token.as_bytes());

    let expires_at = now_ms + SESSION_ABSOLUTE_TIMEOUT_MS;
    insert_web_session(
        &state.pool,
        NewWebSession {
            id_hash: &session_id_hash,
            user_id: record.user.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some(&client_ip.to_string()),
            user_agent: parts
                .headers
                .get("user-agent")
                .and_then(|v| v.to_str().ok()),
            created_at: now_ms,
            expires_at,
        },
    )
    .await
    .map_err(|_| AuthError::Internal)?;

    let cookie_name = state.config.session_cookie_name();
    let secure_flag = if state.config.dev_insecure_cookies {
        ""
    } else {
        "; Secure"
    };
    let cookie_header = format!(
        "{cookie_name}={session_token}; HttpOnly{secure_flag}; SameSite=Strict; Path=/; Max-Age={}",
        SESSION_ABSOLUTE_TIMEOUT_MS / 1000
    );

    let target_path = if record.user.role == Role::Admin {
        "/admin"
    } else {
        "/app"
    };

    let is_json = content_type.contains("application/json");
    if is_json {
        let body = Json(LoginResponse {
            user_id: record.user.id,
            username: record.user.username,
            role: record.user.role,
            csrf_token: csrf_token.clone(),
        });

        Ok((
            StatusCode::OK,
            [("set-cookie", cookie_header), ("x-csrf-token", csrf_token)],
            body,
        )
            .into_response())
    } else {
        Ok((
            StatusCode::SEE_OTHER,
            [("location", target_path), ("set-cookie", &cookie_header)],
        )
            .into_response())
    }
}

#[doc = "Spec: AUTH-7"]
async fn post_logout(
    State(state): State<AuthAppState>,
    headers: HeaderMap,
) -> Result<Response, AuthError> {
    let cookie_name = state.config.session_cookie_name();
    let cookie_header = headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(token) = parse_cookie_value(cookie_header, cookie_name) {
        let id_hash = sha256_hex(token.as_bytes());
        let _ = delete_web_session(&state.pool, &id_hash).await;
    }

    let secure_flag = if state.config.dev_insecure_cookies {
        ""
    } else {
        "; Secure"
    };
    let clear_cookie =
        format!("{cookie_name}=; HttpOnly{secure_flag}; SameSite=Strict; Path=/; Max-Age=0");

    Ok((
        StatusCode::OK,
        [("set-cookie", clear_cookie)],
        Json(serde_json::json!({ "status": "logged_out" })),
    )
        .into_response())
}

#[doc = "Spec: AUTH-7"]
async fn get_me(
    State(state): State<AuthAppState>,
    headers: HeaderMap,
) -> Result<Json<User>, AuthError> {
    let session_user = authenticate_session(&state, &headers).await?;
    Ok(Json(session_user.user))
}

/// Authenticate a request via session cookie, verifying idle and absolute expiry per `SEC-5`.
pub async fn authenticate_session(
    state: &AuthAppState,
    headers: &HeaderMap,
) -> Result<SessionWithUser, AuthError> {
    let cookie_name = state.config.session_cookie_name();
    let cookie_header = headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(token) = parse_cookie_value(cookie_header, cookie_name) else {
        return Err(AuthError::Unauthorized);
    };

    let id_hash = sha256_hex(token.as_bytes());
    let session = load_web_session(&state.pool, &id_hash)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::Unauthorized)?;

    let now_ms = state.clock.now_ms();
    // 1. Check absolute expiry
    if now_ms >= session.session.expires_at {
        let _ = delete_web_session(&state.pool, &id_hash).await;
        return Err(AuthError::Unauthorized);
    }

    // 2. Check idle expiry (12 hours)
    if now_ms - session.session.last_seen_at >= SESSION_IDLE_TIMEOUT_MS {
        let _ = delete_web_session(&state.pool, &id_hash).await;
        return Err(AuthError::Unauthorized);
    }

    // 3. Check account disabled
    if session.user.disabled {
        return Err(AuthError::AccountDisabled);
    }

    // 4. Update last_seen_at
    let _ = touch_web_session(&state.pool, &id_hash, now_ms).await;

    Ok(session)
}

/// Validate CSRF token for mutating requests on authenticated routes per `SEC-5`.
pub async fn validate_session_csrf(
    session: &SessionWithUser,
    headers: &HeaderMap,
) -> Result<(), AuthError> {
    let Some(provided_token) = headers.get("x-csrf-token").and_then(|v| v.to_str().ok()) else {
        return Err(AuthError::InvalidCsrf);
    };
    let provided_hash = sha256_hex(provided_token.as_bytes());
    if !bool::from(
        provided_hash
            .as_bytes()
            .ct_eq(session.session.csrf_hash.as_bytes()),
    ) {
        return Err(AuthError::InvalidCsrf);
    }
    Ok(())
}

/// Security headers middleware enforcing CSP and frame guards per `SEC-5`.
pub async fn security_headers_middleware(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    response
}

/// Derives the verifiable per-session CSRF token from the session token.
#[must_use]
pub fn derive_session_csrf(session_token: &str) -> String {
    sha256_hex(format!("llmroller.csrf.v1:{session_token}").as_bytes())
}

pub fn parse_cookie_value<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    for pair in cookie_header.split(';') {
        let mut parts = pair.trim().splitn(2, '=');
        let key = parts.next()?.trim();
        let value = parts.next()?.trim();
        if key == name {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests;
