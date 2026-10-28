//! The shared filter predicate for the four usage read endpoints (`ISSUE-0175`).
//!
//! `UsageFilterParams` deserialises seven filters — `since`, `until`, `limit`, `after`,
//! `provider_id`, `model`, `initiator` — but before this module each endpoint hand-wrote its own
//! `WHERE` fragment and bound only `since`/`until`. Four hand-written fragments is precisely how
//! `provider_id` came to be accepted and used in zero queries: a parameter that is never
//! referenced in a `WHERE` clause does not fail to compile, it silently does nothing, and the
//! caller gets a confidently wrong number.
//!
//! So the predicate is defined **once** as `const` fragments spliced into every query, and bound
//! by one macro. The fragments are `&'static str` literals with no interpolation, which is why
//! they satisfy sqlx 0.9's `SqlSafeStr` requirement: no user data ever reaches the SQL text.
//!
//! ## Why two flavours
//!
//! Three endpoints aggregate `usage_rollups_hourly`; the events listing reads raw `usage_events`.
//! The column names differ (`hour_ms` vs `ts`), and the rollup table carries no
//! `service_account_id`. Rather than paper over that with a lossy common subset, each table gets
//! its own explicitly-named flavour. **Both flavours declare the same number of placeholders**
//! (2 provider + 2 model + 3 initiator = 7), so [`bind_usage_filters!`] is valid for either — that
//! invariant is what makes one binder safe for both.
//!
//! ## Bind order
//!
//! sqlx binds positionally and the time bound appears **twice** per clause (once for the `IS NULL`
//! guard, once for the comparison). A mis-spliced predicate therefore does not raise an error — it
//! shifts every later bind by one and returns plausible wrong numbers. That is why the bind chain
//! exists in exactly one place, and why `all_four_endpoints_agree_on_a_filtered_totals` exists: a
//! wrong bind makes one endpoint disagree rather than fail.
//!
//! ## `initiator`
//!
//! `14.n.2` defines the Initiator Filter Pill as a list of "all users, service accounts (`agents`,
//! `ci`, `bots`), and individual client keys (`llr_...`)" — one dropdown, three kinds of principal.
//! So the predicate is a **union over the three identity columns**, not a choice of one: filtering
//! by user X returns everything X did (including through keys X no longer uses), filtering by key K
//! returns only K. That matches the label. An earlier design note argued for a single column on the
//! grounds that a union is "broader than its label"; the spec is explicit that the label *is* the
//! union, so the note was wrong — recorded in the cycle report.

/// The rollup-backed predicate, spliced by [`rollup_query!`] into `summary`, `timeseries`, and
/// `breakdown`.
///
/// `usage_rollups_hourly` has no `service_account_id` column (`0001_initial.sql:485`), so the
/// service-account half of the initiator union resolves through the owning `client_keys` row.
/// The `EXISTS` probes `idx_client_keys_owner` / the primary key, and `initiator` is `NULL` for an
/// unfiltered call, in which case the whole clause short-circuits before the subquery runs.
/// Build a rollup query by splicing the shared rollup predicate between `$base` and `$tail`.
///
/// `$tail` exists because the grouped queries need `GROUP BY … ORDER BY …` **after** the
/// predicate. Appending the predicate to a base literal that already ended in `GROUP BY` produced
/// `near "AND": syntax error` at runtime — an ordering bug the single-argument form could not
/// express, and one that only a smoke test would catch.
///
/// A macro rather than `concat!($base, ROLLUP_FILTERS)` because `concat!` accepts only literal
/// tokens, and a `const` is not one. Keeping the predicate text inside the macro body is what
/// makes this the single source of truth: there is exactly one place the clause is written, and
/// `concat!` splices it into a `&'static str` with no runtime allocation and no interpolation.
macro_rules! rollup_query {
    ($base:literal) => {
        rollup_query!($base, "")
    };
    ($base:literal, $tail:literal) => {
        concat!(
            $base,
            "
           AND (? IS NULL OR provider_id = ?)
           AND (? IS NULL OR canonical_id = ?)
           AND (? IS NULL OR EXISTS (
                 SELECT 1 FROM client_keys ck
                 WHERE ck.id = usage_rollups_hourly.client_key_id
                   AND (ck.id = ? OR ck.owner_user_id = ? OR ck.service_account_id = ?)
               ))",
            $tail
        )
    };
}
pub(crate) use rollup_query;

/// Build a raw-`usage_events` query by appending the shared event predicate to `$base`.
macro_rules! event_query {
    ($base:literal) => {
        event_query!($base, "")
    };
    ($base:literal, $tail:literal) => {
        concat!(
            $base,
            "
           AND (? IS NULL OR provider_id = ?)
           AND (? IS NULL OR canonical_id = ?)
           AND (? IS NULL OR service_account_id = ? OR client_key_id = ? OR owner_user_id = ?)",
            $tail
        )
    };
}
pub(crate) use event_query;

/// The raw-`usage_events` predicate, spliced by [`event_query!`] into the events listing.
///
/// `usage_events` carries `service_account_id` directly (`0001_initial.sql:437`), so no join is
/// needed. Same placeholder count as [`ROLLUP_FILTERS`]; only the column names differ.
/// Append `provider_id`, `model`, and `initiator` binds in the order both fragments declare.
///
/// Call **after** the `since`/`until` binds and **before** any trailing binds (`after`, `limit`).
/// Each filter binds the same value to every placeholder in its clause, mirroring the
/// `(? IS NULL OR col = ?)` shape already used for the time bounds.
///
/// The counts are **2 provider + 2 model + 4 initiator = 8**, because the initiator clause is
/// `(? IS NULL OR a = ? OR b = ? OR c = ?)` — one guard plus three identity columns. Miscounting
/// here does not fail to compile: sqlx binds positionally, so a short chain shifts nothing but
/// produces a runtime SQL error on a query whose text looked right. That is exactly the
/// bind-order hazard the cycle brief flagged, and it bit on the first attempt.
macro_rules! bind_usage_filters {
    ($query:expr, $params:expr) => {
        $query
            .bind($params.provider_id.as_deref())
            .bind($params.provider_id.as_deref())
            .bind($params.model.as_deref())
            .bind($params.model.as_deref())
            .bind($params.initiator.as_deref())
            .bind($params.initiator.as_deref())
            .bind($params.initiator.as_deref())
            .bind($params.initiator.as_deref())
    };
}
pub(crate) use bind_usage_filters;

/// The canonical token total: the four priced classes.
///
/// The same definition the Overview KPI, the Usage Ledger, `calculate_cost_micro_usd`, and cycle
/// 22's logs projection use — `input_uncached + cache_read + cache_write + output`.
///
/// **`reasoning` is excluded on purpose.** It is a diagnostic subset of `output`
/// (`crates/core/src/pricing.rs:24`) and `12. SRDD:133-134` records that `output_tokens` "includes
/// reasoning", so adding it would double-count. Reporting it as its own field beside the total is
/// correct; summing it in is not.
pub(crate) const fn canonical_total(
    input: i64,
    cache_read: i64,
    cache_write: i64,
    output: i64,
) -> i64 {
    input + cache_read + cache_write + output
}
