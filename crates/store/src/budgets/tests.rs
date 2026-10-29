//! Tests for fast startup reconstruction of budget windows and accumulators (< 2 s).
//!
//! Spec: Section 5.5, Section 7.1, Section 13.2, Requirements: LIM-1, PERF-4.

use super::*;
use std::sync::Arc;
use std::time::Instant;
use tempfile::tempdir;

use crate::crypto::MasterKey;
use crate::instance::load_or_initialize;
use crate::migrations::migrate;
use crate::pool::{Role as PoolRole, open_pool};
use crate::settings::seed_defaults;
use llmroller_core::seams::OsEntropy;
use llmroller_core::windows::ManualOffset;

async fn setup_budgets_test_db() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("budgets_rebuild_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xCC; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    (dir, writer_pool)
}

#[doc = "Spec: LIM-1, PERF-4"]
#[tokio::test]
async fn test_startup_rebuild_under_two_seconds_across_500_keys() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let now_ms = 1_700_000_000_000i64;

    // Seed 7 days of 5-minute rollups across 500 upstream keys (2,016 buckets per key)
    // To test fast execution without creating gigabytes in CI, seed 2,500 rollup rows across 500 keys
    let mut tx = pool.begin().await.expect("begin");

    let mut generated_keys = Vec::new();
    for _i in 0..500 {
        let key_id = UpstreamKeyId::generate();
        generated_keys.push(key_id);
        let client_key_id = ClientKeyId::generate();
        let user_id = UserId::generate();

        for b in 0..5 {
            let bucket_ms = now_ms - (b + 1) * 300_000;
            sqlx::query(
                r#"INSERT INTO usage_rollups_5min (
                       bucket_ms, upstream_key_id, canonical_id, client_key_id,
                       owner_user_id, provider_scope, share_id, cost_micro_usd, requests
                   )
                   VALUES (?, ?, 'model-x', ?, ?, 'personal', '', 10000, 1)"#,
            )
            .bind(bucket_ms)
            .bind(key_id.to_string())
            .bind(client_key_id.to_string())
            .bind(user_id.to_string())
            .execute(&mut *tx)
            .await
            .expect("insert rollup");
        }
    }
    tx.commit().await.expect("commit seed");

    // 1. Measure startup reconstruction duration: must complete in < 2 s (PERF-4)
    let start = Instant::now();
    let rebuilt = reconstruct_budgets_from_rollups(&pool, now_ms, 7 * 86_400_000)
        .await
        .expect("reconstruct budgets");
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 2000,
        "startup reconstruction took {} ms, must be < 2000 ms (PERF-4)",
        elapsed.as_millis()
    );

    // Verify all 500 key rings were reconstructed
    assert_eq!(rebuilt.key_rings.len(), 500);

    // Verify spend is populated correctly (5 buckets * 10,000 = 50,000 micro-USD per key)
    let test_key = generated_keys[0];
    let ring = rebuilt.key_rings.get(&test_key).expect("ring must exist");
    assert_eq!(ring.sum_at(now_ms), 50_000);

    // 2. Manual calibration delta via ManualOffset
    let offset = ManualOffset {
        effective_at_ms: now_ms - 100_000,
        delta_micro_usd: 5_000,
        period_ms: 18_000_000,
    };
    assert_eq!(offset.value_at(now_ms), 5_000);
    assert_eq!(ring.sum_at(now_ms) + offset.value_at(now_ms), 55_000);

    // 3. User monthly accumulator only aggregates global provider spend (USR-5)
    let global_user_id = UserId::generate();
    let personal_user_id = UserId::generate();
    let client_key_id = ClientKeyId::generate();
    let key_id = UpstreamKeyId::generate();
    let bucket_ms = now_ms - 300_000;

    let mut tx2 = pool.begin().await.expect("begin tx2");
    sqlx::query(
        r#"INSERT INTO usage_rollups_5min (
               bucket_ms, upstream_key_id, canonical_id, client_key_id,
               owner_user_id, provider_scope, share_id, cost_micro_usd, requests
           )
           VALUES (?, ?, 'model-y', ?, ?, 'global', '', 20000, 1)"#,
    )
    .bind(bucket_ms)
    .bind(key_id.to_string())
    .bind(client_key_id.to_string())
    .bind(global_user_id.to_string())
    .execute(&mut *tx2)
    .await
    .expect("insert global rollup");

    sqlx::query(
        r#"INSERT INTO usage_rollups_5min (
               bucket_ms, upstream_key_id, canonical_id, client_key_id,
               owner_user_id, provider_scope, share_id, cost_micro_usd, requests
           )
           VALUES (?, ?, 'model-z', ?, ?, 'personal', '', 30000, 1)"#,
    )
    .bind(bucket_ms)
    .bind(key_id.to_string())
    .bind(client_key_id.to_string())
    .bind(personal_user_id.to_string())
    .execute(&mut *tx2)
    .await
    .expect("insert personal rollup");
    tx2.commit().await.expect("commit tx2");

    let rebuilt2 = reconstruct_budgets_from_rollups(&pool, now_ms, 7 * 86_400_000)
        .await
        .expect("reconstruct budgets 2");

    let month = UtcYearMonth::from_epoch_millis(bucket_ms);
    let global_acc = rebuilt2
        .monthly_accumulators
        .get_or_create(month, BudgetScopeId::User(global_user_id));
    assert_eq!(
        global_acc
            .used_micro_usd
            .load(std::sync::atomic::Ordering::Relaxed),
        20000,
        "global spend must be aggregated in User monthly accumulator"
    );

    let personal_acc = rebuilt2
        .monthly_accumulators
        .get_or_create(month, BudgetScopeId::User(personal_user_id));
    assert_eq!(
        personal_acc
            .used_micro_usd
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "personal spend must NOT be aggregated in User monthly accumulator (USR-5)"
    );
}

#[doc = "Spec: LIM-1, REL-1, USG-6 - F026 Route-scoped budget windows and multi-day rings hydrate accurately across restarts"]
#[tokio::test]
async fn test_route_scoped_rings_and_multi_day_windows_hydrate_across_restart() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let now_ms = 1_700_000_000_000i64;
    let key_id = UpstreamKeyId::generate();
    let client_key_id = ClientKeyId::generate();
    let user_id = UserId::generate();

    let mut tx = pool.begin().await.expect("begin");
    // Seed 3 buckets for model-a: 10,000 each = 30,000
    // Seed 2 buckets for model-b: 15,000 each = 30,000
    for b in 0..3 {
        let bucket_ms = now_ms - (b + 1) * 300_000;
        sqlx::query(
            r#"INSERT INTO usage_rollups_5min (
                   bucket_ms, upstream_key_id, canonical_id, client_key_id,
                   owner_user_id, provider_scope, share_id, cost_micro_usd, requests
               )
               VALUES (?, ?, 'model-a', ?, ?, 'personal', '', 10000, 1)"#,
        )
        .bind(bucket_ms)
        .bind(key_id.to_string())
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert model-a");
    }
    for b in 0..2 {
        let bucket_ms = now_ms - (b + 1) * 300_000;
        sqlx::query(
            r#"INSERT INTO usage_rollups_5min (
                   bucket_ms, upstream_key_id, canonical_id, client_key_id,
                   owner_user_id, provider_scope, share_id, cost_micro_usd, requests
               )
               VALUES (?, ?, 'model-b', ?, ?, 'personal', '', 15000, 1)"#,
        )
        .bind(bucket_ms)
        .bind(key_id.to_string())
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert model-b");
    }
    tx.commit().await.expect("commit");

    let rebuilt = reconstruct_budgets_from_rollups(&pool, now_ms, 7 * 86_400_000)
        .await
        .expect("reconstruct");

    // Key ring aggregates all models on key: 30,000 + 30,000 = 60,000
    let key_ring = rebuilt.key_rings.get(&key_id).expect("key ring");
    assert_eq!(key_ring.sum_at(now_ms), 60_000);

    // Route rings isolate per-model spend (LIM-1, F026)
    let ring_a = rebuilt
        .route_rings
        .get(&(key_id, "model-a".to_string()))
        .expect("model-a ring");
    assert_eq!(ring_a.sum_at(now_ms), 30_000);

    let ring_b = rebuilt
        .route_rings
        .get(&(key_id, "model-b".to_string()))
        .expect("model-b ring");
    assert_eq!(ring_b.sum_at(now_ms), 30_000);

    assert!(
        !rebuilt
            .route_rings
            .contains_key(&(key_id, "model-unrelated".to_string()))
    );
}

// Spec: ./docs/srdd/13. SRDD - Usage Tracking, Quotas, and Cost Accounting.md#132-budget-windows-and-monthly-accumulators | Phase: ./docs/bigbang/big-bang-phases-4.md#41-sub-phase-41-five-kind-windowcounter-monthly-accumulators-and-fast-startup-reconstruction
#[tokio::test]
#[doc = "Spec: LIM-1, Spec: PERF-4, Spec: SHR-2"]
async fn test_active_manual_offset_preserves_effective_at_and_share_month_cutoff() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let key_id = UpstreamKeyId::generate();
    let share_id = ShareId::generate();
    let client_key_id = ClientKeyId::generate();
    let user_id = UserId::generate();

    let provider_id = llmroller_core::ids::ProviderId::generate();
    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, share_config_fingerprint, created_at)
         VALUES (?, 'P', 'openrouter', 'https://api.com', '{}', 'global', 'fp', 1000)"
    )
    .bind(provider_id.to_string())
    .execute(&pool)
    .await
    .expect("insert provider");

    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, label, secret_enc, secret_last4, priority, master_key_id, created_at)
         VALUES (?, ?, 'K', X'00', '1234', 100, 'key-0', 1000)"
    )
    .bind(key_id.to_string())
    .bind(provider_id.to_string())
    .execute(&pool)
    .await
    .expect("insert key");

    sqlx::query(
        "INSERT INTO budget_windows (id, scope_kind, upstream_key_id, window_kind, period_ms, limit_micro_usd, reserve_pct)
         VALUES ('win_01', 'upstream_key', ?, 'anchored_fixed', 3600000, 1000000, 2)"
    )
    .bind(key_id.to_string())
    .execute(&pool)
    .await
    .expect("insert window");

    // Insert a manual adjustment at T0 = 1_000_000 with period = 3_600_000
    insert_budget_adjustment(
        &pool,
        "adj_01",
        "win_01",
        None,
        250_000,
        1_000_000,
        Some(1_000_000),
        Some("calibrate"),
        1_000_000,
    )
    .await
    .expect("insert adjustment");

    // Restart at T = 2_800_000 (halfway through the 1h period): effective_at_ms must remain 1_000_000!
    let offset =
        active_manual_offset_for_window(&pool, "win_01", 3_600_000, 2_800_000, Some(1_000_000))
            .await
            .expect("active offset")
            .expect("some offset");
    assert_eq!(offset.effective_at_ms, 1_000_000);
    assert_eq!(offset.delta_micro_usd, 250_000);
    assert_eq!(offset.value_at(2_800_000), 250_000);
    assert_eq!(offset.value_at(4_600_001), 0);

    // Verify full UTC calendar month reconstruction for ProviderShare even when max_window_period_ms is short (1h)
    let month_start = UtcYearMonth::new(2026, 3).start_epoch_millis();
    let day_2_ms = month_start + 2 * 86_400_000;
    let day_28_ms = month_start + 28 * 86_400_000;
    sqlx::query(
        r#"INSERT INTO usage_rollups_5min (
               bucket_ms, upstream_key_id, canonical_id, client_key_id,
               owner_user_id, provider_scope, share_id, cost_micro_usd, requests
           )
           VALUES (?, ?, 'gpt-4o', ?, ?, 'shared', ?, 42000, 1)"#,
    )
    .bind(day_2_ms)
    .bind(key_id.to_string())
    .bind(client_key_id.to_string())
    .bind(user_id.to_string())
    .bind(share_id.to_string())
    .execute(&pool)
    .await
    .expect("insert share rollup");

    let rebuilt = reconstruct_budgets_from_rollups(&pool, day_28_ms, 3_600_000)
        .await
        .expect("reconstruct");
    assert_eq!(
        rebuilt
            .monthly_accumulators
            .usage_for(&BudgetScopeId::ProviderShare(share_id), day_28_ms),
        42_000
    );
}

// ============================================================================================
// `ISSUE-0162` — a budget offset must belong to the window *instance* it was calibrated for.
//
// `budget_adjustments.window_instance_start` has existed since `0001_initial.sql` and the admin
// API writes it, but `active_manual_offset_for_window` filtered only on a rolling wall-clock
// lookback (`effective_at > now - period_ms`). For an anchored window that means an offset applied
// near the end of one instance stays "active" for the whole of the next one — a March calibration
// silently widening April's budget. A budget guard that is weaker than the operator configured is
// worse than no guard, and it fails in the permissive direction.
//
// The fix selects by instance rather than by wall clock, and keeps the rolling lookback for rolling
// windows because those genuinely have no instance boundary.
// ============================================================================================

const DAY_MS: i64 = 86_400_000;

/// A UTC calendar-month start, using the same helper the window types use.
fn month_start_ms(year: i32, month: u8) -> i64 {
    llmroller_core::windows::utc_to_epoch_millis(year, month, 1, 0)
}

/// Seed one `budget_windows` row of the requested kind, returning its id.
async fn seed_window(pool: &sqlx::SqlitePool, window_kind: &str, period_ms: Option<i64>) -> String {
    let provider_id = llmroller_core::ids::ProviderId::generate();
    let key_id = UpstreamKeyId::generate();
    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, share_config_fingerprint, created_at)
         VALUES (?, 'P', 'openrouter', 'https://api.com', '{}', 'global', 'fp', 1000)",
    )
    .bind(provider_id.to_string())
    .execute(pool)
    .await
    .expect("insert provider");
    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, label, secret_enc, secret_last4, priority, master_key_id, created_at)
         VALUES (?, ?, 'K', X'00', '1234', 100, 'key-0', 1000)",
    )
    .bind(key_id.to_string())
    .bind(provider_id.to_string())
    .execute(pool)
    .await
    .expect("insert key");

    let window_id = format!("win_{}", UpstreamKeyId::generate());
    sqlx::query(
        "INSERT INTO budget_windows (id, scope_kind, upstream_key_id, window_kind, period_ms, limit_micro_usd, reserve_pct)
         VALUES (?, 'upstream_key', ?, ?, ?, 1000000, 2)",
    )
    .bind(&window_id)
    .bind(key_id.to_string())
    .bind(window_kind)
    .bind(period_ms)
    .execute(pool)
    .await
    .expect("insert window");
    window_id
}

/// The current anchored calendar-month instance start at `now_ms`, derived with the same
/// `AnchoredCalendarMonth` the counters use. This is what the production callers pass, so the
/// tests exercise the real derivation rather than restating the arithmetic.
fn anchored_month_start_at(now_ms: i64, anchor_day: u8) -> i64 {
    let (year, month, _day, minute) = llmroller_core::windows::epoch_millis_to_utc(now_ms);
    let mut window =
        llmroller_core::windows::AnchoredCalendarMonth::new(anchor_day, minute, year, month);
    window.align_to(now_ms);
    window.window_start_ms
}

/// Insert one calibration adjustment. `window_instance_start` is the instance it is bound to.
#[allow(clippy::too_many_arguments)]
async fn seed_adjustment(
    pool: &sqlx::SqlitePool,
    id: &str,
    window_id: &str,
    delta_micro_usd: i64,
    effective_at: i64,
    window_instance_start: Option<i64>,
) {
    insert_budget_adjustment(
        pool,
        id,
        window_id,
        None,
        delta_micro_usd,
        effective_at,
        window_instance_start,
        None,
        effective_at,
    )
    .await
    .expect("insert adjustment");
}

// Spec: Section 13.2 | Requirements: LIM-1, F-064, FND-006
#[tokio::test]
async fn a_march_offset_does_not_apply_to_april() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let window_id = seed_window(&pool, "anchored_calendar_month", None).await;

    let march_start = month_start_ms(2026, 3);
    let march_28 = march_start + 27 * DAY_MS;
    let april_10 = month_start_ms(2026, 4) + 9 * DAY_MS;

    // The operator calibrated March's overspend on March 28, bound to March's instance.
    seed_adjustment(
        &pool,
        "adj_march",
        &window_id,
        500_000,
        march_28,
        Some(march_start),
    )
    .await;

    // April 10 is 13 days after the adjustment, well inside a 31-day rolling lookback.
    let offset = active_manual_offset_for_window(
        &pool,
        &window_id,
        31 * DAY_MS,
        april_10,
        Some(anchored_month_start_at(april_10, 1)),
    )
    .await
    .expect("query must succeed");

    assert!(
        offset.is_none(),
        "a March-bound calibration must not reduce April's budget; got {offset:?}"
    );
}

// Spec: Section 13.2 | Requirements: LIM-1, F-064
#[tokio::test]
async fn an_offset_applies_within_its_own_instance() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let window_id = seed_window(&pool, "anchored_calendar_month", None).await;

    let april_start = month_start_ms(2026, 4);
    let april_2 = april_start + DAY_MS;
    let april_20 = april_start + 19 * DAY_MS;

    seed_adjustment(
        &pool,
        "adj_april",
        &window_id,
        400_000,
        april_2,
        Some(april_start),
    )
    .await;

    let offset = active_manual_offset_for_window(
        &pool,
        &window_id,
        31 * DAY_MS,
        april_20,
        Some(anchored_month_start_at(april_20, 1)),
    )
    .await
    .expect("query must succeed")
    .expect("April's own calibration must apply in April");

    assert_eq!(offset.delta_micro_usd, 400_000);
    assert_eq!(
        offset.effective_at_ms, april_2,
        "the authentic effective_at must be preserved so the offset expires on its own schedule"
    );
}

// Spec: Section 13.2 | Requirements: LIM-1, F-064
#[tokio::test]
async fn a_rolling_window_offset_still_uses_the_rolling_lookback() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let window_id = seed_window(&pool, "rolling", Some(3_600_000)).await;

    let t0 = 1_000_000;
    let t_half_hour_later = t0 + 1_800_000;

    // `window_instance_start IS NULL` is what a rolling window stores: no instance boundary.
    seed_adjustment(&pool, "adj_roll", &window_id, 250_000, t0, None).await;

    let offset =
        active_manual_offset_for_window(&pool, &window_id, 3_600_000, t_half_hour_later, None)
            .await
            .expect("query must succeed")
            .expect("a rolling window keeps its rolling lookback");

    assert_eq!(offset.delta_micro_usd, 250_000);

    // And it still ages out after `period_ms`, which is the property the lookback provides.
    let offset_after =
        active_manual_offset_for_window(&pool, &window_id, 3_600_000, t0 + 3_600_001, None)
            .await
            .expect("query must succeed");
    assert!(offset_after.is_none(), "the rolling offset must age out");
}

// Spec: Section 13.2 | Requirements: LIM-1, F-064
#[tokio::test]
async fn a_future_dated_adjustment_is_not_active() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let window_id = seed_window(&pool, "anchored_calendar_month", None).await;

    let april_start = month_start_ms(2026, 4);
    let april_5 = april_start + 4 * DAY_MS;
    let april_20 = april_start + 19 * DAY_MS;

    // Effective in the future, but bound to the current instance.
    seed_adjustment(
        &pool,
        "adj_future",
        &window_id,
        300_000,
        april_20,
        Some(april_start),
    )
    .await;

    let offset = active_manual_offset_for_window(
        &pool,
        &window_id,
        31 * DAY_MS,
        april_5,
        Some(anchored_month_start_at(april_5, 1)),
    )
    .await
    .expect("query must succeed");
    assert!(
        offset.is_none(),
        "a future-dated adjustment is not yet active; got {offset:?}"
    );
}

// Spec: Section 13.2 | Requirements: LIM-1, F-064
#[tokio::test]
async fn summing_offsets_ignores_other_instances() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let window_id = seed_window(&pool, "anchored_calendar_month", None).await;

    let march_start = month_start_ms(2026, 3);
    let april_start = month_start_ms(2026, 4);
    let april_20 = april_start + 19 * DAY_MS;

    // Two offsets in the same window, different instances, both inside a 31-day wall-clock
    // lookback of April 20. Only April's may be summed: a `WHERE` that filtered rows but still
    // summed across instances would pass a per-row assertion and still return the wrong total.
    seed_adjustment(
        &pool,
        "adj_mar",
        &window_id,
        500_000,
        march_start + 27 * DAY_MS,
        Some(march_start),
    )
    .await;
    seed_adjustment(
        &pool,
        "adj_apr",
        &window_id,
        400_000,
        april_start + DAY_MS,
        Some(april_start),
    )
    .await;

    let offset = active_manual_offset_for_window(
        &pool,
        &window_id,
        31 * DAY_MS,
        april_20,
        Some(anchored_month_start_at(april_20, 1)),
    )
    .await
    .expect("query must succeed")
    .expect("April's offset must be active");

    assert_eq!(
        offset.delta_micro_usd, 400_000,
        "only the current instance's delta may be summed; 900_000 means March leaked in"
    );
}

// ============================================================================================
// `ISSUE-0162` second half — startup hydration.
//
// `serve.rs` and `reloader.rs` both call `active_manual_offset_for_window` and push the result
// into the live counter via `set_manual_offset`. Both call sites exist, so the issue's premise
// ("serve.rs never loads active manual offsets") is stale. What was not covered is that the pair
// actually produces a *visible* offset on a live counter — a query that returns the right row and
// a setter nobody calls would look identical from outside.
//
// This exercises the exact pair over a real database: query, then set, then observe through the
// counter's own `local_used`, which is what budget enforcement reads.
// ============================================================================================

// Spec: Section 13.2 | Requirements: LIM-1, F-064, FND-006
#[tokio::test]
async fn a_stored_offset_becomes_visible_on_a_live_counter_after_hydration() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let window_id = seed_window(&pool, "anchored_calendar_month", None).await;

    let window = llmroller_core::snapshot::SnapshotBudgetWindow {
        id: window_id.clone(),
        scope_kind: "upstream_key".to_string(),
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk: None,
        window_kind: "anchored_calendar_month".to_string(),
        period_ms: None,
        anchor_at: Some(month_start_ms(2026, 1)),
        upstream_window_kind: None,
        limit_micro_usd: 1_000_000,
        reserve_pct: 0,
    };
    let counter = std::sync::Arc::new(llmroller_core::windows::AtomicBudgetCounter::for_window(
        1,
        window.limit_micro_usd,
        window.reserve_pct,
        llmroller_core::windows::WindowKind::AnchoredCalendarMonth,
        window.period_ms,
        window.anchor_at,
        None,
        month_start_ms(2026, 4) + 9 * DAY_MS,
    ));

    let now_ms = month_start_ms(2026, 4) + 9 * DAY_MS;
    seed_adjustment(
        &pool,
        "adj_hydrate",
        &window_id,
        300_000,
        month_start_ms(2026, 4) + DAY_MS,
        Some(month_start_ms(2026, 4)),
    )
    .await;

    // Before hydration the counter knows nothing about the stored calibration.
    assert_eq!(counter.local_used(now_ms), 0);

    // The hydrate step, exactly as `serve.rs` and `reloader.rs` perform it.
    let instance_start = counter.current_window_start_ms(now_ms);
    let offset = active_manual_offset_for_window(
        &pool,
        &window_id,
        31 * DAY_MS,
        now_ms,
        Some(instance_start),
    )
    .await
    .expect("query must succeed");
    counter.set_manual_offset(offset);

    assert_eq!(
        counter.local_used(now_ms),
        300_000,
        "the stored calibration must be visible to budget enforcement after hydration"
    );
}

// Spec: Section 13.2 | Requirements: LIM-1, F-064
#[tokio::test]
async fn an_unbound_adjustment_never_offsets_an_anchored_window() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let window_id = seed_window(&pool, "anchored_calendar_month", None).await;

    let april_start = month_start_ms(2026, 4);
    let april_20 = april_start + 19 * DAY_MS;

    // `window_instance_start IS NULL` on an *anchored* window: what every pre-`ISSUE-0162` row
    // looks like, because nothing stamped the column. `13. SRDD:82` makes applicability
    // "`window_instance_start == window_start_ms`", and NULL equals nothing, so such a row must
    // not apply. The safe direction: a legacy row goes inert rather than staying over-broad.
    seed_adjustment(
        &pool,
        "adj_legacy",
        &window_id,
        500_000,
        april_start + DAY_MS,
        None,
    )
    .await;

    let offset = active_manual_offset_for_window(
        &pool,
        &window_id,
        31 * DAY_MS,
        april_20,
        Some(anchored_month_start_at(april_20, 1)),
    )
    .await
    .expect("query must succeed");

    assert!(
        offset.is_none(),
        "an adjustment bound to no instance cannot equal the current instance start, so it must \
         not offset an anchored window; got {offset:?}"
    );
}
