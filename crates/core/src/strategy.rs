#![forbid(unsafe_code)]
//! Pluggable routing strategy definitions.
//!
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#103-pluggable-routing-strategy-engine-effective-priority-and-headroom-scaling
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#1031-common-routeselector-trait-and-routingstrategy-enum-rte-4-rte-14

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ids::{ModelPk, UpstreamKeyId, UserId};
use crate::seams::EntropySource;

/// Key identifying a target selection group so one user's cursor never perturbs another (Section 10.3.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WrrGroupKey {
    pub user_id: UserId,
    pub model_pk: ModelPk,
    pub protocol: String,
    pub pool_tier: u8,
}

/// Composite route identifier (upstream_key_id, model_pk).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RouteId {
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: ModelPk,
}

/// Trait providing live upstream load metrics for Power of Two Choices.
pub trait RouteLoadTracker: Send + Sync {
    fn in_flight(&self, route: &RouteId) -> u32;
    fn ewma_ttfb_ms(&self, route: &RouteId) -> u64;
}

/// Common thread-safe selector trait implemented by all six routing algorithms.
pub trait RouteSelector: Send + Sync {
    fn strategy(&self) -> RoutingStrategy;
    fn select_next(
        &self,
        group_key: &WrrGroupKey,
        candidates: &[(RouteId, i64)],
        now_ms: i64,
        entropy: &dyn EntropySource,
        load_tracker: Option<&dyn RouteLoadTracker>,
    ) -> Option<RouteId>;
    fn on_weights_updated(&self, group_key: &WrrGroupKey);
    fn reset_group(&self, group_key: &WrrGroupKey);
    /// Drop all per-group cursor state belonging to a model whose route set changed.
    fn reset_model(&self, _model_pk: ModelPk) {}
    fn evict_idle_groups(&self, now_ms: i64, max_idle_ms: i64);
}

/// Computes effective weight W_eff scaled by remaining budget headroom basis points (RTE-13).
pub fn compute_effective_weight(
    base_weight: i64,
    windows: &[(i64, i64, i64)],
    headroom_scaling_enabled: bool,
) -> i64 {
    let base = base_weight.clamp(1, 1000);
    if !headroom_scaling_enabled || windows.is_empty() {
        return base;
    }
    let mut min_bps = 10_000i64;
    for &(limit, used, reserved) in windows {
        if limit > 0 {
            let remaining = (limit - used - reserved).max(0);
            let bps = (10_000 * remaining) / limit;
            min_bps = min_bps.min(bps);
        }
    }
    let bps = min_bps.clamp(500, 10_000);
    ((base * bps) / 10_000).max(1)
}

/// Filters candidates to minimum strict tier when models.distribution == 'strict' (RTE-6).
pub fn filter_strict_tier<T: Copy>(candidates: &[(T, Option<i64>)]) -> Vec<T> {
    let min_tier = candidates.iter().filter_map(|(_, t)| *t).min();
    match min_tier {
        Some(best) => candidates
            .iter()
            .filter(|(_, t)| *t == Some(best))
            .map(|(c, _)| *c)
            .collect(),
        None => candidates.iter().map(|(c, _)| *c).collect(),
    }
}

/// State for an unweighted naive round-robin group.
#[derive(Debug, Default)]
pub struct NaiveRrGroup {
    pub cursor: AtomicU64,
    pub last_used_ms: AtomicI64,
}

/// Unweighted cyclic round-robin selector (naive_rr) (RTE-14).
#[derive(Debug, Default)]
pub struct NaiveRrSelector {
    groups: Mutex<HashMap<WrrGroupKey, Arc<NaiveRrGroup>>>,
}

impl NaiveRrSelector {
    pub fn new() -> Self {
        Self {
            groups: Mutex::new(HashMap::new()),
        }
    }
}

impl RouteSelector for NaiveRrSelector {
    fn strategy(&self) -> RoutingStrategy {
        RoutingStrategy::NaiveRr
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
        let group = {
            let mut lock = self.groups.lock();
            lock.entry(group_key.clone())
                .or_insert_with(|| Arc::new(NaiveRrGroup::default()))
                .clone()
        };
        group.last_used_ms.store(now_ms, Ordering::Relaxed);
        let seq = group.cursor.fetch_add(1, Ordering::Relaxed);
        let idx = (seq % (candidates.len() as u64)) as usize;
        Some(candidates[idx].0)
    }

    fn on_weights_updated(&self, _group_key: &WrrGroupKey) {}

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
        lock.retain(|_, g| now_ms - g.last_used_ms.load(Ordering::Relaxed) < max_idle_ms);
    }
}

/// Uniform pseudo-random selector (uniform_random / random) (RTE-14).
#[derive(Debug, Default)]
pub struct UniformRandomSelector;

impl RouteSelector for UniformRandomSelector {
    fn strategy(&self) -> RoutingStrategy {
        RoutingStrategy::UniformRandom
    }

    fn select_next(
        &self,
        _group_key: &WrrGroupKey,
        candidates: &[(RouteId, i64)],
        _now_ms: i64,
        entropy: &dyn EntropySource,
        _load_tracker: Option<&dyn RouteLoadTracker>,
    ) -> Option<RouteId> {
        if candidates.is_empty() {
            return None;
        }
        let draw = entropy.next_u64_bounded(candidates.len() as u64) as usize;
        Some(candidates[draw].0)
    }

    fn on_weights_updated(&self, _group_key: &WrrGroupKey) {}
    fn reset_group(&self, _group_key: &WrrGroupKey) {}
    fn evict_idle_groups(&self, _now_ms: i64, _max_idle_ms: i64) {}
}

/// Weighted pseudo-random cumulative prefix binary search selector (weighted_random) (RTE-14).
#[derive(Debug, Default)]
pub struct WeightedRandomSelector;

impl RouteSelector for WeightedRandomSelector {
    fn strategy(&self) -> RoutingStrategy {
        RoutingStrategy::WeightedRandom
    }

    fn select_next(
        &self,
        _group_key: &WrrGroupKey,
        candidates: &[(RouteId, i64)],
        _now_ms: i64,
        entropy: &dyn EntropySource,
        _load_tracker: Option<&dyn RouteLoadTracker>,
    ) -> Option<RouteId> {
        if candidates.is_empty() {
            return None;
        }
        let mut prefix = Vec::with_capacity(candidates.len());
        let mut acc: i64 = 0;
        for (_, w) in candidates {
            acc += (*w).max(1);
            prefix.push(acc);
        }
        let draw = entropy.next_u64_bounded(acc as u64) as i64;
        let idx = prefix.partition_point(|&cum| cum <= draw);
        let safe_idx = idx.min(candidates.len() - 1);
        Some(candidates[safe_idx].0)
    }

    fn on_weights_updated(&self, _group_key: &WrrGroupKey) {}
    fn reset_group(&self, _group_key: &WrrGroupKey) {}
    fn evict_idle_groups(&self, _now_ms: i64, _max_idle_ms: i64) {}
}

/// Power of Two Choices selector (power_of_two_choices / p2c) (RTE-14).
#[derive(Debug, Default)]
pub struct PowerOfTwoChoicesSelector;

impl RouteSelector for PowerOfTwoChoicesSelector {
    fn strategy(&self) -> RoutingStrategy {
        RoutingStrategy::PowerOfTwoChoices
    }

    fn select_next(
        &self,
        group_key: &WrrGroupKey,
        candidates: &[(RouteId, i64)],
        now_ms: i64,
        entropy: &dyn EntropySource,
        load_tracker: Option<&dyn RouteLoadTracker>,
    ) -> Option<RouteId> {
        if candidates.is_empty() {
            return None;
        }
        if candidates.len() == 1 {
            return Some(candidates[0].0);
        }

        let wr = WeightedRandomSelector;
        let first = wr.select_next(group_key, candidates, now_ms, entropy, load_tracker)?;

        let remaining: Vec<(RouteId, i64)> = candidates
            .iter()
            .filter(|(r, _)| *r != first)
            .copied()
            .collect();
        let second = if remaining.is_empty() {
            first
        } else {
            wr.select_next(group_key, &remaining, now_ms, entropy, load_tracker)?
        };

        if let Some(tracker) = load_tracker {
            let score = |r: &RouteId| -> (u64, RouteId) {
                let in_flight = tracker.in_flight(r) as u64;
                let ttfb_ms = tracker.ewma_ttfb_ms(r).max(1);
                let w = candidates
                    .iter()
                    .find(|(candidate_r, _)| candidate_r == r)
                    .map(|(_, weight)| *weight)
                    .unwrap_or(1)
                    .max(1) as u64;
                (((in_flight + 1) * ttfb_ms * 1000) / w, *r)
            };
            let winner = score(&first).min(score(&second)).1;
            Some(winner)
        } else {
            Some(first)
        }
    }

    fn on_weights_updated(&self, _group_key: &WrrGroupKey) {}
    fn reset_group(&self, _group_key: &WrrGroupKey) {}
    fn evict_idle_groups(&self, _now_ms: i64, _max_idle_ms: i64) {}
}

#[cfg(test)]
pub mod proptests;
///
/// Partial: RTE-14
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum RoutingStrategy {
    /// Nginx smooth weighted round-robin (default).
    #[default]
    SmoothWeightedRr,
    /// Unweighted cyclic round-robin.
    NaiveRr,
    /// Classical GCD block-weighted cyclic round-robin.
    WeightedRr,
    /// Uniform pseudo-random distribution (alias "random").
    UniformRandom,
    /// Cumulative-weight binary search random distribution.
    WeightedRandom,
    /// Power of two choices sampling (alias "p2c").
    PowerOfTwoChoices,
}

impl RoutingStrategy {
    /// Canonical snake_case name of the strategy.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SmoothWeightedRr => "smooth_weighted_rr",
            Self::NaiveRr => "naive_rr",
            Self::WeightedRr => "weighted_rr",
            Self::UniformRandom => "uniform_random",
            Self::WeightedRandom => "weighted_random",
            Self::PowerOfTwoChoices => "power_of_two_choices",
        }
    }

    /// Whether this strategy considers route priority weights `W_eff`.
    #[must_use]
    pub const fn is_weighted(self) -> bool {
        match self {
            Self::SmoothWeightedRr
            | Self::WeightedRr
            | Self::WeightedRandom
            | Self::PowerOfTwoChoices => true,
            Self::NaiveRr | Self::UniformRandom => false,
        }
    }
}

impl fmt::Display for RoutingStrategy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Parse error for invalid routing strategy strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidStrategyError(pub String);

impl fmt::Display for InvalidStrategyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid routing strategy {:?}; valid strategies: smooth_weighted_rr, naive_rr, weighted_rr, uniform_random (random), weighted_random, power_of_two_choices (p2c)",
            self.0
        )
    }
}

impl std::error::Error for InvalidStrategyError {}

impl FromStr for RoutingStrategy {
    type Err = InvalidStrategyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "smooth_weighted_rr" => Ok(Self::SmoothWeightedRr),
            "naive_rr" => Ok(Self::NaiveRr),
            "weighted_rr" => Ok(Self::WeightedRr),
            "uniform_random" | "random" => Ok(Self::UniformRandom),
            "weighted_random" => Ok(Self::WeightedRandom),
            "power_of_two_choices" | "p2c" => Ok(Self::PowerOfTwoChoices),
            other => Err(InvalidStrategyError(other.to_owned())),
        }
    }
}

impl Serialize for RoutingStrategy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RoutingStrategy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Partial: RTE-14"]
    fn strategy_from_str_and_alias_deserialization() {
        assert_eq!(
            RoutingStrategy::from_str("smooth_weighted_rr").expect("parse SWRR"),
            RoutingStrategy::SmoothWeightedRr
        );
        assert_eq!(
            RoutingStrategy::from_str("naive_rr").expect("parse Naive"),
            RoutingStrategy::NaiveRr
        );
        assert_eq!(
            RoutingStrategy::from_str("weighted_rr").expect("parse WRR"),
            RoutingStrategy::WeightedRr
        );
        assert_eq!(
            RoutingStrategy::from_str("uniform_random").expect("parse Uniform"),
            RoutingStrategy::UniformRandom
        );
        assert_eq!(
            RoutingStrategy::from_str("random").expect("parse random alias"),
            RoutingStrategy::UniformRandom
        );
        assert_eq!(
            RoutingStrategy::from_str("weighted_random").expect("parse WeightedRandom"),
            RoutingStrategy::WeightedRandom
        );
        assert_eq!(
            RoutingStrategy::from_str("power_of_two_choices").expect("parse P2C"),
            RoutingStrategy::PowerOfTwoChoices
        );
        assert_eq!(
            RoutingStrategy::from_str("p2c").expect("parse p2c alias"),
            RoutingStrategy::PowerOfTwoChoices
        );
        assert!(RoutingStrategy::from_str("unknown_strategy").is_err());

        // Serde JSON roundtrip and alias deserialization
        let json_str = "\"random\"";
        let deserialized: RoutingStrategy =
            serde_json::from_str(json_str).expect("deserialize random");
        assert_eq!(deserialized, RoutingStrategy::UniformRandom);

        let json_p2c = "\"p2c\"";
        let deserialized_p2c: RoutingStrategy =
            serde_json::from_str(json_p2c).expect("deserialize p2c");
        assert_eq!(deserialized_p2c, RoutingStrategy::PowerOfTwoChoices);

        let serialized = serde_json::to_string(&RoutingStrategy::UniformRandom).expect("serialize");
        assert_eq!(serialized, "\"uniform_random\"");
    }
}
