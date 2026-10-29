//! Session pin state machine and PinStore mutex transitions.
//!
//! Spec: Section 6.7, Section 10.4, Section 21.2,
//! Requirements: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, CLK-2.

use dashmap::DashMap;
use parking_lot::Mutex;
use std::sync::Arc;

use crate::eligibility::CandidatePoolTier;
use crate::ids::{ClientKeyId, UpstreamKeyId};
use crate::rbac::RoutingScope;
use crate::seams::Clock;

/// Key identifying an in-memory session pin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PinKey {
    pub client_key_id: ClientKeyId,
    pub canonical_id: String,
    pub session_hash: String,
}

/// State of an active pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinState {
    Provisional,
    Confirmed,
}

/// Routing outcome for pin acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    NewPin,
    Followed,
    Repin,
    RepinRecovery,
    UnpinnedCapacity,
    ResponseLink,
}

impl PinOutcome {
    /// Stable snake_case label persisted on the usage event (`pin_outcome`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewPin => "new_pin",
            Self::Followed => "followed",
            Self::Repin => "repin",
            Self::RepinRecovery => "repin_recovery",
            Self::UnpinnedCapacity => "unpinned_capacity",
            Self::ResponseLink => "response_link",
        }
    }
}

/// Entry in PinStore guarded by a Mutex.
#[derive(Debug, Clone)]
pub struct PinEntry {
    pub state: PinState,
    pub upstream_key_id: UpstreamKeyId,
    pub generation: u64,
    pub pool_tier: CandidatePoolTier,
    pub created_ms: i64,
    pub last_used_ms: i64,
    pub expires_ms: i64,
    pub repin_count: u32,
    pub tried_this_generation: Vec<UpstreamKeyId>,
}

/// Ephemeral ticket issued to a request referencing an acquired pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinTicket {
    pub upstream_key_id: UpstreamKeyId,
    pub generation: u64,
    pub outcome: PinOutcome,
    pub pool_tier: CandidatePoolTier,
}

/// Minimum interval between coalesced SQLite persistence updates for a confirmed pin (`RTE-2`, Section 10.4).
pub const PIN_PERSIST_COALESCE_MS: i64 = 60_000;

/// In-memory session pin store backed by DashMap and per-entry Mutex.
pub struct PinStore {
    entries: DashMap<PinKey, Arc<Mutex<PinEntry>>>,
    last_persisted_ms: DashMap<PinKey, i64>,
    max_active_pins: usize,
}

impl std::fmt::Debug for PinStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinStore")
            .field("len", &self.entries.len())
            .field("max_active_pins", &self.max_active_pins)
            .finish()
    }
}

impl PinStore {
    pub fn new(max_active_pins: usize) -> Self {
        Self {
            entries: DashMap::new(),
            last_persisted_ms: DashMap::new(),
            max_active_pins,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolve an active confirmed pin matching client_key_id and session_hash (Section 13.4, USG-7).
    ///
    /// When `canonical_model` is provided, looks up that exact route pin.
    /// When omitted, selects the most recently used active pin for this session.
    #[must_use]
    pub fn find_active_pin(
        &self,
        client_key_id: &ClientKeyId,
        session_hash: &str,
        canonical_model: Option<&str>,
        now_ms: i64,
    ) -> Option<UpstreamKeyId> {
        if let Some(model) = canonical_model {
            let key = PinKey {
                client_key_id: *client_key_id,
                session_hash: session_hash.to_string(),
                canonical_id: model.to_string(),
            };
            if let Some(entry_arc) = self.entries.get(&key) {
                let entry = entry_arc.lock();
                if entry.expires_ms > now_ms && entry.state == PinState::Confirmed {
                    return Some(entry.upstream_key_id);
                }
            }
            return None;
        }

        let mut best: Option<(i64, UpstreamKeyId)> = None;
        for item in self.entries.iter() {
            let (k, arc) = (item.key(), item.value());
            if k.client_key_id == *client_key_id && k.session_hash == session_hash {
                let entry = arc.lock();
                if entry.expires_ms > now_ms
                    && entry.state == PinState::Confirmed
                    && best.is_none_or(|(ts, _)| entry.last_used_ms > ts)
                {
                    best = Some((entry.last_used_ms, entry.upstream_key_id));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    /// Acquires a pin for the given session key according to the state machine (Section 10.4).
    #[allow(clippy::too_many_arguments)]
    pub fn acquire<F>(
        &self,
        pin_key: &PinKey,
        clock: &dyn Clock,
        _pin_ttl_ms: i64,
        repin_on_personal_recovery: bool,
        recovery_repin_min_idle_secs: i64,
        scope: RoutingScope,
        best_tier_available: Option<CandidatePoolTier>,
        is_candidate_eligible: impl Fn(UpstreamKeyId) -> bool,
        mut select_fn: F,
    ) -> Option<PinTicket>
    where
        F: FnMut(&[UpstreamKeyId]) -> Option<(UpstreamKeyId, CandidatePoolTier)>,
    {
        let now = clock.now_ms();

        let entry_arc = if let Some(arc) = self.entries.get(pin_key) {
            Arc::clone(&arc)
        } else {
            if self.entries.len() >= self.max_active_pins {
                // Reclaim expired entries before treating capacity as exhausted (`ISSUE-0169`).
                //
                // `entries.len()` counts entries whose `expires_ms` has passed but which the 30 s
                // background sweep (`serve.rs`) has not collected yet. Without this, a store whose
                // live population is zero still reports as full, and every new session in that
                // window loses affinity despite there being nothing to preserve — measured at
                // `active_len() == 0` while refusing.
                //
                // `sweep` only removes entries whose `expires_ms <= now`, which is the state
                // machine's own rule (`10.4`, the `Sweep` row), so this reclaims exactly what the
                // background sweep would and nothing else. It deliberately does **not** evict live
                // pins: `15. SRDD` rejects eviction for this structure by name, because dropping a
                // confirmed pin silently breaks an in-flight stream's affinity. If nothing expired,
                // the cap is genuinely reached and `RTE-10`'s fallback below still applies.
                //
                // Cost: only on the capacity branch, which is rare by construction; `retain` walks
                // the shards internally rather than through an iterator we hold, so there is no
                // mutation-during-iteration.
                self.sweep(now);
            }
            if self.entries.len() >= self.max_active_pins {
                // Still full after reclaiming -> route unpinned without inserting (RTE-10)
                let (k, tier) = select_fn(&[])?;
                return Some(PinTicket {
                    upstream_key_id: k,
                    generation: 1,
                    outcome: PinOutcome::UnpinnedCapacity,
                    pool_tier: tier,
                });
            }

            use dashmap::mapref::entry::Entry;
            match self.entries.entry(pin_key.clone()) {
                Entry::Occupied(occ) => Arc::clone(occ.get()),
                Entry::Vacant(vac) => {
                    let (k, tier) = select_fn(&[])?;
                    let entry = PinEntry {
                        state: PinState::Provisional,
                        upstream_key_id: k,
                        generation: 1,
                        pool_tier: tier,
                        created_ms: now,
                        last_used_ms: now,
                        expires_ms: now + 50_000,
                        repin_count: 0,
                        tried_this_generation: Vec::new(),
                    };
                    let arc = Arc::new(Mutex::new(entry));
                    vac.insert(Arc::clone(&arc));
                    return Some(PinTicket {
                        upstream_key_id: k,
                        generation: 1,
                        outcome: PinOutcome::NewPin,
                        pool_tier: tier,
                    });
                }
            }
        };

        let mut entry = entry_arc.lock();

        match entry.state {
            PinState::Provisional => {
                if now < entry.expires_ms && is_candidate_eligible(entry.upstream_key_id) {
                    Some(PinTicket {
                        upstream_key_id: entry.upstream_key_id,
                        generation: entry.generation,
                        outcome: PinOutcome::Followed,
                        pool_tier: entry.pool_tier,
                    })
                } else {
                    let prev_key = entry.upstream_key_id;
                    if !entry.tried_this_generation.contains(&prev_key) {
                        entry.tried_this_generation.push(prev_key);
                    }
                    let (k_new, tier_new) = select_fn(&entry.tried_this_generation)
                        .or_else(|| select_fn(&[prev_key]))
                        .or_else(|| select_fn(&[]))?;
                    entry.upstream_key_id = k_new;
                    entry.pool_tier = tier_new;
                    entry.generation += 1;
                    entry.expires_ms = now + 50_000;
                    Some(PinTicket {
                        upstream_key_id: k_new,
                        generation: entry.generation,
                        outcome: PinOutcome::Repin,
                        pool_tier: tier_new,
                    })
                }
            }
            PinState::Confirmed => {
                // Check recovery repin (RTE-2)
                if repin_on_personal_recovery
                    && matches!(
                        scope,
                        RoutingScope::PersonalFirst | RoutingScope::PersonalOnly
                    )
                    && let Some(best) = best_tier_available
                    && best < entry.pool_tier
                    && now - entry.last_used_ms >= recovery_repin_min_idle_secs * 1000
                {
                    let (k_new, tier_new) = select_fn(&[])?;
                    entry.upstream_key_id = k_new;
                    entry.pool_tier = tier_new;
                    entry.generation += 1;
                    entry.repin_count += 1;
                    entry.tried_this_generation.clear();
                    entry.state = PinState::Provisional;
                    entry.expires_ms = now + 50_000;
                    return Some(PinTicket {
                        upstream_key_id: k_new,
                        generation: entry.generation,
                        outcome: PinOutcome::RepinRecovery,
                        pool_tier: tier_new,
                    });
                }

                if now >= entry.expires_ms {
                    let (k_new, tier_new) = select_fn(&[])?;
                    entry.upstream_key_id = k_new;
                    entry.pool_tier = tier_new;
                    entry.generation += 1;
                    entry.tried_this_generation.clear();
                    entry.state = PinState::Provisional;
                    entry.expires_ms = now + 50_000;
                    Some(PinTicket {
                        upstream_key_id: k_new,
                        generation: entry.generation,
                        outcome: PinOutcome::NewPin,
                        pool_tier: tier_new,
                    })
                } else if is_candidate_eligible(entry.upstream_key_id) {
                    entry.last_used_ms = now;
                    if _pin_ttl_ms > 0 {
                        entry.expires_ms = now + _pin_ttl_ms;
                    }
                    Some(PinTicket {
                        upstream_key_id: entry.upstream_key_id,
                        generation: entry.generation,
                        outcome: PinOutcome::Followed,
                        pool_tier: entry.pool_tier,
                    })
                } else {
                    let prev_key = entry.upstream_key_id;
                    let (k_new, tier_new) = select_fn(&[prev_key]).or_else(|| select_fn(&[]))?;
                    entry.upstream_key_id = k_new;
                    entry.pool_tier = tier_new;
                    entry.generation += 1;
                    entry.repin_count += 1;
                    entry.tried_this_generation.clear();
                    entry.state = PinState::Provisional;
                    entry.expires_ms = now + 50_000;
                    Some(PinTicket {
                        upstream_key_id: k_new,
                        generation: entry.generation,
                        outcome: PinOutcome::Repin,
                        pool_tier: tier_new,
                    })
                }
            }
        }
    }

    /// Transitions a pin to Confirmed on first valid frame arrival (RTE-7).
    ///
    /// Returns the confirmed projection so the caller can write it behind the
    /// durable queue. A stale ticket whose generation was superseded returns
    /// `None` and leaves the live pin untouched.
    pub fn first_valid_frame(
        &self,
        pin_key: &PinKey,
        ticket: &PinTicket,
        now_ms: i64,
        pin_ttl_ms: i64,
    ) -> Option<PinEntry> {
        if let Some(entry_arc) = self.entries.get(pin_key) {
            let mut entry = entry_arc.lock();
            if ticket.generation == entry.generation
                && ticket.upstream_key_id == entry.upstream_key_id
            {
                let was_provisional = entry.state == PinState::Provisional;
                entry.state = PinState::Confirmed;
                entry.last_used_ms = now_ms;
                entry.expires_ms = now_ms + pin_ttl_ms;
                entry.tried_this_generation.clear();
                let snapshot = entry.clone();
                drop(entry);
                let last = self
                    .last_persisted_ms
                    .get(pin_key)
                    .map_or(0, |v| *v.value());
                if was_provisional
                    || last == 0
                    || now_ms.saturating_sub(last) >= PIN_PERSIST_COALESCE_MS
                {
                    self.last_persisted_ms.insert(pin_key.clone(), now_ms);
                    return Some(snapshot);
                }
            }
        }
        None
    }

    /// Returns a confirmed [`PinEntry`] to persist to SQLite when at least [`PIN_PERSIST_COALESCE_MS`]
    /// (`60 s`) have elapsed since the last durable write for `pin_key` (`F-051`, `RTE-2`).
    pub fn take_coalesced_persist(
        &self,
        pin_key: &PinKey,
        now_ms: i64,
        pin_ttl_ms: i64,
    ) -> Option<PinEntry> {
        let entry_arc = self.entries.get(pin_key)?;
        let mut entry = entry_arc.lock();
        if entry.state != PinState::Confirmed {
            return None;
        }
        entry.last_used_ms = now_ms;
        if pin_ttl_ms > 0 {
            entry.expires_ms = now_ms + pin_ttl_ms;
        }
        let snapshot = entry.clone();
        drop(entry);

        let last = self
            .last_persisted_ms
            .get(pin_key)
            .map_or(0, |v| *v.value());
        if last == 0 || now_ms.saturating_sub(last) >= PIN_PERSIST_COALESCE_MS {
            self.last_persisted_ms.insert(pin_key.clone(), now_ms);
            Some(snapshot)
        } else {
            None
        }
    }

    /// Handles failover before first byte with race-free winner adoption (RTE-8).
    pub fn pre_first_byte_failure<F>(
        &self,
        pin_key: &PinKey,
        ticket: &PinTicket,
        failed_key: UpstreamKeyId,
        now_ms: i64,
        is_candidate_eligible: impl Fn(UpstreamKeyId) -> bool,
        mut select_fn: F,
    ) -> Option<PinTicket>
    where
        F: FnMut(&[UpstreamKeyId]) -> Option<(UpstreamKeyId, CandidatePoolTier)>,
    {
        let entry_arc = self.entries.get(pin_key)?;
        let mut entry = entry_arc.lock();

        if ticket.generation == entry.generation {
            // First thread to fail: advance generation and reselect excluding tried keys
            entry.tried_this_generation.push(failed_key);
            let (k_new, tier_new) = select_fn(&entry.tried_this_generation)?;
            entry.upstream_key_id = k_new;
            entry.pool_tier = tier_new;
            entry.generation += 1;
            entry.expires_ms = now_ms + 50_000;
            Some(PinTicket {
                upstream_key_id: k_new,
                generation: entry.generation,
                outcome: PinOutcome::Repin,
                pool_tier: tier_new,
            })
        } else {
            // Loser adoption: ticket.generation < entry.generation
            // Another thread already picked a winner!
            if entry.upstream_key_id != failed_key && is_candidate_eligible(entry.upstream_key_id) {
                // Adopt winner directly
                Some(PinTicket {
                    upstream_key_id: entry.upstream_key_id,
                    generation: entry.generation,
                    outcome: PinOutcome::Followed,
                    pool_tier: entry.pool_tier,
                })
            } else {
                // Winner also failed for this thread: reselect
                entry.tried_this_generation.push(failed_key);
                let (k_new, tier_new) = select_fn(&entry.tried_this_generation)?;
                entry.upstream_key_id = k_new;
                entry.pool_tier = tier_new;
                entry.generation += 1;
                entry.expires_ms = now_ms + 50_000;
                Some(PinTicket {
                    upstream_key_id: k_new,
                    generation: entry.generation,
                    outcome: PinOutcome::Repin,
                    pool_tier: tier_new,
                })
            }
        }
    }

    /// Admin manually moves pin to target key (RTE-11).
    pub fn admin_move(&self, pin_key: &PinKey, new_key_id: UpstreamKeyId, now_ms: i64) {
        self.admin_move_with_tier(pin_key, new_key_id, None, now_ms);
    }

    /// Admin or user manually moves pin to target key and updates its pool_tier (RTE-11, FND-003).
    pub fn admin_move_with_tier(
        &self,
        pin_key: &PinKey,
        new_key_id: UpstreamKeyId,
        new_pool_tier: Option<CandidatePoolTier>,
        now_ms: i64,
    ) {
        if let Some(entry_arc) = self.entries.get(pin_key) {
            let mut entry = entry_arc.lock();
            entry.upstream_key_id = new_key_id;
            if let Some(tier) = new_pool_tier {
                entry.pool_tier = tier;
            }
            entry.generation += 1;
            entry.tried_this_generation.clear();
            entry.last_used_ms = now_ms;
        }
    }

    /// Invalidates pins pointing to target upstream key (PRV-8).
    pub fn invalidate_key(&self, target_key_id: &UpstreamKeyId) {
        self.entries.retain(|_, arc| {
            let entry = arc.lock();
            entry.upstream_key_id != *target_key_id
        });
    }

    /// Invalidates every pin that belongs to a revoked, paused, or disabled client key.
    ///
    /// `CLK-4` / `CLK-7`: once a key is no longer eligible to authenticate, no live
    /// session pin may keep routing traffic for it. Returns the number evicted.
    pub fn invalidate_client_key(&self, target_client_key_id: &ClientKeyId) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|pin_key, _| pin_key.client_key_id != *target_client_key_id);
        before.saturating_sub(self.entries.len())
    }

    /// Batch form of [`PinStore::invalidate_client_key`] used by the reloader when a
    /// snapshot publication invalidates several client keys at once (`CLK-4`, `SHR-5`).
    pub fn invalidate_client_keys(
        &self,
        targets: &std::collections::HashSet<ClientKeyId>,
    ) -> usize {
        if targets.is_empty() {
            return 0;
        }
        let before = self.entries.len();
        self.entries
            .retain(|pin_key, _| !targets.contains(&pin_key.client_key_id));
        before.saturating_sub(self.entries.len())
    }

    /// Removes the exact session pin addressed by an owner-scoped control-plane action.
    ///
    /// Returns `true` when a live entry was evicted (SEC-17, Section 10.4).
    pub fn remove(&self, pin_key: &PinKey) -> bool {
        self.entries.remove(pin_key).is_some()
    }

    /// Removes every live pin for a session hash, returning the number evicted.
    ///
    /// Used by the admin-wide session unpin route after the durable rows are
    /// deleted (SEC-17, AUD-1, Section 10.4).
    pub fn remove_by_session_hash(&self, session_hash: &str) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|pin_key, _| pin_key.session_hash != session_hash);
        before.saturating_sub(self.entries.len())
    }

    /// Insert an already-confirmed pin loaded from SQLite at startup (RTE-10).
    pub fn insert_hydrated(&self, pin_key: PinKey, entry: PinEntry) {
        if self.entries.len() >= self.max_active_pins {
            // Reclaim expired entries first, for the same reason `acquire` does (`ISSUE-0169`).
            // The hydration query already filters `expires_at > now`, but it and this insert are
            // not atomic, and `upsert_confirmed` reaches here at runtime too — so a stale count
            // would silently drop a legitimate pin.
            self.sweep(entry.last_used_ms.max(0));
            if self.entries.len() >= self.max_active_pins {
                return;
            }
        }
        self.entries.insert(pin_key, Arc::new(Mutex::new(entry)));
    }

    /// Upsert or touch a confirmed pin for HardKey / state-bound operations (RTE-12).
    pub fn upsert_confirmed(
        &self,
        pin_key: PinKey,
        upstream_key_id: UpstreamKeyId,
        pool_tier: CandidatePoolTier,
        now_ms: i64,
        ttl_ms: i64,
    ) {
        if let Some(entry_arc) = self.entries.get(&pin_key) {
            let mut entry = entry_arc.lock();
            entry.upstream_key_id = upstream_key_id;
            entry.state = PinState::Confirmed;
            entry.pool_tier = pool_tier;
            entry.last_used_ms = now_ms;
            entry.expires_ms = now_ms.saturating_add(ttl_ms);
        } else {
            self.insert_hydrated(
                pin_key,
                PinEntry {
                    state: PinState::Confirmed,
                    upstream_key_id,
                    generation: 1,
                    pool_tier,
                    created_ms: now_ms,
                    last_used_ms: now_ms,
                    expires_ms: now_ms.saturating_add(ttl_ms),
                    repin_count: 0,
                    tried_this_generation: Vec::new(),
                },
            );
        }
    }

    /// Sweeper removes expired pins every 60s (RTE-10, F039).
    pub fn sweep(&self, now_ms: i64) {
        self.entries.retain(|pin_key, arc| {
            let entry = arc.lock();
            let keep = now_ms < entry.expires_ms;
            if !keep {
                self.last_persisted_ms.remove(pin_key);
            }
            keep
        });
    }

    /// Count only unexpired pins at `now_ms` (`F039`, `RTE-10`).
    #[must_use]
    pub fn active_len(&self, now_ms: i64) -> usize {
        self.entries
            .iter()
            .filter(|item| item.value().lock().expires_ms > now_ms)
            .count()
    }

    /// Snapshot all unexpired `(PinKey, PinEntry)` pairs at `now_ms` (`F039`).
    #[must_use]
    pub fn list_active(&self, now_ms: i64) -> Vec<(PinKey, PinEntry)> {
        self.entries
            .iter()
            .filter_map(|item| {
                let entry = item.value().lock().clone();
                (entry.expires_ms > now_ms).then(|| (item.key().clone(), entry))
            })
            .collect()
    }
}

#[cfg(test)]
pub mod tests;
