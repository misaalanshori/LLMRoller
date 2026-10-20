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
            if st.first_valid_frame(&pk, &ticket, clk.now_ms(), 86_400_000) {
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
        !reverted,
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
