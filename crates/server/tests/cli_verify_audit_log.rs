//! End-to-end `verify-audit-log` CLI integration tests.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#611-web-ui-and-audit-logging-ui--aud
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-1.md#15-sub-phase-15-tamper-evident-audit_log-hmac-chain-external-anchors-and-verify-audit-log-cli

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use llmroller_core::seams::DeterministicRng;
use llmroller_store::audit::{AuditEntry, append};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};
use sqlx::Acquire as _;

const MASTER_KEY_ID: &str = "mk_audit_test";

fn encoded(byte: u8) -> String {
    STANDARD.encode([byte; 32])
}

async fn seed_audit_database(data_dir: &Path) -> (String, String) {
    let pool = open_pool(&data_dir.join("llmroller.db"), Role::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migration");
    let entropy = Arc::new(DeterministicRng::new(0x8888));
    let instance_state = load_or_initialize(
        &pool,
        MASTER_KEY_ID,
        &MasterKey::from_bytes([0x88; 32]),
        entropy.clone(),
        1_000,
    )
    .await
    .expect("bootstrap");

    use llmroller_core::seams::ExposeSecret;
    let audit_key = instance_state.audit_hmac_key.expose_secret();

    let mut last_hash = String::new();
    for i in 1..=5 {
        let mut conn = pool.acquire().await.expect("acquire");
        let mut tx = conn.begin().await.expect("begin");
        let entry = AuditEntry {
            id: &format!("01ARZ3NDEKTSV4RRFFQ69G5F0{i}"),
            ts: 1_700_000_000_000 + i * 1_000,
            actor_user_id: None,
            ip_addr: None,
            action: &format!("test.action_{i}"),
            target_type: "item",
            target_id: Some("target_1"),
            detail_json: None,
        };
        let (_seq, hash) = append(&mut tx, audit_key, entry).await.expect("append");
        tx.commit().await.expect("commit");
        last_hash = hash;
    }

    pool.close().await;
    ("5".to_owned(), last_hash)
}

#[tokio::test]
#[doc = "Spec: AUD-1, Spec: SEC-13"]
async fn verify_audit_log_cli_succeeds_on_intact_log() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (max_seq, tail_hash) = seed_audit_database(directory.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("verify-audit-log")
        .arg("--data-dir")
        .arg(directory.path())
        .arg("--json")
        .env("LLMROLLER_MASTER_KEY", encoded(0x88))
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
        .output()
        .await
        .expect("run verify-audit-log");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("\"max_seq\":{max_seq}")));
    assert!(stdout.contains(&tail_hash));

    // Also succeeds with valid anchor
    let anchor_output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("verify-audit-log")
        .arg("--anchor-seq")
        .arg("5")
        .arg("--anchor-hash")
        .arg(&tail_hash)
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_MASTER_KEY", encoded(0x88))
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
        .output()
        .await
        .expect("run verify-audit-log with anchor");
    assert!(anchor_output.status.success());
}

#[tokio::test]
#[doc = "Spec: AUD-1, Spec: SEC-13"]
async fn verify_audit_log_cli_exits_8_on_tampering() {
    let directory = tempfile::tempdir().expect("temp dir");
    seed_audit_database(directory.path()).await;

    // Tamper with action on seq 3
    let pool = open_pool(&directory.path().join("llmroller.db"), Role::Writer)
        .await
        .expect("pool");
    sqlx::query("UPDATE audit_log SET action = 'tampered' WHERE seq = 3")
        .execute(&pool)
        .await
        .expect("tamper");
    pool.close().await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("verify-audit-log")
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_MASTER_KEY", encoded(0x88))
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
        .output()
        .await
        .expect("run verify-audit-log");

    assert_eq!(
        output.status.code(),
        Some(8),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[doc = "Spec: AUD-1, Spec: SEC-13"]
async fn verify_audit_log_cli_exits_9_on_tail_truncation() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (_seq, tail_hash) = seed_audit_database(directory.path()).await;

    // Delete last row (seq 5)
    let pool = open_pool(&directory.path().join("llmroller.db"), Role::Writer)
        .await
        .expect("pool");
    sqlx::query("DELETE FROM audit_log WHERE seq = 5")
        .execute(&pool)
        .await
        .expect("delete row 5");
    pool.close().await;

    // Verification against anchor seq 5 detects truncation
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_llmroller"))
        .arg("verify-audit-log")
        .arg("--anchor-seq")
        .arg("5")
        .arg("--anchor-hash")
        .arg(&tail_hash)
        .arg("--data-dir")
        .arg(directory.path())
        .env("LLMROLLER_MASTER_KEY", encoded(0x88))
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
        .output()
        .await
        .expect("run verify-audit-log");

    assert_eq!(
        output.status.code(),
        Some(9),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
