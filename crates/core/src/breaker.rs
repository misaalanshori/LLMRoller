//! Per-origin circuit breaker state machine (OriginBreaker).
//!
//! Spec: Section 10.5.1, Section 21.5, Requirements: REL-5, API-8.

use std::sync::Arc;

use parking_lot::Mutex;

/// Key identifying an upstream HTTP host and port.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OriginKey {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub allow_private_network: bool,
}

impl OriginKey {
    /// Derive the canonical origin for a provider base URL (REL-5).
    ///
    /// A URL that cannot be parsed is keyed verbatim so an unusable provider
    /// still gets an isolated breaker instead of sharing another origin's.
    #[must_use]
    pub fn from_base_url(base_url: &str, allow_private_network: bool) -> Self {
        match url::Url::parse(base_url) {
            Ok(parsed) => Self {
                scheme: parsed.scheme().to_ascii_lowercase(),
                host: parsed
                    .host_str()
                    .map_or_else(|| base_url.to_string(), str::to_ascii_lowercase),
                port: parsed.port_or_known_default().unwrap_or(0),
                allow_private_network,
            },
            Err(_) => Self {
                scheme: String::new(),
                host: base_url.trim().to_string(),
                port: 0,
                allow_private_network,
            },
        }
    }
}

/// Circuit breaker state for an upstream origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open { until_ms: i64 },
    HalfOpen { probe_in_flight: bool },
}

/// Rolling 10-second traffic bucket.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bucket10s {
    pub start_10s: u32, // epoch_millis / 10_000
    pub attempts: u32,
    pub host_failures: u32,
}

/// Per-origin circuit breaker state machine.
#[derive(Debug)]
pub struct OriginBreaker {
    pub state: BreakerState,
    pub consecutive_host_failures: u32,
    pub open_streak: u32,
    pub buckets: [Bucket10s; 6],
}

impl Default for OriginBreaker {
    fn default() -> Self {
        Self {
            state: BreakerState::Closed,
            consecutive_host_failures: 0,
            open_streak: 0,
            buckets: [Bucket10s::default(); 6],
        }
    }
}

impl OriginBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluates Check C7 without claiming the half-open probe slot.
    ///
    /// Eligibility and preflight evaluation must not mutate the breaker, so the
    /// probe slot is only taken by [`Self::try_acquire`] immediately before an
    /// upstream attempt.
    #[must_use]
    pub fn is_admitted(&self, now_ms: i64) -> bool {
        match self.state {
            BreakerState::Closed => true,
            BreakerState::Open { until_ms } => now_ms >= until_ms,
            BreakerState::HalfOpen { probe_in_flight } => !probe_in_flight,
        }
    }

    /// Claims admission for one upstream attempt. Returns `Some(is_probe)` when
    /// admitted, where `is_probe` marks the single half-open probe slot, or
    /// `None` when the breaker rejects the attempt (C7).
    pub fn try_acquire(&mut self, now_ms: i64) -> Option<bool> {
        match self.state {
            BreakerState::Closed => Some(false),
            BreakerState::Open { until_ms } => {
                if now_ms >= until_ms {
                    self.state = BreakerState::HalfOpen {
                        probe_in_flight: true,
                    };
                    Some(true)
                } else {
                    None
                }
            }
            BreakerState::HalfOpen { probe_in_flight } => {
                if probe_in_flight {
                    None
                } else {
                    self.state = BreakerState::HalfOpen {
                        probe_in_flight: true,
                    };
                    Some(true)
                }
            }
        }
    }

    /// Records an upstream response (any status other than 502/503/504 before frame commit).
    pub fn record_success(&mut self, now_ms: i64) {
        match self.state {
            BreakerState::HalfOpen { .. } => {
                self.state = BreakerState::Closed;
                self.open_streak = 0;
                self.consecutive_host_failures = 0;
                self.buckets = [Bucket10s::default(); 6];
            }
            BreakerState::Closed => {
                self.consecutive_host_failures = 0;
                let b = self.get_current_bucket_mut(now_ms);
                b.attempts += 1;
            }
            BreakerState::Open { .. } => {}
        }
    }

    /// Records a host failure (Connect, Tls, WriteStalled, or 502/503/504 before first frame).
    pub fn record_host_failure(&mut self, now_ms: i64) {
        match self.state {
            BreakerState::HalfOpen { .. } => {
                self.open_streak += 1;
                let shift = self.open_streak.min(5);
                let backoff = (15_000i64 * (1i64 << shift)).min(120_000);
                self.state = BreakerState::Open {
                    until_ms: now_ms + backoff,
                };
            }
            BreakerState::Closed => {
                self.consecutive_host_failures += 1;
                let b = self.get_current_bucket_mut(now_ms);
                b.attempts += 1;
                b.host_failures += 1;

                if self.consecutive_host_failures >= 5 {
                    self.state = BreakerState::Open {
                        until_ms: now_ms + 15_000,
                    };
                    return;
                }

                // Check 60-second window ratio
                let current_10s = (now_ms / 10_000) as u32;
                let mut window_attempts = 0u32;
                let mut window_failures = 0u32;
                for bucket in &self.buckets {
                    if current_10s.saturating_sub(bucket.start_10s) < 6 {
                        window_attempts += bucket.attempts;
                        window_failures += bucket.host_failures;
                    }
                }

                if window_attempts >= 20 && (window_failures as f64 / window_attempts as f64) >= 0.5
                {
                    self.state = BreakerState::Open {
                        until_ms: now_ms + 15_000,
                    };
                }
            }
            BreakerState::Open { .. } => {}
        }
    }

    fn get_current_bucket_mut(&mut self, now_ms: i64) -> &mut Bucket10s {
        let current_10s = (now_ms / 10_000) as u32;
        let idx = (current_10s % 6) as usize;
        if self.buckets[idx].start_10s != current_10s {
            self.buckets[idx] = Bucket10s {
                start_10s: current_10s,
                attempts: 0,
                host_failures: 0,
            };
        }
        &mut self.buckets[idx]
    }
}

/// RAII permit for one admitted upstream attempt.
///
/// If the attempt is a half-open probe and no outcome is recorded (client
/// disconnect, cancellation, panic, or timeout), `Drop` releases the probe slot
/// so the origin cannot wedge in `HalfOpen` forever (REL-5, Section 21.5).
#[derive(Debug)]
pub struct OriginProbeGuard {
    breaker: Arc<Mutex<OriginBreaker>>,
    is_probe: bool,
}

impl OriginProbeGuard {
    #[must_use]
    pub fn new(breaker: Arc<Mutex<OriginBreaker>>, is_probe: bool) -> Self {
        Self { breaker, is_probe }
    }
}

impl Drop for OriginProbeGuard {
    fn drop(&mut self) {
        if !self.is_probe {
            return;
        }
        let mut breaker = self.breaker.lock();
        if let BreakerState::HalfOpen { probe_in_flight } = &mut breaker.state
            && *probe_in_flight
        {
            *probe_in_flight = false;
        }
    }
}
