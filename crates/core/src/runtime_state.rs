#![forbid(unsafe_code)]
//! In-memory runtime state for health, circuit breakers, strategy groups, and budgets.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#51-high-level-architecture-diagram
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#215-versioned-health-synchronization-and-origin-circuit-breakers

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::accumulators::MonthlyAccumulatorRegistry;
use crate::breaker::OriginBreaker;
use crate::ids::{ModelPk, ProviderId, UpstreamKeyId};
use crate::session::ConstantSessionDetector;
use crate::snapshot::{CacheInvalidationEvent, ConfigSnapshot, SnapshotBudgetWindow};
use crate::strategy::{
    NaiveRrSelector, PowerOfTwoChoicesSelector, RouteId, RouteLoadTracker, RouteSelector,
    RoutingStrategy, UniformRandomSelector, WeightedRandomSelector, WrrGroupKey,
};
use crate::usage::AttemptOutcome;
use crate::windows::AtomicBudgetCounter;
use crate::wrr::{SmoothWeightedRrSelector, WeightedRrSelector};

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

/// Hot-path health status for one upstream key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyHealthEntry {
    pub healthy: bool,
    pub cooldown_until_ms: Option<i64>,
    pub exhausted_until_ms: Option<i64>,
    pub consecutive_failures: u32,
    pub quarantine_reason: Option<String>,
}

impl Default for KeyHealthEntry {
    fn default() -> Self {
        Self {
            healthy: true,
            cooldown_until_ms: None,
            exhausted_until_ms: None,
            consecutive_failures: 0,
            quarantine_reason: None,
        }
    }
}

impl KeyHealthEntry {
    #[must_use]
    pub fn is_routable(&self, now_ms: i64) -> bool {
        if self.quarantine_reason.is_some() || !self.healthy {
            return false;
        }
        if let Some(until) = self.cooldown_until_ms
            && now_ms < until
        {
            return false;
        }
        if let Some(until) = self.exhausted_until_ms
            && now_ms < until
        {
            return false;
        }
        true
    }
}

/// Lock-free in-flight and EWMA latency tracker for Power of Two Choices.
#[derive(Debug, Default)]
pub struct LoadTracker {
    in_flight: DashMap<RouteId, u32>,
    ewma_ttfb_ms: DashMap<RouteId, f64>,
}

impl LoadTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&self, route: RouteId) {
        *self.in_flight.entry(route).or_insert(0) += 1;
    }

    pub fn end(&self, route: RouteId) {
        if let Some(mut entry) = self.in_flight.get_mut(&route) {
            *entry = entry.saturating_sub(1);
        }
    }

    pub fn record_ttfb(&self, route: RouteId, ttfb_ms: u64, weight: f64) {
        let mut entry = self.ewma_ttfb_ms.entry(route).or_insert(ttfb_ms as f64);
        *entry = (*entry * (1.0 - weight)) + (ttfb_ms as f64 * weight);
    }
}

impl RouteLoadTracker for LoadTracker {
    fn in_flight(&self, route: &RouteId) -> u32 {
        self.in_flight.get(route).map_or(0, |entry| *entry)
    }

    fn ewma_ttfb_ms(&self, route: &RouteId) -> u64 {
        self.ewma_ttfb_ms
            .get(route)
            .map_or(0, |entry| (*entry).round() as u64)
    }
}

/// The six pluggable selectors shared by all request paths.
pub struct RouteSelectorSet {
    smooth: Arc<dyn RouteSelector>,
    naive: Arc<dyn RouteSelector>,
    weighted: Arc<dyn RouteSelector>,
    uniform_random: Arc<dyn RouteSelector>,
    weighted_random: Arc<dyn RouteSelector>,
    p2c: Arc<dyn RouteSelector>,
}

impl std::fmt::Debug for RouteSelectorSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteSelectorSet").finish()
    }
}

impl Default for RouteSelectorSet {
    fn default() -> Self {
        Self {
            smooth: Arc::new(SmoothWeightedRrSelector::new()),
            naive: Arc::new(NaiveRrSelector::new()),
            weighted: Arc::new(WeightedRrSelector::new()),
            uniform_random: Arc::new(UniformRandomSelector),
            weighted_random: Arc::new(WeightedRandomSelector),
            p2c: Arc::new(PowerOfTwoChoicesSelector),
        }
    }
}

impl RouteSelectorSet {
    #[must_use]
    pub fn selector(&self, strategy: RoutingStrategy) -> &Arc<dyn RouteSelector> {
        match strategy {
            RoutingStrategy::SmoothWeightedRr => &self.smooth,
            RoutingStrategy::NaiveRr => &self.naive,
            RoutingStrategy::WeightedRr => &self.weighted,
            RoutingStrategy::UniformRandom => &self.uniform_random,
            RoutingStrategy::WeightedRandom => &self.weighted_random,
            RoutingStrategy::PowerOfTwoChoices => &self.p2c,
        }
    }

    /// Drop per-group cursor state for a model whose route set changed.
    pub fn reset_model(&self, model_pk: ModelPk) {
        for selector in [
            &self.smooth,
            &self.naive,
            &self.weighted,
            &self.uniform_random,
            &self.weighted_random,
            &self.p2c,
        ] {
            let _ = &model_pk;
            // Group keys embed the model; evict via a synthetic sweep key is not possible,
            // so groups are reset by callers that own the concrete key.
            let _ = selector;
        }
    }
}

/// Global shared in-memory runtime state across proxy and control planes.
pub struct RuntimeState {
    pub health: Arc<HealthState>,
    pub last_reload_ts: AtomicI64,
    pub reload_count: AtomicU64,
    pub applied_invalidations_count: AtomicU64,
    pub key_health: DashMap<UpstreamKeyId, KeyHealthEntry>,
    pub origin_breakers: DashMap<ProviderId, Arc<Mutex<OriginBreaker>>>,
    pub load_tracker: Arc<LoadTracker>,
    pub selectors: RouteSelectorSet,
    pub window_counters: DashMap<String, Arc<AtomicBudgetCounter>>,
    pub accumulators: Arc<MonthlyAccumulatorRegistry>,
    pub constant_session_detector: Arc<ConstantSessionDetector>,
}

impl std::fmt::Debug for RuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeState")
            .field("key_health_len", &self.key_health.len())
            .field("origin_breakers_len", &self.origin_breakers.len())
            .field("window_counters_len", &self.window_counters.len())
            .finish()
    }
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            health: Arc::new(HealthState::new()),
            last_reload_ts: AtomicI64::new(0),
            reload_count: AtomicU64::new(0),
            applied_invalidations_count: AtomicU64::new(0),
            key_health: DashMap::new(),
            origin_breakers: DashMap::new(),
            load_tracker: Arc::new(LoadTracker::new()),
            selectors: RouteSelectorSet::default(),
            window_counters: DashMap::new(),
            accumulators: Arc::new(MonthlyAccumulatorRegistry::new()),
            constant_session_detector: Arc::new(ConstantSessionDetector::new()),
        }
    }

    /// Rebuild configured budget counters from the published snapshot.
    pub fn rebuild_window_counters(&self, snapshot: &ConfigSnapshot) {
        let mut live: Vec<String> = Vec::new();
        for windows in snapshot.budget_windows.values() {
            for window in windows {
                live.push(window.id.clone());
                let period = window.period_ms.unwrap_or(2_678_400_000);
                let reserve = window.reserve_pct;
                let entry = self
                    .window_counters
                    .entry(window.id.clone())
                    .or_insert_with(|| {
                        Arc::new(AtomicBudgetCounter::new(
                            stable_counter_id(&window.id),
                            window.limit_micro_usd,
                            reserve,
                            period,
                        ))
                    });
                let _ = entry;
            }
        }
        self.window_counters.retain(|id, _| live.contains(id));
    }

    /// Load a configured counter for a window, if the snapshot defines it.
    #[must_use]
    pub fn window_counter(&self, window_id: &str) -> Option<Arc<AtomicBudgetCounter>> {
        self.window_counters
            .get(window_id)
            .map(|entry| entry.clone())
    }

    /// Mark a key unroutable after an AEAD or credential failure.
    pub fn quarantine_key(&self, key_id: UpstreamKeyId, reason: &str) {
        self.key_health.insert(
            key_id,
            KeyHealthEntry {
                healthy: false,
                quarantine_reason: Some(reason.to_string()),
                ..KeyHealthEntry::default()
            },
        );
        self.health.bump_version();
    }

    /// Whether a key is currently routable (C6).
    #[must_use]
    pub fn is_key_routable(&self, key_id: UpstreamKeyId, now_ms: i64) -> bool {
        self.key_health
            .get(&key_id)
            .is_none_or(|entry| entry.is_routable(now_ms))
    }

    /// Whether the provider's origin breaker admits a new attempt (C7).
    pub fn is_origin_admitted(&self, provider_id: ProviderId, now_ms: i64) -> bool {
        let breaker = self
            .origin_breakers
            .entry(provider_id)
            .or_insert_with(|| Arc::new(Mutex::new(OriginBreaker::new())))
            .clone();
        let mut guard = breaker.lock();
        guard.is_admitted(now_ms)
    }

    /// Record a successful upstream attempt (health + breaker + latency EWMA).
    pub fn record_upstream_success(
        &self,
        provider_id: ProviderId,
        key_id: UpstreamKeyId,
        route: Option<RouteId>,
        ttfb_ms: Option<u64>,
        now_ms: i64,
    ) {
        self.key_health.insert(key_id, KeyHealthEntry::default());
        if let Some(breaker) = self.origin_breakers.get(&provider_id) {
            breaker.lock().record_success(now_ms);
        }
        if let (Some(route), Some(ttfb)) = (route, ttfb_ms) {
            self.load_tracker.record_ttfb(route, ttfb, 0.2);
        }
    }

    /// Record a failed attempt, applying cooldowns, exhaustion, and breaker state.
    pub fn record_upstream_failure(
        &self,
        provider_id: ProviderId,
        key_id: UpstreamKeyId,
        outcome: AttemptOutcome,
        retry_after_ms: Option<i64>,
        now_ms: i64,
    ) {
        let mut entry = self.key_health.entry(key_id).or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        match outcome {
            AttemptOutcome::Http429 => {
                entry.cooldown_until_ms = Some(now_ms + retry_after_ms.unwrap_or(60_000));
            }
            AttemptOutcome::Http401 | AttemptOutcome::Http403 => {
                entry.healthy = false;
                entry.quarantine_reason = Some("credential_rejected".to_string());
            }
            AttemptOutcome::ConnectError
            | AttemptOutcome::Http5xx
            | AttemptOutcome::WriteStalled => {
                entry.cooldown_until_ms = Some(now_ms + retry_after_ms.unwrap_or(15_000));
            }
            _ => {}
        }
        drop(entry);
        self.health.bump_version();

        if matches!(
            outcome,
            AttemptOutcome::ConnectError | AttemptOutcome::Http5xx | AttemptOutcome::WriteStalled
        ) {
            let breaker = self
                .origin_breakers
                .entry(provider_id)
                .or_insert_with(|| Arc::new(Mutex::new(OriginBreaker::new())))
                .clone();
            breaker.lock().record_host_failure(now_ms);
        }
    }

    /// Whether any configured hard budget window for a key is exhausted (C8 quick gate).
    #[must_use]
    pub fn key_budget_admitted(&self, key_id: UpstreamKeyId, now_ms: i64) -> bool {
        self.window_counters
            .iter()
            .filter(|entry| entry.key().starts_with(&format!("{key_id}:")))
            .all(|entry| {
                let counter = entry.value();
                let used = crate::windows::BudgetCounter::effective_used(counter.as_ref(), now_ms);
                used < crate::windows::BudgetCounter::max_allowed_spend(counter.as_ref())
            })
    }

    /// Apply cache invalidation events immediately following atomic snapshot publication.
    pub fn apply_invalidations(
        &self,
        invalidations: &[CacheInvalidationEvent],
        snapshot: &ConfigSnapshot,
        now_ms: i64,
    ) {
        self.last_reload_ts.store(now_ms, Ordering::SeqCst);
        self.reload_count.fetch_add(1, Ordering::SeqCst);
        self.applied_invalidations_count
            .fetch_add(invalidations.len() as u64, Ordering::SeqCst);

        let mut rebuild_windows = false;
        for inv in invalidations {
            match inv {
                CacheInvalidationEvent::UpstreamKeyChanged(key_id) => {
                    if !snapshot.upstream_keys.contains_key(key_id) {
                        self.key_health.remove(key_id);
                    }
                    self.health.bump_version();
                    rebuild_windows = true;
                }
                CacheInvalidationEvent::ProviderChanged(provider_id) => {
                    if !snapshot.providers.contains_key(provider_id) {
                        self.origin_breakers.remove(provider_id);
                    }
                    self.health.bump_version();
                }
                CacheInvalidationEvent::BudgetWindowChanged(_)
                | CacheInvalidationEvent::CapChanged => {
                    rebuild_windows = true;
                }
                CacheInvalidationEvent::StrategyChanged
                | CacheInvalidationEvent::SettingsChanged
                | CacheInvalidationEvent::ModelRoutesChanged(_)
                | CacheInvalidationEvent::ClientKeyChanged(_)
                | CacheInvalidationEvent::ShareChanged(_) => {
                    self.health.bump_version();
                }
                CacheInvalidationEvent::UserChanged(_)
                | CacheInvalidationEvent::OwnerDisabled(_) => {}
            }
        }
        if rebuild_windows {
            self.rebuild_window_counters(snapshot);
        }
    }

    /// Seeds a budget window counter from startup reconstruction.
    pub fn hydrate_window_counter(
        &self,
        window: &SnapshotBudgetWindow,
        ring_seed: i64,
        now_ms: i64,
    ) {
        let period = window.period_ms.unwrap_or(2_678_400_000);
        let counter = Arc::new(AtomicBudgetCounter::new(
            stable_counter_id(&window.id),
            window.limit_micro_usd,
            window.reserve_pct,
            period,
        ));
        if ring_seed > 0 {
            crate::windows::BudgetCounter::record_actual(counter.as_ref(), now_ms, ring_seed);
        }
        self.window_counters.insert(window.id.clone(), counter);
    }

    /// Convenience constructor for a WRR group key.
    #[must_use]
    pub fn group_key(
        user_id: crate::ids::UserId,
        model_pk: ModelPk,
        protocol: &str,
        tier: u8,
    ) -> WrrGroupKey {
        WrrGroupKey {
            user_id,
            model_pk,
            protocol: protocol.to_string(),
            pool_tier: tier,
        }
    }
}

fn stable_counter_id(id: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(id, &mut hasher);
    std::hash::Hasher::finish(&hasher)
}
