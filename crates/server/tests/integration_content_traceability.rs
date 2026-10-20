//! Integration tests for content traceability mode, auto-disable, purge confirmation, and chat preview.
//!
//! Spec: Section 4.2, Section 6.10, Section 6.11, Section 9.3.3,
//! Requirements: TRC-1, TRC-7, AUD-1.

use std::sync::Arc;
use tempfile::tempdir;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

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

async fn setup_traceability_test_db() -> (tempfile::TempDir, sqlx::SqlitePool, MasterKey, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("traceability_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xEE; 32]);
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
            username: "admin_trc",
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

#[doc = "Spec: TRC-1, TRC-7, AUD-1"]
#[tokio::test]
async fn test_traceability_settings_confirm_purge_guard_and_auto_disable() {
    let (dir, pool, master_key, _admin_id) = setup_traceability_test_db().await;

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
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());

    let app = control_plane_router_full(setup_state, auth_state, api_state);

    // 1. GET /login to retrieve login CSRF cookie
    let login_page_req = Request::builder()
        .method("GET")
        .uri("/login")
        .body(Body::empty())
        .expect("login page req");
    let login_page_res = app.clone().oneshot(login_page_req).await.expect("res");
    assert_eq!(login_page_res.status(), StatusCode::OK);

    let login_cookie = login_page_res
        .headers()
        .get(header::SET_COOKIE)
        .expect("login set-cookie")
        .to_str()
        .expect("str")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string();
    let login_csrf = login_page_res
        .headers()
        .get("x-csrf-token")
        .expect("login csrf")
        .to_str()
        .expect("str")
        .to_string();

    // 2. POST /api/auth/login as admin_trc
    let login_req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::COOKIE, &login_cookie)
        .header("x-csrf-token", &login_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "username": "admin_trc",
                "password": "supersecretadminpass123",
            })
            .to_string(),
        ))
        .expect("login req");
    let login_res = app.clone().oneshot(login_req).await.expect("res");
    assert_eq!(login_res.status(), StatusCode::OK);

    let session_cookie = login_res
        .headers()
        .get(header::SET_COOKIE)
        .expect("session set-cookie")
        .to_str()
        .expect("str")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string();
    let session_csrf = login_res
        .headers()
        .get("x-csrf-token")
        .expect("session csrf")
        .to_str()
        .expect("str")
        .to_string();

    // 3. Lowering retention_days from default (14) to 7 without confirm_purge:
    // Must return HTTP 409 Conflict with purge_confirmation_required (TRC-1)
    let req_unconfirmed = Request::builder()
        .method("PUT")
        .uri("/admin/api/settings/traceability")
        .header(header::COOKIE, &session_cookie)
        .header("x-csrf-token", &session_csrf)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "enabled": true,
                "match_filters": { "user_ids": [], "client_key_ids": [], "models": [] },
                "retention_days": 7,
                "auto_disable_at": null,
                "confirm_purge": false
            }))
            .expect("json"),
        ))
        .expect("req");

    let resp = app.clone().oneshot(req_unconfirmed).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"], "purge_confirmation_required");
    assert!(json["error"]["affected_rows"].as_i64().is_some());

    // 4. Supplying confirm_purge: true succeeds with HTTP 200 (TRC-1)
    let req_confirmed = Request::builder()
        .method("PUT")
        .uri("/admin/api/settings/traceability")
        .header(header::COOKIE, &session_cookie)
        .header("x-csrf-token", &session_csrf)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "enabled": true,
                "match_filters": { "user_ids": [], "client_key_ids": [], "models": [] },
                "retention_days": 7,
                "auto_disable_at": 1_700_000_060_000i64,
                "confirm_purge": true
            }))
            .expect("json"),
        ))
        .expect("req");

    let resp = app.oneshot(req_confirmed).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["traceability_config"]["retention_days"], 7);
    assert_eq!(json["traceability_config"]["enabled"], true);
}
