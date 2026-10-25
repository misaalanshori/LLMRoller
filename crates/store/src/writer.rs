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

use crate::traces::SealedTurnResult;
use crate::usage_snapshots::SealedUsageSnapshot;
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
    Pin(String, String, String),
    ResponseLink(String),
    HealthVersion(String),
    RouteHealth(String, String),
    RouteProtocolBlock(String, String, String),
    RouteProtocolClear(String),
    UsageSnapshot(String),
    ClientKeyTouch(String),
    ClientKeyConstantSessionFlag(String),
    AuthFailureCounter(i64, String, String),
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
        repin_count: i64,
    },
    ResponseLinkUpsert(crate::pins::ResponseLinkRow),
    ResponseLinkDelete {
        response_id: String,
    },
    ResponseLinkMarkIncomplete {
        response_id: String,
        updated_at: i64,
    },
    UsageSnapshotInsert(SealedUsageSnapshot),
    ClientKeyTouchLastUsed {
        client_key_id: String,
        last_used_at: i64,
    },
    ClientKeyConstantSessionFlag {
        client_key_id: String,
        flagged_at: i64,
    },
    AuthFailureCounterIncrement {
        minute_ms: i64,
        reason: String,
        ip_prefix: String,
    },
    HealthVersionUpdate {
        upstream_key_id: String,
        health: String,
        health_until: Option<i64>,
        health_version: i64,
        last_error: Option<String>,
    },
    RouteHealthUpdate {
        upstream_key_id: String,
        model_pk: String,
        route_health: String,
        route_health_until: Option<i64>,
        health_version: i64,
        last_error: Option<String>,
    },
    RouteProtocolBlockUpsert {
        upstream_key_id: String,
        model_pk: String,
        protocol: String,
        reason: String,
        until_ms: i64,
    },
    RouteProtocolBlockClear {
        upstream_key_id: String,
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
    /// Sealed traces waiting for the writer to persist them after their
    /// referenced `usage_events` row in the same batch (F09).
    pub sealed_traces: Mutex<VecDeque<SealedTurnResult>>,
    pub sqlite_full: std::sync::atomic::AtomicBool,
}

impl Default for DataQueues {
    fn default() -> Self {
        Self {
            durable_state: Mutex::new(IndexMap::new()),
            usage_writer: Arc::new(ThreeQueueUsageWriter::new(RAW_EVENTS_MAX_ITEMS)),
            rollups: Mutex::new(HashMap::new()),
            raw_events: Mutex::new(VecDeque::new()),
            raw_bytes: Mutex::new(0),
            sealed_traces: Mutex::new(VecDeque::new()),
            sqlite_full: std::sync::atomic::AtomicBool::new(false),
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

    /// Enqueue a sealed trace produced by the sealer pool. The writer persists
    /// it after `usage_events` in the same transaction so the foreign key is
    /// always satisfied (F09).
    pub fn push_sealed_trace(&self, trace: SealedTurnResult) {
        let mut traces = self.sealed_traces.lock();
        while traces.len() >= RAW_EVENTS_MAX_ITEMS {
            traces.pop_front();
        }
        traces.push_back(trace);
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
    #[must_use]
    pub fn is_sqlite_full(&self) -> bool {
        self.queues
            .sqlite_full
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn set_sqlite_full(&self, full: bool) {
        self.queues
            .sqlite_full
            .store(full, std::sync::atomic::Ordering::Release);
    }

    #[must_use]
    pub fn is_durable_queue_full(&self) -> bool {
        self.queues.durable_state.lock().len() >= DURABLE_STATE_MAX_ITEMS
    }

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
    let new_version = bump_config_version_tx(&mut transaction).await?;

    transaction.commit().await?;
    Ok(new_version)
}

/// Increment `instance_meta.config_version` inside an existing write transaction.
///
/// Control-plane mutations use this together with their entity write and
/// `audit_log` append so all three become visible atomically (AUD-1, F09).
pub async fn bump_config_version_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<i64, sqlx::Error> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);

    sqlx::query_scalar(
        "UPDATE instance_meta SET config_version = config_version + 1, updated_at = ?
         RETURNING config_version",
    )
    .bind(now)
    .fetch_one(&mut **tx)
    .await
}

pub async fn flush_data_queues(pool: &SqlitePool, queues: &DataQueues) {
    let has_durable = !queues.durable_state.lock().is_empty();
    let has_usage = !queues.usage_writer.rollups_5min.lock().is_empty()
        || !queues.usage_writer.rollups_hourly.lock().is_empty()
        || !queues.usage_writer.raw_events.lock().is_empty();
    let has_raw = !queues.raw_events.lock().is_empty();
    let has_traces = !queues.sealed_traces.lock().is_empty();

    if !has_durable && !has_usage && !has_raw && !has_traces {
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
    let durable_backup = durable_items.clone();

    for (idx, (_key, op)) in durable_items.into_iter().enumerate() {
        let sp_name = format!("item_{idx}");
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!("SAVEPOINT {sp_name}")))
            .execute(&mut *transaction)
            .await;
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
                repin_count,
            } => sqlx::query(
                r#"INSERT INTO pins (
                           client_key_id, session_hash, model_pk, upstream_key_id,
                           pool_tier, created_at, last_used_at, expires_at, repin_count
                       )
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                       ON CONFLICT (client_key_id, session_hash, model_pk) DO UPDATE SET
                           upstream_key_id = excluded.upstream_key_id,
                           pool_tier = excluded.pool_tier,
                           last_used_at = excluded.last_used_at,
                           expires_at = excluded.expires_at,
                           repin_count = excluded.repin_count"#,
            )
            .bind(client_key_id)
            .bind(session_hash)
            .bind(model_pk)
            .bind(upstream_key_id)
            .bind(pool_tier as i64)
            .bind(created_at)
            .bind(last_used_at)
            .bind(expires_at)
            .bind(repin_count)
            .execute(&mut *transaction)
            .await
            .map(|_| ()),
            DurableOp::ResponseLinkUpsert(link) => {
                crate::pins::insert_response_link_tx(&mut transaction, &link).await
            }
            DurableOp::ResponseLinkDelete { response_id } => {
                sqlx::query("DELETE FROM response_links WHERE response_id = ?")
                    .bind(response_id)
                    .execute(&mut *transaction)
                    .await
                    .map(|_| ())
            }
            DurableOp::ResponseLinkMarkIncomplete {
                response_id,
                updated_at,
            } => sqlx::query(
                "UPDATE response_links SET status = 'incomplete', updated_at = ? WHERE response_id = ?",
            )
            .bind(updated_at)
            .bind(response_id)
            .execute(&mut *transaction)
            .await
            .map(|_| ()),
            DurableOp::UsageSnapshotInsert(snapshot) => {
                crate::usage_snapshots::insert_sealed_usage_snapshot_tx(&mut transaction, &snapshot)
                    .await
                    .map(|_| ())
            }
            DurableOp::HealthVersionUpdate {
                upstream_key_id,
                health,
                health_until,
                health_version,
                last_error,
            } => sqlx::query(
                r#"UPDATE upstream_keys SET
                           health = ?, health_until = ?, health_version = ?, last_error = ?
                       WHERE id = ? AND health_version < ?"#,
            )
            .bind(health)
            .bind(health_until)
            .bind(health_version)
            .bind(last_error)
            .bind(upstream_key_id)
            .bind(health_version)
            .execute(&mut *transaction)
            .await
            .map(|_| ()),
            DurableOp::RouteHealthUpdate {
                upstream_key_id,
                model_pk,
                route_health,
                route_health_until,
                health_version,
                last_error,
            } => sqlx::query(
                r#"UPDATE model_routes SET
                           route_health = ?, route_health_until = ?, health_version = ?, last_error = ?
                       WHERE upstream_key_id = ? AND model_pk = ? AND health_version < ?"#,
            )
            .bind(route_health)
            .bind(route_health_until)
            .bind(health_version)
            .bind(last_error)
            .bind(upstream_key_id)
            .bind(model_pk)
            .bind(health_version)
            .execute(&mut *transaction)
            .await
            .map(|_| ()),
            DurableOp::RouteProtocolBlockUpsert {
                upstream_key_id,
                model_pk,
                protocol,
                reason,
                until_ms,
            } => sqlx::query(
                r#"INSERT INTO route_protocol_blocks (
                           upstream_key_id, model_pk, protocol, reason, until_ms
                       )
                       VALUES (?, ?, ?, ?, ?)
                       ON CONFLICT (upstream_key_id, model_pk, protocol) DO UPDATE SET
                           reason = excluded.reason,
                           until_ms = excluded.until_ms"#,
            )
            .bind(upstream_key_id)
            .bind(model_pk)
            .bind(protocol)
            .bind(reason)
            .bind(until_ms)
            .execute(&mut *transaction)
            .await
            .map(|_| ()),
            DurableOp::ClientKeyTouchLastUsed {
                client_key_id,
                last_used_at,
            } => sqlx::query("UPDATE client_keys SET last_used_at = ? WHERE id = ?")
                .bind(last_used_at)
                .bind(client_key_id)
                .execute(&mut *transaction)
                .await
                .map(|_| ()),
            DurableOp::ClientKeyConstantSessionFlag {
                client_key_id,
                flagged_at,
            } => sqlx::query(
                "UPDATE client_keys SET constant_session_flagged_at = ? WHERE id = ? AND constant_session_flagged_at IS NULL",
            )
            .bind(flagged_at)
            .bind(client_key_id)
            .execute(&mut *transaction)
            .await
            .map(|_| ()),
            DurableOp::AuthFailureCounterIncrement {
                minute_ms,
                reason,
                ip_prefix,
            } => sqlx::query(
                "INSERT INTO auth_failure_counters (minute_ms, reason, ip_prefix, count)
                 VALUES (?, ?, ?, 1)
                 ON CONFLICT(minute_ms, reason, ip_prefix) DO UPDATE SET count = count + 1",
            )
            .bind(minute_ms)
            .bind(reason)
            .bind(ip_prefix)
            .execute(&mut *transaction)
            .await
            .map(|_| ()),
            DurableOp::RouteProtocolBlockClear { upstream_key_id } => sqlx::query(
                "DELETE FROM route_protocol_blocks WHERE upstream_key_id = ?",
            )
            .bind(upstream_key_id)
            .execute(&mut *transaction)
            .await
            .map(|_| ()),
        };

        if res.is_ok() {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!("RELEASE SAVEPOINT {sp_name}")))
                .execute(&mut *transaction)
                .await;
        } else {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "ROLLBACK TO SAVEPOINT {sp_name}"
            )))
            .execute(&mut *transaction)
            .await;
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!("RELEASE SAVEPOINT {sp_name}")))
                .execute(&mut *transaction)
                .await;
        }
    }

    let r5_backup = queues.usage_writer.rollups_5min.lock().clone();
    let rh_backup = queues.usage_writer.rollups_hourly.lock().clone();
    let raw_backup = queues.usage_writer.raw_events.lock().clone();

    // 2. Flush usage rollups, usage_events, and usage_attempts
    let usage_res = queues.usage_writer.flush_to_db(&mut transaction).await;

    // 2b. Persist sealed traces immediately after their `usage_events` rows so
    // the foreign key is satisfied without a retry loop (F09). A trace whose
    // event was dropped by the bounded queue is discarded in its own savepoint.
    let trace_items: Vec<SealedTurnResult> = {
        let mut lock = queues.sealed_traces.lock();
        std::mem::take(&mut *lock).into_iter().collect()
    };
    let trace_backup = trace_items.clone();
    let mut deferred_traces: Vec<SealedTurnResult> = Vec::new();
    for (idx, trace) in trace_items.into_iter().enumerate() {
        let sp_name = format!("trace_{idx}");
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!("SAVEPOINT {sp_name}")))
            .execute(&mut *transaction)
            .await;
        let master_key_id = trace.master_key_id.clone();
        let result =
            crate::traces::insert_sealed_trace_tx(&mut transaction, &trace, &master_key_id).await;
        match result {
            Ok(()) => {
                let _ = sqlx::query(sqlx::AssertSqlSafe(format!("RELEASE SAVEPOINT {sp_name}")))
                    .execute(&mut *transaction)
                    .await;
            }
            Err(error) => {
                let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "ROLLBACK TO SAVEPOINT {sp_name}"
                )))
                .execute(&mut *transaction)
                .await;
                let _ = sqlx::query(sqlx::AssertSqlSafe(format!("RELEASE SAVEPOINT {sp_name}")))
                    .execute(&mut *transaction)
                    .await;
                // F016: If the parent usage_event is still pending in usage_writer.raw_events,
                // defer the trace to the next flush cycle instead of dropping it on a FK race.
                let parent_still_queued = queues
                    .usage_writer
                    .raw_events
                    .lock()
                    .iter()
                    .any(|(ev, _)| ev.id == trace.usage_event_id);
                if parent_still_queued {
                    deferred_traces.push(trace);
                } else {
                    tracing::warn!(
                        usage_event_id = %trace.usage_event_id,
                        %error,
                        "dropping trace whose usage event is unavailable"
                    );
                }
            }
        }
    }
    if !deferred_traces.is_empty() {
        let mut lock = queues.sealed_traces.lock();
        for item in deferred_traces.into_iter().rev() {
            lock.push_front(item);
        }
    }

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

    match transaction.commit().await {
        Ok(()) => {
            queues
                .sqlite_full
                .store(false, std::sync::atomic::Ordering::Release);
        }
        Err(err) => {
            if is_sqlite_disk_full_error(&err) {
                queues
                    .sqlite_full
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            tracing::error!(
                "SQLite batch commit failed ({err}); restoring durable_state, rollups, and traces to memory"
            );
            {
                let mut durable_lock = queues.durable_state.lock();
                for (k, op) in durable_backup {
                    durable_lock.entry(k).or_insert(op);
                }
            }
            if usage_res.is_ok() {
                queues
                    .usage_writer
                    .restore_batch(r5_backup, rh_backup, raw_backup);
            }
            let mut trace_lock = queues.sealed_traces.lock();
            for item in trace_backup.into_iter().rev() {
                trace_lock.push_front(item);
            }
        }
    }
}

/// Returns `true` if `err` represents `SQLITE_FULL` (code 13) or `SQLITE_IOERR` (code 10) (`F054`, `REL-3`).
#[must_use]
pub fn is_sqlite_disk_full_error(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => {
            let code_match = db_err
                .code()
                .is_some_and(|c| c == "13" || c == "10" || c.starts_with("13") || c.starts_with("10"));
            let msg = db_err.message().to_ascii_lowercase();
            code_match
                || msg.contains("database or disk is full")
                || msg.contains("disk i/o error")
                || msg.contains("no space left")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
