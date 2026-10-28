use super::*;

#[doc = "Spec: AUD-2 — Control-plane API request logging and API logs stream"]
#[tokio::test]
async fn test_control_plane_api_logging_and_api_logs_query() {
    use axum::http::header;
    use std::time::Duration;

    let (dir, pool, master_key, _admin_id, instance_state) = setup_dashboard_test_db().await;

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
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

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
    let csrf_token = auth_state.derive_session_csrf(raw_session);
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
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

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
    let csrf_token = auth_state.derive_session_csrf(raw_session);
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
    let admin_csrf = auth_state.derive_session_csrf(admin_session);
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

#[doc = "Spec: UI-6 — Overview Analytics Dashboard KPI summary cards, time series, and model breakdown"]
#[tokio::test]
async fn test_overview_analytics_dashboard_kpis_and_models_breakdown() {
    use axum::http::header;

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
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

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
    let csrf_token = auth_state.derive_session_csrf(raw_session);
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
    let admin_csrf = auth_state.derive_session_csrf(raw_admin);
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
