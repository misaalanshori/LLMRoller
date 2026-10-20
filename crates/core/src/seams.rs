#![forbid(unsafe_code)]
//! Dependency-injection seams and secret wrappers.
//!
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#164-component-mocking-and-dependency-injection-architecture

use std::cell::RefCell;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rand::rngs::SysRng;
use rand::{Rng, SeedableRng, TryRng};
use rand_chacha::ChaCha12Rng;

pub use secrecy::{ExposeSecret, SecretSlice, SecretString};

/// In-memory secret byte buffer that zeroizes when dropped.
pub type SecretBytes = SecretSlice<u8>;

/// Wrap an owned UTF-8 secret for redacted formatting and zeroization on drop.
#[must_use]
pub fn secret_string(value: impl Into<String>) -> SecretString {
    SecretString::from(value.into())
}

/// Wrap owned secret bytes for redacted formatting and zeroization on drop.
#[must_use]
pub fn secret_bytes(value: impl Into<Vec<u8>>) -> SecretBytes {
    SecretBytes::new(value.into().into_boxed_slice())
}

/// Synchronous wall-clock and monotonic-clock seam.
///
/// `crates/core` intentionally exposes no asynchronous sleep operation.
pub trait Clock: Send + Sync {
    /// Current Unix wall-clock time in milliseconds.
    fn now_ms(&self) -> i64;

    /// Milliseconds elapsed on a monotonic timeline.
    fn monotonic_ms(&self) -> i64;
}

/// Production clock backed by `SystemTime` and `Instant`.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    monotonic_origin: Instant,
}

impl SystemClock {
    /// Create a clock whose monotonic timeline starts now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            monotonic_origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
            Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
        }
    }

    fn monotonic_ms(&self) -> i64 {
        i64::try_from(self.monotonic_origin.elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

/// Deterministic clock advanced explicitly by tests.
#[derive(Debug)]
pub struct FakeClock {
    now_ms: AtomicI64,
    monotonic_ms: AtomicI64,
}

impl FakeClock {
    /// Create a fake clock with both timelines starting at `start_ms`.
    #[must_use]
    pub const fn new(start_ms: i64) -> Self {
        Self {
            now_ms: AtomicI64::new(start_ms),
            monotonic_ms: AtomicI64::new(start_ms),
        }
    }

    /// Advance wall and monotonic time by the same non-negative amount.
    pub fn advance_ms(&self, delta_ms: i64) {
        assert!(delta_ms >= 0, "fake clock can only advance forward in time");
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst);
        self.monotonic_ms.fetch_add(delta_ms, Ordering::SeqCst);
    }

    /// Move only the monotonic timeline forward.
    pub fn advance_monotonic_ms(&self, delta_ms: i64) {
        assert!(
            delta_ms >= 0,
            "fake monotonic time can only advance forward"
        );
        self.monotonic_ms.fetch_add(delta_ms, Ordering::SeqCst);
    }

    /// Set wall-clock time independently, including a backwards NTP-style step.
    pub fn set_now_ms(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }

    fn monotonic_ms(&self) -> i64 {
        self.monotonic_ms.load(Ordering::SeqCst)
    }
}

/// Cryptographic and routing entropy seam.
pub trait EntropySource: Send + Sync {
    /// Fill the destination buffer with operating-system or deterministic entropy.
    fn fill_bytes(&self, dest: &mut [u8]);

    /// Return an unbiased value in `0..bound`.
    ///
    /// # Panics
    ///
    /// Panics when `bound` is zero.
    fn next_u64_bounded(&self, bound: u64) -> u64;
}

/// Production entropy source.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsEntropy;

impl OsEntropy {
    /// Create the operating-system entropy source.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

thread_local! {
    static ROUTING_RNG: RefCell<ChaCha12Rng> = RefCell::new(seed_routing_rng());
}

fn seed_routing_rng() -> ChaCha12Rng {
    let mut seed = [0_u8; 32];
    fill_os(&mut seed);
    ChaCha12Rng::from_seed(seed)
}

fn fill_os(dest: &mut [u8]) {
    SysRng
        .try_fill_bytes(dest)
        .unwrap_or_else(|error| panic!("operating-system entropy source failed: {error}"));
}

impl EntropySource for OsEntropy {
    fn fill_bytes(&self, dest: &mut [u8]) {
        fill_os(dest);
    }

    fn next_u64_bounded(&self, bound: u64) -> u64 {
        bounded_u64(bound, || {
            ROUTING_RNG.with(|rng| rng.borrow_mut().next_u64())
        })
    }
}

/// Seeded ChaCha12 entropy source for reproducible tests and fixture vectors.
pub struct DeterministicRng {
    rng: Mutex<ChaCha12Rng>,
}

impl DeterministicRng {
    /// Create a deterministic source from a 64-bit test seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Mutex::new(ChaCha12Rng::seed_from_u64(seed)),
        }
    }

    /// Create a deterministic source from an explicit 256-bit seed.
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            rng: Mutex::new(ChaCha12Rng::from_seed(seed)),
        }
    }

    fn with_rng<R>(&self, operation: impl FnOnce(&mut ChaCha12Rng) -> R) -> R {
        match self.rng.lock() {
            Ok(mut rng) => operation(&mut rng),
            Err(poisoned) => operation(&mut poisoned.into_inner()),
        }
    }
}

impl fmt::Debug for DeterministicRng {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeterministicRng")
            .finish_non_exhaustive()
    }
}

impl EntropySource for DeterministicRng {
    fn fill_bytes(&self, dest: &mut [u8]) {
        self.with_rng(|rng| rng.fill_bytes(dest));
    }

    fn next_u64_bounded(&self, bound: u64) -> u64 {
        bounded_u64(bound, || self.with_rng(Rng::next_u64))
    }
}

fn bounded_u64(bound: u64, mut next: impl FnMut() -> u64) -> u64 {
    assert!(bound > 0, "bound must be greater than zero");
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = next();
        if value >= threshold {
            return value % bound;
        }
    }
}

#[cfg(test)]
mod tests;
