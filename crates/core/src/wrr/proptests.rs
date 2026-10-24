//! Property-based and statistical tests for SWRR, WRR, strict tiers, and headroom scaling.
//!
//! Spec: Section 3.1 (G2), Section 6.7, Section 10.3, Section 10.3.4,
//! Requirements: RTE-4, RTE-5, RTE-6, RTE-13, RTE-14.

use super::*;
use crate::ids::{ModelPk, UpstreamKeyId, UserId};
use crate::seams::OsEntropy;
use crate::strategy::{
    RouteId, RouteSelector, WrrGroupKey, compute_effective_weight, filter_strict_tier,
};

#[doc = "Spec: RTE-4, RTE-5, RTE-6, RTE-13, RTE-14"]
#[test]
fn test_worked_routing_example_and_swrr_distribution() {
    let entropy = OsEntropy;
    let selector = SmoothWeightedRrSelector::new();

    let user_id = UserId::generate();
    let model_pk = ModelPk::generate();
    let group_key = WrrGroupKey {
        user_id,
        model_pk,
        protocol: "chat".to_string(),
        pool_tier: 0,
    };

    // DeepSeek v4.1 Flash from SRDD Section 10.3.4:
    // Open Design AMR (100), OpenCode Go (60), Command Code GOAT (30).
    // Total W = 190.
    // Expected shares: AMR = 52.63%, Go = 31.58%, CC = 15.79%
    let r_amr = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };
    let r_go = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };
    let r_cc = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
    };

    let candidates = [(r_amr, 100), (r_go, 60), (r_cc, 30)];

    let mut counts: HashMap<RouteId, usize> = HashMap::new();
    let mut max_consecutive = 0;
    let mut current_consecutive = 0;
    let mut last_winner = None;

    let iterations = 100_000;
    for i in 0..iterations {
        let winner = selector
            .select_next(&group_key, &candidates, i as i64 * 10, &entropy, None)
            .expect("must pick a winner");

        *counts.entry(winner).or_default() += 1;

        if Some(winner) == last_winner {
            current_consecutive += 1;
        } else {
            current_consecutive = 1;
            last_winner = Some(winner);
        }
        max_consecutive = max_consecutive.max(current_consecutive);
    }

    let amr_share = *counts.get(&r_amr).unwrap_or(&0) as f64 / iterations as f64;
    let go_share = *counts.get(&r_go).unwrap_or(&0) as f64 / iterations as f64;
    let cc_share = *counts.get(&r_cc).unwrap_or(&0) as f64 / iterations as f64;

    // Check empirical distribution stays within 0.5% (0.005) of target shares
    assert!(
        (amr_share - 100.0 / 190.0).abs() < 0.005,
        "amr: {amr_share}"
    );
    assert!((go_share - 60.0 / 190.0).abs() < 0.005, "go: {go_share}");
    assert!((cc_share - 30.0 / 190.0).abs() < 0.005, "cc: {cc_share}");

    // SWRR max consecutive run <= ceil(max(W)/min(W)) = ceil(100/30) = 4
    assert!(
        max_consecutive <= 4,
        "max consecutive was {max_consecutive}, expected <= 4"
    );

    // Test WeightedRrSelector on the exact same candidates
    let wrr_selector = WeightedRrSelector::new();
    let mut wrr_counts: HashMap<RouteId, usize> = HashMap::new();
    for i in 0..iterations {
        let winner = wrr_selector
            .select_next(&group_key, &candidates, i as i64 * 10, &entropy, None)
            .expect("must pick a winner");
        *wrr_counts.entry(winner).or_default() += 1;
    }

    let wrr_amr_share = *wrr_counts.get(&r_amr).unwrap_or(&0) as f64 / iterations as f64;
    let wrr_go_share = *wrr_counts.get(&r_go).unwrap_or(&0) as f64 / iterations as f64;
    let wrr_cc_share = *wrr_counts.get(&r_cc).unwrap_or(&0) as f64 / iterations as f64;

    assert!(
        (wrr_amr_share - 100.0 / 190.0).abs() < 0.005,
        "wrr amr: {wrr_amr_share}"
    );
    assert!(
        (wrr_go_share - 60.0 / 190.0).abs() < 0.005,
        "wrr go: {wrr_go_share}"
    );
    assert!(
        (wrr_cc_share - 30.0 / 190.0).abs() < 0.005,
        "wrr cc: {wrr_cc_share}"
    );
}

#[doc = "Spec: RTE-4, RTE-5, RTE-6, RTE-13, RTE-14"]
#[test]
fn test_swrr_subset_variation_preserves_zero_sum_invariant() {
    let entropy = OsEntropy;
    let selector = SmoothWeightedRrSelector::new();
    let group_key = WrrGroupKey {
        user_id: UserId::generate(),
        model_pk: ModelPk::generate(),
        protocol: "chat".to_string(),
        pool_tier: 0,
    };

    let r1 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk: group_key.model_pk,
    };
    let r2 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk: group_key.model_pk,
    };
    let r3 = RouteId {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk: group_key.model_pk,
    };

    let full_set = [(r1, 50), (r2, 30), (r3, 20)];
    let subset = [(r1, 50), (r2, 30)]; // r3 temporarily dropped

    // Run 1000 picks on full set
    for i in 0..1000 {
        selector.select_next(&group_key, &full_set, i * 10, &entropy, None);
    }

    // Run 1000 picks on subset (dropping r3 for < 300 s)
    for i in 1000..2000 {
        selector.select_next(&group_key, &subset, i * 10, &entropy, None);
    }

    // Return to full set
    for i in 2000..3000 {
        selector.select_next(&group_key, &full_set, i * 10, &entropy, None);
    }

    // Verify sum(current over members) == 0 strictly holds
    let lock = selector.groups.lock();
    let group = lock.get(&group_key).expect("group must exist");
    let sum_current: i64 = group.members.iter().map(|m| m.current).sum();
    assert_eq!(
        sum_current, 0,
        "sum of current weights must strictly equal 0"
    );
}

#[doc = "Spec: RTE-4, RTE-5, RTE-6, RTE-13, RTE-14"]
#[test]
fn test_strict_tier_filtering_and_headroom_scaling() {
    // 1. Strict tier filtering (RTE-6)
    let candidates = [
        ("route-tier-3", Some(3)),
        ("route-tier-1a", Some(1)),
        ("route-tier-2", Some(2)),
        ("route-tier-1b", Some(1)),
    ];
    let filtered = filter_strict_tier(&candidates);
    assert_eq!(filtered.len(), 2);
    assert!(filtered.contains(&"route-tier-1a"));
    assert!(filtered.contains(&"route-tier-1b"));

    // 2. Headroom basis points scaling (RTE-13)
    let base_weight = 100;
    // When disabled, effective weight equals base weight
    assert_eq!(compute_effective_weight(base_weight, &[], false), 100);

    // When 80% remaining: limit = 100, used = 20, reserved = 0 -> remaining = 80 -> 8,000 bps
    let windows_80 = [(100, 20, 0)];
    let w_80 = compute_effective_weight(base_weight, &windows_80, true);
    assert_eq!(w_80, 80); // 100 * 8,000 / 10,000 = 80

    // When 2% remaining: limit = 100, used = 98 -> remaining = 2 -> 200 bps -> clamped to min 500 bps
    let windows_2 = [(100, 98, 0)];
    let w_clamped = compute_effective_weight(base_weight, &windows_2, true);
    assert_eq!(w_clamped, 5); // 100 * 500 / 10,000 = 5

    // Minimum weight is 1
    let w_min = compute_effective_weight(1, &windows_2, true);
    assert_eq!(w_min, 1);

    // 3. Direct basis points scaling (RTE-13, F-058)
    assert_eq!(
        crate::strategy::scale_weight_by_headroom_bps(100, 10_000, true),
        100
    );
    assert_eq!(
        crate::strategy::scale_weight_by_headroom_bps(100, 8_000, true),
        80
    );
    assert_eq!(
        crate::strategy::scale_weight_by_headroom_bps(100, 200, true),
        5
    );
    assert_eq!(
        crate::strategy::scale_weight_by_headroom_bps(100, 0, true),
        5
    );
    assert_eq!(
        crate::strategy::scale_weight_by_headroom_bps(100, 200, false),
        100
    );
}
