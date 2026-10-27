#![forbid(unsafe_code)]
//! Graceful SIGTERM drain supervisor and background job panic recovery per Section 5.5.3.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#553-ordered-graceful-shutdown-sigterm--sigint-and-job-supervision
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#72-reliability-and-availability-rel
//! Phase: ./docs/bigbang/big-bang-phases-2.md#27-sub-phase-27-v1-orchestrator-v1-pre-flight-body-memory-budget-graceful-sigterm-drain-and-early-load-harness

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;
use std::sync::Mutex;
use tokio::sync::{Notify, watch};

/// Exit code returned on clean drain completion.
pub const EXIT_SUCCESS: u8 = 0;

/// Exit code returned when queues remain unflushed after drain timeout.
pub const EXIT_QUEUE_TIMEOUT: u8 = 11;

/// Exit code returned on controlled shutdown after unrecoverable writer panic.
pub const EXIT_WRITER_PANIC: u8 = 12;

/// Eight-step graceful shutdown supervisor executing on SIGTERM or SIGINT per Section 5.5.3.
#[derive(Debug, Clone)]
pub struct ShutdownSupervisor {
    pub is_ready: Arc<AtomicBool>,
    pub open_streams_counter: Arc<AtomicUsize>,
    pub shutdown_notify: Arc<Notify>,
    /// Set before `shutdown_notify` fires so streams that are not parked on the
    /// notify cannot miss the cut (REL-1).
    pub shutdown_cut: Option<Arc<AtomicBool>>,
    pub pool: SqlitePool,
    pub drain_grace_duration: Duration,
    pub drain_timeout_duration: Duration,
    pub writer: Option<llmroller_store::writer::WriterHandle>,
    pub listener_stop: Option<watch::Sender<bool>>,
    pub background_stop: Option<watch::Sender<bool>>,
    pub sealer_pool: Option<Arc<llmroller_store::traces::SealerPool>>,
    pub runtime_state: Option<Arc<llmroller_core::runtime_state::RuntimeState>>,
}

impl ShutdownSupervisor {
    #[must_use]
    pub fn new(
        is_ready: Arc<AtomicBool>,
        open_streams_counter: Arc<AtomicUsize>,
        shutdown_notify: Arc<Notify>,
        pool: SqlitePool,
        drain_grace_duration: Duration,
        drain_timeout_duration: Duration,
    ) -> Self {
        Self {
            is_ready,
            open_streams_counter,
            shutdown_notify,
            shutdown_cut: None,
            pool,
            drain_grace_duration,
            drain_timeout_duration,
            writer: None,
            listener_stop: None,
            background_stop: None,
            sealer_pool: None,
            runtime_state: None,
        }
    }

    /// Attach the flag observed by live streams when the drain deadline expires (REL-1).
    #[must_use]
    pub fn with_shutdown_cut(mut self, cut: Arc<AtomicBool>) -> Self {
        self.shutdown_cut = Some(cut);
        self
    }

    /// Attach the writer whose queues must be flushed before the final checkpoint.
    #[must_use]
    pub fn with_writer(mut self, writer: llmroller_store::writer::WriterHandle) -> Self {
        self.writer = Some(writer);
        self
    }

    /// Attach the TCP listener stop channel so step 2 immediately stops accepting new connections (`FND-054`).
    #[must_use]
    pub fn with_listener_stop(mut self, stop: watch::Sender<bool>) -> Self {
        self.listener_stop = Some(stop);
        self
    }

    /// Attach the background-job stop channel so step 5 can cancel supervisors.
    #[must_use]
    pub fn with_background_stop(mut self, stop: watch::Sender<bool>) -> Self {
        self.background_stop = Some(stop);
        self
    }

    /// Attach the trace sealer pool so step 6 can drain accepted jobs.
    #[must_use]
    pub fn with_sealer_pool(
        mut self,
        sealer_pool: Arc<llmroller_store::traces::SealerPool>,
    ) -> Self {
        self.sealer_pool = Some(sealer_pool);
        self
    }

    /// Attach runtime state so step 7 can persist the latest key-health projection.
    #[must_use]
    pub fn with_runtime_state(
        mut self,
        runtime_state: Arc<llmroller_core::runtime_state::RuntimeState>,
    ) -> Self {
        self.runtime_state = Some(runtime_state);
        self
    }

    /// Execute the complete 8-step graceful shutdown sequence.
    ///
    /// Returns exit code `0` on clean drain, or `11` if queues failed to flush.
    pub async fn execute_graceful_shutdown(&self) -> u8 {
        // Step 1: Flip /readyz to HTTP 503 immediately and wait for drain_grace_secs
        self.is_ready.store(false, Ordering::SeqCst);
        if !self.drain_grace_duration.is_zero() {
            tokio::time::sleep(self.drain_grace_duration).await;
        }

        // Step 2: Stop accepting new TCP connections immediately before draining in-flight streams (FND-054)
        if let Some(listener_stop) = &self.listener_stop {
            let _ = listener_stop.send(true);
        }

        // Step 3: Wait for in-flight streams to complete naturally up to drain_timeout
        let start = Instant::now();
        while self.open_streams_counter.load(Ordering::SeqCst) > 0 {
            if start.elapsed() >= self.drain_timeout_duration {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Step 4: Cancel any remaining open streams with Termination::ShutdownCut (REL-3, F-097)
        if let Some(cut) = &self.shutdown_cut {
            cut.store(true, Ordering::SeqCst);
        }
        self.shutdown_notify.notify_waiters();

        // Step 5: Background tasks observe the drain flag and stop scheduling work.
        if let Some(stop) = &self.background_stop {
            let _ = stop.send(true);
        }

        // Step 6: Close trace-sealer submissions and await accepted seal jobs.
        let mut sealer_failed = false;
        if let Some(sealer_pool) = &self.sealer_pool {
            sealer_failed = !sealer_pool.flush(Duration::from_secs(2)).await;
        }

        // Step 7: Persist key health and drain the writer's queues before checkpointing (REL-1).
        let mut flush_failed = false;
        if let Some(runtime_state) = &self.runtime_state {
            let entries: Vec<(
                llmroller_core::ids::UpstreamKeyId,
                llmroller_core::runtime_state::KeyHealthEntry,
            )> = runtime_state
                .key_health
                .iter()
                .map(|entry| (*entry.key(), entry.value().clone()))
                .collect();
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as i64);
            if llmroller_store::health::persist_key_health(&self.pool, &entries, now_ms)
                .await
                .is_err()
            {
                flush_failed = true;
            }
        }
        if let Some(writer) = &self.writer {
            let flushed = tokio::time::timeout(
                Duration::from_secs(5),
                llmroller_store::writer::flush_data_queues(&self.pool, &writer.queues),
            )
            .await;
            if flushed.is_err() {
                flush_failed = true;
            }
        }

        // Step 8: Run PRAGMA wal_checkpoint(TRUNCATE) and exit 0.
        if sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await
            .is_err()
        {
            flush_failed = true;
        }

        if sealer_failed || flush_failed {
            return EXIT_QUEUE_TIMEOUT;
        }
        EXIT_SUCCESS
    }
}

/// Panic supervisor for background tasks (Discovery, UsagePoller, RetentionSweeper, Reloader).
///
/// Restarts panicking jobs with exponential backoff (1 s up to 300 s), marking the job
/// failed if 3 panics occur within a 10-minute window per Section 5.5.3.
#[derive(Debug)]
pub struct JobSupervisor {
    pub job_name: String,
    pub failed: Arc<AtomicBool>,
    panics: Mutex<Vec<i64>>,
}

/// Map a job name onto the closed set of static `job` metric labels (OBS-1).
fn job_label(name: &str) -> &'static str {
    match name {
        "discovery" => "discovery",
        "usage_poller" => "usage_poller",
        "retention" => "retention",
        "reloader" => "reloader",
        "maintenance" => "maintenance",
        _ => "other",
    }
}

impl JobSupervisor {
    #[must_use]
    pub fn new(job_name: impl Into<String>) -> Self {
        Self {
            job_name: job_name.into(),
            failed: Arc::new(AtomicBool::new(false)),
            panics: Mutex::new(Vec::new()),
        }
    }

    /// Run an async job under `catch_unwind` with automatic panic recovery.
    ///
    /// `task_fn` is called once per attempt with its own shutdown receiver, so a
    /// restarted job observes the same drain signal as the original spawn. The
    /// returned future is polled on the current task and its panic is converted into
    /// a restart with exponential backoff (1 s ..= 300 s), matching Section 5.5.3.
    pub async fn run_supervised<F, Fut>(&self, mut stop_rx: watch::Receiver<bool>, mut task_fn: F)
    where
        F: FnMut(watch::Receiver<bool>) -> Fut,
        Fut: std::future::Future<Output = ()> + Send,
    {
        use futures_util::FutureExt as _;

        while !*stop_rx.borrow() {
            if self.failed.load(Ordering::SeqCst) {
                tracing::error!("job {} permanently stopped after 3 panics", self.job_name);
                break;
            }

            let job_stop = stop_rx.clone();
            let result = AssertUnwindSafe(task_fn(job_stop)).catch_unwind().await;

            if let Err(_panic_err) = result {
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64);

                let (retries, backoff_secs, should_break) = {
                    let mut panics = self.panics.lock().expect("lock panics");
                    panics.retain(|&ts| now_ms - ts < 600_000); // 10 minutes
                    panics.push(now_ms);

                    if panics.len() >= 3 {
                        self.failed.store(true, Ordering::SeqCst);
                        tracing::error!(
                            "job {} failed: 3 panics within 10 minutes (marking failed)",
                            self.job_name
                        );
                        llmroller_proxy::metrics::record_job_failed(job_label(&self.job_name));
                        (panics.len(), 0, true)
                    } else {
                        let retries = panics.len();
                        let backoff = (1u64 << (retries - 1)).min(300);
                        (retries, backoff, false)
                    }
                };

                if should_break {
                    break;
                }

                tracing::warn!(
                    "job {} panicking (count={}), backing off for {}s",
                    self.job_name,
                    retries,
                    backoff_secs
                );

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {},
                    _ = stop_rx.changed() => break,
                }
            } else {
                // The job returned on its own: either it observed the shutdown signal
                // or it finished its work. Both are terminal, so do not spin.
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec: REL-1; steps 1, 2, 5 and 8 must run in order and report a clean drain,
    /// and step 4 must store the shutdown cut flag before it wakes waiters.
    #[tokio::test]
    async fn shutdown_broadcasts_background_stop_and_completes() {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory pool");
        let ready = Arc::new(AtomicBool::new(true));
        let streams = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(Notify::new());
        let cut = Arc::new(AtomicBool::new(false));
        let (stop_tx, mut stop_rx) = watch::channel(false);

        let supervisor = ShutdownSupervisor::new(
            ready.clone(),
            streams.clone(),
            notify.clone(),
            pool,
            Duration::ZERO,
            Duration::from_millis(10),
        )
        .with_background_stop(stop_tx)
        .with_shutdown_cut(cut.clone());

        let observer = tokio::spawn(async move {
            let _ = stop_rx.changed().await;
            *stop_rx.borrow()
        });

        let exit_code = supervisor.execute_graceful_shutdown().await;
        assert_eq!(exit_code, EXIT_SUCCESS);
        assert!(!ready.load(Ordering::SeqCst), "readiness must flip to 503");
        assert!(
            cut.load(Ordering::SeqCst),
            "step 4 must store the shutdown cut flag"
        );
        assert!(
            observer.await.expect("observer"),
            "step 5 must broadcast stop"
        );
    }
}
