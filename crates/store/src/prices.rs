//! Database repositories for model prices and price-book overrides.
//!
//! Spec: Section 6.9, Section 9.1, Section 13.1, Section 14.2, Requirements: USG-3.

use llmroller_core::pricing::{ContextTier, Rates, Schedule, validate_schedules, validate_tiers};
use sqlx::{Row as _, Sqlite, SqlitePool, Transaction};

#[derive(Debug, thiserror::Error)]
pub enum PriceError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone)]
pub struct UpsertPriceParams {
    pub provider_id: String,
    pub provider_kind: String,
    pub upstream_model_id: String,
    pub rates: Rates,
    /// `None` preserves the stored value; `Some(vec![])` explicitly clears it.
    ///
    /// The distinction is load-bearing (`ISSUE-0163`): a UI that edits only the flat rates sends
    /// no `tiers` at all, and treating that as "no tiers" wiped an operator's configuration while
    /// reporting success. This is the same rule `patch_admin_model_handler` documents
    /// (`models.rs:100-104`) — omission means unchanged, and clearing must be asked for.
    pub tiers: Option<Vec<ContextTier>>,
    /// `None` preserves the stored value; `Some(vec![])` explicitly clears it.
    pub schedules: Option<Vec<Schedule>>,
    pub source: String,
    pub now_ms: i64,
}

pub async fn upsert_price_tx(
    tx: &mut Transaction<'_, Sqlite>,
    params: UpsertPriceParams,
) -> Result<(), PriceError> {
    // Validate only what the caller is setting. An omitted collection is not "empty input" — it
    // is "unchanged", so validating it would be validating a value nobody asked to write.
    if let Some(tiers) = &params.tiers {
        validate_tiers(tiers).map_err(PriceError::Validation)?;
    }
    if let Some(schedules) = &params.schedules {
        validate_schedules(schedules).map_err(PriceError::Validation)?;
    }

    // `None` binds as SQL NULL, which `COALESCE` turns into the stored value. `Some(vec![])`
    // serialises to `[]` and therefore clears, so "clear" stays expressible and explicit.
    let tiers_json = params
        .tiers
        .as_ref()
        .map(|tiers| serde_json::to_string(tiers).unwrap_or_else(|_| "[]".to_string()));
    let schedule_json = params
        .schedules
        .as_ref()
        .map(|schedules| serde_json::to_string(schedules).unwrap_or_else(|_| "[]".to_string()));

    sqlx::query(
        r#"INSERT INTO prices (
               provider_id, provider_kind, upstream_model_id,
               input_per_mtok_micro_usd, output_per_mtok_micro_usd,
               cache_read_per_mtok_micro_usd, cache_write_per_mtok_micro_usd,
               tiers_json, schedule_json, source, updated_at
           )
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT (provider_id, provider_kind, upstream_model_id) DO UPDATE SET
               input_per_mtok_micro_usd = excluded.input_per_mtok_micro_usd,
               output_per_mtok_micro_usd = excluded.output_per_mtok_micro_usd,
               cache_read_per_mtok_micro_usd = excluded.cache_read_per_mtok_micro_usd,
               cache_write_per_mtok_micro_usd = excluded.cache_write_per_mtok_micro_usd,
               tiers_json = COALESCE(excluded.tiers_json, prices.tiers_json),
               schedule_json = COALESCE(excluded.schedule_json, prices.schedule_json),
               source = excluded.source,
               updated_at = excluded.updated_at"#,
    )
    .bind(&params.provider_id)
    .bind(&params.provider_kind)
    .bind(&params.upstream_model_id)
    .bind(params.rates.input_per_mtok_micro_usd)
    .bind(params.rates.output_per_mtok_micro_usd)
    .bind(params.rates.cache_read_per_mtok_micro_usd)
    .bind(params.rates.cache_write_per_mtok_micro_usd)
    .bind(&tiers_json)
    .bind(&schedule_json)
    .bind(&params.source)
    .bind(params.now_ms)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Delete a model price entry by provider ID and upstream model ID.
pub async fn delete_price_tx(
    tx: &mut Transaction<'_, Sqlite>,
    provider_id: &str,
    upstream_model_id: &str,
) -> Result<bool, PriceError> {
    let res = sqlx::query("DELETE FROM prices WHERE provider_id = ? AND upstream_model_id = ?")
        .bind(provider_id)
        .bind(upstream_model_id)
        .execute(&mut **tx)
        .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn get_price_tx(
    tx: &mut Transaction<'_, Sqlite>,
    provider_id: &str,
    provider_kind: &str,
    upstream_model_id: &str,
) -> Result<Option<Rates>, sqlx::Error> {
    let row = sqlx::query(
        r#"SELECT input_per_mtok_micro_usd, output_per_mtok_micro_usd,
                  cache_read_per_mtok_micro_usd, cache_write_per_mtok_micro_usd
           FROM prices
           WHERE provider_id = ? AND provider_kind = ? AND upstream_model_id = ?"#,
    )
    .bind(provider_id)
    .bind(provider_kind)
    .bind(upstream_model_id)
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(r) = row {
        return Ok(Some(Rates {
            input_per_mtok_micro_usd: r.try_get("input_per_mtok_micro_usd")?,
            output_per_mtok_micro_usd: r.try_get("output_per_mtok_micro_usd")?,
            cache_read_per_mtok_micro_usd: r.try_get("cache_read_per_mtok_micro_usd")?,
            cache_write_per_mtok_micro_usd: r.try_get("cache_write_per_mtok_micro_usd")?,
        }));
    }

    let row_default = sqlx::query(
        r#"SELECT input_per_mtok_micro_usd, output_per_mtok_micro_usd,
                  cache_read_per_mtok_micro_usd, cache_write_per_mtok_micro_usd
           FROM prices
           WHERE provider_id = '' AND provider_kind = ? AND upstream_model_id = ?"#,
    )
    .bind(provider_kind)
    .bind(upstream_model_id)
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(r) = row_default {
        return Ok(Some(Rates {
            input_per_mtok_micro_usd: r.try_get("input_per_mtok_micro_usd")?,
            output_per_mtok_micro_usd: r.try_get("output_per_mtok_micro_usd")?,
            cache_read_per_mtok_micro_usd: r.try_get("cache_read_per_mtok_micro_usd")?,
            cache_write_per_mtok_micro_usd: r.try_get("cache_write_per_mtok_micro_usd")?,
        }));
    }

    let norm_model = llmroller_core::catalog::normalize_model_id(upstream_model_id).ok();
    let date_stripped = norm_model
        .as_deref()
        .and_then(llmroller_core::snapshot::strip_model_date_suffix)
        .or_else(|| llmroller_core::snapshot::strip_model_date_suffix(upstream_model_id));
    for candidate in std::iter::once(upstream_model_id)
        .chain(norm_model.as_deref())
        .chain(date_stripped.as_deref())
    {
        let row_bundled = sqlx::query(
            r#"SELECT input_per_mtok_micro_usd, output_per_mtok_micro_usd,
                      cache_read_per_mtok_micro_usd, cache_write_per_mtok_micro_usd
               FROM prices
               WHERE provider_id IN ('bundled', '') AND provider_kind = 'global' AND upstream_model_id = ?
               ORDER BY CASE WHEN provider_id = 'bundled' THEN 0 ELSE 1 END
               LIMIT 1"#,
        )
        .bind(candidate)
        .fetch_optional(&mut **tx)
        .await?;

        if let Some(r) = row_bundled {
            return Ok(Some(Rates {
                input_per_mtok_micro_usd: r.try_get("input_per_mtok_micro_usd")?,
                output_per_mtok_micro_usd: r.try_get("output_per_mtok_micro_usd")?,
                cache_read_per_mtok_micro_usd: r.try_get("cache_read_per_mtok_micro_usd")?,
                cache_write_per_mtok_micro_usd: r.try_get("cache_write_per_mtok_micro_usd")?,
            }));
        }
    }

    Ok(None)
}

/// Loads all price-book entries for the immutable ConfigSnapshot.
pub async fn load_price_entries_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<
    std::collections::HashMap<
        llmroller_core::snapshot::PriceKey,
        llmroller_core::pricing::PriceEntry,
    >,
    sqlx::Error,
> {
    let rows = sqlx::query(
        "SELECT provider_id, provider_kind, upstream_model_id,
                input_per_mtok_micro_usd, output_per_mtok_micro_usd,
                cache_read_per_mtok_micro_usd, cache_write_per_mtok_micro_usd,
                tiers_json, schedule_json
         FROM prices",
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut out = std::collections::HashMap::new();
    for row in rows {
        let provider_id: String = row.try_get("provider_id")?;
        let provider_kind: String = row.try_get("provider_kind")?;
        let upstream_model_id: String = row.try_get("upstream_model_id")?;
        let base_rates = Rates {
            input_per_mtok_micro_usd: row.try_get("input_per_mtok_micro_usd")?,
            output_per_mtok_micro_usd: row.try_get("output_per_mtok_micro_usd")?,
            cache_read_per_mtok_micro_usd: row.try_get("cache_read_per_mtok_micro_usd")?,
            cache_write_per_mtok_micro_usd: row.try_get("cache_write_per_mtok_micro_usd")?,
        };
        let tiers: Vec<ContextTier> = row
            .try_get::<Option<String>, _>("tiers_json")
            .unwrap_or(None)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let schedules: Vec<Schedule> = row
            .try_get::<Option<String>, _>("schedule_json")
            .unwrap_or(None)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        out.insert(
            llmroller_core::snapshot::PriceKey {
                provider_id,
                provider_kind,
                upstream_model_id,
            },
            llmroller_core::pricing::PriceEntry {
                provider_id: String::new(),
                provider_kind: String::new(),
                upstream_model_id: String::new(),
                base_rates,
                tiers,
                schedules,
            },
        );
    }
    Ok(out)
}

/// Baseline prices for common upstream models seeded on startup if not already present (USG-3, F071).
pub const BUNDLED_PRICES: &[(&str, i64, i64, i64, i64)] = &[
    (
        "claude-sonnet-4.6",
        3_000_000,
        15_000_000,
        300_000,
        3_750_000,
    ),
    (
        "claude-3-5-sonnet-20241022",
        3_000_000,
        15_000_000,
        300_000,
        3_750_000,
    ),
    (
        "claude-3-5-haiku-20241022",
        800_000,
        4_000_000,
        80_000,
        1_000_000,
    ),
    (
        "claude-3-opus-20240229",
        15_000_000,
        75_000_000,
        1_500_000,
        18_750_000,
    ),
    ("gpt-4o", 2_500_000, 10_000_000, 1_250_000, 0),
    ("gpt-4o-mini", 150_000, 600_000, 75_000, 0),
    ("o1", 15_000_000, 60_000_000, 7_500_000, 0),
    ("o3-mini", 1_100_000, 4_400_000, 550_000, 0),
    ("deepseek-chat", 270_000, 1_100_000, 70_000, 0),
    ("deepseek-reasoner", 550_000, 2_190_000, 140_000, 0),
    ("gpt-5.4", 2_500_000, 10_000_000, 1_250_000, 0),
    (
        "claude-opus-4.6",
        15_000_000,
        75_000_000,
        1_500_000,
        18_750_000,
    ),
    ("deepseek-v4.1", 270_000, 1_100_000, 70_000, 0),
    ("qwen-3.6-plus", 400_000, 1_200_000, 100_000, 0),
];

pub async fn seed_bundled_prices(pool: &SqlitePool, now_ms: i64) -> Result<usize, sqlx::Error> {
    let mut count = 0;
    for (model, input, output, cache_read, cache_write) in BUNDLED_PRICES {
        let inserted = sqlx::query(
            r#"INSERT OR IGNORE INTO prices (
                   provider_id, provider_kind, upstream_model_id,
                   input_per_mtok_micro_usd, output_per_mtok_micro_usd,
                   cache_read_per_mtok_micro_usd, cache_write_per_mtok_micro_usd,
                   source, updated_at
               )
               VALUES ('bundled', 'global', ?, ?, ?, ?, ?, 'bundled', ?)"#,
        )
        .bind(model)
        .bind(input)
        .bind(output)
        .bind(cache_read)
        .bind(cache_write)
        .bind(now_ms)
        .execute(pool)
        .await?
        .rows_affected();
        count += inserted as usize;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::migrate;
    use crate::pool::{Role, open_pool};
    use llmroller_core::ids::ProviderId;
    use llmroller_core::provider::ProviderKind;
    use llmroller_core::snapshot::ConfigSnapshot;
    use std::sync::Arc;

    #[tokio::test]
    #[doc = "Spec: USG-2, USG-3"]
    async fn price_for_resolves_seeded_bundled_global_prices() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = open_pool(&dir.path().join("prices.db"), Role::Writer)
            .await
            .expect("open pool");
        migrate(&pool).await.expect("migrate");

        let seeded = seed_bundled_prices(&pool, 1_000).await.expect("seed");
        assert!(seeded >= 10);

        let mut tx = pool.begin().await.expect("begin");
        let rates = get_price_tx(&mut tx, "prv_1", "openrouter", "claude-sonnet-4.6")
            .await
            .expect("get_price_tx")
            .expect("bundled price must resolve");
        assert_eq!(rates.input_per_mtok_micro_usd, 3_000_000);

        let entries = load_price_entries_tx(&mut tx)
            .await
            .expect("load_price_entries_tx");
        tx.commit().await.expect("commit");

        let snap = ConfigSnapshot {
            prices: Arc::new(entries),
            ..ConfigSnapshot::default()
        };
        let p_id = ProviderId::generate();
        let entry = snap
            .price_for(&p_id, ProviderKind::Openrouter, "claude-sonnet-4.6")
            .expect("snapshot price_for must resolve bundled global price");
        assert_eq!(entry.base_rates.input_per_mtok_micro_usd, 3_000_000);
        assert_eq!(entry.base_rates.output_per_mtok_micro_usd, 15_000_000);
    }
}
