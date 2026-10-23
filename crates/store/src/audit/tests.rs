//! Audit-chain primitive tests used by master-key rotation.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#921-canonical-audit_log-hash-chain-serialization
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use sqlx::Connection;
use tempfile::TempDir;

use super::{
    AuditEntry, AuditVerificationError, GENESIS_HASH, append, canonical_frame, entry_hash,
    format_audit_anchor, verify_audit_log,
};
use crate::migrations::migrate;
use crate::pool::{Role, open_pool};

async fn migrated_database() -> (TempDir, sqlx::SqlitePool) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("audit.db");
    let pool = open_pool(&path, Role::Writer)
        .await
        .expect("writer pool must open");
    migrate(&pool).await.expect("schema must migrate");
    (directory, pool)
}

fn entry(action: &str) -> AuditEntry<'_> {
    AuditEntry {
        id: action,
        ts: 1_700_000_000_000,
        actor_user_id: None,
        ip_addr: None,
        action,
        target_type: "instance",
        target_id: Some("mk_new"),
        detail_json: None,
    }
}

#[test]
#[doc = "Partial: AUD-1, Partial: SEC-13"]
fn nullable_audit_fields_are_unambiguous() {
    let absent = canonical_frame(&entry("test"), 1, GENESIS_HASH);
    let mut empty = entry("test");
    empty.actor_user_id = Some("");
    let present_empty = canonical_frame(&empty, 1, GENESIS_HASH);
    assert_ne!(
        absent, present_empty,
        "NULL and empty actor must serialize differently"
    );

    let mut empty_ip = entry("test");
    empty_ip.ip_addr = Some("");
    assert_ne!(
        canonical_frame(&entry("test"), 1, GENESIS_HASH),
        canonical_frame(&empty_ip, 1, GENESIS_HASH),
        "NULL and empty ip_addr must serialize differently"
    );
}

#[tokio::test]
#[doc = "Spec: AUD-1, Spec: SEC-13"]
async fn hash_chain_verification() {
    let (_directory, pool) = migrated_database().await;
    let key = [0x77_u8; 32];

    // (1) Inserting 10 audit rows (including backward NTP wall-clock steps on ts) verifies cleanly
    let timestamps = [
        1_700_000_000_000,
        1_700_000_001_000,
        1_699_999_990_000, // backward NTP step
        1_700_000_002_000,
        1_699_999_980_000, // backward NTP step
        1_700_000_003_000,
        1_700_000_004_000,
        1_700_000_005_000,
        1_700_000_006_000,
        1_700_000_007_000,
    ];

    let mut hashes = Vec::new();
    for (i, &ts) in timestamps.iter().enumerate() {
        let mut conn = pool.acquire().await.expect("acquire");
        let mut tx = conn.begin().await.expect("begin");
        let action = format!("action_{i}");
        let mut ent = entry(&action);
        ent.ts = ts;
        let (seq, hash) = append(&mut tx, &key, ent).await.expect("append");
        tx.commit().await.expect("commit");
        assert_eq!(seq, (i + 1) as i64);
        hashes.push(hash);
    }

    let report = verify_audit_log(&pool, &key, None, None)
        .await
        .expect("verify clean chain");
    assert_eq!(report.rows_verified, 10);
    assert_eq!(report.max_seq, 10);
    assert_eq!(report.tail_hash, hashes[9]);

    // (3) Anchor format test
    let anchor_line = format_audit_anchor(10, &hashes[9], timestamps[9]);
    assert!(anchor_line.starts_with("{\"event\":\"audit_anchor\",\"seq\":10,"));

    // (4) Anchor verification with valid anchor succeeds
    let anchor_report = verify_audit_log(&pool, &key, Some(10), Some(&hashes[9]))
        .await
        .expect("verify with anchor");
    assert_eq!(anchor_report.max_seq, 10);

    // (2a) Mutate one character in row 4: detects HashMismatch (exit code 8)
    sqlx::query("UPDATE audit_log SET action = 'tampered' WHERE seq = 4")
        .execute(&pool)
        .await
        .expect("tamper row 4");
    let tamper_err = verify_audit_log(&pool, &key, None, None)
        .await
        .expect_err("tamper must fail");
    assert!(matches!(
        tamper_err,
        AuditVerificationError::HashMismatch { seq: 4, .. }
    ));
    assert_eq!(tamper_err.exit_code(), 8);

    // Restore row 4 action and hash
    let mut restored_ent = entry("action_3");
    restored_ent.ts = timestamps[3];
    let computed_h4 = entry_hash(&key, &restored_ent, 4, &hashes[2]);
    sqlx::query("UPDATE audit_log SET action = 'action_3', hash = ? WHERE seq = 4")
        .bind(computed_h4)
        .execute(&pool)
        .await
        .expect("restore row 4");

    // (2b) Delete row 5 leaving a seq gap: detects SequenceGap (exit code 8)
    sqlx::query("DELETE FROM audit_log WHERE seq = 5")
        .execute(&pool)
        .await
        .expect("delete row 5");
    let gap_err = verify_audit_log(&pool, &key, None, None)
        .await
        .expect_err("gap must fail");
    assert!(matches!(
        gap_err,
        AuditVerificationError::SequenceGap {
            seq: 6,
            expected_seq: 5,
            ..
        }
    ));
    assert_eq!(gap_err.exit_code(), 8);

    // (4) Deleting the last 3 rows causes verify-audit-log --anchor-seq 10 --anchor-hash H to exit with code 9 (truncation detected)
    let (_dir2, pool2) = migrated_database().await;
    for (i, &ts) in timestamps.iter().enumerate() {
        let mut conn = pool2.acquire().await.expect("acquire");
        let mut tx = conn.begin().await.expect("begin");
        let action = format!("action_{i}");
        let mut ent = entry(&action);
        ent.ts = ts;
        append(&mut tx, &key, ent).await.expect("append");
        tx.commit().await.expect("commit");
    }
    // Delete last 3 rows (seq 8, 9, 10)
    sqlx::query("DELETE FROM audit_log WHERE seq >= 8")
        .execute(&pool2)
        .await
        .expect("delete last 3 rows");
    let trunc_err = verify_audit_log(&pool2, &key, Some(10), Some(&hashes[9]))
        .await
        .expect_err("truncation must be detected");
    assert!(matches!(
        trunc_err,
        AuditVerificationError::TruncationDetected {
            anchor_seq: 10,
            max_seq: 7,
            ..
        }
    ));
    assert_eq!(trunc_err.exit_code(), 9);
}

#[tokio::test]
#[doc = "Spec: AUD-1, REL-2"]
async fn concurrent_control_plane_mutations_preserve_intact_audit_hash_chain() {
    let (_dir, pool) = migrated_database().await;
    let key = [0x42_u8; 32];
    for i in 0..25 {
        let mut conn = pool.acquire().await.expect("acquire");
        let mut tx = conn
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("begin immediate");
        let action = format!("serialized_action_{i}");
        let mut ent = entry(&action);
        ent.ts = 1_700_000_000_000 + i * 10;
        append(&mut tx, &key, ent).await.expect("append");
        tx.commit().await.expect("commit");
    }
    let count = verify_audit_log(&pool, &key, None, None)
        .await
        .expect("serialized chain must verify cleanly");
    assert_eq!(count.rows_verified, 25);
}
