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
use llmroller_core::seams::{Clock, OsEntropy};
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
    let (dir, pool, master_key, admin_id) = setup_dashboard_test_db().await;

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
    assert!(!sanitized.contains("alert('xss')"));
    assert!(!sanitized.contains("<img"));

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
    let resp_static = app.clone().oneshot(req_static).await.expect("resp");
    assert_eq!(resp_static.status(), StatusCode::OK);
    let body = resp_static
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let css = std::str::from_utf8(&body).expect("utf-8");
    assert!(css.contains(".traceability-banner"));

    // 7. Test real HTTP rendering of /app and /admin via Askama templates (UI-1, UI-2)
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let raw_session = "test_session_token_12345678901234567890";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_session.as_bytes());
    let csrf_token = llmroller_admin::auth::derive_session_csrf(raw_session);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: admin_id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("test-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("insert session");

    // GET /app returns 200 OK with rendered Personal Workspace
    let req_app = Request::builder()
        .method("GET")
        .uri("/app")
        .header(
            axum::http::header::COOKIE,
            format!("llmroller_session={raw_session}"),
        )
        .body(Body::empty())
        .expect("req");
    let resp_app = app.clone().oneshot(req_app).await.expect("resp");
    assert_eq!(resp_app.status(), StatusCode::OK);
    let app_bytes = resp_app
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let app_html_rendered = std::str::from_utf8(&app_bytes).expect("utf-8");
    assert!(app_html_rendered.contains("Personal Workspace"));
    assert!(app_html_rendered.contains("Global pool"));

    // GET /admin returns 200 OK with rendered Admin Console
    let req_admin = Request::builder()
        .method("GET")
        .uri("/admin")
        .header(
            axum::http::header::COOKIE,
            format!("llmroller_session={raw_session}"),
        )
        .body(Body::empty())
        .expect("req");
    let resp_admin = app.oneshot(req_admin).await.expect("resp");
    assert_eq!(resp_admin.status(), StatusCode::OK);
    let admin_bytes = resp_admin
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let admin_html_rendered = std::str::from_utf8(&admin_bytes).expect("utf-8");
    assert!(admin_html_rendered.contains("Admin Console"));
    assert!(admin_html_rendered.contains("Projected Disk Usage"));
}

#[doc = "Spec: SHR-3, MDL-3, MDL-4, UI-2, Section 3.3 (A6)"]
#[tokio::test]
async fn test_admin_share_approval_catalog_ops_and_a6_telemetry() {
    use axum::http::header;
    use llmroller_core::provider::ProviderKind;
    use llmroller_core::rbac::OwnershipScope;
    use llmroller_core::seams::Clock as _;
    use llmroller_store::providers::{NewProvider, create_provider};
    use llmroller_store::shares::{NewShare, create_provider_share_tx};

    let (dir, pool, master_key, admin_id) = setup_dashboard_test_db().await;

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

    let entropy = OsEntropy;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    // Create user Alice and Bob
    let alice = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "alice_custom",
            password: "supersecretalicepass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("alice");

    let bob = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "bob_grantee",
            password: "supersecretbobpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("bob");

    let alice_actor = ActorScope::user(alice.id, Role::User);

    // Create custom provider for Alice
    let custom_prov = create_provider(
        &pool,
        &alice_actor,
        NewProvider {
            name: "alice_custom_llm".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://custom.endpoint.internal".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(alice.id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        now_ms,
    )
    .await
    .expect("custom prov");

    // Alice shares custom provider with Bob
    let mut tx = pool.begin().await.expect("tx");
    let share = create_provider_share_tx(
        &mut tx,
        &alice.id,
        NewShare {
            provider_id: custom_prov.id,
            upstream_key_id: None,
            grantee_user_id: bob.id,
            allowed_models: None,
            max_monthly_micro_usd: None,
        },
        now_ms,
    )
    .await
    .expect("create share");
    tx.commit().await.expect("commit share");

    // Authenticate Admin session
    let raw_session = "admin_approval_session_token_123456789";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_session.as_bytes());
    let csrf_token = llmroller_admin::auth::derive_session_csrf(raw_session);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: admin_id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("admin-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("insert admin session");

    let admin_cookie = format!("llmroller_session={raw_session}");

    // 1. POST /admin/api/shares/{id}/approve: Admin approves custom provider share (SHR-3)
    let approve_req = Request::builder()
        .method("POST")
        .uri(format!("/admin/api/shares/{}/approve", share.share_id))
        .header(header::COOKIE, &admin_cookie)
        .header("x-csrf-token", &csrf_token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .expect("req");
    let approve_res = app.clone().oneshot(approve_req).await.expect("res");
    assert_eq!(approve_res.status(), StatusCode::OK);

    // 2. Telemetry: GET /admin/api/overview returns dynamic A6 disk usage calculation (UI-2, Section 3.3 A6)
    let overview_req = Request::builder()
        .method("GET")
        .uri("/admin/api/overview")
        .header(header::COOKIE, &admin_cookie)
        .body(Body::empty())
        .expect("req");
    let overview_res = app.oneshot(overview_req).await.expect("res");
    assert_eq!(overview_res.status(), StatusCode::OK);
    let overview_bytes = overview_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let overview_json: serde_json::Value =
        serde_json::from_slice(&overview_bytes).expect("parse json");
    assert!(overview_json["projected_disk_mb"].is_number());
    assert!(overview_json["unpriced_models_count"].is_number());
}

#[doc = "Spec: USG-1, USG-2, USG-3, SEC-6, UI-1, UI-2"]
#[tokio::test]
async fn test_usage_explorer_summary_events_and_formula_safe_csv_export() {
    use axum::http::header;
    use llmroller_core::ids::{ClientKeyId, RequestId, UsageEventId};
    use llmroller_core::seams::Clock as _;

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

    let entropy = OsEntropy;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    // 1. Create Alice user
    let alice = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "alice_usage",
            password: "supersecretalicepass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("alice");

    // Authenticate Alice session
    let raw_session = "alice_usage_session_token_1234567890";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_session.as_bytes());
    let csrf_token = llmroller_admin::auth::derive_session_csrf(raw_session);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: alice.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("alice-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("insert alice session");

    let alice_cookie = format!("llmroller_session={raw_session}");

    // 2. Insert usage events for Alice: one normal, one with malicious CSV formula cell (=SUM and +CMD)
    let ev1_id = UsageEventId::generate();
    let req1_id = RequestId::generate();
    let ev2_id = UsageEventId::generate();
    let req2_id = RequestId::generate();
    let clk_id = ClientKeyId::generate();

    sqlx::query(
        "INSERT INTO usage_events (
            id, ts, request_id, client_key_id, owner_user_id, canonical_id, endpoint, protocol,
            status, stream, cost_micro_usd, cost_source, input_uncached_tokens, cache_read_tokens,
            output_tokens, error_code
        ) VALUES
        (?, ?, ?, ?, ?, 'claude-sonnet-4.6', 'chat', 'chat', 200, 1, 15000, 'price_book', 1000, 200, 300, NULL),
        (?, ?, ?, ?, ?, 'gpt-4o', 'chat', 'chat', 500, 0, 5000, 'price_book', 500, 0, 0, '+CMD|/C calc')"
    )
    .bind(ev1_id.to_string())
    .bind(now_ms)
    .bind(req1_id.to_string())
    .bind(clk_id.to_string())
    .bind(alice.id.to_string())
    .bind(ev2_id.to_string())
    .bind(now_ms)
    .bind(req2_id.to_string())
    .bind(clk_id.to_string())
    .bind(alice.id.to_string())
    .execute(&pool)
    .await
    .expect("insert alice usage events");

    // 3. GET /api/user/usage/summary: returns aggregated usage metrics
    let summary_req = Request::builder()
        .method("GET")
        .uri("/api/user/usage/summary")
        .header(header::COOKIE, &alice_cookie)
        .body(Body::empty())
        .expect("req");
    let summary_res = app.clone().oneshot(summary_req).await.expect("res");
    assert_eq!(summary_res.status(), StatusCode::OK);
    let summary_bytes = summary_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let summary_json: serde_json::Value =
        serde_json::from_slice(&summary_bytes).expect("summary json");
    assert_eq!(summary_json["total_requests"], 2);
    assert_eq!(summary_json["input_tokens"], 1500);
    assert_eq!(summary_json["cached_tokens"], 200);
    assert_eq!(summary_json["output_tokens"], 300);
    assert_eq!(summary_json["total_cost_micro_usd"], 20000);

    // 4. GET /api/user/usage/events: returns paginated event list
    let events_req = Request::builder()
        .method("GET")
        .uri("/api/user/usage/events?limit=10")
        .header(header::COOKIE, &alice_cookie)
        .body(Body::empty())
        .expect("req");
    let events_res = app.clone().oneshot(events_req).await.expect("res");
    assert_eq!(events_res.status(), StatusCode::OK);
    let events_bytes = events_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let events_json: serde_json::Value =
        serde_json::from_slice(&events_bytes).expect("events json");
    assert!(events_json["events"].is_array());
    assert_eq!(events_json["events"].as_array().expect("arr").len(), 2);

    // 5. GET /api/user/usage/export.csv: streams formula-safe CSV export (SEC-6)
    let csv_req = Request::builder()
        .method("GET")
        .uri("/api/user/usage/export.csv")
        .header(header::COOKIE, &alice_cookie)
        .body(Body::empty())
        .expect("req");
    let csv_res = app.oneshot(csv_req).await.expect("res");
    assert_eq!(csv_res.status(), StatusCode::OK);
    let ct = csv_res
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .expect("str");
    assert!(ct.contains("text/csv"));

    let csv_bytes = csv_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let csv_str = std::str::from_utf8(&csv_bytes).expect("utf8");

    // Invariant SEC-6: +CMD is sanitized with leading single-quote: '+CMD
    assert!(
        csv_str.contains("'+CMD|/C calc"),
        "Formula injection payloads starting with +,-,=,@ must be prefixed with single quote (SEC-6)"
    );
    assert!(csv_str.contains("claude-sonnet-4.6"));
}
