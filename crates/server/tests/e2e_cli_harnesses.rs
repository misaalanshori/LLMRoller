//! Headless CLI harness compatibility, SDK integration, and privacy egress sandbox checks.
//!
//! Spec: Section 7.3, Section 7.4, Section 16.5.5, Section 19.2,
//! Requirements: COM-1, PRIV-1, OBS-1.

use std::sync::Arc;
use tempfile::tempdir;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;

use llmroller_core::ids::{ModelPk, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_proxy::metrics::{CLOSED_LABEL_ALLOWLIST, validate_metric_labels};
use llmroller_server::reloader::load_initial_snapshot;
use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pins::{ResponseLinkRow, insert_response_link_tx};
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

async fn setup_harness_test_db() -> (tempfile::TempDir, sqlx::SqlitePool, MasterKey, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("harness_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x77; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "harness_user",
            password: "supersecretpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("user");

    (dir, writer_pool, master_key, user.id)
}

#[doc = "Spec: COM-1, PRIV-1, OBS-1"]
#[tokio::test]
async fn test_headless_cli_harnesses_and_egress_sandbox() {
    let (_dir, pool, master_key, user_id) = setup_harness_test_db().await;
    let mock_app = axum::Router::new()
        .route(
            "/chat/completions",
            axum::routing::post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"id":"chatcmpl-1","choices":[{"message":{"role":"assistant","content":"mock answer"}}],"usage":{"prompt_tokens":10,"completion_tokens":10,"total_tokens":20}}"#,
                )
            }),
        )
        .route(
            "/responses",
            axum::routing::post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"id":"resp-1","object":"response","output":"mock output"}"#,
                )
            }),
        )
        .route(
            "/messages",
            axum::routing::post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"id":"msg-1","type":"message","content":[{"type":"text","text":"mock message"}]}"#,
                )
            }),
        )
        .route(
            "/messages/count_tokens",
            axum::routing::post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"input_tokens": 10}"#,
                )
            }),
        );
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let mock_addr = mock_listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_app).await;
    });

    let mut tx = pool.begin().await.expect("begin");

    let actor = ActorScope::user(user_id, Role::Admin);

    let provider = create_provider_tx(
        &mut tx,
        &actor,
        NewProvider {
            name: "Harness Provider".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: format!("http://{}", mock_addr),
            config_json: "{}".to_string(),
            ownership_scope: llmroller_core::rbac::OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: true,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("provider");

    let key = create_upstream_key_tx(
        &mut tx,
        &actor,
        &master_key,
        "sk-harness",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Harness Key".to_string(),
            secret: llmroller_core::seams::secret_string("sk-harness"),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("key");

    let created_client_key = create_client_key_tx(
        &mut tx,
        &actor,
        user_id,
        NewClientKey {
            label: "CLI Key".to_string(),
            allowed_models: None,
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &OsEntropy,
        1000,
    )
    .await
    .expect("client key");

    use llmroller_core::seams::ExposeSecret as _;
    let token = created_client_key.raw_token.expose_secret().to_string();

    let model_pk = ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id, origin) VALUES (?, 'claude-3-5-sonnet', 'global')")
        .bind(model_pk.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert model");

    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, 'claude-3-5-sonnet', '[\"chat\",\"responses\",\"messages\"]', 'active', 1, 1000)",
    )
    .bind(key.id.to_string())
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model route");

    let link = ResponseLinkRow {
        response_id: "resp_codex_prev_1".to_string(),
        owner_user_id: user_id,
        client_key_id: created_client_key.client_key.id,
        upstream_key_id: key.id,
        model_pk,
        session_hash: "codex-session-chain".to_string(),
        status: "completed".to_string(),
        store_flag: true,
        created_at: 1000,
        updated_at: 1000,
        expires_at: 1000 + 86400,
    };
    insert_response_link_tx(&mut tx, &link)
        .await
        .expect("insert link");

    tx.commit().await.expect("commit");

    let snapshot = load_initial_snapshot(&pool).await.expect("snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let auth_state =
        ProxyAuthState::new(shared_snapshot, pool.clone(), vec![]).with_master_key(master_key);
    auth_state
        .is_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let app = data_plane_router(auth_state);

    // 1. Claude Code CLI compatibility: session header + subagent + count_tokens (COM-1)
    let claude_req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("x-claude-code-session-id", "claude-sess-001")
        .header("x-claude-code-agent-id", "agent-sub-1")
        .header("content-type", "application/json")
        .body(Body::from(include_str!(
            "../../../fixtures/harnesses/claude_code.json"
        )))
        .expect("req");
    let claude_res = app.clone().oneshot(claude_req).await.expect("res");
    assert_eq!(claude_res.status(), StatusCode::OK);

    let count_req = Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model":"claude-3-5-sonnet","messages":[{"role":"user","content":"count me"}]}"#,
        ))
        .expect("req");
    let count_res = app.clone().oneshot(count_req).await.expect("res");
    assert_eq!(count_res.status(), StatusCode::OK);

    // 2. Codex CLI Responses chaining with previous_response_id (COM-1)
    let codex_req = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(include_str!(
            "../../../fixtures/harnesses/codex_cli.json"
        )))
        .expect("req");
    let codex_res = app.clone().oneshot(codex_req).await.expect("res");
    assert_eq!(codex_res.status(), StatusCode::OK);

    // 3. OpenCode CLI compatibility with x-opencode-session (COM-1)
    let opencode_req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("x-opencode-session", "opencode-sess-100")
        .header("content-type", "application/json")
        .body(Body::from(include_str!(
            "../../../fixtures/harnesses/opencode.json"
        )))
        .expect("req");
    let opencode_res = app.oneshot(opencode_req).await.expect("res");
    assert_eq!(opencode_res.status(), StatusCode::OK);

    // 4. Closed 10-label observability metric validation (OBS-1)
    let labels = [("status_class", "2xx"), ("endpoint", "chat")];
    assert!(validate_metric_labels(&labels).is_ok());

    let invalid_labels = [("forbidden_user_id", "secret")];
    assert!(validate_metric_labels(&invalid_labels).is_err());
    assert_eq!(CLOSED_LABEL_ALLOWLIST.len(), 10);

    // 5. Query /metrics and verify real Prometheus metrics are rendered with closed allowlist labels (OBS-1)
    use http_body_util::BodyExt as _;
    use llmroller_proxy::listeners::metrics_router;
    let metrics_app = metrics_router();
    let metrics_req = Request::builder()
        .method("GET")
        .uri("/metrics")
        .body(Body::empty())
        .expect("metrics req");
    let metrics_res = metrics_app.oneshot(metrics_req).await.expect("metrics res");
    assert_eq!(metrics_res.status(), StatusCode::OK);
    let body_bytes = metrics_res
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let metrics_text = String::from_utf8(body_bytes.to_vec()).expect("utf-8");
    assert!(
        metrics_text.contains("llmroller_requests_total"),
        "metrics must contain llmroller_requests_total, got: {metrics_text}"
    );
    assert!(
        metrics_text.contains("endpoint="),
        "metrics must include endpoint label, got: {metrics_text}"
    );

    // 6. Network sandbox and zero-telemetry egress check (PRIV-1)
    // Assert that outbound proxy connections reject unconfigured private addresses,
    // AWS metadata endpoints, and non-HTTPS external hosts without allow_private_network
    use llmroller_proxy::ssrf::{is_ip_blocked, validate_url};
    use std::net::IpAddr;

    // Zero telemetry: AWS metadata (169.254.169.254) is blocked
    let aws_metadata: IpAddr = "169.254.169.254".parse().expect("parsed");
    assert!(
        is_ip_blocked(aws_metadata, false),
        "metadata service must be blocked (PRIV-1)"
    );

    // Loopback (127.0.0.1) is blocked for outbound egress
    let loopback: IpAddr = "127.0.0.1".parse().expect("parsed");
    assert!(
        is_ip_blocked(loopback, false),
        "loopback egress must be blocked (PRIV-1)"
    );

    // Non-configured plain HTTP URLs are rejected at validation time
    assert!(
        validate_url("http://telemetry.example.com", false).is_err(),
        "plain HTTP telemetry egress must be rejected (PRIV-1)"
    );
    // Configured HTTPS provider URL is allowed
    assert!(
        validate_url("https://openrouter.ai/api/v1", false).is_ok(),
        "configured provider origin must be permitted"
    );
}
