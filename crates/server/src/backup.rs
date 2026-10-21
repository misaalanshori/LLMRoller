//! Atomic crash-safe database backup implementation.
//!
//! Spec: Section 5.5, Section 7.2, Requirements: REL-6.

use sqlx::SqlitePool;
use std::fs::{self, File};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("destination already exists and --overwrite not specified")]
    AlreadyExists,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("integrity check failed")]
    IntegrityCheckFailed,
}

/// Executes crash-safe atomic database backup with fsync and integrity check (REL-6).
pub async fn run_backup(
    pool: &SqlitePool,
    out_path: &Path,
    overwrite: bool,
    master_key_id: &str,
) -> Result<serde_json::Value, BackupError> {
    if out_path.exists() && !overwrite {
        return Err(BackupError::AlreadyExists);
    }

    let parent = out_path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_path = parent.join(format!(
        "{}.tmp",
        out_path.file_name().unwrap_or_default().to_string_lossy()
    ));

    if tmp_path.exists() {
        let _ = fs::remove_file(&tmp_path);
    }

    let tmp_str = tmp_path.to_string_lossy().replace('\\', "/");
    let escaped_tmp_str = tmp_str.replace('\'', "''");
    let query_str = format!("VACUUM INTO '{escaped_tmp_str}';");
    sqlx::query(sqlx::AssertSqlSafe(query_str))
        .execute(pool)
        .await?;

    // fsync the backup file
    if let Ok(file) = File::open(&tmp_path) {
        let _ = file.sync_all();
    }

    // Verify PRAGMA integrity_check before commit (REL-6)
    let tmp_pool =
        llmroller_store::pool::open_pool(&tmp_path, llmroller_store::pool::Role::Reader).await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check;")
        .fetch_one(&tmp_pool)
        .await?;
    tmp_pool.close().await;

    if integrity != "ok" {
        let _ = fs::remove_file(&tmp_path);
        return Err(BackupError::IntegrityCheckFailed);
    }

    // Atomic rename to destination
    fs::rename(&tmp_path, out_path)?;

    // fsync the parent directory
    if let Ok(parent_dir) = File::open(parent) {
        let _ = parent_dir.sync_all();
    }

    let metadata = fs::metadata(out_path)?;

    Ok(serde_json::json!({
        "status": "ok",
        "backup_path": out_path.to_string_lossy(),
        "master_key_id": master_key_id,
        "bytes": metadata.len()
    }))
}
