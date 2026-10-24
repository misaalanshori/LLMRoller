//! Fast startup reconstruction of budget windows and monthly accumulators (< 2 s).
//!
//! Spec: Section 5.5, Section 7.1, Section 13.2, Requirements: LIM-1, PERF-4.

use sqlx::{Row as _, SqlitePool};
use std::collections::HashMap;

use futures_util::TryStreamExt as _;
use llmroller_core::accumulators::{BudgetScopeId, MonthlyAccumulatorRegistry, UtcYearMonth};
use llmroller_core::ids::{ClientKeyId, ShareId, UpstreamKeyId, UserId};
use llmroller_core::windows::RollingRing;

/// In-memory structures rebuilt during startup reconstruction.
pub struct RebuiltState {
    pub key_rings: HashMap<UpstreamKeyId, RollingRing>,
    pub route_rings: HashMap<(UpstreamKeyId, String), RollingRing>,
    pub monthly_accumulators: MonthlyAccumulatorRegistry,
}

impl std::fmt::Debug for RebuiltState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RebuiltState")
            .field("key_rings_len", &self.key_rings.len())
            .field("route_rings_len", &self.route_rings.len())
            .field("monthly_accumulators_len", &self.monthly_accumulators.len())
            .finish()
    }
}

/// Fast startup reconstruction of windows and accumulators from usage_rollups_5min (PERF-4).
///
/// Never scans usage_events; processes only the non-dropping usage_rollups_5min table.
pub async fn reconstruct_budgets_from_rollups(
    pool: &SqlitePool,
    now_ms: i64,
    max_window_period_ms: i64,
) -> Result<RebuiltState, sqlx::Error> {
    let cutoff_ms = now_ms - max_window_period_ms - 300_000;

    let bucket_width_ms = RollingRing::new(max_window_period_ms).bucket_width_ms;
    let mut key_rings: HashMap<UpstreamKeyId, RollingRing> = HashMap::new();

    // Aggregate source 5-minute rows into the ring's native bucket width in
    // SQLite. This keeps startup work proportional to distinct rings, not to
    // every raw rollup row, and mirrors RollingRing::record's positive-only
    // accounting exactly.
    let mut ring_rows = sqlx::query(
        r#"SELECT upstream_key_id,
                  ((bucket_ms + 150000) / ?) * ? AS ring_bucket_ms,
                  SUM(CASE WHEN cost_micro_usd > 0 THEN cost_micro_usd ELSE 0 END)
           FROM usage_rollups_5min
           WHERE bucket_ms >= ? AND upstream_key_id <> ''
           GROUP BY upstream_key_id, ring_bucket_ms
           ORDER BY ring_bucket_ms"#,
    )
    .bind(bucket_width_ms)
    .bind(bucket_width_ms)
    .bind(cutoff_ms)
    .fetch(pool);

    while let Some(r) = ring_rows.try_next().await? {
        let upstream_key_id_str: &str = r.try_get(0)?;
        let ring_bucket_ms: i64 = r.try_get(1)?;
        let cost_micro_usd: i64 = r.try_get(2)?;
        if let Ok(key_id) = UpstreamKeyId::parse(upstream_key_id_str) {
            let ring = key_rings
                .entry(key_id)
                .or_insert_with(|| RollingRing::new(max_window_period_ms));
            ring.record(ring_bucket_ms, cost_micro_usd);
        }
    }

    // Reconstruct route-scoped rings (upstream_key_id, canonical_id) for route windows (LIM-1, F026)
    let mut route_ring_rows = sqlx::query(
        r#"SELECT upstream_key_id, canonical_id,
                  ((bucket_ms + 150000) / ?) * ? AS ring_bucket_ms,
                  SUM(CASE WHEN cost_micro_usd > 0 THEN cost_micro_usd ELSE 0 END)
           FROM usage_rollups_5min
           WHERE bucket_ms >= ? AND upstream_key_id <> '' AND canonical_id <> ''
           GROUP BY upstream_key_id, canonical_id, ring_bucket_ms
           ORDER BY ring_bucket_ms"#,
    )
    .bind(bucket_width_ms)
    .bind(bucket_width_ms)
    .bind(cutoff_ms)
    .fetch(pool);

    let mut route_rings: HashMap<(UpstreamKeyId, String), RollingRing> = HashMap::new();
    while let Some(r) = route_ring_rows.try_next().await? {
        let upstream_key_id_str: &str = r.try_get(0)?;
        let canonical_id: String = r.try_get(1)?;
        let ring_bucket_ms: i64 = r.try_get(2)?;
        let cost_micro_usd: i64 = r.try_get(3)?;
        if let Ok(key_id) = UpstreamKeyId::parse(upstream_key_id_str) {
            let ring = route_rings
                .entry((key_id, canonical_id))
                .or_insert_with(|| RollingRing::new(max_window_period_ms));
            ring.record(ring_bucket_ms, cost_micro_usd);
        }
    }

    let monthly_accumulators = MonthlyAccumulatorRegistry::new();

    // Calendar-month totals are also aggregated in SQLite. Grouping by UTC
    // epoch day keeps SQLite's per-row work small; each day maps to exactly one
    // calendar month via UtcYearMonth::from_epoch_millis.
    let mut client_rows = sqlx::query(
        r#"SELECT client_key_id, bucket_ms / 86400000 AS epoch_day, SUM(cost_micro_usd)
           FROM usage_rollups_5min
           WHERE bucket_ms >= ?
           GROUP BY client_key_id, epoch_day"#,
    )
    .bind(cutoff_ms)
    .fetch(pool);
    while let Some(r) = client_rows.try_next().await? {
        let client_key_id_str: &str = r.try_get(0)?;
        let epoch_day: i64 = r.try_get(1)?;
        let cost_micro_usd: i64 = r.try_get(2)?;
        let Ok(client_id) = ClientKeyId::parse(client_key_id_str) else {
            continue;
        };
        let month = UtcYearMonth::from_epoch_millis(epoch_day.saturating_mul(86_400_000));
        let acc = monthly_accumulators.get_or_create(month, BudgetScopeId::ClientKey(client_id));
        acc.used_micro_usd
            .fetch_add(cost_micro_usd, std::sync::atomic::Ordering::Relaxed);
    }

    // Only global provider spend counts toward user monthly budget (USR-5).
    let mut user_rows = sqlx::query(
        r#"SELECT owner_user_id, bucket_ms / 86400000 AS epoch_day, SUM(cost_micro_usd)
           FROM usage_rollups_5min
           WHERE bucket_ms >= ? AND provider_scope = 'global'
           GROUP BY owner_user_id, epoch_day"#,
    )
    .bind(cutoff_ms)
    .fetch(pool);
    while let Some(r) = user_rows.try_next().await? {
        let owner_user_id_str: &str = r.try_get(0)?;
        let epoch_day: i64 = r.try_get(1)?;
        let cost_micro_usd: i64 = r.try_get(2)?;
        let Ok(user_id) = UserId::parse(owner_user_id_str) else {
            continue;
        };
        let month = UtcYearMonth::from_epoch_millis(epoch_day.saturating_mul(86_400_000));
        let acc = monthly_accumulators.get_or_create(month, BudgetScopeId::User(user_id));
        acc.used_micro_usd
            .fetch_add(cost_micro_usd, std::sync::atomic::Ordering::Relaxed);
    }

    let mut share_rows = sqlx::query(
        r#"SELECT share_id, bucket_ms / 86400000 AS epoch_day, SUM(cost_micro_usd)
           FROM usage_rollups_5min
           WHERE bucket_ms >= ? AND share_id <> ''
           GROUP BY share_id, epoch_day"#,
    )
    .bind(cutoff_ms)
    .fetch(pool);
    while let Some(r) = share_rows.try_next().await? {
        let share_id_str: &str = r.try_get(0)?;
        let epoch_day: i64 = r.try_get(1)?;
        let cost_micro_usd: i64 = r.try_get(2)?;
        let Ok(share_id) = ShareId::parse(share_id_str) else {
            continue;
        };
        let month = UtcYearMonth::from_epoch_millis(epoch_day.saturating_mul(86_400_000));
        let acc = monthly_accumulators.get_or_create(month, BudgetScopeId::ProviderShare(share_id));
        acc.used_micro_usd
            .fetch_add(cost_micro_usd, std::sync::atomic::Ordering::Relaxed);
    }

    Ok(RebuiltState {
        key_rings,
        route_rings,
        monthly_accumulators,
    })
}

#[cfg(test)]
pub mod tests;
/// Loads configured budget windows grouped by upstream key for the hot-path snapshot.
pub async fn load_budget_windows_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<HashMap<UpstreamKeyId, Vec<llmroller_core::snapshot::SnapshotBudgetWindow>>, sqlx::Error>
{
    let rows = sqlx::query(
        "SELECT id, scope_kind, upstream_key_id, model_pk, window_kind, period_ms,
                anchor_at, upstream_window_kind, limit_micro_usd, reserve_pct
         FROM budget_windows",
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut out: HashMap<UpstreamKeyId, Vec<llmroller_core::snapshot::SnapshotBudgetWindow>> =
        HashMap::new();
    for row in rows {
        let key_str: String = row.try_get("upstream_key_id")?;
        let Ok(key_id) = UpstreamKeyId::parse(&key_str) else {
            continue;
        };
        let model_str: Option<String> = row.try_get("model_pk").unwrap_or(None);
        let window = llmroller_core::snapshot::SnapshotBudgetWindow {
            id: row.try_get("id")?,
            scope_kind: row.try_get("scope_kind")?,
            upstream_key_id: key_id,
            model_pk: model_str
                .as_deref()
                .and_then(|s| llmroller_core::ids::ModelPk::parse(s).ok()),
            window_kind: row.try_get("window_kind")?,
            period_ms: row.try_get("period_ms").unwrap_or(None),
            anchor_at: row.try_get("anchor_at").unwrap_or(None),
            upstream_window_kind: row.try_get("upstream_window_kind").unwrap_or(None),
            limit_micro_usd: row.try_get("limit_micro_usd")?,
            reserve_pct: row.try_get("reserve_pct").unwrap_or(2),
        };
        out.entry(key_id).or_default().push(window);
    }
    Ok(out)
}

/// Single budget adjustment record from SQLite `budget_adjustments` table (`LIM-1`, `F-064`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetAdjustmentRow {
    pub id: String,
    pub budget_window_id: String,
    pub actor_user_id: Option<String>,
    pub delta_micro_usd: i64,
    pub effective_at: i64,
    pub window_instance_start: Option<i64>,
    pub source: String,
    pub note: Option<String>,
    pub created_at: i64,
}

/// Insert a manual calibration budget adjustment into SQLite within an active transaction (`LIM-1`, `F-064`).
#[allow(clippy::too_many_arguments)]
pub async fn insert_budget_adjustment_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
    budget_window_id: &str,
    actor_user_id: Option<&str>,
    delta_micro_usd: i64,
    effective_at: i64,
    window_instance_start: Option<i64>,
    note: Option<&str>,
    created_at: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO budget_adjustments (
               id, budget_window_id, actor_user_id, delta_micro_usd,
               effective_at, window_instance_start, source, note, created_at
           ) VALUES (?, ?, ?, ?, ?, ?, 'manual_calibration', ?, ?)"#,
    )
    .bind(id)
    .bind(budget_window_id)
    .bind(actor_user_id)
    .bind(delta_micro_usd)
    .bind(effective_at)
    .bind(window_instance_start)
    .bind(note)
    .bind(created_at)
    .execute(&mut **tx)
    .await
    .map(|_| ())
}

/// Insert a manual calibration budget adjustment into SQLite (`LIM-1`, `F-064`).
#[allow(clippy::too_many_arguments)]
pub async fn insert_budget_adjustment(
    pool: &SqlitePool,
    id: &str,
    budget_window_id: &str,
    actor_user_id: Option<&str>,
    delta_micro_usd: i64,
    effective_at: i64,
    window_instance_start: Option<i64>,
    note: Option<&str>,
    created_at: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO budget_adjustments (
               id, budget_window_id, actor_user_id, delta_micro_usd,
               effective_at, window_instance_start, source, note, created_at
           ) VALUES (?, ?, ?, ?, ?, ?, 'manual_calibration', ?, ?)"#,
    )
    .bind(id)
    .bind(budget_window_id)
    .bind(actor_user_id)
    .bind(delta_micro_usd)
    .bind(effective_at)
    .bind(window_instance_start)
    .bind(note)
    .bind(created_at)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Calculate the sum of all budget adjustments for a window effective on or before `now_ms` (`LIM-1`, `F-064`).
pub async fn sum_budget_adjustments_for_window(
    pool: &SqlitePool,
    budget_window_id: &str,
    effective_at_or_before: i64,
) -> Result<i64, sqlx::Error> {
    let sum: Option<i64> = sqlx::query_scalar(
        "SELECT SUM(delta_micro_usd) FROM budget_adjustments WHERE budget_window_id = ? AND effective_at <= ?",
    )
    .bind(budget_window_id)
    .bind(effective_at_or_before)
    .fetch_one(pool)
    .await?;
    Ok(sum.unwrap_or(0))
}

/// List all budget adjustments for a budget window ordered by effective_at descending (`LIM-1`, `F-064`).
pub async fn list_budget_adjustments_for_window(
    pool: &SqlitePool,
    budget_window_id: &str,
) -> Result<Vec<BudgetAdjustmentRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, budget_window_id, actor_user_id, delta_micro_usd, effective_at, window_instance_start, source, note, created_at
         FROM budget_adjustments WHERE budget_window_id = ? ORDER BY effective_at DESC",
    )
    .bind(budget_window_id)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(BudgetAdjustmentRow {
            id: row.try_get("id")?,
            budget_window_id: row.try_get("budget_window_id")?,
            actor_user_id: row.try_get("actor_user_id")?,
            delta_micro_usd: row.try_get("delta_micro_usd")?,
            effective_at: row.try_get("effective_at")?,
            window_instance_start: row.try_get("window_instance_start")?,
            source: row.try_get("source")?,
            note: row.try_get("note")?,
            created_at: row.try_get("created_at")?,
        });
    }
    Ok(out)
}
