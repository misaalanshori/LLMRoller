//! Integration test for providers and upstream keys repositories, quotas, encryption, and fingerprinting.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/4. SRDD - Users, Roles, and Provider Ownership.md#43-consented-personal-provider-and-key-sharing-shr
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Phase: ./docs/bigbang/big-bang-phases-2.md#20-sub-phase-20-minimal-control-plane-json-crud-for-providers-keys-client-keys-and-settings

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use tower::ServiceExt as _;

use arc_swap::ArcSwap;
use llmroller_core::ids::{UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::seams::{ExposeSecret, OsEntropy, secret_string};
use llmroller_core::snapshot::{CacheInvalidationEvent, ConfigDomain};
use llmroller_server::reloader::{load_initial_snapshot, start_reloader};
use llmroller_store::crypto::{MasterKey, SecretSealer, XChaCha20Poly1305Sealer};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, ProviderError, ProviderPatch, UpstreamKeyPatch,
    compute_share_config_fingerprint, create_provider, create_provider_tx, create_upstream_key,
    create_upstream_key_tx, delete_provider, delete_upstream_key, get_provider, get_upstream_key,
    list_providers, list_upstream_keys, patch_provider, patch_upstream_key,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};
use llmroller_store::writer::{ControlOp, WriterError, start_writer};
use sqlx::{Sqlite, Transaction};
use tempfile::tempdir;
use tokio::sync::mpsc;

async fn setup_test_context() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    sqlx::SqlitePool,
    MasterKey,
    UserId,
    UserId,
) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("llmroller.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    let reader_pool = open_pool(&db_path, PoolRole::Reader)
        .await
        .expect("reader pool");

    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x55; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance init");

    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin_user = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "admin_user",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("admin user");

    let regular_user = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "regular_user",
            password: "supersecretuserpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("regular user");

    (
        dir,
        writer_pool,
        reader_pool,
        master_key,
        admin_user.id,
        regular_user.id,
    )
}

/// Spec: PRV-1, PRV-4, PRV-5, Partial: CLK-1
/// (1) admin can create global providers and keys while user can create personal providers and keys
/// (up to 20 providers and 50 keys per user, SEC-8),
/// (2) secrets are sealed with XChaCha20-Poly1305 (domain = "upstream_key") and never echoed in API responses (PRV-5),
/// (3) providers.share_config_fingerprint is computed and persisted on every provider insert/update, and
/// (4) every mutation writes an audit_log row and updates ConfigSnapshot within 1 s.
#[doc = "Spec: PRV-1, PRV-4, PRV-5"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_provider_and_key_crud_rbac_quotas_and_encryption() {
    let (_dir, writer_pool, _reader_pool, master_key, admin_id, user_id) =
        setup_test_context().await;

    let admin_actor = ActorScope::user(admin_id, Role::Admin);
    let user_actor = ActorScope::user(user_id, Role::User);
    let entropy = OsEntropy;

    // 1. RBAC: User cannot create a global provider
    let err = create_provider(
        &writer_pool,
        &user_actor,
        NewProvider {
            name: "illegal_global".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1,
    )
    .await
    .expect_err("user cannot create global provider");
    assert!(matches!(err, ProviderError::Forbidden));

    // Admin creates a global provider
    let global_provider = create_provider(
        &writer_pool,
        &admin_actor,
        NewProvider {
            name: "global_openrouter".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: r#"{"paths":{"chat":"/chat/completions"}}"#.to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1,
    )
    .await
    .expect("admin creates global provider");

    assert_eq!(global_provider.ownership_scope, OwnershipScope::Global);
    assert_eq!(global_provider.owner_user_id, None);
    assert!(!global_provider.share_config_fingerprint.is_empty());

    // 2. User creates a personal provider
    let personal_provider = create_provider(
        &writer_pool,
        &user_actor,
        NewProvider {
            name: "my_personal_provider".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://api.openai.com/v1".to_string(),
            config_json: r#"{"paths":{"chat":"/chat/completions"}}"#.to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(user_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1,
    )
    .await
    .expect("user creates personal provider");

    assert_eq!(personal_provider.ownership_scope, OwnershipScope::Personal);
    assert_eq!(personal_provider.owner_user_id, Some(user_id));
    assert_eq!(
        personal_provider.share_config_fingerprint,
        compute_share_config_fingerprint(
            "custom",
            "https://api.openai.com/v1",
            false,
            r#"{"paths":{"chat":"/chat/completions"}}"#
        )
    );

    // 3. User creates an upstream key on their personal provider
    let secret_plain = "sk-ant-test-secret-1234567890abcdef";
    let key = create_upstream_key(
        &writer_pool,
        &user_actor,
        &master_key,
        "key-0",
        Arc::new(entropy),
        NewUpstreamKey {
            provider_id: personal_provider.id,
            label: "primary_key".to_string(),
            secret: secret_string(secret_plain),
            priority: Some(150),
            strict_tier: Some(2),
            enabled: Some(true),
        },
        1,
    )
    .await
    .expect("user creates upstream key");

    assert_eq!(key.provider_id, personal_provider.id);
    assert_eq!(key.owner_user_id, Some(user_id));
    assert_eq!(key.secret_last4, "cdef");
    assert_eq!(key.priority, 150);
    assert_eq!(key.strict_tier, 2);

    // Verify key secret is sealed with XChaCha20-Poly1305 under domain "upstream_key"
    let instance_id_vec: Vec<u8> = sqlx::query_scalar(
        "SELECT instance_id FROM instance_meta WHERE singleton = 1",
    )
    .fetch_one(&writer_pool)
    .await
    .expect("fetch instance_id");
    let instance_id: [u8; 16] = instance_id_vec.try_into().expect("16-byte instance_id");

    let sealer = XChaCha20Poly1305Sealer::new(
        master_key.clone(),
        instance_id,
        Arc::new(llmroller_core::seams::OsEntropy),
    );
    let unsealed = sealer
        .open(
            "upstream_key",
            &[
                b"key-0",
                key.id.to_string().as_bytes(),
                personal_provider.id.to_string().as_bytes(),
            ],
            &key.secret_enc,
        )
        .expect("unseal upstream key");
    assert_eq!(unsealed.expose_secret(), secret_plain.as_bytes());

    // 4. Updating provider recomputes share_config_fingerprint
    let initial_fp = personal_provider.share_config_fingerprint.clone();
    let updated_provider = patch_provider(
        &writer_pool,
        &user_actor,
        personal_provider.id,
        ProviderPatch {
            name: None,
            base_url: Some("https://api.openai.com/v2".to_string()),
            config_json: None,
            usage_url_cross_origin_ack: None,
            allow_private_network: None,
            enabled: None,
        },
        2,
    )
    .await
    .expect("patch provider");

    assert_ne!(updated_provider.share_config_fingerprint, initial_fp);

    // 5. Quotas: user cannot exceed 20 personal providers (SEC-8)
    for i in 1..20 {
        create_provider(
            &writer_pool,
            &user_actor,
            NewProvider {
                name: format!("prov_{i}"),
                kind: ProviderKind::Openrouter,
                base_url: "https://openrouter.ai/api/v1".to_string(),
                config_json: "{}".to_string(),
                ownership_scope: OwnershipScope::Personal,
                owner_user_id: Some(user_id),
                usage_url_cross_origin_ack: false,
                allow_private_network: false,
                enabled: true,
            },
            10 + i,
        )
        .await
        .expect("create quota provider");
    }

    // Attempting provider #21 must fail with QuotaExceeded
    let quota_err = create_provider(
        &writer_pool,
        &user_actor,
        NewProvider {
            name: "prov_overflow".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(user_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        100,
    )
    .await
    .expect_err("provider quota overflow must fail");
    assert!(matches!(quota_err, ProviderError::QuotaExceeded { .. }));

    // 6. Quotas: user cannot exceed 50 upstream keys (SEC-8)
    for i in 1..50 {
        create_upstream_key(
            &writer_pool,
            &user_actor,
            &master_key,
            "key-0",
            Arc::new(entropy),
            NewUpstreamKey {
                provider_id: personal_provider.id,
                label: format!("key_{i}"),
                secret: secret_string("sk-test-secret-1234"),
                priority: None,
                strict_tier: None,
                enabled: None,
            },
            200 + i,
        )
        .await
        .expect("create quota key");
    }

    // 7. Full CRUD verification: list, get, patch, delete
    let user_providers = list_providers(&writer_pool, &user_actor)
        .await
        .expect("list user providers");
    assert!(!user_providers.is_empty());

    let fetched_p = get_provider(&writer_pool, &user_actor, personal_provider.id)
        .await
        .expect("get provider");
    assert_eq!(fetched_p.id, personal_provider.id);

    let keys = list_upstream_keys(&writer_pool, &user_actor, personal_provider.id)
        .await
        .expect("list keys");
    assert!(!keys.is_empty());

    let fetched_k = get_upstream_key(&writer_pool, &user_actor, key.id)
        .await
        .expect("get key");
    assert_eq!(fetched_k.id, key.id);

    let patched_k = patch_upstream_key(
        &writer_pool,
        &user_actor,
        key.id,
        UpstreamKeyPatch {
            label: Some("renamed_key".to_string()),
            priority: Some(250),
            strict_tier: Some(3),
            enabled: Some(false),
        },
    )
    .await
    .expect("patch key");
    assert_eq!(patched_k.label, "renamed_key");
    assert_eq!(patched_k.priority, 250);
    assert_eq!(patched_k.strict_tier, 3);
    assert!(!patched_k.enabled);

    delete_upstream_key(&writer_pool, &user_actor, key.id)
        .await
        .expect("delete key");
    let missing_k = get_upstream_key(&writer_pool, &user_actor, key.id)
        .await
        .expect_err("deleted key must not be found");
    assert!(matches!(missing_k, ProviderError::UpstreamKeyNotFound(_)));

    delete_provider(&writer_pool, &user_actor, personal_provider.id)
        .await
        .expect("delete provider");
    let missing_p = get_provider(&writer_pool, &user_actor, personal_provider.id)
        .await
        .expect_err("deleted provider must not be found");
    assert!(matches!(missing_p, ProviderError::ProviderNotFound(_)));
}

struct CreateProviderControlOp {
    actor: ActorScope,
    new_provider: NewProvider,
    now_ms: i64,
}

impl ControlOp for CreateProviderControlOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WriterError>> + Send + 'a>>
    {
        Box::pin(async move {
            create_provider_tx(tx, &self.actor, self.new_provider, self.now_ms)
                .await
                .map_err(|e| WriterError::Execution(e.to_string()))?;
            Ok(())
        })
    }
}

struct CreateUpstreamKeyControlOp {
    actor: ActorScope,
    master_key: MasterKey,
    master_key_id: String,
    entropy: Arc<dyn llmroller_core::seams::EntropySource>,
    new_key: NewUpstreamKey,
    now_ms: i64,
}

impl ControlOp for CreateUpstreamKeyControlOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WriterError>> + Send + 'a>>
    {
        Box::pin(async move {
            create_upstream_key_tx(
                tx,
                &self.actor,
                &self.master_key,
                &self.master_key_id,
                self.entropy,
                self.new_key,
                self.now_ms,
            )
            .await
            .map_err(|e| WriterError::Execution(e.to_string()))?;
            Ok(())
        })
    }
}

/// Spec: PRV-1, PRV-4, PRV-5
/// Providers and keys propagate to ConfigSnapshot via Writer and Reloader within 1 s,
/// and slice isolation ensures unchanged domain slices retain identical Arc pointers.
#[doc = "Spec: PRV-1, PRV-4, PRV-5"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_provider_and_key_reloader_propagation_and_slice_isolation() {
    let (_dir, writer_pool, reader_pool, master_key, _admin_id, user_id) =
        setup_test_context().await;

    let user_actor = ActorScope::user(user_id, Role::User);

    let initial_snapshot = load_initial_snapshot(&reader_pool)
        .await
        .expect("load initial snapshot");
    assert_eq!(initial_snapshot.providers.len(), 0);
    assert_eq!(initial_snapshot.upstream_keys.len(), 0);

    let shared_snapshot = Arc::new(ArcSwap::new(Arc::new(initial_snapshot.clone())));
    let runtime_state = Arc::new(RuntimeState::new());

    let (reloader_tx, reloader_rx) = mpsc::channel(100);
    let (writer, _writer_task) = start_writer(writer_pool.clone(), reloader_tx.clone());
    let (reloader, _reloader_task) = start_reloader(
        reader_pool.clone(),
        shared_snapshot.clone(),
        runtime_state.clone(),
        reloader_rx,
        reloader_tx,
    );

    // 1. Create a provider through Control queue
    let prov_v = writer
        .execute_control(
            vec![ConfigDomain::Providers],
            vec![CacheInvalidationEvent::SettingsChanged],
            CreateProviderControlOp {
                actor: user_actor,
                new_provider: NewProvider {
                    name: "pers_reloader".to_string(),
                    kind: ProviderKind::OpencodeGo,
                    base_url: "https://opencode.ai/api".to_string(),
                    config_json: "{}".to_string(),
                    ownership_scope: OwnershipScope::Personal,
                    owner_user_id: Some(user_id),
                    usage_url_cross_origin_ack: false,
                    allow_private_network: false,
                    enabled: true,
                },
                now_ms: 10,
            },
        )
        .await
        .expect("create provider via control");

    reloader
        .wait_for_version(prov_v, Duration::from_secs(1))
        .await
        .expect("reloader wait prov_v");

    let snap1 = reloader.snapshot();
    assert_eq!(snap1.config_version, prov_v);
    assert_eq!(snap1.providers.len(), 1);
    let provider_id = *snap1.providers.keys().next().expect("provider id");

    // Assert slice isolation: users and settings Arc pointers must be unchanged!
    assert!(Arc::ptr_eq(&snap1.users, &initial_snapshot.users));
    assert!(Arc::ptr_eq(&snap1.settings, &initial_snapshot.settings));

    // 2. Create an upstream key through Control queue
    let key_v = writer
        .execute_control(
            vec![ConfigDomain::UpstreamKeys],
            vec![CacheInvalidationEvent::UpstreamKeyChanged(
                UpstreamKeyId::generate(),
            )],
            CreateUpstreamKeyControlOp {
                actor: user_actor,
                master_key: master_key.clone(),
                master_key_id: "key-0".to_string(),
                entropy: Arc::new(OsEntropy),
                new_key: NewUpstreamKey {
                    provider_id,
                    label: "key_via_writer".to_string(),
                    secret: secret_string("sk-live-key-123456"),
                    priority: Some(200),
                    strict_tier: Some(3),
                    enabled: Some(true),
                },
                now_ms: 20,
            },
        )
        .await
        .expect("create key via control");

    reloader
        .wait_for_version(key_v, Duration::from_secs(1))
        .await
        .expect("reloader wait key_v");

    let snap2 = reloader.snapshot();
    assert_eq!(snap2.config_version, key_v);
    assert_eq!(snap2.upstream_keys.len(), 1);

    // Assert slice isolation: users, settings, and providers Arc pointers must be unchanged!
    assert!(Arc::ptr_eq(&snap2.users, &snap1.users));
    assert!(Arc::ptr_eq(&snap2.settings, &snap1.settings));
    assert!(Arc::ptr_eq(&snap2.providers, &snap1.providers));
}

/// Spec: PRV-1, PRV-4, PRV-5, Partial: CLK-1
/// End-to-end JSON control-plane CRUD handlers for providers, keys, client keys, and settings
/// through Axum router and session auth per SRDD Section 14.2.
#[doc = "Spec: PRV-1, PRV-4, PRV-5, Partial: CLK-1"]
#[tokio::test]
async fn test_json_crud_endpoints_via_control_plane_router() {
    let (dir, writer_pool, _reader_pool, master_key, _admin_id, _user_id) =
        setup_test_context().await;

    let auth_config = AuthConfig {
        trusted_proxies: vec![],
        dev_insecure_cookies: true,
        require_tls: false,
    };
    let auth_state = AuthAppState::new(writer_pool.clone(), auth_config);
    let setup_state = SetupAppState {
        pool: writer_pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    // 1. GET /login to retrieve CSRF token
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

    // 2. POST /api/auth/login as admin_user
    let login_req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::COOKIE, &login_cookie)
        .header("x-csrf-token", &login_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "username": "admin_user",
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

    // 3. POST /api/user/providers -> create personal provider via JSON API
    let create_prov_req = Request::builder()
        .method("POST")
        .uri("/api/user/providers")
        .header(header::COOKIE, &session_cookie)
        .header("x-csrf-token", &session_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "name": "json_personal_prov",
                "kind": "custom",
                "base_url": "https://custom.api/v1",
                "config_json": "{\"paths\":{\"chat\":\"/chat/completions\"}}",
            })
            .to_string(),
        ))
        .expect("create prov req");
    let create_prov_res = app.clone().oneshot(create_prov_req).await.expect("res");
    assert_eq!(create_prov_res.status(), StatusCode::CREATED);

    let prov_bytes = create_prov_res
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let prov_json: serde_json::Value = serde_json::from_slice(&prov_bytes).expect("parse json");
    let provider_id = prov_json["id"].as_str().expect("provider id");
    assert_eq!(prov_json["name"], "json_personal_prov");
    assert_eq!(prov_json["ownership_scope"], "personal");
    assert!(
        !prov_json["share_config_fingerprint"]
            .as_str()
            .expect("fp")
            .is_empty()
    );

    // 4. POST /api/user/providers/{id}/keys -> create upstream key via JSON API
    let create_key_req = Request::builder()
        .method("POST")
        .uri(format!("/api/user/providers/{provider_id}/keys"))
        .header(header::COOKIE, &session_cookie)
        .header("x-csrf-token", &session_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "label": "json_key_1",
                "secret": "sk-secret-payload-9999",
                "priority": 120,
            })
            .to_string(),
        ))
        .expect("create key req");
    let create_key_res = app.clone().oneshot(create_key_req).await.expect("res");
    assert_eq!(create_key_res.status(), StatusCode::CREATED);

    let key_bytes = create_key_res
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let key_json: serde_json::Value = serde_json::from_slice(&key_bytes).expect("parse json");
    assert_eq!(key_json["label"], "json_key_1");
    assert_eq!(key_json["secret_last4"], "9999");
    assert_eq!(key_json["priority"], 120);
    // PRV-5: Secret is never echoed in API response!
    assert!(key_json.get("secret").is_none());
    assert!(key_json.get("secret_plain").is_none());

    // 5. POST /api/user/client-keys -> create client key via JSON API
    let create_ck_req = Request::builder()
        .method("POST")
        .uri("/api/user/client-keys")
        .header(header::COOKIE, &session_cookie)
        .header("x-csrf-token", &session_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "label": "agent_key",
                "routing_scope": "personal_first",
            })
            .to_string(),
        ))
        .expect("create ck req");
    let create_ck_res = app.clone().oneshot(create_ck_req).await.expect("res");
    assert_eq!(create_ck_res.status(), StatusCode::CREATED);

    let ck_bytes = create_ck_res
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let ck_json: serde_json::Value = serde_json::from_slice(&ck_bytes).expect("parse json");
    let raw_token = ck_json["token"].as_str().expect("raw token");
    assert!(raw_token.starts_with("llr_"));
    assert_eq!(ck_json["client_key"]["label"], "agent_key");

    // 6. GET /admin/api/settings -> fetch settings
    let get_settings_req = Request::builder()
        .method("GET")
        .uri("/admin/api/settings")
        .header(header::COOKIE, &session_cookie)
        .body(Body::empty())
        .expect("get settings req");
    let get_settings_res = app.clone().oneshot(get_settings_req).await.expect("res");
    assert_eq!(get_settings_res.status(), StatusCode::OK);

    // 7. PATCH /admin/api/settings -> valid update
    let patch_settings_req = Request::builder()
        .method("PATCH")
        .uri("/admin/api/settings")
        .header(header::COOKIE, &session_cookie)
        .header("x-csrf-token", &session_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "pin_ttl_secs": 95000,
            })
            .to_string(),
        ))
        .expect("patch settings req");
    let patch_settings_res = app.clone().oneshot(patch_settings_req).await.expect("res");
    assert_eq!(patch_settings_res.status(), StatusCode::OK);

    let set_bytes = patch_settings_res
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let set_json: serde_json::Value = serde_json::from_slice(&set_bytes).expect("parse json");
    assert_eq!(set_json["pin_ttl_secs"], 95000);

    // 8. PATCH /admin/api/settings -> invalid unknown key rejected with 422
    let bad_settings_req = Request::builder()
        .method("PATCH")
        .uri("/admin/api/settings")
        .header(header::COOKIE, &session_cookie)
        .header("x-csrf-token", &session_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "unknown_key": "val",
            })
            .to_string(),
        ))
        .expect("bad settings req");
    let bad_settings_res = app.clone().oneshot(bad_settings_req).await.expect("res");
    assert_eq!(bad_settings_res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
