//! Writer actor tests.
//!
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#72-reliability-and-availability-rel
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#92-storage-invariants-dual-queue-write-batching-and-retention

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use llmroller_core::seams::DeterministicRng;
use sqlx::{Sqlite, Transaction};
use tempfile::TempDir;
use tokio::sync::mpsc;

use super::{ControlOp, DataQueues, RAW_EVENTS_MAX_ITEMS, WriterError, start_writer};
use crate::crypto::MasterKey;
use crate::instance::load_or_initialize;
use crate::migrations::migrate;
use crate::pool::{Role, open_pool};

async fn setup_writer_db() -> (TempDir, sqlx::SqlitePool) {
    let directory = tempfile::tempdir().expect("tempdir");
    let pool = open_pool(&directory.path().join("llmroller.db"), Role::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    load_or_initialize(
        &pool,
        "mk_test",
        &MasterKey::from_bytes([0x33; 32]),
        Arc::new(DeterministicRng::new(0x33)),
        1_000,
    )
    .await
    .expect("bootstrap");
    (directory, pool)
}

struct InsertSettingOp(&'static str, &'static str);

impl ControlOp for InsertSettingOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), WriterError>> + Send + 'a>> {
        Box::pin(async move {
            sqlx::query("INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, 1)")
                .bind(self.0)
                .bind(self.1)
                .execute(&mut **tx)
                .await
                .map_err(WriterError::from)?;
            Ok(())
        })
    }
}

struct UpdateSettingOp(&'static str, &'static str);

impl ControlOp for UpdateSettingOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), WriterError>> + Send + 'a>> {
        Box::pin(async move {
            sqlx::query("UPDATE settings SET value_json = ? WHERE key = ?")
                .bind(self.1)
                .bind(self.0)
                .execute(&mut **tx)
                .await
                .map_err(WriterError::from)?;
            Ok(())
        })
    }
}

#[tokio::test]
#[doc = "Spec: REL-4"]
async fn writer_control_transaction_increments_config_version_and_notifies_reloader() {
    let (_dir, pool) = setup_writer_db().await;
    let (reloader_tx, mut reloader_rx) = mpsc::channel(10);
    let (writer, _handle) = start_writer(pool.clone(), reloader_tx);

    let v1 = writer
        .execute_control(vec![], vec![], InsertSettingOp("test_k", "\"test_v\""))
        .await
        .expect("execute control 1");

    assert_eq!(v1, 1);
    let notif1 = reloader_rx.recv().await.expect("reloader notification");
    assert_eq!(notif1.min_version, 1);

    let v2 = writer
        .execute_control(vec![], vec![], UpdateSettingOp("test_k", "\"updated\""))
        .await
        .expect("execute control 2");

    assert_eq!(v2, 2);
    let notif2 = reloader_rx.recv().await.expect("reloader notification");
    assert_eq!(notif2.min_version, 2);
}

#[test]
#[doc = "Spec: REL-3"]
fn data_queues_raw_events_bounds_and_evicts_oldest() {
    let queues = DataQueues::new();
    for i in 0..RAW_EVENTS_MAX_ITEMS + 500 {
        queues.push_raw(format!("item_{i}"));
    }

    let lock = queues.raw_events.lock();
    assert_eq!(lock.len(), RAW_EVENTS_MAX_ITEMS);
    // Oldest 500 were evicted: front is item_500
    assert_eq!(lock.front().expect("front"), "item_500");
    assert_eq!(
        lock.back().expect("back"),
        &format!("item_{}", RAW_EVENTS_MAX_ITEMS + 499)
    );
}
