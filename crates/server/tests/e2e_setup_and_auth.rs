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

#[doc = "Spec: AUTH-1, AUTH-2, AUTH-3, AUTH-7, UI-1"]
#[tokio::test]
async fn test_browser_web_flow_setup_login_and_redirects() {
    let (_dir, pool, setup_state, auth_state) = setup_test_env().await;
    let expected_token = setup_state.expected_token.clone().expect("token");

    let master_key = llmroller_store::crypto::MasterKey::from_bytes([0x77; 32]);
    llmroller_store::instance::load_or_initialize(
        &pool,
        "key-0",
        &master_key,
        Arc::new(llmroller_core::seams::OsEntropy),
        1,
    )
    .await
    .expect("load");
    llmroller_store::settings::seed_defaults(&pool, None, 1)
        .await
        .expect("seed");

    let api_state =
        llmroller_admin::ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = llmroller_admin::control_plane_router_full(setup_state, auth_state, api_state);

    // 1. GET / redirects to /setup when zero users exist
    let req_root = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .expect("request");
    let resp_root = app.clone().oneshot(req_root).await.expect("response");
    assert_eq!(resp_root.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp_root.headers().get("location").expect("location"),
        "/setup"
    );

    // 2. Browser form submission (application/x-www-form-urlencoded) on /admin/api/setup succeeds and redirects to /login
    let form_body =
        format!("username=web_admin&password=Password12345!%23&setup_token={expected_token}");
    let req_setup = Request::builder()
        .method("POST")
        .uri("/admin/api/setup")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "text/html")
        .body(Body::from(form_body))
        .expect("request");
    let resp_setup = app.clone().oneshot(req_setup).await.expect("response");
    assert_eq!(resp_setup.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp_setup.headers().get("location").expect("location"),
        "/login"
    );

    // 3. GET /setup now returns 404 (permanent lockout per AUTH-3)
    let req_setup_page = Request::builder()
        .method("GET")
        .uri("/setup")
        .body(Body::empty())
        .expect("request");
    let resp_setup_page = app.clone().oneshot(req_setup_page).await.expect("response");
    assert_eq!(resp_setup_page.status(), StatusCode::NOT_FOUND);

    // 4. GET / now redirects to /login since users exist
    let req_root2 = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .expect("request");
    let resp_root2 = app.clone().oneshot(req_root2).await.expect("response");
    assert_eq!(resp_root2.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp_root2.headers().get("location").expect("location"),
        "/login"
    );

    // 5. GET /login returns login HTML with CSRF cookie
    let req_login_page = Request::builder()
        .method("GET")
        .uri("/login")
        .body(Body::empty())
        .expect("request");
    let resp_login_page = app.clone().oneshot(req_login_page).await.expect("response");
    assert_eq!(resp_login_page.status(), StatusCode::OK);
    let login_cookie = resp_login_page
        .headers()
        .get("set-cookie")
        .expect("csrf cookie")
        .to_str()
        .expect("str");
    let csrf_val = resp_login_page
        .headers()
        .get("x-csrf-token")
        .expect("csrf token")
        .to_str()
        .expect("str");

    // 6. Browser form submission (application/x-www-form-urlencoded) on /api/auth/login succeeds and redirects to /admin
    let login_form_body =
        format!("username=web_admin&password=Password12345!%23&csrf_token={csrf_val}");
    let req_login = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "text/html")
        .header("cookie", login_cookie)
        .body(Body::from(login_form_body))
        .expect("request");
    let resp_login = app.clone().oneshot(req_login).await.expect("response");
    assert_eq!(resp_login.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp_login.headers().get("location").expect("location"),
        "/admin"
    );
    let session_cookie = resp_login
        .headers()
        .get("set-cookie")
        .expect("session cookie")
        .to_str()
        .expect("str")
        .to_string();

    // 7. GET / with session cookie redirects to /admin
    let req_root3 = Request::builder()
        .method("GET")
        .uri("/")
        .header("cookie", &session_cookie)
        .body(Body::empty())
        .expect("request");
    let resp_root3 = app.clone().oneshot(req_root3).await.expect("response");
    assert_eq!(resp_root3.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp_root3.headers().get("location").expect("location"),
        "/admin"
    );

    // 8. GET /app without session cookie redirects browser (accept: text/html) to /login
    let req_app_unauth = Request::builder()
        .method("GET")
        .uri("/app")
        .header("accept", "text/html,application/xhtml+xml")
        .body(Body::empty())
        .expect("request");
    let resp_app_unauth = app.clone().oneshot(req_app_unauth).await.expect("response");
    assert_eq!(resp_app_unauth.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp_app_unauth.headers().get("location").expect("location"),
        "/login"
    );
}

/// Spec: AUTH-9, SEC-4
/// Login must require a valid TOTP (or unused recovery code) whenever 2FA is enabled,
/// and replaying the same code in the same time step must be rejected.
#[tokio::test]
async fn login_requires_totp_and_rejects_replayed_codes() {
    use llmroller_core::rbac::{SystemSeal, SystemTask};
    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::XChaCha20Poly1305Sealer;
    use llmroller_store::users::{
        NewUser, confirm_totp, create_user, enroll_totp, find_login_record_by_id,
    };

    let (_dir, pool, setup_state, mut auth_state) = setup_test_env().await;
    let master_key = llmroller_store::crypto::MasterKey::from_bytes([0x55; 32]);
    let instance = llmroller_store::instance::load_or_initialize(
        &pool,
        "key-0",
        &master_key,
        Arc::new(llmroller_core::seams::OsEntropy),
        1,
    )
    .await
    .expect("load instance");
    llmroller_store::settings::seed_defaults(&pool, None, 1)
        .await
        .expect("seed settings");
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();

    let admin = create_user(
        &pool,
        &ActorScope::system(SystemTask::Writer, SystemSeal::store()),
        NewUser {
            username: "admin_totp",
            password: "Password12345!#",
            role: Role::Admin,
            must_change_password: false,
        },
        &DeterministicRng::new(11),
        1,
    )
    .await
    .expect("admin user");

    let sealer = XChaCha20Poly1305Sealer::new(
        master_key.clone(),
        instance.instance_id,
        Arc::new(llmroller_core::seams::OsEntropy),
    );
    let admin_actor = ActorScope::user(admin.id, Role::Admin);
    let enrollment = enroll_totp(
        &pool,
        &admin_actor,
        &sealer,
        "key-0",
        &DeterministicRng::new(22),
    )
    .await
    .expect("enroll totp");
    let secret = totp_rs::Secret::try_from_base32(enrollment.secret_base32.expose_secret())
        .expect("base32 secret");
    #[allow(deprecated)]
    let totp = totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        secret.as_bytes().to_vec(),
    )
    .expect("totp");
    let code = totp.generate_current().to_string();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    confirm_totp(&pool, &admin_actor, &code, &sealer, "key-0", now_ms)
        .await
        .expect("confirm totp");

    let app = control_plane_router(setup_state, auth_state.clone());

    let page = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/login")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("login page");
    assert_eq!(page.status(), StatusCode::OK);
    let cookie = page
        .headers()
        .get("set-cookie")
        .expect("csrf cookie")
        .to_str()
        .expect("str")
        .to_string();
    let csrf = page
        .headers()
        .get("x-csrf-token")
        .expect("csrf token")
        .to_str()
        .expect("str")
        .to_string();

    let post_login = |body: String| {
        Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "text/html")
            .header("cookie", cookie.clone())
            .body(Body::from(body))
            .expect("request")
    };

    let missing = app
        .clone()
        .oneshot(post_login(format!(
            "username=admin_totp&password=Password12345!%23&csrf_token={csrf}"
        )))
        .await
        .expect("missing totp");
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    // Confirmation consumed the current step; use the next step for login.
    let fresh_code = totp
        .generate((u64::try_from(now_ms).unwrap_or(0) / 1000) + 30)
        .to_string();
    let accepted = app
        .clone()
        .oneshot(post_login(format!(
            "username=admin_totp&password=Password12345!%23&totp_code={fresh_code}&csrf_token={csrf}"
        )))
        .await
        .expect("totp login");
    let accepted_status = accepted.status();
    let accepted_body = http_body_util::BodyExt::collect(accepted.into_body())
        .await
        .expect("body")
        .to_bytes();
    assert_eq!(
        accepted_status,
        StatusCode::SEE_OTHER,
        "body: {}",
        String::from_utf8_lossy(&accepted_body)
    );

    let replayed = app
        .clone()
        .oneshot(post_login(format!(
            "username=admin_totp&password=Password12345!%23&totp_code={fresh_code}&csrf_token={csrf}"
        )))
        .await
        .expect("replay");
    assert_eq!(replayed.status(), StatusCode::UNAUTHORIZED);

    // Recovery codes are single use.
    let recovery = enrollment.recovery_codes[0].expose_secret().to_string();
    let unlocked = app
        .clone()
        .oneshot(post_login(format!(
            "username=admin_totp&password=Password12345!%23&totp_code={recovery}&csrf_token={csrf}"
        )))
        .await
        .expect("recovery login");
    assert_eq!(unlocked.status(), StatusCode::SEE_OTHER);
    let reused = app
        .clone()
        .oneshot(post_login(format!(
            "username=admin_totp&password=Password12345!%23&totp_code={recovery}&csrf_token={csrf}"
        )))
        .await
        .expect("recovery replay");
    assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);

    // Silence unused-import style warnings for the record lookup helper.
    let record = find_login_record_by_id(&pool, admin.id)
        .await
        .expect("record");
    assert!(record.user.totp_enabled);
}
