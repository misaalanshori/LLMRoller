use super::*;

#[doc = "Spec: UI-1, UI-2, UI-4, TRC-6, AUTH-4, USR-5, MDL-3, MDL-4, AUD-1, SEC-6, TRC-5, TRC-8, TRC-9, SEC-19"]
#[tokio::test]
async fn test_htmx_dashboards_key_projection_and_chat_preview() {
    let (dir, pool, master_key, admin_id, instance_state) = setup_dashboard_test_db().await;

    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    // 1. UI-1, UI-2: Personal Workspace template rendering
    let app_html = include_str!("../../../../crates/admin/templates/app.html");
    assert!(app_html.contains("Personal Workspace"));
    assert!(app_html.contains("Global pool"));
    assert!(
        app_html.contains("value=\"open_design_amr\""),
        "PRV-2: personal provider selector must expose every built-in preset"
    );

    // 2. UI-1, UI-2: Admin Console template rendering with A6 disk meter and uPlot chart
    let admin_html = include_str!("../../../../crates/admin/templates/admin.html");
    assert!(admin_html.contains("Admin Console"));
    assert!(
        admin_html.contains("value=\"open_design_amr\""),
        "PRV-2: global provider selector must expose every built-in preset"
    );
    let app_js = llmroller_admin::assets::APP_JS;
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
    let app_js = llmroller_admin::assets::APP_JS;
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
    let base_html = include_str!("../../../../crates/admin/templates/base.html");
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
    let csrf_token = auth_state.derive_session_csrf(raw_session);
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

    let (dir, pool, master_key, admin_id, instance_state) = setup_dashboard_test_db().await;

    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

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
    let csrf_token = auth_state.derive_session_csrf(raw_session);
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

#[doc = "Spec: UI-1, UI-2, UI-3, UI-4, SEC-5, SEC-6, SHR-3, RTE-7"]
#[tokio::test]
async fn test_htmx_partial_endpoints_and_live_swapping() {
    use axum::http::header;
    use llmroller_core::ids::ClientKeyId;
    use llmroller_core::seams::Clock as _;

    let (dir, pool, master_key, admin_id, instance_state) = setup_dashboard_test_db().await;

    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

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
    let csrf_token = auth_state.derive_session_csrf(raw_dave);
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
    let admin_csrf = auth_state.derive_session_csrf(raw_admin);
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

#[doc = "Spec: PRV-1, PRV-2, UI-1, UI-2, UI-5 — full provider preset catalog in UI selectors, preset-slug creation, partial refetch, and dual-theme form styling"]
#[tokio::test]
async fn test_provider_preset_creation_partial_refetch_and_light_mode_styles() {
    use axum::http::header;

    let app_html = include_str!("../../../../crates/admin/templates/app.html");
    let admin_html = include_str!("../../../../crates/admin/templates/admin.html");
    let app_js = llmroller_admin::assets::APP_JS;
    let app_css = include_str!("../../../../crates/admin/static/app.css");

    for preset in [
        "anthropic_claude_code_stealth_oauth",
        "openai_codex_chatgpt_backend_oauth_and_websocket",
        "antigravity_google_cloud_code_assist",
        "groq_cloud_lpu_inference",
        "deepseek_api_and_interleaved_reasoning_content",
        "together_ai_cloud",
        "fireworks_ai_inference",
    ] {
        assert!(
            admin_html.contains(&format!("value=\"{preset}\"")),
            "admin #gp-kind must include preset {preset}"
        );
        assert!(
            app_html.contains(&format!("value=\"{preset}\"")),
            "personal #prov-kind must include preset {preset}"
        );
    }
    assert!(app_js.contains("invalidateAndRefetch"));
    assert!(app_js.contains("refreshPartial"));
    assert!(app_css.contains("--input-bg:"));
    assert!(app_css.contains(".btn-outline"));
    assert!(app_css.contains(".setting-control"));

    let (dir, pool, master_key, admin_id, instance_state) = setup_dashboard_test_db().await;
    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let raw_admin = "admin_preset_session_token_123456789";
    let admin_id_hash = llmroller_store::crypto::sha256_hex(raw_admin.as_bytes());
    let admin_csrf = auth_state.derive_session_csrf(raw_admin);
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

    // Create a global provider by passing a preset slug directly in `kind`
    let create_req = Request::builder()
        .method("POST")
        .uri("/admin/api/providers")
        .header(header::COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"name":"Global Groq Pool","kind":"groq_cloud_lpu_inference","base_url":""}"#,
        ))
        .expect("req");
    let create_res = app.clone().oneshot(create_req).await.expect("res");
    assert_eq!(create_res.status(), StatusCode::CREATED);

    // Refetch /admin/partials/providers and verify the new provider and preset badge render immediately
    let partial_req = Request::builder()
        .method("GET")
        .uri("/admin/partials/providers")
        .header(header::COOKIE, &admin_cookie)
        .header("hx-request", "true")
        .body(Body::empty())
        .expect("req");
    let partial_res = app.clone().oneshot(partial_req).await.expect("res");
    assert_eq!(partial_res.status(), StatusCode::OK);
    let partial_bytes = partial_res
        .into_body()
        .collect()
        .await
        .expect("b")
        .to_bytes();
    let partial_html = std::str::from_utf8(&partial_bytes).expect("utf8");
    assert!(partial_html.contains("Global Groq Pool"));
    assert!(partial_html.contains("groq_cloud_lpu_inference"));
    assert!(partial_html.contains("https://api.groq.com/openai/v1"));

    // Verify GET /admin/api/prices works for the Price Book tab
    let prices_req = Request::builder()
        .method("GET")
        .uri("/admin/api/prices")
        .header(header::COOKIE, &admin_cookie)
        .body(Body::empty())
        .expect("req");
    let prices_res = app.oneshot(prices_req).await.expect("res");
    assert_eq!(prices_res.status(), StatusCode::OK);
}
