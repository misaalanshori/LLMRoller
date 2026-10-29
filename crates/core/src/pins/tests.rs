//! Tests for PinStore state machine, loser adoption, and recovery repin.
//!
//! Spec: Section 6.7, Section 10.4, Section 21.2,
//! Requirements: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, CLK-2.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::seams::FakeClock;

#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, CLK-2"]
#[test]
fn test_concurrent_turn1_and_first_valid_frame_confirmation() {
    let store = Arc::new(PinStore::new(10_000));
    let clock = Arc::new(FakeClock::new(1_000_000_000_000));

    let client_key_id = ClientKeyId::generate();
    let pin_key = PinKey {
        client_key_id,
        canonical_id: "test-model".to_string(),
        session_hash: "sess-hash-1".to_string(),
    };

    let k1 = UpstreamKeyId::generate();
    let confirmed_count = Arc::new(AtomicUsize::new(0));

    // 100 concurrent requests for turn 1
    let mut handles = Vec::new();
    for _ in 0..100 {
        let st = Arc::clone(&store);
        let clk = Arc::clone(&clock);
        let pk = pin_key.clone();
        let cc = Arc::clone(&confirmed_count);

        handles.push(thread::spawn(move || {
            let ticket = st
                .acquire(
                    &pk,
                    &*clk,
                    86_400_000,
                    true,
                    300,
                    RoutingScope::PersonalFirst,
                    Some(CandidatePoolTier::Own),
                    |_| true,
                    |_| Some((k1, CandidatePoolTier::Own)),
                )
                .expect("ticket");

            assert_eq!(ticket.upstream_key_id, k1);

            // Confirm first valid frame
            if st
                .first_valid_frame(&pk, &ticket, clk.now_ms(), 86_400_000)
                .is_some()
            {
                cc.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }

    for h in handles {
        h.join().expect("join");
    }

    // Exactly 1 pin in store, confirmed, with k1
    assert_eq!(store.len(), 1);
    let entry = store
        .entries
        .get(&pin_key)
        .expect("must exist")
        .lock()
        .clone();
    assert_eq!(entry.state, PinState::Confirmed);
    assert_eq!(entry.upstream_key_id, k1);
    assert!(confirmed_count.load(Ordering::SeqCst) >= 1);
}

#[doc = "Spec: RTE-7, Spec: RTE-8"]
#[test]
fn test_turn1_absent_slot_coalesces_single_selection() {
    let store = Arc::new(PinStore::new(10_000));
    let clock = Arc::new(FakeClock::new(1_000_000_000_000));

    let client_key_id = ClientKeyId::generate();
    let pin_key = PinKey {
        client_key_id,
        canonical_id: "test-model-concurrent".to_string(),
        session_hash: "sess-hash-concurrent-1".to_string(),
    };

    let k1 = UpstreamKeyId::generate();
    let select_counter = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..50 {
        let st = Arc::clone(&store);
        let clk = Arc::clone(&clock);
        let pk = pin_key.clone();
        let counter = Arc::clone(&select_counter);
        handles.push(std::thread::spawn(move || {
            let ticket = st.acquire(
                &pk,
                &*clk,
                86_400_000,
                false,
                300,
                RoutingScope::PersonalFirst,
                None,
                |_| true,
                |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    Some((k1, CandidatePoolTier::Own))
                },
            );
            assert!(ticket.is_some());
            assert_eq!(ticket.expect("ticket must exist").upstream_key_id, k1);
        }));
    }

    for h in handles {
        h.join().expect("join");
    }

    assert_eq!(
        select_counter.load(Ordering::SeqCst),
        1,
        "select_fn must be called exactly once across 50 concurrent Turn-1 callers on absent slot"
    );
}

#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, CLK-2"]
#[test]
fn test_concurrent_failover_coalescing_and_loser_adoption() {
    let store = Arc::new(PinStore::new(10_000));
    let clock = Arc::new(FakeClock::new(1_000_000_000_000));

    let client_key_id = ClientKeyId::generate();
    let pin_key = PinKey {
        client_key_id,
        canonical_id: "test-model".to_string(),
        session_hash: "sess-hash-2".to_string(),
    };

    let k1 = UpstreamKeyId::generate();
    let k2 = UpstreamKeyId::generate();
    let k3 = UpstreamKeyId::generate();

    // 10 concurrent requests acquire ticket on K1
    let mut tickets = Vec::new();
    for _ in 0..10 {
        let t = store
            .acquire(
                &pin_key,
                &*clock,
                86_400_000,
                true,
                300,
                RoutingScope::PersonalFirst,
                Some(CandidatePoolTier::Own),
                |_| true,
                |_| Some((k1, CandidatePoolTier::Own)),
            )
            .expect("ticket");
        tickets.push(t);
    }

    // Now all 10 requests fail on K1 concurrently:
    // They must all coalesce onto K2 via loser adoption, never touching K3!
    let k3_calls = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for ticket in tickets {
        let st = Arc::clone(&store);
        let pk = pin_key.clone();
        let k3_c = Arc::clone(&k3_calls);

        handles.push(thread::spawn(move || {
            let failover_ticket = st
                .pre_first_byte_failure(
                    &pk,
                    &ticket,
                    k1,
                    1_000_000_001_000,
                    |_| true,
                    |tried| {
                        if tried.contains(&k1) && !tried.contains(&k2) {
                            Some((k2, CandidatePoolTier::Own))
                        } else {
                            k3_c.fetch_add(1, Ordering::SeqCst);
                            Some((k3, CandidatePoolTier::Own))
                        }
                    },
                )
                .expect("failover ticket");

            assert_eq!(failover_ticket.upstream_key_id, k2);
        }));
    }

    for h in handles {
        h.join().expect("join");
    }

    assert_eq!(
        k3_calls.load(Ordering::SeqCst),
        0,
        "zero calls to K3 allowed"
    );
    let entry = store
        .entries
        .get(&pin_key)
        .expect("must exist")
        .lock()
        .clone();
    assert_eq!(entry.upstream_key_id, k2);
}

#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, CLK-2"]
#[test]
fn test_provisional_pin_expiration_and_admin_move_safety() {
    let store = PinStore::new(10_000);
    let clock = FakeClock::new(1_000_000_000_000);

    let client_key_id = ClientKeyId::generate();
    let pin_key = PinKey {
        client_key_id,
        canonical_id: "test-model".to_string(),
        session_hash: "sess-hash-3".to_string(),
    };

    let k1 = UpstreamKeyId::generate();
    let k2 = UpstreamKeyId::generate();

    // Acquire provisional pin at t0 (expires at t0 + 50s)
    let ticket = store
        .acquire(
            &pin_key,
            &clock,
            86_400_000,
            true,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own),
            |_| true,
            |_| Some((k1, CandidatePoolTier::Own)),
        )
        .expect("ticket");
    assert_eq!(ticket.outcome, PinOutcome::NewPin);

    // After 50 seconds on FakeClock, provisional pin expires and sweep prunes it
    clock.advance_ms(50_001);
    store.sweep(clock.now_ms());
    assert_eq!(store.len(), 0);

    // Admin move during in-flight stream test:
    // Create new pin
    let ticket_move = store
        .acquire(
            &pin_key,
            &clock,
            86_400_000,
            true,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own),
            |_| true,
            |_| Some((k1, CandidatePoolTier::Own)),
        )
        .expect("ticket");

    // Admin moves pin to k2
    store.admin_move(&pin_key, k2, clock.now_ms());

    // Stale FirstValidFrame arriving from earlier ticket_move (generation 1 on k1) must NOT revert to k1!
    let reverted = store.first_valid_frame(&pin_key, &ticket_move, clock.now_ms(), 86_400_000);
    assert!(
        reverted.is_none(),
        "stale FirstValidFrame must be rejected after admin move"
    );

    let current = store
        .entries
        .get(&pin_key)
        .expect("must exist")
        .lock()
        .clone();
    assert_eq!(
        current.upstream_key_id, k2,
        "pin must remain on k2 after admin move"
    );
}

#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, CLK-2"]
#[test]
fn test_capacity_overflow_routes_unpinned() {
    // Capacity = 2
    let store = PinStore::new(2);
    let clock = FakeClock::new(1_000_000_000_000);
    let k = UpstreamKeyId::generate();

    for i in 0..2 {
        let pk = PinKey {
            client_key_id: ClientKeyId::generate(),
            canonical_id: "test-model".to_string(),
            session_hash: format!("sess-{i}"),
        };
        let t = store
            .acquire(
                &pk,
                &clock,
                86_400_000,
                true,
                300,
                RoutingScope::PersonalFirst,
                Some(CandidatePoolTier::Own),
                |_| true,
                |_| Some((k, CandidatePoolTier::Own)),
            )
            .expect("ticket");
        assert_eq!(t.outcome, PinOutcome::NewPin);
        store.first_valid_frame(&pk, &t, clock.now_ms(), 86_400_000);
    }
    assert_eq!(store.len(), 2);

    // 3rd session exceeds capacity -> routes UnpinnedCapacity without inserting
    let pk3 = PinKey {
        client_key_id: ClientKeyId::generate(),
        canonical_id: "test-model".to_string(),
        session_hash: "sess-overflow".to_string(),
    };
    let t3 = store
        .acquire(
            &pk3,
            &clock,
            86_400_000,
            true,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own),
            |_| true,
            |_| Some((k, CandidatePoolTier::Own)),
        )
        .expect("ticket");
    assert_eq!(t3.outcome, PinOutcome::UnpinnedCapacity);
    assert_eq!(store.len(), 2);
}

#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, CLK-2"]
#[test]
fn test_repin_on_personal_recovery_timing() {
    let store = PinStore::new(10_000);
    let clock = FakeClock::new(1_000_000_000_000);

    let client_key_id = ClientKeyId::generate();
    let pin_key = PinKey {
        client_key_id,
        canonical_id: "test-model".to_string(),
        session_hash: "sess-recovery".to_string(),
    };

    let k_global = UpstreamKeyId::generate();
    let k_recovered_own = UpstreamKeyId::generate();

    // Pin starts on Global (tier = Global / 2) because Own key was cooling down
    let t1 = store
        .acquire(
            &pin_key,
            &clock,
            86_400_000,
            true,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Global),
            |_| true,
            |_| Some((k_global, CandidatePoolTier::Global)),
        )
        .expect("ticket");
    store.first_valid_frame(&pin_key, &t1, clock.now_ms(), 86_400_000);

    // 1. At 60 seconds idle (< 300s min idle), session stays on Global!
    clock.advance_ms(60_000);
    let t_60 = store
        .acquire(
            &pin_key,
            &clock,
            86_400_000,
            true,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own), // Own tier is now recovered!
            |_| true,
            |_| Some((k_recovered_own, CandidatePoolTier::Own)),
        )
        .expect("ticket");
    assert_eq!(t_60.outcome, PinOutcome::Followed);
    assert_eq!(t_60.upstream_key_id, k_global);

    // 2. At 301 seconds idle (>= 300s min idle after t_60), recovery repin triggers!
    clock.advance_ms(301_000);
    let t_301 = store
        .acquire(
            &pin_key,
            &clock,
            86_400_000,
            true,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own),
            |_| true,
            |_| Some((k_recovered_own, CandidatePoolTier::Own)),
        )
        .expect("ticket");
    assert_eq!(t_301.outcome, PinOutcome::RepinRecovery);
    assert_eq!(t_301.upstream_key_id, k_recovered_own);
}

#[doc = "Spec: SECTION-10.4, SEC-17 — F11 manual pin deletion evicts only the addressed pin"]
#[test]
fn test_remove_exact_pin_keeps_other_sessions() {
    let store = PinStore::new(10_000);
    let clock = FakeClock::new(1_000_000_000_000);
    let client_key_id = ClientKeyId::generate();
    let upstream_key_id = UpstreamKeyId::generate();

    let target = PinKey {
        client_key_id,
        canonical_id: "model-a".to_string(),
        session_hash: "session-a".to_string(),
    };
    let sibling = PinKey {
        client_key_id,
        canonical_id: "model-b".to_string(),
        session_hash: "session-a".to_string(),
    };
    for key in [&target, &sibling] {
        store
            .acquire(
                key,
                &clock,
                86_400_000,
                false,
                300,
                RoutingScope::PersonalFirst,
                Some(CandidatePoolTier::Own),
                |_| true,
                |_| Some((upstream_key_id, CandidatePoolTier::Own)),
            )
            .expect("ticket");
    }

    assert!(store.remove(&target));
    assert_eq!(store.len(), 1);
    assert!(
        !store.remove(&target),
        "deleting an absent pin is idempotent"
    );
    assert!(store.entries.contains_key(&sibling));
}

#[doc = "Spec: SECTION-10.4, SEC-17 — F11 admin session deletion evicts every pin for the hash"]
#[test]
fn test_remove_by_session_hash_keeps_other_sessions() {
    let store = PinStore::new(10_000);
    let clock = FakeClock::new(1_000_000_000_000);
    let upstream_key_id = UpstreamKeyId::generate();

    let target_a = PinKey {
        client_key_id: ClientKeyId::generate(),
        canonical_id: "model-a".to_string(),
        session_hash: "session-target".to_string(),
    };
    let target_b = PinKey {
        client_key_id: ClientKeyId::generate(),
        canonical_id: "model-b".to_string(),
        session_hash: "session-target".to_string(),
    };
    let other = PinKey {
        client_key_id: ClientKeyId::generate(),
        canonical_id: "model-a".to_string(),
        session_hash: "session-other".to_string(),
    };
    for key in [&target_a, &target_b, &other] {
        store
            .acquire(
                key,
                &clock,
                86_400_000,
                false,
                300,
                RoutingScope::PersonalFirst,
                Some(CandidatePoolTier::Own),
                |_| true,
                |_| Some((upstream_key_id, CandidatePoolTier::Own)),
            )
            .expect("ticket");
    }

    assert_eq!(store.remove_by_session_hash("session-target"), 2);
    assert_eq!(store.len(), 1);
    assert!(store.entries.contains_key(&other));
    assert_eq!(store.remove_by_session_hash("session-target"), 0);
}

#[doc = "Spec: PRV-8, CLK-4, SHR-5"]
#[test]
fn test_invalidate_client_key_evicts_only_that_keys_pins() {
    let store = PinStore::new(10_000);
    let clock = FakeClock::new(1_000_000_000_000);
    let upstream_key_id = UpstreamKeyId::generate();

    let revoked_client_key = ClientKeyId::generate();
    let live_client_key = ClientKeyId::generate();
    let mut keys = Vec::new();
    for (client_key_id, canonical_id) in [
        (revoked_client_key, "model-a"),
        (revoked_client_key, "model-b"),
        (live_client_key, "model-a"),
    ] {
        let pin_key = PinKey {
            client_key_id,
            canonical_id: canonical_id.to_string(),
            session_hash: "shared-session".to_string(),
        };
        store
            .acquire(
                &pin_key,
                &clock,
                86_400_000,
                false,
                300,
                RoutingScope::PersonalFirst,
                Some(CandidatePoolTier::Own),
                |_| true,
                |_| Some((upstream_key_id, CandidatePoolTier::Own)),
            )
            .expect("ticket");
        keys.push(pin_key);
    }

    assert_eq!(store.len(), 3);
    assert_eq!(store.invalidate_client_key(&revoked_client_key), 2);
    assert_eq!(store.len(), 1);
    assert!(store.entries.contains_key(&keys[2]));
    assert_eq!(store.invalidate_client_key(&revoked_client_key), 0);

    let mut batch = std::collections::HashSet::new();
    batch.insert(live_client_key);
    assert_eq!(store.invalidate_client_keys(&batch), 1);
    assert!(store.is_empty());
    assert_eq!(
        store.invalidate_client_keys(&std::collections::HashSet::new()),
        0
    );
}

#[doc = "Spec: RTE-2, RTE-10, REL-1, REL-4"]
#[test]
fn test_expired_provisional_excludes_tried_keys_and_confirmed_pin_coalesces_persistence() {
    let store = PinStore::new(10_000);
    let clock = FakeClock::new(1_000_000_000_000);
    let pin_key = PinKey {
        client_key_id: ClientKeyId::generate(),
        canonical_id: "claude-sonnet-4.6".to_string(),
        session_hash: "sess-f051-f059".to_string(),
    };
    let k1 = UpstreamKeyId::generate();
    let k2 = UpstreamKeyId::generate();

    // 1. Provisional pin on k1 expires after 50s -> next acquire excludes k1 and picks k2 (F-059)
    let t1 = store
        .acquire(
            &pin_key,
            &clock,
            86_400_000,
            false,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own),
            |_| true,
            |_| Some((k1, CandidatePoolTier::Own)),
        )
        .expect("t1");
    assert_eq!(t1.upstream_key_id, k1);

    clock.advance_ms(50_001);
    let t2 = store
        .acquire(
            &pin_key,
            &clock,
            86_400_000,
            false,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own),
            |_| true,
            |excluded| {
                assert!(
                    excluded.contains(&k1),
                    "expired provisional key k1 must be excluded on reselect"
                );
                Some((k2, CandidatePoolTier::Own))
            },
        )
        .expect("t2");
    assert_eq!(t2.upstream_key_id, k2);
    assert_eq!(t2.outcome, PinOutcome::Repin);

    // 2. Confirm on k2 -> sets last_persisted_ms; coalesced persistence triggers only after >= 60s (F-051)
    let confirmed = store
        .first_valid_frame(&pin_key, &t2, clock.now_ms(), 86_400_000)
        .expect("confirmed");
    assert_eq!(confirmed.state, PinState::Confirmed);

    clock.advance_ms(30_000);
    assert!(
        store
            .take_coalesced_persist(&pin_key, clock.now_ms(), 86_400_000)
            .is_none(),
        "must not persist again within 60s coalescing window"
    );

    clock.advance_ms(30_001);
    assert!(
        store
            .take_coalesced_persist(&pin_key, clock.now_ms(), 86_400_000)
            .is_some(),
        "must emit coalesced persist after 60s"
    );
}

#[doc = "Spec: RTE-10, CLK-1 — F039 active pin filtering and F043 wildcard model allowlist"]
#[test]
fn test_phase_2b_pin_active_filtering_and_wildcard_models() {
    let store = PinStore::new(100);
    let ck = ClientKeyId::generate();
    let k1 = UpstreamKeyId::generate();
    let pk1 = PinKey {
        client_key_id: ck,
        canonical_id: "gpt-4o".to_string(),
        session_hash: "s1".to_string(),
    };
    let pk2 = PinKey {
        client_key_id: ck,
        canonical_id: "gpt-4o".to_string(),
        session_hash: "s2".to_string(),
    };
    store.upsert_confirmed(pk1, k1, CandidatePoolTier::Own, 1_000, 5_000);
    store.upsert_confirmed(pk2, k1, CandidatePoolTier::Own, 1_000, 50_000);

    assert_eq!(store.active_len(10_000), 1);
    assert_eq!(store.list_active(10_000).len(), 1);
    store.sweep(10_000);
    assert_eq!(store.len(), 1);

    // F043: Wildcard and canonical model matching
    let allowed = vec!["gpt-4o*".to_string(), "anthropic/claude-*".to_string()];
    assert!(crate::client_key::is_model_allowed_by_patterns(
        &allowed,
        "openai/gpt-4o-mini",
        "gpt-4o-mini"
    ));
    assert!(crate::client_key::is_model_allowed_by_patterns(
        &allowed,
        "claude-sonnet-4.6",
        "claude-sonnet-4.6"
    ));
    assert!(!crate::client_key::is_model_allowed_by_patterns(
        &allowed,
        "deepseek-r1",
        "deepseek-r1"
    ));
}

// Spec: ./docs/srdd/10. SRDD - Routing Design.md#104-session-pin-state-machine | Phase: ./docs/bigbang/big-bang-phases-3.md#32-sub-phase-32-two-phase-session-pinstore-and-response-link-hard-affinity
#[doc = "Spec: RTE-11"]
#[test]
fn test_admin_move_with_tier_updates_pool_tier_and_generation() {
    let store = PinStore::new(100);
    let ck = ClientKeyId::generate();
    let k_global = UpstreamKeyId::generate();
    let k_own = UpstreamKeyId::generate();
    let pk = PinKey {
        client_key_id: ck,
        canonical_id: "claude-sonnet-4.6".to_string(),
        session_hash: "sess_move_tier".to_string(),
    };
    store.upsert_confirmed(
        pk.clone(),
        k_global,
        CandidatePoolTier::Global,
        1_000,
        60_000,
    );
    store.admin_move_with_tier(&pk, k_own, Some(CandidatePoolTier::Own), 2_000);

    let clock = crate::seams::FakeClock::new(2_500);
    let ticket = store
        .acquire(
            &pk,
            &clock,
            60_000,
            false,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own),
            |id| id == k_own,
            |_| Some((k_own, CandidatePoolTier::Own)),
        )
        .expect("ticket");
    assert_eq!(ticket.upstream_key_id, k_own);
    assert_eq!(ticket.pool_tier, CandidatePoolTier::Own);
    assert_eq!(ticket.outcome, PinOutcome::Followed);
}

// ---------------------------------------------------------------------------
// ISSUE-0169 — capacity pressure must reclaim expired entries instead of refusing
// ---------------------------------------------------------------------------

/// Build a `PinKey` for the capacity tests.
fn capacity_key(session: &str) -> PinKey {
    PinKey {
        client_key_id: ClientKeyId::generate(),
        canonical_id: "capacity-model".to_string(),
        session_hash: session.to_string(),
    }
}

/// Acquire one pin, expecting a ticket.
fn acquire_one(store: &PinStore, key: &PinKey, clock: &FakeClock, k: UpstreamKeyId) -> PinTicket {
    store
        .acquire(
            key,
            clock,
            86_400_000,
            false,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own),
            |_| true,
            |_| Some((k, CandidatePoolTier::Own)),
        )
        .expect("selection must succeed")
}

/// Fill `store` with `count` confirmed entries that are **already expired** at `now_ms`.
///
/// Written through `upsert_confirmed` with a TTL that puts `expires_ms` in the past relative to the
/// clock afterwards, which is the state a real store reaches whenever writes outpace the 30 s
/// background sweep.
fn fill_with_expired(store: &PinStore, count: usize, now_ms: i64, k: UpstreamKeyId) {
    for index in 0..count {
        store.upsert_confirmed(
            capacity_key(&format!("stale-{index}")),
            k,
            CandidatePoolTier::Own,
            now_ms.saturating_sub(1_000),
            // expires 1 000 ms after created_ms, i.e. before `now_ms`.
            1_000,
        );
    }
}

/// A full store whose entries have all expired must reclaim one and insert the new pin
/// (`ISSUE-0169`).
///
/// **This is the measured defect.** A store at capacity with `active_len(now_ms) == 0` — not one
/// live pin in it — still returns `UnpinnedCapacity` and refuses the new session. The capacity
/// branch consults only `entries.len()`, which counts expired entries that the 30 s background
/// sweep has not collected yet.
///
/// The window is bounded by the sweep interval rather than permanent (the issue claims permanent;
/// see the cycle-42 report), but inside it a burst of new sessions all lose affinity even though
/// the store is empty of anything live.
#[doc = "Spec: RTE-10, F039 — ISSUE-0169"]
#[test]
fn an_expired_pin_is_reclaimed_when_the_store_is_full() {
    let store = PinStore::new(2);
    let clock = FakeClock::new(1_000_000_000_000);
    let k = UpstreamKeyId::generate();

    fill_with_expired(&store, 2, clock.now_ms(), k);
    assert_eq!(store.len(), 2, "precondition: the map is full");
    assert_eq!(
        store.active_len(clock.now_ms()),
        0,
        "precondition: nothing in the store is live"
    );

    let fresh = capacity_key("fresh-session");
    let ticket = acquire_one(&store, &fresh, &clock, k);

    assert_ne!(
        ticket.outcome,
        PinOutcome::UnpinnedCapacity,
        "a store full of expired pins must make room, not refuse"
    );
    assert_eq!(ticket.outcome, PinOutcome::NewPin);
    assert!(
        store.len() <= 2,
        "an expired entry must have been reclaimed to stay within the cap, len={}",
        store.len()
    );
    assert!(
        store.entries.contains_key(&fresh),
        "the new pin must actually be inserted"
    );
}

/// The acceptance test for the degradation (`ISSUE-0169`).
///
/// A store that is already full of expired pins must keep serving new sessions indefinitely, as
/// long as each session's own pin expires between rounds. That second clause is what makes this a
/// realistic test rather than a tautology: a pin that is confirmed and kept live genuinely occupies
/// capacity, and `RTE-10`'s refusal is then *correct*. The defect is refusing while holding nothing
/// live.
///
/// So each round: acquire (the capacity branch must reclaim the round-before-last's expired
/// provisional), then advance past the 50 s provisional expiry. Without the reclaim the very first
/// round is refused, and every later one too — measured pre-fix at 92 of 100 refused even with
/// reclaiming, because a confirmed pin never expires.
#[doc = "Spec: RTE-10, F039 — ISSUE-0169"]
#[test]
fn a_store_that_is_full_of_stale_pins_never_degrades() {
    let cap = 8;
    let store = PinStore::new(cap);
    let clock = FakeClock::new(1_000_000_000_000);
    let k = UpstreamKeyId::generate();

    fill_with_expired(&store, cap, clock.now_ms(), k);

    let mut degraded = 0_usize;
    for index in 0..100 {
        let key = capacity_key(&format!("run-{index}"));
        let ticket = acquire_one(&store, &key, &clock, k);
        if ticket.outcome == PinOutcome::UnpinnedCapacity {
            degraded += 1;
        }
        // Let this round's provisional pin expire, exactly as a session that goes quiet does.
        // Pins are left unconfirmed on purpose: a confirmed pin is supposed to hold capacity, and
        // a test that kept confirming would be asserting that the cap is unreachable.
        clock.advance_ms(60_000);
        assert!(
            store.len() <= cap,
            "the cap must still be respected at iteration {index}, len={}",
            store.len()
        );
    }

    assert_eq!(
        degraded, 0,
        "no acquisition in a 100-session run of expiring sessions may be refused; {degraded} were"
    );
}

/// `UnpinnedCapacity` must stay reachable when nothing can be freed (`RTE-10`).
///
/// This is the safety valve, and it is what makes the fix a repair of a window rather than a
/// removal of the limit. A full store of **live** confirmed pins with a genuine cap must still
/// refuse — the alternative would be evicting a live, confirmed pin, which `15. SRDD` rejects by
/// name for exactly this structure ("TinyLFU eviction would silently drop confirmed pins").
#[doc = "Spec: RTE-10 — ISSUE-0169"]
#[test]
fn capacity_is_still_reported_when_nothing_can_be_freed() {
    let store = PinStore::new(2);
    let clock = FakeClock::new(1_000_000_000_000);
    let k = UpstreamKeyId::generate();

    for index in 0..2 {
        let key = capacity_key(&format!("live-{index}"));
        let ticket = acquire_one(&store, &key, &clock, k);
        store.first_valid_frame(&key, &ticket, clock.now_ms(), 86_400_000);
    }
    assert_eq!(store.len(), 2);
    assert_eq!(
        store.active_len(clock.now_ms()),
        2,
        "precondition: both entries are live confirmed pins"
    );

    let overflow = acquire_one(&store, &capacity_key("overflow"), &clock, k);
    assert_eq!(
        overflow.outcome,
        PinOutcome::UnpinnedCapacity,
        "with nothing expired there is nothing to reclaim, so RTE-10's fallback must fire"
    );
    assert_eq!(store.len(), 2, "no live entry may be evicted");
}

/// The capacity branch must not disturb live pins while reclaiming expired ones (`ISSUE-0169`).
///
/// Mixed store: some live, some expired, at capacity. Reclaiming must free *only* the expired ones
/// and leave every live pin in place — including its `upstream_key_id` and `generation`, so an
/// in-flight stream holding a ticket is not invalidated by an unrelated capacity event.
#[doc = "Spec: RTE-10, RTE-2 — ISSUE-0169"]
#[test]
fn reclaiming_expired_entries_leaves_live_pins_untouched() {
    let now = 1_000_000_000_000;
    let clock = FakeClock::new(now);
    let k = UpstreamKeyId::generate();
    let store = PinStore::new(3);

    // One live confirmed pin.
    let live_key = capacity_key("live");
    let live_ticket = acquire_one(&store, &live_key, &clock, k);
    store.first_valid_frame(&live_key, &live_ticket, now, 86_400_000);
    // Two expired pins.
    fill_with_expired(&store, 2, now, k);
    assert_eq!(store.len(), 3);

    let fresh = capacity_key("fresh");
    let ticket = acquire_one(&store, &fresh, &clock, k);
    assert_ne!(ticket.outcome, PinOutcome::UnpinnedCapacity);

    let live = store
        .entries
        .get(&live_key)
        .expect("the live pin must survive")
        .lock()
        .clone();
    assert_eq!(
        live.upstream_key_id, k,
        "the live pin's target must be unchanged"
    );
    assert_eq!(
        live.generation, live_ticket.generation,
        "the live pin's generation must not be bumped by a capacity event"
    );
    assert!(
        live.expires_ms > now,
        "the live pin must still be unexpired"
    );
}

/// Concurrent acquisition at capacity must not deadlock and must not lose the cap
/// (`ISSUE-0169`).
///
/// The capacity branch mutates the same `DashMap` that `acquire` reads, and `DashMap` iteration
/// holds shard locks — so this is the test most likely to hang rather than fail. It is bounded by
/// a watchdog thread: a deadlock is reported as a failure, not as a hung suite (cycle 38's lesson:
/// an unbounded wait on something that never terminates is a lost cycle, and the bound *is* the
/// measurement).
#[doc = "Spec: RTE-10, REL-1 — ISSUE-0169"]
#[test]
fn the_capacity_branch_does_not_deadlock_under_concurrent_acquire() {
    use std::sync::Arc;
    use std::sync::mpsc;

    let cap = 8;
    let store = Arc::new(PinStore::new(cap));
    let k = UpstreamKeyId::generate();
    let clock = Arc::new(FakeClock::new(1_000_000_000_000));

    // Start full of already-expired entries, so every worker hits the capacity branch.
    fill_with_expired(&store, cap, clock.now_ms(), k);

    let (done_tx, done_rx) = mpsc::channel::<(usize, usize)>();
    let mut handles = Vec::new();
    for worker in 0..6 {
        let store = Arc::clone(&store);
        let clock = Arc::clone(&clock);
        let done_tx = done_tx.clone();
        handles.push(thread::spawn(move || {
            let mut refused = 0;
            let mut inserted = 0;
            for index in 0..30 {
                let key = capacity_key(&format!("worker-{worker}-{index}"));
                let ticket = store
                    .acquire(
                        &key,
                        &*clock,
                        86_400_000,
                        false,
                        300,
                        RoutingScope::PersonalFirst,
                        Some(CandidatePoolTier::Own),
                        |_| true,
                        |_| Some((k, CandidatePoolTier::Own)),
                    )
                    .expect("selection");
                match ticket.outcome {
                    PinOutcome::UnpinnedCapacity => refused += 1,
                    PinOutcome::NewPin => inserted += 1,
                    _ => {}
                }
            }
            let _ = done_tx.send((refused, inserted));
        }));
    }
    drop(done_tx);

    // Watchdog: if the shard locks deadlock, this returns Err instead of hanging the suite.
    let (watch_tx, watch_rx) = mpsc::channel();
    thread::spawn(move || {
        let (refused, inserted) = done_rx.iter().fold((0_usize, 0_usize), |acc, item| {
            (acc.0 + item.0, acc.1 + item.1)
        });
        let _ = watch_tx.send((refused, inserted));
    });
    let (total_refused, total_inserted) = watch_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("capacity branch deadlocked: no worker finished within 30 s");

    for handle in handles {
        handle.join().expect("worker thread must not panic");
    }

    // The property under test is that the capacity branch *makes room* under concurrency instead
    // of refusing everything. Asserted on the observed insertions rather than on the store's final
    // population, because the population depends on interleaving.
    //
    // Deliberately **not** asserting `len() <= cap` here: the check-then-insert is not atomic, so
    // concurrent acquirers can overshoot. That overshoot is pre-existing (measured on the unfixed
    // code: `len=9` against `cap=8` in 5 of 25 runs) and is reported as an out-of-scope finding
    // rather than folded into this fix, which is about reclaiming expired entries.
    assert!(
        total_inserted > 0,
        "expired entries must be reclaimed under concurrency, so at least one acquire must \
         insert; refused={total_refused} inserted={total_inserted}"
    );
}

/// `insert_hydrated` must reclaim expired entries rather than silently drop the pin
/// (`ISSUE-0169`).
///
/// The hydration query filters `expires_at > now`, but it and this insert are not atomic, and
/// `upsert_confirmed` reaches this path at runtime too — so a stale `len()` here silently discards
/// a legitimate pin, with no ticket outcome to notice it by. Mutation M4 (deleting the reclaim from
/// this function) survived the first cut of the suite precisely because nothing exercised it.
#[doc = "Spec: RTE-10, RTE-12 — ISSUE-0169"]
#[test]
fn insert_hydrated_reclaims_expired_entries_rather_than_dropping_the_pin() {
    let now = 1_000_000_000_000;
    let store = PinStore::new(2);
    let k = UpstreamKeyId::generate();

    fill_with_expired(&store, 2, now, k);
    assert_eq!(store.len(), 2, "precondition: full");
    assert_eq!(
        store.active_len(now),
        0,
        "precondition: nothing live, so capacity is not genuinely reached"
    );

    let incoming = capacity_key("hydrated");
    store.insert_hydrated(
        incoming.clone(),
        PinEntry {
            state: PinState::Confirmed,
            upstream_key_id: k,
            generation: 1,
            pool_tier: CandidatePoolTier::Own,
            created_ms: now,
            last_used_ms: now,
            expires_ms: now + 86_400_000,
            repin_count: 0,
            tried_this_generation: Vec::new(),
        },
    );

    assert!(
        store.entries.contains_key(&incoming),
        "a still-valid pin must not be dropped just because the map held expired entries"
    );
    assert!(
        store.len() <= 2,
        "the cap must still hold, len={}",
        store.len()
    );
}
