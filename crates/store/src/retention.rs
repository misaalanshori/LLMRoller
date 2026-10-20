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

/// Executes incremental vacuum to reclaim freed database pages.
pub async fn run_incremental_vacuum(pool: &SqlitePool, pages: u32) -> Result<(), sqlx::Error> {
    let query_str = format!("PRAGMA incremental_vacuum({pages});");
    sqlx::query(sqlx::AssertSqlSafe(query_str))
        .execute(pool)
        .await?;
    Ok(())
}
