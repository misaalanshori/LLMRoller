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

/// Inserts a sealed upstream usage snapshot (USG-5, SEC-18).
pub async fn insert_usage_snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
    sealer: &XChaCha20Poly1305Sealer,
    snapshot: NewUsageSnapshot<'_>,
) -> Result<String, sqlx::Error> {
    let snapshot_id = format!("snap-{}", ulid::Ulid::generate());
    let key_str = snapshot.key_id.to_string();

    // Seal raw_json_enc under domain "usage_raw" with AAD [key_id] per SEC-18
    let raw_json_enc = sealer.seal("usage_raw", &[key_str.as_bytes()], snapshot.raw_json_bytes);

    sqlx::query(
        r#"INSERT INTO upstream_usage_snapshots (
               id, upstream_key_id, fetched_at, http_status, plan_name,
               windows_json, balance_micro_usd, raw_json_enc, master_key_id, sanitized_json
           )
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&snapshot_id)
    .bind(&key_str)
    .bind(snapshot.now_ms)
    .bind(snapshot.http_status)
    .bind(snapshot.plan_name)
    .bind(snapshot.windows_json)
    .bind(snapshot.balance_micro_usd)
    .bind(&raw_json_enc)
    .bind(snapshot.master_key_id)
    .bind(snapshot.sanitized_json)
    .execute(&mut **tx)
    .await?;

    Ok(snapshot_id)
}
