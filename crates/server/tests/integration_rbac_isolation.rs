//! Integration tests for RBAC tenant isolation, project_key redaction, and share consent.
//!
//! Spec: Section 4.2, Section 6.2..6.5, Section 12.6.2, Section 14.2.1, Section 14.3,
//! Requirements: USR-2, USR-3, USR-4, UI-1, UI-2.

use std::sync::Arc;
use tempfile::tempdir;

use llmroller_admin::projection::{KeyProjectionInput, project_key};
use llmroller_core::ids::{UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{OsEntropy, secret_string};
use llmroller_store::client_keys::{
    ClientKeyError, NewClientKey, create_client_key, get_client_key,
};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, ProviderError, ProviderPatch, create_provider,
    create_upstream_key, delete_provider, get_provider, get_upstream_key, patch_provider,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

async fn setup_rbac_test_db() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    MasterKey,
    UserId,
    UserId,
) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("rbac_isolation_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xAB; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let alice = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "alice_rbac",
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
        &writer_pool,
        &system_actor,
        NewUser {
            username: "bob_rbac",
            password: "supersecretbobpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("bob");

    (dir, writer_pool, master_key, alice.id, bob.id)
}

#[doc = "Spec: USR-2, USR-3, USR-4, UI-1, UI-2"]
#[tokio::test]
async fn test_rbac_isolation_and_project_key_redaction() {
    let (_dir, _pool, _master_key, alice_id, bob_id) = setup_rbac_test_db().await;

    let alice_actor = ActorScope::user(alice_id, Role::User);
    let bob_actor = ActorScope::user(bob_id, Role::User);
    let admin_actor = ActorScope::user(UserId::generate(), Role::Admin);

    let key_id = UpstreamKeyId::generate();
    let global_key_id = UpstreamKeyId::generate();

    // 1. project_key for Owner Alice: sees full key ID, priority, and last4
    let alice_view = project_key(
        &alice_actor,
        &KeyProjectionInput {
            key_id: &key_id,
            key_label: "Alice Personal Key",
            provider_name: "Custom Provider",
            ownership: OwnershipScope::Personal,
            owner_user_id: Some(&alice_id),
            priority: Some(100),
            secret_last4: Some("9999"),
        },
    );
    assert_eq!(alice_view.id, Some(key_id.to_string()));
    assert_eq!(alice_view.label, "Alice Personal Key");
    assert_eq!(alice_view.priority, Some(100));
    assert_eq!(alice_view.secret_last4, Some("9999".to_string()));
    assert!(!alice_view.is_global_pool);

    // 2. project_key for Bob viewing Alice's key: redacted! (USR-4)
    let bob_view = project_key(
        &bob_actor,
        &KeyProjectionInput {
            key_id: &key_id,
            key_label: "Alice Personal Key",
            provider_name: "Custom Provider",
            ownership: OwnershipScope::Personal,
            owner_user_id: Some(&alice_id),
            priority: Some(100),
            secret_last4: Some("9999"),
        },
    );
    assert_eq!(bob_view.id, None);
    assert_eq!(bob_view.label, "global pool");
    assert_eq!(bob_view.priority, None);
    assert_eq!(bob_view.secret_last4, None);
    assert!(bob_view.is_global_pool);

    // 3. project_key for Bob viewing a Global key: redacted to "global pool" (USR-4)
    let bob_global_view = project_key(
        &bob_actor,
        &KeyProjectionInput {
            key_id: &global_key_id,
            key_label: "Internal Production Global Key",
            provider_name: "OpenRouter",
            ownership: OwnershipScope::Global,
            owner_user_id: None,
            priority: Some(50),
            secret_last4: Some("1234"),
        },
    );
    assert_eq!(bob_global_view.id, None);
    assert_eq!(bob_global_view.label, "global pool");
    assert!(bob_global_view.is_global_pool);

    // 4. Admin viewing the same Global key sees full details (USR-2, USR-4)
    let admin_view = project_key(
        &admin_actor,
        &KeyProjectionInput {
            key_id: &global_key_id,
            key_label: "Internal Production Global Key",
            provider_name: "OpenRouter",
            ownership: OwnershipScope::Global,
            owner_user_id: None,
            priority: Some(50),
            secret_last4: Some("1234"),
        },
    );
    assert_eq!(admin_view.id, Some(global_key_id.to_string()));
    assert_eq!(admin_view.label, "Internal Production Global Key");
    assert_eq!(admin_view.priority, Some(50));
    assert_eq!(admin_view.secret_last4, Some("1234".to_string()));
}

#[doc = "Spec: USR-3, SEC-17"]
#[tokio::test]
async fn test_cross_tenant_existence_oracle_and_mutation_guards() {
    let (_dir, writer_pool, master_key, alice_id, bob_id) = setup_rbac_test_db().await;
    let entropy = OsEntropy;

    let alice_actor = ActorScope::user(alice_id, Role::User);
    let bob_actor = ActorScope::user(bob_id, Role::User);
    let admin_actor = ActorScope::user(UserId::generate(), Role::Admin);

    // 1. Alice creates personal provider, upstream key, and client key
    let alice_provider = create_provider(
        &writer_pool,
        &alice_actor,
        NewProvider {
            name: "alice_personal".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(alice_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        100,
    )
    .await
    .expect("alice provider");

    let alice_key = create_upstream_key(
        &writer_pool,
        &alice_actor,
        &master_key,
        "key-0",
        Arc::new(entropy),
        NewUpstreamKey {
            provider_id: alice_provider.id,
            label: "alice_key".to_string(),
            secret: secret_string("sk-alice-secret-1234"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        100,
    )
    .await
    .expect("alice upstream key");

    let alice_client_key_res = create_client_key(
        &writer_pool,
        &alice_actor,
        alice_id,
        NewClientKey {
            label: "alice_client_key".to_string(),
            service_account_id: None,
            routing_scope: None,
            allowed_models: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: None,
            expires_at: None,
        },
        &entropy,
        100,
    )
    .await
    .expect("alice client key");
    let alice_client_key = alice_client_key_res.client_key;

    // 2. Admin creates global provider
    let global_provider = create_provider(
        &writer_pool,
        &admin_actor,
        NewProvider {
            name: "global_openrouter".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        100,
    )
    .await
    .expect("global provider");

    // 3. Bob attempts to read Alice's personal resources: must return NotFound (USR-3, SEC-17)
    let err_provider = get_provider(&writer_pool, &bob_actor, alice_provider.id)
        .await
        .expect_err("bob cannot read alice provider");
    assert!(
        matches!(err_provider, ProviderError::ProviderNotFound(id) if id == alice_provider.id),
        "Bob must receive ProviderNotFound to prevent existence oracles, got: {err_provider:?}"
    );

    let err_key = get_upstream_key(&writer_pool, &bob_actor, alice_key.id)
        .await
        .expect_err("bob cannot read alice key");
    assert!(
        matches!(err_key, ProviderError::UpstreamKeyNotFound(id) if id == alice_key.id),
        "Bob must receive UpstreamKeyNotFound to prevent existence oracles, got: {err_key:?}"
    );

    let err_client_key = get_client_key(&writer_pool, &bob_actor, alice_client_key.id)
        .await
        .expect_err("bob cannot read alice client key");
    assert!(
        matches!(err_client_key, ClientKeyError::NotFound(id) if id == alice_client_key.id),
        "Bob must receive ClientKeyError::NotFound to prevent existence oracles, got: {err_client_key:?}"
    );

    // 4. Bob attempts to modify Global provider: Bob can read it, but modifying returns Forbidden (USR-3)
    let patch = ProviderPatch {
        name: Some("hacked_name".to_string()),
        base_url: None,
        config_json: None,
        usage_url_cross_origin_ack: None,
        allow_private_network: None,
        enabled: None,
    };
    let err_patch_global = patch_provider(
        &writer_pool,
        &bob_actor,
        global_provider.id,
        patch.clone(),
        100,
    )
    .await
    .expect_err("bob cannot patch global provider");
    assert!(
        matches!(err_patch_global, ProviderError::Forbidden),
        "Bob must receive Forbidden when trying to modify readable global provider, got: {err_patch_global:?}"
    );

    let err_delete_global = delete_provider(&writer_pool, &bob_actor, global_provider.id)
        .await
        .expect_err("bob cannot delete global provider");
    assert!(
        matches!(err_delete_global, ProviderError::Forbidden),
        "Bob must receive Forbidden when trying to delete readable global provider, got: {err_delete_global:?}"
    );

    let err_create_key_global = create_upstream_key(
        &writer_pool,
        &bob_actor,
        &master_key,
        "key-0",
        Arc::new(entropy),
        NewUpstreamKey {
            provider_id: global_provider.id,
            label: "bob_global_key".to_string(),
            secret: secret_string("sk-bob-secret-1234"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        100,
    )
    .await
    .expect_err("bob cannot create key under global provider");
    assert!(
        matches!(err_create_key_global, ProviderError::Forbidden),
        "Bob must receive Forbidden when creating key under global provider, got: {err_create_key_global:?}"
    );

    // 5. Bob attempts to modify Alice's personal provider: returns NotFound (USR-3)
    let err_patch_alice = patch_provider(&writer_pool, &bob_actor, alice_provider.id, patch, 100)
        .await
        .expect_err("bob cannot patch alice provider");
    assert!(
        matches!(err_patch_alice, ProviderError::ProviderNotFound(id) if id == alice_provider.id),
        "Bob must receive ProviderNotFound when patching unowned personal provider, got: {err_patch_alice:?}"
    );

    let err_delete_alice = delete_provider(&writer_pool, &bob_actor, alice_provider.id)
        .await
        .expect_err("bob cannot delete alice provider");
    assert!(
        matches!(err_delete_alice, ProviderError::ProviderNotFound(id) if id == alice_provider.id),
        "Bob must receive ProviderNotFound when deleting unowned personal provider, got: {err_delete_alice:?}"
    );
}

#[doc = "Spec: RTE-4, RTE-5, RTE-6, USR-4, AUTH-4, AUTH-6, PRV-6, UI-1"]
#[tokio::test]
async fn test_user_routing_matrix_and_self_service_security() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt as _;
    use llmroller_admin::auth::{AuthConfig, derive_session_csrf};
    use llmroller_admin::setup::SetupAppState;
    use llmroller_admin::{ApiAppState, AuthAppState, control_plane_router_full};
    use llmroller_store::crypto::sha256_hex;
    use llmroller_store::models::upsert_discovered_models_tx;
    use tower::ServiceExt as _;

    let (dir, pool, master_key, alice_id, _bob_id) = setup_rbac_test_db().await;

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
    let api_state = ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    let alice_actor = ActorScope::user(alice_id, Role::User);
    let admin_actor = ActorScope::user(UserId::generate(), Role::Admin);
    let entropy = OsEntropy;

    // 1. Setup Alice's personal provider and key
    let alice_prov = create_provider(
        &pool,
        &alice_actor,
        NewProvider {
            name: "alice_personal".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(alice_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        100,
    )
    .await
    .expect("alice provider");

    let alice_key = create_upstream_key(
        &pool,
        &alice_actor,
        &master_key,
        "key-0",
        Arc::new(entropy),
        NewUpstreamKey {
            provider_id: alice_prov.id,
            label: "alice_key".to_string(),
            secret: secret_string("sk-alice-test"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        100,
    )
    .await
    .expect("alice key");

    // 2. Setup Global provider and key
    let global_prov = create_provider(
        &pool,
        &admin_actor,
        NewProvider {
            name: "global_pool".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        100,
    )
    .await
    .expect("global prov");

    let global_key = create_upstream_key(
        &pool,
        &admin_actor,
        &master_key,
        "key-0",
        Arc::new(entropy),
        NewUpstreamKey {
            provider_id: global_prov.id,
            label: "global_secret_key".to_string(),
            secret: secret_string("sk-global-test"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        100,
    )
    .await
    .expect("global key");

    use llmroller_core::seams::Clock as _;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    // Discover a canonical model for both keys
    let mut tx = pool.begin().await.expect("tx");
    upsert_discovered_models_tx(
        &mut tx,
        &alice_key.id,
        false,
        Some(&alice_id),
        now_ms,
        &["claude-sonnet-4.6".to_string()],
        100,
    )
    .await
    .expect("discover alice");
    upsert_discovered_models_tx(
        &mut tx,
        &global_key.id,
        true,
        None,
        now_ms,
        &["claude-sonnet-4.6".to_string()],
        100,
    )
    .await
    .expect("discover global");
    tx.commit().await.expect("commit discover");

    // 3. Establish Alice's authenticated session
    let raw_session = "alice_test_session_token_1234567890123456";
    let id_hash = sha256_hex(raw_session.as_bytes());
    let csrf_token = derive_session_csrf(raw_session);
    let csrf_hash = sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: alice_id,
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

    // 4. GET /api/user/routes: Alice views the routing matrix
    let routes_req = Request::builder()
        .method("GET")
        .uri("/api/user/routes")
        .header(header::COOKIE, &alice_cookie)
        .body(Body::empty())
        .expect("req");
    let routes_res = app.clone().oneshot(routes_req).await.expect("res");
    assert_eq!(routes_res.status(), StatusCode::OK);
    let routes_body = routes_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let routes_json: serde_json::Value = serde_json::from_slice(&routes_body).expect("parse json");
    assert!(routes_json["models"].is_array());
    let models_arr = routes_json["models"].as_array().expect("models array");
    assert!(!models_arr.is_empty());
    // Invariant USR-4: Global keys are collapsed into "Global pool", no global key ID/label exposed
    let json_str = routes_json.to_string();
    assert!(
        !json_str.contains("global_secret_key"),
        "Global key label must be redacted to standard user (USR-4)"
    );

    // 5. PUT /api/user/routes/{alice_key.id}/{model}: Updating Alice's own key route preference succeeds
    let put_own_route_req = Request::builder()
        .method("PUT")
        .uri(format!(
            "/api/user/routes/{}/claude-sonnet-4.6",
            alice_key.id
        ))
        .header(header::COOKIE, &alice_cookie)
        .header("x-csrf-token", &csrf_token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "priority_override": 250,
                "enabled": true
            })
            .to_string(),
        ))
        .expect("req");
    let put_own_res = app.clone().oneshot(put_own_route_req).await.expect("res");
    assert_eq!(put_own_res.status(), StatusCode::OK);

    // 6. PUT /api/user/routes/{global_key.id}/{model}: Modifying global route preference fails with 403 Forbidden (RTE-5)
    let put_global_route_req = Request::builder()
        .method("PUT")
        .uri(format!(
            "/api/user/routes/{}/claude-sonnet-4.6",
            global_key.id
        ))
        .header(header::COOKIE, &alice_cookie)
        .header("x-csrf-token", &csrf_token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "priority_override": 500,
                "enabled": true
            })
            .to_string(),
        ))
        .expect("req");
    let put_global_res = app
        .clone()
        .oneshot(put_global_route_req)
        .await
        .expect("res");
    assert_eq!(
        put_global_res.status(),
        StatusCode::FORBIDDEN,
        "Standard user cannot set route preference on global key (RTE-5)"
    );

    // 7. Self-Service: POST /api/user/password (AUTH-4)
    let pw_req = Request::builder()
        .method("POST")
        .uri("/api/user/password")
        .header(header::COOKIE, &alice_cookie)
        .header("x-csrf-token", &csrf_token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "current_password": "supersecretalicepass123",
                "new_password": "newsupersecretalicepass456"
            })
            .to_string(),
        ))
        .expect("req");
    let pw_res = app.clone().oneshot(pw_req).await.expect("res");
    assert_eq!(pw_res.status(), StatusCode::OK);

    // 8. Self-Service: POST /api/user/totp/enroll (AUTH-6)
    let totp_req = Request::builder()
        .method("POST")
        .uri("/api/user/totp/enroll")
        .header(header::COOKIE, &alice_cookie)
        .header("x-csrf-token", &csrf_token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .expect("req");
    let totp_res = app.clone().oneshot(totp_req).await.expect("res");
    assert_eq!(totp_res.status(), StatusCode::OK);
    let totp_body = totp_res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let totp_json: serde_json::Value = serde_json::from_slice(&totp_body).expect("totp json");
    assert!(totp_json["otpauth_url"].as_str().is_some());
    assert!(totp_json["recovery_codes"].as_array().is_some());

    // 9. Key testing: POST /api/user/upstream-keys/{id}/test (PRV-6)
    let test_key_req = Request::builder()
        .method("POST")
        .uri(format!("/api/user/upstream-keys/{}/test", alice_key.id))
        .header(header::COOKIE, &alice_cookie)
        .header("x-csrf-token", &csrf_token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .expect("req");
    let test_key_res = app.oneshot(test_key_req).await.expect("res");
    assert_eq!(test_key_res.status(), StatusCode::OK);
}
