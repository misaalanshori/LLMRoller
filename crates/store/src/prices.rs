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
    pub tiers: Vec<ContextTier>,
    pub schedules: Vec<Schedule>,
    pub source: String,
    pub now_ms: i64,
}

pub async fn upsert_price_tx(
    tx: &mut Transaction<'_, Sqlite>,
    params: UpsertPriceParams,
) -> Result<(), PriceError> {
    validate_tiers(&params.tiers).map_err(PriceError::Validation)?;
    validate_schedules(&params.schedules).map_err(PriceError::Validation)?;

    let tiers_json = if params.tiers.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&params.tiers).unwrap_or_else(|_| "[]".to_string()))
    };

    let schedule_json = if params.schedules.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&params.schedules).unwrap_or_else(|_| "[]".to_string()))
    };

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
               tiers_json = excluded.tiers_json,
               schedule_json = excluded.schedule_json,
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
    ("claude-sonnet-4.6", 3_000_000, 15_000_000, 300_000, 3_750_000),
    ("claude-3-5-sonnet-20241022", 3_000_000, 15_000_000, 300_000, 3_750_000),
    ("claude-3-5-haiku-20241022", 800_000, 4_000_000, 80_000, 1_000_000),
    ("claude-3-opus-20240229", 15_000_000, 75_000_000, 1_500_000, 18_750_000),
    ("gpt-4o", 2_500_000, 10_000_000, 1_250_000, 0),
    ("gpt-4o-mini", 150_000, 600_000, 75_000, 0),
    ("o1", 15_000_000, 60_000_000, 7_500_000, 0),
    ("o3-mini", 1_100_000, 4_400_000, 550_000, 0),
    ("deepseek-chat", 270_000, 1_100_000, 70_000, 0),
    ("deepseek-reasoner", 550_000, 2_190_000, 140_000, 0),
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
