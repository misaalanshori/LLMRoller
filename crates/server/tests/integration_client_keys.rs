//! Integration tests for client keys, grace rotation, and request gates G1-G6.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#100-ordered-request-gates-g1-through-g7
//! Phase: ./docs/bigbang/big-bang-phases-2.md#22-sub-phase-22-client-key-llr_-auth-rotation-grace-model-request-gates-g1g6-and-listeners

use std::sync::Arc;

use llmroller_core::client_key::{GateFailure, HashRole, evaluate_request_gates};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_server::reloader::load_initial_snapshot;
use llmroller_store::client_keys::{
    ClientKeyPatch, NewClientKey, create_client_key, patch_client_key, rotate_client_key,
};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user, set_disabled};
use tempfile::tempdir;

async fn setup_test_context() -> (tempfile::TempDir, sqlx::SqlitePool, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("llmroller.db");

    let pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x77; 32]);
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance init");

    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "ck_user",
            password: "supersecretuserpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("create user");

    (dir, pool, user.id)
}

/// Spec: CLK-1, SEC-3, Partial: CLK-2, Partial: CLK-5
/// Verify that client keys are created with `llr_` prefix, hashed with SHA-256,
/// support in-place grace rotation and G1-G6 gate checks.
#[doc = "Spec: CLK-1, SEC-3, Partial: CLK-2, Partial: CLK-5"]
#[tokio::test]
async fn test_client_key_lifecycle_and_rotation_grace() {
    let (_dir, pool, user_id) = setup_test_context().await;
    let user_actor = ActorScope::user(user_id, Role::User);
    let entropy = OsEntropy;
    let now_ms = 1_000_000;

    // 1. Create client key
    let res = create_client_key(
        &pool,
        &user_actor,
        user_id,
        NewClientKey {
            label: "coding_agent".to_string(),
            routing_scope: Some("personal_first".to_string()),
            allowed_models: Some(vec!["claude-sonnet-4.6".to_string()]),
            rate_limit_rpm: Some(100),
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: None,
            expires_at: None,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("create client key");

    use llmroller_core::seams::ExposeSecret as _;
    let raw_token1 = res.raw_token.expose_secret();
    assert!(raw_token1.starts_with("llr_"));
    assert_eq!(res.client_key.label, "coding_agent");
    assert_eq!(res.client_key.routing_scope, "personal_first");
    assert!(res.client_key.prev_key_hash.is_none());

    let key_id = res.client_key.id;

    // 2. Rotate client key in-place with grace_period_secs = 300
    let rot_res = rotate_client_key(&pool, &user_actor, key_id, 300, &entropy, now_ms + 10_000)
        .await
        .expect("rotate client key");

    let raw_token2 = rot_res.raw_token.expose_secret();
    assert_ne!(raw_token1, raw_token2);
    assert_eq!(rot_res.client_key.id, key_id);
    assert!(rot_res.client_key.prev_key_hash.is_some());
    assert_eq!(
        rot_res.client_key.prev_key_expires_at,
        Some(now_ms + 10_000 + 300_000)
    );

    // 3. Second rotation replaces predecessor
    let rot_res2 = rotate_client_key(&pool, &user_actor, key_id, 300, &entropy, now_ms + 50_000)
        .await
        .expect("second rotation");

    let raw_token3 = rot_res2.raw_token.expose_secret();
    assert_ne!(raw_token2, raw_token3);
    assert_eq!(
        rot_res2.client_key.prev_key_hash,
        Some(rot_res.client_key.key_hash)
    );
}

/// Spec: CLK-1, SEC-3, Partial: CLK-2
/// Verify request gates G1-G6 against ConfigSnapshot loaded from database,
/// including G2 key disabled, and G3 owner disabled.
#[doc = "Spec: CLK-1, SEC-3, Partial: CLK-2"]
#[tokio::test]
async fn test_client_key_request_gates_and_owner_disable() {
    let (_dir, pool, user_id) = setup_test_context().await;
    let user_actor = ActorScope::user(user_id, Role::User);
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let entropy = OsEntropy;
    let now_ms = 2_000_000;

    let res = create_client_key(
        &pool,
        &user_actor,
        user_id,
        NewClientKey {
            label: "gate_test_key".to_string(),
            routing_scope: Some("inherit".to_string()),
            allowed_models: Some(vec!["gpt-4o".to_string()]),
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: None,
            expires_at: None,
        },
        &entropy,
        now_ms,
    )
    .await
    .expect("create key");

    use llmroller_core::seams::ExposeSecret as _;
    let raw_token = res.raw_token.expose_secret();
    let hash = llmroller_store::crypto::sha256_hex(raw_token.as_bytes());

    // Convert hex hash to 32 bytes
    let mut hash_bytes = [0u8; 32];
    for i in 0..32 {
        hash_bytes[i] = u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16).expect("hex");
    }

    // Load snapshot from SQLite
    let snapshot = load_initial_snapshot(&pool)
        .await
        .expect("load initial snapshot");
    assert_eq!(snapshot.client_keys_by_hash.len(), 1);

    // Evaluate gates: happy path
    let (rec, role) = evaluate_request_gates(
        &hash_bytes,
        None,
        Some("gpt-4o"),
        Some(&["gpt-4o".to_string()]),
        &snapshot,
        None,
        now_ms,
    )
    .expect("happy path gates");
    assert_eq!(rec.id, res.client_key.id);
    assert_eq!(role, HashRole::Current);

    // Disable client key in DB, reload snapshot, assert G2 fails
    patch_client_key(
        &pool,
        &user_actor,
        res.client_key.id,
        ClientKeyPatch {
            disabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("patch disable");

    let snapshot_disabled_key = load_initial_snapshot(&pool).await.expect("reload");
    let err_g2 = evaluate_request_gates(
        &hash_bytes,
        None,
        Some("gpt-4o"),
        None,
        &snapshot_disabled_key,
        None,
        now_ms,
    )
    .expect_err("disabled key must fail G2");
    assert_eq!(err_g2, GateFailure::G2KeyDisabledOrExpired);

    // Re-enable client key, disable owner, reload snapshot, assert G3 fails
    patch_client_key(
        &pool,
        &user_actor,
        res.client_key.id,
        ClientKeyPatch {
            disabled: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("patch enable");

    set_disabled(&pool, &system_actor, user_id, true, now_ms)
        .await
        .expect("disable owner");

    let snapshot_disabled_owner = load_initial_snapshot(&pool).await.expect("reload");
    let err_g3 = evaluate_request_gates(
        &hash_bytes,
        None,
        Some("gpt-4o"),
        None,
        &snapshot_disabled_owner,
        None,
        now_ms,
    )
    .expect_err("disabled owner must fail G3");
    assert_eq!(err_g3, GateFailure::G3OwnerDisabled);
}
