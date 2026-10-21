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
