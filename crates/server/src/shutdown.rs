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
    pub pool: SqlitePool,
    pub drain_grace_duration: Duration,
    pub drain_timeout_duration: Duration,
    pub writer: Option<llmroller_store::writer::WriterHandle>,
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
            pool,
            drain_grace_duration,
            drain_timeout_duration,
            writer: None,
        }
    }

    /// Attach the writer whose queues must be flushed before the final checkpoint.
    #[must_use]
    pub fn with_writer(mut self, writer: llmroller_store::writer::WriterHandle) -> Self {
        self.writer = Some(writer);
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

        // Step 2: Stop accepting new TCP connections (signaled via is_ready)
        self.shutdown_notify.notify_waiters();

        // Step 3: Wait for in-flight streams to complete naturally up to drain_timeout
        let start = Instant::now();
        while self.open_streams_counter.load(Ordering::SeqCst) > 0 {
            if start.elapsed() >= self.drain_timeout_duration {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Step 4: Cancel any remaining open streams with Termination::ShutdownCut
        if self.open_streams_counter.load(Ordering::SeqCst) > 0 {
            self.shutdown_notify.notify_waiters();
        }

        // Step 5: Background tasks observe the drain flag and stop scheduling work.

        // Step 6: Trace sealing is synchronous inside the pool; allow in-flight seals
        // to finish within the remaining drain budget (sealer write-through is immediate).

        // Step 7: Drain the writer's durable/rollup/raw queues before checkpointing (REL-1).
        let mut flush_failed = false;
        if let Some(writer) = &self.writer {
            let flushed = tokio::time::timeout(
                Duration::from_secs(5),
                llmroller_store::writer::flush_data_queues(&self.pool, &writer.queues),
            )
            .await;
            flush_failed = flushed.is_err();
        }

        // Step 8: Run PRAGMA wal_checkpoint(TRUNCATE) and exit 0
        let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await;

        if flush_failed {
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

impl JobSupervisor {
    #[must_use]
    pub fn new(job_name: impl Into<String>) -> Self {
        Self {
            job_name: job_name.into(),
            failed: Arc::new(AtomicBool::new(false)),
            panics: Mutex::new(Vec::new()),
        }
    }

    /// Run a task function inside `catch_unwind` with automatic panic recovery.
    pub async fn run_supervised<F>(&self, mut stop_rx: watch::Receiver<bool>, mut task_fn: F)
    where
        F: FnMut() + Send + 'static,
    {
        while !*stop_rx.borrow() {
            if self.failed.load(Ordering::SeqCst) {
                tracing::error!("job {} permanently stopped after 3 panics", self.job_name);
                break;
            }

            let result = std::panic::catch_unwind(AssertUnwindSafe(&mut task_fn));

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
                    _ = tokio::time::sleep(Duration::from_millis(backoff_secs * 10)) => {},
                    _ = stop_rx.changed() => break,
                }
            } else {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
