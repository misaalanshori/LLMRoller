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

/// Assert an HTMX partial rendered user-controlled text as inert markup (SEC-19).
fn assert_no_raw_user_markup(body: &str, context: &str) {
    assert!(
        !body.contains("<script"),
        "{context} leaked a raw <script> tag"
    );
    assert!(!body.contains("<img"), "{context} leaked a raw <img> tag");
    assert!(
        body.contains("&lt;script&gt;"),
        "{context} did not render the payload escaped"
    );
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
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    // 1. UI-1, UI-2: Personal Workspace template rendering
    let app_html = include_str!("../../../crates/admin/templates/app.html");
    assert!(app_html.contains("Personal Workspace"));
    assert!(app_html.contains("Global pool"));
    assert!(
        app_html.contains("value=\"open_design_amr\""),
        "PRV-2: personal provider selector must expose every built-in preset"
    );

    // 2. UI-1, UI-2: Admin Console template rendering with A6 disk meter and uPlot chart
    let admin_html = include_str!("../../../crates/admin/templates/admin.html");
    assert!(admin_html.contains("Admin Console"));
    assert!(
        admin_html.contains("value=\"open_design_amr\""),
        "PRV-2: global provider selector must expose every built-in preset"
    );
    let app_js = include_str!("../../../crates/admin/static/app.js");
    assert!(app_js.contains("open_design_amr"));
    assert!(app_js.contains("https://amr-link.open-design.ai/v1"));
    assert!(app_js.contains("https://api.commandcode.ai/provider/v1"));
    assert!(
        !app_js.contains("https://commandcode.ai/alpha"),
        "PRV-2: Command Code must use the current Provider API base URL"
    );
    assert!(admin_html.contains("Projected Disk Usage"));
    assert!(admin_html.contains("uplot-container"));
    assert!(admin_html.contains("id=\"uplot-summary\""));
    assert!(admin_html.contains("id=\"adm-settings-grid\""));
    assert!(admin_html.contains("id=\"adm-aliases-body\""));

    // 2b. UI-1, UI-2: Admin tabs are populated from the control-plane APIs and uPlot is initialized.
    let app_js = include_str!("../../../crates/admin/static/app.js");
    assert!(app_js.contains("loadAdminSettings"));
    assert!(app_js.contains("loadAdminAliases"));
    assert!(app_js.contains("loadUsageChart"));
    assert!(app_js.contains("/admin/api/settings"));
    assert!(app_js.contains("/admin/api/models"));
    assert!(app_js.contains("/admin/api/usage/summary"));
    assert!(app_js.contains("adm-settings-grid"));
    assert!(app_js.contains("adm-aliases-body"));
    assert!(app_js.contains("new uPlot("));
    assert!(
        app_html.contains("id=\"share-provider-select\""),
        "F05: the create-share modal must expose a provider selector"
    );
    assert!(
        app_js.contains("populateShareProviders"),
        "F05: the create-share modal must populate personal providers"
    );

    // 3. UI-4: Global traceability status banner
    let base_html = include_str!("../../../crates/admin/templates/base.html");
    assert!(base_html.contains("id=\"traceability-banner\""));
    assert!(base_html.contains("Content Traceability Mode is active"));

    // 4. TRC-6, SEC-19: Read-only session chat preview and XSS protection
    let malicious = "<script>alert('xss')</script><img src=x onerror=alert(1)>";
    let sanitized = sanitize_assistant_text(malicious);
    assert!(!sanitized.contains("<script"));
    // Raw HTML is neutralized into inert escaped text, never a live script tag
    // or event-handler attribute (SEC-19).
    assert!(sanitized.contains("&lt;script&gt;"));
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
        audit_hmac_key: None,
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
        audit_hmac_key: None,
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

#[doc = "Spec: UI-1, UI-2, UI-3, UI-4, SEC-5, SEC-6, SHR-3, RTE-7"]
#[tokio::test]
async fn test_htmx_partial_endpoints_and_live_swapping() {
    use axum::http::header;
    use llmroller_core::ids::ClientKeyId;
    use llmroller_core::seams::Clock as _;

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
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    let entropy = OsEntropy;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    // 1. Create User Dave
    let dave = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "dave_htmx",
            password: "supersecretdavepass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("dave");

    let raw_dave = "dave_session_token_123456789012345";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_dave.as_bytes());
    let csrf_token = llmroller_admin::auth::derive_session_csrf(raw_dave);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: dave.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("dave-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("insert dave session");
    let dave_cookie = format!("llmroller_session={raw_dave}");

    // Authenticate Admin session
    let raw_admin = "admin_htmx_session_token_123456789";
    let admin_id_hash = llmroller_store::crypto::sha256_hex(raw_admin.as_bytes());
    let admin_csrf = llmroller_admin::auth::derive_session_csrf(raw_admin);
    let admin_csrf_hash = llmroller_store::crypto::sha256_hex(admin_csrf.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &admin_id_hash,
            user_id: admin_id,
            csrf_hash: &admin_csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("admin-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("insert admin session");
    let admin_cookie = format!("llmroller_session={raw_admin}");

    // Insert client key and pin for Dave
    let malicious = "<script>alert('xss')</script><img src=x onerror=alert(1)>";
    let clk_id = ClientKeyId::generate();
    let prov_id = llmroller_core::ids::ProviderId::generate();
    let uk_id = llmroller_core::ids::UpstreamKeyId::generate();
    let model_pk = llmroller_core::ids::ModelPk::generate();
    let global_prov_id = llmroller_core::ids::ProviderId::generate();

    let mut tx = pool.begin().await.expect("tx");
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, prefix, key_hash, label, routing_scope, created_at)
         VALUES (?, ?, 'llr_dave', 'dummyhash123', ?, 'personal_first', ?)"
    )
    .bind(clk_id.to_string())
    .bind(dave.id.to_string())
    .bind(format!("Dave {malicious}"))
    .bind(now_ms)
    .execute(&mut *tx)
    .await
    .expect("insert client key");

    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, owner_user_id, created_at)
         VALUES (?, ?, 'custom', ?, '{}', 'personal', ?, ?)"
    )
    .bind(prov_id.to_string())
    .bind(format!("dave_prov {malicious}"))
    .bind(format!("https://dave.ai/{malicious}"))
    .bind(dave.id.to_string())
    .bind(now_ms)
    .execute(&mut *tx)
    .await
    .expect("insert provider");

    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, owner_user_id, label, secret_enc, secret_last4, master_key_id, created_at)
         VALUES (?, ?, ?, ?, X'00', '1234', 'key-0', ?)"
    )
    .bind(uk_id.to_string())
    .bind(prov_id.to_string())
    .bind(dave.id.to_string())
    .bind(format!("dave_key {malicious}"))
    .bind(now_ms)
    .execute(&mut *tx)
    .await
    .expect("insert upstream key");

    // Admin-owned global provider with the same hostile display fields.
    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, owner_user_id, created_at)
         VALUES (?, ?, 'custom', ?, '{}', 'global', NULL, ?)"
    )
    .bind(global_prov_id.to_string())
    .bind(format!("global_prov {malicious}"))
    .bind(format!("https://global.ai/{malicious}"))
    .bind(now_ms)
    .execute(&mut *tx)
    .await
    .expect("insert global provider");

    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id)
         VALUES (?, 'claude-sonnet-4.6')",
    )
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model");

    sqlx::query(
        "INSERT INTO pins (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier, created_at, last_used_at, expires_at)
         VALUES (?, 'dave-active-hash', ?, ?, 1, ?, ?, ?)"
    )
    .bind(clk_id.to_string())
    .bind(model_pk.to_string())
    .bind(uk_id.to_string())
    .bind(now_ms)
    .bind(now_ms)
    .bind(now_ms + 3600 * 1000)
    .execute(&mut *tx)
    .await
    .expect("insert pin");
    tx.commit().await.expect("commit");

    // 1. GET /app/partials/pins: returns rendered HTML rows with live HTMX Unpin & Chat Preview actions
    let pins_req = Request::builder()
        .method("GET")
        .uri("/app/partials/pins")
        .header(header::COOKIE, &dave_cookie)
        .header("hx-request", "true")
        .body(Body::empty())
        .expect("req");
    let pins_res = app.clone().oneshot(pins_req).await.expect("res");
    assert_eq!(pins_res.status(), StatusCode::OK);
    let pins_body = pins_res.into_body().collect().await.expect("b").to_bytes();
    let pins_html = std::str::from_utf8(&pins_body).expect("utf8");
    assert!(pins_html.contains("dave-active-"));
    assert!(pins_html.contains("data-action=\"preview-chat\""));
    assert!(pins_html.contains(&format!(
        "hx-delete=\"/api/user/pins/{clk_id}/dave-active-hash/claude-sonnet-4.6\""
    )));
    assert_no_raw_user_markup(pins_html, "/app/partials/pins");

    // 2. GET /app/partials/client-keys: returns rendered HTML rows with HTMX Revoke action
    let clk_req = Request::builder()
        .method("GET")
        .uri("/app/partials/client-keys")
        .header(header::COOKIE, &dave_cookie)
        .header("hx-request", "true")
        .body(Body::empty())
        .expect("req");
    let clk_res = app.clone().oneshot(clk_req).await.expect("res");
    assert_eq!(clk_res.status(), StatusCode::OK);
    let clk_body = clk_res.into_body().collect().await.expect("b").to_bytes();
    let clk_html = std::str::from_utf8(&clk_body).expect("utf8");
    assert!(clk_html.contains("Dave &lt;script&gt;"));
    assert!(clk_html.contains(&format!("hx-delete=\"/api/user/client-keys/{clk_id}\"")));
    assert_no_raw_user_markup(clk_html, "/app/partials/client-keys");

    // 2b. GET /app/partials/providers: personal provider and key labels are inert.
    let prov_req = Request::builder()
        .method("GET")
        .uri("/app/partials/providers")
        .header(header::COOKIE, &dave_cookie)
        .header("hx-request", "true")
        .body(Body::empty())
        .expect("req");
    let prov_res = app.clone().oneshot(prov_req).await.expect("res");
    assert_eq!(prov_res.status(), StatusCode::OK);
    let prov_body = prov_res.into_body().collect().await.expect("b").to_bytes();
    let prov_html = std::str::from_utf8(&prov_body).expect("utf8");
    assert!(prov_html.contains("dave_prov &lt;script&gt;"));
    assert_no_raw_user_markup(prov_html, "/app/partials/providers");

    // 2c. GET /admin/partials/providers: global provider fields are inert.
    let admin_prov_req = Request::builder()
        .method("GET")
        .uri("/admin/partials/providers")
        .header(header::COOKIE, &admin_cookie)
        .header("hx-request", "true")
        .body(Body::empty())
        .expect("req");
    let admin_prov_res = app.clone().oneshot(admin_prov_req).await.expect("res");
    assert_eq!(admin_prov_res.status(), StatusCode::OK);
    let admin_prov_body = admin_prov_res
        .into_body()
        .collect()
        .await
        .expect("b")
        .to_bytes();
    let admin_prov_html = std::str::from_utf8(&admin_prov_body).expect("utf8");
    assert!(admin_prov_html.contains("global_prov &lt;script&gt;"));
    assert_no_raw_user_markup(admin_prov_html, "/admin/partials/providers");

    // 3. HTMX Unpin action: owner-scoped DELETE /api/user/pins/... with hx-request returns 200 OK
    let unpin_req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/user/pins/{clk_id}/dave-active-hash/claude-sonnet-4.6"
        ))
        .header(header::COOKIE, &dave_cookie)
        .header("x-csrf-token", &csrf_token)
        .header("hx-request", "true")
        .body(Body::empty())
        .expect("req");
    let unpin_res = app.clone().oneshot(unpin_req).await.expect("res");
    assert_eq!(
        unpin_res.status(),
        StatusCode::OK,
        "HTMX DELETE requests must return 200 OK so HTMX replaces/removes the target row"
    );

    // 4. GET /admin/partials/overview: returns rendered A6 telemetry cards for admin
    let adm_ov_req = Request::builder()
        .method("GET")
        .uri("/admin/partials/overview")
        .header(header::COOKIE, &admin_cookie)
        .header("hx-request", "true")
        .body(Body::empty())
        .expect("req");
    let adm_ov_res = app.oneshot(adm_ov_req).await.expect("res");
    assert_eq!(adm_ov_res.status(), StatusCode::OK);
    let adm_ov_body = adm_ov_res
        .into_body()
        .collect()
        .await
        .expect("b")
        .to_bytes();
    let adm_ov_html = std::str::from_utf8(&adm_ov_body).expect("utf8");
    assert!(adm_ov_html.contains("Projected Disk Usage:"));
    assert!(adm_ov_html.contains("disk-meter"));
}

#[doc = "Spec: AUD-2 — Control-plane API request logging and API logs stream"]
#[tokio::test]
async fn test_control_plane_api_logging_and_api_logs_query() {
    use axum::http::header;
    use std::time::Duration;

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
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    let entropy = OsEntropy;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "aud2_user",
            password: "supersecretaud2pass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("user");

    let raw_session = "aud2_session_token_12345678901234";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_session.as_bytes());
    let csrf_token = llmroller_admin::auth::derive_session_csrf(raw_session);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: user.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("aud2-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("session");
    let cookie = format!("llmroller_session={raw_session}");

    // 1. Make an authenticated control plane request that should be logged
    let req = Request::builder()
        .method("GET")
        .uri("/api/user/client-keys")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let res = app.clone().oneshot(req).await.expect("res");
    assert_eq!(res.status(), StatusCode::OK);

    // Yield briefly to let non-blocking tokio::spawn commit
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 2. Query API logs via GET /api/user/logs?stream=api
    let logs_req = Request::builder()
        .method("GET")
        .uri("/api/user/logs?stream=api")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("logs req");
    let logs_res = app.clone().oneshot(logs_req).await.expect("logs res");
    assert_eq!(logs_res.status(), StatusCode::OK);
    let body_bytes = logs_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).expect("json");
    assert_eq!(body_json["stream"], "api");
    let rows = body_json["rows"].as_array().expect("rows");
    assert!(!rows.is_empty(), "control plane request must be recorded");
    assert_eq!(rows[0]["method"], "GET");
    assert_eq!(rows[0]["path"], "/api/user/client-keys");
    assert_eq!(rows[0]["status"], 200);

    // 3. Export API logs CSV via GET /api/user/logs/export.csv?stream=api
    let csv_req = Request::builder()
        .method("GET")
        .uri("/api/user/logs/export.csv?stream=api")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("csv req");
    let csv_res = app.oneshot(csv_req).await.expect("csv res");
    assert_eq!(csv_res.status(), StatusCode::OK);
    let csv_bytes = csv_res.into_body().collect().await.expect("b").to_bytes();
    let csv_str = std::str::from_utf8(&csv_bytes).expect("utf8");
    assert!(csv_str.starts_with("id,ts,request_id,method,path,status,duration_ms,ip_addr"));
    assert!(csv_str.contains("/api/user/client-keys"));
}

#[doc = "Spec: USG-9 — Dual-Stream Logs Explorer, Request Detail Inspection Drawer, and CSV export"]
#[tokio::test]
async fn test_dual_stream_logs_explorer_and_request_detail_drawer() {
    use axum::http::header;
    use llmroller_core::ids::{ClientKeyId, RequestId, UsageEventId};

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
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    let entropy = OsEntropy;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "usg9_user",
            password: "supersecretusg9pass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("user");

    let raw_session = "usg9_session_token_12345678901234";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_session.as_bytes());
    let csrf_token = llmroller_admin::auth::derive_session_csrf(raw_session);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: user.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("usg9-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("session");
    let cookie = format!("llmroller_session={raw_session}");

    let admin_session = "usg9_admin_session_token_12345678";
    let admin_id_hash = llmroller_store::crypto::sha256_hex(admin_session.as_bytes());
    let admin_csrf = llmroller_admin::auth::derive_session_csrf(admin_session);
    let admin_csrf_hash = llmroller_store::crypto::sha256_hex(admin_csrf.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &admin_id_hash,
            user_id: admin_id,
            csrf_hash: &admin_csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("admin-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("admin session");
    let admin_cookie = format!("llmroller_session={admin_session}");

    let ev_id = UsageEventId::generate();
    let req_id = RequestId::generate();
    let clk_id = ClientKeyId::generate();

    sqlx::query(
        "INSERT INTO usage_events (
            id, ts, request_id, client_key_id, owner_user_id, canonical_id, endpoint, protocol,
            status, stream, cost_micro_usd, list_cost_micro_usd, plan_multiplier_bps,
            billing_coverage_mode, cost_source, input_uncached_tokens, cache_read_tokens,
            output_tokens, latency_ms, ttfb_ms, stop_reason
        ) VALUES (?, ?, ?, ?, ?, 'gpt-4o', 'chat', 'chat', 200, 1, 15000, 15000, 10000, 'pay_as_you_go', 'price_book', 1000, 200, 300, 450, 120, 'stop')"
    )
    .bind(ev_id.to_string())
    .bind(now_ms)
    .bind(req_id.to_string())
    .bind(clk_id.to_string())
    .bind(user.id.to_string())
    .execute(&pool)
    .await
    .expect("insert event");

    sqlx::query(
        "INSERT INTO usage_attempts (request_id, attempt_no, ts, upstream_key_id, outcome, http_status, bytes_sent, charged_micro_usd, charge_basis)
         VALUES (?, 1, ?, 'uk-test-1', 'served', 200, 1024, 15000, 'actual')"
    )
    .bind(req_id.to_string())
    .bind(now_ms)
    .execute(&pool)
    .await
    .expect("insert attempt");

    // 1. GET /api/user/logs?stream=inference
    let logs_req = Request::builder()
        .method("GET")
        .uri("/api/user/logs?stream=inference")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let logs_res = app.clone().oneshot(logs_req).await.expect("res");
    assert_eq!(logs_res.status(), StatusCode::OK);
    let body_bytes = logs_res.into_body().collect().await.expect("b").to_bytes();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).expect("json");
    assert_eq!(body_json["stream"], "inference");
    let rows = body_json["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["canonical_id"], "gpt-4o");
    assert_eq!(rows[0]["status"], 200);

    // 2. GET /api/user/logs/{request_id}: Request Detail Inspection Drawer
    let detail_req = Request::builder()
        .method("GET")
        .uri(format!("/api/user/logs/{}", req_id))
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let detail_res = app.clone().oneshot(detail_req).await.expect("res");
    assert_eq!(detail_res.status(), StatusCode::OK);
    let d_bytes = detail_res
        .into_body()
        .collect()
        .await
        .expect("b")
        .to_bytes();
    let d_json: serde_json::Value = serde_json::from_slice(&d_bytes).expect("json");
    assert_eq!(d_json["request_id"], req_id.to_string());
    assert_eq!(d_json["status"], 200);
    assert_eq!(d_json["stop_reason"], "stop");
    assert_eq!(d_json["tokens"]["input"], 1000);
    assert_eq!(d_json["tokens"]["output"], 300);
    assert_eq!(d_json["tokens"]["cache_read"], 200);
    assert_eq!(d_json["cost"]["cost_micro_usd"], 15000);
    let attempts = d_json["attempts"].as_array().expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["outcome"], "served");

    // 3. GET /api/user/logs/export.csv?stream=inference
    let csv_req = Request::builder()
        .method("GET")
        .uri("/api/user/logs/export.csv?stream=inference")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("csv req");
    let csv_res = app.clone().oneshot(csv_req).await.expect("csv res");
    assert_eq!(csv_res.status(), StatusCode::OK);
    let csv_bytes = csv_res.into_body().collect().await.expect("b").to_bytes();
    let csv_str = std::str::from_utf8(&csv_bytes).expect("utf8");
    assert!(csv_str.contains("gpt-4o"));

    // 4. GET /app/partials/logs?stream=inference (USG-9, SRDD 14.l)
    let p_inf_req = Request::builder()
        .method("GET")
        .uri("/app/partials/logs?stream=inference")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let p_inf_res = app.clone().oneshot(p_inf_req).await.expect("res");
    assert_eq!(p_inf_res.status(), StatusCode::OK);
    let p_inf_bytes = p_inf_res.into_body().collect().await.expect("b").to_bytes();
    let p_inf_html = std::str::from_utf8(&p_inf_bytes).expect("utf8");
    assert!(p_inf_html.contains("gpt-4o"));
    assert!(p_inf_html.contains("● 200"));
    assert!(p_inf_html.contains("data-action=\"open-drawer\""));

    // 5. GET /app/partials/logs?stream=api (USG-9, SRDD 14.l)
    let p_api_req = Request::builder()
        .method("GET")
        .uri("/app/partials/logs?stream=api")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let p_api_res = app.clone().oneshot(p_api_req).await.expect("res");
    assert_eq!(p_api_res.status(), StatusCode::OK);
    let p_api_bytes = p_api_res.into_body().collect().await.expect("b").to_bytes();
    let p_api_html = std::str::from_utf8(&p_api_bytes).expect("utf8");
    assert!(p_api_html.contains("<th>Endpoint</th>"));

    // 6. GET /app/partials/logs/{request_id} (USG-9, SRDD 14.m 7-section drawer)
    let p_detail_req = Request::builder()
        .method("GET")
        .uri(format!("/app/partials/logs/{}", req_id))
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let p_detail_res = app.clone().oneshot(p_detail_req).await.expect("res");
    assert_eq!(p_detail_res.status(), StatusCode::OK);
    let p_detail_bytes = p_detail_res
        .into_body()
        .collect()
        .await
        .expect("b")
        .to_bytes();
    let p_detail_html = std::str::from_utf8(&p_detail_bytes).expect("utf8");
    assert!(p_detail_html.contains("Log Detail"));
    assert!(p_detail_html.contains("1. Request"));
    assert!(p_detail_html.contains("3. Tokens &amp; Cost"));
    assert!(p_detail_html.contains("gpt-4o"));
    assert!(p_detail_html.contains("data-action=\"close-drawer\""));

    // 7. GET /app and GET /admin contain the Logs tab and pane container
    let app_req = Request::builder()
        .method("GET")
        .uri("/app")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let app_res = app.clone().oneshot(app_req).await.expect("res");
    assert_eq!(app_res.status(), StatusCode::OK);
    let app_bytes = app_res.into_body().collect().await.expect("b").to_bytes();
    let app_html = std::str::from_utf8(&app_bytes).expect("utf8");
    assert!(app_html.contains("data-tab=\"tab-logs\""));
    assert!(app_html.contains("id=\"tab-logs\""));
    assert!(app_html.contains("id=\"right-inspection-drawer\""));

    let admin_req = Request::builder()
        .method("GET")
        .uri("/admin")
        .header(header::COOKIE, &admin_cookie)
        .body(Body::empty())
        .expect("req");
    let admin_res = app.oneshot(admin_req).await.expect("res");
    assert_eq!(admin_res.status(), StatusCode::OK);
    let admin_bytes = admin_res.into_body().collect().await.expect("b").to_bytes();
    let admin_html = std::str::from_utf8(&admin_bytes).expect("utf8");
    assert!(admin_html.contains("data-tab=\"tab-adm-logs\""));
    assert!(admin_html.contains("id=\"tab-adm-logs\""));
    assert!(admin_html.contains("id=\"right-inspection-drawer\""));
}

#[doc = "Spec: USG-10 — Usage Ledger, 5-Token Summary Strip, and Plan Coverage Multipliers"]
#[tokio::test]
async fn test_usage_ledger_token_strip_and_plan_coverage() {
    use axum::http::header;
    use llmroller_core::ids::{ClientKeyId, RequestId, UsageEventId};

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
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    let entropy = OsEntropy;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "usg10_user",
            password: "supersecretusg10pass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("user");

    let raw_session = "usg10_session_token_12345678901234";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_session.as_bytes());
    let csrf_token = llmroller_admin::auth::derive_session_csrf(raw_session);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: user.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("usg10-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("session");
    let cookie = format!("llmroller_session={raw_session}");

    let clk_id = ClientKeyId::generate();

    // Event 1: Subscription plan (50% multiplier): list_cost = $0.02, effective cost = $0.01
    sqlx::query(
        "INSERT INTO usage_events (
            id, ts, request_id, client_key_id, owner_user_id, canonical_id, endpoint, protocol,
            status, stream, cost_micro_usd, list_cost_micro_usd, plan_multiplier_bps,
            billing_coverage_mode, cost_source, input_uncached_tokens, cache_read_tokens,
            cache_write_tokens, output_tokens
        ) VALUES (?, ?, ?, ?, ?, 'deepseek-v4.1-flash', 'chat', 'chat', 200, 1, 10000, 20000, 5000, 'subscription_plan', 'price_book', 1000, 0, 0, 500)"
    )
    .bind(UsageEventId::generate().to_string())
    .bind(now_ms)
    .bind(RequestId::generate().to_string())
    .bind(clk_id.to_string())
    .bind(user.id.to_string())
    .execute(&pool)
    .await
    .expect("event 1");

    // Event 2: Pay-as-you-go: list_cost = $0.015, effective cost = $0.015
    sqlx::query(
        "INSERT INTO usage_events (
            id, ts, request_id, client_key_id, owner_user_id, canonical_id, endpoint, protocol,
            status, stream, cost_micro_usd, list_cost_micro_usd, plan_multiplier_bps,
            billing_coverage_mode, cost_source, input_uncached_tokens, cache_read_tokens,
            cache_write_tokens, output_tokens
        ) VALUES (?, ?, ?, ?, ?, 'deepseek-v4.1-flash', 'chat', 'chat', 200, 1, 15000, 15000, 10000, 'pay_as_you_go', 'price_book', 500, 200, 0, 300)"
    )
    .bind(UsageEventId::generate().to_string())
    .bind(now_ms)
    .bind(RequestId::generate().to_string())
    .bind(clk_id.to_string())
    .bind(user.id.to_string())
    .execute(&pool)
    .await
    .expect("event 2");

    // Query Usage Ledger
    let ledger_req = Request::builder()
        .method("GET")
        .uri("/api/user/usage/ledger")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("ledger req");
    let ledger_res = app.clone().oneshot(ledger_req).await.expect("res");
    assert_eq!(ledger_res.status(), StatusCode::OK);
    let bytes = ledger_res
        .into_body()
        .collect()
        .await
        .expect("b")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

    let summary = &json["summary"];
    assert_eq!(summary["total_tokens"], 2500); // 1500 + 1000
    assert_eq!(summary["input_tokens"], 1500);
    assert_eq!(summary["output_tokens"], 800);
    assert_eq!(summary["cache_read_tokens"], 200);
    assert_eq!(summary["billed_cost_micro_usd"], 25000);
    assert_eq!(summary["list_cost_micro_usd"], 35000);
    assert_eq!(summary["plan_covered_savings_micro_usd"], 10000); // $0.035 - $0.025 = $0.01

    let rows = json["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);

    // 2. GET /app/partials/ledger (USG-10, SRDD 14.n)
    let p_ledger_req = Request::builder()
        .method("GET")
        .uri("/app/partials/ledger")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let p_ledger_res = app.clone().oneshot(p_ledger_req).await.expect("res");
    assert_eq!(p_ledger_res.status(), StatusCode::OK);
    let p_ledger_bytes = p_ledger_res
        .into_body()
        .collect()
        .await
        .expect("b")
        .to_bytes();
    let p_ledger_html = std::str::from_utf8(&p_ledger_bytes).expect("utf8");
    assert!(p_ledger_html.contains("Total tokens"));
    assert!(p_ledger_html.contains("Plan Covered Savings"));
    assert!(p_ledger_html.contains("Covered by plan credits"));
    assert!(p_ledger_html.contains("deepseek-v4.1-flash"));

    // 3. GET /app and GET /admin contain the Usage Ledger tab and pane container
    let app_req = Request::builder()
        .method("GET")
        .uri("/app")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let app_res = app.clone().oneshot(app_req).await.expect("res");
    assert_eq!(app_res.status(), StatusCode::OK);
    let app_bytes = app_res.into_body().collect().await.expect("b").to_bytes();
    let app_html = std::str::from_utf8(&app_bytes).expect("utf8");
    assert!(app_html.contains("data-tab=\"tab-ledger\""));
    assert!(app_html.contains("id=\"tab-ledger\""));

    let admin_session = "usg10_admin_session_token_12345678";
    let admin_id_hash = llmroller_store::crypto::sha256_hex(admin_session.as_bytes());
    let admin_csrf = llmroller_admin::auth::derive_session_csrf(admin_session);
    let admin_csrf_hash = llmroller_store::crypto::sha256_hex(admin_csrf.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &admin_id_hash,
            user_id: _admin_id,
            csrf_hash: &admin_csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("admin-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("admin session");
    let admin_cookie = format!("llmroller_session={admin_session}");

    let admin_req = Request::builder()
        .method("GET")
        .uri("/admin")
        .header(header::COOKIE, &admin_cookie)
        .body(Body::empty())
        .expect("req");
    let admin_res = app.clone().oneshot(admin_req).await.expect("res");
    assert_eq!(admin_res.status(), StatusCode::OK);
    let admin_bytes = admin_res.into_body().collect().await.expect("b").to_bytes();
    let admin_html = std::str::from_utf8(&admin_bytes).expect("utf8");
    assert!(admin_html.contains("data-tab=\"tab-adm-ledger\""));
    assert!(admin_html.contains("id=\"tab-adm-ledger\""));

    let p_adm_ledger_req = Request::builder()
        .method("GET")
        .uri("/admin/partials/ledger")
        .header(header::COOKIE, &admin_cookie)
        .body(Body::empty())
        .expect("req");
    let p_adm_ledger_res = app.oneshot(p_adm_ledger_req).await.expect("res");
    assert_eq!(p_adm_ledger_res.status(), StatusCode::OK);
    let p_adm_ledger_bytes = p_adm_ledger_res
        .into_body()
        .collect()
        .await
        .expect("b")
        .to_bytes();
    let p_adm_ledger_html = std::str::from_utf8(&p_adm_ledger_bytes).expect("utf8");
    assert!(p_adm_ledger_html.contains("Total tokens"));
}

#[doc = "Spec: UI-6 — Overview Analytics Dashboard KPI summary cards, time series, and model breakdown"]
#[tokio::test]
async fn test_overview_analytics_dashboard_kpis_and_models_breakdown() {
    use axum::http::header;

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
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    let entropy = OsEntropy;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "ui6_user",
            password: "supersecretui6pass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("user");

    let raw_session = "ui6_session_token_12345678901234";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_session.as_bytes());
    let csrf_token = llmroller_admin::auth::derive_session_csrf(raw_session);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: user.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("ui6-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("session");
    let cookie = format!("llmroller_session={raw_session}");

    let raw_admin = "ui6_admin_token_12345678901234";
    let admin_hash = llmroller_store::crypto::sha256_hex(raw_admin.as_bytes());
    let admin_csrf = llmroller_admin::auth::derive_session_csrf(raw_admin);
    let admin_csrf_hash = llmroller_store::crypto::sha256_hex(admin_csrf.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &admin_hash,
            user_id: admin_id,
            csrf_hash: &admin_csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("admin-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("admin session");
    let admin_cookie = format!("llmroller_session={raw_admin}");

    // Insert hourly rollups for user
    sqlx::query(
        "INSERT INTO usage_rollups_hourly (hour_ms, owner_user_id, client_key_id, upstream_key_id, provider_id, provider_scope, share_id, canonical_id, requests, errors, input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens, reasoning_tokens, cost_micro_usd, list_cost_micro_usd)
         VALUES (?, ?, 'ck1', 'uk1', 'p1', 'personal', '', 'gpt-4o', 10, 0, 1000, 200, 0, 500, 0, 15000, 15000),
                (?, ?, 'ck1', 'uk1', 'p1', 'personal', '', 'claude-3-5-sonnet', 5, 0, 2000, 0, 0, 800, 0, 25000, 25000)",
    )
    .bind(now_ms - 3_600_000)
    .bind(user.id.to_string())
    .bind(now_ms)
    .bind(user.id.to_string())
    .execute(&pool)
    .await
    .expect("insert rollups");

    // 1. GET /api/user/overview
    let ov_req = Request::builder()
        .method("GET")
        .uri("/api/user/overview?range=7d")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .expect("req");
    let ov_res = app.clone().oneshot(ov_req).await.expect("res");
    assert_eq!(ov_res.status(), StatusCode::OK);
    let bytes = ov_res.into_body().collect().await.expect("b").to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

    assert_eq!(json["kpi"]["requests"], 15);
    assert_eq!(json["kpi"]["cost_micro_usd"], 40000);
    assert_eq!(json["kpi"]["tokens_total"], 4500); // (1000+200+500) + (2000+800) = 1700 + 2800 = 4500
    assert!(json["series"].is_array());
    let models = json["models"].as_array().expect("models");
    assert_eq!(models.len(), 2);
    // Cost desc: claude-3-5-sonnet (25000) before gpt-4o (15000)
    assert_eq!(models[0]["canonical_id"], "claude-3-5-sonnet");
    assert_eq!(models[1]["canonical_id"], "gpt-4o");

    // 2. GET /admin/api/overview
    let adm_req = Request::builder()
        .method("GET")
        .uri("/admin/api/overview")
        .header(header::COOKIE, &admin_cookie)
        .body(Body::empty())
        .expect("req");
    let adm_res = app.oneshot(adm_req).await.expect("res");
    assert_eq!(adm_res.status(), StatusCode::OK);
    let adm_bytes = adm_res.into_body().collect().await.expect("b").to_bytes();
    let adm_json: serde_json::Value = serde_json::from_slice(&adm_bytes).expect("json");
    assert!(adm_json["projected_disk_mb"].is_number());
    assert!(adm_json["kpi"]["requests"].is_number());
    assert!(adm_json["models"].is_array());
}
