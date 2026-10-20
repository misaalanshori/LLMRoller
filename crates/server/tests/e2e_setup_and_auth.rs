//! End-to-end setup, authentication, CSRF, and rate limiting integration tests.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-1.md#14-sub-phase-14-atomic-first-admin-setup-argon2id--totp-auth-and-reset-admin-cli

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use llmroller_admin::auth::{
    AuthAppState, AuthConfig, ThreeTierLoginLimiter, check_admin_listener_address,
    extract_client_ip,
};
use llmroller_admin::control_plane_router;
use llmroller_admin::setup::{
    SETUP_TOKEN_FILE_NAME, SetupAppState, delete_setup_token_file, resolve_or_create_setup_token,
};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role};
use llmroller_core::seams::DeterministicRng;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::users::{UserError, count_users, create_user, set_role};
use tempfile::TempDir;
use tower::ServiceExt as _;

async fn setup_test_env() -> (TempDir, sqlx::SqlitePool, SetupAppState, AuthAppState) {
    let directory = tempfile::tempdir().expect("temp directory");
    let pool = open_pool(&directory.path().join("llmroller.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let entropy = Arc::new(DeterministicRng::new(0x7777));
    let token = resolve_or_create_setup_token(directory.path(), None, true, entropy.as_ref())
        .expect("setup token");

    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: token,
        data_dir: directory.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: entropy.clone(),
    };

    let auth_config = AuthConfig {
        trusted_proxies: vec!["172.28.1.10/32".to_owned()],
        dev_insecure_cookies: false,
        require_tls: true,
    };

    let auth_state = AuthAppState::new(pool.clone(), auth_config);
    (directory, pool, setup_state, auth_state)
}

#[tokio::test]
#[doc = "Spec: AUTH-2"]
async fn setup_token_file_lifecycle_and_deletion() {
    let directory = tempfile::tempdir().expect("temp dir");
    let entropy = DeterministicRng::new(42);

    // 1. Created with expected permissions
    let token = resolve_or_create_setup_token(directory.path(), None, true, &entropy)
        .expect("token resolved");
    assert!(token.is_some());

    let token_file = directory.path().join(SETUP_TOKEN_FILE_NAME);
    assert!(token_file.is_file());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_file)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "setup-token permissions must be exactly 0600"
        );
    }

    // 2. Reused across restarts
    let reused = resolve_or_create_setup_token(directory.path(), None, true, &entropy)
        .expect("token reused");
    assert_eq!(token, reused);

    // 3. Deleted upon setup completion
    delete_setup_token_file(directory.path());
    assert!(!token_file.exists());
}

#[tokio::test]
#[doc = "Spec: AUTH-1, Spec: AUTH-2, Spec: AUTH-3"]
async fn setup_20_concurrent_requests_yield_exactly_one_201_and_one_user() {
    let (_dir, pool, setup_state, auth_state) = setup_test_env().await;
    let expected_token = setup_state.expected_token.clone().expect("token");

    let mut handles = Vec::new();
    for i in 0..20 {
        let app = control_plane_router(setup_state.clone(), auth_state.clone());
        let token = expected_token.clone();
        let payload = serde_json::json!({
            "username": format!("admin_{i}"),
            "password": "Password12345!#",
            "setup_token": token,
        });

        handles.push(tokio::spawn(async move {
            let request = Request::builder()
                .method("POST")
                .uri("/admin/api/setup")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("valid request");

            app.oneshot(request).await.expect("oneshot")
        }));
    }

    let mut created_count = 0;
    let mut rejected_count = 0;
    for handle in handles {
        let response = handle.await.expect("task join");
        let status = response.status();
        let body_bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("body")
            .to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes);
        println!("status: {status}, body: {body_str}");
        if status == StatusCode::CREATED {
            created_count += 1;
        } else if status == StatusCode::CONFLICT || status == StatusCode::NOT_FOUND {
            rejected_count += 1;
        }
    }

    assert_eq!(created_count, 1, "exactly one setup request must succeed");
    assert_eq!(rejected_count, 19, "19 competing requests must be rejected");

    let user_count = count_users(&pool).await.expect("count users");
    assert_eq!(user_count, 1, "exactly one user must exist in the database");

    // Permanent lockout: subsequent calls to /setup or /admin/api/setup return 404
    let app = control_plane_router(setup_state.clone(), auth_state.clone());
    let get_setup = Request::builder()
        .method("GET")
        .uri("/setup")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(get_setup).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "GET /setup must return 404 after initialization (AUTH-3)"
    );
}

#[tokio::test]
#[doc = "Spec: AUTH-4, Spec: AUTH-5"]
async fn last_admin_guard_prevents_zero_admins_under_concurrent_demotions() {
    let (_dir, pool) = {
        let directory = tempfile::tempdir().expect("temp dir");
        let pool = open_pool(&directory.path().join("llmroller.db"), PoolRole::Writer)
            .await
            .expect("writer pool");
        migrate(&pool).await.expect("migrate");
        (directory, pool)
    };

    let entropy = DeterministicRng::new(55);
    let actor_id = UserId::generate();
    let actor = ActorScope::user(actor_id, Role::Admin);

    let admin1 = create_user(
        &pool,
        &actor,
        llmroller_store::users::NewUser {
            username: "admin1",
            password: "Password12345!#",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1_000,
    )
    .await
    .expect("admin1");

    let admin2 = create_user(
        &pool,
        &actor,
        llmroller_store::users::NewUser {
            username: "admin2",
            password: "Password12345!#",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1_000,
    )
    .await
    .expect("admin2");

    // Both try to demote each other concurrently
    let pool1 = pool.clone();
    let actor1 = ActorScope::user(admin1.id, Role::Admin);
    let id2 = admin2.id;
    let handle1 =
        tokio::spawn(async move { set_role(&pool1, &actor1, id2, Role::User, 2_000).await });

    let pool2 = pool.clone();
    let actor2 = ActorScope::user(admin2.id, Role::Admin);
    let id1 = admin1.id;
    let handle2 =
        tokio::spawn(async move { set_role(&pool2, &actor2, id1, Role::User, 2_000).await });

    let res1 = handle1.await.expect("join1");
    let res2 = handle2.await.expect("join2");

    // One succeeds, the other fails with LastAdmin
    let successes = [res1.is_ok(), res2.is_ok()]
        .into_iter()
        .filter(|&v| v)
        .count();
    let last_admin_errors = [
        matches!(res1, Err(UserError::LastAdmin)),
        matches!(res2, Err(UserError::LastAdmin)),
    ]
    .into_iter()
    .filter(|&v| v)
    .count();

    assert_eq!(successes, 1, "exactly one demotion must succeed");
    assert_eq!(
        last_admin_errors, 1,
        "second demotion must fail with LastAdmin"
    );
}

#[test]
#[doc = "Spec: AUTH-8, Partial: SEC-12"]
fn three_tier_rate_limiting_and_trusted_proxy_extraction() {
    let limiter = ThreeTierLoginLimiter::new();
    let ip: std::net::IpAddr = "192.0.2.10".parse().expect("ip");
    let now = 1_000_000_000;

    // 1. IP rate limiting (20 failures -> 429)
    for _ in 0..20 {
        limiter.record_failure("user", ip, now);
    }
    assert!(limiter.check("user", ip, now).is_err());

    // 2. Trusted proxy client IP extraction
    let trusted = vec!["172.28.1.10/32".to_owned()];
    let peer: SocketAddr = "172.28.1.10:8081".parse().expect("socket");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "198.51.100.99, 172.28.1.10".parse().expect("header"),
    );
    let extracted = extract_client_ip(peer, &headers, &trusted);
    assert_eq!(
        extracted,
        "198.51.100.99".parse::<IpAddr>().expect("parsed")
    );
}

#[tokio::test]
#[doc = "Spec: AUTH-7, Spec: SEC-4, Spec: SEC-5, Partial: SEC-9"]
async fn login_csrf_and_security_headers() {
    let (_dir, _pool, setup_state, auth_state) = setup_test_env().await;
    let app = control_plane_router(setup_state, auth_state);

    // 1. GET /login issues login CSRF cookie and meta tag
    let request = Request::builder()
        .method("GET")
        .uri("/login")
        .body(Body::empty())
        .expect("request");

    let response = app.clone().oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let headers = response.headers();
    let csp = headers.get("content-security-policy").expect("csp");
    assert!(csp.to_str().expect("str").contains("default-src 'self'"));
    assert_eq!(headers.get("x-frame-options").expect("frame"), "DENY");
    assert_eq!(
        headers.get("x-content-type-options").expect("nosniff"),
        "nosniff"
    );

    let cookie = headers.get(header::SET_COOKIE).expect("set-cookie");
    let cookie_str = cookie.to_str().expect("str");
    assert!(cookie_str.contains("__Host-llmroller_login_csrf"));
    assert!(cookie_str.contains("SameSite=Strict"));
    assert!(cookie_str.contains("Secure"));

    // 2. Non-loopback check (SEC-5, SEC-9)
    assert_eq!(
        check_admin_listener_address("0.0.0.0:8081", false, ""),
        Err(2)
    );
    assert_eq!(
        check_admin_listener_address("127.0.0.1:8081", false, ""),
        Ok(())
    );
}
