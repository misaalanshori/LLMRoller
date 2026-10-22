//! Service Account repository tests: CIDR parsing, CRUD, and tenant isolation.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk (CLK-6)
//! Spec: ./docs/srdd/14.k. SRDD - Service Account Detail View, CIDR IP Restrictions, and Child Keys.md#14k4

use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role};
use sqlx::SqlitePool;
use tempfile::TempDir;

use super::{
    MAX_SERVICE_ACCOUNT_CIDRS, NewServiceAccount, ServiceAccountError, ServiceAccountPatch,
    create_service_account, delete_service_account, get_service_account, list_service_accounts,
    normalize_cidrs, parse_cidrs_text, patch_service_account, set_service_account_cidrs,
};
use crate::migrations::migrate;
use crate::pool::{Role as PoolRole, open_pool};

const USER_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const USER_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

async fn database() -> (TempDir, SqlitePool) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("service-accounts.db");
    let pool = open_pool(&path, PoolRole::Writer)
        .await
        .expect("writer pool must open");
    migrate(&pool).await.expect("schema must migrate");
    for (id, username) in [(USER_A, "alice"), (USER_B, "bob")] {
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, role, created_at) \
             VALUES (?, ?, 'hash', 'user', 1)",
        )
        .bind(id)
        .bind(username)
        .execute(&pool)
        .await
        .expect("user insert");
    }
    (directory, pool)
}

#[test]
#[doc = "Spec: CLK-6, Spec: SEC-17"]
fn cidrs_normalize_bare_hosts_and_reject_invalid_or_oversized_lists() {
    assert_eq!(parse_cidrs_text("").expect("empty list"), None);
    assert_eq!(
        parse_cidrs_text(" 203.0.113.10, \n2001:db8::1/48\n").expect("mixed list"),
        Some(vec![
            "203.0.113.10/32".to_string(),
            "2001:db8::/48".to_string()
        ])
    );

    let error = parse_cidrs_text("not-an-ip").expect_err("invalid token must fail");
    assert!(
        matches!(error, ServiceAccountError::Validation(message) if message.contains("not-an-ip"))
    );

    let too_many: Vec<String> = (0..=MAX_SERVICE_ACCOUNT_CIDRS)
        .map(|i| format!("10.0.{}.0/24", i % 256))
        .collect();
    let error = normalize_cidrs(&too_many).expect_err("oversized list must fail");
    assert!(
        matches!(error, ServiceAccountError::Validation(message) if message.contains("Maximum of 64"))
    );
}

#[tokio::test]
#[doc = "Spec: CLK-6, Spec: SEC-17, Spec: USR-3"]
async fn create_list_patch_cidrs_and_delete_are_scoped_to_the_owner() {
    let (_directory, pool) = database().await;
    let alice = UserId::parse(USER_A).expect("alice id");
    let bob = UserId::parse(USER_B).expect("bob id");
    let alice_actor = ActorScope::user(alice, Role::User);
    let bob_actor = ActorScope::user(bob, Role::User);

    let created = create_service_account(
        &pool,
        &alice_actor,
        alice,
        NewServiceAccount {
            name: "agents".to_string(),
            description: Some("CI runners".to_string()),
            workspace_scope: None,
            allowed_cidrs: Some(vec!["203.0.113.10".to_string()]),
        },
        1_000,
    )
    .await
    .expect("create service account");
    assert_eq!(created.workspace_scope, "personal");
    assert_eq!(
        created.allowed_cidrs.as_deref(),
        Some(["203.0.113.10/32".to_string()].as_slice())
    );

    let duplicate = create_service_account(
        &pool,
        &alice_actor,
        alice,
        NewServiceAccount {
            name: "agents".to_string(),
            ..NewServiceAccount::default()
        },
        1_001,
    )
    .await
    .expect_err("duplicate name must fail");
    assert!(matches!(duplicate, ServiceAccountError::NameTaken));

    let alice_list = list_service_accounts(&pool, &alice_actor, None, None)
        .await
        .expect("list service accounts");
    assert_eq!(alice_list.len(), 1);

    let bob_list = list_service_accounts(&pool, &bob_actor, None, None)
        .await
        .expect("bob list");
    assert!(bob_list.is_empty());

    let cross_tenant = get_service_account(&pool, &bob_actor, &created.id)
        .await
        .expect_err("cross-tenant read must 404");
    assert!(matches!(cross_tenant, ServiceAccountError::NotFound(_)));

    let renamed = patch_service_account(
        &pool,
        &alice_actor,
        &created.id,
        ServiceAccountPatch {
            name: Some("ci".to_string()),
            ..ServiceAccountPatch::default()
        },
        2_000,
    )
    .await
    .expect("rename service account");
    assert_eq!(renamed.name, "ci");

    let cleared = set_service_account_cidrs(&pool, &alice_actor, &created.id, None, 3_000)
        .await
        .expect("clear cidrs");
    assert_eq!(cleared.allowed_cidrs, None);

    delete_service_account(&pool, &bob_actor, &created.id)
        .await
        .expect_err("cross-tenant delete must fail");
    delete_service_account(&pool, &alice_actor, &created.id)
        .await
        .expect("owner delete");
    assert!(
        list_service_accounts(&pool, &alice_actor, None, None)
            .await
            .expect("list after delete")
            .is_empty()
    );

    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: CLK-6, Spec: SEC-17"]
async fn standard_users_cannot_create_global_service_accounts() {
    let (_directory, pool) = database().await;
    let alice = UserId::parse(USER_A).expect("alice id");
    let actor = ActorScope::user(alice, Role::User);
    let error = create_service_account(
        &pool,
        &actor,
        alice,
        NewServiceAccount {
            name: "global-agents".to_string(),
            workspace_scope: Some("global".to_string()),
            ..NewServiceAccount::default()
        },
        1_000,
    )
    .await
    .expect_err("global workspace requires admin");
    assert!(matches!(error, ServiceAccountError::Forbidden));
    pool.close().await;
}
