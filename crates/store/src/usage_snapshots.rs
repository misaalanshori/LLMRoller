//! Database repositories for upstream_usage_snapshots.
//!
//! Spec: Section 6.9, Section 9.1, Section 13.3, Requirements: USG-5, SEC-18.

use sqlx::{Sqlite, Transaction};

use crate::crypto::{SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_core::ids::UpstreamKeyId;

/// Parameters for inserting an upstream usage snapshot.
#[derive(Debug, Clone)]
pub struct NewUsageSnapshot<'a> {
    pub key_id: &'a UpstreamKeyId,
    pub now_ms: i64,
    pub http_status: i64,
    pub plan_name: Option<&'a str>,
    pub windows_json: &'a str,
    pub balance_micro_usd: Option<i64>,
    pub master_key_id: &'a str,
    pub sanitized_json: &'a str,
    pub raw_json_bytes: &'a [u8],
}

/// A usage snapshot sealed and serialized before entering the writer queue.
///
/// Sealing is CPU-only; the resulting row is inserted by `SqliteWriterActor`
/// so the poller and proxy never open a competing write transaction (F09).
#[derive(Debug, Clone)]
pub struct SealedUsageSnapshot {
    pub id: String,
    pub key_id: String,
    pub fetched_at: i64,
    pub http_status: i64,
    pub plan_name: Option<String>,
    pub windows_json: String,
    pub balance_micro_usd: Option<i64>,
    pub raw_json_enc: Vec<u8>,
    pub master_key_id: String,
    pub sanitized_json: String,
}

/// Seal a raw usage response and prepare it for the writer queue.
pub fn seal_usage_snapshot(
    sealer: &XChaCha20Poly1305Sealer,
    snapshot: &NewUsageSnapshot<'_>,
) -> SealedUsageSnapshot {
    let snapshot_id = format!("snap-{}", ulid::Ulid::generate());

    // Seal raw_json_enc under domain "usage_raw" with AAD [master_key_id, id] per SEC-18 and Section 9.2.2
    let raw_json_enc = sealer.seal(
        "usage_raw",
        &[snapshot.master_key_id.as_bytes(), snapshot_id.as_bytes()],
        snapshot.raw_json_bytes,
    );

    SealedUsageSnapshot {
        id: snapshot_id,
        key_id: snapshot.key_id.to_string(),
        fetched_at: snapshot.now_ms,
        http_status: snapshot.http_status,
        plan_name: snapshot.plan_name.map(str::to_string),
        windows_json: snapshot.windows_json.to_string(),
        balance_micro_usd: snapshot.balance_micro_usd,
        raw_json_enc,
        master_key_id: snapshot.master_key_id.to_string(),
        sanitized_json: snapshot.sanitized_json.to_string(),
    }
}

/// Inserts a sealed upstream usage snapshot (USG-5, SEC-18).
pub async fn insert_sealed_usage_snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
    snapshot: &SealedUsageSnapshot,
) -> Result<String, sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO upstream_usage_snapshots (
               id, upstream_key_id, fetched_at, http_status, plan_name,
               windows_json, balance_micro_usd, raw_json_enc, master_key_id, sanitized_json
           )
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&snapshot.id)
    .bind(&snapshot.key_id)
    .bind(snapshot.fetched_at)
    .bind(snapshot.http_status)
    .bind(&snapshot.plan_name)
    .bind(&snapshot.windows_json)
    .bind(snapshot.balance_micro_usd)
    .bind(&snapshot.raw_json_enc)
    .bind(&snapshot.master_key_id)
    .bind(&snapshot.sanitized_json)
    .execute(&mut **tx)
    .await?;

    Ok(snapshot.id.clone())
}

/// Inserts a sealed upstream usage snapshot (USG-5, SEC-18).
pub async fn insert_usage_snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
    sealer: &XChaCha20Poly1305Sealer,
    snapshot: NewUsageSnapshot<'_>,
) -> Result<String, sqlx::Error> {
    let sealed = seal_usage_snapshot(sealer, &snapshot);
    insert_sealed_usage_snapshot_tx(tx, &sealed).await
}
