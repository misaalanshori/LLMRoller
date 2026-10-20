//! End-to-end `reset-admin` CLI tests.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Phase: ./docs/bigbang/big-bang-phases-1.md#14-sub-phase-14-atomic-first-admin-setup-argon2id--totp-auth-and-reset-admin-cli

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use llmroller_core::rbac::{ActorScope, Role as UserRole};
use llmroller_core::seams::DeterministicRng;
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::users::{NewUser, create_user, find_login_record};

const MASTER_KEY_ID: &str = "mk_test";

fn encoded(byte: u8) -> String {
    STANDARD.encode([byte; 32])
}

async fn seed_database(data_dir: &Path) {
    let pool = open_pool(&data_dir.join("llmroller.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migration");
    let entropy = Arc::new(DeterministicRng::new(0x42));
    load_or_initialize(
        &pool,
        MASTER_KEY_ID,
        &MasterKey::from_bytes([0x42; 32]),
        entropy.clone(),
        1_000,
    )
    .await
    .expect("bootstrap");

    let actor = ActorScope::user(llmroller_core::ids::UserId::generate(), UserRole::Admin);
    create_user(
        &pool,
        &actor,
        NewUser {
            username: "admin",
            password: "InitialPassword123!",
            role: UserRole::Admin,
            must_change_password: false,
        },
        entropy.as_ref(),
        1_000,
    )
    .await
    .expect("create admin");
    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: AUTH-4, Spec: AUTH-5"]
async fn reset_admin_cli_completes_and_restores_admin_access() {
    let directory = tempfile::tempdir().expect("temporary directory");
    seed_database(directory.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("reset-admin")
        .arg("--username")
        .arg("admin")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_MASTER_KEY", encoded(0x42))
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
        .output()
        .await
        .expect("run llmroller reset-admin");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("temporary password for administrator \"admin\":"),
        "stderr: {stderr}"
    );

    let pool = open_pool(&directory.path().join("llmroller.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    let record = find_login_record(&pool, "admin")
        .await
        .expect("find admin")
        .expect("admin exists");
    assert_eq!(record.user.role, UserRole::Admin);
    assert!(!record.user.disabled);
    assert!(record.user.must_change_password);

    let audit_action: String = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action = 'admin.break_glass' ORDER BY seq DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("audit action");
    assert_eq!(audit_action, "admin.break_glass");
}

#[tokio::test]
#[doc = "Spec: AUTH-4, Spec: AUTH-5"]
async fn reset_admin_cli_exits_10_when_user_not_found() {
    let directory = tempfile::tempdir().expect("temporary directory");
    seed_database(directory.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("reset-admin")
        .arg("--username")
        .arg("nonexistent")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_MASTER_KEY", encoded(0x42))
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
        .output()
        .await
        .expect("run llmroller reset-admin");
    assert_eq!(
        output.status.code(),
        Some(10),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[doc = "Spec: AUTH-4, Spec: AUTH-5"]
async fn reset_admin_cli_exits_4_when_lock_is_held() {
    let directory = tempfile::tempdir().expect("temporary directory");
    seed_database(directory.path()).await;

    let lock_path = directory.path().join("llmroller.lock");
    let _lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open lock");
    _lock.try_lock().expect("acquire test lock");

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("reset-admin")
        .arg("--username")
        .arg("admin")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_MASTER_KEY", encoded(0x42))
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
        .output()
        .await
        .expect("run llmroller reset-admin");
    assert_eq!(
        output.status.code(),
        Some(4),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[doc = "Spec: SEC-2"]
async fn reset_admin_cli_exits_3_when_master_key_is_missing() {
    let directory = tempfile::tempdir().expect("temporary directory");
    seed_database(directory.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("reset-admin")
        .arg("--username")
        .arg("admin")
        .arg("--data-dir")
        .arg(directory.path())
        .env_remove("LLMROLLER_MASTER_KEY")
        .env_remove("LLMROLLER_MASTER_KEY_FILE")
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
        .output()
        .await
        .expect("run llmroller reset-admin");
    assert_eq!(
        output.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
