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

/// The ledger's predicate: `model` + `initiator` on raw `usage_events`.
///
/// `LedgerQuery` has no `provider_id`, so it cannot reuse [`event_query!`] — that macro binds
/// provider_id as well. Rather than widen `LedgerQuery`'s public API to make the reuse cosmetic,
/// the ledger gets its own clause, still defined here so every `WHERE` fragment for a read
/// endpoint lives in one file.
///
/// `initiator` is the same three-way union cycle 23 settled on (`14.n.2`, `ISSUE-0142`):
/// `usage_events` carries `client_key_id`, `owner_user_id`, and `service_account_id` directly, so
/// no join is needed here.
macro_rules! ledger_query {
    ($base:literal) => {
        ledger_query!($base, "")
    };
    ($base:literal, $tail:literal) => {
        concat!(
            $base,
            "
           AND (? IS NULL OR canonical_id = ?)
           AND (? IS NULL OR service_account_id = ? OR client_key_id = ? OR owner_user_id = ?)",
            $tail
        )
    };
}
pub(crate) use ledger_query;

/// Bind `model` then `initiator` for [`ledger_query!`]: **2 + 4 = 6** placeholders.
macro_rules! bind_ledger_filters {
    ($query:expr, $query_struct:expr) => {
        $query
            .bind($query_struct.model.as_deref())
            .bind($query_struct.model.as_deref())
            .bind($query_struct.initiator.as_deref())
            .bind($query_struct.initiator.as_deref())
            .bind($query_struct.initiator.as_deref())
            .bind($query_struct.initiator.as_deref())
    };
}
pub(crate) use bind_ledger_filters;

/// The inference CSV's predicate: `model`, `client_key_id`, `service_account_id`, `status` on
/// `usage_events`.
///
/// This is the same table as [`event_query!`] but a different filter set: the export is driven by
/// `LogsQuery`, which has no `initiator`/`provider_id` but does have `client_key_id`,
/// `service_account_id`, and `status`. `model` and `initiator` are not interchangeable with those,
/// so a distinct clause is honest rather than clever.
macro_rules! logs_export_query {
    ($base:literal) => {
        logs_export_query!($base, "")
    };
    ($base:literal, $tail:literal) => {
        concat!(
            $base,
            "
           AND (? IS NULL OR canonical_id = ?)
           AND (? IS NULL OR client_key_id = ?)
           AND (? IS NULL OR service_account_id = ?)
           AND (? IS NULL OR status = ?)",
            $tail
        )
    };
}
pub(crate) use logs_export_query;

/// Bind `model`, `client_key_id`, `service_account_id`, `status`: **2 × 4 = 8** placeholders.
macro_rules! bind_logs_export_filters {
    ($query:expr, $query_struct:expr) => {
        $query
            .bind($query_struct.model.as_deref())
            .bind($query_struct.model.as_deref())
            .bind($query_struct.client_key_id.as_deref())
            .bind($query_struct.client_key_id.as_deref())
            .bind($query_struct.service_account_id.as_deref())
            .bind($query_struct.service_account_id.as_deref())
            .bind($query_struct.status)
            .bind($query_struct.status)
    };
}
pub(crate) use bind_logs_export_filters;

/// The API-stream CSV's predicate: `method` and `status` on `control_plane_logs`.
///
/// `control_plane_logs` has no `canonical_id`, `client_key_id`, or `service_account_id`
/// (`0001_initial.sql:572`), so this branch genuinely cannot share the inference export's clause.
macro_rules! api_logs_export_query {
    ($base:literal) => {
        api_logs_export_query!($base, "")
    };
    ($base:literal, $tail:literal) => {
        concat!(
            $base,
            "
           AND (? IS NULL OR method = ?)
           AND (? IS NULL OR status = ?)",
            $tail
        )
    };
}
pub(crate) use api_logs_export_query;

/// Bind `method`, `status`: **2 × 2 = 4** placeholders.
macro_rules! bind_api_logs_export_filters {
    ($query:expr, $query_struct:expr) => {
        $query
            .bind($query_struct.method.as_deref())
            .bind($query_struct.method.as_deref())
            .bind($query_struct.status)
            .bind($query_struct.status)
    };
}
pub(crate) use bind_api_logs_export_filters;

/// The admin audit log's predicate: `action` and `target_type` on `audit_log`.
///
/// Time bounds and the `seq` cursor are added by the handler inline, because the audit log is
/// ordered by `seq DESC` rather than by a timestamp — its cursor is a simple `seq < ?`
/// (`ISSUE-0144`), unlike the composite cursors elsewhere.
macro_rules! audit_log_query {
    ($base:literal) => {
        audit_log_query!($base, "")
    };
    ($base:literal, $tail:literal) => {
        concat!(
            $base,
            "
           AND (? IS NULL OR action = ?)
           AND (? IS NULL OR target_type = ?)",
            $tail
        )
    };
}
pub(crate) use audit_log_query;

/// Bind `action`, `target_type`: **2 × 2 = 4** placeholders.
macro_rules! bind_audit_log_filters {
    ($query:expr, $query_struct:expr) => {
        $query
            .bind($query_struct.action.as_deref())
            .bind($query_struct.action.as_deref())
            .bind($query_struct.target_type.as_deref())
            .bind($query_struct.target_type.as_deref())
    };
}
pub(crate) use bind_audit_log_filters;

/// The **tenant** visibility predicate for `audit_log` (`ISSUE-0172`).
///
/// `audit_log` is instance-wide; `/api/user/audit-log` and `/app/partials/audit-log` are its
/// tenant-facing views. This fragment is the security boundary between them, so it is written
/// **once** here and spliced into both call sites. The two handlers had already drifted on
/// columns (8 vs 5), which is what happens when two copies of a query differ only by whoever last
/// edited one of them; a second copy of *this* predicate would drift the same way, and a wrong
/// copy is a data leak rather than an ugly response.
///
/// ## Shape: an allow-list of target types, each with its own ownership test
///
/// Never "any event whose `target_id` resolves to a row I own". That is the obvious implementation
/// and it leaks: `setting`, `instance`, `instance_meta` and `http_path` are instance-wide facts
/// with ids that can collide with a user's, so a catch-all would hand a tenant the instance's
/// master-key rotations. Every clause below names both the type and the ownership path, and a
/// type that is not named is invisible — which is the safe direction for a boundary to fail, and
/// what makes a target type added by a future change default to invisible.
///
/// | `target_type` | Visible to a tenant? | Ownership path |
/// | --- | --- | --- |
/// | `user` | yes, own | `target_id = caller` |
/// | `share` | yes, own or granted | `provider_shares.owner_user_id / grantee_user_id` |
/// | `client_key` | yes, own | `client_keys.owner_user_id` (NOT NULL) |
/// | `upstream_key` | yes, own | `upstream_keys.owner_user_id` (nullable) |
/// | `provider` | yes, own | `providers.owner_user_id` (nullable) |
/// | `service_account` | yes, own | `service_accounts.owner_user_id` (NOT NULL) |
/// | `session`, `web_session`, `trace` | yes, own | via `usage_events.owner_user_id` |
/// | `setting`, `instance`, `instance_meta`, `http_path` | **no** | instance-wide / operator-only |
/// | anything else | **no** | allow-list |
///
/// ## Nullable owners
///
/// `upstream_keys.owner_user_id` and `providers.owner_user_id` are nullable, and NULL there means
/// "global / unowned" — a shared provider's config change is not any one tenant's event. Plain
/// equality handles that correctly without a special case: `NULL = ?` is never true in SQL, so an
/// unowned row matches no caller. An `IS NULL OR owner_user_id = ?` would be a bug.
///
/// ## Why `trace` and `session` are here
///
/// A trace is an admin's *view of a user's conversation*, and a `session`-targeted `trace.viewed`
/// row is the same event keyed by session hash. Both resolve to an owner through `usage_events`,
/// so a tenant sees activity on their own traffic and nothing else. Deciding this deliberately
/// (rather than letting it fall out of whatever the SQL happens to join) is the point: a user
/// seeing "an admin inspected your session" is a legitimate audit fact about their own data.
///
/// `web_session` is keyed on `id_hash` of the caller's own login session, so it is visible through
/// `web_sessions.user_id`.
///
/// ## Bind order
///
/// **Twelve** placeholders, all the caller's own user id. See
/// [`bind_tenant_audit_visibility`], which is the only place the bind chain is written, and the
/// placeholder-count test which keeps the two in step.
macro_rules! tenant_audit_visibility {
    () => {
        "
           (actor_user_id = ?
            OR (target_type = 'user' AND target_id = ?)
            OR (target_type = 'share' AND target_id IN (
                SELECT id FROM provider_shares WHERE owner_user_id = ? OR grantee_user_id = ?
            ))
            OR (target_type = 'client_key' AND target_id IN (
                SELECT id FROM client_keys WHERE owner_user_id = ?
            ))
            OR (target_type = 'upstream_key' AND target_id IN (
                SELECT id FROM upstream_keys WHERE owner_user_id = ?
            ))
            OR (target_type = 'provider' AND target_id IN (
                SELECT id FROM providers WHERE owner_user_id = ?
            ))
            OR (target_type = 'service_account' AND target_id IN (
                SELECT id FROM service_accounts WHERE owner_user_id = ?
            ))
            OR (target_type = 'web_session' AND target_id IN (
                SELECT id_hash FROM web_sessions WHERE user_id = ?
            ))
            OR (target_type = 'trace' AND target_id IN (
                SELECT session_hash FROM trace_records WHERE owner_user_id = ?
                UNION
                SELECT request_id FROM trace_records WHERE owner_user_id = ?
            ))
            OR (target_type = 'session' AND target_id IN (
                SELECT session_hash FROM usage_events WHERE owner_user_id = ?
            )))"
    };
}
pub(crate) use tenant_audit_visibility;

/// Bind [`tenant_audit_visibility`]: **12** placeholders, all the caller's user id.
///
/// One macro rather than twelve `.bind(..)` calls per handler, because a mis-spliced predicate
/// is **not** a compile error and not necessarily a SQL error either — it shifts every later bind
/// by one, and the query then returns a plausible but wrong set, or fails with a bare
/// "datatype mismatch" that names no column. That is exactly what happened while writing this:
/// the fragment has 12 placeholders (actor, user target, 2× share, client_key, upstream_key,
/// provider, service_account, web_session, 2× trace, session) and the first binder copied 11.
/// `tenant_audit_visibility_placeholder_count_matches_binder` in this module's tests now pins the
/// two together so the next edit cannot repeat it.
macro_rules! bind_tenant_audit_visibility {
    ($query:expr, $user_id:expr) => {
        $query
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
            .bind($user_id)
    };
}
pub(crate) use bind_tenant_audit_visibility;

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

#[cfg(test)]
mod tests {
    //! Placeholder-count tests for every clause above.
    //!
    //! These exist because cycle 23 shipped a `(? IS NULL OR a = ? OR b = ? OR c = ?)` clause bound
    //! with only three values. sqlx binds positionally, so that did not fail to compile — it
    //! surfaced as a runtime `no such column` on SQL whose text looked correct. A count assertion
    //! turns that class of mistake into a test failure with no database required.

    /// Number of `?` placeholders a clause declares.
    fn placeholders(sql: &str) -> usize {
        sql.matches('?').count()
    }

    /// Number of `.bind(` calls a binder macro expands to.
    ///
    /// The binder macros are invoked against a recording stand-in that counts calls, so this
    /// measures the real expansion rather than a transcribed number.
    macro_rules! count_binds {
        ($binder:ident, $query_struct:expr) => {{
            struct Counter(usize);
            #[allow(dead_code)]
            impl Counter {
                fn bind<T>(&mut self, _v: T) -> &mut Self {
                    self.0 += 1;
                    self
                }
            }
            let mut c = Counter(0);
            $binder!(&mut c, $query_struct).0
        }};
    }

    #[derive(Default)]
    struct Filters {
        provider_id: Option<String>,
        model: Option<String>,
        initiator: Option<String>,
        client_key_id: Option<String>,
        service_account_id: Option<String>,
        method: Option<String>,
        action: Option<String>,
        target_type: Option<String>,
        status: Option<i64>,
    }

    #[test]
    fn every_clause_declares_the_number_of_binds_its_binder_supplies() {
        let f = Filters::default();

        let rollup = rollup_query!("SELECT 1 FROM usage_rollups_hourly WHERE 1 = 1");
        assert_eq!(
            placeholders(rollup),
            count_binds!(bind_usage_filters, &f),
            "rollup clause vs bind_usage_filters!:\n{rollup}"
        );

        let event = event_query!("SELECT 1 FROM usage_events WHERE 1 = 1");
        assert_eq!(
            placeholders(event),
            count_binds!(bind_usage_filters, &f),
            "event clause vs bind_usage_filters!:\n{event}"
        );

        let ledger = ledger_query!("SELECT 1 FROM usage_events WHERE 1 = 1");
        assert_eq!(
            placeholders(ledger),
            count_binds!(bind_ledger_filters, &f),
            "ledger clause vs bind_ledger_filters!:\n{ledger}"
        );

        let export = logs_export_query!("SELECT 1 FROM usage_events WHERE 1 = 1");
        assert_eq!(
            placeholders(export),
            count_binds!(bind_logs_export_filters, &f),
            "export clause vs bind_logs_export_filters!:\n{export}"
        );

        let api = api_logs_export_query!("SELECT 1 FROM control_plane_logs WHERE 1 = 1");
        assert_eq!(
            placeholders(api),
            count_binds!(bind_api_logs_export_filters, &f),
            "api export clause vs bind_api_logs_export_filters!:\n{api}"
        );

        let audit = audit_log_query!("SELECT 1 FROM audit_log WHERE 1 = 1");
        assert_eq!(
            placeholders(audit),
            count_binds!(bind_audit_log_filters, &f),
            "audit clause vs bind_audit_log_filters!:\n{audit}"
        );

        // The tenant-visibility predicate binds the caller's id once per placeholder, so its
        // binder takes a plain string rather than a filter struct.
        let tenant = tenant_audit_visibility!();
        assert_eq!(
            placeholders(tenant),
            count_binds!(bind_tenant_audit_visibility, "user-id"),
            "tenant visibility clause vs bind_tenant_audit_visibility!:\n{tenant}"
        );
        assert_eq!(
            placeholders(tenant),
            12,
            "the tenant predicate has 12 placeholders; if this number changed, the binder must \
             change with it and the visibility test must still cover every target type:\n{tenant}"
        );
    }

    #[test]
    fn the_tail_is_spliced_after_the_predicate() {
        let sql = rollup_query!("BASE", "GROUP BY x");
        let group_at = sql.find("GROUP BY").expect("tail present");
        let clause_at = sql.find("provider_id").expect("clause present");
        assert!(
            clause_at < group_at,
            "the predicate must precede the trailing clause, else SQL is malformed:\n{sql}"
        );
    }
}
