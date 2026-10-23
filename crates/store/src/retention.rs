//! Portable ULID-range retention sweeper and incremental vacuum supervisor.
//!
//! Spec: Section 3.3 (A6), Section 9.2, Section 21.6.3, Requirements: REL-3, Partial: PERF-4.

use sqlx::{Sqlite, SqlitePool, Transaction};

/// Parameters governing data retention sweeping.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    pub usage_events_retention_days: i64,
    pub usage_events_max_rows: i64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            usage_events_retention_days: 30,
            usage_events_max_rows: 50_000_000,
        }
    }
}

/// Deletes expired usage_events in bounded chunks using portable subqueries (REL-3).
pub async fn sweep_expired_usage_events_chunk(
    tx: &mut Transaction<'_, Sqlite>,
    cutoff_id: &str,
    chunk_limit: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"DELETE FROM usage_events WHERE id IN (
               SELECT id FROM usage_events WHERE id < ? ORDER BY id LIMIT ?
           )"#,
    )
    .bind(cutoff_id)
    .bind(chunk_limit)
    .execute(&mut **tx)
    .await?;

    Ok(result.rows_affected())
}

/// Prunes oldest usage_events rows when total rows exceed usage_events_max_rows (A6).
pub async fn enforce_max_usage_events_rows(
    tx: &mut Transaction<'_, Sqlite>,
    max_rows: i64,
    chunk_limit: i64,
) -> Result<u64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&mut **tx)
        .await?;

    if count <= max_rows {
        return Ok(0);
    }

    let excess = (count - max_rows).min(chunk_limit);
    let result = sqlx::query(
        r#"DELETE FROM usage_events WHERE id IN (
               SELECT id FROM usage_events ORDER BY id ASC LIMIT ?
           )"#,
    )
    .bind(excess)
    .execute(&mut **tx)
    .await?;

    Ok(result.rows_affected())
}

/// Deletes expired web_sessions rows.
pub async fn sweep_expired_web_sessions(
    tx: &mut Transaction<'_, Sqlite>,
    now_ms: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM web_sessions WHERE expires_at <= ?")
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}

/// Deletes expired response_links rows.
pub async fn sweep_expired_response_links(
    tx: &mut Transaction<'_, Sqlite>,
    now_ms: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM response_links WHERE expires_at <= ?")
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}

/// Deletes usage_events older than `cutoff_ms` in bounded chunks by timestamp.
pub async fn sweep_expired_usage_events(
    pool: &SqlitePool,
    cutoff_ms: i64,
    chunk_limit: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM usage_events WHERE id IN (
             SELECT id FROM usage_events WHERE ts < ? ORDER BY ts LIMIT ?
         )",
    )
    .bind(cutoff_ms)
    .bind(chunk_limit)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Prunes the oldest usage_events rows when the table exceeds the configured cap.
pub async fn enforce_max_usage_events_rows_pool(
    pool: &SqlitePool,
    max_rows: i64,
    chunk_limit: i64,
) -> Result<u64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(pool)
        .await?;
    if count <= max_rows {
        return Ok(0);
    }
    let excess = (count - max_rows).min(chunk_limit);
    let result = sqlx::query(
        "DELETE FROM usage_events WHERE id IN (
             SELECT id FROM usage_events ORDER BY id ASC LIMIT ?
         )",
    )
    .bind(excess)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes expired routing pins.
pub async fn sweep_expired_pins(pool: &SqlitePool, now_ms: i64) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM pins WHERE expires_at <= ?")
        .bind(now_ms)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Deletes expired sealed trace records.
pub async fn sweep_expired_trace_records(
    pool: &SqlitePool,
    now_ms: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM trace_records WHERE expires_at <= ?")
        .bind(now_ms)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Executes incremental vacuum to reclaim freed database pages.
pub async fn run_incremental_vacuum(pool: &SqlitePool, pages: u32) -> Result<(), sqlx::Error> {
    let query_str = format!("PRAGMA incremental_vacuum({pages});");
    sqlx::query(sqlx::AssertSqlSafe(query_str))
        .execute(pool)
        .await?;
    Ok(())
}

/// Deletes 5-minute usage rollup buckets older than `cutoff_ms` in bounded
/// chunks (REL-3). Rollups are derived data, so they are swept on their own
/// schedule independent of `usage_events` row pruning.
pub async fn sweep_expired_usage_rollups_5min(
    pool: &SqlitePool,
    cutoff_ms: i64,
    chunk_limit: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM usage_rollups_5min WHERE rowid IN (
             SELECT rowid FROM usage_rollups_5min WHERE bucket_ms < ? ORDER BY bucket_ms LIMIT ?
         )",
    )
    .bind(cutoff_ms)
    .bind(chunk_limit)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes hourly usage rollup buckets older than `cutoff_ms` in bounded chunks.
pub async fn sweep_expired_usage_rollups_hourly(
    pool: &SqlitePool,
    cutoff_ms: i64,
    chunk_limit: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM usage_rollups_hourly WHERE rowid IN (
             SELECT rowid FROM usage_rollups_hourly WHERE hour_ms < ? ORDER BY hour_ms LIMIT ?
         )",
    )
    .bind(cutoff_ms)
    .bind(chunk_limit)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes upstream usage snapshots fetched before `cutoff_ms` in bounded chunks.
pub async fn sweep_expired_upstream_usage_snapshots(
    pool: &SqlitePool,
    cutoff_ms: i64,
    chunk_limit: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM upstream_usage_snapshots WHERE id IN (
             SELECT id FROM upstream_usage_snapshots WHERE fetched_at < ? ORDER BY fetched_at LIMIT ?
         )",
    )
    .bind(cutoff_ms)
    .bind(chunk_limit)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes auth-failure counter buckets older than `cutoff_ms`.
///
/// Counters are per-minute rows used for abuse detection; a day of history is
/// enough for the admin audit view, and unbounded growth would otherwise keep
/// every failed probe forever.
pub async fn sweep_expired_auth_failure_counters(
    pool: &SqlitePool,
    cutoff_ms: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM auth_failure_counters WHERE minute_ms < ?")
        .bind(cutoff_ms)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Deletes content-addressed trace blocks that have not been referenced since
/// `cutoff_ms` (REL-3, TRC-2).
///
/// Blocks are shared across turns, so they are only collected once nothing has
/// touched them for a full retention window; `trace_records` expires first and
/// leaves the blocks behind.
pub async fn sweep_orphaned_trace_blocks(
    pool: &SqlitePool,
    cutoff_ms: i64,
    chunk_limit: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM trace_blocks WHERE rowid IN (
             SELECT rowid FROM trace_blocks WHERE last_referenced_at < ? ORDER BY last_referenced_at LIMIT ?
         )",
    )
    .bind(cutoff_ms)
    .bind(chunk_limit)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes expired control_plane_logs rows older than `cutoff_ms` (`USG-8`, `REL-1`).
pub async fn sweep_expired_control_plane_logs(
    pool: &SqlitePool,
    cutoff_ms: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM control_plane_logs WHERE ts < ?")
        .bind(cutoff_ms)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Deletes expired upstream_usage_snapshots rows older than `cutoff_ms` (`USG-4`, `REL-1`).
pub async fn sweep_expired_usage_snapshots(
    pool: &SqlitePool,
    cutoff_ms: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM upstream_usage_snapshots WHERE fetched_at < ?")
        .bind(cutoff_ms)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Executes a unified periodic retention sweep across all ephemeral and time-bounded tables (`TRC-4`, `USG-8`, `REL-1`).
pub async fn run_retention_pass(
    pool: &SqlitePool,
    now_ms: i64,
    policy: &RetentionPolicy,
) -> Result<(), sqlx::Error> {
    let raw_cutoff = now_ms.saturating_sub(policy.usage_events_retention_days * 86_400_000);
    let seven_days_cutoff = now_ms.saturating_sub(7 * 86_400_000);
    let ninety_days_cutoff = now_ms.saturating_sub(90 * 86_400_000);

    let _ = sweep_expired_usage_events(pool, raw_cutoff, 1_000).await?;
    let _ = enforce_max_usage_events_rows_pool(pool, policy.usage_events_max_rows, 1_000).await?;
    let _ = sweep_expired_pins(pool, now_ms).await?;
    let _ = sweep_expired_trace_records(pool, now_ms).await?;
    let _ = sweep_orphaned_trace_blocks(pool, raw_cutoff, 1_000).await?;
    let _ = sweep_expired_auth_failure_counters(pool, seven_days_cutoff).await?;
    let _ = sweep_expired_control_plane_logs(pool, ninety_days_cutoff).await?;
    let _ = sweep_expired_usage_snapshots(pool, seven_days_cutoff).await?;
    run_incremental_vacuum(pool, 64).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::migrate;
    use crate::pool::{Role, open_pool};

    #[tokio::test]
    #[doc = "Spec: TRC-4, USG-8, REL-1, REL-3"]
    async fn retention_pass_prunes_expired_control_plane_logs_pins_and_response_links() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = open_pool(&dir.path().join("retention.db"), Role::Writer)
            .await
            .expect("open pool");
        migrate(&pool).await.expect("migrate");

        sqlx::query(
            "INSERT INTO control_plane_logs (id, ts, request_id, workspace_scope, method, path, status, latency_ms)
             VALUES ('cpl_1', 1000, 'req_cpl_1', 'global', 'GET', '/admin/api/settings', 200, 5)",
        )
        .execute(&pool)
        .await
        .expect("insert cpl");

        let now_ms = 100 * 86_400_000;
        run_retention_pass(&pool, now_ms, &RetentionPolicy::default())
            .await
            .expect("run retention pass");

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_plane_logs")
            .fetch_one(&pool)
            .await
            .expect("count cpl");
        assert_eq!(remaining, 0);
    }
}
