#![forbid(unsafe_code)]
//! RequestLifecycle RAII completion guard and abort handling per Section 21.1.
//!
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#211-per-request-raii-completion-guard-requestlifecycle
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#68-resilience-and-completion-lifecycle-api
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#1242-deterministic-completion-and-abort-guard-requestlifecycle
//! Phase: ./docs/bigbang/big-bang-phases-2.md#25-sub-phase-25-unbuffered-sse-forwarder-64-kib-pre-first-frame-holding-buffer-single-frame-filter-and-requestlifecycle-guard

use llmroller_core::ids::RequestId;

/// Request completion / termination classification per Section 21.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// Normal stream completion ([DONE], response.completed, or message_stop).
    Completed,
    /// Upstream stream closed before terminal event was received.
    UpstreamTruncated,
    /// Client disconnected or cancelled request future before completion.
    ClientGone,
    /// Stream idle timeout expired (300,000 ms).
    IdleTimeout,
    /// Server graceful shutdown drain timer expired.
    ShutdownCut,
}

impl Termination {
    #[must_use]
    pub const fn is_incomplete(self) -> bool {
        !matches!(self, Self::Completed)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::UpstreamTruncated => "upstream_truncated",
            Self::ClientGone => "client_gone",
            Self::IdleTimeout => "idle_timeout",
            Self::ShutdownCut => "shutdown_cut",
        }
    }
}

/// Accumulator for token counts observed during streaming response.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UsageAccumulator {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub decoded_text_bytes: u64,
    pub observed_usage: bool,
}

/// Callback invoked once when RequestLifecycle finalizes.
pub type FinalizeCallback = Box<dyn FnOnce(Termination, UsageAccumulator) + Send + 'static>;

/// RAII completion guard for every routed inference request per Section 21.1.
pub struct RequestLifecycle {
    pub request_id: RequestId,
    pub started_ms: i64,
    pub first_frame_ms: Option<i64>,
    pub usage: UsageAccumulator,
    pub termination: Termination,
    finalized: bool,
    on_finalize: Option<FinalizeCallback>,
}

impl std::fmt::Debug for RequestLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestLifecycle")
            .field("request_id", &self.request_id)
            .field("started_ms", &self.started_ms)
            .field("first_frame_ms", &self.first_frame_ms)
            .field("usage", &self.usage)
            .field("termination", &self.termination)
            .field("finalized", &self.finalized)
            .finish()
    }
}

impl RequestLifecycle {
    #[must_use]
    pub fn new(
        request_id: RequestId,
        started_ms: i64,
        on_finalize: Option<FinalizeCallback>,
    ) -> Self {
        Self {
            request_id,
            started_ms,
            first_frame_ms: None,
            usage: UsageAccumulator::default(),
            termination: Termination::ClientGone, // default if dropped prematurely
            finalized: false,
            on_finalize,
        }
    }

    /// Mark the arrival of the first valid frame.
    pub fn record_first_frame(&mut self, now_ms: i64) {
        if self.first_frame_ms.is_none() {
            self.first_frame_ms = Some(now_ms);
        }
    }

    /// Update accumulated usage observation.
    pub fn update_usage(&mut self, usage: UsageAccumulator) {
        self.usage = usage;
    }

    /// Set the specific termination mode before finalization.
    pub fn set_termination(&mut self, termination: Termination) {
        self.termination = termination;
    }

    /// Deterministically finalize the request lifecycle exactly once with zero blocking I/O.
    pub fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        if let Some(cb) = self.on_finalize.take() {
            cb(self.termination, self.usage);
        }
    }
}

impl Drop for RequestLifecycle {
    fn drop(&mut self) {
        self.finalize();
    }
}
