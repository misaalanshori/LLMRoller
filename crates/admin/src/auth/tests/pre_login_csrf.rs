//! Pre-login double-submit CSRF tests (`SEC-5`, `ISSUE-0046`, `ISSUE-0045`).
//!
//! `get_login_page` mints a random nonce, sets it as the cookie **and** echoes it in
//! `x-csrf-token`; `post_login` accepts the pair only when they match. Signing the
//! cookie was declined (see the issue) because it stops the same attacker for the
//! same reason. What remains is the comparison itself: it must not leak the token
//! length, so both sides are compared as fixed-width digests.

use std::net::{IpAddr, SocketAddr};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use sqlx::SqlitePool;
use tower::ServiceExt as _;

use crate::auth::{AuthAppState, AuthConfig, router};

const LOGIN_COOKIE: &str = "llmroller_login_csrf";

async fn empty_state() -> AuthAppState {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory pool");
    AuthAppState::new(
        pool,
        AuthConfig {
            dev_insecure_cookies: true,
            require_tls: false,
            trusted_proxies: vec![],
        },
    )
}

/// Drive `POST /api/auth/login` with an explicit cookie/header pair.
///
/// The body is malformed and the user does not exist, so a passing CSRF check
/// can only lead to a credential failure. That isolates the CSRF decision: any
/// `403` returned here comes from the CSRF gate, not from the rest of `post_login`.
async fn post_login_with(cookie: Option<&str>, header_token: Option<&str>) -> StatusCode {
    let state = empty_state().await;
    let app = router(state);

    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(SocketAddr::new(
            IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1)),
            40000,
        )));

    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    if let Some(header_token) = header_token {
        builder = builder.header("x-csrf-token", header_token);
    }

    let response = app
        .oneshot(
            builder
                .body(Body::from(
                    r#"{"username":"nobody_here","password":"wrong-password-123"}"#.to_string(),
                ))
                .expect("build request"),
        )
        .await
        .expect("router call");
    let status = response.status();
    let _ = axum::body::to_bytes(response.into_body(), 64 * 1024).await;
    status
}

fn cookie_pair(nonce: &str) -> String {
    format!("{LOGIN_COOKIE}={nonce}")
}

/// A cookie and header that disagree must be rejected as `invalid_csrf`.
#[tokio::test]
#[doc = "Spec: SEC-5"]
async fn pre_login_csrf_rejects_a_mismatched_pair() {
    let status = post_login_with(
        Some(&cookie_pair("nonce-from-cookie")),
        Some("nonce-from-header"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a cookie/header mismatch must be rejected with 403"
    );
}

/// A header that is a strict prefix of the cookie must be rejected.
///
/// `subtle::ConstantTimeEq` returns early on unequal lengths, so the raw
/// comparison happened to get this right; comparing fixed-width digests makes it
/// true by construction. This test pins the property rather than demonstrating a
/// bug, so a future token-format change cannot silently weaken it.
#[tokio::test]
#[doc = "Spec: SEC-5"]
async fn pre_login_csrf_rejects_a_truncated_header() {
    let nonce = "0123456789abcdef0123456789abcdef";
    let truncated = &nonce[..nonce.len() - 1];
    let status = post_login_with(Some(&cookie_pair(nonce)), Some(truncated)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a truncated header must be rejected with 403"
    );
}

/// A missing cookie or a missing header must be rejected, not treated as a match.
#[tokio::test]
#[doc = "Spec: SEC-5"]
async fn pre_login_csrf_rejects_a_missing_half() {
    let nonce = "0123456789abcdef0123456789abcdef";

    let no_cookie = post_login_with(None, Some(nonce)).await;
    assert_eq!(
        no_cookie,
        StatusCode::FORBIDDEN,
        "a login without the CSRF cookie must be rejected"
    );

    let no_header = post_login_with(Some(&cookie_pair(nonce)), None).await;
    assert_eq!(
        no_header,
        StatusCode::FORBIDDEN,
        "a login without the CSRF header must be rejected"
    );
}

/// A matching pair must get past the CSRF gate.
///
/// The credentials are wrong, so the expected outcome is a credential failure —
/// what matters is that it is *not* `invalid_csrf`. Without this, every
/// rejection test above would still pass if the gate rejected everything.
#[tokio::test]
#[doc = "Spec: SEC-5"]
async fn pre_login_csrf_accepts_a_matching_pair() {
    let nonce = "0123456789abcdef0123456789abcdef";
    let status = post_login_with(Some(&cookie_pair(nonce)), Some(nonce)).await;
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "a matching cookie/header pair must pass the CSRF gate; got {status}"
    );
}

/// The rendered page must echo its cookie nonce in `x-csrf-token`, otherwise no
/// real login can ever satisfy the double-submit check.
#[tokio::test]
#[doc = "Spec: SEC-5"]
async fn login_page_echoes_its_cookie_nonce_in_the_header() {
    let state = empty_state().await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/login")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router call");
    assert_eq!(response.status(), StatusCode::OK);

    let cookie_header = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("login page must set a CSRF cookie")
        .to_str()
        .expect("cookie is ASCII")
        .to_string();
    let echoed = response
        .headers()
        .get("x-csrf-token")
        .expect("login page must echo x-csrf-token")
        .to_str()
        .expect("token is ASCII")
        .to_string();

    let cookie_nonce = cookie_header
        .split(';')
        .next()
        .expect("cookie pair")
        .split_once('=')
        .expect("cookie name=value")
        .1;

    assert_eq!(
        cookie_nonce, echoed,
        "the double-submit check requires the cookie nonce and the header to be the same value"
    );
    assert!(
        !cookie_nonce.is_empty(),
        "the nonce must not be empty, or an empty pair would trivially match"
    );
}
