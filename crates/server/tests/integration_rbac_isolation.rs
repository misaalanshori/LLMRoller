//! Integration tests for RBAC tenant isolation, project_key redaction, and share consent.
//!
//! Spec: Section 4.2, Section 6.2..6.5, Section 12.6.2, Section 14.2.1, Section 14.3,
//! Requirements: USR-2, USR-3, USR-4, UI-1, UI-2.

use std::sync::Arc;
use tempfile::tempdir;

use llmroller_admin::projection::{KeyProjectionInput, project_key};
use llmroller_core::ids::{UpstreamKeyId, UserId};
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
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
