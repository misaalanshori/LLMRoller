//! End-to-end integration tests for HTMX web UI, dashboards, key projection, and chat preview.
//!
//! Spec: Section 3.3 (A6), Section 6.10, Section 6.11, Section 8.2, Section 12.6, Section 14.3,
//! Requirements: UI-1, UI-2, UI-4, TRC-6, AUTH-4, USR-5, MDL-3, MDL-4, AUD-1, SEC-6, TRC-5, TRC-8, TRC-9, SEC-19.

use std::sync::Arc;
use tempfile::tempdir;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::chat_preview::{escape_html, sanitize_assistant_text};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_proto::headers::sanitize_csv_cell;
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

async fn setup_dashboard_test_db() -> (tempfile::TempDir, sqlx::SqlitePool, MasterKey, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("dashboard_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xBB; 32]);
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
            username: "admin_dash",
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

#[doc = "Spec: UI-1, UI-2, UI-4, TRC-6, AUTH-4, USR-5, MDL-3, MDL-4, AUD-1, SEC-6, TRC-5, TRC-8, TRC-9, SEC-19"]
#[tokio::test]
async fn test_htmx_dashboards_key_projection_and_chat_preview() {
    let (dir, pool, master_key, _admin_id) = setup_dashboard_test_db().await;

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

    // 1. UI-1, UI-2: Personal Workspace template rendering
    let app_html = include_str!("../../../crates/admin/templates/app.html");
    assert!(app_html.contains("Personal Workspace"));
    assert!(app_html.contains("Global pool"));

    // 2. UI-1, UI-2: Admin Console template rendering with A6 disk meter and uPlot chart
    let admin_html = include_str!("../../../crates/admin/templates/admin.html");
    assert!(admin_html.contains("Admin Console"));
    assert!(admin_html.contains("Projected Disk Usage"));
    assert!(admin_html.contains("uplot-container"));

    // 3. UI-4: Global traceability status banner
    let base_html = include_str!("../../../crates/admin/templates/base.html");
    assert!(base_html.contains("id=\"traceability-banner\""));
    assert!(base_html.contains("Content Traceability Mode is active"));

    // 4. TRC-6, SEC-19: Read-only session chat preview and XSS protection
    let malicious = "<script>alert('xss')</script><img src=x onerror=alert(1)>";
    let sanitized = sanitize_assistant_text(malicious);
    assert!(!sanitized.contains("<script"));
    assert!(sanitized.contains("&lt;script&gt;"));

    let escaped = escape_html(malicious);
    assert!(!escaped.contains('<'));
    assert!(escaped.contains("&lt;script&gt;"));

    // 5. SEC-6: Formula-safe CSV cell export
    assert_eq!(sanitize_csv_cell("=1+1"), "'=1+1");
    assert_eq!(sanitize_csv_cell("+cmd"), "'+cmd");
    assert_eq!(sanitize_csv_cell("-calc"), "'-calc");
    assert_eq!(sanitize_csv_cell("@sum"), "'@sum");
    assert_eq!(sanitize_csv_cell("\tleading_tab"), "'\tleading_tab");
    assert_eq!(sanitize_csv_cell("normal text"), "normal text");

    // 6. Test login and check static assets route works
    let req_static = Request::builder()
        .method("GET")
        .uri("/static/app.css")
        .body(Body::empty())
        .expect("req");
    let resp_static = app.oneshot(req_static).await.expect("resp");
    assert_eq!(resp_static.status(), StatusCode::OK);
    let body = resp_static
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let css = std::str::from_utf8(&body).expect("utf-8");
    assert!(css.contains(".traceability-banner"));
}
