//! End-to-end tests for /v1/* orchestrator v1, pre-flight body budget admission, and graceful drain.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#53-orchestration-and-request-flow-state-machine
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#553-ordered-graceful-shutdown-sigterm--sigint-and-job-supervision
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-performance-and-resource-efficiency-perf
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#72-reliability-and-availability-rel
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#27-sub-phase-27-v1-orchestrator-v1-pre-flight-body-memory-budget-graceful-sigterm-drain-and-early-load-harness

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{ExposeSecret, OsEntropy, secret_string};
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_proxy::orchestrator::PreflightBudgetSemaphore;
use llmroller_server::reloader::load_initial_snapshot;
use llmroller_store::client_keys::{NewClientKey, create_client_key};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider, create_upstream_key,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};
use tempfile::tempdir;
use tower::ServiceExt as _;

async fn setup_test_context() -> (tempfile::TempDir, sqlx::SqlitePool, String, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("llmroller.db");

    let pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x88; 32]);
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance init");

    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "orch_user",
            password: "supersecretuserpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("create user");

    let user_actor = ActorScope::user(user.id, Role::User);

    // Create personal provider
    let provider = create_provider(
        &pool,
        &user_actor,
        NewProvider {
            name: "orch_provider".to_string(),
            kind: llmroller_core::provider::ProviderKind::Custom,
            base_url: "https://api.openai.com/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(user.id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1,
    )
    .await
    .expect("create provider");

    // Create upstream key
    create_upstream_key(
        &pool,
        &user_actor,
        &master_key,
        "key-0",
        Arc::new(entropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "key_primary".to_string(),
            secret: secret_string("sk-upstream-real-key"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1,
    )
    .await
    .expect("create upstream key");

    // Create client key
    let ck_res = create_client_key(
        &pool,
        &user_actor,
        user.id,
        NewClientKey {
            label: "client_agent".to_string(),
            routing_scope: Some("personal_first".to_string()),
            allowed_models: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: None,
            expires_at: None,
        },
        &entropy,
        1,
    )
    .await
    .expect("create client key");

    let raw_token = ck_res.raw_token.expose_secret().to_string();
    (dir, pool, raw_token, user.id)
}

/// Spec: REL-1, SEC-8, Partial: PERF-1, Partial: PERF-2, Partial: PERF-3
/// 1. Single-provider pass-through across /v1/chat/completions, /v1/responses, /v1/messages.
/// 2. Pre-flight body budget semaphore queues excess requests up to 2,000 ms and returns 503 on timeout.
#[doc = "Spec: REL-1, SEC-8, Partial: PERF-1, Partial: PERF-2, Partial: PERF-3"]
#[tokio::test]
async fn test_orchestrator_passthrough_and_body_budget_admission() {
    let (_dir, pool, client_token, _user_id) = setup_test_context().await;

    let snapshot = load_initial_snapshot(&pool).await.expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::new(Arc::new(snapshot)));
    let auth_state = ProxyAuthState::new(shared_snapshot.clone(), pool.clone(), vec![]);
    auth_state
        .is_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let app = data_plane_router(auth_state.clone());

    // 1. /v1/chat/completions pass-through
    let chat_req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false
            })
            .to_string(),
        ))
        .expect("chat req");
    let chat_res = app.clone().oneshot(chat_req).await.expect("chat res");
    // Expect 200 OK or handled upstream response
    assert!(chat_res.status().is_success() || chat_res.status() == StatusCode::OK);

    // 2. /v1/responses pass-through
    let resp_req = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "model": "gpt-4o",
                "input": "test prompt"
            })
            .to_string(),
        ))
        .expect("resp req");
    let resp_res = app.clone().oneshot(resp_req).await.expect("resp res");
    assert!(resp_res.status().is_success() || resp_res.status() == StatusCode::OK);

    // 3. /v1/messages pass-through
    let msg_req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "model": "claude-sonnet-4.6",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        ))
        .expect("msg req");
    let msg_res = app.clone().oneshot(msg_req).await.expect("msg res");
    assert!(msg_res.status().is_success() || msg_res.status() == StatusCode::OK);

    // 4. Pre-flight body budget semaphore queuing and 503 gateway_busy on timeout (SEC-8)
    let tiny_budget = PreflightBudgetSemaphore::new(1, 32); // only 1 MB budget, 256 permits of 4096 bytes
    // Acquire all permits to simulate exhaustion
    let _held = tiny_budget
        .semaphore
        .acquire_many(256)
        .await
        .expect("acquire all");

    // Trying to acquire another permit with 50 ms timeout must fail with 503 gateway_busy
    let err = tiny_budget
        .acquire(4096, Duration::from_millis(50))
        .await
        .expect_err("must timeout when budget exhausted");
    assert_eq!(err.status_code(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.retry_after_secs(), 1);
}
