//! WAL checkpoint supervisor and SQLite storage pressure handler.
//!
//! Spec: Section 21.6.3, Requirements: REL-3, Partial: PERF-4.

use sqlx::SqlitePool;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// WAL file size threshold triggering RESTART checkpoint (256 MiB).
pub const WAL_RESTART_THRESHOLD_BYTES: u64 = 256 * 1024 * 1024;
/// WAL file size threshold setting /readyz to 503 (1 GiB).
pub const WAL_SATURATION_THRESHOLD_BYTES: u64 = 1024 * 1024 * 1024;

/// Supervisor managing passive and restart WAL checkpoints.
#[derive(Debug)]
pub struct WalCheckpointSupervisor {
    pub is_ready: Arc<AtomicBool>,
    pub storage_full: Arc<AtomicBool>,
}

impl WalCheckpointSupervisor {
    pub fn new(is_ready: Arc<AtomicBool>) -> Self {
        Self {
            is_ready,
            storage_full: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Evaluates WAL file size and runs the appropriate checkpoint pragma.
    pub async fn run_checkpoint(
        &self,
        pool: &SqlitePool,
        wal_bytes: u64,
    ) -> Result<(), sqlx::Error> {
        if wal_bytes >= WAL_SATURATION_THRESHOLD_BYTES {
            // Saturated (> 1 GiB) -> flip ready to false (503)
            self.is_ready.store(false, Ordering::SeqCst);
        } else if !self.storage_full.load(Ordering::SeqCst) {
            self.is_ready.store(true, Ordering::SeqCst);
        }

        if wal_bytes >= WAL_SATURATION_THRESHOLD_BYTES {
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE);")
                .execute(pool)
                .await?;
            // Once emergency TRUNCATE checkpoint succeeds and disk is not full, restore readiness (FND-028).
            if !self.storage_full.load(Ordering::SeqCst) {
                self.is_ready.store(true, Ordering::SeqCst);
            }
        } else if wal_bytes >= WAL_RESTART_THRESHOLD_BYTES {
            sqlx::query("PRAGMA wal_checkpoint(RESTART);")
                .execute(pool)
                .await?;
        } else {
            sqlx::query("PRAGMA wal_checkpoint(PASSIVE);")
                .execute(pool)
                .await?;
        }

        Ok(())
    }

    /// Handles simulated or actual SQLITE_FULL (code 13).
    pub fn handle_storage_full(&self) {
        self.storage_full.store(true, Ordering::SeqCst);
        self.is_ready.store(false, Ordering::SeqCst);
    }

    /// Clears storage full state when disk space is restored.
    pub fn clear_storage_full(&self) {
        self.storage_full.store(false, Ordering::SeqCst);
        self.is_ready.store(true, Ordering::SeqCst);
    }
}
