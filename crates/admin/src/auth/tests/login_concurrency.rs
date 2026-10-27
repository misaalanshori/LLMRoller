//! Login concurrency tests: the Argon2id permit must be acquired once per request
//! and must never be held across a second acquisition (`ISSUE-0001`, `AUTH-7`).
//!
//! These tests drive the real `post_login` handler over the real router, so they
//! assert the observable HTTP contract. They deliberately do not inspect
//! `Semaphore` internals or permit counts.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use tokio::sync::Semaphore;
use tower::ServiceExt as _;

use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;

use crate::auth::{AuthAppState, AuthConfig, router};

const TEST_USERNAME: &str = "concurrency_user";
const TEST_PASSWORD: &str = "concurrency-test-password-123";

/// Bound for a full batch of concurrent logins. Generous relative to the 5 s
/// outer permit timeout so the test fails on the deadlock rather than on a slow
/// machine, but tight enough that a genuine deadlock is reported in seconds.
const BATCH_TIMEOUT: Duration = Duration::from_secs(30);

struct LoginHarness {
    state: AuthAppState,
    _dir: tempfile::TempDir,
}

/// Build an `AuthAppState` over a migrated temp-file database with one seeded user.
///
/// Argon2id is deliberately expensive, so the test budget is a few dozen
/// verifications; the database is a temp file rather than in-memory because the
/// harness opens more than one connection.
async fn build_harness(permits: Option<usize>) -> LoginHarness {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("login_concurrency.db");

    let pool = llmroller_store::pool::open_pool(&db_path, llmroller_store::pool::Role::Writer)
        .await
        .expect("writer pool");
    llmroller_store::migrations::migrate(&pool)
        .await
        .expect("migrate");

    let entropy = OsEntropy::new();
    let master_key = llmroller_store::crypto::MasterKey::from_bytes([0x5A; 32]);
    llmroller_store::instance::load_or_initialize(
        &pool,
        "key-0",
        &master_key,
        Arc::new(OsEntropy::new()),
        1,
    )
    .await
    .expect("instance load");
    llmroller_store::settings::seed_defaults(&pool, None, 1)
        .await
        .expect("seed settings");

    let actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    llmroller_store::users::create_user(
        &pool,
        &actor,
        llmroller_store::users::NewUser {
            username: TEST_USERNAME,
            password: TEST_PASSWORD,
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("seed user");

    let config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut state = AuthAppState::new(pool, config);
    if let Some(permits) = permits {
        state.argon2_semaphore = Arc::new(Semaphore::new(permits));
    }

    LoginHarness { state, _dir: dir }
}

/// One `POST /api/auth/login` against the real router.
///
/// The pre-login CSRF pair is generated the same way `get_login_page` does it:
/// one random nonce serves as both the cookie value and the header value, which
/// is what the double-submit check compares.
async fn attempt_login(app: &axum::Router, username: &str) -> StatusCode {
    let nonce = "concurrency-test-csrf-nonce-0123456789";
    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::COOKIE, format!("llmroller_login_csrf={nonce}"))
        .header("x-csrf-token", nonce)
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(SocketAddr::new(
            IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1)),
            40000,
        )))
        .body(Body::from(
            serde_json::json!({
                "username": username,
                "password": TEST_PASSWORD,
            })
            .to_string(),
        ))
        .expect("build request");

    let response = app.clone().oneshot(request).await.expect("router call");
    let status = response.status();
    // Drain the body so the response is fully completed before the next attempt.
    let _ = axum::body::to_bytes(response.into_body(), 64 * 1024).await;
    status
}

/// Two concurrent logins for the same credentials must both complete when the
/// Argon2id pool holds exactly one permit.
///
/// This isolates the double-acquisition defect from machine parallelism. With the
/// second acquisition inside the first permit's scope, the first request holds the
/// only permit and then waits for a second one it can never get; the second
/// request waits behind it. Both then hit the bounded acquire timeout and return
/// `503 login_busy`. Serialised single-permit acquisition must instead let both
/// requests through, one after the other.
#[doc = "Spec: AUTH-7"]
#[tokio::test]
async fn single_permit_allows_two_concurrent_logins_to_complete() {
    let harness = build_harness(Some(1)).await;
    let app = router(harness.state.clone());

    let batch = async {
        let (first, second) = tokio::join!(
            attempt_login(&app, TEST_USERNAME),
            attempt_login(&app, TEST_USERNAME),
        );
        (first, second)
    };

    let (first, second) = tokio::time::timeout(BATCH_TIMEOUT, batch)
        .await
        .expect("two concurrent logins must not deadlock on a single Argon2id permit");

    assert_ne!(
        first,
        StatusCode::SERVICE_UNAVAILABLE,
        "first concurrent login must not be rejected as login_busy; got {first}"
    );
    assert_ne!(
        second,
        StatusCode::SERVICE_UNAVAILABLE,
        "second concurrent login must not be rejected as login_busy; got {second}"
    );
}

/// A burst of concurrent logins sized from the real permit count must all complete
/// without `503 login_busy`.
///
/// The permit count is derived from `available_parallelism` for the default state
/// (the same arithmetic `AuthAppState::new` performs), because the failure mode is
/// "every permit holder waits for a second permit" — so the burst must be at least
/// as wide as the pool to reproduce it.
#[doc = "Spec: AUTH-7"]
#[tokio::test]
async fn burst_of_concurrent_logins_does_not_deadlock() {
    let harness = build_harness(None).await;
    let parallelism = harness.state.argon2_semaphore.available_permits();
    let app = router(harness.state.clone());

    // Two full waves: every permit is taken, then every holder tries to take a
    // second one.
    let logins = parallelism * 2;
    let batch = async {
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..logins {
            let app = app.clone();
            set.spawn(async move { attempt_login(&app, TEST_USERNAME).await });
        }
        let mut statuses = Vec::with_capacity(logins);
        while let Some(joined) = set.join_next().await {
            statuses.push(joined.expect("login task must not panic"));
        }
        statuses
    };

    let statuses = tokio::time::timeout(BATCH_TIMEOUT, batch)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "burst of {logins} concurrent logins ({parallelism} permits) must not deadlock; \
                 the whole batch timed out after {BATCH_TIMEOUT:?}"
            )
        });

    let busy = statuses
        .iter()
        .filter(|status| **status == StatusCode::SERVICE_UNAVAILABLE)
        .count();
    assert_eq!(
        busy, 0,
        "{busy} of {logins} concurrent logins were rejected as login_busy; \
         none should be when the burst is bounded by the permit count"
    );
}
