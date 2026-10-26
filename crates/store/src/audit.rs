#![forbid(unsafe_code)]
//! Canonical `audit_log` hash-chain serialization.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#921-canonical-audit_log-hash-chain-serialization
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::fmt::Write as _;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::{Row as _, Sqlite, SqlitePool, Transaction};
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

/// Format an `audit_anchor` structured JSON log line per `AUD-1` and `SEC-13`.
#[must_use]
pub fn format_audit_anchor(seq: i64, hash: &str, ts: i64) -> String {
    format!("{{\"event\":\"audit_anchor\",\"seq\":{seq},\"hash\":\"{hash}\",\"ts\":{ts}}}")
}

/// Audit verification failures with deterministic exit codes.
#[derive(Debug, Error)]
pub enum AuditVerificationError {
    /// SQLite query failure.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// A sequence number gap was found, or genesis hash was invalid.
    #[error(
        "sequence gap or invalid link detected at seq {seq}: expected {expected_seq}, found {found_seq}"
    )]
    SequenceGap {
        seq: i64,
        expected_seq: i64,
        found_seq: i64,
    },
    /// An HMAC hash mismatch was detected (tampering).
    #[error(
        "audit record tampering detected at seq {seq}: stored hash {stored_hash} does not match computed hash {computed_hash}"
    )]
    HashMismatch {
        seq: i64,
        stored_hash: String,
        computed_hash: String,
    },
    /// Previous hash linkage mismatch.
    #[error(
        "hash chain broken at seq {seq}: prev_hash {prev_hash} does not match predecessor hash {expected_prev_hash}"
    )]
    ChainBroken {
        seq: i64,
        prev_hash: String,
        expected_prev_hash: String,
    },
    /// Audit log tail truncation detected against anchor parameters.
    #[error(
        "audit log tail truncation detected: anchor requires seq {anchor_seq} with hash {anchor_hash}, but log ends at seq {max_seq}"
    )]
    TruncationDetected {
        anchor_seq: i64,
        anchor_hash: String,
        max_seq: i64,
    },
    /// Anchor hash mismatch.
    #[error(
        "anchor hash mismatch at seq {anchor_seq}: expected {expected_hash}, found {found_hash}"
    )]
    AnchorHashMismatch {
        anchor_seq: i64,
        expected_hash: String,
        found_hash: String,
    },
}

impl AuditVerificationError {
    /// Process exit code required by SRDD Section 5.5.1.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::SequenceGap { .. } | Self::HashMismatch { .. } | Self::ChainBroken { .. } => 8,
            Self::TruncationDetected { .. } | Self::AnchorHashMismatch { .. } => 9,
            Self::Sqlx(_) => 13,
        }
    }
}

/// Verification report summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditVerificationReport {
    pub rows_verified: usize,
    pub max_seq: i64,
    pub tail_hash: String,
}

/// Verify the entire `audit_log` HMAC chain ordered by `seq ASC` per `AUD-1` and `SEC-13`.
///
/// # Errors
///
/// Returns exit code 8 on sequence gap, broken chain, or HMAC mismatch;
/// returns exit code 9 on tail truncation against `--anchor-seq` and `--anchor-hash`;
/// returns exit code 13 on SQLite failures.
pub async fn verify_audit_log(
    pool: &SqlitePool,
    audit_hmac_key: &[u8],
    anchor_seq: Option<i64>,
    anchor_hash: Option<&str>,
) -> Result<AuditVerificationReport, AuditVerificationError> {
    let mut expected_seq = 1_i64;
    let mut expected_prev_hash = GENESIS_HASH.to_owned();
    let mut tail_hash = GENESIS_HASH.to_owned();
    let mut anchor_found = false;
    let mut last_seq = 0_i64;
    let mut total_verified = 0_usize;

    loop {
        let rows = sqlx::query(
            "SELECT seq, id, ts, actor_user_id, ip_addr, action, target_type, target_id, detail_json, prev_hash, hash
             FROM audit_log WHERE seq > ? ORDER BY seq ASC LIMIT 1000",
        )
        .bind(last_seq)
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            total_verified += 1;
            let seq: i64 = row.try_get("seq")?;
            if seq != expected_seq {
                return Err(AuditVerificationError::SequenceGap {
                    seq,
                    expected_seq,
                    found_seq: seq,
                });
            }

            let id: String = row.try_get("id")?;
            let ts: i64 = row.try_get("ts")?;
            let actor_user_id: Option<String> = row.try_get("actor_user_id")?;
            let ip_addr: Option<String> = row.try_get("ip_addr")?;
            let action: String = row.try_get("action")?;
            let target_type: String = row.try_get("target_type")?;
            let target_id: Option<String> = row.try_get("target_id")?;
            let detail_json: Option<String> = row.try_get("detail_json")?;
            let prev_hash: String = row.try_get("prev_hash")?;
            let stored_hash: String = row.try_get("hash")?;

            if prev_hash != expected_prev_hash {
                return Err(AuditVerificationError::ChainBroken {
                    seq,
                    prev_hash,
                    expected_prev_hash,
                });
            }

            let entry = AuditEntry {
                id: &id,
                ts,
                actor_user_id: actor_user_id.as_deref(),
                ip_addr: ip_addr.as_deref(),
                action: &action,
                target_type: &target_type,
                target_id: target_id.as_deref(),
                detail_json: detail_json.as_deref(),
            };

            let computed_hash = entry_hash(audit_hmac_key, &entry, seq, &prev_hash);
            if computed_hash != stored_hash {
                return Err(AuditVerificationError::HashMismatch {
                    seq,
                    stored_hash,
                    computed_hash,
                });
            }

            if let (Some(a_seq), Some(a_hash)) = (anchor_seq, anchor_hash)
                && seq == a_seq
            {
                if stored_hash != a_hash {
                    return Err(AuditVerificationError::AnchorHashMismatch {
                        anchor_seq: a_seq,
                        expected_hash: a_hash.to_owned(),
                        found_hash: stored_hash,
                    });
                }
                anchor_found = true;
            }

            last_seq = seq;
            expected_prev_hash = stored_hash.clone();
            tail_hash = stored_hash;
            expected_seq += 1;
        }
    }

    let max_seq = expected_seq - 1;
    if let (Some(a_seq), Some(a_hash)) = (anchor_seq, anchor_hash)
        && (!anchor_found || max_seq < a_seq)
    {
        return Err(AuditVerificationError::TruncationDetected {
            anchor_seq: a_seq,
            anchor_hash: a_hash.to_owned(),
            max_seq,
        });
    }

    Ok(AuditVerificationReport {
        rows_verified: total_verified,
        max_seq,
        tail_hash,
    })
}
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

    if seq % 100 == 0 {
        let anchor_json = format_audit_anchor(seq, &hash, entry.ts);
        tracing::info!(
            target: "llmroller::audit_anchor",
            seq = seq,
            entry_hash = %hash,
            ts = entry.ts,
            anchor = %anchor_json,
            "audit log hash chain anchor"
        );
    }

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
