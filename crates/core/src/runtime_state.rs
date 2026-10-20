#![forbid(unsafe_code)]
//! In-memory runtime state for health, circuit breakers, and strategy groups.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#51-high-level-architecture-diagram
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#215-versioned-health-synchronization-and-origin-circuit-breakers

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Monotonic health version tracker for upstream keys and routes.
#[derive(Debug, Default)]
pub struct HealthState {
    pub version: AtomicU64,
}

impl HealthState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
        }
    }

    pub fn bump_version(&self) -> u64 {
        self.version.fetch_add(1, Ordering::SeqCst) + 1
    }

    #[must_use]
    pub fn current_version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }
}

/// Global shared in-memory runtime state across proxy and control planes.
#[derive(Debug, Default)]
pub struct RuntimeState {
    pub health: Arc<HealthState>,
    pub last_reload_ts: AtomicI64,
}

impl RuntimeState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            health: Arc::new(HealthState::new()),
            last_reload_ts: AtomicI64::new(0),
        }
    }
}
