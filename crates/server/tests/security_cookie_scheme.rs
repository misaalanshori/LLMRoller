//! `Secure` on the admin cookies must follow the request's actual scheme
//! (`ISSUE-0188`, `SEC-4`, `SEC-12`).
//!
//! # The issue is inverted
//!
//! #188 says the cookie "only appends `; Secure` when a static flag is set, ignoring
//! `X-Forwarded-Proto: https`". The code sets `Secure` **unconditionally** unless a dev
//! override is on — so `X-Forwarded-Proto` is not consulted, but neither is it *needed* for
//! safety. The defect is the mirror image: `Secure` is set even over plain HTTP, so the
//! browser refuses to store or return the cookie and **login is impossible** on a deployment
//! that serves the admin plane over plaintext loopback or behind a TLS-terminating proxy
//! whose `X-Forwarded-Proto` the server ignores.
//!
//! # There is no TLS listener, so the proxy header is the *only* signal
//!
//! `serve.rs` binds a plain `TcpListener` and calls plain `axum::serve` for both the admin and
//! data planes — `rustls` appears only as reqwest's *outbound* client. TLS in this product is
//! always terminated upstream: `docs/srdd/22`'s Caddyfile sends
//! `header_up X-Forwarded-Proto {scheme}` to the plaintext listener. So the resolver cannot
//! consult "was this connection TLS"; the honest rule is
//!
//! **`Secure` iff a *trusted* peer said `X-Forwarded-Proto: https`, else no `Secure`.**
//!
//! That is correct for every deployment this code can actually be in, and the trust check is
//! the load-bearing part: an untrusted client must not be able to talk the server into a
//! cookie attribute it cannot use.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tempfile::TempDir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

/// A peer that is inside `TRUSTED_PROXY` and one that is not.
const TRUSTED_PROXY: &str = "172.28.1.10/32";
const TRUSTED_PEER: &str = "172.28.1.10";
const UNTRUSTED_PEER: &str = "203.0.113.9";
const LOOPBACK_PEER: &str = "127.0.0.1";

struct Harness {
    _dir: TempDir,
    pool: sqlx::SqlitePool,
    app: axum::Router,
    user_id: UserId,
}

async fn harness(trusted_proxies: Vec<String>) -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("secure_cookie.db"), PoolRole::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    seed_defaults(&pool, None, 1).await.expect("seed");

    let master_key = MasterKey::from_bytes([0xE3; 32]);
    let instance = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance");

    let user = create_user(
        &pool,
        &ActorScope::system(SystemTask::Writer, SystemSeal::store()),
        NewUser {
            username: "scheme_user",
            password: "Password12345!#",
            role: Role::Admin,
            must_change_password: false,
        },
        &OsEntropy,
        SystemClock::new().now_ms(),
    )
    .await
    .expect("user");

    // `dev_insecure_cookies: false` and `require_tls: true` — the production shape. With the
    // old unconditional rule this is exactly the combination that makes login impossible
    // over plain http.
    let auth_config = AuthConfig {
        dev_insecure_cookies: false,
        require_tls: true,
        trusted_proxies,
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance.session_hmac_secret.clone());
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();

    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: None,
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    Harness {
        _dir: dir,
        pool,
        app,
        user_id: user.id,
    }
}

use std::sync::Arc;

use llmroller_core::seams::SystemClock;

/// Build a request from a chosen peer address, the way `axum::serve` would.
fn request_from(
    method: &str,
    uri: &str,
    peer: &str,
    forwarded_proto: Option<&str>,
    cookie: Option<&str>,
    body: Body,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(proto) = forwarded_proto {
        builder = builder.header("x-forwarded-proto", proto);
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    if method == "POST" {
        builder = builder.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        builder = builder.header("accept", "text/html");
    }
    let mut request = builder.body(body).expect("request");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(addr(peer)));
    request
}

fn addr(ip: &str) -> SocketAddr {
    SocketAddr::new(
        ip.parse::<IpAddr>()
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        41234,
    )
}

fn set_cookie_header(response: &axum::response::Response) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The login-page CSRF cookie, which is the first cookie any visitor receives.
async fn login_page_cookie(harness: &Harness, peer: &str, forwarded: Option<&str>) -> String {
    let response = harness
        .app
        .clone()
        .oneshot(request_from(
            "GET",
            "/login",
            peer,
            forwarded,
            None,
            Body::empty(),
        ))
        .await
        .expect("login page");
    assert_eq!(response.status(), StatusCode::OK);
    set_cookie_header(&response)
}

// ---------------------------------------------------------------------------
// The scheme rule, on the CSRF cookie (the simplest observable)
// ---------------------------------------------------------------------------

/// A trusted proxy saying `https` must still produce `Secure`. This is the behaviour the issue
/// asked for — it already worked, and must keep working.
#[tokio::test]
#[doc = "Spec: SEC-4, SEC-12 — ISSUE-0188 a trusted forwarded https request gets a Secure cookie"]
async fn session_cookie_is_secure_over_a_forwarded_https_request() {
    let h = harness(vec![TRUSTED_PROXY.to_string()]).await;
    let cookie = login_page_cookie(&h, TRUSTED_PEER, Some("https")).await;
    assert!(
        cookie.to_ascii_lowercase().contains("secure"),
        "a trusted proxy reporting https must yield Secure; got {cookie:?}"
    );
}

/// The primary red. Plain http on loopback with no forwarding header must NOT be `Secure`,
/// because a browser discards a `Secure` cookie it received over http — login then cannot work
/// at all.
#[tokio::test]
#[doc = "Spec: SEC-4 — ISSUE-0188 a plain http loopback request gets a non-Secure cookie"]
async fn session_cookie_omits_secure_on_plain_http_loopback() {
    let h = harness(vec![TRUSTED_PROXY.to_string()]).await;
    let cookie = login_page_cookie(&h, LOOPBACK_PEER, None).await;
    assert!(
        !cookie.to_ascii_lowercase().contains("secure"),
        "a plain http request must not receive a Secure cookie, or the browser drops it and \
         login is impossible; got {cookie:?}"
    );
}

/// The security-relevant one. An untrusted peer claiming https must not get `Secure` — it is
/// asserting a fact about a connection it does not control, and honouring it would let anyone
/// influence a cookie attribute.
#[tokio::test]
#[doc = "Spec: SEC-12 — ISSUE-0188 an untrusted forwarded proto cannot force Secure"]
async fn untrusted_forwarded_proto_cannot_force_secure() {
    let h = harness(vec![TRUSTED_PROXY.to_string()]).await;
    let cookie = login_page_cookie(&h, UNTRUSTED_PEER, Some("https")).await;
    assert!(
        !cookie.to_ascii_lowercase().contains("secure"),
        "only a trusted proxy may assert the scheme; got {cookie:?}"
    );
}

/// With no trusted proxies configured at all, even a loopback peer's header is not believed.
#[tokio::test]
#[doc = "Spec: SEC-12 — ISSUE-0188 forwarded proto is ignored when no proxies are trusted"]
async fn forwarded_proto_is_ignored_without_trusted_proxies() {
    let h = harness(Vec::new()).await;
    let cookie = login_page_cookie(&h, LOOPBACK_PEER, Some("https")).await;
    assert!(
        !cookie.to_ascii_lowercase().contains("secure"),
        "with an empty trusted-proxy list there is nobody to trust; got {cookie:?}"
    );
}

// ---------------------------------------------------------------------------
// The same rule on the other two cookie sites
// ---------------------------------------------------------------------------

/// The login page's CSRF cookie and the session cookie are set by different handlers; a
/// half-migration that fixed only one would break the login form.
#[tokio::test]
#[doc = "Spec: SEC-4 — ISSUE-0188 the session cookie follows the same scheme rule"]
async fn session_cookie_and_csrf_cookie_agree() {
    let h = harness(vec![TRUSTED_PROXY.to_string()]).await;

    // Fetch the CSRF cookie/token pair, then log in over plain http.
    let page = h
        .app
        .clone()
        .oneshot(request_from(
            "GET",
            "/login",
            LOOPBACK_PEER,
            None,
            None,
            Body::empty(),
        ))
        .await
        .expect("login page");
    let csrf = page
        .headers()
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .expect("csrf token")
        .to_string();
    let csrf_cookie = page
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(" | ");

    let login = h
        .app
        .clone()
        .oneshot(request_from(
            "POST",
            "/api/auth/login",
            LOOPBACK_PEER,
            None,
            None,
            Body::from(format!(
                "username=scheme_user&password=Password12345!%23&csrf_token={csrf}"
            )),
        ))
        .await
        .expect("login");
    let session_cookie = set_cookie_header(&login);

    assert!(
        !csrf_cookie.to_ascii_lowercase().contains("secure"),
        "the CSRF cookie must also drop Secure over plain http; got {csrf_cookie:?}"
    );
    assert!(
        !session_cookie.to_ascii_lowercase().contains("secure"),
        "the session cookie must not be Secure over plain http; got {session_cookie:?}"
    );
}

/// The third site is `post_logout`'s cookie *clear*. A `Secure` clear over http would be
/// ignored by the browser, leaving a stale session cookie behind after logout.
#[tokio::test]
#[doc = "Spec: SEC-4 — ISSUE-0188 the logout clear-cookie follows the same scheme rule"]
async fn logout_clear_cookie_follows_the_same_rule() {
    let h = harness(vec![TRUSTED_PROXY.to_string()]).await;
    let response = h
        .app
        .clone()
        .oneshot(request_from(
            "POST",
            "/api/auth/logout",
            LOOPBACK_PEER,
            None,
            Some("llmroller_session=not-a-real-session"),
            Body::empty(),
        ))
        .await
        .expect("logout");
    let cookie = set_cookie_header(&response);
    assert!(
        !cookie.to_ascii_lowercase().contains("secure"),
        "a Secure clear over http is ignored by the browser, so logout would not clear the \
         cookie; got {cookie:?}"
    );
}

// ---------------------------------------------------------------------------
// The user-facing receipt
// ---------------------------------------------------------------------------

/// The whole point: a real login over plain http must produce a usable session.
#[tokio::test]
#[doc = "Spec: SEC-4 — ISSUE-0188 login succeeds over plain http loopback"]
async fn login_succeeds_over_plain_http_loopback() {
    let h = harness(vec![TRUSTED_PROXY.to_string()]).await;

    let page = h
        .app
        .clone()
        .oneshot(request_from(
            "GET",
            "/login",
            LOOPBACK_PEER,
            None,
            None,
            Body::empty(),
        ))
        .await
        .expect("login page");
    let csrf = page
        .headers()
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .expect("csrf token")
        .to_string();
    // The login POST must echo the CSRF cookie: the check is a double submit (form token vs
    // cookie), so omitting the cookie fails `invalid_csrf` regardless of scheme.
    let csrf_cookie = page
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");

    let login = h
        .app
        .clone()
        .oneshot(request_from(
            "POST",
            "/api/auth/login",
            LOOPBACK_PEER,
            None,
            Some(&csrf_cookie),
            Body::from(format!(
                "username=scheme_user&password=Password12345!%23&csrf_token={csrf}"
            )),
        ))
        .await
        .expect("login");
    // A test HTTP client echoes whatever cookie it was given, so the redirect alone cannot
    // distinguish "usable by a browser" from "silently discarded". Assert the attribute too:
    // this is what a real browser keys on, and it is what makes this test fail if the
    // resolver regresses to always-Secure.
    let issued = set_cookie_header(&login);
    let login_status = login.status();
    let login_body =
        String::from_utf8_lossy(&login.into_body().collect().await.expect("body").to_bytes())
            .to_string();
    assert!(
        !issued.to_ascii_lowercase().contains("secure"),
        "the session cookie handed to an http client must not be Secure, or the browser drops \
         it and this redirect is unreachable in practice; got {issued:?}"
    );
    assert_eq!(
        login_status,
        StatusCode::SEE_OTHER,
        "a correct password over http must log in; body was {login_body}"
    );

    // And the store now holds a session for the user, which is the durable receipt.
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE user_id = ?")
        .bind(h.user_id.to_string())
        .fetch_one(&h.pool)
        .await
        .expect("count sessions");
    assert_eq!(
        sessions, 1,
        "a session row must exist after a successful login"
    );
}
