//! Content-addressed trace_blocks, StoredTurnTrace v2, sealer pool, and reference-aware trace purge.
//!
//! Spec: Section 6.10, Section 7.1, Section 8.2, Section 9.1, Section 9.2,
//! Requirements: TRC-4, TRC-7, PERF-5, SEC-19.

use sqlx::{Sqlite, Transaction};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

use crate::crypto::{SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_core::ids::{ClientKeyId, RequestId, UserId};

/// Stored turn trace v2 contract (Section 12.6).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredTurnTraceV2 {
    pub v: u32,
    pub request_skeleton: serde_json::Value,
    pub block_refs: Vec<String>,
    pub response: serde_json::Value,
    #[serde(default)]
    pub response_raw: Option<serde_json::Value>,
    #[serde(default)]
    pub truncation: Option<String>,
}

/// Job submitted to the 2-thread sealer pool.
#[derive(Debug, Clone)]
pub struct SealJob {
    pub usage_event_id: String,
    pub request_id: RequestId,
    pub ts: i64,
    pub owner_user_id: UserId,
    pub client_key_id: ClientKeyId,
    pub session_hash: String,
    pub thread_id: String,
    pub canonical_id: String,
    pub protocol: String,
    pub truncated_content: bool,
    pub turn_trace: StoredTurnTraceV2,
    pub blocks: Vec<(String, Vec<u8>)>, // (block_hash, canonical_block_bytes)
    pub expires_at: i64,
}

/// Output of a completed seal job.
#[derive(Debug)]
pub struct SealedTurnResult {
    pub usage_event_id: String,
    pub request_id: RequestId,
    pub ts: i64,
    pub owner_user_id: UserId,
    pub client_key_id: ClientKeyId,
    pub session_hash: String,
    pub thread_id: String,
    pub canonical_id: String,
    pub truncated_content: bool,
    pub payload_enc: Vec<u8>,
    pub sealed_blocks: Vec<(String, Vec<u8>, usize)>, // (hash, payload_enc, byte_len)
    pub block_refs: Vec<String>,
    pub expires_at: i64,
}

/// Seals a single turn synchronously using zstd compression and XChaCha20-Poly1305 (SEC-19).
pub fn seal_turn_sync(
    job: &SealJob,
    sealer: &XChaCha20Poly1305Sealer,
) -> Result<SealedTurnResult, String> {
    let owner_bytes = job.owner_user_id.to_string();
    let req_bytes = job.request_id.to_string();

    let mut sealed_blocks = Vec::new();
    for (hash, block_bytes) in &job.blocks {
        let compressed = zstd::bulk::compress(block_bytes, 3)
            .map_err(|e| format!("zstd block compression failed: {e}"))?;
        let payload_enc = sealer.seal(
            "trace_block",
            &[owner_bytes.as_bytes(), hash.as_bytes()],
            &compressed,
        );
        sealed_blocks.push((hash.clone(), payload_enc, block_bytes.len()));
    }

    let trace_json = serde_json::to_vec(&job.turn_trace)
        .map_err(|e| format!("serialize StoredTurnTraceV2 failed: {e}"))?;
    let compressed_trace = zstd::bulk::compress(&trace_json, 3)
        .map_err(|e| format!("zstd trace compression failed: {e}"))?;
    let payload_enc = sealer.seal(
        "trace_record",
        &[owner_bytes.as_bytes(), req_bytes.as_bytes()],
        &compressed_trace,
    );

    Ok(SealedTurnResult {
        usage_event_id: job.usage_event_id.clone(),
        request_id: job.request_id,
        ts: job.ts,
        owner_user_id: job.owner_user_id,
        client_key_id: job.client_key_id,
        session_hash: job.session_hash.clone(),
        thread_id: job.thread_id.clone(),
        canonical_id: job.canonical_id.clone(),
        truncated_content: job.truncated_content,
        payload_enc,
        sealed_blocks,
        block_refs: job.turn_trace.block_refs.clone(),
        expires_at: job.expires_at,
    })
}

/// Dedicated 2-thread OS sealer pool with bounded 64-item channel (PERF-5).
pub struct SealerPool {
    tx: SyncSender<SealJob>,
    pub dropped_jobs_count: AtomicUsize,
}

impl std::fmt::Debug for SealerPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealerPool")
            .field(
                "dropped_jobs_count",
                &self.dropped_jobs_count.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl SealerPool {
    pub fn new(_sealer: Arc<XChaCha20Poly1305Sealer>) -> (Self, Receiver<SealJob>) {
        let (tx, rx) = sync_channel(64);
        (
            Self {
                tx,
                dropped_jobs_count: AtomicUsize::new(0),
            },
            rx,
        )
    }

    /// Submits a seal job using non-blocking try_send (never delaying inference) (PERF-5).
    pub fn try_submit(&self, job: SealJob) -> bool {
        match self.tx.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped_jobs_count.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

/// Persists a sealed turn and its content-addressed blocks into SQLite (TRC-4).
pub async fn insert_sealed_trace_tx(
    tx: &mut Transaction<'_, Sqlite>,
    res: &SealedTurnResult,
    master_key_id: &str,
) -> Result<(), sqlx::Error> {
    let owner_str = res.owner_user_id.to_string();

    // 1. Insert content-addressed blocks (TRC-4)
    for (hash, payload_enc, byte_len) in &res.sealed_blocks {
        sqlx::query(
            r#"INSERT INTO trace_blocks (
                   block_hash, owner_user_id, payload_enc, master_key_id, byte_len, last_referenced_at
               )
               VALUES (?, ?, ?, ?, ?, ?)
               ON CONFLICT (owner_user_id, block_hash) DO UPDATE SET
                   last_referenced_at = excluded.last_referenced_at"#,
        )
        .bind(hash)
        .bind(&owner_str)
        .bind(payload_enc)
        .bind(master_key_id)
        .bind(*byte_len as i64)
        .bind(res.ts)
        .execute(&mut **tx)
        .await?;
    }

    // 2. Insert trace record
    sqlx::query(
        r#"INSERT INTO trace_records (
               usage_event_id, request_id, ts, owner_user_id, client_key_id,
               session_hash, thread_id, canonical_id, protocol, truncated_content,
               payload_enc, master_key_id, expires_at
           )
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'chat', ?, ?, ?, ?)"#,
    )
    .bind(&res.usage_event_id)
    .bind(res.request_id.to_string())
    .bind(res.ts)
    .bind(&owner_str)
    .bind(res.client_key_id.to_string())
    .bind(&res.session_hash)
    .bind(&res.thread_id)
    .bind(&res.canonical_id)
    .bind(if res.truncated_content { 1 } else { 0 })
    .bind(&res.payload_enc)
    .bind(master_key_id)
    .bind(res.expires_at)
    .execute(&mut **tx)
    .await?;

    // 3. Insert block references into trace_record_blocks
    for hash in &res.block_refs {
        sqlx::query(
            r#"INSERT INTO trace_record_blocks (usage_event_id, owner_user_id, block_hash)
               VALUES (?, ?, ?)
               ON CONFLICT (usage_event_id, block_hash) DO NOTHING"#,
        )
        .bind(&res.usage_event_id)
        .bind(&owner_str)
        .bind(hash)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

/// Reference-aware session trace purge deleting trace_records and unreferenced trace_blocks (TRC-7).
pub async fn delete_session_trace_tx(
    tx: &mut Transaction<'_, Sqlite>,
    owner_user_id: &UserId,
    session_hash: &str,
) -> Result<(u64, u64), sqlx::Error> {
    let owner_str = owner_user_id.to_string();

    // 1. Delete trace_records for this session (cascades to trace_record_blocks)
    let records_deleted =
        sqlx::query("DELETE FROM trace_records WHERE owner_user_id = ? AND session_hash = ?")
            .bind(&owner_str)
            .bind(session_hash)
            .execute(&mut **tx)
            .await?
            .rows_affected();

    // 2. Delete unreferenced trace_blocks for this owner (preserves shared blocks!) (TRC-7)
    let blocks_deleted = sqlx::query(
        r#"DELETE FROM trace_blocks
           WHERE owner_user_id = ? AND block_hash NOT IN (
               SELECT block_hash FROM trace_record_blocks WHERE owner_user_id = ?
           )"#,
    )
    .bind(&owner_str)
    .bind(&owner_str)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    Ok((records_deleted, blocks_deleted))
}

#[cfg(test)]
pub mod tests;
