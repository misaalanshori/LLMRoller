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
        audit_hmac_key: None,
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

#[doc = "Spec: TRC-5, TRC-8, TRC-9, SEC-17, SEC-19, Partial: TRC-6"]
#[tokio::test]
async fn test_rbac_isolated_sessions_and_preview() {
    use llmroller_core::chat_preview::{compute_lcp, escape_html, sanitize_assistant_text};
    use llmroller_core::ids::RequestId;
    use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
    use llmroller_store::traces::{
        SealJob, StoredTurnTraceV2, insert_sealed_trace_tx, seal_turn_sync,
    };

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
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let entropy = OsEntropy;

    // Create Alice and Bob users
    let alice = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "alice_trc",
            password: "supersecretalicepass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("alice");

    let bob = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "bob_trc",
            password: "supersecretbobpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("bob");

    // Login Alice
    let login_page_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/login")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("res");
    let login_cookie = login_page_res
        .headers()
        .get(header::SET_COOKIE)
        .expect("cookie")
        .to_str()
        .expect("str")
        .split(';')
        .next()
        .expect("cookie part")
        .to_string();
    let login_csrf = login_page_res
        .headers()
        .get("x-csrf-token")
        .expect("csrf")
        .to_str()
        .expect("str")
        .to_string();

    let login_alice = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header(header::COOKIE, &login_cookie)
                .header("x-csrf-token", &login_csrf)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "username": "alice_trc",
                        "password": "supersecretalicepass123",
                    })
                    .to_string(),
                ))
                .expect("req"),
        )
        .await
        .expect("res");
    assert_eq!(login_alice.status(), StatusCode::OK);

    let alice_cookie = login_alice
        .headers()
        .get(header::SET_COOKIE)
        .expect("alice cookie")
        .to_str()
        .expect("str")
        .split(';')
        .next()
        .expect("part")
        .to_string();
    let alice_csrf = login_alice
        .headers()
        .get("x-csrf-token")
        .expect("alice csrf")
        .to_str()
        .expect("str")
        .to_string();

    let mut tx = pool.begin().await.expect("begin");
    let _alice_actor = ActorScope::user(alice.id, Role::User);
    let bob_actor = ActorScope::user(bob.id, Role::User);

    let bob_key = create_client_key_tx(
        &mut tx,
        &bob_actor,
        bob.id,
        NewClientKey {
            label: "Bob Key".to_string(),
            service_account_id: None,
            allowed_models: None,
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &entropy,
        1000,
    )
    .await
    .expect("create bob key");

    // Insert dummy usage_event and trace_record for Bob's session
    sqlx::query(
        r#"INSERT INTO usage_events (
               id, ts, request_id, client_key_id, owner_user_id,
               endpoint, status, stream, incomplete, termination,
               trace_dropped, cost_micro_usd, cost_source
           )
           VALUES ('ev-bob-1', 1000, 'req-bob-1', ?, ?, 'chat', 200, 0, 0, 'completed', 0, 1000, 'provider')"#,
    )
    .bind(bob_key.client_key.id.to_string())
    .bind(bob.id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert bob event");

    let sealer = Arc::new(llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
        master_key,
        [0xEE; 16],
        Arc::new(entropy),
    ));

    let bob_job = SealJob {
        master_key_id: "key-0".to_string(),
        usage_event_id: "ev-bob-1".to_string(),
        request_id: RequestId::generate(),
        ts: 1000,
        owner_user_id: bob.id,
        client_key_id: bob_key.client_key.id,
        session_hash: "bob-secret-session".to_string(),
        thread_id: "main".to_string(),
        canonical_id: "gpt-4o".to_string(),
        protocol: "chat".to_string(),
        truncated_content: false,
        turn_trace: StoredTurnTraceV2 {
            v: 2,
            request_skeleton: serde_json::json!({ "model": "gpt-4o" }),
            block_refs: vec!["b_bob_1".to_string()],
            response: serde_json::json!({ "text": "bob answer" }),
            response_raw: None,
            truncation: None,
        },
        blocks: vec![("b_bob_1".to_string(), b"bob prompt".to_vec())],
        expires_at: 1000 + 86400 * 14 * 1000,
    };
    let sealed_bob = seal_turn_sync(&bob_job, &sealer).expect("seal bob");
    insert_sealed_trace_tx(&mut tx, &sealed_bob, "key-0")
        .await
        .expect("insert sealed bob");

    tx.commit().await.expect("commit");

    // 1. RBAC Isolation (TRC-8): Grantor Alice queries Grantee Bob's client key sessions -> HTTP 404
    let req_alice_bob_sess = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/user/client-keys/{}/sessions",
            bob_key.client_key.id
        ))
        .header(header::COOKIE, &alice_cookie)
        .header("x-csrf-token", &alice_csrf)
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req_alice_bob_sess).await.expect("resp");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "Alice cannot view Bob's sessions (TRC-8)"
    );

    // 2. Alice queries Bob's session preview directly -> HTTP 404 (TRC-8)
    let req_alice_preview = Request::builder()
        .method("GET")
        .uri("/api/user/sessions/bob-secret-session/preview")
        .header(header::COOKIE, &alice_cookie)
        .header("x-csrf-token", &alice_csrf)
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req_alice_preview).await.expect("resp");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "Alice cannot preview Bob's session (TRC-8)"
    );

    // 3. XSS Sanitization & Escaping (SEC-19)
    let malicious = "<script>alert(1)</script><img src=x onerror=alert(1)>";
    let sanitized = sanitize_assistant_text(malicious);
    assert!(!sanitized.contains("<script"));
    assert!(!sanitized.contains("alert(1)"));
    assert!(!sanitized.contains("<img"));

    let escaped = escape_html(malicious);
    assert!(!escaped.contains('<'));
    assert!(escaped.contains("&lt;script&gt;"));

    // 4. LCP timeline reconstruction (TRC-5)
    let r1 = vec!["b1".to_string(), "b2".to_string()];
    let r2 = vec!["b1".to_string(), "b2".to_string(), "b3".to_string()];
    assert_eq!(compute_lcp(&r1, &r2), 2);
}

#[doc = "Spec: TRC-5, TRC-6, TRC-7, TRC-8, SEC-19"]
#[tokio::test]
async fn test_chat_preview_timeline_reconstruction_and_trace_purge() {
    use axum::http::header;
    use llmroller_core::ids::RequestId;
    use llmroller_core::seams::Clock as _;
    use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
    use llmroller_store::traces::{
        SealJob, StoredTurnTraceV2, insert_sealed_trace_tx, seal_turn_sync,
    };

    let (dir, pool, master_key, admin_id) = setup_traceability_test_db().await;

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

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let entropy = OsEntropy;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    let charlie = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "charlie_trc",
            password: "supersecretcharliepass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("charlie");

    // Authenticate Charlie session
    let raw_charlie = "charlie_trace_session_token_123456789";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_charlie.as_bytes());
    let csrf_token = llmroller_admin::auth::derive_session_csrf(raw_charlie);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: charlie.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("charlie-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86400 * 1000,
        },
    )
    .await
    .expect("insert charlie session");
    let charlie_cookie = format!("llmroller_session={raw_charlie}");

    // Authenticate Admin session
    let raw_admin = "admin_trace_session_token_123456789";
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

    // Create client key and sealed turn for Charlie
    let charlie_actor = ActorScope::user(charlie.id, Role::User);
    let mut tx = pool.begin().await.expect("begin");
    let charlie_key = create_client_key_tx(
        &mut tx,
        &charlie_actor,
        charlie.id,
        NewClientKey {
            label: "Charlie Key".to_string(),
            service_account_id: None,
            allowed_models: None,
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("create charlie key");

    let req_id = RequestId::generate();
    let usage_ev_id = "ev-charlie-1";
    sqlx::query(
        r#"INSERT INTO usage_events (
               id, ts, request_id, client_key_id, owner_user_id,
               endpoint, status, stream, incomplete, termination,
               trace_dropped, cost_micro_usd, cost_source
           )
           VALUES (?, ?, ?, ?, ?, 'chat', 200, 0, 0, 'completed', 0, 1500, 'price_book')"#,
    )
    .bind(usage_ev_id)
    .bind(now_ms)
    .bind(req_id.to_string())
    .bind(charlie_key.client_key.id.to_string())
    .bind(charlie.id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert usage event");

    let instance_id_vec: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_one(&mut *tx)
            .await
            .expect("instance_id");
    let instance_id: [u8; 16] = instance_id_vec.try_into().expect("16 bytes");
    let sealer = Arc::new(llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
        master_key,
        instance_id,
        Arc::new(entropy),
    ));

    let charlie_job = SealJob {
        master_key_id: "key-0".to_string(),
        usage_event_id: usage_ev_id.to_string(),
        request_id: req_id,
        ts: now_ms,
        owner_user_id: charlie.id,
        client_key_id: charlie_key.client_key.id,
        session_hash: "charlie-session-hash-1".to_string(),
        thread_id: "main".to_string(),
        canonical_id: "claude-sonnet-4.6".to_string(),
        protocol: "chat".to_string(),
        truncated_content: false,
        turn_trace: StoredTurnTraceV2 {
            v: 2,
            request_skeleton: serde_json::json!({
                "system": "You are a helpful assistant.",
                "tools": []
            }),
            block_refs: vec!["blk_charlie_prompt".to_string()],
            response: serde_json::json!({
                "assistant_text": "Hello Charlie! <script>alert('xss')</script><img src=x onerror=alert(1)>",
                "assistant_reasoning": "<script>alert('reason')</script>",
                "assistant_tool_calls": [{
                    "id": "call_1",
                    "name": "<script>alert('tool')</script>",
                    "arguments": {"q": "<img src=x onerror=alert(1)>"}
                }]
            }),
            response_raw: None,
            truncation: None,
        },
        blocks: vec![(
            "blk_charlie_prompt".to_string(),
            serde_json::to_vec(&serde_json::json!({
                "role": "user",
                "content": "Hello! <script>alert('prompt')</script><img src=x onerror=alert(1)>"
            }))
            .expect("block bytes"),
        )],
        expires_at: now_ms + 86400 * 14 * 1000,
    };
    let sealed_charlie = seal_turn_sync(&charlie_job, &sealer).expect("seal charlie");
    insert_sealed_trace_tx(&mut tx, &sealed_charlie, "key-0")
        .await
        .expect("insert trace");
    tx.commit().await.expect("commit charlie");

    // 1. Charlie queries his own reconstructed chat preview timeline (TRC-5)
    let req_chat = Request::builder()
        .method("GET")
        .uri("/api/user/sessions/charlie-session-hash-1/chat")
        .header(header::COOKIE, &charlie_cookie)
        .body(Body::empty())
        .expect("req");
    let resp_chat = app.clone().oneshot(req_chat).await.expect("resp");
    assert_eq!(resp_chat.status(), StatusCode::OK);
    let chat_bytes = resp_chat
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let chat_json: serde_json::Value = serde_json::from_slice(&chat_bytes).expect("parse chat");
    assert_eq!(chat_json["session_hash"], "charlie-session-hash-1");
    assert_eq!(chat_json["total_turns"], 1);

    let timeline = chat_json["timeline"].as_array().expect("timeline array");
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0]["canonical_model"], "claude-sonnet-4.6");
    assert_eq!(
        timeline[0]["assistant_reasoning"],
        "&lt;script&gt;alert(&#39;reason&#39;)&lt;/script&gt;"
    );
    assert!(
        timeline[0]["assistant_text"]
            .as_str()
            .expect("assistant_text str")
            .contains("Hello Charlie")
    );

    let messages = timeline[0]["new_input_messages"]
        .as_array()
        .expect("messages array");
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0]["content"]
            .as_str()
            .expect("content str")
            .contains("Hello! &lt;script&gt;alert(")
    );

    // SEC-19: every preview field is inert, including prose, reasoning and tool payloads.
    let raw_chat = std::str::from_utf8(&chat_bytes).expect("utf8 preview");
    assert!(
        !raw_chat.contains("<script") && !raw_chat.contains("<img"),
        "chat preview leaked executable markup: {raw_chat}"
    );
    assert!(raw_chat.contains("&lt;script&gt;alert"));
    let tool_name = timeline[0]["assistant_tool_calls"][0]["name"]
        .as_str()
        .expect("tool name");
    assert_eq!(
        tool_name,
        "&lt;script&gt;alert(&#39;tool&#39;)&lt;/script&gt;"
    );
    let tool_arg = timeline[0]["assistant_tool_calls"][0]["arguments"]["q"]
        .as_str()
        .expect("tool argument");
    assert_eq!(tool_arg, "&lt;img src=x onerror=alert(1)&gt;");

    // 2. Single turn raw JSON drawer: GET /api/user/traces/{request_id}
    let req_single = Request::builder()
        .method("GET")
        .uri(format!("/api/user/traces/{}", req_id))
        .header(header::COOKIE, &charlie_cookie)
        .body(Body::empty())
        .expect("req");
    let resp_single = app.clone().oneshot(req_single).await.expect("resp");
    assert_eq!(resp_single.status(), StatusCode::OK);
    let single_bytes = resp_single
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let single_json: serde_json::Value =
        serde_json::from_slice(&single_bytes).expect("parse single");
    assert_eq!(single_json["v"], 2);

    // 3. Admin purges the session trace (TRC-7): DELETE /admin/api/sessions/{session_hash}/trace
    let req_purge = Request::builder()
        .method("DELETE")
        .uri("/admin/api/sessions/charlie-session-hash-1/trace")
        .header(header::COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .body(Body::empty())
        .expect("req");
    let resp_purge = app.clone().oneshot(req_purge).await.expect("resp");
    assert_eq!(resp_purge.status(), StatusCode::NO_CONTENT);

    // 4. After purge, querying chat preview returns 404 (session trace gone)
    let req_chat_after = Request::builder()
        .method("GET")
        .uri("/api/user/sessions/charlie-session-hash-1/chat")
        .header(header::COOKIE, &charlie_cookie)
        .body(Body::empty())
        .expect("req");
    let resp_chat_after = app.oneshot(req_chat_after).await.expect("resp");
    assert_eq!(resp_chat_after.status(), StatusCode::NOT_FOUND);
}
