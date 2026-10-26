use super::*;

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
