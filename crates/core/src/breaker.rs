//! Per-origin circuit breaker state machine (OriginBreaker).
//!
//! Spec: Section 10.5.1, Section 21.5, Requirements: REL-5, API-8.

/// Key identifying an upstream HTTP host and port.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OriginKey {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub allow_private_network: bool,
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

    /// Evaluates Check C7 for this origin. Returns true if request is admitted.
    pub fn is_admitted(&mut self, now_ms: i64) -> bool {
        match self.state {
            BreakerState::Closed => true,
            BreakerState::Open { until_ms } => {
                if now_ms >= until_ms {
                    self.state = BreakerState::HalfOpen {
                        probe_in_flight: true,
                    };
                    true
                } else {
                    false
                }
            }
            BreakerState::HalfOpen {
                ref mut probe_in_flight,
            } => {
                if *probe_in_flight {
                    false
                } else {
                    *probe_in_flight = true;
                    true
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
