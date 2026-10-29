use super::*;
use sqlx::Row as _;

/// `GET /api/user/audit-log` (`AUD-2`).
///
/// Returns only rows attributable to the authenticated actor or their resources; admins use
/// `/admin/api/audit-log` for the instance-wide chain. Hashes are omitted per AUD-2.
pub(crate) async fn get_user_audit_log_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AuditLogQuery>,
) -> Result<Json<Vec<serde_json::Value>>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    // The tenant-visibility predicate is spliced from `tenant_audit_visibility!` rather than
    // written here, so this handler and `/app/partials/audit-log` cannot disagree about who may
    // see what (`ISSUE-0172`). See that macro for the allow-list and the nullable-owner note.
    let rows = bind_tenant_audit_visibility!(
        sqlx::query(concat!(
            "SELECT seq, ts, actor_user_id, ip_addr, action, target_type, target_id, detail_json
             FROM audit_log
             WHERE ",
            tenant_audit_visibility!(),
            "
               AND (? IS NULL OR action = ?)
               AND (? IS NULL OR target_type = ?)
               AND (? IS NULL OR ts >= ?)
               AND (? IS NULL OR ts <= ?)
               AND (? IS NULL OR seq < ?)
             ORDER BY seq DESC LIMIT ?"
        )),
        user_id.to_string()
    )
    .bind(query.action.as_deref())
    .bind(query.action.as_deref())
    .bind(query.target_type.as_deref())
    .bind(query.target_type.as_deref())
    .bind(query.from)
    .bind(query.from)
    .bind(query.to)
    .bind(query.to)
    .bind(query.cursor)
    .bind(query.cursor)
    .bind(limit)
    .fetch_all(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(
        rows.iter()
            .map(|row| {
                serde_json::json!({
                    "seq": row.try_get::<i64, _>("seq").unwrap_or_default(),
                    "ts": row.try_get::<i64, _>("ts").unwrap_or_default(),
                    "actor_user_id": row.try_get::<Option<String>, _>("actor_user_id").unwrap_or_default(),
                    "ip_addr": row.try_get::<Option<String>, _>("ip_addr").unwrap_or_default(),
                    "action": row.try_get::<String, _>("action").unwrap_or_default(),
                    "target_type": row.try_get::<String, _>("target_type").unwrap_or_default(),
                    "target_id": row.try_get::<Option<String>, _>("target_id").unwrap_or_default(),
                    "detail_json": row.try_get::<Option<String>, _>("detail_json").unwrap_or_default(),
                })
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// Admin Endpoints (/admin/api/*)
// ---------------------------------------------------------------------------

pub(crate) async fn get_admin_audit_log_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AuditLogQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    // The audit chain is ordered by `seq DESC`, so its cursor is a plain `seq < ?` — the newest
    // row of the previous page is the exclusive upper bound of the next. No composite key and no
    // timestamp tie-breaking, unlike the usage endpoints (`ISSUE-0144`).
    let rows = bind_audit_log_filters!(
        sqlx::query(audit_log_query!(
            "SELECT seq, ts, actor_user_id, ip_addr, action, target_type, target_id, detail_json,
                    hash AS hash_hex
             FROM audit_log
             WHERE (? IS NULL OR ts >= ?)
               AND (? IS NULL OR ts <= ?)
               AND (? IS NULL OR seq < ?)",
            "ORDER BY seq DESC LIMIT ?"
        ))
        .bind(query.from)
        .bind(query.from)
        .bind(query.to)
        .bind(query.to)
        .bind(query.cursor)
        .bind(query.cursor),
        query
    )
    .bind(limit)
    .fetch_all(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut list = Vec::new();
    for r in rows {
        list.push(serde_json::json!({
            "seq": r.try_get::<i64, _>("seq").unwrap_or_default(),
            "ts": r.try_get::<i64, _>("ts").unwrap_or_default(),
            "actor_user_id": r.try_get::<Option<String>, _>("actor_user_id").unwrap_or_default(),
            // Nullable `TEXT` (`0001_initial.sql:561`), so it renders as-is or as `None`.
            "ip_addr": r.try_get::<Option<String>, _>("ip_addr").unwrap_or_default(),
            "action": r.try_get::<String, _>("action").unwrap_or_default(),
            "target_type": r.try_get::<String, _>("target_type").unwrap_or_default(),
            "target_id": r.try_get::<Option<String>, _>("target_id").unwrap_or_default(),
            "payload_json": r.try_get::<Option<String>, _>("detail_json").unwrap_or_default(),
            "detail_json": r.try_get::<Option<String>, _>("detail_json").unwrap_or_default(),
            "hash": r.try_get::<String, _>("hash_hex").unwrap_or_default(),
        }));
    }

    Ok(Json(list))
}

// ---------------------------------------------------------------------------
// Active Pins & Sessions (/api/user/pins, /admin/api/pins)
// ---------------------------------------------------------------------------
pub(crate) async fn get_user_logs_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LogsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let uid = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    get_logs_internal(&state, Some(uid.to_string()), query).await
}

pub(crate) async fn get_admin_logs_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LogsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_logs_internal(&state, None, query).await
}

/// The four priced token classes of a `usage_events` row, each preserved as `Option`.
///
/// `None` means the column was SQL `NULL` — the provider never reported it. `Some(0)` means it
/// reported zero. The distinction is load-bearing for display: a rejected request that consumed
/// nothing is worth showing as zero, while a row with no recorded counts must render `—`
/// (`ISSUE-0173`).
///
/// `reasoning_tokens` is not a member: it is a subset of `output` (`12. SRDD:133-134`), so it is
/// never part of a total. The log-detail drawer shows it per row from `usage_events` directly.
#[derive(Debug, Clone, Copy, Default)]
struct TokenClasses {
    input_uncached: Option<i64>,
    cache_read: Option<i64>,
    cache_write: Option<i64>,
    output: Option<i64>,
}

impl TokenClasses {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        let get = |col: &str| row.try_get::<Option<i64>, _>(col).unwrap_or(None);
        Self {
            input_uncached: get("input_uncached_tokens"),
            cache_read: get("cache_read_tokens"),
            cache_write: get("cache_write_tokens"),
            output: get("output_tokens"),
        }
    }

    /// The canonical token total: `input_uncached + cache_read + cache_write + output`.
    ///
    /// This is the number the rest of the control plane already reports — the Usage Ledger's
    /// aggregate, the overview KPI query, and `llmroller_core::pricing::calculate_cost_micro_usd`,
    /// which prices exactly these four classes. Reusing it is the point: the bug was two
    /// projections each owning their own sum (`ISSUE-0173`).
    ///
    /// **`reasoning` is excluded on purpose.** It is a *subset of* `output`, not a fifth class:
    /// `TokenUsage::reasoning` is documented as a "diagnostic subset of `output`", and
    /// `12. SRDD:133-134` records that `output_tokens` "includes reasoning". Adding it would
    /// double-count and overstate volume.
    ///
    /// `None` propagates when no class was recorded at all, so an absent row stays absent rather
    /// than becoming a zero.
    fn total(&self) -> Option<i64> {
        let classes = [
            self.input_uncached,
            self.cache_read,
            self.cache_write,
            self.output,
        ];
        classes
            .iter()
            .any(Option::is_some)
            .then(|| classes.iter().flatten().sum())
    }

    /// Render the total, or `—` when the row recorded no token data at all.
    fn formatted_or_dash(&self, format: impl FnOnce(i64) -> String) -> String {
        self.total().map_or_else(|| "—".to_string(), format)
    }
}

/// Human label for the status column.
///
/// A 5xx is an **upstream fault**, not a decision about the caller's request, so it is `Failed`
/// rather than `Rejected` (`ISSUE-0173`). `14.l` groups both under one colour but leaves the
/// wording to the host; conflating them sends an operator looking at policy when the provider is
/// the problem.
fn status_badge(status: i64) -> String {
    match status {
        200 => "200".to_string(),
        500..=599 => format!("{status} Failed"),
        _ => format!("{status} Rejected"),
    }
}

pub(crate) async fn get_logs_internal(
    state: &ApiAppState,
    actor_user_id: Option<String>,
    query: LogsQuery,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let is_api = query.stream.as_deref() == Some("api");
    let limit = query.limit.unwrap_or(50).clamp(1, 1000);

    // The retention window each stream may claim, loaded once so the two branches cannot
    // disagree (`ISSUE-0133`).
    //
    // The two streams describe *different tables*, and only one of them is pruned:
    //
    // - `inference` reads `usage_events`, which the live maintenance loop sweeps at exactly
    //   `usage_events_retention_days` (`cli/serve.rs`, `retention.rs:296`).
    // - `api` reads `control_plane_logs`, which **nothing prunes in the live path**. The
    //   maintenance loop does not touch it, and `run_retention_pass` — which does, at a fixed
    //   90 days — has no production caller. So its window is unbounded, and `None` says that.
    //
    // Reporting `30` (the old hardcoded value) claimed a policy that was not in force. The
    // `.max(31)` in `run_retention_pass` clamps the *rollup* cutoff, not this table, so it is
    // not the number this response should carry either.
    let settings = llmroller_store::settings::load_settings(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let usage_events_retention_days = Some(settings.usage_events_retention_days.max(1));

    if is_api {
        let sql = if actor_user_id.is_some() {
            "SELECT id, ts, request_id, actor_user_id, workspace_scope, method, path, status, latency_ms, ip_addr, user_agent
             FROM control_plane_logs
             WHERE actor_user_id = ?
               AND (? IS NULL OR ts >= ?)
               AND (? IS NULL OR ts <= ?)
               AND (? IS NULL OR status = ?)
               AND (? IS NULL OR method = ?)
               AND (? IS NULL OR ts < ?)
             ORDER BY ts DESC LIMIT ?"
        } else {
            "SELECT id, ts, request_id, actor_user_id, workspace_scope, method, path, status, latency_ms, ip_addr, user_agent
             FROM control_plane_logs
             WHERE (? IS NULL OR ts >= ?)
               AND (? IS NULL OR ts <= ?)
               AND (? IS NULL OR status = ?)
               AND (? IS NULL OR method = ?)
               AND (? IS NULL OR ts < ?)
             ORDER BY ts DESC LIMIT ?"
        };
        let cursor_ts = query.cursor.as_deref().and_then(|c| c.parse::<i64>().ok());

        let rows = if let Some(ref uid) = actor_user_id {
            sqlx::query(sql)
                .bind(uid)
                .bind(query.from)
                .bind(query.from)
                .bind(query.to)
                .bind(query.to)
                .bind(query.status)
                .bind(query.status)
                .bind(query.method.as_deref())
                .bind(query.method.as_deref())
                .bind(cursor_ts)
                .bind(cursor_ts)
                .bind(limit)
                .fetch_all(&state.auth.reader_pool)
                .await
        } else {
            sqlx::query(sql)
                .bind(query.from)
                .bind(query.from)
                .bind(query.to)
                .bind(query.to)
                .bind(query.status)
                .bind(query.status)
                .bind(query.method.as_deref())
                .bind(query.method.as_deref())
                .bind(cursor_ts)
                .bind(cursor_ts)
                .bind(limit)
                .fetch_all(&state.auth.reader_pool)
                .await
        }
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

        let logs = rows
            .into_iter()
            .map(|r| {
                let status: i64 = r.try_get("status").unwrap_or(200);
                let latency_ms: i64 = r.try_get("latency_ms").unwrap_or(0);
                serde_json::json!({
                    "id": r.try_get::<String, _>("id").unwrap_or_default(),
                    "request_id": r.try_get::<String, _>("request_id").unwrap_or_default(),
                    "ts": r.try_get::<i64, _>("ts").unwrap_or(0),
                    "actor_user_id": r.try_get::<Option<String>, _>("actor_user_id").unwrap_or_default(),
                    "workspace_scope": r.try_get::<String, _>("workspace_scope").unwrap_or_default(),
                    "method": r.try_get::<String, _>("method").unwrap_or_default(),
                    "path": r.try_get::<String, _>("path").unwrap_or_default(),
                    "endpoint": r.try_get::<String, _>("path").unwrap_or_default(),
                    "status": status,
                    "latency_ms": latency_ms,
                    "duration_formatted": format!("{latency_ms} ms"),
                    "ip_addr": r.try_get::<Option<String>, _>("ip_addr").unwrap_or_default(),
                    "user_agent": r.try_get::<Option<String>, _>("user_agent").unwrap_or_default(),
                })
            })
            .collect::<Vec<_>>();

        return Ok(Json(serde_json::json!({
            "stream": "api",
            // Nothing prunes `control_plane_logs` in the live maintenance loop, so there is
            // no window to report. `null` is the honest value; `retention_scope` names which
            // table the window would apply to, so the absence cannot be mistaken for a
            // missing field.
            "retention_days": serde_json::Value::Null,
            "retention_scope": "control_plane_logs",
            "rows": logs,
        })));
    }

    // Default stream: inference
    let sql = if actor_user_id.is_some() {
        "SELECT e.id, e.ts, e.request_id, e.client_key_id, e.owner_user_id, e.canonical_id, e.endpoint, e.protocol,
                e.session_hash, e.status, e.stream, e.incomplete, e.termination, e.input_uncached_tokens,
                e.cache_read_tokens, e.cache_write_tokens, e.output_tokens, e.reasoning_tokens,
                e.cost_micro_usd, e.latency_ms, e.ttfb_ms, e.stop_reason, e.service_account_id,
                e.error_code, e.pin_outcome, e.failovers, ck.label AS client_key_label
         FROM usage_events e
         LEFT JOIN client_keys ck ON e.client_key_id = ck.id
         WHERE e.owner_user_id = ?
           AND (? IS NULL OR e.ts >= ?)
           AND (? IS NULL OR e.ts <= ?)
           AND (? IS NULL OR e.canonical_id = ?)
           AND (? IS NULL OR e.status = ?)
           AND (? IS NULL OR e.client_key_id = ?)
           AND (? IS NULL OR e.service_account_id = ?)
           AND (? IS NULL OR e.id < ?)
         ORDER BY e.id DESC LIMIT ?"
    } else {
        "SELECT e.id, e.ts, e.request_id, e.client_key_id, e.owner_user_id, e.canonical_id, e.endpoint, e.protocol,
                e.session_hash, e.status, e.stream, e.incomplete, e.termination, e.input_uncached_tokens,
                e.cache_read_tokens, e.cache_write_tokens, e.output_tokens, e.reasoning_tokens,
                e.cost_micro_usd, e.latency_ms, e.ttfb_ms, e.stop_reason, e.service_account_id,
                e.error_code, e.pin_outcome, e.failovers, ck.label AS client_key_label
         FROM usage_events e
         LEFT JOIN client_keys ck ON e.client_key_id = ck.id
         WHERE (? IS NULL OR e.ts >= ?)
           AND (? IS NULL OR e.ts <= ?)
           AND (? IS NULL OR e.canonical_id = ?)
           AND (? IS NULL OR e.status = ?)
           AND (? IS NULL OR e.client_key_id = ?)
           AND (? IS NULL OR e.service_account_id = ?)
           AND (? IS NULL OR e.id < ?)
         ORDER BY e.id DESC LIMIT ?"
    };

    let rows = if let Some(ref uid) = actor_user_id {
        sqlx::query(sql)
            .bind(uid)
            .bind(query.from)
            .bind(query.from)
            .bind(query.to)
            .bind(query.to)
            .bind(query.model.as_deref())
            .bind(query.model.as_deref())
            .bind(query.status)
            .bind(query.status)
            .bind(query.client_key_id.as_deref())
            .bind(query.client_key_id.as_deref())
            .bind(query.service_account_id.as_deref())
            .bind(query.service_account_id.as_deref())
            .bind(query.cursor.as_deref())
            .bind(query.cursor.as_deref())
            .bind(limit)
            .fetch_all(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(sql)
            .bind(query.from)
            .bind(query.from)
            .bind(query.to)
            .bind(query.to)
            .bind(query.model.as_deref())
            .bind(query.model.as_deref())
            .bind(query.status)
            .bind(query.status)
            .bind(query.client_key_id.as_deref())
            .bind(query.client_key_id.as_deref())
            .bind(query.service_account_id.as_deref())
            .bind(query.service_account_id.as_deref())
            .bind(query.cursor.as_deref())
            .bind(query.cursor.as_deref())
            .bind(limit)
            .fetch_all(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let logs = rows
        .into_iter()
        .map(|r| {
            let status: i64 = r.try_get("status").unwrap_or(200);
            let cost: i64 = r.try_get("cost_micro_usd").unwrap_or(0);
            let latency_ms: Option<i64> = r.try_get("latency_ms").unwrap_or(None);
            // Kept as `Option` all the way from the row: the token columns are nullable
            // `INTEGER`s, so `NULL` ("the provider never told us") is a different fact from `0`
            // ("the provider told us zero"). Collapsing them here is what forced the old
            // `status == 200` guard, which then hid every charged rejection (`ISSUE-0173`).
            let tokens = TokenClasses::from_row(&r);
            let total_tokens = tokens.total();
            let status_badge = status_badge(status);
            let tokens_formatted = tokens.formatted_or_dash(format_tokens_compact);
            // `cost_micro_usd` is `NOT NULL` in the schema, so there is no absent case to
            // distinguish: a zero here is a real zero and renders as `$0.00000`.
            let cost_formatted = format_cost_usd(cost);

            serde_json::json!({
                "id": r.try_get::<String, _>("id").unwrap_or_default(),
                "request_id": r.try_get::<String, _>("request_id").unwrap_or_default(),
                "ts": r.try_get::<i64, _>("ts").unwrap_or(0),
                "status": status,
                "status_badge": status_badge,
                "canonical_id": r.try_get::<Option<String>, _>("canonical_id").unwrap_or_default(),
                "model": r.try_get::<Option<String>, _>("canonical_id").unwrap_or_default(),
                "client_key_id": r.try_get::<String, _>("client_key_id").unwrap_or_default(),
                "client_key_label": r.try_get::<Option<String>, _>("client_key_label").unwrap_or_default(),
                "service_account_id": r.try_get::<Option<String>, _>("service_account_id").unwrap_or_default(),
                "session_hash": r.try_get::<Option<String>, _>("session_hash").unwrap_or_default(),
                "endpoint": r.try_get::<String, _>("endpoint").unwrap_or_default(),
                "protocol": r.try_get::<Option<String>, _>("protocol").unwrap_or_default(),
                "stream": r.try_get::<i64, _>("stream").unwrap_or(0) == 1,
                "latency_ms": latency_ms,
                "duration_formatted": latency_ms.map(|ms| format!("{ms} ms")).unwrap_or_else(|| "—".to_string()),
                "total_tokens": total_tokens,
                "tokens_formatted": tokens_formatted,
                "cost_micro_usd": cost,
                "cost_formatted": cost_formatted,
                "error_code": r.try_get::<Option<String>, _>("error_code").unwrap_or_default(),
                "stop_reason": r.try_get::<Option<String>, _>("stop_reason").unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({
        "stream": "inference",
        "retention_days": usage_events_retention_days,
        "retention_scope": "usage_events",
        "rows": logs,
    })))
}

pub(crate) async fn get_user_log_detail_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    get_log_detail_internal(&state, &actor, &request_id).await
}

pub(crate) async fn get_admin_log_detail_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    get_log_detail_internal(&state, &actor, &request_id).await
}

/// Shape the sanitized `request_headers_json` for the drawer's headers grid.
///
/// `SEC-6`/`USG-9` guarantee the stored blob is an allow-list of diagnostic headers, never
/// credentials — so this is not a confidentiality boundary, only a parsing one.
///
/// Two decisions:
///
/// - **A corrupt blob is not emptied.** Returning `{}` (the `unwrap_or_default()` reflex) would
///   render "no headers recorded" for a value that exists and is damaged, which is the
///   silent-empty failure mode this project keeps having to fix. Instead the raw text is preserved
///   and `parse_error` is set, so the damage is visible in the UI.
/// - **Pairs, not a map.** The grid renders `name → value` rows; an array of pairs keeps document
///   order (JSON objects are unordered, and a user scanning headers expects the order they were
///   captured in) and avoids the "what if two keys differ only by case" question.
///
/// `None` (never captured) and the empty string are genuinely "no headers" and yield an empty
/// pair list with `parse_error: false` — absent is not corrupt.
fn parse_headers_blob(raw: Option<&str>) -> serde_json::Value {
    let Some(text) = raw.filter(|t| !t.trim().is_empty()) else {
        return serde_json::json!({ "pairs": [], "parse_error": false, "raw": null });
    };

    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => {
            let pairs: Vec<(String, String)> = map
                .into_iter()
                .map(|(k, v)| {
                    let rendered = match v {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (k, rendered)
                })
                .collect();
            serde_json::json!({ "pairs": pairs, "parse_error": false, "raw": null })
        }
        // Valid JSON but not an object, or not JSON at all: both are corruption from this
        // renderer's point of view, and both keep the raw text.
        _ => serde_json::json!({ "pairs": [], "parse_error": true, "raw": text }),
    }
}

pub(crate) async fn get_log_detail_internal(
    state: &ApiAppState,
    actor: &ActorScope,
    request_id: &str,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let is_admin = actor.is_admin() || actor.is_system();
    let uid = actor.user_id().map(|u| u.to_string());

    let sql = if !is_admin {
        "SELECT * FROM usage_events WHERE request_id = ? AND owner_user_id = ?"
    } else {
        "SELECT * FROM usage_events WHERE request_id = ?"
    };

    let row = if !is_admin {
        sqlx::query(sql)
            .bind(request_id)
            .bind(uid.as_deref().unwrap_or_default())
            .fetch_optional(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(sql)
            .bind(request_id)
            .fetch_optional(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "log record not found"))?;

    let attempt_rows = sqlx::query(
        "SELECT attempt_no, ts, upstream_key_id, share_id, outcome, http_status, bytes_sent, charged_micro_usd, charge_basis
         FROM usage_attempts WHERE request_id = ? ORDER BY attempt_no ASC",
    )
    .bind(request_id)
    .fetch_all(&state.auth.reader_pool)
    .await
    .unwrap_or_default();

    let event_provider_scope: String = row
        .try_get::<Option<String>, _>("provider_scope")
        .unwrap_or_default()
        .unwrap_or_default();
    let is_own_personal = matches!(event_provider_scope.as_str(), "personal" | "own");

    let attempts = attempt_rows
        .into_iter()
        .map(|ar| {
            let kid: String = ar.try_get("upstream_key_id").unwrap_or_default();
            let share_id: Option<String> = ar.try_get("share_id").unwrap_or_default();
            let kid_projected = if is_admin || (is_own_personal && share_id.is_none()) {
                Some(kid)
            } else if share_id.is_some() {
                Some("shared pool".to_string())
            } else {
                Some("global pool".to_string())
            };

            serde_json::json!({
                "attempt_no": ar.try_get::<i64, _>("attempt_no").unwrap_or(1),
                "ts": ar.try_get::<i64, _>("ts").unwrap_or(0),
                "upstream_key_id": kid_projected,
                "share_id": share_id,
                "outcome": ar.try_get::<String, _>("outcome").unwrap_or_default(),
                "http_status": ar.try_get::<Option<i64>, _>("http_status").unwrap_or_default(),
                "bytes_sent": ar.try_get::<i64, _>("bytes_sent").unwrap_or(0),
                "charged_micro_usd": ar.try_get::<i64, _>("charged_micro_usd").unwrap_or(0),
                "charge_basis": ar.try_get::<String, _>("charge_basis").unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();

    let has_trace: bool =
        sqlx::query_scalar::<_, i64>("SELECT 1 FROM trace_records WHERE request_id = ? LIMIT 1")
            .bind(request_id)
            .fetch_optional(&state.auth.reader_pool)
            .await
            .unwrap_or_default()
            .is_some();

    let status: i64 = row.try_get("status").unwrap_or(200);
    let cost: i64 = row.try_get("cost_micro_usd").unwrap_or(0);
    let list_cost: i64 = row.try_get("list_cost_micro_usd").unwrap_or(cost);
    let latency_ms: Option<i64> = row.try_get("latency_ms").unwrap_or(None);
    let ttfb_ms: Option<i64> = row.try_get("ttfb_ms").unwrap_or(None);

    Ok(Json(serde_json::json!({
        "id": row.try_get::<String, _>("id").unwrap_or_default(),
        "request_id": request_id,
        "ts": row.try_get::<i64, _>("ts").unwrap_or(0),
        "status": status,
        "stop_reason": row.try_get::<Option<String>, _>("stop_reason").unwrap_or_default(),
        "endpoint": row.try_get::<String, _>("endpoint").unwrap_or_default(),
        "protocol": row.try_get::<Option<String>, _>("protocol").unwrap_or_default(),
        "stream": row.try_get::<i64, _>("stream").unwrap_or(0) == 1,
        "latency_ms": latency_ms,
        "ttfb_ms": ttfb_ms,
        "model": row.try_get::<Option<String>, _>("canonical_id").unwrap_or_default(),
        "tokens": {
            "input": row.try_get::<Option<i64>, _>("input_uncached_tokens").unwrap_or(None).unwrap_or(0),
            "output": row.try_get::<Option<i64>, _>("output_tokens").unwrap_or(None).unwrap_or(0),
            "reasoning": row.try_get::<Option<i64>, _>("reasoning_tokens").unwrap_or(None).unwrap_or(0),
            "cache_read": row.try_get::<Option<i64>, _>("cache_read_tokens").unwrap_or(None).unwrap_or(0),
            "cache_write": row.try_get::<Option<i64>, _>("cache_write_tokens").unwrap_or(None).unwrap_or(0),
        },
        "cost": {
            "cost_micro_usd": cost,
            "list_cost_micro_usd": list_cost,
            "plan_multiplier_bps": row.try_get::<i64, _>("plan_multiplier_bps").unwrap_or(10_000),
            "billing_coverage_mode": row.try_get::<String, _>("billing_coverage_mode").unwrap_or_else(|_| "pay_as_you_go".to_string()),
        },
        "identity": {
            "client_key_id": row.try_get::<String, _>("client_key_id").unwrap_or_default(),
            "service_account_id": row.try_get::<Option<String>, _>("service_account_id").unwrap_or_default(),
            "owner_user_id": row.try_get::<String, _>("owner_user_id").unwrap_or_default(),
        },
        "session_hash": row.try_get::<Option<String>, _>("session_hash").unwrap_or_default(),
        "client_user_agent": row.try_get::<Option<String>, _>("client_user_agent").unwrap_or_default(),
        // `14.m` section 5 "Location". Geo headers are gated on trusted proxies (`#158`), so this
        // is legitimately absent for most deployments; the renderer shows a dash, never an
        // invented value.
        "client_location": row.try_get::<Option<String>, _>("client_location").unwrap_or_default(),
        // Enum-valued, CHECK-constrained columns, returned as the **stored strings**
        // (`0001_initial.sql:419-420`, `:432-433`, `:442-443`). No remapping to display labels:
        // the vocabulary is the column's, and a consumer comparing against it must not see our
        // invented synonyms.
        "termination": row.try_get::<String, _>("termination").unwrap_or_else(|_| "completed".to_string()),
        "cost_source": row.try_get::<String, _>("cost_source").unwrap_or_default(),
        "pin_outcome": row.try_get::<Option<String>, _>("pin_outcome").unwrap_or_default(),
        // `incomplete` is `INTEGER` 0/1; a JSON boolean is what a renderer wants.
        "incomplete": row.try_get::<i64, _>("incomplete").unwrap_or(0) != 0,
        "failovers": row.try_get::<i64, _>("failovers").unwrap_or(0),
        "error_code": row.try_get::<Option<String>, _>("error_code").unwrap_or_default(),
        "request_headers": parse_headers_blob(
            row.try_get::<Option<String>, _>("request_headers_json")
                .unwrap_or_default()
                .as_deref(),
        ),
        "metadata_json": row.try_get::<Option<String>, _>("metadata_json").unwrap_or_default(),
        // `trace_records.usage_event_id` is the PK referencing `usage_events(id)`
        // (`0001_initial.sql:529`), and its `request_id` is UNIQUE, so this is the same single
        // lookup `has_trace` already does. Traces are opt-in (`ADR-9`), so `null` is the common
        // case and an honest one: a dead link in an inspector is worse than no link.
        "trace_href": has_trace.then(|| format!("/admin/api/traces/{request_id}")),
        "attempts": attempts,
        "has_trace": has_trace,
    })))
}

/// Append the truncation footer to a CSV body when the row cap was hit.
///
/// A `#`-prefixed line is not a data row for any spreadsheet importer or `pandas.read_csv` with
/// comment handling, so it cannot be mistaken for a record. It is written **last** so a streaming
/// consumer that stops early still sees the data, and its presence is the only signal an operator
/// gets that the file is a prefix rather than the whole filtered set (`ISSUE-0143`).
///
/// Silent truncation is the same class of defect as an ignored filter: the artefact claims a
/// completeness it does not have, and acts on that claim elsewhere.
fn push_truncation_footer(csv: &mut String, truncated: bool, cap: i64) {
    if truncated {
        csv.push_str(&format!(
            "# TRUNCATED: export limited to {cap} rows; more rows matched the filter. \
             Narrow the date range or filters to retrieve the remainder.\r\n"
        ));
    }
}

pub(crate) async fn get_user_logs_export_csv_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LogsQuery>,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let uid = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    export_logs_csv_internal(&state, Some(uid.to_string()), query).await
}

pub(crate) async fn get_admin_logs_export_csv_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LogsQuery>,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    export_logs_csv_internal(&state, None, query).await
}

pub(crate) async fn export_logs_csv_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    query: LogsQuery,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_proto::headers::sanitize_csv_cell;

    let is_api = query.stream.as_deref() == Some("api");
    // The cap stays: an uncapped export is a memory and disk hazard. It is now *reported* rather
    // than silent, because a filtered export that quietly stops at N rows claims a completeness it
    // does not have (`ISSUE-0143`).
    //
    // One extra row is fetched beyond the cap so truncation is detected exactly — a result of
    // precisely `cap` rows is complete, and fetching `cap` alone could not distinguish the two.
    let cap = query.limit.unwrap_or(5_000).clamp(1, 5_000);
    let fetch = cap.saturating_add(1);
    if is_api {
        let sql = if owner_user_id.is_some() {
            api_logs_export_query!(
                "SELECT id, ts, request_id, method, path, status, latency_ms, ip_addr
                 FROM control_plane_logs
                 WHERE actor_user_id = ?
                   AND (? IS NULL OR ts >= ?)
                   AND (? IS NULL OR ts <= ?)",
                "ORDER BY ts DESC LIMIT ?"
            )
        } else {
            api_logs_export_query!(
                "SELECT id, ts, request_id, method, path, status, latency_ms, ip_addr
                 FROM control_plane_logs
                 WHERE (? IS NULL OR ts >= ?)
                   AND (? IS NULL OR ts <= ?)",
                "ORDER BY ts DESC LIMIT ?"
            )
        };
        let rows = if let Some(ref uid) = owner_user_id {
            bind_api_logs_export_filters!(
                sqlx::query(sql)
                    .bind(uid)
                    .bind(query.from)
                    .bind(query.from)
                    .bind(query.to)
                    .bind(query.to),
                query
            )
            .bind(fetch)
            .fetch_all(&state.auth.reader_pool)
            .await
        } else {
            bind_api_logs_export_filters!(
                sqlx::query(sql)
                    .bind(query.from)
                    .bind(query.from)
                    .bind(query.to)
                    .bind(query.to),
                query
            )
            .bind(fetch)
            .fetch_all(&state.auth.reader_pool)
            .await
        }
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

        let truncated = rows.len() > cap as usize;
        let mut csv = String::from("id,ts,request_id,method,path,status,duration_ms,ip_addr\r\n");
        for r in rows.into_iter().take(cap as usize) {
            let id: String = r.try_get("id").unwrap_or_default();
            let ts: i64 = r.try_get("ts").unwrap_or(0);
            let req_id: String = r.try_get("request_id").unwrap_or_default();
            let method: String = r.try_get("method").unwrap_or_default();
            let path: String = r.try_get("path").unwrap_or_default();
            let status: i64 = r.try_get("status").unwrap_or(0);
            let latency: i64 = r.try_get("latency_ms").unwrap_or(0);
            let ip: String = r
                .try_get::<Option<String>, _>("ip_addr")
                .unwrap_or_default()
                .unwrap_or_default();
            let line = format!(
                "{},{},{},{},{},{},{},{}\r\n",
                sanitize_csv_cell(&id),
                ts,
                sanitize_csv_cell(&req_id),
                sanitize_csv_cell(&method),
                sanitize_csv_cell(&path),
                status,
                latency,
                sanitize_csv_cell(&ip),
            );
            csv.push_str(&line);
        }
        push_truncation_footer(&mut csv, truncated, cap);

        return axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")
            .header(
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"api_logs.csv\"",
            )
            .body(axum::body::Body::from(csv))
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e));
    }

    let sql = if owner_user_id.is_some() {
        logs_export_query!(
            "SELECT id, ts, request_id, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, output_tokens, error_code
             FROM usage_events
             WHERE owner_user_id = ?
               AND (? IS NULL OR ts >= ?)
               AND (? IS NULL OR ts <= ?)",
            "ORDER BY id DESC LIMIT ?"
        )
    } else {
        logs_export_query!(
            "SELECT id, ts, request_id, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, output_tokens, error_code
             FROM usage_events
             WHERE (? IS NULL OR ts >= ?)
               AND (? IS NULL OR ts <= ?)",
            "ORDER BY id DESC LIMIT ?"
        )
    };

    let rows = if let Some(ref uid) = owner_user_id {
        bind_logs_export_filters!(
            sqlx::query(sql)
                .bind(uid)
                .bind(query.from)
                .bind(query.from)
                .bind(query.to)
                .bind(query.to),
            query
        )
        .bind(fetch)
        .fetch_all(&state.auth.reader_pool)
        .await
    } else {
        bind_logs_export_filters!(
            sqlx::query(sql)
                .bind(query.from)
                .bind(query.from)
                .bind(query.to)
                .bind(query.to),
            query
        )
        .bind(fetch)
        .fetch_all(&state.auth.reader_pool)
        .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let truncated = rows.len() > cap as usize;
    let mut csv = String::from(
        "id,ts,request_id,canonical_id,status,cost_micro_usd,input_tokens,output_tokens,error_code\r\n",
    );
    for r in rows.into_iter().take(cap as usize) {
        let id: String = r.try_get("id").unwrap_or_default();
        let ts: i64 = r.try_get("ts").unwrap_or(0);
        let req_id: String = r.try_get("request_id").unwrap_or_default();
        let model: String = r
            .try_get::<Option<String>, _>("canonical_id")
            .unwrap_or_default()
            .unwrap_or_default();
        let status: i64 = r.try_get("status").unwrap_or(0);
        let cost: i64 = r.try_get("cost_micro_usd").unwrap_or(0);
        let input: i64 = r
            .try_get::<Option<i64>, _>("input_uncached_tokens")
            .unwrap_or(None)
            .unwrap_or(0);
        let output: i64 = r
            .try_get::<Option<i64>, _>("output_tokens")
            .unwrap_or(None)
            .unwrap_or(0);
        let err: String = r
            .try_get::<Option<String>, _>("error_code")
            .unwrap_or_default()
            .unwrap_or_default();
        let line = format!(
            "{},{},{},{},{},{},{},{},{}\r\n",
            sanitize_csv_cell(&id),
            ts,
            sanitize_csv_cell(&req_id),
            sanitize_csv_cell(&model),
            status,
            cost,
            input,
            output,
            sanitize_csv_cell(&err),
        );
        csv.push_str(&line);
    }
    push_truncation_footer(&mut csv, truncated, cap);

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            "attachment; filename=\"inference_logs.csv\"",
        )
        .body(axum::body::Body::from(csv))
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct LedgerQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub model: Option<String>,
    pub initiator: Option<String>,
    pub limit: Option<i64>,
}
