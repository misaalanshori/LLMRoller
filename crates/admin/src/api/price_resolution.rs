//! Price-row resolution for the per-event cost breakdown (`USG-3`, `USG-10`, `F-070`).
//!
//! Spec: ./docs/srdd/13. SRDD - Pricing, Limits, and Source Provider Usage Engine.md
//! Spec: ./docs/srdd/14.n. SRDD - Usage Ledger, 5-Token Summary Strip, and Initiator-Model-Date Pill Filters.md
//! Requirements: USG-3, USG-10, F-070
//!
//! # Why this is its own module
//!
//! `ISSUE-0164` was a price lookup that bound `canonical_id` into a column named
//! `upstream_model_id`, missed for most providers, and then fell back to `Rates::default()` — the
//! settings default of `$3 / $15 / $0.30 / $3.75 per MTok`. A user asking "why did this request
//! cost that?" got an answer computed from rates that were never in the price book, with nothing
//! saying so. Money is where a plausible default does the most damage: wrong, confident and
//! consequential.
//!
//! The precedence, the fallback chain and the absence policy are therefore one named, testable
//! function rather than an inline block inside a 400-line handler.
//!
//! # Precedence
//!
//! 1. `(provider_id, upstream_model_id)` — the event's own provider, exact upstream id.
//! 2. `('', upstream_model_id)` — the provider-agnostic row for the same upstream id.
//! 3. the same two steps with `canonical_id` — a last resort. Canonically-priced rows genuinely
//!    exist; the original bug was using that path *first and only*, not using it at all.
//!
//! This mirrors `ConfigSnapshot::price_for` (`crates/core/src/snapshot.rs`), which cannot be called
//! here: it needs the in-memory snapshot, and `ApiAppState` carries no `ConfigSnapshot`. The `CASE`
//! ordering below is the same policy expressed in SQL, and `priced_by` reports which identifier
//! actually matched so the two are distinguishable after the fact.
//!
//! # Absence is reported, never invented
//!
//! No row means [`PriceResolution::Unavailable`]. The caller reports `pricing: "unavailable"` and
//! omits rates and money, while still reporting the metered token counts. A row that is *found*
//! but whose columns cannot be read is a different case: that is a schema surprise, and
//! [`PriceResolution::Unavailable`] and it are handled differently on purpose — the former is a
//! reportable state, the latter is a fault.

use super::*;
use llmroller_core::pricing::{ContextTier, Rates, Schedule};
use sqlx::Row as _;

/// Price-row lookup: the event's provider first, then the provider-agnostic row.
///
/// The first bind is the model identifier (upstream id, or canonical id as a last resort), the
/// second is the provider id used only for the ordering.
const PRICE_LOOKUP_SQL: &str = "SELECT pr.input_per_mtok_micro_usd, pr.output_per_mtok_micro_usd, \
            pr.cache_read_per_mtok_micro_usd, pr.cache_write_per_mtok_micro_usd, \
            pr.tiers_json, pr.schedule_json \
     FROM prices pr \
     WHERE pr.upstream_model_id = ? \
     ORDER BY CASE WHEN pr.provider_id = ? THEN 0 \
                   WHEN pr.provider_id = '' THEN 1 \
                   ELSE 2 END \
     LIMIT 1";

/// Which identifier the price row was matched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PricedBy {
    UpstreamModelId,
    CanonicalId,
}

impl PricedBy {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamModelId => "upstream_model_id",
            Self::CanonicalId => "canonical_id",
        }
    }
}

/// The result of resolving a price for one usage event.
pub(crate) enum PriceResolution {
    /// A price row was found and read.
    Priced {
        base_rates: Rates,
        tiers: Vec<ContextTier>,
        schedules: Vec<Schedule>,
        priced_by: PricedBy,
    },
    /// No price row exists for this event's upstream id or its canonical id.
    Unavailable,
}

/// Resolve the price for `(provider, upstream_model_id, canonical_id)`.
///
/// # Errors
///
/// Returns `500` when a price row *was found* but a rate column cannot be read. That is a schema
/// surprise, not an absent row, and surfacing it beats substituting a number — the substitution is
/// exactly the defect this module exists to remove.
pub(crate) async fn resolve_price(
    state: &ApiAppState,
    provider_id: Option<&str>,
    upstream_model_id: Option<&str>,
    canonical_id: &str,
) -> Result<PriceResolution, (StatusCode, Json<serde_json::Value>)> {
    let mut model_candidates: Vec<(PricedBy, String)> = Vec::with_capacity(2);
    if let Some(umid) = upstream_model_id
        && !umid.is_empty()
    {
        model_candidates.push((PricedBy::UpstreamModelId, umid.to_string()));
    }
    model_candidates.push((PricedBy::CanonicalId, canonical_id.to_string()));

    for (priced_by, candidate) in model_candidates {
        let attempt = sqlx::query(PRICE_LOOKUP_SQL)
            .bind(&candidate)
            .bind(provider_id.unwrap_or(""))
            .fetch_optional(&state.auth.reader_pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let Some(row) = attempt else {
            continue;
        };

        // The row was found, so every rate column must be readable.
        let read_rate = |name: &str| -> Result<i64, (StatusCode, Json<serde_json::Value>)> {
            row.try_get::<i64, _>(name).map_err(|e| {
                map_price_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("prices row is missing column {name}: {e}"),
                )
            })
        };
        let base_rates = Rates {
            input_per_mtok_micro_usd: read_rate("input_per_mtok_micro_usd")?,
            output_per_mtok_micro_usd: read_rate("output_per_mtok_micro_usd")?,
            cache_read_per_mtok_micro_usd: read_rate("cache_read_per_mtok_micro_usd")?,
            cache_write_per_mtok_micro_usd: read_rate("cache_write_per_mtok_micro_usd")?,
        };
        let mut tiers: Vec<ContextTier> = Vec::new();
        let mut schedules: Vec<Schedule> = Vec::new();
        if let Ok(Some(tj)) = row.try_get::<Option<String>, _>("tiers_json") {
            tiers = serde_json::from_str(&tj).unwrap_or_default();
        }
        if let Ok(Some(sj)) = row.try_get::<Option<String>, _>("schedule_json") {
            schedules = serde_json::from_str(&sj).unwrap_or_default();
        }
        return Ok(PriceResolution::Priced {
            base_rates,
            tiers,
            schedules,
            priced_by,
        });
    }
    Ok(PriceResolution::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priced_by_labels_the_identifier_that_matched() {
        // The label is what makes "priced by the canonical id as a last resort" distinguishable
        // from "priced by this provider's own upstream id" after the fact — which is the
        // distinction an operator debugging a surprising cost needs.
        assert_eq!(PricedBy::UpstreamModelId.as_str(), "upstream_model_id");
        assert_eq!(PricedBy::CanonicalId.as_str(), "canonical_id");
    }

    #[test]
    fn the_lookup_sql_prefers_the_events_own_provider_then_the_agnostic_row() {
        // The ordering is policy, and it is asserted on the SQL text because the alternative —
        // a database fixture per ordering — would not make the precedence any clearer. The
        // behaviour is covered end-to-end by
        // `the_provider_specific_price_row_wins_over_the_provider_agnostic_one`.
        let sql = PRICE_LOOKUP_SQL;
        assert!(
            sql.contains("WHEN pr.provider_id = ? THEN 0"),
            "the event's own provider must sort first: {sql}"
        );
        assert!(
            sql.contains("WHEN pr.provider_id = '' THEN 1"),
            "the provider-agnostic row (`provider_id = ''`) must sort second: {sql}"
        );
        assert!(
            sql.contains("LIMIT 1"),
            "one row must win, so the caller never has to pick: {sql}"
        );
        // Exactly two binds: the model identifier and the ordering provider id.
        assert_eq!(
            sql.matches('?').count(),
            2,
            "the statement must bind exactly the model id and the provider id: {sql}"
        );
    }

    #[test]
    fn availability_is_modelled_as_a_variant_not_as_a_zero_rate() {
        // `PriceResolution::Unavailable` is a distinct state with no `Rates` in it, so a caller
        // cannot accidentally price an event from it — which is what `Rates::default()` allowed
        // (`ISSUE-0164`). This is asserted by construction: there is no field to read.
        let unavailable = PriceResolution::Unavailable;
        assert!(matches!(unavailable, PriceResolution::Unavailable));
    }
}
