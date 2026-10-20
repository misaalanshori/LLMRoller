//! Generalized rolling rings, anchored calendar month, and decaying UpstreamObservation.
//!
//! Spec: Section 6.9, Section 13.2, Section 13.2.1, Section 21.4, Requirements: LIM-1, Partial: USG-6, Partial: RTE-3.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// Maximum age of an upstream /usage observation in milliseconds (30 minutes).
pub const MAX_OBSERVATION_AGE_MS: i64 = 1_800_000;

/// Number of days in a given Gregorian month (clamping for leap years).
pub fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

pub fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 => 31,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        3 => 31,
        4 => 30,
        5 => 31,
        6 => 30,
        7 => 31,
        8 => 31,
        9 => 30,
        10 => 31,
        11 => 30,
        12 => 31,
        _ => 30,
    }
}

/// Convert UTC (year, month, day, minute_of_day) to epoch milliseconds.
pub fn utc_to_epoch_millis(year: i32, month: u8, day: u8, minute_of_day: u16) -> i64 {
    // Days from 1970-01-01
    // Algorithm based on Gregorian calendar calculations
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let m = if month <= 2 {
        month as i64 + 9
    } else {
        month as i64 - 3
    };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * m + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    (days * 86400 + minute_of_day as i64 * 60) * 1000
}

/// Single bucket in a rolling ring.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RingBucket {
    pub start_ms: i64,
    pub amount_micro_usd: i64,
}

/// Generalized rolling ring buffer of at most 360 buckets.
///
/// Bucket width: `b = max(60_000, div_ceil(period_ms, 360 * 60_000) * 60_000)`
/// Bucket count: `n = div_ceil(period_ms, b)`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollingRing {
    pub period_ms: i64,
    pub bucket_width_ms: i64,
    pub bucket_count: usize,
    pub buckets: Vec<RingBucket>,
}

impl RollingRing {
    pub fn new(period_ms: i64) -> Self {
        let period_ms = period_ms.clamp(3_600_000, 2_678_400_000); // 1h to 31d
        let minute_ms = 60_000;
        let denominator = 360 * minute_ms;
        let multiplier = (period_ms + denominator - 1) / denominator;
        let b = (multiplier * minute_ms).max(minute_ms);
        let n = (((period_ms + b - 1) / b) as usize).clamp(1, 360);

        Self {
            period_ms,
            bucket_width_ms: b,
            bucket_count: n,
            buckets: vec![RingBucket::default(); n],
        }
    }

    pub fn record(&mut self, timestamp_ms: i64, amount_micro_usd: i64) {
        if amount_micro_usd <= 0 {
            return;
        }
        let b_start = (timestamp_ms / self.bucket_width_ms) * self.bucket_width_ms;
        let idx = ((b_start / self.bucket_width_ms).rem_euclid(self.bucket_count as i64)) as usize;
        if self.buckets[idx].start_ms == b_start {
            self.buckets[idx].amount_micro_usd += amount_micro_usd;
        } else {
            self.buckets[idx] = RingBucket {
                start_ms: b_start,
                amount_micro_usd,
            };
        }
    }

    pub fn sum_at(&self, now_ms: i64) -> i64 {
        let min_start = now_ms - self.period_ms;
        let mut total: i64 = 0;
        for bucket in &self.buckets {
            if bucket.start_ms > min_start && bucket.start_ms <= now_ms {
                total += bucket.amount_micro_usd;
            }
        }
        total
    }
}

/// Manual calibration delta inserted into the ring bucket for effective_at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManualOffset {
    pub effective_at_ms: i64,
    pub delta_micro_usd: i64,
    pub period_ms: i64,
}

impl ManualOffset {
    pub fn value_at(&self, now_ms: i64) -> i64 {
        if now_ms >= self.effective_at_ms && now_ms < self.effective_at_ms + self.period_ms {
            self.delta_micro_usd
        } else {
            0
        }
    }
}

/// Upstream /usage observation with 30-minute decaying projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamObservation {
    pub used_micro_usd: i64,
    pub observed_at_ms: i64,
    pub local_at_observation: i64,
    pub resets_at_ms: Option<i64>,
}

impl UpstreamObservation {
    pub fn is_expired(&self, now_ms: i64) -> bool {
        let expiry = self
            .resets_at_ms
            .unwrap_or(i64::MAX)
            .min(self.observed_at_ms + MAX_OBSERVATION_AGE_MS);
        now_ms >= expiry
    }

    pub fn project(&self, local: i64, now_ms: i64) -> i64 {
        if self.is_expired(now_ms) {
            local
        } else {
            let growth = (local - self.local_at_observation).max(0);
            local.max(self.used_micro_usd + growth)
        }
    }
}

/// Fixed period window with anchor reset semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredFixed {
    pub period_ms: i64,
    pub anchor_at_ms: i64,
    pub used_micro_usd: i64,
    pub window_start_ms: i64,
    pub resets_at_ms: i64,
}

impl AnchoredFixed {
    pub fn new(period_ms: i64, anchor_at_ms: i64, now_ms: i64) -> Self {
        let mut s = Self {
            period_ms,
            anchor_at_ms,
            used_micro_usd: 0,
            window_start_ms: anchor_at_ms,
            resets_at_ms: anchor_at_ms + period_ms,
        };
        s.advance(now_ms);
        s
    }

    pub fn advance(&mut self, now_ms: i64) {
        if now_ms >= self.resets_at_ms {
            let diff = now_ms - self.resets_at_ms;
            let periods = (diff / self.period_ms) + 1;
            self.resets_at_ms += periods * self.period_ms;
            self.window_start_ms = self.resets_at_ms - self.period_ms;
            self.used_micro_usd = 0;
        }
    }

    pub fn record(&mut self, now_ms: i64, amount_micro_usd: i64) {
        self.advance(now_ms);
        if now_ms >= self.window_start_ms && now_ms < self.resets_at_ms {
            self.used_micro_usd += amount_micro_usd;
        }
    }

    pub fn used_at(&mut self, now_ms: i64) -> i64 {
        self.advance(now_ms);
        self.used_micro_usd
    }
}

/// Calendar-month anchored window with day-of-month clamping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredCalendarMonth {
    pub anchor_day: u8, // 1..=31
    pub anchor_minute: u16,
    pub current_year: i32,
    pub current_month: u8,
    pub window_start_ms: i64,
    pub resets_at_ms: i64,
    pub used_micro_usd: i64,
}

impl AnchoredCalendarMonth {
    pub fn new(anchor_day: u8, anchor_minute: u16, start_year: i32, start_month: u8) -> Self {
        let d = anchor_day.min(days_in_month(start_year, start_month));
        let window_start_ms = utc_to_epoch_millis(start_year, start_month, d, anchor_minute);

        let (next_y, next_m) = if start_month == 12 {
            (start_year + 1, 1)
        } else {
            (start_year, start_month + 1)
        };
        let next_d = anchor_day.min(days_in_month(next_y, next_m));
        let resets_at_ms = utc_to_epoch_millis(next_y, next_m, next_d, anchor_minute);

        Self {
            anchor_day,
            anchor_minute,
            current_year: start_year,
            current_month: start_month,
            window_start_ms,
            resets_at_ms,
            used_micro_usd: 0,
        }
    }

    pub fn advance(&mut self, now_ms: i64) {
        while now_ms >= self.resets_at_ms {
            self.current_year = if self.current_month == 12 {
                self.current_year + 1
            } else {
                self.current_year
            };
            self.current_month = if self.current_month == 12 {
                1
            } else {
                self.current_month + 1
            };

            self.window_start_ms = self.resets_at_ms;

            let (next_y, next_m) = if self.current_month == 12 {
                (self.current_year + 1, 1)
            } else {
                (self.current_year, self.current_month + 1)
            };
            let next_d = self.anchor_day.min(days_in_month(next_y, next_m));
            self.resets_at_ms = utc_to_epoch_millis(next_y, next_m, next_d, self.anchor_minute);
            self.used_micro_usd = 0;
        }
    }

    pub fn record(&mut self, now_ms: i64, amount_micro_usd: i64) {
        self.advance(now_ms);
        self.used_micro_usd += amount_micro_usd;
    }

    pub fn used_at(&mut self, now_ms: i64) -> i64 {
        self.advance(now_ms);
        self.used_micro_usd
    }
}

/// Provider window that tracks upstream resets_at timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredProvider {
    pub period_ms: i64,
    pub ring: RollingRing,
    pub observation: Option<UpstreamObservation>,
    pub window_start_ms: Option<i64>,
}

impl AnchoredProvider {
    pub fn new(period_ms: i64) -> Self {
        Self {
            period_ms,
            ring: RollingRing::new(period_ms),
            observation: None,
            window_start_ms: None,
        }
    }

    pub fn record(&mut self, timestamp_ms: i64, amount_micro_usd: i64) {
        self.ring.record(timestamp_ms, amount_micro_usd);
    }

    pub fn update_observation(&mut self, obs: UpstreamObservation) {
        if let Some(resets_at) = obs.resets_at_ms {
            self.window_start_ms = Some(resets_at - self.period_ms);
        }
        self.observation = Some(obs);
    }

    pub fn effective_used(&self, now_ms: i64) -> i64 {
        let local = match self.window_start_ms {
            Some(start) => {
                let mut sum = 0;
                for bucket in &self.ring.buckets {
                    if bucket.start_ms >= start && bucket.start_ms <= now_ms {
                        sum += bucket.amount_micro_usd;
                    }
                }
                sum
            }
            None => self.ring.sum_at(now_ms),
        };

        match &self.observation {
            None => local,
            Some(o) => o.project(local, now_ms),
        }
    }
}

/// Computes the cache-aware pre-flight reservation estimate in micro-USD.
///
/// Clamped to `[1_000, 2_000_000]` ($0.001 to $2.00) per SRDD Section 13.2.2.
pub fn estimate_reservation_cost_micro_usd(
    input_tokens_est: i64,
    cache_ratio: f64,
    rates: &crate::pricing::Rates,
    max_tokens: Option<i64>,
    model_max_output_tokens: Option<i64>,
) -> i64 {
    let cache_ratio = cache_ratio.clamp(0.0, 1.0);
    let in_rate = (1.0 - cache_ratio) * (rates.input_per_mtok_micro_usd as f64)
        + cache_ratio * (rates.cache_read_per_mtok_micro_usd as f64);
    let in_cost = (input_tokens_est as f64) * in_rate;

    let out_tokens_est = max_tokens
        .or(model_max_output_tokens)
        .unwrap_or(8192)
        .min(8192);
    let out_cost = (out_tokens_est as f64) * (rates.output_per_mtok_micro_usd as f64);

    let total = in_cost + out_cost;
    let est = (total / 1_000_000.0).ceil() as i64;
    est.clamp(1_000, 2_000_000)
}

/// Budget counter abstraction for CAS-based reservation.
pub trait BudgetCounter: Send + Sync {
    fn counter_id(&self) -> u64;
    fn effective_used(&self, now_ms: i64) -> i64;
    fn limit_micro_usd(&self) -> i64;
    fn reserve_percent(&self) -> i64;
    fn max_allowed_spend(&self) -> i64 {
        let pct = (100 - self.reserve_percent()).clamp(0, 100);
        (self.limit_micro_usd() * pct) / 100
    }
    fn reserved_in_flight(&self) -> &AtomicI64;
    fn record_actual(&self, now_ms: i64, amount_micro_usd: i64);
}

/// Thread-safe in-memory budget counter backed by a rolling ring and atomic reservation ledger.
pub struct AtomicBudgetCounter {
    pub id: u64,
    pub limit_micro_usd: i64,
    pub reserve_percent: i64,
    pub ring: RwLock<RollingRing>,
    pub reserved: AtomicI64,
    pub observation: RwLock<Option<UpstreamObservation>>,
}

impl AtomicBudgetCounter {
    pub fn new(id: u64, limit_micro_usd: i64, reserve_percent: i64, period_ms: i64) -> Self {
        Self {
            id,
            limit_micro_usd,
            reserve_percent,
            ring: RwLock::new(RollingRing::new(period_ms)),
            reserved: AtomicI64::new(0),
            observation: RwLock::new(None),
        }
    }
}

impl std::fmt::Debug for AtomicBudgetCounter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtomicBudgetCounter")
            .field("id", &self.id)
            .field("limit_micro_usd", &self.limit_micro_usd)
            .field("reserve_percent", &self.reserve_percent)
            .field(
                "reserved",
                &self.reserved.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish()
    }
}

impl BudgetCounter for AtomicBudgetCounter {
    fn counter_id(&self) -> u64 {
        self.id
    }

    fn effective_used(&self, now_ms: i64) -> i64 {
        let local = self.ring.read().sum_at(now_ms);
        let obs = *self.observation.read();
        match obs {
            None => local,
            Some(o) => o.project(local, now_ms),
        }
    }

    fn limit_micro_usd(&self) -> i64 {
        self.limit_micro_usd
    }

    fn reserve_percent(&self) -> i64 {
        self.reserve_percent
    }

    fn reserved_in_flight(&self) -> &AtomicI64 {
        &self.reserved
    }

    fn record_actual(&self, now_ms: i64, amount_micro_usd: i64) {
        self.ring.write().record(now_ms, amount_micro_usd);
    }
}

/// Error returned when reservation fails admission check on any counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservationError {
    LimitExceeded {
        counter_id: u64,
        effective_used: i64,
        reserved: i64,
        max_allowed: i64,
        requested: i64,
    },
}

/// RAII reservation guard that rolls back on abort/drop or posts actual spend on commit.
pub struct Reservation {
    counters: Vec<Arc<dyn BudgetCounter>>,
    pub estimate_micro_usd: i64,
    committed: bool,
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reservation")
            .field("counter_count", &self.counters.len())
            .field("estimate_micro_usd", &self.estimate_micro_usd)
            .field("committed", &self.committed)
            .finish()
    }
}

impl Reservation {
    /// Attempts to acquire an atomic reservation across all counters in fixed ID order.
    pub fn try_reserve(
        mut counters: Vec<Arc<dyn BudgetCounter>>,
        estimate_micro_usd: i64,
        now_ms: i64,
    ) -> Result<Self, ReservationError> {
        // Sort in fixed ascending order to prevent deadlocks
        counters.sort_by_key(|c| c.counter_id());

        let mut acquired: Vec<&Arc<dyn BudgetCounter>> = Vec::new();
        for counter in &counters {
            let max_allowed = counter.max_allowed_spend();
            loop {
                let current_reserved = counter.reserved_in_flight().load(Ordering::Acquire);
                let used = counter.effective_used(now_ms);
                if used + current_reserved + estimate_micro_usd > max_allowed {
                    // Roll back all previously acquired counters in reverse order
                    for prev in acquired.iter().rev() {
                        prev.reserved_in_flight()
                            .fetch_sub(estimate_micro_usd, Ordering::AcqRel);
                    }
                    return Err(ReservationError::LimitExceeded {
                        counter_id: counter.counter_id(),
                        effective_used: used,
                        reserved: current_reserved,
                        max_allowed,
                        requested: estimate_micro_usd,
                    });
                }

                if counter
                    .reserved_in_flight()
                    .compare_exchange_weak(
                        current_reserved,
                        current_reserved + estimate_micro_usd,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    acquired.push(counter);
                    break;
                }
            }
        }

        Ok(Self {
            counters,
            estimate_micro_usd,
            committed: false,
        })
    }

    /// Commits actual spend to counters and releases reservation.
    pub fn commit(mut self, actual_micro_usd: i64, now_ms: i64) {
        for counter in &self.counters {
            // Post actual cost first
            counter.record_actual(now_ms, actual_micro_usd);
            // Then release in-flight reservation
            counter
                .reserved_in_flight()
                .fetch_sub(self.estimate_micro_usd, Ordering::AcqRel);
        }
        self.committed = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.committed {
            for counter in &self.counters {
                counter
                    .reserved_in_flight()
                    .fetch_sub(self.estimate_micro_usd, Ordering::AcqRel);
            }
        }
    }
}

#[cfg(test)]
pub mod tests;
