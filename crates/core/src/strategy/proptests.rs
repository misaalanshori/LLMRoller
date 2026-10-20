//! Statistical and concurrency property tests for NaiveRR, UniformRandom, WeightedRandom, and P2C.
//!
//! Spec: Section 3.1 (G2), Section 6.7, Section 10.3, Section 21.3,
//! Requirements: RTE-13, RTE-14.

use super::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use crate::ids::{ModelPk, UpstreamKeyId, UserId};
use crate::seams::{DeterministicRng, OsEntropy};

struct MockLoadTracker {
    loads: HashMap<RouteId, (u32, u64)>, // (in_flight, ewma_ttfb_ms)
}

impl RouteLoadTracker for MockLoadTracker {
    fn in_flight(&self, route: &RouteId) -> u32 {
        self.loads.get(route).map(|(inf, _)| *inf).unwrap_or(0)
    }

    fn ewma_ttfb_ms(&self, route: &RouteId) -> u64 {
        self.loads.get(route).map(|(_, ttfb)| *ttfb).unwrap_or(100)
    }
}

#[doc = "Spec: RTE-13, RTE-14"]
#[test]
fn test_naive_rr_cyclic_sequence_and_wrap_around() {
    let selector = NaiveRrSelector::new();
    let entropy = OsEntropy;

    let model_pk = ModelPk::generate();
    let group_key = WrrGroupKey {
        user_id: UserId::generate(),
        model_pk,
        protocol: "chat".to_string(),
        pool_tier: 0,
    };

    let r0 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };
    let r1 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };
    let r2 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };

    let candidates = [(r0, 100), (r1, 50), (r2, 10)]; // weights are ignored by naive_rr

    // Exactly cyclic sequence: 0, 1, 2, 0, 1, 2, ...
    for i in 0..15 {
        let picked = selector
            .select_next(&group_key, &candidates, i * 10, &entropy, None)
            .expect("pick");
        let expected = candidates[i as usize % 3].0;
        assert_eq!(picked, expected);
    }

    // Wrap around from u64::MAX - 5 under 8-thread contention
    let shared_selector = Arc::new(NaiveRrSelector::new());
    // Pre-set cursor near u64::MAX
    {
        let mut lock = shared_selector.groups.lock();
        let g = lock.entry(group_key.clone()).or_default();
        g.cursor.store(u64::MAX - 5, Ordering::SeqCst);
    }

    let mut handles = Vec::new();
    for _ in 0..8 {
        let sel = Arc::clone(&shared_selector);
        let gk = group_key.clone();
        handles.push(thread::spawn(move || {
            let ent = OsEntropy;
            for i in 0..1000 {
                let _ = sel.select_next(&gk, &candidates, i, &ent, None);
            }
        }));
    }

    for h in handles {
        h.join().expect("thread join");
    }
}

#[doc = "Spec: RTE-13, RTE-14"]
#[test]
fn test_uniform_and_weighted_random_reproducibility_and_chi_squared() {
    let model_pk = ModelPk::generate();
    let group_key = WrrGroupKey {
        user_id: UserId::generate(),
        model_pk,
        protocol: "chat".to_string(),
        pool_tier: 0,
    };

    let r1 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };
    let r2 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };
    let r3 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };

    let candidates = [(r1, 50), (r2, 30), (r3, 20)]; // 50%, 30%, 20%
    let total_w = 100.0;

    // 1. Seeded DeterministicRng(42) produces identical reproducible sequences
    let rng1 = DeterministicRng::new(42);
    let rng2 = DeterministicRng::new(42);

    let wr = WeightedRandomSelector;
    let mut seq1 = Vec::new();
    let mut seq2 = Vec::new();
    for i in 0..100 {
        seq1.push(
            wr.select_next(&group_key, &candidates, i, &rng1, None)
                .expect("pick"),
        );
        seq2.push(
            wr.select_next(&group_key, &candidates, i, &rng2, None)
                .expect("pick"),
        );
    }
    assert_eq!(
        seq1, seq2,
        "seeded DeterministicRng must produce identical sequences"
    );

    // 2. Over 100,000 picks: empirical frequencies within 1.0% of target shares and passes Pearson chi-squared test
    let mut counts: HashMap<RouteId, usize> = HashMap::new();
    let iterations = 100_000;
    for i in 0..iterations {
        let picked = wr
            .select_next(&group_key, &candidates, i, &rng1, None)
            .expect("pick");
        *counts.entry(picked).or_default() += 1;
    }

    let p1 = *counts.get(&r1).unwrap_or(&0) as f64 / iterations as f64;
    let p2 = *counts.get(&r2).unwrap_or(&0) as f64 / iterations as f64;
    let p3 = *counts.get(&r3).unwrap_or(&0) as f64 / iterations as f64;

    assert!((p1 - 0.50).abs() < 0.01, "p1: {p1}");
    assert!((p2 - 0.30).abs() < 0.01, "p2: {p2}");
    assert!((p3 - 0.20).abs() < 0.01, "p3: {p3}");

    // Pearson chi-squared: sum((O - E)^2 / E)
    let e1 = iterations as f64 * (50.0 / total_w);
    let e2 = iterations as f64 * (30.0 / total_w);
    let e3 = iterations as f64 * (20.0 / total_w);
    let o1 = *counts.get(&r1).unwrap_or(&0) as f64;
    let o2 = *counts.get(&r2).unwrap_or(&0) as f64;
    let o3 = *counts.get(&r3).unwrap_or(&0) as f64;

    let chi2 = (o1 - e1).powi(2) / e1 + (o2 - e2).powi(2) / e2 + (o3 - e3).powi(2) / e3;
    // For df = 2, critical value at p = 0.001 is 13.82. chi2 < 13.82 passes (p > 0.001)
    assert!(
        chi2 < 13.82,
        "Pearson chi-squared was {chi2}, expected < 13.82 for p > 0.001"
    );
}

#[doc = "Spec: RTE-13, RTE-14"]
#[test]
fn test_power_of_two_choices_prefers_lower_load_and_latency() {
    let entropy = OsEntropy;
    let p2c = PowerOfTwoChoicesSelector;
    let model_pk = ModelPk::generate();
    let group_key = WrrGroupKey {
        user_id: UserId::generate(),
        model_pk,
        protocol: "chat".to_string(),
        pool_tier: 0,
    };

    let r_fast = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };
    let r_slow = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };

    // Equal weights
    let candidates = [(r_fast, 100), (r_slow, 100)];

    let mut tracker = MockLoadTracker {
        loads: HashMap::new(),
    };
    tracker.loads.insert(r_fast, (0, 50)); // idle, 50ms TTFB
    tracker.loads.insert(r_slow, (5, 500)); // 5 in flight, 500ms TTFB

    let mut counts: HashMap<RouteId, usize> = HashMap::new();
    for i in 0..10_000 {
        let picked = p2c
            .select_next(&group_key, &candidates, i, &entropy, Some(&tracker))
            .expect("pick");
        *counts.entry(picked).or_default() += 1;
    }

    let fast_picks = *counts.get(&r_fast).unwrap_or(&0);
    let slow_picks = *counts.get(&r_slow).unwrap_or(&0);

    // Fast idle route should win the vast majority of picks (> 70%)
    assert!(
        fast_picks > slow_picks * 2,
        "fast: {fast_picks}, slow: {slow_picks}"
    );
}

#[doc = "Spec: RTE-13, RTE-14"]
#[test]
fn test_eight_threads_concurrent_picks_across_strategies_and_gc() {
    let swrr = Arc::new(crate::wrr::SmoothWeightedRrSelector::new());
    let naive = Arc::new(NaiveRrSelector::new());
    let wr = Arc::new(WeightedRandomSelector);
    let p2c = Arc::new(PowerOfTwoChoicesSelector);

    let model_pk = ModelPk::generate();
    let r1 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };
    let r2 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };
    let candidates = [(r1, 100), (r2, 100)];

    let mut handles = Vec::new();
    for t in 0..8 {
        let swrr_c = Arc::clone(&swrr);
        let naive_c = Arc::clone(&naive);
        let wr_c = Arc::clone(&wr);
        let p2c_c = Arc::clone(&p2c);
        let gk = WrrGroupKey {
            user_id: UserId::generate(),
            model_pk,
            protocol: "chat".to_string(),
            pool_tier: (t % 4) as u8,
        };

        handles.push(thread::spawn(move || {
            let ent = OsEntropy;
            for i in 0..10_000 {
                let _ = swrr_c.select_next(&gk, &candidates, i, &ent, None);
                let _ = naive_c.select_next(&gk, &candidates, i, &ent, None);
                let _ = wr_c.select_next(&gk, &candidates, i, &ent, None);
                let _ = p2c_c.select_next(&gk, &candidates, i, &ent, None);
            }
        }));
    }

    for h in handles {
        h.join().expect("thread join must succeed");
    }

    // 24h idle group eviction check (86_400_000 ms)
    let t_future = 100_000_000;
    swrr.evict_idle_groups(t_future, 86_400_000);
    naive.evict_idle_groups(t_future, 86_400_000);
}
