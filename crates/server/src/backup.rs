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

    struct TempPathGuard(std::path::PathBuf, bool);
    impl Drop for TempPathGuard {
        fn drop(&mut self) {
            if !self.1 && self.0.exists() {
                let _ = fs::remove_file(&self.0);
            }
        }
    }
    if tmp_path.exists() {
        let _ = fs::remove_file(&tmp_path);
    }
    let mut temp_guard = TempPathGuard(tmp_path.clone(), false);

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
    temp_guard.1 = true;

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

/// Restores a SQLite backup into `target_db_path` after verifying `PRAGMA integrity_check`,
/// schema presence (`instance_meta` / `instance_secrets`), and optional DEK unwrap under `master_key` (`REL-6`, `SEC-1`, `F-104`).
pub async fn restore_backup(
    backup_path: &Path,
    target_db_path: &Path,
    master_key: Option<&llmroller_store::crypto::MasterKey>,
    master_key_id: Option<&str>,
) -> Result<(), BackupError> {
    let verify_pool =
        llmroller_store::pool::open_pool(backup_path, llmroller_store::pool::Role::Reader).await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check;")
        .fetch_one(&verify_pool)
        .await?;
    if integrity != "ok" {
        verify_pool.close().await;
        return Err(BackupError::IntegrityCheckFailed);
    }

    let has_meta: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'instance_meta' LIMIT 1",
    )
    .fetch_optional(&verify_pool)
    .await?;
    if has_meta.is_none() {
        verify_pool.close().await;
        return Err(BackupError::IntegrityCheckFailed);
    }

    if let (Some(mk), Some(mk_id)) = (master_key, master_key_id) {
        let entropy = std::sync::Arc::new(llmroller_core::seams::OsEntropy::new());
        if llmroller_store::instance::load_or_initialize(&verify_pool, mk_id, mk, entropy, 0)
            .await
            .is_err()
        {
            verify_pool.close().await;
            return Err(BackupError::IntegrityCheckFailed);
        }
    }
    verify_pool.close().await;

    let parent = target_db_path.parent().unwrap_or_else(|| Path::new("."));
    let staging_path = parent.join(format!(
        "{}.restore.tmp",
        target_db_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ));
    fs::copy(backup_path, &staging_path)?;
    if let Ok(file) = File::open(&staging_path) {
        let _ = file.sync_all();
    }
    // FND-027: Remove any pre-existing -wal and -shm sidecar files before and after swapping the restored database
    // so SQLite never replays old WAL frames onto the newly restored database file.
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = target_db_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = fs::remove_file(std::path::PathBuf::from(sidecar));
    }
    fs::rename(&staging_path, target_db_path)?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = target_db_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = fs::remove_file(std::path::PathBuf::from(sidecar));
    }
    if let Ok(parent_dir) = File::open(parent) {
        let _ = parent_dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[doc = "Spec: REL-6, SEC-1"]
    async fn restore_backup_verifies_sqlite_integrity_and_schema_before_swap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty_db = dir.path().join("empty.db");
        let target_db = dir.path().join("target.db");
        std::fs::write(&target_db, b"original").expect("write target");
        // Create an uninitialized SQLite file lacking instance_meta
        let pool = llmroller_store::pool::open_pool(&empty_db, llmroller_store::pool::Role::Writer)
            .await
            .expect("open empty db");
        sqlx::query("CREATE TABLE dummy (id INTEGER PRIMARY KEY);")
            .execute(&pool)
            .await
            .expect("create dummy");
        pool.close().await;

        let res = restore_backup(&empty_db, &target_db, None, None).await;
        assert!(matches!(res, Err(BackupError::IntegrityCheckFailed)));
        assert_eq!(std::fs::read(&target_db).expect("read target"), b"original");
    }
}
