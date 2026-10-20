#![forbid(unsafe_code)]
//! Asynchronous clock extension for proxy retry and sleep delays.
//!
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#164-component-mocking-and-dependency-injection-architecture

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use llmroller_core::seams::{Clock, FakeClock, SystemClock};

/// Asynchronous extension of the synchronous [`Clock`] seam.
///
/// The returned future is boxed so the trait remains object-safe for runtime state.
pub trait AsyncClock: Clock {
    /// Sleep for the requested duration.
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Production asynchronous clock backed by Tokio timers.
#[derive(Debug, Clone, Copy, Default)]
pub struct AsyncSystemClock {
    clock: SystemClock,
}

impl AsyncSystemClock {
    /// Create a clock whose monotonic timeline starts now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clock: SystemClock::new(),
        }
    }

    /// Wrap an existing synchronous system clock.
    #[must_use]
    pub const fn with_clock(clock: SystemClock) -> Self {
        Self { clock }
    }

    /// Borrow the underlying synchronous clock.
    #[must_use]
    pub const fn clock(&self) -> &SystemClock {
        &self.clock
    }
}

impl Clock for AsyncSystemClock {
    fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    fn monotonic_ms(&self) -> i64 {
        self.clock.monotonic_ms()
    }
}

impl AsyncClock for AsyncSystemClock {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Deterministic asynchronous clock that advances a [`FakeClock`] instead of sleeping.
#[derive(Debug, Default)]
pub struct FakeAsyncClock {
    clock: FakeClock,
}

impl FakeAsyncClock {
    /// Create a fake asynchronous clock starting at `start_ms`.
    #[must_use]
    pub const fn new(start_ms: i64) -> Self {
        Self {
            clock: FakeClock::new(start_ms),
        }
    }

    /// Wrap an existing fake clock.
    #[must_use]
    pub fn from_clock(clock: FakeClock) -> Self {
        Self { clock }
    }

    /// Advance wall and monotonic time by the same non-negative amount.
    pub fn advance_ms(&self, delta_ms: i64) {
        self.clock.advance_ms(delta_ms);
    }

    /// Move only the monotonic timeline forward.
    pub fn advance_monotonic_ms(&self, delta_ms: i64) {
        self.clock.advance_monotonic_ms(delta_ms);
    }

    /// Set wall-clock time independently, including a backwards NTP-style step.
    pub fn set_now_ms(&self, now_ms: i64) {
        self.clock.set_now_ms(now_ms);
    }

    /// Borrow the underlying fake clock.
    #[must_use]
    pub const fn clock(&self) -> &FakeClock {
        &self.clock
    }
}

impl Clock for FakeAsyncClock {
    fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    fn monotonic_ms(&self) -> i64 {
        self.clock.monotonic_ms()
    }
}

impl AsyncClock for FakeAsyncClock {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.clock
            .advance_ms(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX));
        Box::pin(std::future::ready(()))
    }
}

#[cfg(test)]
mod tests;
