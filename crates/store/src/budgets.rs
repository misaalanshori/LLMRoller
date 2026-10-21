//! Fast startup reconstruction of budget windows and monthly accumulators (< 2 s).
//!
//! Spec: Section 5.5, Section 7.1, Section 13.2, Requirements: LIM-1, PERF-4.

use sqlx::{Row as _, SqlitePool};
use std::collections::HashMap;

use llmroller_core::accumulators::{BudgetScopeId, MonthlyAccumulatorRegistry, UtcYearMonth};
use llmroller_core::ids::{ClientKeyId, ShareId, UpstreamKeyId, UserId};
use llmroller_core::windows::RollingRing;

/// In-memory structures rebuilt during startup reconstruction.
pub struct RebuiltState {
    pub key_rings: HashMap<UpstreamKeyId, RollingRing>,
    pub monthly_accumulators: MonthlyAccumulatorRegistry,
}

impl std::fmt::Debug for RebuiltState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RebuiltState")
            .field("key_rings_len", &self.key_rings.len())
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

    let rows = sqlx::query(
        r#"SELECT bucket_ms, upstream_key_id, client_key_id, owner_user_id, provider_scope, share_id, cost_micro_usd
           FROM usage_rollups_5min
           WHERE bucket_ms >= ?"#,
    )
    .bind(cutoff_ms)
    .fetch_all(pool)
    .await?;

    let mut key_rings: HashMap<UpstreamKeyId, RollingRing> = HashMap::new();
    let monthly_accumulators = MonthlyAccumulatorRegistry::new();

    for r in rows {
        let bucket_ms: i64 = r.try_get("bucket_ms")?;
        let upstream_key_id_str: String = r.try_get("upstream_key_id")?;
        let client_key_id_str: String = r.try_get("client_key_id")?;
        let owner_user_id_str: String = r.try_get("owner_user_id")?;
        let provider_scope: String = r.try_get("provider_scope")?;
        let share_id_str: String = r.try_get("share_id")?;
        let cost_micro_usd: i64 = r.try_get("cost_micro_usd")?;

        // 1. Fold into key ring at bucket midpoint (bucket_ms + 150_000)
        if let Ok(key_id) = UpstreamKeyId::parse(&upstream_key_id_str) {
            let ring = key_rings
                .entry(key_id)
                .or_insert_with(|| RollingRing::new(max_window_period_ms));
            ring.record(bucket_ms + 150_000, cost_micro_usd);
        }

        // 2. Fold into monthly accumulators
        let month = UtcYearMonth::from_epoch_millis(bucket_ms);

        if let Ok(client_id) = ClientKeyId::parse(&client_key_id_str) {
            let acc =
                monthly_accumulators.get_or_create(month, BudgetScopeId::ClientKey(client_id));
            acc.used_micro_usd
                .fetch_add(cost_micro_usd, std::sync::atomic::Ordering::Relaxed);
        }

        // Only global provider spend counts toward user monthly budget (USR-5)
        if provider_scope == "global"
            && let Ok(user_id) = UserId::parse(&owner_user_id_str)
        {
            let acc = monthly_accumulators.get_or_create(month, BudgetScopeId::User(user_id));
            acc.used_micro_usd
                .fetch_add(cost_micro_usd, std::sync::atomic::Ordering::Relaxed);
        }

        if !share_id_str.is_empty()
            && let Ok(share_id) = ShareId::parse(&share_id_str)
        {
            let acc =
                monthly_accumulators.get_or_create(month, BudgetScopeId::ProviderShare(share_id));
            acc.used_micro_usd
                .fetch_add(cost_micro_usd, std::sync::atomic::Ordering::Relaxed);
        }
    }

    Ok(RebuiltState {
        key_rings,
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
