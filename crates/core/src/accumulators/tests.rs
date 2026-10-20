//! Tests for UTC calendar-month accumulators, rollover pruning, and gate G7.
//!
//! Spec: Section 6.2, Section 6.4, Section 6.5, Section 10.0, Section 13.2.2,
//! Requirements: USR-5, CLK-5, Partial: SHR-2.

use super::*;
use crate::ids::ClientKeyId;

#[doc = "Spec: USR-5, CLK-5, Partial: SHR-2"]
#[test]
fn test_midnight_straddling_reservation_posts_to_reservation_month() {
    let registry = MonthlyAccumulatorRegistry::new();
    let key_id = ClientKeyId::generate();
    let scope_id = BudgetScopeId::ClientKey(key_id);

    // 2026-01-31 23:59:59.900 UTC (100 ms before midnight into February)
    let jan_start = utc_to_epoch_millis(2026, 1, 31, 1439); // 23:59:00
    let reservation_ts = jan_start + 59_900; // 23:59:59.900
    let jan_month = UtcYearMonth::new(2026, 1);
    assert_eq!(UtcYearMonth::from_epoch_millis(reservation_ts), jan_month);

    let budget_cap = 10_000_000; // $10.00
    let estimate = 50_000; // $0.05

    // Acquire reservation in January
    let guard = registry
        .try_reserve(scope_id.clone(), Some(budget_cap), estimate, reservation_ts)
        .expect("reservation must succeed");

    assert_eq!(guard.reservation_month, jan_month);
    let jan_acc = registry.get_or_create(jan_month, scope_id.clone());
    assert_eq!(jan_acc.reserved(), estimate);
    assert_eq!(jan_acc.used(), 0);

    // Stream completes at 2026-02-01 00:00:04.000 UTC in February!
    let feb_completion_ts = utc_to_epoch_millis(2026, 2, 1, 0) + 4_000;
    let feb_month = UtcYearMonth::new(2026, 2);
    assert_eq!(
        UtcYearMonth::from_epoch_millis(feb_completion_ts),
        feb_month
    );

    // Commit actual cost of $0.04 (40,000 micro-USD)
    guard.commit(40_000);

    // Verify: Posted to January accumulator, and January reservation released!
    assert_eq!(jan_acc.reserved(), 0);
    assert_eq!(jan_acc.used(), 40_000);
    assert_eq!(jan_acc.effective_used(), 40_000);

    // Verify February accumulator is untouched (0 used, 0 reserved)
    let feb_acc = registry.get_or_create(feb_month, scope_id);
    assert_eq!(feb_acc.used(), 0);
    assert_eq!(feb_acc.reserved(), 0);
}

#[doc = "Spec: USR-5, CLK-5, Partial: SHR-2"]
#[test]
fn test_rollover_pruning_one_hour_after_rollover() {
    let registry = MonthlyAccumulatorRegistry::new();
    let key1 = BudgetScopeId::ClientKey(ClientKeyId::generate());
    let key2 = BudgetScopeId::ClientKey(ClientKeyId::generate());

    let jan_month = UtcYearMonth::new(2026, 1);
    let jan_acc1 = registry.get_or_create(jan_month, key1.clone());
    let jan_acc2 = registry.get_or_create(jan_month, key2.clone());

    // key1 completed work (used = 100k, reserved = 0)
    jan_acc1.used_micro_usd.store(100_000, Ordering::SeqCst);
    // key2 still has an open in-flight stream (reserved = 50k)
    jan_acc2.reserved_in_flight.store(50_000, Ordering::SeqCst);

    assert_eq!(registry.len(), 2);

    let feb_start = UtcYearMonth::new(2026, 2).start_epoch_millis();

    // 1. At 30 minutes after February rollover (30m < 1h): NO pruning should happen
    registry.prune_old_months(feb_start + 1_800_000);
    assert_eq!(
        registry.len(),
        2,
        "cannot prune before 1 hour after rollover"
    );

    // 2. At 61 minutes after February rollover (> 1h):
    // key1 has reserved == 0 -> pruned!
    // key2 has reserved > 0 -> retained!
    registry.prune_old_months(feb_start + 3_660_000);
    assert_eq!(registry.len(), 1, "entry with reserved == 0 must be pruned");

    // Finish key2's in-flight stream
    jan_acc2.reserved_in_flight.store(0, Ordering::SeqCst);
    // Prune again -> now key2 is also pruned
    registry.prune_old_months(feb_start + 3_700_000);
    assert_eq!(registry.len(), 0, "all old zero-reserved entries pruned");
}

#[doc = "Spec: USR-5, CLK-5, Partial: SHR-2"]
#[test]
fn test_gate_g7_exhaustion_returns_retry_after_and_cap_invalidation() {
    let registry = MonthlyAccumulatorRegistry::new();
    let key_id = ClientKeyId::generate();
    let scope_id = BudgetScopeId::ClientKey(key_id);

    // Timestamp: 2026-01-20 12:00:00 UTC
    let ts = utc_to_epoch_millis(2026, 1, 20, 720);
    let budget_cap = 5_000_000; // $5.00
    let estimate = 1_000_000; // $1.00

    // Spend $4.50
    let acc = registry.get_or_create(UtcYearMonth::from_epoch_millis(ts), scope_id.clone());
    acc.used_micro_usd.store(4_500_000, Ordering::SeqCst);

    // Next request with estimate $1.00 exceeds $5.00 ($4.50 + $1.00 = $5.50 > $5.00)
    let res = registry.try_reserve(scope_id, Some(budget_cap), estimate, ts);
    match res {
        Err(GateG7Error::ClientKeyBudgetExhausted {
            retry_after_secs,
            used_micro_usd,
            budget_micro_usd_month,
        }) => {
            assert_eq!(used_micro_usd, 4_500_000);
            assert_eq!(budget_micro_usd_month, 5_000_000);
            // Must have positive retry_after seconds until 2026-02-01 00:00:00 UTC
            assert!(
                retry_after_secs > 0,
                "retry_after_secs must be positive: {retry_after_secs}"
            );
            // From Jan 20 12:00 to Feb 1 00:00 is 11.5 days = ~993,600 seconds
            assert!((900_000..=1_100_000).contains(&retry_after_secs));
        }
        other => panic!("expected ClientKeyBudgetExhausted, got {other:?}"),
    }

    // Verify lowering a cap emits CacheInvalidationEvent::CapChanged
    assert_eq!(on_cap_changed(), CacheInvalidationEvent::CapChanged);
}

fn utc_to_epoch_millis(year: i32, month: u8, day: u8, minute_of_day: u16) -> i64 {
    crate::windows::utc_to_epoch_millis(year, month, day, minute_of_day)
}
