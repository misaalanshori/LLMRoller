//! Weighted Round Robin and Smooth Weighted Round Robin selection engines.
//!
//! Spec: Section 3.1 (G2), Section 6.7, Section 10.3, Section 21.3,
//! Requirements: RTE-4, RTE-5, RTE-6, RTE-13, RTE-14.

use parking_lot::Mutex;
use std::collections::HashMap;

use crate::ids::ModelPk;
use crate::seams::EntropySource;
use crate::strategy::{RouteId, RouteLoadTracker, RouteSelector, RoutingStrategy, WrrGroupKey};

/// Eviction threshold in milliseconds for members absent from selection candidates.
pub const WRR_MEMBER_EVICT_MS: i64 = 300_000; // 5 minutes

/// Single member in a Smooth Weighted Round Robin selection group.
#[derive(Debug, Clone)]
pub struct WrrMember {
    pub route: RouteId,
    pub current: i64,
    pub last_present_ms: i64,
}

/// In-memory state for one SWRR group.
#[derive(Debug, Default)]
pub struct WrrGroup {
    pub members: Vec<WrrMember>,
    pub last_used_ms: i64,
}

/// Nginx Smooth Weighted Round Robin selector (RTE-4).
#[derive(Debug, Default)]
pub struct SmoothWeightedRrSelector {
    groups: Mutex<HashMap<WrrGroupKey, WrrGroup>>,
}

impl SmoothWeightedRrSelector {
    pub fn new() -> Self {
        Self {
            groups: Mutex::new(HashMap::new()),
        }
    }
}

impl RouteSelector for SmoothWeightedRrSelector {
    fn strategy(&self) -> RoutingStrategy {
        RoutingStrategy::SmoothWeightedRr
    }

    fn select_next(
        &self,
        group_key: &WrrGroupKey,
        candidates: &[(RouteId, i64)],
        now_ms: i64,
        _entropy: &dyn EntropySource,
        _load_tracker: Option<&dyn RouteLoadTracker>,
    ) -> Option<RouteId> {
        if candidates.is_empty() {
            return None;
        }

        let mut lock = self.groups.lock();
        let group = lock.entry(group_key.clone()).or_default();
        group.last_used_ms = now_ms;

        // Ensure all candidates exist in members
        for &(route, _) in candidates {
            if let Some(m) = group.members.iter_mut().find(|m| m.route == route) {
                m.last_present_ms = now_ms;
            } else {
                group.members.push(WrrMember {
                    route,
                    current: 0,
                    last_present_ms: now_ms,
                });
            }
        }

        // Evict members absent >= 300,000 ms and rebalance residual weight so sum(current) stays 0
        let mut evicted_current = 0i64;
        group.members.retain(|m| {
            let is_present = candidates.iter().any(|(r, _)| *r == m.route);
            if is_present || now_ms - m.last_present_ms < WRR_MEMBER_EVICT_MS {
                true
            } else {
                evicted_current += m.current;
                false
            }
        });

        if evicted_current != 0 && !group.members.is_empty() {
            let share = evicted_current / (group.members.len() as i64);
            let mut rem = evicted_current % (group.members.len() as i64);
            for m in &mut group.members {
                m.current += share;
                if rem > 0 {
                    m.current += 1;
                    rem -= 1;
                } else if rem < 0 {
                    m.current -= 1;
                    rem += 1;
                }
            }
        }

        // Step 1: m.current += W_eff for each present candidate
        let total_w: i64 = candidates.iter().map(|(_, w)| (*w).max(1)).sum();
        for m in &mut group.members {
            if let Some(&(_, w)) = candidates.iter().find(|(r, _)| *r == m.route) {
                m.current += w.max(1);
            }
        }

        // Step 2: Winner = argmax(m.current, r.id)
        let winner_idx = group
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| candidates.iter().any(|(r, _)| *r == m.route))
            .max_by(|(_, a), (_, b)| {
                a.current
                    .cmp(&b.current)
                    .then_with(|| b.route.cmp(&a.route))
            })
            .map(|(idx, _)| idx)?;

        // Step 3: Winner.current -= sum(W_eff)
        group.members[winner_idx].current -= total_w;
        let winner_route = group.members[winner_idx].route;

        Some(winner_route)
    }

    fn on_weights_updated(&self, group_key: &WrrGroupKey) {
        let mut lock = self.groups.lock();
        if let Some(group) = lock.get_mut(group_key) {
            for m in &mut group.members {
                m.current = 0;
            }
        }
    }

    fn reset_group(&self, group_key: &WrrGroupKey) {
        let mut lock = self.groups.lock();
        lock.remove(group_key);
    }

    fn reset_model(&self, model_pk: ModelPk) {
        let mut lock = self.groups.lock();
        lock.retain(|key, _| key.model_pk != model_pk);
    }

    fn evict_idle_groups(&self, now_ms: i64, max_idle_ms: i64) {
        let mut lock = self.groups.lock();
        lock.retain(|_, g| now_ms - g.last_used_ms < max_idle_ms);
    }
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a.abs()
}

/// State for Classical GCD block-weighted cyclic round-robin.
#[derive(Debug, Clone)]
pub struct ClassicalWrrGroup {
    pub index: usize,
    pub current_weight: i64,
    pub last_used_ms: i64,
}

/// Classical GCD block-weighted round-robin selector (RTE-5).
#[derive(Debug, Default)]
pub struct WeightedRrSelector {
    groups: Mutex<HashMap<WrrGroupKey, ClassicalWrrGroup>>,
}

impl WeightedRrSelector {
    pub fn new() -> Self {
        Self {
            groups: Mutex::new(HashMap::new()),
        }
    }
}

impl RouteSelector for WeightedRrSelector {
    fn strategy(&self) -> RoutingStrategy {
        RoutingStrategy::WeightedRr
    }

    fn select_next(
        &self,
        group_key: &WrrGroupKey,
        candidates: &[(RouteId, i64)],
        now_ms: i64,
        _entropy: &dyn EntropySource,
        _load_tracker: Option<&dyn RouteLoadTracker>,
    ) -> Option<RouteId> {
        if candidates.is_empty() {
            return None;
        }

        let n = candidates.len();
        let max_w = candidates
            .iter()
            .map(|(_, w)| (*w).max(1))
            .max()
            .unwrap_or(1);
        let gcd_w = candidates
            .iter()
            .map(|(_, w)| (*w).max(1))
            .fold(0, |acc, w| if acc == 0 { w } else { gcd(acc, w) })
            .max(1);

        let mut lock = self.groups.lock();
        let group = lock
            .entry(group_key.clone())
            .or_insert_with(|| ClassicalWrrGroup {
                index: n - 1,
                current_weight: 0,
                last_used_ms: now_ms,
            });
        group.last_used_ms = now_ms;

        let mut i = group.index;
        let mut cw = group.current_weight;

        loop {
            i = (i + 1) % n;
            if i == 0 {
                cw -= gcd_w;
                if cw <= 0 {
                    cw = max_w;
                    if cw == 0 {
                        return None;
                    }
                }
            }
            if candidates[i].1.max(1) >= cw {
                group.index = i;
                group.current_weight = cw;
                return Some(candidates[i].0);
            }
        }
    }

    fn on_weights_updated(&self, group_key: &WrrGroupKey) {
        let mut lock = self.groups.lock();
        if let Some(group) = lock.get_mut(group_key) {
            group.current_weight = 0;
            group.index = 0;
        }
    }

    fn reset_group(&self, group_key: &WrrGroupKey) {
        let mut lock = self.groups.lock();
        lock.remove(group_key);
    }

    fn reset_model(&self, model_pk: ModelPk) {
        let mut lock = self.groups.lock();
        lock.retain(|key, _| key.model_pk != model_pk);
    }

    fn evict_idle_groups(&self, now_ms: i64, max_idle_ms: i64) {
        let mut lock = self.groups.lock();
        lock.retain(|_, g| now_ms - g.last_used_ms < max_idle_ms);
    }
}

#[cfg(test)]
pub mod proptests;
