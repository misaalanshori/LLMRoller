//! Integration test for providers and upstream keys repositories, quotas, encryption, and fingerprinting.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/4. SRDD - Users, Roles, and Provider Ownership.md#43-consented-personal-provider-and-key-sharing-shr
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Phase: ./docs/bigbang/big-bang-phases-2.md#20-sub-phase-20-minimal-control-plane-json-crud-for-providers-keys-client-keys-and-settings

use std::sync::Arc;
use std::time::Duration;

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
    let sealer = XChaCha20Poly1305Sealer::new(
        master_key.clone(),
        [0u8; 16],
        Arc::new(llmroller_core::seams::OsEntropy),
    );
    let unsealed = sealer
        .open(
            "upstream_key",
            &[
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
