#![forbid(unsafe_code)]
//! Offline resumable master-key rotation.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#922-canonical-aead-associated-data-aad-specification

use std::sync::Arc;

use llmroller_core::seams::{EntropySource, ExposeSecret, SecretBytes};
use sqlx::{Connection, Row, Sqlite, SqlitePool, Transaction};
use thiserror::Error;

use crate::audit::{AuditEntry, AuditError, append};
use crate::crypto::{CryptoError, MasterKey, SecretSealer, XChaCha20Poly1305Sealer};

/// Maximum rows re-sealed in one `BEGIN IMMEDIATE` transaction.
pub const ROTATION_BATCH_SIZE: usize = 500;

const INSTANCE_ID_LEN: usize = 16;
const SESSION_SECRET: &str = "session_hmac_secret";
const AUDIT_SECRET: &str = "audit_hmac_key";

const TABLES: [RotationTable; 6] = [
    RotationTable::InstanceSecrets,
    RotationTable::Users,
    RotationTable::UpstreamKeys,
    RotationTable::UsageSnapshots,
    RotationTable::TraceBlocks,
    RotationTable::TraceRecords,
];

/// Result counters for one completed or resumed rotation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationReport {
    /// Whether this invocation resumed an interrupted rotation.
    pub resumed: bool,
    /// Number of non-empty 500-row batches committed.
    pub batches: usize,
    /// Number of rows re-sealed under the new master key.
    pub rows_re_sealed: u64,
}

/// Master-key rotation failures with their unified CLI exit codes.
#[derive(Debug, Error)]
pub enum RotationError {
    /// SQLite failed while reading or writing rotated rows.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// The audit row could not be appended.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// AEAD encryption of a new envelope failed.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// The new key identifier was empty.
    #[error("new master key id must not be empty")]
    EmptyNewKeyId,
    /// The new key identifier equals the active identifier.
    #[error("new master key id {0:?} is already active")]
    SameKeyId(String),
    /// An interrupted rotation targets a different key.
    #[error("interrupted rotation targets {found:?}, not requested {requested:?}")]
    ResumeMismatch {
        /// Target stored in `instance_meta`.
        found: String,
        /// Target supplied on the command line.
        requested: String,
    },
    /// The instance metadata is absent.
    #[error("instance_meta is missing")]
    MissingInstanceMeta,
    /// The instance id is not 16 bytes.
    #[error("instance_meta.instance_id is corrupt")]
    InvalidInstanceId,
    /// Rotation bookkeeping is inconsistent.
    #[error("master-key rotation state is corrupt: {0}")]
    CorruptState(String),
    /// A sealed instance secret failed authentication.
    #[error("instance secret {name} failed authentication")]
    InstanceAuthenticatedFailure { name: String },
    /// `audit_hmac_key` could not be recovered.
    #[error("audit_hmac_key is missing from instance_secrets")]
    MissingAuditKey,
    /// A row was sealed under neither the old nor the new key.
    #[error("sealed row {table}/{id} uses unexpected master key {found:?}")]
    UnexpectedKeyId {
        /// Table being rotated.
        table: &'static str,
        /// Cursor key of the failed row.
        id: String,
        /// Master-key id stored on the row.
        found: String,
    },
    /// A row failed AEAD authentication under the expected old key.
    #[error("sealed row {table}/{id} failed authentication")]
    Authentication {
        /// Table being rotated.
        table: &'static str,
        /// Cursor key of the failed row.
        id: String,
    },
    /// Finalization found rows that still reference the old key.
    #[error("rotation incomplete: {rows} rows still use {key_id:?}")]
    Incomplete {
        /// Number of stale rows.
        rows: i64,
        /// Old master-key id.
        key_id: String,
    },
}

impl RotationError {
    /// Unified CLI exit code for this rotation failure.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Sqlx(_) | Self::Audit(_) => 13,
            Self::Crypto(_)
            | Self::UnexpectedKeyId { .. }
            | Self::Authentication { .. }
            | Self::Incomplete { .. } => 7,
            Self::EmptyNewKeyId | Self::SameKeyId(_) => 2,
            Self::ResumeMismatch { .. }
            | Self::MissingInstanceMeta
            | Self::InvalidInstanceId
            | Self::CorruptState(_)
            | Self::InstanceAuthenticatedFailure { .. }
            | Self::MissingAuditKey => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RotationTable {
    InstanceSecrets,
    Users,
    UpstreamKeys,
    UsageSnapshots,
    TraceBlocks,
    TraceRecords,
}

impl RotationTable {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InstanceSecrets => "instance_secrets",
            Self::Users => "users",
            Self::UpstreamKeys => "upstream_keys",
            Self::UsageSnapshots => "upstream_usage_snapshots",
            Self::TraceBlocks => "trace_blocks",
            Self::TraceRecords => "trace_records",
        }
    }

    const fn domain(self) -> &'static str {
        match self {
            Self::InstanceSecrets => "instance_secret",
            Self::Users => "totp_secret",
            Self::UpstreamKeys => "upstream_key",
            Self::UsageSnapshots => "usage_raw",
            Self::TraceBlocks => "trace_block",
            Self::TraceRecords => "trace_record",
        }
    }

    fn next(self) -> Option<Self> {
        TABLES
            .iter()
            .position(|candidate| *candidate == self)
            .and_then(|index| TABLES.get(index + 1).copied())
    }

    fn parse(value: &str) -> Result<Self, RotationError> {
        TABLES
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == value)
            .ok_or_else(|| RotationError::CorruptState(format!("unknown cursor table {value:?}")))
    }
}

#[derive(Debug)]
struct MetaState {
    instance_id: [u8; INSTANCE_ID_LEN],
    active_master_key_id: String,
    rotation_state: String,
    rotation_from_key_id: Option<String>,
    rotation_to_key_id: Option<String>,
    rotation_cursor_table: Option<String>,
    rotation_cursor_id: Option<String>,
}

type MetaRow = (
    Vec<u8>,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Debug)]
struct SealedRow {
    cursor_key: String,
    sealed: Vec<u8>,
    master_key_id: String,
    aad_fields: Vec<String>,
    target: RowTarget,
}

#[derive(Debug)]
enum RowTarget {
    InstanceSecret {
        name: String,
    },
    User {
        id: String,
    },
    UpstreamKey {
        id: String,
    },
    UsageSnapshot {
        id: String,
    },
    TraceBlock {
        owner_user_id: String,
        block_hash: String,
    },
    TraceRecord {
        usage_event_id: String,
    },
}

#[derive(Debug)]
struct BatchOutcome {
    row_count: usize,
    re_sealed: u64,
    last_cursor: Option<String>,
}

/// Re-seal every encrypted row under a replacement master key.
///
/// A new rotation starts by unsealing both instance secrets with the currently
/// active key, records `rotation_state = 'in_progress'`, then re-seals
/// `instance_secrets`, `users`, `upstream_keys`, `upstream_usage_snapshots`,
/// `trace_blocks`, and `trace_records` in 500-row `BEGIN IMMEDIATE` batches.
/// An interrupted run resumes at `rotation_cursor_table`/`rotation_cursor_id`.
/// Finalization appends `master_key.rotated` using the unchanged
/// `audit_hmac_key`, then marks the new key active.
///
/// # Errors
///
/// Returns a [`RotationError`] whose [`RotationError::exit_code`] follows the
/// unified CLI table. The function assumes the caller holds the exclusive
/// `llmroller.lock`; no other writer may mutate the database during rotation.
pub async fn rotate_master_key(
    pool: &SqlitePool,
    old_master_key: MasterKey,
    new_master_key: MasterKey,
    new_key_id: &str,
    entropy: Arc<dyn EntropySource>,
    now_ms: i64,
) -> Result<RotationReport, RotationError> {
    if new_key_id.is_empty() {
        return Err(RotationError::EmptyNewKeyId);
    }

    let meta = load_meta(pool).await?;
    if meta.active_master_key_id == new_key_id {
        return Err(RotationError::SameKeyId(new_key_id.to_owned()));
    }

    let resumed = meta.rotation_state == "in_progress";
    let (old_key_id, cursor_table, cursor_id) = if resumed {
        let found = meta
            .rotation_to_key_id
            .clone()
            .ok_or_else(|| RotationError::CorruptState("missing rotation_to_key_id".to_owned()))?;
        if found != new_key_id {
            return Err(RotationError::ResumeMismatch {
                found,
                requested: new_key_id.to_owned(),
            });
        }
        let old = meta.rotation_from_key_id.clone().ok_or_else(|| {
            RotationError::CorruptState("missing rotation_from_key_id".to_owned())
        })?;
        if old != meta.active_master_key_id {
            return Err(RotationError::CorruptState(
                "active_master_key_id differs from rotation_from_key_id".to_owned(),
            ));
        }
        let table = meta.rotation_cursor_table.as_deref().ok_or_else(|| {
            RotationError::CorruptState("missing rotation cursor table".to_owned())
        })?;
        (
            old,
            RotationTable::parse(table)?,
            meta.rotation_cursor_id.clone(),
        )
    } else if meta.rotation_state == "idle" {
        (
            meta.active_master_key_id.clone(),
            RotationTable::InstanceSecrets,
            None,
        )
    } else {
        return Err(RotationError::CorruptState(format!(
            "unknown rotation_state {:?}",
            meta.rotation_state
        )));
    };

    let old_sealer =
        XChaCha20Poly1305Sealer::new(old_master_key, meta.instance_id, Arc::clone(&entropy));
    let new_sealer =
        XChaCha20Poly1305Sealer::new(new_master_key, meta.instance_id, Arc::clone(&entropy));
    let audit_key = load_audit_key(
        pool,
        &old_sealer,
        &new_sealer,
        &old_key_id,
        new_key_id,
        resumed,
    )
    .await?;

    let mut connection = pool.acquire().await?;
    if !resumed {
        let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
        let updated = sqlx::query(
            "UPDATE instance_meta SET rotation_state = 'in_progress', \
             rotation_from_key_id = ?, rotation_to_key_id = ?, \
             rotation_cursor_table = 'instance_secrets', rotation_cursor_id = NULL, \
             updated_at = ? \
             WHERE singleton = 1 AND rotation_state = 'idle' AND active_master_key_id = ?",
        )
        .bind(&old_key_id)
        .bind(new_key_id)
        .bind(now_ms)
        .bind(&old_key_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if updated != 1 {
            return Err(RotationError::CorruptState(
                "could not start rotation from idle state".to_owned(),
            ));
        }
        transaction.commit().await?;
    }

    let mut current_table = cursor_table;
    let mut current_cursor = cursor_id;
    let mut report = RotationReport {
        resumed,
        batches: 0,
        rows_re_sealed: 0,
    };

    loop {
        let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
        let outcome = process_batch(
            &mut transaction,
            current_table,
            current_cursor.as_deref(),
            &old_sealer,
            &new_sealer,
            &old_key_id,
            new_key_id,
        )
        .await?;

        if outcome.row_count == 0 {
            if let Some(next_table) = current_table.next() {
                set_cursor(&mut transaction, next_table, None, now_ms).await?;
                transaction.commit().await?;
                current_table = next_table;
                current_cursor = None;
                continue;
            }

            finalize(
                &mut transaction,
                &audit_key,
                &old_key_id,
                new_key_id,
                now_ms,
            )
            .await?;
            transaction.commit().await?;
            break;
        }

        report.batches += 1;
        report.rows_re_sealed += outcome.re_sealed;
        current_cursor = outcome.last_cursor;
        set_cursor(
            &mut transaction,
            current_table,
            current_cursor.as_deref(),
            now_ms,
        )
        .await?;
        transaction.commit().await?;
    }

    drop(connection);
    Ok(report)
}

async fn load_meta(pool: &SqlitePool) -> Result<MetaState, RotationError> {
    let row: Option<MetaRow> = sqlx::query_as(
        "SELECT instance_id, active_master_key_id, rotation_state, \
         rotation_from_key_id, rotation_to_key_id, rotation_cursor_table, rotation_cursor_id \
         FROM instance_meta WHERE singleton = 1",
    )
    .fetch_optional(pool)
    .await?;
    let Some((
        instance_id,
        active_master_key_id,
        rotation_state,
        rotation_from_key_id,
        rotation_to_key_id,
        rotation_cursor_table,
        rotation_cursor_id,
    )) = row
    else {
        return Err(RotationError::MissingInstanceMeta);
    };
    let instance_id: [u8; INSTANCE_ID_LEN] = instance_id
        .as_slice()
        .try_into()
        .map_err(|_| RotationError::InvalidInstanceId)?;
    Ok(MetaState {
        instance_id,
        active_master_key_id,
        rotation_state,
        rotation_from_key_id,
        rotation_to_key_id,
        rotation_cursor_table,
        rotation_cursor_id,
    })
}

async fn load_audit_key(
    pool: &SqlitePool,
    old_sealer: &XChaCha20Poly1305Sealer,
    new_sealer: &XChaCha20Poly1305Sealer,
    old_key_id: &str,
    new_key_id: &str,
    resumed: bool,
) -> Result<SecretBytes, RotationError> {
    let rows: Vec<(String, Vec<u8>, String)> =
        sqlx::query_as("SELECT name, sealed, master_key_id FROM instance_secrets ORDER BY name")
            .fetch_all(pool)
            .await?;

    let mut audit_key = None;
    let mut saw_session = false;
    for (name, sealed, row_key_id) in rows {
        let selected: &dyn SecretSealer = if row_key_id == old_key_id {
            old_sealer
        } else if resumed && row_key_id == new_key_id {
            new_sealer
        } else {
            return Err(RotationError::CorruptState(format!(
                "instance secret {name} uses master key {row_key_id:?}"
            )));
        };
        let plain = selected
            .open(
                "instance_secret",
                &[row_key_id.as_bytes(), name.as_bytes()],
                &sealed,
            )
            .map_err(|_| RotationError::InstanceAuthenticatedFailure { name: name.clone() })?;
        if plain.expose_secret().len() != 32 {
            return Err(RotationError::CorruptState(format!(
                "instance secret {name} is not 32 bytes"
            )));
        }
        match name.as_str() {
            AUDIT_SECRET => audit_key = Some(plain),
            SESSION_SECRET => saw_session = true,
            _ => {
                return Err(RotationError::CorruptState(format!(
                    "unknown instance secret {name}"
                )));
            }
        }
    }

    if !saw_session {
        return Err(RotationError::CorruptState(
            "session_hmac_secret is missing".to_owned(),
        ));
    }
    audit_key.ok_or(RotationError::MissingAuditKey)
}

async fn process_batch(
    transaction: &mut Transaction<'_, Sqlite>,
    table: RotationTable,
    cursor: Option<&str>,
    old_sealer: &XChaCha20Poly1305Sealer,
    new_sealer: &XChaCha20Poly1305Sealer,
    old_key_id: &str,
    new_key_id: &str,
) -> Result<BatchOutcome, RotationError> {
    let rows = load_rows(transaction, table, cursor, ROTATION_BATCH_SIZE).await?;
    let mut re_sealed = 0_u64;

    for row in &rows {
        if row.master_key_id == new_key_id {
            continue;
        }
        if row.master_key_id != old_key_id {
            return Err(RotationError::UnexpectedKeyId {
                table: table.as_str(),
                id: row.cursor_key.clone(),
                found: row.master_key_id.clone(),
            });
        }

        let old_fields = aad_fields(&row.master_key_id, &row.aad_fields);
        let old_refs: Vec<&[u8]> = old_fields.iter().map(Vec::as_slice).collect();
        let plain = old_sealer
            .open(table.domain(), &old_refs, &row.sealed)
            .map_err(|_| RotationError::Authentication {
                table: table.as_str(),
                id: row.cursor_key.clone(),
            })?;

        let new_fields = aad_fields(new_key_id, &row.aad_fields);
        let new_refs: Vec<&[u8]> = new_fields.iter().map(Vec::as_slice).collect();
        let sealed = new_sealer.try_seal(table.domain(), &new_refs, plain.expose_secret())?;
        update_row(transaction, &row.target, new_key_id, &sealed).await?;
        re_sealed += 1;
    }

    Ok(BatchOutcome {
        row_count: rows.len(),
        re_sealed,
        last_cursor: rows.last().map(|row| row.cursor_key.clone()),
    })
}

async fn load_rows(
    transaction: &mut Transaction<'_, Sqlite>,
    table: RotationTable,
    cursor: Option<&str>,
    limit: usize,
) -> Result<Vec<SealedRow>, RotationError> {
    let limit = i64::try_from(limit).expect("rotation batch size fits i64");
    let cursor = cursor.unwrap_or_default();

    match table {
        RotationTable::InstanceSecrets => {
            let rows = sqlx::query(
                "SELECT name, sealed, master_key_id FROM instance_secrets \
                 WHERE name > ? ORDER BY name LIMIT ?",
            )
            .bind(cursor)
            .bind(limit)
            .fetch_all(&mut **transaction)
            .await?;
            rows.into_iter()
                .map(|row| {
                    let name: String = row.try_get("name")?;
                    Ok(SealedRow {
                        cursor_key: name.clone(),
                        sealed: row.try_get("sealed")?,
                        master_key_id: row.try_get("master_key_id")?,
                        aad_fields: vec![name.clone()],
                        target: RowTarget::InstanceSecret { name },
                    })
                })
                .collect()
        }
        RotationTable::Users => {
            let rows = sqlx::query(
                "SELECT id, totp_secret_enc, totp_master_key_id FROM users \
                 WHERE totp_secret_enc IS NOT NULL AND id > ? ORDER BY id LIMIT ?",
            )
            .bind(cursor)
            .bind(limit)
            .fetch_all(&mut **transaction)
            .await?;
            rows.into_iter()
                .map(|row| {
                    let id: String = row.try_get("id")?;
                    let master_key_id: Option<String> = row.try_get("totp_master_key_id")?;
                    let master_key_id = master_key_id.ok_or_else(|| {
                        RotationError::CorruptState(format!(
                            "users/{id} has a TOTP secret without a master-key id"
                        ))
                    })?;
                    Ok(SealedRow {
                        cursor_key: id.clone(),
                        sealed: row.try_get("totp_secret_enc")?,
                        master_key_id,
                        aad_fields: vec![id.clone()],
                        target: RowTarget::User { id },
                    })
                })
                .collect()
        }
        RotationTable::UpstreamKeys => {
            let rows = sqlx::query(
                "SELECT id, provider_id, secret_enc, master_key_id FROM upstream_keys \
                 WHERE id > ? ORDER BY id LIMIT ?",
            )
            .bind(cursor)
            .bind(limit)
            .fetch_all(&mut **transaction)
            .await?;
            rows.into_iter()
                .map(|row| {
                    let id: String = row.try_get("id")?;
                    let provider_id: String = row.try_get("provider_id")?;
                    Ok(SealedRow {
                        cursor_key: id.clone(),
                        sealed: row.try_get("secret_enc")?,
                        master_key_id: row.try_get("master_key_id")?,
                        aad_fields: vec![id.clone(), provider_id],
                        target: RowTarget::UpstreamKey { id },
                    })
                })
                .collect()
        }
        RotationTable::UsageSnapshots => {
            let rows = sqlx::query(
                "SELECT id, raw_json_enc, master_key_id FROM upstream_usage_snapshots \
                 WHERE id > ? ORDER BY id LIMIT ?",
            )
            .bind(cursor)
            .bind(limit)
            .fetch_all(&mut **transaction)
            .await?;
            rows.into_iter()
                .map(|row| {
                    let id: String = row.try_get("id")?;
                    Ok(SealedRow {
                        cursor_key: id.clone(),
                        sealed: row.try_get("raw_json_enc")?,
                        master_key_id: row.try_get("master_key_id")?,
                        aad_fields: vec![id.clone()],
                        target: RowTarget::UsageSnapshot { id },
                    })
                })
                .collect()
        }
        RotationTable::TraceBlocks => {
            let (owner_cursor, block_cursor) = match cursor.split_once('|') {
                Some((owner, block)) => (owner, block),
                None if cursor.is_empty() => ("", ""),
                None => {
                    return Err(RotationError::CorruptState(format!(
                        "malformed trace_blocks cursor {cursor:?}"
                    )));
                }
            };
            let rows = sqlx::query(
                "SELECT owner_user_id, block_hash, payload_enc, master_key_id \
                 FROM trace_blocks \
                 WHERE owner_user_id > ? OR (owner_user_id = ? AND block_hash > ?) \
                 ORDER BY owner_user_id, block_hash LIMIT ?",
            )
            .bind(owner_cursor)
            .bind(owner_cursor)
            .bind(block_cursor)
            .bind(limit)
            .fetch_all(&mut **transaction)
            .await?;
            rows.into_iter()
                .map(|row| {
                    let owner_user_id: String = row.try_get("owner_user_id")?;
                    let block_hash: String = row.try_get("block_hash")?;
                    Ok(SealedRow {
                        cursor_key: format!("{owner_user_id}|{block_hash}"),
                        sealed: row.try_get("payload_enc")?,
                        master_key_id: row.try_get("master_key_id")?,
                        aad_fields: vec![owner_user_id.clone(), block_hash.clone()],
                        target: RowTarget::TraceBlock {
                            owner_user_id,
                            block_hash,
                        },
                    })
                })
                .collect()
        }
        RotationTable::TraceRecords => {
            let rows = sqlx::query(
                "SELECT usage_event_id, request_id, owner_user_id, session_hash, \
                 payload_enc, master_key_id FROM trace_records \
                 WHERE usage_event_id > ? ORDER BY usage_event_id LIMIT ?",
            )
            .bind(cursor)
            .bind(limit)
            .fetch_all(&mut **transaction)
            .await?;
            rows.into_iter()
                .map(|row| {
                    let usage_event_id: String = row.try_get("usage_event_id")?;
                    let request_id: String = row.try_get("request_id")?;
                    let owner_user_id: String = row.try_get("owner_user_id")?;
                    let session_hash: String = row.try_get("session_hash")?;
                    Ok(SealedRow {
                        cursor_key: usage_event_id.clone(),
                        sealed: row.try_get("payload_enc")?,
                        master_key_id: row.try_get("master_key_id")?,
                        aad_fields: vec![request_id, owner_user_id, session_hash],
                        target: RowTarget::TraceRecord { usage_event_id },
                    })
                })
                .collect()
        }
    }
}

async fn update_row(
    transaction: &mut Transaction<'_, Sqlite>,
    target: &RowTarget,
    new_key_id: &str,
    sealed: &[u8],
) -> Result<(), RotationError> {
    let affected = match target {
        RowTarget::InstanceSecret { name } => {
            sqlx::query("UPDATE instance_secrets SET sealed = ?, master_key_id = ? WHERE name = ?")
                .bind(sealed)
                .bind(new_key_id)
                .bind(name)
                .execute(&mut **transaction)
                .await?
                .rows_affected()
        }
        RowTarget::User { id } => {
            sqlx::query("UPDATE users SET totp_secret_enc = ?, totp_master_key_id = ? WHERE id = ?")
                .bind(sealed)
                .bind(new_key_id)
                .bind(id)
                .execute(&mut **transaction)
                .await?
                .rows_affected()
        }
        RowTarget::UpstreamKey { id } => {
            sqlx::query("UPDATE upstream_keys SET secret_enc = ?, master_key_id = ? WHERE id = ?")
                .bind(sealed)
                .bind(new_key_id)
                .bind(id)
                .execute(&mut **transaction)
                .await?
                .rows_affected()
        }
        RowTarget::UsageSnapshot { id } => sqlx::query(
            "UPDATE upstream_usage_snapshots SET raw_json_enc = ?, master_key_id = ? WHERE id = ?",
        )
        .bind(sealed)
        .bind(new_key_id)
        .bind(id)
        .execute(&mut **transaction)
        .await?
        .rows_affected(),
        RowTarget::TraceBlock {
            owner_user_id,
            block_hash,
        } => sqlx::query(
            "UPDATE trace_blocks SET payload_enc = ?, master_key_id = ? \
             WHERE owner_user_id = ? AND block_hash = ?",
        )
        .bind(sealed)
        .bind(new_key_id)
        .bind(owner_user_id)
        .bind(block_hash)
        .execute(&mut **transaction)
        .await?
        .rows_affected(),
        RowTarget::TraceRecord { usage_event_id } => sqlx::query(
            "UPDATE trace_records SET payload_enc = ?, master_key_id = ? \
             WHERE usage_event_id = ?",
        )
        .bind(sealed)
        .bind(new_key_id)
        .bind(usage_event_id)
        .execute(&mut **transaction)
        .await?
        .rows_affected(),
    };

    if affected != 1 {
        return Err(RotationError::CorruptState(format!(
            "expected one row update for {target:?}, got {affected}"
        )));
    }
    Ok(())
}

async fn set_cursor(
    transaction: &mut Transaction<'_, Sqlite>,
    table: RotationTable,
    cursor_id: Option<&str>,
    now_ms: i64,
) -> Result<(), RotationError> {
    sqlx::query(
        "UPDATE instance_meta SET rotation_cursor_table = ?, rotation_cursor_id = ?, \
         updated_at = ? WHERE singleton = 1",
    )
    .bind(table.as_str())
    .bind(cursor_id)
    .bind(now_ms)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn finalize(
    transaction: &mut Transaction<'_, Sqlite>,
    audit_key: &SecretBytes,
    old_key_id: &str,
    new_key_id: &str,
    now_ms: i64,
) -> Result<(), RotationError> {
    let stale = count_stale_rows(transaction, old_key_id).await?;
    if stale != 0 {
        return Err(RotationError::Incomplete {
            rows: stale,
            key_id: old_key_id.to_owned(),
        });
    }

    let detail_json = serde_json::json!({
        "from_master_key_id": old_key_id,
        "to_master_key_id": new_key_id,
    })
    .to_string();
    let audit_id = ulid::Ulid::generate().to_string();
    append(
        transaction,
        audit_key.expose_secret(),
        AuditEntry {
            id: &audit_id,
            ts: now_ms,
            actor_user_id: None,
            ip_addr: None,
            action: "master_key.rotated",
            target_type: "instance_meta",
            target_id: Some(new_key_id),
            detail_json: Some(&detail_json),
        },
    )
    .await?;

    let updated = sqlx::query(
        "UPDATE instance_meta SET active_master_key_id = ?, rotation_state = 'idle', \
         rotation_from_key_id = NULL, rotation_to_key_id = NULL, \
         rotation_cursor_table = NULL, rotation_cursor_id = NULL, updated_at = ? \
         WHERE singleton = 1",
    )
    .bind(new_key_id)
    .bind(now_ms)
    .execute(&mut **transaction)
    .await?
    .rows_affected();
    if updated != 1 {
        return Err(RotationError::CorruptState(
            "could not finalize instance_meta".to_owned(),
        ));
    }
    Ok(())
}

async fn count_stale_rows(
    transaction: &mut Transaction<'_, Sqlite>,
    old_key_id: &str,
) -> Result<i64, RotationError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT \
         (SELECT COUNT(*) FROM instance_secrets WHERE master_key_id = ?) + \
         (SELECT COUNT(*) FROM users WHERE totp_master_key_id = ?) + \
         (SELECT COUNT(*) FROM upstream_keys WHERE master_key_id = ?) + \
         (SELECT COUNT(*) FROM upstream_usage_snapshots WHERE master_key_id = ?) + \
         (SELECT COUNT(*) FROM trace_blocks WHERE master_key_id = ?) + \
         (SELECT COUNT(*) FROM trace_records WHERE master_key_id = ?)",
    )
    .bind(old_key_id)
    .bind(old_key_id)
    .bind(old_key_id)
    .bind(old_key_id)
    .bind(old_key_id)
    .bind(old_key_id)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(count)
}

fn aad_fields(key_id: &str, suffix: &[String]) -> Vec<Vec<u8>> {
    let mut fields = Vec::with_capacity(1 + suffix.len());
    fields.push(key_id.as_bytes().to_vec());
    fields.extend(suffix.iter().map(|field| field.as_bytes().to_vec()));
    fields
}

#[cfg(test)]
mod tests;
