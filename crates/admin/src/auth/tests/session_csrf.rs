//! Session CSRF derivation tests (`SEC-5`, `FND-041`, `ISSUE-0002`).
//!
//! The authenticated-session CSRF token must be a keyed derivation over a
//! per-instance secret. It must never be reproducible from a value that is
//! visible in the source tree, otherwise anyone who learns a session token can
//! compute the matching CSRF header offline and the header stops being an
//! independent proof.

use super::super::{
    AuthAppState, AuthConfig, derive_session_csrf_with_secret, validate_session_csrf,
};
use axum::http::{HeaderMap, header};
use llmroller_store::crypto::sha256_hex;
use sqlx::SqlitePool;

/// A session token of the shape `post_login` issues (256-bit, base64url).
const SESSION_TOKEN: &str = "session-token-for-csrf-derivation-tests";

/// Two different 32-byte secrets for the keyed-derivation test. Test fixtures
/// only; these are not, and must never be, real instance secrets.
const SECRET_A: [u8; 32] = [0x11; 32];
const SECRET_B: [u8; 32] = [0x22; 32];

async fn empty_state() -> AuthAppState {
    // A connection pool is required by `AuthAppState` but is never touched by the
    // CSRF derivation itself, so an un-migrated in-memory pool is enough.
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

/// The token must change when the instance secret changes.
///
/// This passes on the pre-fix tree too (the secret argument was already
/// honoured); it is kept because it pins the property that makes the derivation
/// keyed at all, and it is the natural pair to the red test below.
#[tokio::test]
#[doc = "Spec: SEC-5"]
async fn session_csrf_depends_on_the_instance_secret() {
    let with_a = derive_session_csrf_with_secret(&SECRET_A, SESSION_TOKEN);
    let with_b = derive_session_csrf_with_secret(&SECRET_B, SESSION_TOKEN);
    assert_ne!(
        with_a, with_b,
        "two different instance secrets must derive two different CSRF tokens"
    );
}

/// The token must not be reproducible from anything visible in the source.
///
/// Two `AuthAppState`s that were never given an instance secret must derive
/// *different* tokens for the same session token, because the fallback key is
/// per-process random rather than a compile-time constant. This fails while the
/// fallback is the literal `b"llmroller.csrf.v1.default_hmac_key"`.
#[tokio::test]
#[doc = "Spec: SEC-5"]
async fn session_csrf_is_not_derived_from_a_public_constant() {
    let first = empty_state().await;
    let second = empty_state().await;

    let from_first = first.derive_session_csrf(SESSION_TOKEN);
    let from_second = second.derive_session_csrf(SESSION_TOKEN);

    assert_ne!(
        from_first, from_second,
        "two independent states without an instance secret must not derive the \
         same CSRF token for the same session token; the fallback key must be \
         per-process random, not a public constant"
    );

    // The documented public constant must not reproduce either token.
    let public_constant =
        derive_session_csrf_with_secret(b"llmroller.csrf.v1.default_hmac_key", SESSION_TOKEN);
    assert_ne!(
        from_first, public_constant,
        "the CSRF token must not equal the value derived from the public constant"
    );
}

/// Rendering and validation must agree, and the CSRF comparison must be a
/// fixed-width digest comparison.
///
/// `post_login` renders `derive_session_csrf(...)` and stores
/// `sha256_hex(token)`; `validate_session_csrf` hashes the presented header and
/// compares. This test pins both halves of that pair together so a change to one
/// side cannot silently pass.
#[tokio::test]
#[doc = "Spec: SEC-5"]
async fn session_csrf_matches_between_render_and_validation() {
    let state = empty_state().await;
    let rendered = state.derive_session_csrf(SESSION_TOKEN);
    let stored_hash = sha256_hex(rendered.as_bytes());

    let session = fake_session(stored_hash.clone());

    let mut accepting = HeaderMap::new();
    accepting.insert("x-csrf-token", rendered.parse().expect("header value"));
    assert!(
        validate_session_csrf(&session, &accepting).await.is_ok(),
        "the rendered token must be accepted by the validator"
    );

    let mut rejecting = HeaderMap::new();
    rejecting.insert("x-csrf-token", "wrong-token".parse().expect("header value"));
    assert!(
        validate_session_csrf(&session, &rejecting).await.is_err(),
        "a token that was not rendered must be rejected"
    );

    let mut missing = HeaderMap::new();
    missing.insert(header::ACCEPT, "application/json".parse().expect("header"));
    assert!(
        validate_session_csrf(&session, &missing).await.is_err(),
        "a request without the CSRF header must be rejected"
    );
}

/// Two states that share an instance secret must agree, which is what lets a
/// token rendered by one handler be validated by another.
#[tokio::test]
#[doc = "Spec: SEC-5"]
async fn session_csrf_is_stable_for_the_same_secret() {
    let secret = SECRET_A;
    let first = derive_session_csrf_with_secret(&secret, SESSION_TOKEN);
    let second = derive_session_csrf_with_secret(&secret, SESSION_TOKEN);
    assert_eq!(
        first, second,
        "same secret and token must derive the same value"
    );
}

/// Build the minimum `SessionWithUser` shape `validate_session_csrf` reads.
fn fake_session(csrf_hash: String) -> llmroller_store::users::SessionWithUser {
    use llmroller_core::ids::UserId;
    use llmroller_store::users::{User, WebSession};

    let now_ms = 1_700_000_000_000;
    let user = User {
        id: UserId::generate(),
        username: "csrf-fixture".to_string(),
        role: llmroller_core::rbac::Role::User,
        must_change_password: false,
        totp_enabled: false,
        can_use_global_providers: true,
        global_budget_micro_usd_month: None,
        disabled: false,
        created_at: now_ms,
        last_login_at: None,
    };
    let session = WebSession {
        id_hash: sha256_hex(SESSION_TOKEN.as_bytes()),
        user_id: user.id,
        csrf_hash,
        ip_addr: None,
        user_agent: None,
        created_at: now_ms,
        last_seen_at: now_ms,
        expires_at: now_ms + 86_400_000,
    };
    llmroller_store::users::SessionWithUser { user, session }
}
