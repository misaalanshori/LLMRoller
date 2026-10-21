//! Database repositories for model prices and price-book overrides.
//!
//! Spec: Section 6.9, Section 9.1, Section 13.1, Section 14.2, Requirements: USG-3.

use llmroller_core::pricing::{ContextTier, Rates, Schedule, validate_schedules, validate_tiers};
use sqlx::{Row as _, Sqlite, Transaction};

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
