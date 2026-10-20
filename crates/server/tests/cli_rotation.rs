//! End-to-end `rotate-master-key` CLI tests.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use llmroller_core::seams::{DeterministicRng, ExposeSecret};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};

fn encoded(byte: u8) -> String {
    STANDARD.encode([byte; 32])
}

async fn seed_database(data_dir: &Path) {
    let pool = open_pool(&data_dir.join("llmroller.db"), Role::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migration");
    load_or_initialize(
        &pool,
        "mk_old",
        &MasterKey::from_bytes([0x61; 32]),
        Arc::new(DeterministicRng::new(0x61)),
        1_000,
    )
    .await
    .expect("bootstrap");
    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: SEC-1, Spec: SEC-2"]
async fn rotate_master_key_cli_completes_and_flips_the_active_key() {
    let directory = tempfile::tempdir().expect("temporary directory");
    seed_database(directory.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("rotate-master-key")
        .arg("--old-key-env")
        .arg("LLMROLLER_TEST_OLD_KEY")
        .arg("--new-key-env")
        .arg("LLMROLLER_TEST_NEW_KEY")
        .arg("--new-key-id")
        .arg("mk_new")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_TEST_OLD_KEY", encoded(0x61))
        .env("LLMROLLER_TEST_NEW_KEY", encoded(0x62))
        .output()
        .await
        .expect("run llmroller");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("rotation complete"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pool = open_pool(&directory.path().join("llmroller.db"), Role::Writer)
        .await
        .expect("writer pool");
    let state = load_or_initialize(
        &pool,
        "mk_new",
        &MasterKey::from_bytes([0x62; 32]),
        Arc::new(DeterministicRng::new(0x62)),
        2_000,
    )
    .await
    .expect("reload with new key");
    assert_eq!(state.active_master_key_id, "mk_new");
    assert_eq!(state.session_hmac_secret.expose_secret().len(), 32);
    assert_eq!(state.audit_hmac_key.expose_secret().len(), 32);

    let audit_action: String =
        sqlx::query_scalar("SELECT action FROM audit_log ORDER BY seq DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("audit row");
    assert_eq!(audit_action, "master_key.rotated");

    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: SEC-2"]
async fn rotate_master_key_cli_exits_3_when_a_key_variable_is_missing() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("rotate-master-key")
        .arg("--old-key-env")
        .arg("LLMROLLER_TEST_DEFINITELY_MISSING_OLD_KEY")
        .arg("--new-key-env")
        .arg("LLMROLLER_TEST_NEW_KEY")
        .arg("--new-key-id")
        .arg("mk_new")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_TEST_NEW_KEY", encoded(0x63))
        .env_remove("LLMROLLER_TEST_DEFINITELY_MISSING_OLD_KEY")
        .output()
        .await
        .expect("run llmroller");
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is not set"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[doc = "Partial: SEC-2"]
async fn rotate_master_key_cli_exits_4_when_the_lock_is_held() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let lock_path = directory.path().join("llmroller.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("lock file");
    lock.try_lock().expect("test must acquire lock");

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("rotate-master-key")
        .arg("--old-key-env")
        .arg("LLMROLLER_TEST_OLD_KEY")
        .arg("--new-key-env")
        .arg("LLMROLLER_TEST_NEW_KEY")
        .arg("--new-key-id")
        .arg("mk_new")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_TEST_OLD_KEY", encoded(0x61))
        .env("LLMROLLER_TEST_NEW_KEY", encoded(0x62))
        .output()
        .await
        .expect("run llmroller");
    assert_eq!(output.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("held by another process"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
