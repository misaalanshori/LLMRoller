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
        let rows = sqlx::query(
        "SELECT seq, ts, actor_user_id, ip_addr, action, target_type, target_id, detail_json
         FROM audit_log
         WHERE (actor_user_id = ?
                OR (target_type = 'user' AND target_id = ?)
                OR (target_type = 'share' AND target_id IN (
                    SELECT id FROM provider_shares WHERE owner_user_id = ? OR grantee_user_id = ?
                )))
           AND (? IS NULL OR action = ?)
           AND (? IS NULL OR target_type = ?)
           AND (? IS NULL OR ts >= ?)
           AND (? IS NULL OR ts <= ?)
           AND (? IS NULL OR seq < ?)
         ORDER BY seq DESC LIMIT ?",
    )
    .bind(user_id.to_string())
    .bind(user_id.to_string())
    .bind(user_id.to_string())
    .bind(user_id.to_string())
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
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

        let rows = sqlx::query(
        "SELECT seq, ts, actor_user_id, action, target_type, target_id, detail_json, hash AS hash_hex FROM audit_log ORDER BY seq DESC LIMIT 100"
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut list = Vec::new();
    for r in rows {
        list.push(serde_json::json!({
            "seq": r.try_get::<i64, _>("seq").unwrap_or_default(),
            "ts": r.try_get::<i64, _>("ts").unwrap_or_default(),
            "actor_user_id": r.try_get::<Option<String>, _>("actor_user_id").unwrap_or_default(),
            "action": r.try_get::<String, _>("action").unwrap_or_default(),
            "target_type": r.try_get::<String, _>("target_type").unwrap_or_default(),
            "target_id": r.try_get::<Option<String>, _>("target_id").unwrap_or_default(),
            "payload_json": r.try_get::<String, _>("detail_json").unwrap_or_default(),
            "detail_json": r.try_get::<String, _>("detail_json").unwrap_or_default(),
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

pub(crate) async fn get_logs_internal(
    state: &ApiAppState,
    actor_user_id: Option<String>,
    query: LogsQuery,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        let is_api = query.stream.as_deref() == Some("api");
    let limit = query.limit.unwrap_or(50).clamp(1, 1000);

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
            "retention_days": 30,
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
            let input: i64 = r.try_get::<Option<i64>, _>("input_uncached_tokens").unwrap_or(None).unwrap_or(0);
            let output: i64 = r.try_get::<Option<i64>, _>("output_tokens").unwrap_or(None).unwrap_or(0);
            let total_tokens = input + output;

            serde_json::json!({
                "id": r.try_get::<String, _>("id").unwrap_or_default(),
                "request_id": r.try_get::<String, _>("request_id").unwrap_or_default(),
                "ts": r.try_get::<i64, _>("ts").unwrap_or(0),
                "status": status,
                "status_badge": if status == 200 { "200".to_string() } else { format!("{status} Rejected") },
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
                "tokens_formatted": if status == 200 { format_tokens_compact(total_tokens) } else { "—".to_string() },
                "cost_micro_usd": cost,
                "cost_formatted": if status == 200 { format_cost_usd(cost) } else { "—".to_string() },
                "error_code": r.try_get::<Option<String>, _>("error_code").unwrap_or_default(),
                "stop_reason": r.try_get::<Option<String>, _>("stop_reason").unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({
        "stream": "inference",
        "retention_days": 30,
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
        "request_headers_json": row.try_get::<Option<String>, _>("request_headers_json").unwrap_or_default(),
        "metadata_json": row.try_get::<Option<String>, _>("metadata_json").unwrap_or_default(),
        "attempts": attempts,
        "has_trace": has_trace,
    })))
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
    if is_api {
        let sql = if owner_user_id.is_some() {
            "SELECT id, ts, request_id, method, path, status, latency_ms, ip_addr
             FROM control_plane_logs WHERE actor_user_id = ? ORDER BY ts DESC LIMIT 5000"
        } else {
            "SELECT id, ts, request_id, method, path, status, latency_ms, ip_addr
             FROM control_plane_logs ORDER BY ts DESC LIMIT 5000"
        };
        let rows = if let Some(ref uid) = owner_user_id {
            sqlx::query(sql)
                .bind(uid)
                .fetch_all(&state.auth.reader_pool)
                .await
        } else {
            sqlx::query(sql).fetch_all(&state.auth.reader_pool).await
        }
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

        let mut csv = String::from("id,ts,request_id,method,path,status,duration_ms,ip_addr\r\n");
        for r in rows {
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
        "SELECT id, ts, request_id, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, output_tokens, error_code
         FROM usage_events WHERE owner_user_id = ? ORDER BY id DESC LIMIT 5000"
    } else {
        "SELECT id, ts, request_id, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, output_tokens, error_code
         FROM usage_events ORDER BY id DESC LIMIT 5000"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(sql)
            .bind(uid)
            .fetch_all(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(sql).fetch_all(&state.auth.reader_pool).await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut csv = String::from(
        "id,ts,request_id,canonical_id,status,cost_micro_usd,input_tokens,output_tokens,error_code\r\n",
    );
    for r in rows {
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
#[allow(dead_code)]
pub(crate) struct LedgerQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub model: Option<String>,
    pub initiator: Option<String>,
    pub limit: Option<i64>,
}

