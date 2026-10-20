#![forbid(unsafe_code)]
//! Canonical `audit_log` hash-chain serialization.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#921-canonical-audit_log-hash-chain-serialization
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::fmt::Write as _;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use sqlx::{Sqlite, Transaction};
use thiserror::Error;

/// Genesis `prev_hash` for the first audit row.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const AUDIT_PREFIX: &[u8] = b"llmroller.audit.v1\0";

/// One audit row before its sequence and chain hash are computed.
#[derive(Debug, Clone, Copy)]
pub struct AuditEntry<'a> {
    /// ULID stored in `audit_log.id`.
    pub id: &'a str,
    /// UTC epoch milliseconds.
    pub ts: i64,
    /// Acting user, or `None` for system/offline actions.
    pub actor_user_id: Option<&'a str>,
    /// Client IP, or `None` when unavailable.
    pub ip_addr: Option<&'a str>,
    /// Stable action label.
    pub action: &'a str,
    /// Target record type.
    pub target_type: &'a str,
    /// Target record identifier, when applicable.
    pub target_id: Option<&'a str>,
    /// Canonical JSON detail object, when applicable.
    pub detail_json: Option<&'a str>,
}

/// Audit serialization or persistence failures.
#[derive(Debug, Error)]
pub enum AuditError {
    /// SQLite rejected the audit query or insert.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// Build the exact canonical frame from SRDD Section 9.2.1.
#[must_use]
pub fn canonical_frame(entry: &AuditEntry<'_>, seq: i64, prev_hash: &str) -> Vec<u8> {
    let mut frame = Vec::with_capacity(
        AUDIT_PREFIX.len()
            + 8
            + 4
            + prev_hash.len()
            + 4
            + entry.id.len()
            + 8
            + 1
            + entry.actor_user_id.map_or(0, |value| 4 + value.len())
            + 1
            + entry.ip_addr.map_or(0, |value| 4 + value.len())
            + 4
            + entry.action.len()
            + 4
            + entry.target_type.len()
            + 1
            + entry.target_id.map_or(0, |value| 4 + value.len())
            + 1
            + entry.detail_json.map_or(0, |value| 4 + value.len()),
    );
    frame.extend_from_slice(AUDIT_PREFIX);
    frame.extend_from_slice(&seq.to_be_bytes());
    push_required(&mut frame, prev_hash);
    push_required(&mut frame, entry.id);
    frame.extend_from_slice(&entry.ts.to_be_bytes());
    push_optional(&mut frame, entry.actor_user_id);
    push_optional(&mut frame, entry.ip_addr);
    push_required(&mut frame, entry.action);
    push_required(&mut frame, entry.target_type);
    push_optional(&mut frame, entry.target_id);
    push_optional(&mut frame, entry.detail_json);
    frame
}

/// Compute the lowercase HMAC-SHA256 chain hash for one entry.
#[must_use]
pub fn entry_hash(
    audit_hmac_key: &[u8],
    entry: &AuditEntry<'_>,
    seq: i64,
    prev_hash: &str,
) -> String {
    let frame = canonical_frame(entry, seq, prev_hash);
    let mut mac = Hmac::<Sha256>::new_from_slice(audit_hmac_key)
        .expect("HMAC-SHA256 accepts keys of any length");
    mac.update(&frame);
    hex_lower(&mac.finalize().into_bytes())
}

/// Append one row in the caller's transaction, chaining from the current tail.
///
/// # Errors
///
/// Returns [`AuditError::Sqlx`] when the tail cannot be read or the row cannot
/// be inserted.
pub async fn append(
    transaction: &mut Transaction<'_, Sqlite>,
    audit_hmac_key: &[u8],
    entry: AuditEntry<'_>,
) -> Result<(i64, String), AuditError> {
    let max_seq: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM audit_log")
        .fetch_one(&mut **transaction)
        .await?;
    let prev_hash = if max_seq == 0 {
        GENESIS_HASH.to_owned()
    } else {
        sqlx::query_scalar("SELECT hash FROM audit_log ORDER BY seq DESC LIMIT 1")
            .fetch_one(&mut **transaction)
            .await?
    };
    let seq = max_seq + 1;
    let hash = entry_hash(audit_hmac_key, &entry, seq, &prev_hash);

    sqlx::query(
        "INSERT INTO audit_log \
         (seq, id, ts, actor_user_id, ip_addr, action, target_type, target_id, \
          detail_json, prev_hash, hash) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(seq)
    .bind(entry.id)
    .bind(entry.ts)
    .bind(entry.actor_user_id)
    .bind(entry.ip_addr)
    .bind(entry.action)
    .bind(entry.target_type)
    .bind(entry.target_id)
    .bind(entry.detail_json)
    .bind(&prev_hash)
    .bind(&hash)
    .execute(&mut **transaction)
    .await?;

    Ok((seq, hash))
}

fn push_required(buffer: &mut Vec<u8>, value: &str) {
    let length = u32::try_from(value.len()).expect("audit field length fits u32");
    buffer.extend_from_slice(&length.to_be_bytes());
    buffer.extend_from_slice(value.as_bytes());
}

fn push_optional(buffer: &mut Vec<u8>, value: Option<&str>) {
    match value {
        None => buffer.push(0x00),
        Some(value) => {
            buffer.push(0x01);
            push_required(buffer, value);
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests;
