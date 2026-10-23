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

#[doc = "Spec: LIM-1, Partial: USG-6"]
#[test]
fn test_anchored_fixed_aligns_backwards_for_future_anchor() {
    // Anchor in the future still identifies the instance containing `now`.
    let anchor = utc_to_epoch_millis(2026, 1, 31, 0);
    let now = utc_to_epoch_millis(2026, 1, 15, 0);
    let mut window = AnchoredFixed::new(18_000_000, anchor, now);
    assert!(
        window.window_start_ms <= now && now < window.resets_at_ms,
        "future anchors must align backwards to the instance containing now"
    );
    assert!(window.window_start_ms < anchor);
    assert_eq!(window.resets_at_ms - window.window_start_ms, 18_000_000);
    assert_eq!(window.used_at(now), 0);
}

#[doc = "Spec: LIM-1"]
#[test]
fn test_anchored_calendar_month_aligns_backwards_for_future_anchor() {
    // 31 January anchor evaluated on 15 January must resolve to the
    // 31 December 2025 instance rather than a future window.
    let now = utc_to_epoch_millis(2026, 1, 15, 870);
    let mut calendar = AnchoredCalendarMonth::new(31, 870, 2026, 1);
    calendar.align_to(now);
    assert_eq!(
        calendar.window_start_ms,
        utc_to_epoch_millis(2025, 12, 31, 870)
    );
    assert_eq!(calendar.resets_at_ms, utc_to_epoch_millis(2026, 1, 31, 870));
    assert!(calendar.current_year == 2025 && calendar.current_month == 12);
}

#[doc = "Spec: LIM-1"]
#[test]
fn test_epoch_millis_round_trips_utc_components() {
    let expected = (2026, 3, 1, 870);
    let ms = utc_to_epoch_millis(expected.0, expected.1, expected.2, expected.3);
    assert_eq!(epoch_millis_to_utc(ms), expected);
    assert_eq!(epoch_millis_to_utc(0), (1970, 1, 1, 0));
}

#[doc = "Spec: LIM-1 — F05 anchored_fixed counters enforce and reset on the anchor grid"]
#[test]
fn test_anchored_fixed_counter_enforces_and_resets() {
    let anchor = 1_000_000_000_000;
    let period = 18_000_000;
    let counter = AtomicBudgetCounter::for_window(
        7,
        10_000_000,
        0,
        WindowKind::AnchoredFixed,
        Some(period),
        Some(anchor),
        None,
        anchor + 1_000,
    );
    counter.record_actual(anchor + 1_000, 9_000_000);
    assert_eq!(counter.effective_used(anchor + 1_000), 9_000_000);
    assert!(counter.effective_used(anchor + 1_000) < counter.max_allowed_spend());

    // Spend reaches the cap inside the instance...
    counter.record_actual(anchor + 2_000, 1_000_000);
    assert_eq!(counter.effective_used(anchor + 2_000), 10_000_000);
    assert!(
        counter.effective_used(anchor + 2_000) >= counter.max_allowed_spend(),
        "spend at the cap must no longer be admitted"
    );

    // ...and the anchored grid resets the instance at `resets_at_ms`.
    assert_eq!(counter.effective_used(anchor + period), 0);
}

#[doc = "Spec: LIM-1, Partial: USG-6"]
#[test]
fn test_provider_counter_observation_moves_window_start() {
    let t0 = 1_000_000_000_000;
    let counter = AtomicBudgetCounter::for_window(
        9,
        10_000_000,
        0,
        WindowKind::AnchoredProvider,
        Some(18_000_000),
        None,
        Some("5h"),
        t0,
    );
    counter.record_actual(t0, 1_000);
    counter.record_actual(t0 + 10_000_000, 2_000);
    assert_eq!(counter.effective_used(t0 + 10_000_000), 3_000);

    // Provider reports a window starting at t0 + 10M: earlier local spend is
    // outside the reported instance.
    counter.observe(UpstreamObservation {
        used_micro_usd: 5_000_000,
        observed_at_ms: t0 + 10_000_000,
        local_at_observation: 2_000,
        resets_at_ms: Some(t0 + 28_000_000),
    });
    assert_eq!(counter.effective_used(t0 + 10_000_000), 5_000_000);
}

#[doc = "Spec: LIM-1, Partial: RTE-3"]
#[test]
fn test_50_concurrent_try_reserve_at_97_9_pct_caps_at_98_pct() {
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    let t0 = 1_000_000_000_000;
    // $10.00 window with 2% reserve -> max allowed spend is $10.00 * 0.98 = $9.80 (9_800_000 micro-USD)
    let limit_micro_usd = 10_000_000;
    let reserve_pct = 2;
    let period_ms = 18_000_000; // 5h

    let counter = Arc::new(AtomicBudgetCounter::new(
        1,
        limit_micro_usd,
        reserve_pct,
        period_ms,
    ));

    // Seed at 97.9% = 9_790_000 micro-USD
    counter.record_actual(t0, 9_790_000);
    assert_eq!(counter.effective_used(t0), 9_790_000);
    assert_eq!(counter.max_allowed_spend(), 9_800_000);

    // Rate that calculates minimum clamped estimate: $0.001 (1,000 micro-USD)
    let rates = crate::pricing::Rates {
        input_per_mtok_micro_usd: 1_000,
        output_per_mtok_micro_usd: 1_000,
        cache_read_per_mtok_micro_usd: 100,
        cache_write_per_mtok_micro_usd: 100,
    };
    let est = estimate_reservation_cost_micro_usd(100, 0.5, &rates, Some(10), Some(10));
    assert_eq!(est, 1_000); // exactly $0.001 clamped minimum

    // Headroom: 9_800_000 - 9_790_000 = 10_000 micro-USD.
    // Each reservation takes 1,000 micro-USD, so exactly 10 can fit.
    let admitted_count = Arc::new(AtomicUsize::new(0));
    let rejected_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..50 {
        let c: Arc<dyn BudgetCounter> = Arc::clone(&counter) as Arc<dyn BudgetCounter>;
        let admitted = Arc::clone(&admitted_count);
        let rejected = Arc::clone(&rejected_count);

        handles.push(thread::spawn(move || {
            match Reservation::try_reserve(vec![c], est, t0) {
                Ok(res) => {
                    admitted.fetch_add(1, Ordering::Relaxed);
                    // Simulate fast work and commit
                    res.commit(est, t0 + 1000);
                }
                Err(_) => {
                    rejected.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("thread join must succeed");
    }

    assert_eq!(admitted_count.load(Ordering::SeqCst), 10);
    assert_eq!(rejected_count.load(Ordering::SeqCst), 40);

    // Final reserved should be back to 0
    assert_eq!(counter.reserved_in_flight().load(Ordering::SeqCst), 0);
    // Final effective used is exactly 9_790_000 + 10 * 1_000 = 9_800_000
    assert_eq!(counter.effective_used(t0 + 1000), 9_800_000);
}

#[doc = "Spec: LIM-1, LIM-2"]
#[test]
fn test_atomic_budget_counter_manual_offset_and_calendar_month_window_guard() {
    let oct_15 = utc_to_epoch_millis(2025, 10, 15, 0);
    let nov_01 = utc_to_epoch_millis(2025, 11, 1, 0);
    let nov_02 = utc_to_epoch_millis(2025, 11, 2, 0);

    // F-065: AnchoredCalendarMonth ignores events before window_start_ms and resets at UTC month boundary
    let cal_counter = AtomicBudgetCounter::for_window(
        42,
        10_000_000,
        0,
        WindowKind::AnchoredCalendarMonth,
        None,
        Some(utc_to_epoch_millis(2025, 10, 1, 0)),
        None,
        oct_15,
    );
    cal_counter.record_actual(oct_15, 4_000_000);
    assert_eq!(cal_counter.effective_used(oct_15 + 1_000), 4_000_000);

    // At November 1 UTC boundary, October spend is reset to 0, and late October events are ignored
    assert_eq!(cal_counter.effective_used(nov_01), 0);
    cal_counter.record_actual(oct_15, 2_000_000);
    assert_eq!(cal_counter.effective_used(nov_02), 0);

    // F-064: ManualOffset applies to local_used and effective_used until period expiry
    cal_counter.set_manual_offset(Some(ManualOffset {
        effective_at_ms: nov_01,
        delta_micro_usd: 1_500_000,
        period_ms: 86_400_000,
    }));
    assert_eq!(cal_counter.local_used(nov_01 + 1_000), 1_500_000);
    assert_eq!(cal_counter.effective_used(nov_01 + 1_000), 1_500_000);
    assert_eq!(cal_counter.effective_used(nov_02 + 1_000), 0);
}
