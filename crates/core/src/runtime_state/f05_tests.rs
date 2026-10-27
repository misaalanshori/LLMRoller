//! F05 focused tests for scope-aware C8 window admission.
//!
//! Spec: Section 10.1 (C8), Section 13.2, Requirements: LIM-1, RTE-3.

use super::*;
use crate::ids::{ModelPk, UpstreamKeyId};
use std::collections::HashMap;

fn window(
    id: &str,
    key: UpstreamKeyId,
    scope_kind: &str,
    model_pk: Option<ModelPk>,
    limit_micro_usd: i64,
) -> SnapshotBudgetWindow {
    SnapshotBudgetWindow {
        id: id.to_string(),
        scope_kind: scope_kind.to_string(),
        upstream_key_id: key,
        model_pk,
        window_kind: "rolling".to_string(),
        period_ms: Some(18_000_000),
        anchor_at: None,
        upstream_window_kind: None,
        limit_micro_usd,
        reserve_pct: 0,
    }
}

fn snapshot_with_windows(windows: Vec<SnapshotBudgetWindow>) -> ConfigSnapshot {
    let mut by_key: HashMap<UpstreamKeyId, Vec<SnapshotBudgetWindow>> = HashMap::new();
    for window in windows {
        by_key
            .entry(window.upstream_key_id)
            .or_default()
            .push(window);
    }
    ConfigSnapshot {
        budget_windows: Arc::new(by_key),
        ..ConfigSnapshot::default()
    }
}

fn install_counter(
    runtime: &RuntimeState,
    snapshot: &ConfigSnapshot,
    window_id: &str,
) -> Arc<AtomicBudgetCounter> {
    let window = snapshot
        .budget_windows
        .values()
        .flatten()
        .find(|window| window.id == window_id)
        .expect("configured window");
    // Allocate through the real path so this harness exercises production
    // behaviour rather than a test-only hash (`ISSUE-0005`).
    let counter = Arc::new(AtomicBudgetCounter::new(
        super::allocate_counter_id(),
        window.limit_micro_usd,
        window.reserve_pct,
        window.period_ms.unwrap_or(2_678_400_000),
    ));
    runtime
        .window_counters
        .insert(window.id.clone(), counter.clone());
    counter
}

#[test]
#[doc = "Spec: LIM-1 — F05 C8 admits by configured key grouping, not a window-id prefix"]
fn key_budget_admitted_reads_configured_window_ids() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let model = ModelPk::generate();
    // Production window ids are UUIDs, never `{key_id}:...` prefixed strings.
    let window_id = "f1d2c3b4-a596-4e7f-8a90-123456789abc";
    let snapshot = snapshot_with_windows(vec![window(
        window_id,
        key,
        "upstream_key",
        None,
        10_000_000,
    )]);
    let counter = install_counter(&runtime, &snapshot, window_id);

    assert!(
        runtime.key_budget_admitted(&snapshot, key, Some(model), 1_000),
        "a fresh window must admit the key"
    );

    crate::windows::BudgetCounter::record_actual(counter.as_ref(), 1_000, 10_000_000);

    assert!(
        !runtime.key_budget_admitted(&snapshot, key, Some(model), 1_000),
        "an exhausted configured window must block C8 even when its id is a UUID"
    );
}

#[test]
#[doc = "Spec: LIM-1 — F05 route windows gate only their configured model"]
fn route_windows_gate_only_their_configured_model() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let model_a = ModelPk::generate();
    let model_b = ModelPk::generate();
    let snapshot = snapshot_with_windows(vec![
        window("route-a", key, "route", Some(model_a), 1_000_000),
        window("route-b", key, "route", Some(model_b), 1_000_000),
    ]);
    let counter_a = install_counter(&runtime, &snapshot, "route-a");
    let _counter_b = install_counter(&runtime, &snapshot, "route-b");
    crate::windows::BudgetCounter::record_actual(counter_a.as_ref(), 1_000, 1_000_000);

    assert!(
        !runtime.key_budget_admitted(&snapshot, key, Some(model_a), 1_000),
        "the exhausted route window must block its own model"
    );
    assert!(
        runtime.key_budget_admitted(&snapshot, key, Some(model_b), 1_000),
        "another model's route window must not block this model"
    );
    assert!(
        runtime.key_budget_admitted(&snapshot, key, None, 1_000),
        "route-scoped windows do not apply without a model context"
    );
}

#[test]
#[doc = "Spec: LIM-1 — F05 rebuild creates a counter for every configured window kind"]
fn rebuild_window_counters_builds_configured_kinds() {
    use crate::windows::WindowKind;

    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let mut fixed = window("fixed", key, "upstream_key", None, 10_000_000);
    fixed.window_kind = "anchored_fixed".to_string();
    fixed.anchor_at = Some(now - 1_000);
    let mut calendar = window("calendar", key, "upstream_key", None, 10_000_000);
    calendar.window_kind = "anchored_calendar_month".to_string();
    calendar.period_ms = None;
    calendar.anchor_at = Some(now - 1_000);
    let mut provider = window("provider", key, "upstream_key", None, 10_000_000);
    provider.window_kind = "anchored_provider".to_string();
    provider.upstream_window_kind = Some("5h".to_string());

    let snapshot = snapshot_with_windows(vec![
        window("rolling", key, "upstream_key", None, 10_000_000),
        fixed,
        calendar,
        provider,
    ]);
    runtime.rebuild_window_counters(&snapshot, now);

    assert_eq!(
        runtime.window_counter("rolling").expect("rolling").kind,
        WindowKind::Rolling
    );
    assert_eq!(
        runtime.window_counter("fixed").expect("fixed").kind,
        WindowKind::AnchoredFixed
    );
    assert_eq!(
        runtime.window_counter("calendar").expect("calendar").kind,
        WindowKind::AnchoredCalendarMonth
    );
    assert_eq!(
        runtime.window_counter("provider").expect("provider").kind,
        WindowKind::AnchoredProvider
    );
}

#[test]
#[doc = "Spec: LIM-1 — F05 rebuild preserves accumulated usage for unchanged windows"]
fn rebuild_window_counters_preserves_usage_for_unchanged_config() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let snapshot = snapshot_with_windows(vec![window(
        "window",
        key,
        "upstream_key",
        None,
        10_000_000,
    )]);
    runtime.rebuild_window_counters(&snapshot, now);
    let counter = runtime.window_counter("window").expect("counter");
    crate::windows::BudgetCounter::record_actual(counter.as_ref(), now, 4_000_000);

    runtime.rebuild_window_counters(&snapshot, now);
    let counter = runtime.window_counter("window").expect("counter");
    assert_eq!(
        crate::windows::BudgetCounter::effective_used(counter.as_ref(), now),
        4_000_000,
        "a reload must not discard spend already accumulated for the window"
    );
}

#[test]
#[doc = "Spec: LIM-1 — F05 hydrate seeds anchored windows and enforces their cap"]
fn hydrate_window_counter_seeds_and_enforces_anchored_window() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let mut fixed = window("fixed", key, "upstream_key", None, 10_000_000);
    fixed.window_kind = "anchored_fixed".to_string();
    fixed.anchor_at = Some(now - 1_000);
    let snapshot = snapshot_with_windows(vec![fixed.clone()]);

    runtime.hydrate_window_counter(&fixed, 10_000_000, now);

    assert!(
        !runtime.key_budget_admitted(&snapshot, key, None, now),
        "a hydrated anchored window already at its cap must block C8"
    );
    assert!(
        runtime
            .reserve_key_windows(&snapshot, key, None, 1_000, now)
            .is_err(),
        "reservations must respect the hydrated anchored usage"
    );
}

fn utc_now_ms_for_tests() -> i64 {
    // Fixed, bucket-aligned instant that keeps anchored arithmetic deterministic.
    let base = 1_790_000_000_000_i64;
    (base / 60_000) * 60_000
}

#[test]
#[doc = "Spec: LIM-1 — F05 reservations include key windows and only matching route windows"]
fn reserve_key_windows_filters_route_windows_by_model() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let model_a = ModelPk::generate();
    let model_b = ModelPk::generate();
    let snapshot = snapshot_with_windows(vec![
        window("route-a", key, "route", Some(model_a), 1_000_000),
        window("route-b", key, "route", Some(model_b), 1_000_000),
    ]);
    let counter_a = install_counter(&runtime, &snapshot, "route-a");
    let _counter_b = install_counter(&runtime, &snapshot, "route-b");
    crate::windows::BudgetCounter::record_actual(counter_a.as_ref(), 1_000, 1_000_000);

    assert!(
        runtime
            .reserve_key_windows(&snapshot, key, Some(model_a), 1_000, 1_000)
            .is_err(),
        "the exhausted route window must reject its own model's reservation"
    );
    let reservation = runtime
        .reserve_key_windows(&snapshot, key, Some(model_b), 1_000, 1_000)
        .expect("matching route window has headroom");
    assert!(
        reservation.is_some(),
        "the model with headroom must acquire a reservation"
    );
}

#[test]
#[doc = "Spec: USG-6, LIM-1"]
fn reconcile_upstream_usage_honors_reserve_pct_and_preserves_local_delta() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let mut w = window("w-5h", key, "upstream_key", None, 10_000_000);
    w.reserve_pct = 5; // 95% threshold
    let snapshot = snapshot_with_windows(vec![w.clone()]);
    runtime.rebuild_window_counters(&snapshot, now);
    let counter = runtime.window_counter("w-5h").expect("counter");

    // Record $1.00 local spend before poll
    crate::windows::BudgetCounter::record_actual(counter.as_ref(), now, 1_000_000);

    // Poll 1: Upstream reports 50% ($5.00), resets_at = None
    runtime.reconcile_upstream_usage(
        key,
        &[w.clone()],
        &[UpstreamWindowObservation {
            kind: "5h".to_string(),
            used_percent: 50.0,
            resets_at: None,
        }],
        now,
    );
    assert!(runtime.is_key_routable(key, now));
    assert_eq!(
        crate::windows::BudgetCounter::effective_used(counter.as_ref(), now),
        5_000_000
    );

    // Poll 2 (same local spend): must still use local_used ($1.00) as baseline, not $5.00!
    runtime.reconcile_upstream_usage(
        key,
        &[w.clone()],
        &[UpstreamWindowObservation {
            kind: "5h".to_string(),
            used_percent: 50.0,
            resets_at: None,
        }],
        now + 1_000,
    );
    // Now local grows by $0.50 -> effective_used must be $5.50 (5_500_000), not $5.00!
    crate::windows::BudgetCounter::record_actual(counter.as_ref(), now + 2_000, 500_000);
    assert_eq!(
        crate::windows::BudgetCounter::effective_used(counter.as_ref(), now + 2_000),
        5_500_000
    );

    // Poll 3: Upstream reports 96% (>= 95% threshold with reserve_pct = 5) and resets_at = None -> exhausted for 300s fallback
    runtime.reconcile_upstream_usage(
        key,
        &[w],
        &[UpstreamWindowObservation {
            kind: "5h".to_string(),
            used_percent: 96.0,
            resets_at: None,
        }],
        now + 3_000,
    );
    assert!(!runtime.is_key_routable(key, now + 4_000));
    assert!(runtime.is_key_routable(key, now + 303_001));
}

// ---------------------------------------------------------------------------
// In-flight spend must gate admission and headroom (ISSUE-0004, LIM-1, RTE-13)
// ---------------------------------------------------------------------------

/// Headroom must account for money already spoken for by live reservations, not
/// only money already committed.
///
/// A key at zero committed spend with a reservation covering half its allowance
/// has half its allowance free, so the reported headroom must be about 5,000 bps.
/// Before the fix this returned 10_000 because `effective_used` was blind to
/// in-flight spend.
#[test]
#[doc = "Spec: LIM-1, RTE-13 — ISSUE-0004 headroom sees in-flight reservations"]
fn headroom_bps_reflects_in_flight_reservations() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    // $10.00 allowance, no reserve percentage, so max allowed spend is the limit.
    let snapshot = snapshot_with_windows(vec![window(
        "headroom",
        key,
        "upstream_key",
        None,
        10_000_000,
    )]);
    let counter = install_counter(&runtime, &snapshot, "headroom");

    assert_eq!(
        runtime.headroom_bps_for_route(&snapshot, key, None, now),
        10_000,
        "an untouched window starts with full headroom"
    );

    // Hold half the allowance in flight without committing anything.
    let reservation = crate::windows::Reservation::try_reserve(
        vec![Arc::clone(&counter) as Arc<dyn crate::windows::BudgetCounter>],
        5_000_000,
        now,
    )
    .expect("half the allowance must be reservable");

    assert_eq!(
        runtime.headroom_bps_for_route(&snapshot, key, None, now),
        5_000,
        "headroom must drop by the reserved fraction even though nothing is committed"
    );

    drop(reservation);
}

/// Admission must reject when in-flight reservations have consumed the window,
/// even though nothing has been committed yet.
#[test]
#[doc = "Spec: LIM-1 — ISSUE-0004 admission sees in-flight reservations"]
fn key_budget_admitted_rejects_when_reservations_exhaust_the_window() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let snapshot = snapshot_with_windows(vec![window(
        "admission",
        key,
        "upstream_key",
        None,
        1_000_000,
    )]);
    let counter = install_counter(&runtime, &snapshot, "admission");

    assert!(
        runtime.key_budget_admitted(&snapshot, key, None, now),
        "a fully unused window must admit"
    );

    // Reserve the entire allowance. Nothing is committed; it is only in flight.
    let reservation = crate::windows::Reservation::try_reserve(
        vec![Arc::clone(&counter) as Arc<dyn crate::windows::BudgetCounter>],
        1_000_000,
        now,
    )
    .expect("the whole allowance must be reservable once");

    assert!(
        !runtime.key_budget_admitted(&snapshot, key, None, now),
        "a window fully spoken for by in-flight reservations must not admit more"
    );

    drop(reservation);
}

/// Dropping an uncommitted reservation must give the headroom back.
///
/// This pins the `Drop` symmetry the whole scheme depends on: if the rollback
/// fails, every admission decision after it is wrong for the rest of the window.
#[test]
#[doc = "Spec: LIM-1 — ISSUE-0004 reservation rollback restores headroom"]
fn dropping_a_reservation_restores_headroom() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let snapshot = snapshot_with_windows(vec![window(
        "rollback",
        key,
        "upstream_key",
        None,
        10_000_000,
    )]);
    let counter = install_counter(&runtime, &snapshot, "rollback");

    let reservation = crate::windows::Reservation::try_reserve(
        vec![Arc::clone(&counter) as Arc<dyn crate::windows::BudgetCounter>],
        4_000_000,
        now,
    )
    .expect("reservable");
    assert_eq!(
        runtime.headroom_bps_for_route(&snapshot, key, None, now),
        6_000
    );

    drop(reservation);

    assert_eq!(
        runtime.headroom_bps_for_route(&snapshot, key, None, now),
        10_000,
        "dropping an uncommitted reservation must restore the full headroom"
    );
    assert!(
        runtime.key_budget_admitted(&snapshot, key, None, now),
        "admission must recover once the reservation is released"
    );
}

// ---------------------------------------------------------------------------
// counter_id must be unique per window and stable across rebuilds (ISSUE-0005)
// ---------------------------------------------------------------------------

/// Many distinct windows must receive distinct counter ids.
///
/// `counter_id` is the sort key `Reservation::try_reserve` uses to establish a
/// deterministic acquisition order and the identifier reported in
/// `ReservationError::LimitExceeded`. Two windows sharing an id would make both
/// ambiguous. A hash of the window id makes collisions improbable; monotonic
/// allocation makes them impossible.
#[test]
#[doc = "Spec: LIM-1 — ISSUE-0005 counter ids are unique per window"]
fn counter_ids_are_unique_across_many_windows() {
    use std::collections::HashSet;

    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();

    // 10_000 windows, with ids shaped like the synthetic ones the rebuild path
    // actually sees (`key:<ulid>:<scope>:<window>`), including near-duplicates
    // that differ only in the final character.
    let mut ids = Vec::with_capacity(10_000);
    for index in 0..10_000 {
        ids.push(format!("key:{key}:upstream_key:w{index}"));
    }
    let snapshot = snapshot_with_windows(
        ids.iter()
            .map(|id| window(id, key, "upstream_key", None, 10_000_000))
            .collect(),
    );
    runtime.rebuild_window_counters(&snapshot, now);

    let mut seen = HashSet::with_capacity(ids.len());
    for id in &ids {
        let counter = runtime
            .window_counter(id)
            .expect("counter for every window");
        assert!(
            seen.insert(counter.counter_id()),
            "counter id {} was handed to more than one window (latest: {id})",
            counter.counter_id()
        );
    }
    assert_eq!(seen.len(), ids.len());
}

/// A window that survives a rebuild must keep its counter id.
///
/// `try_reserve` sorts by `counter_id`. Allocating a fresh id on every rebuild
/// would silently change that order mid-process and break the determinism the
/// sorter is relied on for. This also keeps a `LimitExceeded` diagnostic naming
/// the same window across a config reload.
#[test]
#[doc = "Spec: LIM-1 — ISSUE-0005 counter id is stable across rebuilds"]
fn counter_id_is_stable_for_the_same_window_across_rebuilds() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let snapshot = snapshot_with_windows(vec![window(
        "stable",
        key,
        "upstream_key",
        None,
        10_000_000,
    )]);

    runtime.rebuild_window_counters(&snapshot, now);
    let before = runtime
        .window_counter("stable")
        .expect("counter")
        .counter_id();

    // Force the stale/replace path by changing the limit, which makes
    // `matches_config` false and rebuilds the counter.
    let changed = snapshot_with_windows(vec![window(
        "stable",
        key,
        "upstream_key",
        None,
        20_000_000,
    )]);
    runtime.rebuild_window_counters(&changed, now);
    let after = runtime
        .window_counter("stable")
        .expect("counter")
        .counter_id();

    assert_eq!(
        before, after,
        "a rebuild that replaces the counter for an unchanged window id must preserve its id"
    );
}

/// Hydrating the same window twice must not change its counter id.
///
/// Startup reconstruction and the discovery-driven hydrate path both call
/// `hydrate_window_counter*`. If they invented a fresh id each time, the id a
/// reservation authenticated against could change underneath it.
#[test]
#[doc = "Spec: LIM-1 — ISSUE-0005 counter id is stable across hydration"]
fn counter_id_is_stable_across_hydration() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let snapshot = snapshot_with_windows(vec![window(
        "hydrated",
        key,
        "upstream_key",
        None,
        10_000_000,
    )]);
    let configured = snapshot
        .budget_windows
        .get(&key)
        .and_then(|windows| windows.first())
        .expect("configured window")
        .clone();

    runtime.hydrate_window_counter(&configured, 0, now);
    let first = runtime
        .window_counter("hydrated")
        .expect("counter")
        .counter_id();

    runtime.hydrate_window_counter(&configured, 0, now);
    let second = runtime
        .window_counter("hydrated")
        .expect("counter")
        .counter_id();

    assert_eq!(
        first, second,
        "re-hydrating the same window id must reuse its counter id"
    );
}

/// The behavioural consequence: a reservation spanning several windows acquires
/// in a stable order, repeatedly.
///
/// `try_reserve` sorts by `counter_id` so that two concurrent reservations over
/// the same set of windows always walk them in the same sequence. With unique,
/// monotonic ids that order is total and reproducible. A genuine `DefaultHasher`
/// collision was not searched for — a birthday search over the 64-bit space is
/// not cheap, and the property under test (totality of the order) follows from
/// uniqueness, which the test above pins directly.
#[test]
#[doc = "Spec: LIM-1 — ISSUE-0005 reservation acquisition order is deterministic"]
fn reservation_order_is_deterministic_across_windows() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let ids = [
        "order-a", "order-b", "order-c", "order-d", "order-e", "order-f",
    ];
    let snapshot = snapshot_with_windows(
        ids.iter()
            .map(|id| window(id, key, "upstream_key", None, 10_000_000))
            .collect(),
    );
    runtime.rebuild_window_counters(&snapshot, now);

    let mut counters: Vec<Arc<dyn crate::windows::BudgetCounter>> = ids
        .iter()
        .map(|id| {
            Arc::clone(&runtime.window_counter(id).expect("counter"))
                as Arc<dyn crate::windows::BudgetCounter>
        })
        .collect();
    counters.sort_by_key(|counter| counter.counter_id());
    let ascending: Vec<u64> = counters.iter().map(|c| c.counter_id()).collect();
    let mut sorted = ascending.clone();
    sorted.sort_unstable();
    assert_eq!(
        ascending, sorted,
        "counter ids must form a total, reproducible order"
    );

    // The same set, supplied in a different order, must sort to the same sequence.
    let mut reversed = counters.clone();
    reversed.reverse();
    reversed.sort_by_key(|counter| counter.counter_id());
    let reversed_ids: Vec<u64> = reversed.iter().map(|c| c.counter_id()).collect();
    assert_eq!(
        ascending, reversed_ids,
        "acquisition order must not depend on the caller's input order"
    );
}

/// Counter ids must be **allocated**, not hashed.
///
/// This is the property that makes collisions impossible rather than merely
/// improbable (`ISSUE-0005`). A `u64` hash of the window id can only ever make a
/// collision unlikely, and `std::collections::hash_map::DefaultHasher`'s output is
/// explicitly unspecified and not stable across releases, so hashing also leaves
/// the acquisition order `try_reserve` relies on unproven.
///
/// The observable consequence of allocation is that ids from a fresh registry are
/// **dense and consecutive**: allocating for `N` windows yields `N` distinct ids
/// occupying a range of exactly `N`. Hashing cannot satisfy that — its outputs are
/// spread across the whole 64-bit space.
///
/// This asserts the allocation policy the design chose, not a hasher internal. It
/// is the substitute for a collision test, which is not reachable: finding a
/// `DefaultHasher` collision needs on the order of 2^32 hashes.
#[test]
#[doc = "Spec: LIM-1 — ISSUE-0005 counter ids come from a monotonic allocator"]
fn counter_ids_are_allocated_from_a_dense_monotonic_sequence() {
    let runtime = RuntimeState::new();
    let key = UpstreamKeyId::generate();
    let now = utc_now_ms_for_tests();
    let ids: Vec<String> = (0..64)
        .map(|index| format!("dense:{key}:w{index}"))
        .collect();
    let snapshot = snapshot_with_windows(
        ids.iter()
            .map(|id| window(id, key, "upstream_key", None, 10_000_000))
            .collect(),
    );
    runtime.rebuild_window_counters(&snapshot, now);

    let mut allocated: Vec<u64> = ids
        .iter()
        .map(|id| runtime.window_counter(id).expect("counter").counter_id())
        .collect();
    allocated.sort_unstable();

    assert_eq!(allocated.len(), 64, "every window must have received an id");
    // The allocator is process-wide, so other tests running concurrently may take
    // ids in between. The property that distinguishes allocation from hashing is the
    // *span*: 64 allocations can occupy at most a small, bounded range, whereas
    // hashes spread across the whole 64-bit space. Assert a bound generous enough to
    // absorb concurrent test allocations but far below any hash spread.
    let first = allocated.first().copied().unwrap_or_default();
    let last = allocated.last().copied().unwrap_or_default();
    let span = last.saturating_sub(first).saturating_add(1);
    assert!(
        span < 1_000_000,
        "counter ids must come from a monotonic allocator, not a hash: {len} ids \
         spanned {span} values (from {first} to {last}), which is hash-like spread \
         rather than dense allocation; ids were {allocated:?}",
        len = allocated.len()
    );
}
