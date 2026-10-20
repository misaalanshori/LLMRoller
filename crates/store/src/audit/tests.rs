//! Audit-chain primitive tests used by master-key rotation.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#921-canonical-audit_log-hash-chain-serialization
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use sqlx::Connection;
use tempfile::TempDir;

use super::{AuditEntry, GENESIS_HASH, append, canonical_frame, entry_hash};
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
#[doc = "Partial: AUD-1, Partial: SEC-13"]
async fn appended_rows_chain_from_genesis_and_previous_hash() {
    let (_directory, pool) = migrated_database().await;
    let key = [0x42_u8; 32];
    let mut connection = pool.acquire().await.expect("connection");

    let mut first_tx = connection.begin().await.expect("transaction");
    let (first_seq, first_hash) = append(&mut first_tx, &key, entry("first"))
        .await
        .expect("first audit row");
    first_tx.commit().await.expect("commit first row");

    let mut second_tx = connection.begin().await.expect("transaction");
    let (second_seq, second_hash) = append(&mut second_tx, &key, entry("second"))
        .await
        .expect("second audit row");
    second_tx.commit().await.expect("commit second row");

    assert_eq!(first_seq, 1);
    assert_eq!(second_seq, 2);
    assert_eq!(first_hash.len(), 64);
    assert_eq!(second_hash.len(), 64);

    drop(connection);
    let first: (String, String) =
        sqlx::query_as("SELECT prev_hash, hash FROM audit_log WHERE seq = 1")
            .fetch_one(&pool)
            .await
            .expect("first row");
    let second: (String, String) =
        sqlx::query_as("SELECT prev_hash, hash FROM audit_log WHERE seq = 2")
            .fetch_one(&pool)
            .await
            .expect("second row");
    assert_eq!(first.0, GENESIS_HASH);
    assert_eq!(first.1, first_hash);
    assert_eq!(second.0, first_hash);
    assert_eq!(second.1, second_hash);
    assert_eq!(
        entry_hash(&key, &entry("second"), 2, &first_hash),
        second_hash
    );

    pool.close().await;
}
