#![forbid(unsafe_code)]
//! Web authentication, sessions, CSRF, and three-tier rate limiting.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

mod login_limiter;

pub use login_limiter::{
    LimiterOutcome, MAX_FAILURES_PER_IP_15M, MAX_FAILURES_PER_USER_IP_15M,
    MAX_TRACKED_LIMITER_KEYS, NormalizedClientIp, ThreeTierLoginLimiter,
    USERNAME_ATTACK_THRESHOLD_1H,
};

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
use llmroller_core::seams::{
    Clock, EntropySource, OsEntropy, SecretBytes, SystemClock, secret_bytes,
};
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

/// The cookie attributes a given request's scheme allows (`ISSUE-0188`, `SEC-4`).
///
/// Name and `Secure` are decided **together** and must never be split. The `__Host-` prefix is
/// a browser-enforced contract: a cookie named `__Host-…` must carry `Secure` (and no `Domain`,
/// with `Path=/`), and one that does not is rejected outright. So dropping `Secure` while
/// keeping the prefix produces an invalid cookie, and keeping `Secure` while dropping the
/// prefix gives up a defence the browser would otherwise enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CookiePolicy {
    /// Whether the cookie must be marked `Secure`, and whether the `__Host-` names are usable.
    pub secure: bool,
    /// The prefix the request's scheme supports.
    pub prefix: &'static str,
}

impl CookiePolicy {
    /// Name for the session cookie under this policy.
    #[must_use]
    pub const fn session_cookie_name(self) -> &'static str {
        if self.secure {
            SESSION_COOKIE_NAME
        } else {
            DEV_SESSION_COOKIE_NAME
        }
    }

    /// Name for the login CSRF cookie under this policy.
    #[must_use]
    pub const fn login_csrf_cookie_name(self) -> &'static str {
        if self.secure {
            LOGIN_CSRF_COOKIE_NAME
        } else {
            DEV_LOGIN_CSRF_COOKIE_NAME
        }
    }

    /// The `; Secure` fragment, or empty for a plaintext request.
    #[must_use]
    pub const fn secure_flag(self) -> &'static str {
        if self.secure { "; Secure" } else { "" }
    }
}

/// Read the session token from a cookie header, accepting **either** name.
///
/// Writes are scheme-dependent (`__Host-llmroller_session` over TLS, `llmroller_session`
/// otherwise), but reads must not be: a user who established a session over https and then
/// reaches the instance over http — or the reverse, during a proxy cutover — must not be
/// silently logged out because the reader only knew one name (`ISSUE-0188`).
#[must_use]
pub fn session_token_from_cookie_header(cookie_header: &str) -> Option<String> {
    parse_cookie_value(cookie_header, SESSION_COOKIE_NAME)
        .or_else(|| parse_cookie_value(cookie_header, DEV_SESSION_COOKIE_NAME))
        .map(ToString::to_string)
}

/// Resolve the cookie policy for one request (`ISSUE-0188`).
///
/// **The rule is: `Secure` iff the request actually arrived over TLS.** There is no TLS
/// listener in this product — `serve.rs` binds a plain `TcpListener` and calls plain
/// `axum::serve`, with `rustls` used only as reqwest's outbound client — so in practice TLS is
/// always terminated upstream and the only signal available is `X-Forwarded-Proto` from a
/// peer we trust (`docs/srdd/22`'s Caddyfile sends it to the plaintext listener).
///
/// The trust check is load-bearing: an untrusted peer asserting `https` must not be believed,
/// or any client could talk the server into a cookie it cannot use. This reuses the same
/// trusted-peer CIDR matching as [`extract_client_ip`] rather than introducing a second one.
#[must_use]
pub fn cookie_policy_for(
    config: &AuthConfig,
    peer_addr: SocketAddr,
    headers: &HeaderMap,
) -> CookiePolicy {
    // An explicit dev override still wins: it exists to make a deliberately insecure local
    // setup usable, and it is what the Compose dev profile sets.
    if config.dev_insecure_cookies {
        return CookiePolicy {
            secure: false,
            prefix: "",
        };
    }
    let secure = peer_is_trusted_proxy(peer_addr.ip(), &config.trusted_proxies)
        && headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|proto| {
                // Proxies may append, e.g. `https,http`; the first entry is the client-facing
                // scheme.
                proto
                    .split(',')
                    .next()
                    .is_some_and(|first| first.trim().eq_ignore_ascii_case("https"))
            });
    CookiePolicy {
        secure,
        prefix: if secure { "__Host-" } else { "" },
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

/// The connecting peer's address, defaulting to loopback when the router was built without
/// `ConnectInfo` (in-process tests).
#[must_use]
pub fn peer_addr_from(extensions: &axum::http::Extensions) -> SocketAddr {
    extensions
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map_or_else(
            || SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8081),
            |ci| ci.0,
        )
}

/// Whether `peer_ip` is inside `trusted_proxies` (`SEC-12`).
///
/// One matcher for both forwarded-header readers — [`extract_client_ip`] for
/// `X-Forwarded-For` and [`cookie_policy_for`] for `X-Forwarded-Proto` — so the two can never
/// disagree about who is trusted.
fn peer_is_trusted_proxy(peer_ip: IpAddr, trusted_proxies: &[String]) -> bool {
    if trusted_proxies.is_empty() {
        return false;
    }
    let peer_str = peer_ip.to_string();
    trusted_proxies
        .iter()
        .any(|p| p.trim() == peer_str || cidr_contains(p.trim(), peer_ip))
}

/// Extract the effective client IP honoring trusted proxies per `SEC-12`.
#[must_use]
pub fn extract_client_ip(
    peer_addr: SocketAddr,
    headers: &HeaderMap,
    trusted_proxies: &[String],
) -> IpAddr {
    let peer_ip = peer_addr.ip();
    if !peer_is_trusted_proxy(peer_ip, trusted_proxies) {
        return peer_ip;
    }

    // Parse X-Forwarded-For from right to left
    let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) else {
        return peer_ip;
    };

    let entries: Vec<&str> = xff.split(',').map(str::trim).collect();
    for entry in entries.into_iter().rev() {
        let Ok(ip) = entry.parse::<IpAddr>() else {
            break;
        };
        let is_proxy = trusted_proxies
            .iter()
            .any(|p| p.trim() == entry || cidr_contains(p.trim(), ip));
        if !is_proxy {
            return ip;
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
    /// Read-only connection pool (`SELECT` paths) kept separate from the single
    /// serialized writer connection (`REL-2`, F036). Defaults to `pool` when no
    /// reader pool is attached (unit tests and CLI commands).
    pub reader_pool: SqlitePool,
    pub limiter: Arc<ThreeTierLoginLimiter>,
    pub config: AuthConfig,
    pub clock: Arc<dyn Clock>,
    pub entropy: Arc<dyn EntropySource>,
    pub argon2_semaphore: Arc<Semaphore>,
    /// Master key used to open sealed TOTP secrets during login.
    pub master_key: Option<llmroller_store::crypto::MasterKey>,
    /// Identifier of the active master key, bound into the TOTP AAD.
    pub master_key_id: String,
    pub audit_hmac_key: Option<llmroller_core::seams::SecretBytes>,
    /// Instance-wide session secret, shared with the data plane's `session_hash`
    /// derivation and used as the CSRF key (`SEC-5`, `FND-041`).
    ///
    /// The production path always supplies this from `instance_secrets`. When it
    /// is absent — unit tests and CLI paths that construct this state without
    /// instance secrets — CSRF derivation falls back to
    /// [`Self::csrf_fallback_key`], which is **per-process random**, never a
    /// compile-time constant, so a token stays unforgeable without the session
    /// token even on that path. The fallback does not survive a restart, which is
    /// correct: CSRF tokens are per-rendered-page and are re-minted on every
    /// login.
    pub session_hmac_secret: Option<SecretBytes>,
    /// Per-process random key used only when `session_hmac_secret` is absent.
    csrf_fallback_key: SecretBytes,
    pub writer: Option<llmroller_store::writer::WriterHandle>,
    /// Short-lived replay cache `(user_id, totp_code) -> expires_at_ms` rejecting reuse within 90s (`AUTH-3`, `F-113`).
    pub used_totp_codes: Arc<Mutex<HashMap<(UserId, String), i64>>>,
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
        let entropy = OsEntropy::new();
        let mut fallback_key = [0_u8; 32];
        entropy.fill_bytes(&mut fallback_key);
        Self {
            reader_pool: pool.clone(),
            pool,
            limiter: Arc::new(ThreeTierLoginLimiter::new()),
            config,
            clock: Arc::new(SystemClock::new()),
            entropy: Arc::new(OsEntropy::new()),
            argon2_semaphore: Arc::new(Semaphore::new(parallelism)),
            master_key: None,
            master_key_id: String::new(),
            audit_hmac_key: None,
            session_hmac_secret: None,
            csrf_fallback_key: secret_bytes(fallback_key.to_vec()),
            writer: None,
            used_totp_codes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Derive the verifiable per-session CSRF token for this instance (`SEC-5`).
    ///
    /// Uses the instance session secret when present, otherwise the per-process
    /// fallback key. Callers must not pass a token derived by a different state
    /// instance and expect it to validate, unless both share the same instance
    /// secret.
    #[must_use]
    pub fn derive_session_csrf(&self, session_token: &str) -> String {
        derive_session_csrf_with_secret(self.csrf_key(), session_token)
    }

    /// The key this state uses for CSRF derivation.
    fn csrf_key(&self) -> &[u8] {
        use secrecy::ExposeSecret as _;
        match &self.session_hmac_secret {
            Some(secret) => secret.expose_secret(),
            None => self.csrf_fallback_key.expose_secret(),
        }
    }

    /// Reject replayed 6-digit TOTP codes within their 90-second (`±1` step) validity window (`AUTH-3`, `F-113`).
    pub fn check_and_record_totp_replay(&self, user_id: UserId, code: &str, now_ms: i64) -> bool {
        let normalized = code.trim().to_string();
        let mut cache = self.used_totp_codes.lock();
        cache.retain(|_, &mut exp| exp > now_ms);
        let key = (user_id, normalized);
        if cache.get(&key).is_some_and(|&exp| exp > now_ms) {
            return false;
        }
        cache.insert(key, now_ms + 90_000);
        true
    }

    /// Attach the eight-connection read-only pool used for session and login reads
    /// so they no longer serialize behind the writer actor (F036, REL-2).
    #[must_use]
    pub fn with_reader_pool(mut self, reader_pool: SqlitePool) -> Self {
        self.reader_pool = reader_pool;
        self
    }

    /// Read `instance_meta.instance_id`, which is the **HKDF salt** for every sealer subkey
    /// (`store/src/crypto.rs:303`).
    ///
    /// A substitute here is not a cosmetic default: a zero salt derives a *different* key, so any
    /// row sealed under it becomes permanently unreadable once the real id loads — lost upstream
    /// credentials and locked-out TOTP accounts, with no error reported (`ISSUE-0229`).
    ///
    /// Both failures the old code swallowed now propagate:
    ///
    /// ```ignore
    /// .fetch_optional(…)               // a missing row became `None`
    /// .map_err(|_| AuthError::Internal)?  // <- previously `.ok()`
    /// .unwrap_or_else(|| vec![0; 16])     // <- the substitution
    /// … .try_into().unwrap_or([0; 16])    // <- and a bad length
    /// ```
    ///
    /// `fetch_one` errors on a missing row; the length is checked explicitly because the schema's
    /// `CHECK (length(instance_id) = 16)` can be bypassed by a direct writer or a partial restore.
    async fn load_instance_id(&self) -> Result<[u8; 16], AuthError> {
        let bytes: Vec<u8> =
            sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
                .fetch_one(&self.pool)
                .await
                .map_err(|_| AuthError::Internal)?;
        bytes.try_into().map_err(|_| AuthError::Internal)
    }

    /// Test-only entry point for the TOTP verification seam.
    #[cfg(test)]
    pub(crate) async fn verify_totp_code_for_test(
        &self,
        user_id: llmroller_core::ids::UserId,
        code: &str,
        now_ms: i64,
    ) -> Result<bool, AuthError> {
        self.verify_totp_code(user_id, code, now_ms).await
    }

    /// Verify a login TOTP or single-use recovery code, advancing replay state.
    async fn verify_totp_code(
        &self,
        user_id: llmroller_core::ids::UserId,
        code: &str,
        now_ms: i64,
    ) -> Result<bool, AuthError> {
        let master_key = self.master_key.clone().ok_or(AuthError::Internal)?;
        let instance_id = self.load_instance_id().await?;
        let sealer = llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
            master_key,
            instance_id,
            Arc::new(OsEntropy::new()),
        );
        if !self.check_and_record_totp_replay(user_id, code, now_ms) {
            return Ok(false);
        }
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
            | Err(llmroller_store::users::UserError::TotpReplay) => {
                self.used_totp_codes
                    .lock()
                    .remove(&(user_id, code.trim().to_string()));
                Ok(false)
            }
            Err(_) => {
                self.used_totp_codes
                    .lock()
                    .remove(&(user_id, code.trim().to_string()));
                Err(AuthError::Internal)
            }
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
    let count = count_users(&state.reader_pool).await.unwrap_or(0);
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

use askama::Template as _;

#[derive(Debug, askama::Template)]
#[template(path = "login.html")]
pub struct LoginTemplate {
    pub csrf_token: String,
}

#[doc = "Spec: SEC-5"]
async fn get_login_page(State(state): State<AuthAppState>, parts: Parts) -> Response {
    let mut raw_csrf = [0_u8; 32];
    state.entropy.fill_bytes(&mut raw_csrf);
    let csrf_token = URL_SAFE_NO_PAD.encode(raw_csrf);

    let policy = cookie_policy_for(
        &state.config,
        peer_addr_from(&parts.extensions),
        &parts.headers,
    );
    let cookie_name = policy.login_csrf_cookie_name();
    let secure_flag = policy.secure_flag();
    let cookie_header = format!(
        "{cookie_name}={csrf_token}; HttpOnly{secure_flag}; SameSite=Strict; Path=/; Max-Age={LOGIN_CSRF_MAX_AGE_SECS}"
    );

    let template = LoginTemplate {
        csrf_token: csrf_token.clone(),
    };
    let html = template.render().unwrap_or_default();

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

async fn log_login_failed(
    state: &AuthAppState,
    username: &str,
    ip: IpAddr,
    reason: &str,
    now_ms: i64,
) {
    let Some(ref audit_key) = state.audit_hmac_key else {
        return;
    };
    use llmroller_core::seams::ExposeSecret as _;
    let audit_id = llmroller_core::ids::RequestId::generate().to_string();
    let ip_str = ip.to_string();
    let detail = format!("{{\"username\":{username:?},\"reason\":{reason:?}}}");
    if let Ok(mut tx) = state.pool.begin().await {
        let _ = llmroller_store::audit::append(
            &mut tx,
            audit_key.expose_secret(),
            llmroller_store::audit::AuditEntry {
                id: &audit_id,
                ts: now_ms,
                actor_user_id: None,
                ip_addr: Some(&ip_str),
                action: "auth.login.failed",
                target_type: "user",
                target_id: Some(username),
                detail_json: Some(&detail),
            },
        )
        .await;
        let _ = tx.commit().await;
    }
}

/// Bounded wait for one Argon2id permit.
///
/// A login burst can legitimately queue behind in-flight verifications, so the
/// wait is bounded rather than unbounded; when the bound is reached the caller
/// receives `503 login_busy` and can retry. Five seconds matches the historical
/// outer bound and is long enough to absorb a full queue of realistic logins.
const ARGON2_PERMIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of permits in the shared Argon2id pool.
#[must_use]
pub fn argon2_permit_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .max(2)
}

/// Verify a password while holding exactly one Argon2id permit.
///
/// The permit bounds concurrent Argon2id work, which is the whole point of the
/// semaphore, and it is released as soon as the blocking task finishes so the
/// caller's rate-limit and audit work never pins it (`ISSUE-0001`).
///
/// Every Argon2id invocation in the login path goes through this helper — the
/// real verification and the constant-time dummy verification used for
/// user-enumeration defence — so both are bounded identically.
async fn verify_password_with_permit(
    state: &AuthAppState,
    password: String,
    password_hash: String,
) -> Result<bool, AuthError> {
    let _permit = tokio::time::timeout(ARGON2_PERMIT_TIMEOUT, state.argon2_semaphore.acquire())
        .await
        .map_err(|_| AuthError::LoginBusy)?
        .map_err(|_| AuthError::Internal)?;
    tokio::task::spawn_blocking(move || verify_password(&password, &password_hash))
        .await
        .map_err(|_| AuthError::Internal)
}

#[doc = "Spec: AUTH-7, Spec: AUTH-8, Spec: SEC-4, Spec: SEC-5"]
async fn post_login(
    State(state): State<AuthAppState>,
    parts: Parts,
    body: Bytes,
) -> Result<Response, AuthError> {
    let now_ms = state.clock.now_ms();
    let peer_addr = peer_addr_from(&parts.extensions);
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
    let cookie_header = parts
        .headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Accept either name: the cookie may have been issued before a scheme change.
    let expected_csrf = parse_cookie_value(cookie_header, LOGIN_CSRF_COOKIE_NAME)
        .or_else(|| parse_cookie_value(cookie_header, DEV_LOGIN_CSRF_COOKIE_NAME));

    let header_csrf = parts
        .headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or(form_csrf);

    let (Some(expected), Some(header_token)) = (expected_csrf, header_csrf) else {
        return Err(AuthError::InvalidCsrf);
    };
    // Double-submit comparison. Both sides are hashed first so the compared values
    // are fixed-width regardless of the token format: `ct_eq` for byte slices
    // returns early on unequal lengths, which would leak the token length by
    // timing the moment the format stopped being a constant-width nonce
    // (`SEC-5`, `ISSUE-0045`). This also matches the digest comparison the
    // authenticated-session path already uses.
    if expected.is_empty()
        || !bool::from(
            sha256_hex(expected.as_bytes())
                .as_bytes()
                .ct_eq(sha256_hex(header_token.as_bytes()).as_bytes()),
        )
    {
        return Err(AuthError::InvalidCsrf);
    }

    // 2. Evaluate three-tier rate limiter before Argon2id
    let limiter_outcome = state.limiter.check(&payload.username, client_ip, now_ms)?;

    // 3. Lookup the user before taking any Argon2id permit. A slow or blocked
    //    reader must not pin a verification permit, and the two early-return
    //    branches below perform no password work at all (ISSUE-0001).
    const DUMMY_ARGON2_PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHR2YWx1ZTEyMzQ1Ng$RSub7nN5qN1sK7O+8m+6P0vK3X2zW9Y1qP4L5M6N7O8";
    let record = match find_login_record(&state.reader_pool, &payload.username).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            // Constant-time user-enumeration defence (`F081`): verify against a
            // dummy PHC through the same single-permit helper, so this Argon2id
            // call is bounded exactly like a real one. The join handle is awaited
            // so the work actually completes before the timing comparison is made.
            let _ = verify_password_with_permit(
                &state,
                payload.password.clone(),
                DUMMY_ARGON2_PHC.to_string(),
            )
            .await?;
            state
                .limiter
                .record_failure(&payload.username, client_ip, now_ms);
            log_login_failed(
                &state,
                &payload.username,
                client_ip,
                "user_not_found",
                now_ms,
            )
            .await;
            return Err(AuthError::InvalidCredentials);
        }
        Err(_) => return Err(AuthError::Internal),
    };

    if record.user.disabled {
        state
            .limiter
            .record_failure(&payload.username, client_ip, now_ms);
        log_login_failed(
            &state,
            &payload.username,
            client_ip,
            "account_disabled",
            now_ms,
        )
        .await;
        return Err(AuthError::AccountDisabled);
    }

    // 4. Verify the password holding exactly one permit for the duration of the
    //    Argon2id work only (ISSUE-0001). The helper releases the permit before
    //    returning, so the rate-limit and audit work below never pins it.
    let verified = verify_password_with_permit(
        &state,
        payload.password.clone(),
        record.password_hash.clone(),
    )
    .await?;

    if !verified {
        state
            .limiter
            .record_failure(&payload.username, client_ip, now_ms);
        log_login_failed(
            &state,
            &payload.username,
            client_ip,
            "invalid_password",
            now_ms,
        )
        .await;
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
            log_login_failed(&state, &payload.username, client_ip, "invalid_totp", now_ms).await;
            return Err(AuthError::InvalidCredentials);
        }
    } else if limiter_outcome == LimiterOutcome::EnforceTotpOrDelay {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Record login success in rate limiter and update users.last_login_at (F081)
    state.limiter.record_success(&payload.username, client_ip);
    let _ = llmroller_store::users::mark_login(&state.pool, record.user.id, now_ms).await;

    // Audit log success (AUD-1, SEC-13, F048)
    if let Some(ref audit_key) = state.audit_hmac_key {
        use llmroller_core::seams::ExposeSecret as _;
        let audit_id = llmroller_core::ids::RequestId::generate().to_string();
        let uid_str = record.user.id.to_string();
        let ip_str = client_ip.to_string();
        if let Ok(mut tx) = state.pool.begin().await {
            let _ = llmroller_store::audit::append(
                &mut tx,
                audit_key.expose_secret(),
                llmroller_store::audit::AuditEntry {
                    id: &audit_id,
                    ts: now_ms,
                    actor_user_id: Some(&uid_str),
                    ip_addr: Some(&ip_str),
                    action: "auth.login.succeeded",
                    target_type: "user",
                    target_id: Some(&uid_str),
                    detail_json: Some("{}"),
                },
            )
            .await;
            let _ = tx.commit().await;
        }
    }

    // 6. Issue 256-bit session token and random 256-bit CSRF token per AUTH-7 / SEC-5
    let mut raw_session = [0_u8; 32];
    state.entropy.fill_bytes(&mut raw_session);
    let session_token = URL_SAFE_NO_PAD.encode(raw_session);
    let session_id_hash = sha256_hex(session_token.as_bytes());

    let csrf_token = state.derive_session_csrf(&session_token);
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

    let policy = cookie_policy_for(&state.config, peer_addr, &parts.headers);
    let cookie_name = policy.session_cookie_name();
    let secure_flag = policy.secure_flag();
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
    parts: Parts,
) -> Result<Response, AuthError> {
    let headers = parts.headers.clone();
    let cookie_header = headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(token) = session_token_from_cookie_header(cookie_header) {
        let id_hash = sha256_hex(token.as_bytes());
        let session = load_web_session(&state.reader_pool, &id_hash)
            .await
            .ok()
            .flatten();
        let _ = delete_web_session(&state.pool, &id_hash).await;

        if let Some(ref audit_key) = state.audit_hmac_key {
            use llmroller_core::seams::ExposeSecret as _;
            let audit_id = llmroller_core::ids::RequestId::generate().to_string();
            let uid_str = session.as_ref().map(|s| s.user.id.to_string());
            let now_ms = state.clock.now_ms();
            if let Ok(mut tx) = state.pool.begin().await {
                let _ = llmroller_store::audit::append(
                    &mut tx,
                    audit_key.expose_secret(),
                    llmroller_store::audit::AuditEntry {
                        id: &audit_id,
                        ts: now_ms,
                        actor_user_id: uid_str.as_deref(),
                        ip_addr: None,
                        action: "auth.logout",
                        target_type: "web_session",
                        target_id: Some(&id_hash),
                        detail_json: Some("{}"),
                    },
                )
                .await;
                let _ = tx.commit().await;
            }
        }
    }

    let policy = cookie_policy_for(&state.config, peer_addr_from(&parts.extensions), &headers);
    let clear_cookie = format!(
        "{}=; HttpOnly{}; SameSite=Strict; Path=/; Max-Age=0",
        policy.session_cookie_name(),
        policy.secure_flag()
    );

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
    let cookie_header = headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(token) = session_token_from_cookie_header(cookie_header) else {
        return Err(AuthError::Unauthorized);
    };

    let id_hash = sha256_hex(token.as_bytes());
    let session = load_web_session(&state.reader_pool, &id_hash)
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

    // 4. Throttle last_seen_at updates to at most once every 5 minutes (300_000 ms) to prevent SQLite write lock contention (FND-042)
    if now_ms.saturating_sub(session.session.last_seen_at) >= 300_000 {
        let _ = touch_web_session(&state.pool, &id_hash, now_ms).await;
    }

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

/// Derive a keyed CSRF token from a session token (`SEC-5`, `FND-041`).
///
/// `HMAC-SHA256(key = instance secret, message = domain separator || session token)`,
/// rendered as lowercase hex. The `b"llmroller.csrf.v1:"` domain separator keeps
/// this derivation separated from the data plane's use of the same
/// `session_hmac_secret` for `session_hash`.
///
/// The key must be supplied by the caller. There is deliberately no "derive with
/// no key" form: a public or absent key would make the token reproducible by
/// anyone who learns the session token, which is exactly the property the header
/// exists to add.
#[must_use]
pub fn derive_session_csrf_with_secret(secret: &[u8], session_token: &str) -> String {
    use hmac::{Hmac, Mac as _};
    let mut mac = <Hmac<sha2::Sha256> as hmac::KeyInit>::new_from_slice(secret)
        .unwrap_or_else(|_| unreachable!("HMAC-SHA256 accepts keys of any length"));
    mac.update(b"llmroller.csrf.v1:");
    mac.update(session_token.as_bytes());
    let digest = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
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
