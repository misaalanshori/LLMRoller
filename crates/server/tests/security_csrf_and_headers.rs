//! Security, CSRF, strict CSP, static asset embedding, and template lint tests.
//!
//! Spec: Section 6.11, Section 8.2, Section 14.2, Section 14.3,
//! Requirements: UI-1, UI-3, UI-4, SEC-5.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::tempdir;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_admin::assets::{APP_CSS, APP_JS};
use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

async fn setup_csrf_test_db() -> (tempfile::TempDir, sqlx::SqlitePool, MasterKey, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("csrf_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xFA; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "csrf_admin",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("admin");

    (dir, writer_pool, master_key, admin.id)
}

#[doc = "Spec: UI-1, UI-3, UI-4, SEC-5"]
#[test]
fn test_template_lint_zero_external_assets_and_zero_eval() {
    // 1. Template lint scans all crates/admin/templates/**/*.html (UI-3)
    let template_dir = if Path::new("crates/admin/templates").exists() {
        Path::new("crates/admin/templates")
    } else {
        Path::new("../../crates/admin/templates")
    };
    assert!(
        template_dir.exists(),
        "template directory must exist at {}",
        template_dir.display()
    );

    let mut template_files = Vec::new();
    collect_html_files(template_dir, &mut template_files);
    assert!(!template_files.is_empty(), "templates must exist");

    for path in template_files {
        let content = fs::read_to_string(&path).expect("read template");

        // Zero external script/stylesheet asset URLs (UI-3)
        assert!(
            !content.contains("src=\"http://")
                && !content.contains("src=\"https://")
                && !content.contains("src='http://")
                && !content.contains("src='https://")
                && !content.contains("href=\"http://")
                && !content.contains("href=\"https://")
                && !content.contains("href='http://")
                && !content.contains("href='https://"),
            "Template {} contains external asset URL (UI-3 violation)",
            path.display()
        );

        // Zero hx-on:* or js: attributes to prevent eval-based XSS (SEC-5)
        assert!(
            !content.contains("hx-on:") && !content.contains("js:"),
            "Template {} contains hx-on: or js: eval attributes (SEC-5 violation)",
            path.display()
        );

        // Zero inline event handlers (onclick=, onsubmit=, onchange=, onload=, onerror=) to prevent CSP violations (SEC-5, UI-3)
        assert!(
            !content.contains("onclick=")
                && !content.contains("onsubmit=")
                && !content.contains("onchange=")
                && !content.contains("onload=")
                && !content.contains("onerror="),
            "Template {} contains inline event handler (CSP script-src 'self' violation)",
            path.display()
        );
    }

    // 2. /static/app.js sets required HTMX CSP flags
    assert!(APP_JS.contains("htmx.config.includeIndicatorStyles = false;"));
    assert!(APP_JS.contains("htmx.config.allowEval = false;"));
    assert!(APP_JS.contains("htmx.config.allowScriptTags = false;"));

    // 3. /static/app.css contains spinner indicators
    assert!(APP_CSS.contains(".htmx-indicator"));
}

fn collect_html_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_html_files(&p, out);
            } else if p.extension().is_some_and(|ext| ext == "html") {
                out.push(p);
            }
        }
    }
}

#[doc = "Spec: UI-1, UI-3, UI-4, SEC-5"]
#[tokio::test]
async fn test_strict_csp_headers_and_csrf_middleware_enforcement() {
    let (dir, pool, master_key, _admin_id) = setup_csrf_test_db().await;

    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let auth_state = AuthAppState::new(pool.clone(), auth_config);
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    // 1. GET /login: Check security headers (SEC-5, UI-3)
    let login_page_req = Request::builder()
        .method("GET")
        .uri("/login")
        .body(Body::empty())
        .expect("req");
    let login_page_res = app.clone().oneshot(login_page_req).await.expect("res");
    assert_eq!(login_page_res.status(), StatusCode::OK);

    let headers = login_page_res.headers();
    let csp = headers
        .get("content-security-policy")
        .expect("CSP header")
        .to_str()
        .expect("str");
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("script-src 'self'"));
    assert!(csp.contains("style-src 'self'"));
    assert!(csp.contains("frame-ancestors 'none'"));

    let x_frame = headers
        .get("x-frame-options")
        .expect("x-frame")
        .to_str()
        .expect("str");
    assert_eq!(x_frame, "DENY");

    let x_content = headers
        .get("x-content-type-options")
        .expect("nosniff")
        .to_str()
        .expect("str");
    assert_eq!(x_content, "nosniff");

    // 2. Static asset endpoint /static/app.js
    let static_req = Request::builder()
        .method("GET")
        .uri("/static/app.js")
        .body(Body::empty())
        .expect("req");
    let static_res = app.clone().oneshot(static_req).await.expect("res");
    assert_eq!(static_res.status(), StatusCode::OK);
    let static_body = static_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let js_str = std::str::from_utf8(&static_body).expect("utf-8");
    assert!(js_str.contains("htmx.config.allowEval = false;"));

    // 3. Log in to establish authenticated session
    let login_cookie = login_page_res
        .headers()
        .get(header::SET_COOKIE)
        .expect("login cookie")
        .to_str()
        .expect("str")
        .split(';')
        .next()
        .expect("part")
        .to_string();
    let login_csrf = login_page_res
        .headers()
        .get("x-csrf-token")
        .expect("login csrf")
        .to_str()
        .expect("str")
        .to_string();

    let login_req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::COOKIE, &login_cookie)
        .header("x-csrf-token", &login_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "username": "csrf_admin",
                "password": "supersecretadminpass123",
            })
            .to_string(),
        ))
        .expect("req");
    let login_res = app.clone().oneshot(login_req).await.expect("res");
    assert_eq!(login_res.status(), StatusCode::OK);

    let session_cookie = login_res
        .headers()
        .get(header::SET_COOKIE)
        .expect("session cookie")
        .to_str()
        .expect("str")
        .split(';')
        .next()
        .expect("part")
        .to_string();

    // 4. Mutating request with valid session BUT missing x-csrf-token returns HTTP 403 Forbidden (SEC-5)
    let mutating_req = Request::builder()
        .method("PUT")
        .uri("/admin/api/settings/traceability")
        .header(header::COOKIE, &session_cookie)
        .header(header::CONTENT_TYPE, "application/json")
        // Missing x-csrf-token!
        .body(Body::from(r#"{"enabled":false,"retention_days":14}"#))
        .expect("req");
    let mutating_res = app.clone().oneshot(mutating_req).await.expect("res");
    assert_eq!(mutating_res.status(), StatusCode::FORBIDDEN);

    // 5. POST /api/auth/login without Cookie or X-CSRF-Token returns HTTP 403 Forbidden (SEC-5 login CSRF)
    let bad_login_req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "username": "csrf_admin",
                "password": "supersecretadminpass123",
            })
            .to_string(),
        ))
        .expect("req");
    let bad_login_res = app.clone().oneshot(bad_login_req).await.expect("res");
    assert_eq!(
        bad_login_res.status(),
        StatusCode::FORBIDDEN,
        "Login without CSRF cookie/header must be rejected with 403"
    );

    // 6. GET /app exposes verifiable x-csrf-token in hx-headers, allowing mutations (SEC-5, UI-1)
    let app_get_req = Request::builder()
        .method("GET")
        .uri("/app")
        .header(header::COOKIE, &session_cookie)
        .body(Body::empty())
        .expect("req");
    let app_get_res = app.clone().oneshot(app_get_req).await.expect("res");
    assert_eq!(app_get_res.status(), StatusCode::OK);
    let app_body_bytes = app_get_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let app_html = std::str::from_utf8(&app_body_bytes).expect("utf8");

    // Extract x-csrf-token from <body hx-headers='{"x-csrf-token": "..."}'>
    let token_marker = r#"{"x-csrf-token": ""#;
    let token_start = app_html
        .find(token_marker)
        .expect("hx-headers x-csrf-token")
        + token_marker.len();
    let token_end = app_html[token_start..].find('"').expect("token end quote") + token_start;
    let extracted_csrf = &app_html[token_start..token_end];

    // Mutating request using extracted CSRF token succeeds
    let mutating_with_csrf_req = Request::builder()
        .method("PUT")
        .uri("/admin/api/settings/traceability")
        .header(header::COOKIE, &session_cookie)
        .header("x-csrf-token", extracted_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"enabled":false,"retention_days":14}"#))
        .expect("req");
    let mutating_with_csrf_res = app
        .clone()
        .oneshot(mutating_with_csrf_req)
        .await
        .expect("res");
    assert_eq!(
        mutating_with_csrf_res.status(),
        StatusCode::OK,
        "Mutation with extracted x-csrf-token from /app should succeed"
    );

    // 7. UI-4: Global traceability status banner renders when enabled
    let enable_trace_req = Request::builder()
        .method("PUT")
        .uri("/admin/api/settings/traceability")
        .header(header::COOKIE, &session_cookie)
        .header("x-csrf-token", extracted_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"enabled":true,"retention_days":14}"#))
        .expect("req");
    let enable_trace_res = app.clone().oneshot(enable_trace_req).await.expect("res");
    assert_eq!(enable_trace_res.status(), StatusCode::OK);

    let app_get_banner_req = Request::builder()
        .method("GET")
        .uri("/app")
        .header(header::COOKIE, &session_cookie)
        .body(Body::empty())
        .expect("req");
    let app_get_banner_res = app.clone().oneshot(app_get_banner_req).await.expect("res");
    assert_eq!(app_get_banner_res.status(), StatusCode::OK);
    let app_banner_bytes = app_get_banner_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let app_banner_html = std::str::from_utf8(&app_banner_bytes).expect("utf8");
    assert!(
        app_banner_html.contains("id=\"traceability-banner\""),
        "Traceability banner must render on /app when enabled (UI-4)"
    );
}
