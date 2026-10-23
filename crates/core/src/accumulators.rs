//! UTC calendar-month accumulators for monthly caps and budgets.
//!
//! Spec: Section 6.2, Section 6.4, Section 6.5, Section 10.0, Section 13.2.2,
//! Requirements: USR-5, CLK-5, Partial: SHR-2.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::snapshot::CacheInvalidationEvent;

/// Gregorian calendar year and month in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UtcYearMonth {
    pub year: i32,
    pub month: u8, // 1..=12
}

impl UtcYearMonth {
    pub fn new(year: i32, month: u8) -> Self {
        assert!((1..=12).contains(&month), "month must be 1..=12");
        Self { year, month }
    }

    pub fn from_epoch_millis(now_ms: i64) -> Self {
        let secs = now_ms / 1000;
        let days = secs.div_euclid(86400);
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z.rem_euclid(146097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        let _ = d;
        Self {
            year: y as i32,
            month: m as u8,
        }
    }

    pub fn next(&self) -> Self {
        if self.month == 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    pub fn start_epoch_millis(&self) -> i64 {
        crate::windows::utc_to_epoch_millis(self.year, self.month, 1, 0)
    }

    pub fn seconds_until_next_month(&self, now_ms: i64) -> u64 {
        let next_start = self.next().start_epoch_millis();
        let diff = (next_start - now_ms) / 1000;
        diff.max(1) as u64
    }
}

/// In-memory monthly accumulator for a budget scope.
#[derive(Debug, Default)]
pub struct MonthlyAccumulator {
    pub used_micro_usd: AtomicI64,
    pub reserved_in_flight: AtomicI64,
    pub current_yyyymm: AtomicI64,
}

impl MonthlyAccumulator {
    pub fn new(initial_used_micro_usd: i64) -> Self {
        Self {
            used_micro_usd: AtomicI64::new(initial_used_micro_usd),
            reserved_in_flight: AtomicI64::new(0),
            current_yyyymm: AtomicI64::new(0),
        }
    }

    /// Atomically resets `used_micro_usd` when the UTC calendar month (`YYYYMM`) rolls over (`F-076`, `LIM-1`).
    pub fn ensure_month(&self, now_ms: i64) {
        let ym_struct = UtcYearMonth::from_epoch_millis(now_ms);
        let ym = i64::from(ym_struct.year) * 100 + i64::from(ym_struct.month);
        let prev = self.current_yyyymm.load(Ordering::Acquire);
        if prev == 0 {
            let _ =
                self.current_yyyymm
                    .compare_exchange(0, ym, Ordering::AcqRel, Ordering::Acquire);
        } else if ym > prev
            && self
                .current_yyyymm
                .compare_exchange(prev, ym, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            self.used_micro_usd.store(0, Ordering::Release);
        }
    }

    pub fn used(&self) -> i64 {
        self.used_micro_usd.load(Ordering::Acquire)
    }

    pub fn committed(&self) -> i64 {
        self.used()
    }

    pub fn reserved(&self) -> i64 {
        self.reserved_in_flight.load(Ordering::Acquire)
    }

    pub fn effective_used(&self) -> i64 {
        self.used() + self.reserved()
    }
}

/// Budget scope identifier for client key, user, or provider share.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BudgetScopeId {
    ClientKey(crate::ids::ClientKeyId),
    User(crate::ids::UserId),
    ProviderShare(crate::ids::ShareId),
}

/// Error returned when Gate G7 or a budget cap is exhausted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateG7Error {
    ClientKeyBudgetExhausted {
        retry_after_secs: u64,
        used_micro_usd: i64,
        budget_micro_usd_month: i64,
    },
    UserGlobalBudgetExhausted {
        used_micro_usd: i64,
        budget_micro_usd_month: i64,
    },
    ProviderShareCapExhausted {
        used_micro_usd: i64,
        max_monthly_micro_usd: i64,
    },
}

/// Thread-safe registry of monthly accumulators keyed by reservation month and scope ID.
#[derive(Debug, Default)]
pub struct MonthlyAccumulatorRegistry {
    accumulators: DashMap<(UtcYearMonth, BudgetScopeId), Arc<MonthlyAccumulator>>,
}

impl MonthlyAccumulatorRegistry {
    pub fn new() -> Self {
        Self {
            accumulators: DashMap::new(),
        }
    }

    /// Lookup or insert an accumulator for a given month and scope.
    pub fn get_or_create(
        &self,
        month: UtcYearMonth,
        scope_id: BudgetScopeId,
    ) -> Arc<MonthlyAccumulator> {
        self.accumulators
            .entry((month, scope_id))
            .or_insert_with(|| Arc::new(MonthlyAccumulator::default()))
            .clone()
    }

    /// Evaluates Gate G7 and acquires a CAS reservation against the monthly cap.
    pub fn try_reserve(
        &self,
        scope_id: BudgetScopeId,
        budget_cap_micro_usd: Option<i64>,
        estimate_micro_usd: i64,
        now_ms: i64,
    ) -> Result<MonthReservationGuard, GateG7Error> {
        let month = UtcYearMonth::from_epoch_millis(now_ms);
        let acc = self.get_or_create(month, scope_id.clone());

        if let Some(cap) = budget_cap_micro_usd {
            loop {
                let current_reserved = acc.reserved_in_flight.load(Ordering::Acquire);
                let used = acc.used_micro_usd.load(Ordering::Acquire);
                if used + current_reserved + estimate_micro_usd > cap {
                    match scope_id {
                        BudgetScopeId::ClientKey(_) => {
                            return Err(GateG7Error::ClientKeyBudgetExhausted {
                                retry_after_secs: month.seconds_until_next_month(now_ms),
                                used_micro_usd: used,
                                budget_micro_usd_month: cap,
                            });
                        }
                        BudgetScopeId::User(_) => {
                            return Err(GateG7Error::UserGlobalBudgetExhausted {
                                used_micro_usd: used,
                                budget_micro_usd_month: cap,
                            });
                        }
                        BudgetScopeId::ProviderShare(_) => {
                            return Err(GateG7Error::ProviderShareCapExhausted {
                                used_micro_usd: used,
                                max_monthly_micro_usd: cap,
                            });
                        }
                    }
                }

                if acc
                    .reserved_in_flight
                    .compare_exchange_weak(
                        current_reserved,
                        current_reserved + estimate_micro_usd,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    break;
                }
            }
        } else {
            acc.reserved_in_flight
                .fetch_add(estimate_micro_usd, Ordering::AcqRel);
        }

        Ok(MonthReservationGuard {
            accumulator: acc,
            reservation_month: month,
            scope_id,
            estimate_micro_usd,
            committed: false,
        })
    }

    /// Sweeper prunes entries for months < M+1 with reserved == 0 one hour after rollover.
    pub fn prune_old_months(&self, current_time_ms: i64) {
        let current_month = UtcYearMonth::from_epoch_millis(current_time_ms);
        let current_month_start = current_month.start_epoch_millis();
        // One hour after rollover (3,600,000 ms)
        if current_time_ms < current_month_start + 3_600_000 {
            return;
        }

        self.accumulators.retain(|(month, _scope_id), acc| {
            if *month < current_month {
                acc.reserved_in_flight.load(Ordering::Acquire) > 0
            } else {
                true
            }
        });
    }

    /// Merge another registry's committed usage into this one (startup hydration).
    pub fn merge_from(&self, other: &Self) {
        for entry in other.accumulators.iter() {
            let (month, scope_id) = entry.key();
            let used = entry
                .value()
                .used_micro_usd
                .load(std::sync::atomic::Ordering::Relaxed);
            if used == 0 {
                continue;
            }
            let target = self.get_or_create(*month, scope_id.clone());
            target
                .used_micro_usd
                .fetch_add(used, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Current committed usage for a scope in the month containing `now_ms`.
    #[must_use]
    pub fn usage_for(&self, scope_id: &BudgetScopeId, now_ms: i64) -> i64 {
        let month = UtcYearMonth::from_epoch_millis(now_ms);
        self.accumulators
            .get(&(month, scope_id.clone()))
            .map_or(0, |acc| acc.used())
    }

    /// Whether a scope has no remaining headroom for the month containing
    /// `now_ms`. A configured cap of zero admits no spend at all.
    #[must_use]
    pub fn is_exhausted(
        &self,
        scope_id: &BudgetScopeId,
        cap_micro_usd: Option<i64>,
        now_ms: i64,
    ) -> bool {
        let Some(cap) = cap_micro_usd else {
            return false;
        };
        if cap <= 0 {
            return true;
        }
        let month = UtcYearMonth::from_epoch_millis(now_ms);
        self.accumulators
            .get(&(month, scope_id.clone()))
            .is_some_and(|acc| acc.effective_used() >= cap)
    }

    /// Number of active accumulators in registry.
    pub fn len(&self) -> usize {
        self.accumulators.len()
    }

    /// Checks if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.accumulators.is_empty()
    }
}

/// Emits CacheInvalidationEvent::CapChanged when a monthly budget cap is lowered.
pub fn on_cap_changed() -> CacheInvalidationEvent {
    CacheInvalidationEvent::CapChanged
}

/// RAII reservation guard tied to a specific UTC reservation month.
#[derive(Debug)]
pub struct MonthReservationGuard {
    pub accumulator: Arc<MonthlyAccumulator>,
    pub reservation_month: UtcYearMonth,
    pub scope_id: BudgetScopeId,
    pub estimate_micro_usd: i64,
    committed: bool,
}

impl MonthReservationGuard {
    /// Posts actual spend to the reservation month's accumulator and releases reservation.
    pub fn commit(mut self, actual_cost_micro_usd: i64) {
        self.accumulator
            .used_micro_usd
            .fetch_add(actual_cost_micro_usd, Ordering::AcqRel);
        self.accumulator
            .reserved_in_flight
            .fetch_sub(self.estimate_micro_usd, Ordering::AcqRel);
        self.committed = true;
    }
}

impl Drop for MonthReservationGuard {
    fn drop(&mut self) {
        if !self.committed {
            self.accumulator
                .reserved_in_flight
                .fetch_sub(self.estimate_micro_usd, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
pub mod tests;
