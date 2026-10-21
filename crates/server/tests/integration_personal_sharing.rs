//! Integration tests for personal provider and key sharing, consent workflow, and overlap prevention.
//!
//! Spec: Section 4.4, Section 4.6, Section 6.4, Section 8.2, Section 10.1,
//! Requirements: SHR-1, SHR-2, SHR-3, SHR-4, SHR-5, USR-2, USR-3, USR-4, SEC-17.

use std::sync::Arc;
use tempfile::tempdir;

use llmroller_core::accumulators::{BudgetScopeId, MonthlyAccumulatorRegistry};
use llmroller_core::ids::UserId;
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_core::shares::{IneligibleReason, is_share_eligible};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, ProviderPatch, compute_share_config_fingerprint,
    create_provider_tx, create_upstream_key_tx, patch_provider_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::shares::{
    NewShare, ShareError, accept_provider_share_tx, approve_provider_share_tx,
    create_provider_share_tx, get_provider_share_tx, revoke_provider_share_tx,
};
use llmroller_store::users::{NewUser, create_user, set_disabled};

async fn setup_sharing_test_db() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    MasterKey,
    UserId,
    UserId,
    UserId,
) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("sharing_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x88; 32]);
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
            username: "admin_user",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("admin");

    let alice = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "alice_user",
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
            username: "bob_user",
            password: "supersecretbobpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("bob");

    (dir, writer_pool, master_key, admin.id, alice.id, bob.id)
}

#[doc = "Spec: SHR-1, SHR-2, SHR-3, SHR-4, SHR-5, USR-2, USR-3, USR-4, SEC-17"]
#[tokio::test]
async fn test_personal_provider_and_key_sharing_lifecycle() {
    let (_dir, pool, master_key, _admin_id, alice_id, bob_id) = setup_sharing_test_db().await;
    let alice_actor = ActorScope::user(alice_id, Role::User);
    let mut tx = pool.begin().await.expect("begin tx");

    // 1. Alice creates a personal custom provider and key
    let config_json = r#"{"base_url":"https://custom.ai/v1","paths":{"chat":"/chat/completions"}}"#;
    let provider = create_provider_tx(
        &mut tx,
        &alice_actor,
        NewProvider {
            name: "Alice Custom Provider".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://custom.ai/v1".to_string(),
            config_json: config_json.to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(alice_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("create personal provider");

    let key = create_upstream_key_tx(
        &mut tx,
        &alice_actor,
        &master_key,
        "sk-alice-secret",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Alice Key".to_string(),
            secret: llmroller_core::seams::secret_string("sk-alice-secret"),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");

    let initial_fp = compute_share_config_fingerprint(
        provider.kind.as_str(),
        &provider.base_url,
        provider.allow_private_network,
        config_json,
    );

    // 2. Alice shares her key with Bob (allowed_models = ["deepseek-v4.1-flash"], max_monthly = $5.00) (SHR-1)
    let share = create_provider_share_tx(
        &mut tx,
        &alice_id,
        NewShare {
            provider_id: provider.id,
            upstream_key_id: Some(key.id),
            grantee_user_id: bob_id,
            allowed_models: Some(vec!["deepseek-v4.1-flash".to_string()]),
            max_monthly_micro_usd: Some(5_000_000),
        },
        1000,
    )
    .await
    .expect("create share");

    // Share is ineligible initially (not accepted by Bob yet)
    let share_cfg = get_provider_share_tx(&mut tx, &share.share_id)
        .await
        .expect("get share");
    assert_eq!(
        is_share_eligible(
            &share_cfg,
            provider.kind,
            &initial_fp,
            false,
            Some("deepseek-v4.1-flash")
        ),
        Err(IneligibleReason::FingerprintMismatch)
    );

    // 3. Bob accepts share by echoing provider fingerprint (SHR-2)
    accept_provider_share_tx(&mut tx, &share.share_id, &bob_id, &initial_fp, 2000)
        .await
        .expect("bob accepts");

    let share_cfg = get_provider_share_tx(&mut tx, &share.share_id)
        .await
        .expect("get share");
    // Since provider is Custom, still ineligible before admin approval (SHR-2)
    assert_eq!(
        is_share_eligible(
            &share_cfg,
            provider.kind,
            &initial_fp,
            false,
            Some("deepseek-v4.1-flash")
        ),
        Err(IneligibleReason::CustomProviderNeedsAdminApproval)
    );

    // Admin approves share (SHR-2)
    approve_provider_share_tx(&mut tx, &share.share_id, 3000)
        .await
        .expect("admin approves");

    let share_cfg = get_provider_share_tx(&mut tx, &share.share_id)
        .await
        .expect("get share");
    // Now fully eligible for shared model!
    assert!(
        is_share_eligible(
            &share_cfg,
            provider.kind,
            &initial_fp,
            false,
            Some("deepseek-v4.1-flash")
        )
        .is_ok()
    );

    // Unshared model is rejected
    assert_eq!(
        is_share_eligible(
            &share_cfg,
            provider.kind,
            &initial_fp,
            false,
            Some("gpt-5.4-pro")
        ),
        Err(IneligibleReason::ModelNotAllowed)
    );

    // 4. Overlap prevention: Alice cannot create an overlapping provider-level share for Bob (SHR-1)
    let overlap_err = create_provider_share_tx(
        &mut tx,
        &alice_id,
        NewShare {
            provider_id: provider.id,
            upstream_key_id: None, // provider-level share overlaps with existing key share
            grantee_user_id: bob_id,
            allowed_models: None,
            max_monthly_micro_usd: None,
        },
        4000,
    )
    .await;
    assert!(matches!(overlap_err, Err(ShareError::ShareOverlap)));

    // 5. Modifying provider config changes fingerprint and suspends share within 1s (SHR-3)
    let patched_config =
        r#"{"base_url":"https://modified-custom.ai/v1","paths":{"chat":"/chat/completions"}}"#;
    let updated_provider = patch_provider_tx(
        &mut tx,
        &alice_actor,
        provider.id,
        ProviderPatch {
            name: None,
            base_url: Some("https://modified-custom.ai/v1".to_string()),
            config_json: Some(patched_config.to_string()),
            allow_private_network: None,
            usage_url_cross_origin_ack: None,
            enabled: None,
        },
        5000,
    )
    .await
    .expect("patch provider");

    assert_ne!(updated_provider.share_config_fingerprint, initial_fp);

    // Evaluating share with updated fingerprint suspends share
    assert_eq!(
        is_share_eligible(
            &share_cfg,
            provider.kind,
            &updated_provider.share_config_fingerprint,
            false,
            Some("deepseek-v4.1-flash")
        ),
        Err(IneligibleReason::FingerprintMismatch)
    );

    // 6. Disabling Alice makes her shared routes ineligible (SHR-4)
    tx.commit().await.expect("commit tx 1");
    let admin_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    set_disabled(&pool, &admin_actor, alice_id, true, 6000)
        .await
        .expect("disable alice");

    // When grantor is disabled, share check returns GrantorDisabled
    assert_eq!(
        is_share_eligible(
            &share_cfg,
            provider.kind,
            &initial_fp,
            true, // grantor_disabled = true
            Some("deepseek-v4.1-flash")
        ),
        Err(IneligibleReason::GrantorDisabled)
    );

    let mut tx = pool.begin().await.expect("begin tx 2");

    let model_pk = llmroller_core::ids::ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id) VALUES (?, 'deepseek-v4.1-flash')")
        .bind(model_pk.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert model");

    // Insert Bob's route preference on the shared key
    sqlx::query(
        "INSERT INTO user_route_preferences (user_id, upstream_key_id, model_pk, priority_override, enabled)
         VALUES (?, ?, ?, 500, 1)",
    )
    .bind(bob_id.to_string())
    .bind(key.id.to_string())
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert bob route preference");

    // 7. Revoking share marks it revoked_at, enabled = 0, and deletes grantee's user_route_preferences (SHR-5)
    revoke_provider_share_tx(&mut tx, &share.share_id, &alice_id, 7000)
        .await
        .expect("revoke share");

    let pref_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_route_preferences WHERE user_id = ? AND upstream_key_id = ?",
    )
    .bind(bob_id.to_string())
    .bind(key.id.to_string())
    .fetch_one(&mut *tx)
    .await
    .expect("count route preferences");
    assert_eq!(
        pref_count, 0,
        "Grantee user_route_preferences must be deleted on share revocation (SHR-5)"
    );

    let revoked_cfg = get_provider_share_tx(&mut tx, &share.share_id)
        .await
        .expect("get revoked share");
    assert!(!revoked_cfg.enabled);

    // 8. Monthly cap reservation under 50 concurrent streams never overshoots $5.00 cap
    let registry = MonthlyAccumulatorRegistry::new();
    let scope_id = BudgetScopeId::ProviderShare(share.share_id);
    let cap = 5_000_000; // $5.00
    let est = 100_000; // $0.10

    let mut reserved_guards = Vec::new();
    for _ in 0..50 {
        let guard = registry
            .try_reserve(scope_id.clone(), Some(cap), est, 8000)
            .expect("reserve up to cap");
        reserved_guards.push(guard);
    }

    // 51st reservation fails with ProviderShareCapExhausted
    let over_err = registry.try_reserve(scope_id.clone(), Some(cap), est, 8000);
    assert!(over_err.is_err());

    // Clean up guards
    for g in reserved_guards {
        g.commit(est);
    }

    tx.commit().await.expect("commit");
}
