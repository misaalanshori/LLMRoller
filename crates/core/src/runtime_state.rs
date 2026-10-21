#![forbid(unsafe_code)]
//! In-memory runtime state for health, circuit breakers, strategy groups, and budgets.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#51-high-level-architecture-diagram
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#215-versioned-health-synchronization-and-origin-circuit-breakers

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::accumulators::MonthlyAccumulatorRegistry;
use crate::breaker::{OriginBreaker, OriginKey, OriginProbeGuard};
use crate::health::{CredentialStrikeVerifier, RouteProtocolBlocks, StrikeResult};
use crate::ids::{ClientKeyId, ModelPk, UpstreamKeyId};
use crate::session::ConstantSessionDetector;
use crate::snapshot::{CacheInvalidationEvent, ConfigSnapshot, SnapshotBudgetWindow};
use crate::strategy::{
    NaiveRrSelector, PowerOfTwoChoicesSelector, RouteId, RouteLoadTracker, RouteSelector,
    RoutingStrategy, UniformRandomSelector, WeightedRandomSelector, WrrGroupKey,
};
use crate::usage::AttemptOutcome;
use crate::windows::{AtomicBudgetCounter, BudgetCounter as _};
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

/// Normalized remote window observation used for reconciliation (USG-5).
#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamWindowObservation {
    pub kind: String,
    pub used_percent: f64,
    pub resets_at: Option<i64>,
}

/// Canonical `upstream_window_kind` label for a configured budget window.
fn local_window_kind(window: &SnapshotBudgetWindow) -> &'static str {
    match window.period_ms {
        None => "1mo",
        Some(period) if period <= 6 * 3_600_000 => "5h",
        Some(period) if period <= 8 * 86_400_000 => "7d",
        Some(_) => "1mo",
    }
}

/// Normalize provider-native window labels to the canonical schema values.
fn canonical_kind(kind: &str) -> &str {
    match kind.trim().to_ascii_lowercase().as_str() {
        "5h" | "rolling" | "rolling5h" | "rolling_5h" | "rolling-5h" => "5h",
        "7d" | "weekly" | "week" | "1w" => "7d",
        "1mo" | "monthly" | "month" | "30d" => "1mo",
        _ => kind.trim(),
    }
}

fn kind_matches(remote: &str, local: &str) -> bool {
    canonical_kind(remote) == canonical_kind(local)
}

/// Whether a configured window constrains a given model route (C8).
///
/// Key-scoped windows constrain every route on the key; route-scoped windows
/// constrain only the model they were configured for.
fn window_applies_to_model(window: &SnapshotBudgetWindow, model_pk: Option<ModelPk>) -> bool {
    match window.scope_kind.as_str() {
        "route" => model_pk.is_some_and(|model| window.model_pk == Some(model)),
        _ => true,
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
            selector.reset_model(model_pk);
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
    pub origin_breakers: DashMap<OriginKey, Arc<Mutex<OriginBreaker>>>,
    /// Two-strike credential verification with an out-of-band `/models` probe (API-8).
    pub strike_verifier: CredentialStrikeVerifier,
    /// Keys with a credential verification probe already in flight (API-8).
    pub credential_probes_in_flight: DashMap<UpstreamKeyId, ()>,
    /// `(upstream key, model, protocol)` blocks written by `PlanRestricted` errors (API-8).
    pub route_protocol_blocks: RouteProtocolBlocks,
    pub load_tracker: Arc<LoadTracker>,
    pub selectors: RouteSelectorSet,
    pub window_counters: DashMap<String, Arc<AtomicBudgetCounter>>,
    /// Latest allow-listed upstream `/usage` snapshot per key (USG-4, USG-7).
    pub upstream_snapshots: DashMap<UpstreamKeyId, Arc<serde_json::Value>>,
    /// Last on-demand fresh poll start per key for the 10 s single-flight gate.
    pub last_fresh_poll_ts: DashMap<UpstreamKeyId, i64>,
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
            strike_verifier: CredentialStrikeVerifier::new(),
            credential_probes_in_flight: DashMap::new(),
            route_protocol_blocks: RouteProtocolBlocks::new(),
            load_tracker: Arc::new(LoadTracker::new()),
            selectors: RouteSelectorSet::default(),
            window_counters: DashMap::new(),
            upstream_snapshots: DashMap::new(),
            last_fresh_poll_ts: DashMap::new(),
            accumulators: Arc::new(MonthlyAccumulatorRegistry::new()),
            constant_session_detector: Arc::new(ConstantSessionDetector::new()),
        }
    }

    /// Rebuild configured budget counters from the published snapshot.
    pub fn rebuild_window_counters(&self, snapshot: &ConfigSnapshot, now_ms: i64) {
        let mut live: Vec<String> = Vec::new();
        for windows in snapshot.budget_windows.values() {
            for window in windows {
                live.push(window.id.clone());
                let kind = crate::windows::WindowKind::parse(&window.window_kind);
                let stale = self.window_counters.get(&window.id).is_none_or(|counter| {
                    !counter.matches_config(
                        kind,
                        window.period_ms,
                        window.anchor_at,
                        window.limit_micro_usd,
                        window.reserve_pct,
                    )
                });
                if stale {
                    let counter = Arc::new(AtomicBudgetCounter::for_window(
                        stable_counter_id(&window.id),
                        window.limit_micro_usd,
                        window.reserve_pct,
                        kind,
                        window.period_ms,
                        window.anchor_at,
                        window.upstream_window_kind.as_deref(),
                        now_ms,
                    ));
                    self.window_counters.insert(window.id.clone(), counter);
                }
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

    /// Publish the latest allow-listed upstream usage snapshot for a key.
    pub fn publish_upstream_snapshot(&self, key_id: UpstreamKeyId, snapshot: serde_json::Value) {
        self.upstream_snapshots.insert(key_id, Arc::new(snapshot));
    }

    /// Read the latest allow-listed upstream usage snapshot for a key.
    #[must_use]
    pub fn upstream_snapshot(&self, key_id: UpstreamKeyId) -> Option<Arc<serde_json::Value>> {
        self.upstream_snapshots
            .get(&key_id)
            .map(|entry| Arc::clone(entry.value()))
    }

    /// Claim the 10 s on-demand poll slot for a key. Returns `false` while another
    /// fresh poll is recent or in flight (USG-7 single-flight).
    pub fn try_begin_fresh_poll(
        &self,
        key_id: UpstreamKeyId,
        now_ms: i64,
        min_interval_ms: i64,
    ) -> bool {
        let mut last = self.last_fresh_poll_ts.entry(key_id).or_insert(0);
        if *last > 0 && now_ms.saturating_sub(*last) < min_interval_ms {
            return false;
        }
        *last = now_ms;
        true
    }

    /// Reconcile normalized upstream window observations into local counters and
    /// key health, using the elapsed-time decay projection (USG-5, USG-6).
    pub fn reconcile_upstream_usage(
        &self,
        key_id: UpstreamKeyId,
        budget_windows: &[SnapshotBudgetWindow],
        windows: &[UpstreamWindowObservation],
        now_ms: i64,
    ) {
        for configured in budget_windows {
            let local_kind: &str = configured
                .upstream_window_kind
                .as_deref()
                .unwrap_or_else(|| local_window_kind(configured));
            let Some(remote) = windows
                .iter()
                .find(|window| kind_matches(&window.kind, local_kind))
            else {
                continue;
            };
            let Some(counter) = self.window_counter(&configured.id) else {
                continue;
            };
            if remote.used_percent >= 100.0 {
                // Mutate in place: overwriting the entry would silently clear a
                // quarantine, cooldown, or credential strike counter (USG-5).
                let mut entry = self.key_health.entry(key_id).or_default();
                if let Some(incoming) = remote.resets_at {
                    entry.exhausted_until_ms = Some(
                        entry
                            .exhausted_until_ms
                            .map_or(incoming, |existing| existing.max(incoming)),
                    );
                }
            }
            let used_micro_usd =
                (counter.limit_micro_usd() as f64 * remote.used_percent / 100.0).round() as i64;
            let local_at_observation = counter.effective_used(now_ms);
            counter.observe(crate::windows::UpstreamObservation {
                used_micro_usd,
                observed_at_ms: now_ms,
                local_at_observation,
                resets_at_ms: remote.resets_at,
            });
        }
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

    /// Block one `(key, model, protocol)` route until the next discovery refresh (API-8).
    pub fn block_route_protocol(
        &self,
        key_id: UpstreamKeyId,
        model_pk: ModelPk,
        protocol: &str,
        until_ms: i64,
    ) {
        self.route_protocol_blocks
            .block_protocol(key_id, model_pk, protocol, until_ms);
        self.health.bump_version();
    }

    /// Clear a key's protocol blocks after a successful `/models` refresh (API-8).
    pub fn clear_route_protocol_blocks(&self, key_id: UpstreamKeyId) {
        self.route_protocol_blocks.clear_key(key_id);
        self.health.bump_version();
    }

    /// Whether a `(key, model, protocol)` route is blocked by a prior `PlanRestricted` (C5).
    #[must_use]
    pub fn is_route_protocol_blocked(
        &self,
        key_id: UpstreamKeyId,
        model_pk: ModelPk,
        protocol: &str,
        now_ms: i64,
    ) -> bool {
        self.route_protocol_blocks
            .is_blocked(key_id, model_pk, protocol, now_ms)
    }

    /// Exhaust a key's upstream quota window without recording a credential strike.
    pub fn record_quota_exhausted(
        &self,
        key_id: UpstreamKeyId,
        retry_after_ms: Option<i64>,
        now_ms: i64,
    ) {
        let until = now_ms.saturating_add(retry_after_ms.unwrap_or(300_000));
        let mut entry = self.key_health.entry(key_id).or_default();
        entry.exhausted_until_ms = Some(
            entry
                .exhausted_until_ms
                .map_or(until, |existing| existing.max(until)),
        );
        drop(entry);
        self.health.bump_version();
    }

    /// Whether the origin breaker would admit a new attempt (C7, non-claiming).
    #[must_use]
    pub fn is_origin_admitted(&self, origin: &OriginKey, now_ms: i64) -> bool {
        self.origin_breakers
            .get(origin)
            .is_none_or(|breaker| breaker.lock().is_admitted(now_ms))
    }

    /// Claim admission for one upstream attempt.
    ///
    /// The returned guard releases the half-open probe slot if the attempt ends
    /// without recording an outcome, preventing a permanent `HalfOpen` wedge
    /// (REL-5, Section 21.5).
    #[must_use]
    pub fn acquire_origin_permit(
        &self,
        origin: &OriginKey,
        now_ms: i64,
    ) -> Option<OriginProbeGuard> {
        let breaker = self
            .origin_breakers
            .entry(origin.clone())
            .or_insert_with(|| Arc::new(Mutex::new(OriginBreaker::new())))
            .clone();
        let is_probe = breaker.lock().try_acquire(now_ms)?;
        Some(OriginProbeGuard::new(breaker, is_probe))
    }

    /// Record a successful upstream attempt (health + breaker + latency EWMA).
    pub fn record_upstream_success(
        &self,
        origin: &OriginKey,
        key_id: UpstreamKeyId,
        route: Option<RouteId>,
        ttfb_ms: Option<u64>,
        now_ms: i64,
    ) {
        self.key_health.insert(key_id, KeyHealthEntry::default());
        if let Some(breaker) = self.origin_breakers.get(origin) {
            breaker.lock().record_success(now_ms);
        }
        if let (Some(route), Some(ttfb)) = (route, ttfb_ms) {
            self.load_tracker.record_ttfb(route, ttfb, 0.2);
        }
    }

    /// Record a failed attempt, applying cooldowns, exhaustion, and breaker state.
    ///
    /// Returns the credential-strike decision for `401`/`403` outcomes so the
    /// caller can run the out-of-band `/models` verification probe (API-8).
    #[must_use]
    pub fn record_upstream_failure(
        &self,
        origin: &OriginKey,
        key_id: UpstreamKeyId,
        client_key_id: ClientKeyId,
        outcome: AttemptOutcome,
        retry_after_ms: Option<i64>,
        now_ms: i64,
    ) -> Option<StrikeResult> {
        let mut entry = self.key_health.entry(key_id).or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        let mut strike = None;
        match outcome {
            AttemptOutcome::Http429 => {
                entry.cooldown_until_ms = Some(now_ms + retry_after_ms.unwrap_or(60_000));
            }
            AttemptOutcome::Http401 | AttemptOutcome::Http403 => {
                let decision = self
                    .strike_verifier
                    .record_strike(key_id, client_key_id, now_ms);
                match decision {
                    StrikeResult::MarkInvalid => {
                        entry.healthy = false;
                        entry.quarantine_reason = Some("credential_rejected".to_string());
                    }
                    // A single strike only cools the key down while the probe runs;
                    // it must never quarantine a key on first sight (API-8).
                    StrikeResult::ProbeRequired => {
                        entry.cooldown_until_ms = Some(now_ms + 15_000);
                    }
                    StrikeResult::Healthy => {}
                }
                strike = Some(decision);
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
                .entry(origin.clone())
                .or_insert_with(|| Arc::new(Mutex::new(OriginBreaker::new())))
                .clone();
            breaker.lock().record_host_failure(now_ms);
        }
        strike
    }

    /// Claim the single-flight slot for a credential verification probe (API-8).
    pub fn try_begin_credential_probe(&self, key_id: UpstreamKeyId) -> bool {
        self.credential_probes_in_flight
            .insert(key_id, ())
            .is_none()
    }

    /// Release the single-flight credential probe slot.
    pub fn finish_credential_probe(&self, key_id: UpstreamKeyId) {
        self.credential_probes_in_flight.remove(&key_id);
    }

    /// Apply the result of an out-of-band `GET /models` credential probe (API-8).
    pub fn record_credential_probe_result(
        &self,
        key_id: UpstreamKeyId,
        probe_success: bool,
    ) -> StrikeResult {
        let decision = self
            .strike_verifier
            .record_probe_result(key_id, probe_success);
        match decision {
            StrikeResult::Healthy => {
                if let Some(mut entry) = self.key_health.get_mut(&key_id) {
                    entry.cooldown_until_ms = None;
                }
            }
            StrikeResult::MarkInvalid => {
                let mut entry = self.key_health.entry(key_id).or_default();
                entry.healthy = false;
                entry.quarantine_reason = Some("credential_rejected".to_string());
            }
            StrikeResult::ProbeRequired => {}
        }
        self.health.bump_version();
        decision
    }

    /// Whether every configured hard budget window that applies to this key and
    /// model has headroom (C8 quick gate).
    ///
    /// Windows are resolved through the published snapshot because window ids are
    /// opaque UUIDs: `scope_kind = 'upstream_key'` windows always apply, while
    /// `scope_kind = 'route'` windows apply only to their configured model.
    #[must_use]
    pub fn key_budget_admitted(
        &self,
        snapshot: &ConfigSnapshot,
        key_id: UpstreamKeyId,
        model_pk: Option<ModelPk>,
        now_ms: i64,
    ) -> bool {
        let Some(windows) = snapshot.budget_windows.get(&key_id) else {
            return true;
        };
        windows
            .iter()
            .filter(|window| window_applies_to_model(window, model_pk))
            .filter_map(|window| self.window_counter(&window.id))
            .all(|counter| {
                let used = crate::windows::BudgetCounter::effective_used(counter.as_ref(), now_ms);
                used < crate::windows::BudgetCounter::max_allowed_spend(counter.as_ref())
            })
    }

    /// Current health projection for one upstream key (C6).
    #[must_use]
    pub fn key_health_entry(&self, key_id: UpstreamKeyId) -> KeyHealthEntry {
        self.key_health
            .get(&key_id)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    /// Acquire an RAII reservation across every hard budget window configured for a key.
    ///
    /// Returns `Ok(None)` when the key has no configured windows. A `LimitExceeded`
    /// error means the caller must try the next candidate (C8).
    pub fn reserve_key_windows(
        &self,
        snapshot: &ConfigSnapshot,
        key_id: UpstreamKeyId,
        model_pk: Option<ModelPk>,
        estimate_micro_usd: i64,
        now_ms: i64,
    ) -> Result<Option<crate::windows::Reservation>, crate::windows::ReservationError> {
        let Some(windows) = snapshot.budget_windows.get(&key_id) else {
            return Ok(None);
        };
        let mut counters: Vec<Arc<dyn crate::windows::BudgetCounter>> = Vec::new();
        for window in windows
            .iter()
            .filter(|window| window_applies_to_model(window, model_pk))
        {
            if let Some(counter) = self.window_counter(&window.id) {
                counters.push(counter);
            }
        }
        if counters.is_empty() {
            return Ok(None);
        }
        crate::windows::Reservation::try_reserve(counters, estimate_micro_usd, now_ms).map(Some)
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
                CacheInvalidationEvent::ProviderChanged(_) => {
                    // Origin breakers are keyed by scheme/host/port, not provider id,
                    // so drop any origin the published snapshot no longer references.
                    let live: HashSet<OriginKey> = snapshot
                        .providers
                        .values()
                        .map(|provider| {
                            OriginKey::from_base_url(
                                &provider.base_url,
                                provider.allow_private_network,
                            )
                        })
                        .collect();
                    self.origin_breakers
                        .retain(|origin, _| live.contains(origin));
                    self.health.bump_version();
                }
                CacheInvalidationEvent::BudgetWindowChanged(_)
                | CacheInvalidationEvent::CapChanged => {
                    rebuild_windows = true;
                }
                CacheInvalidationEvent::ModelRoutesChanged(model_pk) => {
                    self.selectors.reset_model(*model_pk);
                    self.health.bump_version();
                }
                CacheInvalidationEvent::StrategyChanged
                | CacheInvalidationEvent::SettingsChanged
                | CacheInvalidationEvent::ClientKeyChanged(_)
                | CacheInvalidationEvent::ShareChanged(_) => {
                    self.health.bump_version();
                }
                CacheInvalidationEvent::UserChanged(_)
                | CacheInvalidationEvent::OwnerDisabled(_) => {}
            }
        }
        if rebuild_windows {
            self.rebuild_window_counters(snapshot, now_ms);
        }
    }

    /// Seeds a budget window counter from startup reconstruction.
    pub fn hydrate_window_counter(
        &self,
        window: &SnapshotBudgetWindow,
        ring_seed: i64,
        now_ms: i64,
    ) {
        let counter = Arc::new(AtomicBudgetCounter::for_window(
            stable_counter_id(&window.id),
            window.limit_micro_usd,
            window.reserve_pct,
            crate::windows::WindowKind::parse(&window.window_kind),
            window.period_ms,
            window.anchor_at,
            window.upstream_window_kind.as_deref(),
            now_ms,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::UserId;
    use crate::seams::EntropySource;

    struct FixedEntropy(u64);

    impl EntropySource for FixedEntropy {
        fn fill_bytes(&self, dest: &mut [u8]) {
            dest.fill(0);
        }

        fn next_u64_bounded(&self, upper: u64) -> u64 {
            if upper == 0 { 0 } else { self.0 % upper }
        }
    }

    fn route_a() -> RouteId {
        RouteId {
            upstream_key_id: UpstreamKeyId::generate(),
            model_pk: ModelPk::generate(),
        }
    }

    fn route_b() -> RouteId {
        RouteId {
            upstream_key_id: UpstreamKeyId::generate(),
            model_pk: ModelPk::generate(),
        }
    }

    fn route_c() -> RouteId {
        RouteId {
            upstream_key_id: UpstreamKeyId::generate(),
            model_pk: ModelPk::generate(),
        }
    }

    /// Spec: RTE-4, RTE-5, F08; ModelRoutesChanged must drop stale cursor state.
    #[test]
    fn model_route_invalidation_resets_round_robin_cursors() {
        let runtime = RuntimeState::new();
        let model_pk = ModelPk::generate();
        let candidates = [(route_a(), 1), (route_b(), 1), (route_c(), 1)];
        let group = RuntimeState::group_key(UserId::generate(), model_pk, "chat", 0);
        let entropy = FixedEntropy(0);
        let selector = runtime.selectors.selector(RoutingStrategy::NaiveRr);

        let first = selector
            .select_next(&group, &candidates, 0, &entropy, None)
            .expect("first selection");
        let second = selector
            .select_next(&group, &candidates, 0, &entropy, None)
            .expect("second selection");
        assert_ne!(first, second, "round robin must advance its cursor");

        runtime.selectors.reset_model(model_pk);

        let after_reset = selector
            .select_next(&group, &candidates, 0, &entropy, None)
            .expect("selection after reset");
        assert_eq!(
            after_reset, first,
            "reset_model must clear the per-model round-robin cursor"
        );
    }

    /// Spec: RTE-4, F08; the reloader applies ModelRoutesChanged invalidations.
    #[test]
    fn apply_invalidations_resets_selectors_for_changed_model() {
        let runtime = RuntimeState::new();
        let snapshot = ConfigSnapshot::default();
        let model_pk = ModelPk::generate();
        let candidates = [(route_a(), 1), (route_b(), 1), (route_c(), 1)];
        let group = RuntimeState::group_key(UserId::generate(), model_pk, "chat", 0);
        let entropy = FixedEntropy(0);
        let selector = runtime.selectors.selector(RoutingStrategy::NaiveRr);

        let first = selector
            .select_next(&group, &candidates, 0, &entropy, None)
            .expect("first selection");
        let _ = selector
            .select_next(&group, &candidates, 0, &entropy, None)
            .expect("second selection");

        runtime.apply_invalidations(
            &[CacheInvalidationEvent::ModelRoutesChanged(model_pk)],
            &snapshot,
            1,
        );

        let after_reload = selector
            .select_next(&group, &candidates, 0, &entropy, None)
            .expect("selection after reload");
        assert_eq!(after_reload, first);
    }
}

#[cfg(test)]
mod f04_tests;
#[cfg(test)]
mod f05_tests;
