#![forbid(unsafe_code)]
//! Dual-priority SQLite writer actor.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#92-storage-invariants-dual-queue-write-batching-and-retention
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#2161-writer-queue-priority-and-byte-bounds

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use llmroller_core::snapshot::{CacheInvalidationEvent, ConfigDomain};
use parking_lot::Mutex;
use sqlx::{Connection as _, Sqlite, SqlitePool, Transaction};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::usage_writer::ThreeQueueUsageWriter;

/// Maximum number of raw items retained in the bounded queue before evicting oldest per `REL-3`.
pub const RAW_EVENTS_MAX_ITEMS: usize = 10_000;
/// Maximum bytes in raw items queue (64 MiB).
pub const RAW_EVENTS_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Maximum items in durable state queue before rejecting new confirmations (500,000).
pub const DURABLE_STATE_MAX_ITEMS: usize = 500_000;
/// Maximum items per data batch commit.
pub const BATCH_MAX_ITEMS: usize = 200;
/// Maximum byte size per data batch commit (4 MiB).
pub const BATCH_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Maximum duration before flushing an uncommitted data batch.
pub const BATCH_MAX_AGE: Duration = Duration::from_millis(250);

/// Writer actor errors.
#[derive(Debug, Error)]
pub enum WriterError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("control transaction timeout")]
    Timeout,
    #[error("writer channel closed")]
    ChannelClosed,
    #[error("writer execution failure: {0}")]
    Execution(String),
}

/// Notification dispatched to the Reloader task whenever a control mutation commits.
#[derive(Debug, Clone)]
pub struct ReloadNotification {
    pub min_version: i64,
    pub changed_domains: Vec<ConfigDomain>,
    pub invalidations: Vec<CacheInvalidationEvent>,
}

/// Trait for transactional operations executed inside the control-plane writer.
pub trait ControlOp: Send {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), WriterError>> + Send + 'a>>;
}

pub struct ControlTask {
    pub op: Box<dyn ControlOp>,
    pub changed_domains: Vec<ConfigDomain>,
    pub invalidations: Vec<CacheInvalidationEvent>,
    pub reply: oneshot::Sender<Result<i64, WriterError>>,
}

impl std::fmt::Debug for ControlTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlTask")
            .field("changed_domains", &self.changed_domains)
            .field("invalidations", &self.invalidations)
            .finish()
    }
}

/// Key for deduplicating in-flight durable ops.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DurableKey {
    Pin(String),
    ResponseLink(String),
    HealthVersion(String),
}

/// Durable operation executed in SQLite with per-item SAVEPOINT.
#[derive(Debug, Clone)]
pub enum DurableOp {
    PinUpsert {
        client_key_id: String,
        session_hash: String,
        model_pk: String,
        upstream_key_id: String,
        pool_tier: u8,
        created_at: i64,
        last_used_at: i64,
        expires_at: i64,
    },
    ResponseLinkUpsert(crate::pins::ResponseLinkRow),
    HealthVersionUpdate {
        upstream_key_id: String,
        health: String,
        health_until: Option<i64>,
        health_version: i64,
        last_error: Option<String>,
    },
}

/// In-memory data queues backing the non-blocking data plane.
#[derive(Debug)]
pub struct DataQueues {
    pub durable_state: Mutex<IndexMap<DurableKey, DurableOp>>,
    pub usage_writer: Arc<ThreeQueueUsageWriter>,
    pub rollups: Mutex<HashMap<String, i64>>,
    pub raw_events: Mutex<VecDeque<String>>,
    pub raw_bytes: Mutex<usize>,
}

impl Default for DataQueues {
    fn default() -> Self {
        Self {
            durable_state: Mutex::new(IndexMap::new()),
            usage_writer: Arc::new(ThreeQueueUsageWriter::new(RAW_EVENTS_MAX_ITEMS)),
            rollups: Mutex::new(HashMap::new()),
            raw_events: Mutex::new(VecDeque::new()),
            raw_bytes: Mutex::new(0),
        }
    }
}

impl DataQueues {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a raw event item, dropping the oldest item if capacity is exceeded per `REL-3`.
    pub fn push_raw(&self, item: String) {
        let item_len = item.len();
        let mut raw = self.raw_events.lock();
        let mut bytes = self.raw_bytes.lock();

        while raw.len() >= RAW_EVENTS_MAX_ITEMS
            || (*bytes + item_len > RAW_EVENTS_MAX_BYTES && !raw.is_empty())
        {
            if let Some(popped) = raw.pop_front() {
                *bytes = bytes.saturating_sub(popped.len());
            }
        }
        *bytes += item_len;
        raw.push_back(item);
    }
}

/// Handle for callers interacting with the writer actor.
#[derive(Clone)]
pub struct WriterHandle {
    control_tx: mpsc::Sender<ControlTask>,
    pub queues: Arc<DataQueues>,
}

impl std::fmt::Debug for WriterHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterHandle")
            .field("queues", &self.queues)
            .finish()
    }
}

impl WriterHandle {
    /// Execute an atomic control-plane mutation with strict priority, incrementing
    /// `config_version` and notifying the Reloader.
    pub async fn execute_control<O: ControlOp + 'static>(
        &self,
        changed_domains: Vec<ConfigDomain>,
        invalidations: Vec<CacheInvalidationEvent>,
        op: O,
    ) -> Result<i64, WriterError> {
        let (reply, rx) = oneshot::channel();
        let task = ControlTask {
            op: Box::new(op),
            changed_domains,
            invalidations,
            reply,
        };

        self.control_tx
            .send(task)
            .await
            .map_err(|_| WriterError::ChannelClosed)?;

        tokio::time::timeout(Duration::from_millis(2000), rx)
            .await
            .map_err(|_| WriterError::Timeout)?
            .map_err(|_| WriterError::ChannelClosed)?
    }
}

/// Spawn the dedicated background writer actor.
pub fn start_writer(
    pool: SqlitePool,
    reloader_tx: mpsc::Sender<ReloadNotification>,
) -> (WriterHandle, tokio::task::JoinHandle<()>) {
    let (control_tx, mut control_rx) = mpsc::channel::<ControlTask>(100);
    let queues = Arc::new(DataQueues::new());
    let queues_clone = queues.clone();

    let join_handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(BATCH_MAX_AGE);

        loop {
            tokio::select! {
                biased;

                // Priority 1: Control queue commands always drain first
                Some(task) = control_rx.recv() => {
                    handle_control_task(&pool, task, &reloader_tx).await;
                }

                // Priority 2: Periodic batch commit for data queues
                _ = ticker.tick() => {
                    // Drain any pending control commands before committing data batch
                    while let Ok(task) = control_rx.try_recv() {
                        handle_control_task(&pool, task, &reloader_tx).await;
                    }

                    // Flush data queues if any items exist
                    flush_data_queues(&pool, &queues_clone).await;

                    // Drain any pending control commands after committing data batch
                    while let Ok(task) = control_rx.try_recv() {
                        handle_control_task(&pool, task, &reloader_tx).await;
                    }
                }

                else => break,
            }
        }
    });

    (WriterHandle { control_tx, queues }, join_handle)
}

async fn handle_control_task(
    pool: &SqlitePool,
    task: ControlTask,
    reloader_tx: &mpsc::Sender<ReloadNotification>,
) {
    let result = execute_single_control_task(pool, task.op).await;
    match result {
        Ok(new_version) => {
            let notification = ReloadNotification {
                min_version: new_version,
                changed_domains: task.changed_domains,
                invalidations: task.invalidations,
            };
            let _ = reloader_tx.send(notification).await;
            let _ = task.reply.send(Ok(new_version));
        }
        Err(err) => {
            let _ = task.reply.send(Err(err));
        }
    }
}

async fn execute_single_control_task(
    pool: &SqlitePool,
    op: Box<dyn ControlOp>,
) -> Result<i64, WriterError> {
    let mut connection = pool.acquire().await?;
    let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;

    op.call(&mut transaction).await?;

    // Bump instance_meta.config_version in the exact same transaction per SRDD Section 5.4 / 21.6.2
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);

    let new_version: i64 = sqlx::query_scalar(
        "UPDATE instance_meta SET config_version = config_version + 1, updated_at = ?
         RETURNING config_version",
    )
    .bind(now)
    .fetch_one(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(new_version)
}

pub async fn flush_data_queues(pool: &SqlitePool, queues: &DataQueues) {
    let has_durable = !queues.durable_state.lock().is_empty();
    let has_usage = !queues.usage_writer.rollups_5min.lock().is_empty()
        || !queues.usage_writer.rollups_hourly.lock().is_empty()
        || !queues.usage_writer.raw_events.lock().is_empty();
    let has_raw = !queues.raw_events.lock().is_empty();

    if !has_durable && !has_usage && !has_raw {
        return;
    }

    let mut connection = match pool.acquire().await {
        Ok(conn) => conn,
        Err(_) => return,
    };
    let mut transaction = match connection.begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => tx,
        Err(_) => return,
    };

    // 1. Drain and execute durable_state items with per-item SAVEPOINT
    let durable_items: Vec<(DurableKey, DurableOp)> = {
        let mut lock = queues.durable_state.lock();
        std::mem::take(&mut *lock).into_iter().collect()
    };

    for (idx, (_key, op)) in durable_items.into_iter().enumerate() {
        let sp_name = format!("item_{idx}");
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!("SAVEPOINT {sp_name}"))).execute(&mut *transaction).await;
        let res = match op {
            DurableOp::PinUpsert {
                client_key_id,
                session_hash,
                model_pk,
                upstream_key_id,
                pool_tier,
                created_at,
                last_used_at,
                expires_at,
            } => {
                sqlx::query(
                    r#"INSERT INTO pins (
                           client_key_id, session_hash, model_pk, upstream_key_id,
                           pool_tier, created_at, last_used_at, expires_at
                       )
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                       ON CONFLICT (client_key_id, session_hash, model_pk) DO UPDATE SET
                           upstream_key_id = excluded.upstream_key_id,
                           pool_tier = excluded.pool_tier,
                           last_used_at = excluded.last_used_at,
                           expires_at = excluded.expires_at"#,
                )
                .bind(client_key_id)
                .bind(session_hash)
                .bind(model_pk)
                .bind(upstream_key_id)
                .bind(pool_tier as i64)
                .bind(created_at)
                .bind(last_used_at)
                .bind(expires_at)
                .execute(&mut *transaction)
                .await
                .map(|_| ())
            }
            DurableOp::ResponseLinkUpsert(link) => {
                crate::pins::insert_response_link_tx(&mut transaction, &link).await
            }
            DurableOp::HealthVersionUpdate {
                upstream_key_id,
                health,
                health_until,
                health_version,
                last_error,
            } => {
                sqlx::query(
                    r#"UPDATE upstream_keys SET
                           health = ?, health_until = ?, health_version = ?, last_error = ?
                       WHERE id = ?"#,
                )
                .bind(health)
                .bind(health_until)
                .bind(health_version)
                .bind(last_error)
                .bind(upstream_key_id)
                .execute(&mut *transaction)
                .await
                .map(|_| ())
            }
        };

        if res.is_ok() {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!("RELEASE SAVEPOINT {sp_name}"))).execute(&mut *transaction).await;
        } else {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!("ROLLBACK TO SAVEPOINT {sp_name}"))).execute(&mut *transaction).await;
        }
    }

    // 2. Flush usage rollups, usage_events, and usage_attempts
    let _ = queues.usage_writer.flush_to_db(&mut transaction).await;

    // 3. Drain raw_events items up to batch limits
    {
        let mut raw_lock = queues.raw_events.lock();
        let mut bytes_lock = queues.raw_bytes.lock();
        let drain_count = raw_lock.len().min(BATCH_MAX_ITEMS);
        for _ in 0..drain_count {
            if let Some(popped) = raw_lock.pop_front() {
                *bytes_lock = bytes_lock.saturating_sub(popped.len());
            }
        }
    }

    let _ = transaction.commit().await;
}

#[cfg(test)]
mod tests;
