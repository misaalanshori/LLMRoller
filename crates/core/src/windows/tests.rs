//! Tests for rolling rings, calendar-month windows, and decaying UpstreamObservation.
//!
//! Spec: Section 6.9, Section 13.2, Section 13.2.1, Requirements: LIM-1, Partial: USG-6, Partial: RTE-3.

use super::*;

#[doc = "Spec: LIM-1, Partial: USG-6"]
#[test]
fn test_rolling_ring_bucket_geometry_and_aging() {
    // 5h window: 18,000,000 ms -> 300 buckets of 60,000 ms (1 min)
    let ring_5h = RollingRing::new(18_000_000);
    assert_eq!(ring_5h.bucket_width_ms, 60_000);
    assert_eq!(ring_5h.bucket_count, 300);

    // 7d window: 604,800,000 ms -> 360 buckets of 1,680,000 ms (28 min)
    let ring_7d = RollingRing::new(604_800_000);
    assert_eq!(ring_7d.bucket_width_ms, 1_680_000);
    assert_eq!(ring_7d.bucket_count, 360);

    // 30d window: 2,592_000_000 ms -> 360 buckets of 7,200_000 ms (120 min)
    let ring_30d = RollingRing::new(2_592_000_000);
    assert_eq!(ring_30d.bucket_width_ms, 7_200_000);
    assert_eq!(ring_30d.bucket_count, 360);

    // Record spend in ring_5h and verify aging at exact bucket boundary
    let mut ring = ring_5h;
    let t0 = 1_000_000_000_000; // arbitrary base aligned to minute
    let t0 = (t0 / 60_000) * 60_000;

    ring.record(t0, 10_000); // 10,000 micro-USD at t0
    ring.record(t0 + 60_000, 20_000); // 20,000 micro-USD at t0 + 1 min

    // At t0 + 2 min, total is 30,000
    assert_eq!(ring.sum_at(t0 + 120_000), 30_000);

    // At t0 + 18,000,000 ms (exact window boundary for t0), t0 is still inside (t0 > t0 + 18M - 18M is false: min_start is t0, so bucket > min_start is false)
    assert_eq!(ring.sum_at(t0 + 18_000_000), 20_000);
    // At t0 + 18,000_000 + 60_000 ms, t0 + 1 min also ages out
    assert_eq!(ring.sum_at(t0 + 18_060_000), 0);

    // ManualOffset test
    let offset = ManualOffset {
        effective_at_ms: t0,
        delta_micro_usd: 50_000,
        period_ms: 18_000_000,
    };
    assert_eq!(offset.value_at(t0 - 1), 0);
    assert_eq!(offset.value_at(t0), 50_000);
    assert_eq!(offset.value_at(t0 + 17_999_999), 50_000);
    assert_eq!(offset.value_at(t0 + 18_000_000), 0);
}

#[doc = "Spec: LIM-1, Partial: USG-6"]
#[test]
fn test_anchored_calendar_month_day_clamping() {
    // 31 January billing anchor at 14:30 UTC (870 minutes) in 2026 (non-leap year)
    let mut cal = AnchoredCalendarMonth::new(31, 870, 2026, 1);
    assert_eq!(cal.current_year, 2026);
    assert_eq!(cal.current_month, 1);
    // Window start: 2026-01-31 14:30 UTC
    let expected_start = utc_to_epoch_millis(2026, 1, 31, 870);
    assert_eq!(cal.window_start_ms, expected_start);

    // Next reset: February 2026 has 28 days -> clamped to 28 February 14:30 UTC
    let expected_feb_reset = utc_to_epoch_millis(2026, 2, 28, 870);
    assert_eq!(cal.resets_at_ms, expected_feb_reset);

    // Record spend in January
    cal.record(expected_start + 1000, 100_000);
    assert_eq!(cal.used_at(expected_start + 2000), 100_000);

    // Advance to February (at or past Feb reset)
    cal.advance(expected_feb_reset);
    assert_eq!(cal.current_year, 2026);
    assert_eq!(cal.current_month, 2);
    assert_eq!(cal.window_start_ms, expected_feb_reset);
    assert_eq!(cal.used_micro_usd, 0);

    // Next reset after Feb: March 2026 has 31 days -> clamped to 31 March 14:30 UTC
    let expected_mar_reset = utc_to_epoch_millis(2026, 3, 31, 870);
    assert_eq!(cal.resets_at_ms, expected_mar_reset);

    // Leap year test: 2024 has 29 days in February
    let leap_cal = AnchoredCalendarMonth::new(31, 0, 2024, 1);
    let expected_leap_feb = utc_to_epoch_millis(2024, 2, 29, 0);
    assert_eq!(leap_cal.resets_at_ms, expected_leap_feb);
}

#[doc = "Spec: LIM-1, Partial: USG-6"]
#[test]
fn test_decaying_upstream_observation_projection_and_expiry() {
    let t0 = 1_000_000_000_000;
    let obs = UpstreamObservation {
        used_micro_usd: 9_000_000, // $9.00 observed at 12:00
        observed_at_ms: t0,
        local_at_observation: 1_000_000, // $1.00 local captured at poll start
        resets_at_ms: Some(t0 + 3_600_000), // resets in 1 hour
    };

    // 1. Immediately after observation (t0 + 10s), local has not grown:
    // projected = 9M + max(0, 1M - 1M) = 9M.
    assert_eq!(obs.project(1_000_000, t0 + 10_000), 9_000_000);

    // 2. Local grows by $0.50 (local = 1.5M):
    // projected = 9M + (1.5M - 1M) = 9.5M.
    assert_eq!(obs.project(1_500_000, t0 + 60_000), 9_500_000);

    // 3. At t0 + 29 minutes (< 30 min MAX_OBSERVATION_AGE_MS), observation still active:
    assert_eq!(obs.project(1_500_000, t0 + 29 * 60_000), 9_500_000);

    // 4. At t0 + 30 minutes, observation expires! Local ring governs alone.
    assert_eq!(
        obs.project(1_500_000, t0 + MAX_OBSERVATION_AGE_MS),
        1_500_000
    );

    // 5. If resets_at_ms occurs earlier (e.g. at t0 + 15 min), it expires at resets_at_ms:
    let short_obs = UpstreamObservation {
        used_micro_usd: 9_000_000,
        observed_at_ms: t0,
        local_at_observation: 1_000_000,
        resets_at_ms: Some(t0 + 15 * 60_000),
    };
    assert!(!short_obs.is_expired(t0 + 14 * 60_000));
    assert!(short_obs.is_expired(t0 + 15 * 60_000));
    assert_eq!(short_obs.project(1_200_000, t0 + 15 * 60_000), 1_200_000);
}
