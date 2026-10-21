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
    let counter = Arc::new(AtomicBudgetCounter::new(
        stable_counter_id(&window.id),
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
