use super::*;
use sqlx::Row as _;

pub(crate) async fn get_admin_overview_telemetry_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    let now_ms = state.auth.clock.now_ms();

    let settings = llmroller_store::settings::load_settings(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let retention_days = settings.traceability_config.retention_days;

    let daily_requests: i64 =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_events WHERE ts >= ?")
            .bind(now_ms - 86400 * 1000)
            .fetch_one(&state.auth.pool)
            .await
            .unwrap_or(0)
            .max(1000);

    let trace_bytes: i64 =
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(SUM(byte_len), 0) FROM trace_blocks")
            .fetch_one(&state.auth.pool)
            .await
            .unwrap_or(0);

    let projected_bytes =
        ((daily_requests as f64) * (retention_days as f64) * 600.0 * 1.3) + (trace_bytes as f64);
    let projected_disk_mb = (projected_bytes / (1024.0 * 1024.0) * 10.0).round() / 10.0;

    let unpriced_count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT r.model_pk) FROM model_routes r
         WHERE r.enabled = 1 AND NOT EXISTS (
            SELECT 1 FROM prices p WHERE p.upstream_model_id = r.upstream_model_id
         )",
    )
    .fetch_one(&state.auth.pool)
    .await
    .unwrap_or(0);

    let usage_events_count: i64 = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&state.auth.pool)
        .await
        .unwrap_or(0);

    let overview = get_overview_internal(
        &state,
        None,
        OverviewQuery {
            range: Some("7d".to_string()),
            since: None,
            until: None,
        },
    )
    .await?;

    let mut map = overview.as_object().cloned().unwrap_or_default();
    map.insert(
        "projected_disk_mb".to_string(),
        serde_json::json!(projected_disk_mb),
    );
    map.insert(
        "unpriced_models_count".to_string(),
        serde_json::json!(unpriced_count),
    );
    map.insert(
        "usage_events_count".to_string(),
        serde_json::json!(usage_events_count),
    );
    map.insert(
        "max_usage_events_rows".to_string(),
        serde_json::json!(50_000_000i64),
    );
    map.insert(
        "retention_days".to_string(),
        serde_json::json!(retention_days),
    );
    map.insert(
        "daily_requests".to_string(),
        serde_json::json!(daily_requests),
    );

    Ok(Json(serde_json::Value::Object(map)))
}

// ---------------------------------------------------------------------------
// Audit Log Viewer (/admin/api/audit-log)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Usage Explorer & CSV Export (USG-1..USG-3, SEC-6)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub(crate) struct UsageFilterParams {
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: Option<i64>,
    pub after: Option<String>,
}

pub(crate) async fn get_user_usage_summary_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    get_usage_summary_internal(&state, Some(user_id.to_string()), params).await
}

pub(crate) async fn get_admin_usage_summary_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_summary_internal(&state, None, params).await
}

pub(crate) async fn get_usage_summary_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    params: UsageFilterParams,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        // USG-1/USG-2: aggregate from the hourly rollups instead of scanning raw
    // `usage_events`; the rollups carry the same token/cost columns and are
    // written in the same transaction batch as the raw rows (`REL-3`).
    let rollup_sql = if owner_user_id.is_some() {
        "SELECT COALESCE(SUM(requests), 0) as total_requests,
                COALESCE(SUM(input_uncached_tokens), 0) as input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) as cached_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(cost_micro_usd), 0) as total_cost_micro_usd
         FROM usage_rollups_hourly
         WHERE owner_user_id = ?
           AND (? IS NULL OR hour_ms >= ?)
           AND (? IS NULL OR hour_ms <= ?)"
    } else {
        "SELECT COALESCE(SUM(requests), 0) as total_requests,
                COALESCE(SUM(input_uncached_tokens), 0) as input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) as cached_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(cost_micro_usd), 0) as total_cost_micro_usd
         FROM usage_rollups_hourly
         WHERE (? IS NULL OR hour_ms >= ?)
           AND (? IS NULL OR hour_ms <= ?)"
    };

    let row = if let Some(ref uid) = owner_user_id {
        sqlx::query(rollup_sql)
            .bind(uid)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_one(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(rollup_sql)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_one(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut total_requests: i64 = row.try_get("total_requests").unwrap_or(0);
    let mut input_tokens: i64 = row.try_get("input_tokens").unwrap_or(0);
    let mut cached_tokens: i64 = row.try_get("cached_tokens").unwrap_or(0);
    let mut output_tokens: i64 = row.try_get("output_tokens").unwrap_or(0);
    let mut total_cost_micro_usd: i64 = row.try_get("total_cost_micro_usd").unwrap_or(0);

    if total_requests == 0 {
        // Synthetic fixtures and freshly restored databases may have raw events
        // without rollups; keep the endpoint useful instead of returning zero.
        let event_sql = if owner_user_id.is_some() {
            "SELECT COUNT(*) as total_requests,
                    COALESCE(SUM(input_uncached_tokens), 0) as input_tokens,
                    COALESCE(SUM(cache_read_tokens), 0) as cached_tokens,
                    COALESCE(SUM(output_tokens), 0) as output_tokens,
                    COALESCE(SUM(cost_micro_usd), 0) as total_cost_micro_usd
             FROM usage_events
             WHERE owner_user_id = ?
               AND (? IS NULL OR ts >= ?)
               AND (? IS NULL OR ts <= ?)"
        } else {
            "SELECT COUNT(*) as total_requests,
                    COALESCE(SUM(input_uncached_tokens), 0) as input_tokens,
                    COALESCE(SUM(cache_read_tokens), 0) as cached_tokens,
                    COALESCE(SUM(output_tokens), 0) as output_tokens,
                    COALESCE(SUM(cost_micro_usd), 0) as total_cost_micro_usd
             FROM usage_events
             WHERE (? IS NULL OR ts >= ?)
               AND (? IS NULL OR ts <= ?)"
        };
        let fallback = if let Some(ref uid) = owner_user_id {
            sqlx::query(event_sql)
                .bind(uid)
                .bind(params.since)
                .bind(params.since)
                .bind(params.until)
                .bind(params.until)
                .fetch_one(&state.auth.reader_pool)
                .await
        } else {
            sqlx::query(event_sql)
                .bind(params.since)
                .bind(params.since)
                .bind(params.until)
                .bind(params.until)
                .fetch_one(&state.auth.reader_pool)
                .await
        }
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        total_requests = fallback.try_get("total_requests").unwrap_or(0);
        input_tokens = fallback.try_get("input_tokens").unwrap_or(0);
        cached_tokens = fallback.try_get("cached_tokens").unwrap_or(0);
        output_tokens = fallback.try_get("output_tokens").unwrap_or(0);
        total_cost_micro_usd = fallback.try_get("total_cost_micro_usd").unwrap_or(0);
    }

    let total_input = (input_tokens + cached_tokens).max(1) as f64;
    let cache_hit_rate = (cached_tokens as f64 / total_input * 1000.0).round() / 1000.0;

    Ok(Json(serde_json::json!({
        "total_requests": total_requests,
        "input_tokens": input_tokens,
        "cached_tokens": cached_tokens,
        "output_tokens": output_tokens,
        "total_cost_micro_usd": total_cost_micro_usd,
        "cache_hit_rate": cache_hit_rate,
    })))
}

/// `GET /api/user/usage/timeseries` (`USG-1`): hourly buckets from the rollups.
pub(crate) async fn get_user_usage_timeseries_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    get_usage_timeseries_internal(&state, Some(user_id.to_string()), params).await
}

pub(crate) async fn get_admin_usage_timeseries_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_timeseries_internal(&state, None, params).await
}

pub(crate) async fn get_usage_timeseries_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    params: UsageFilterParams,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        // Hourly buckets keep the payload small enough for the uPlot consumer while
    // still covering multi-week ranges (`USG-1`, `REL-3`).
    let sql = if owner_user_id.is_some() {
        "SELECT hour_ms AS bucket_ms,
                COALESCE(SUM(requests), 0) AS requests,
                COALESCE(SUM(cost_micro_usd), 0) AS cost_micro_usd,
                COALESCE(SUM(input_uncached_tokens), 0) AS input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) AS cached_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens
         FROM usage_rollups_hourly
         WHERE owner_user_id = ?
           AND (? IS NULL OR hour_ms >= ?)
           AND (? IS NULL OR hour_ms <= ?)
         GROUP BY hour_ms ORDER BY hour_ms ASC"
    } else {
        "SELECT hour_ms AS bucket_ms,
                COALESCE(SUM(requests), 0) AS requests,
                COALESCE(SUM(cost_micro_usd), 0) AS cost_micro_usd,
                COALESCE(SUM(input_uncached_tokens), 0) AS input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) AS cached_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens
         FROM usage_rollups_hourly
         WHERE (? IS NULL OR hour_ms >= ?)
           AND (? IS NULL OR hour_ms <= ?)
         GROUP BY hour_ms ORDER BY hour_ms ASC"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(sql)
            .bind(uid)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_all(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(sql)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_all(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let series = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "bucket_ms": row.try_get::<i64, _>("bucket_ms").unwrap_or(0),
                "requests": row.try_get::<i64, _>("requests").unwrap_or(0),
                "cost_micro_usd": row.try_get::<i64, _>("cost_micro_usd").unwrap_or(0),
                "input_tokens": row.try_get::<i64, _>("input_tokens").unwrap_or(0),
                "cached_tokens": row.try_get::<i64, _>("cached_tokens").unwrap_or(0),
                "output_tokens": row.try_get::<i64, _>("output_tokens").unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({ "series": series })))
}

/// `GET /api/user/usage/breakdown` (`USG-2`): per-model totals from the rollups.
pub(crate) async fn get_user_usage_breakdown_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    get_usage_breakdown_internal(&state, Some(user_id.to_string()), params).await
}

pub(crate) async fn get_admin_usage_breakdown_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_breakdown_internal(&state, None, params).await
}

pub(crate) async fn get_usage_breakdown_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    params: UsageFilterParams,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        let sql = if owner_user_id.is_some() {
        "SELECT canonical_id,
                COALESCE(SUM(requests), 0) AS requests,
                COALESCE(SUM(cost_micro_usd), 0) AS cost_micro_usd,
                COALESCE(SUM(input_uncached_tokens), 0) AS input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) AS cached_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens
         FROM usage_rollups_hourly
         WHERE owner_user_id = ?
           AND (? IS NULL OR hour_ms >= ?)
           AND (? IS NULL OR hour_ms <= ?)
         GROUP BY canonical_id ORDER BY cost_micro_usd DESC LIMIT 50"
    } else {
        "SELECT canonical_id,
                COALESCE(SUM(requests), 0) AS requests,
                COALESCE(SUM(cost_micro_usd), 0) AS cost_micro_usd,
                COALESCE(SUM(input_uncached_tokens), 0) AS input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) AS cached_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens
         FROM usage_rollups_hourly
         WHERE (? IS NULL OR hour_ms >= ?)
           AND (? IS NULL OR hour_ms <= ?)
         GROUP BY canonical_id ORDER BY cost_micro_usd DESC LIMIT 50"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(sql)
            .bind(uid)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_all(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(sql)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_all(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let breakdown = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "canonical_id": row.try_get::<String, _>("canonical_id").unwrap_or_default(),
                "requests": row.try_get::<i64, _>("requests").unwrap_or(0),
                "cost_micro_usd": row.try_get::<i64, _>("cost_micro_usd").unwrap_or(0),
                "input_tokens": row.try_get::<i64, _>("input_tokens").unwrap_or(0),
                "cached_tokens": row.try_get::<i64, _>("cached_tokens").unwrap_or(0),
                "output_tokens": row.try_get::<i64, _>("output_tokens").unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({ "breakdown": breakdown })))
}

/// `GET /api/user/usage/events/{id}/breakdown` (`USG-3`, `USG-10`, `F-070`).
pub(crate) async fn get_user_usage_event_breakdown_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    get_usage_event_breakdown_internal(&state, Some(user_id.to_string()), &id).await
}

/// `GET /admin/api/usage/events/{id}/breakdown` (`USG-3`, `USG-10`, `F-070`).
pub(crate) async fn get_admin_usage_event_breakdown_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_event_breakdown_internal(&state, None, &id).await
}

pub(crate) async fn get_usage_event_breakdown_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    event_id_or_request_id: &str,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_core::pricing::{ContextTier, Rates, Schedule, TokenUsage, div_ceil_i128};
    
    let sql = if owner_user_id.is_some() {
        "SELECT id, ts, request_id, canonical_id, upstream_key_id,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                cost_micro_usd, list_cost_micro_usd, plan_multiplier_bps, billing_coverage_mode
         FROM usage_events
         WHERE (id = ? OR request_id = ?) AND owner_user_id = ?
         LIMIT 1"
    } else {
        "SELECT id, ts, request_id, canonical_id, upstream_key_id,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                cost_micro_usd, list_cost_micro_usd, plan_multiplier_bps, billing_coverage_mode
         FROM usage_events
         WHERE (id = ? OR request_id = ?)
         LIMIT 1"
    };

    let row = if let Some(ref uid) = owner_user_id {
        sqlx::query(sql)
            .bind(event_id_or_request_id)
            .bind(event_id_or_request_id)
            .bind(uid)
            .fetch_optional(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(sql)
            .bind(event_id_or_request_id)
            .bind(event_id_or_request_id)
            .fetch_optional(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "usage event not found"))?;

    let ts: i64 = row.try_get("ts").unwrap_or(0);
    let canonical_id: String = row
        .try_get::<Option<String>, _>("canonical_id")
        .unwrap_or(None)
        .unwrap_or_default();
    let upstream_key_id: Option<String> = row.try_get("upstream_key_id").unwrap_or(None);
    let in_uncached = row
        .try_get::<Option<i64>, _>("input_uncached_tokens")
        .unwrap_or(None)
        .unwrap_or(0);
    let cache_read = row
        .try_get::<Option<i64>, _>("cache_read_tokens")
        .unwrap_or(None)
        .unwrap_or(0);
    let cache_write = row
        .try_get::<Option<i64>, _>("cache_write_tokens")
        .unwrap_or(None)
        .unwrap_or(0);
    let output = row
        .try_get::<Option<i64>, _>("output_tokens")
        .unwrap_or(None)
        .unwrap_or(0);
    let cost_micro_usd: i64 = row.try_get("cost_micro_usd").unwrap_or(0);
    let list_cost_micro_usd: i64 = row.try_get("list_cost_micro_usd").unwrap_or(cost_micro_usd);
    let plan_multiplier_bps: i64 = row.try_get("plan_multiplier_bps").unwrap_or(10_000);
    let billing_coverage_mode: String = row
        .try_get("billing_coverage_mode")
        .unwrap_or_else(|_| "pay_as_you_go".to_string());

    // Look up price entry (base rates + tiers_json + schedule_json) for this event (F-070)
    let mut base_rates = Rates::default();
    let mut tiers: Vec<ContextTier> = Vec::new();
    let mut schedules: Vec<Schedule> = Vec::new();
    let price_row = if let Some(ref uk_id) = upstream_key_id {
        sqlx::query(
            "SELECT pr.input_per_mtok_micro_usd, pr.output_per_mtok_micro_usd,
                    pr.cache_read_per_mtok_micro_usd, pr.cache_write_per_mtok_micro_usd,
                    pr.tiers_json, pr.schedule_json
             FROM prices pr
             LEFT JOIN upstream_keys k ON k.id = ?
             WHERE pr.upstream_model_id = ?
             ORDER BY CASE WHEN pr.provider_id = k.provider_id THEN 0
                           WHEN pr.provider_id = '' THEN 1
                           ELSE 2 END
             LIMIT 1",
        )
        .bind(uk_id)
        .bind(&canonical_id)
        .fetch_optional(&state.auth.reader_pool)
        .await
        .ok()
        .flatten()
    } else {
        sqlx::query(
            "SELECT input_per_mtok_micro_usd, output_per_mtok_micro_usd,
                    cache_read_per_mtok_micro_usd, cache_write_per_mtok_micro_usd,
                    tiers_json, schedule_json
             FROM prices
             WHERE upstream_model_id = ?
             ORDER BY CASE WHEN provider_id = '' THEN 0 ELSE 1 END
             LIMIT 1",
        )
        .bind(&canonical_id)
        .fetch_optional(&state.auth.reader_pool)
        .await
        .ok()
        .flatten()
    };

    if let Some(pr) = price_row {
        base_rates = Rates {
            input_per_mtok_micro_usd: pr.try_get("input_per_mtok_micro_usd").unwrap_or(3_000_000),
            output_per_mtok_micro_usd: pr
                .try_get("output_per_mtok_micro_usd")
                .unwrap_or(15_000_000),
            cache_read_per_mtok_micro_usd: pr
                .try_get("cache_read_per_mtok_micro_usd")
                .unwrap_or(300_000),
            cache_write_per_mtok_micro_usd: pr
                .try_get("cache_write_per_mtok_micro_usd")
                .unwrap_or(3_750_000),
        };
        if let Ok(Some(tj)) = pr.try_get::<Option<String>, _>("tiers_json") {
            tiers = serde_json::from_str(&tj).unwrap_or_default();
        }
        if let Ok(Some(sj)) = pr.try_get::<Option<String>, _>("schedule_json") {
            schedules = serde_json::from_str(&sj).unwrap_or_default();
        }
    }

    let tokens = TokenUsage {
        input_uncached: in_uncached,
        cache_read,
        cache_write,
        output,
        reasoning: 0,
    };
    let total_input = tokens.total_input();
    let active_tier = tiers.iter().find(|t| t.matches(total_input));
    let effective_rates = active_tier.map_or(base_rates, ContextTier::rates);
    let active_schedule = schedules.iter().find(|s| s.matches(ts));
    let (mult_num, mult_den) = active_schedule.map_or((1_i64, 1_i64), |s| {
        (s.multiplier_num.max(1), s.multiplier_den.max(1))
    });

    let line_cost = |tok: i64, rate: i64| -> i64 {
        let num = (tok as i128) * (rate as i128) * (mult_num as i128);
        let den = 1_000_000_i128 * (mult_den as i128);
        div_ceil_i128(num, den) as i64
    };
    let input_cost = line_cost(in_uncached, effective_rates.input_per_mtok_micro_usd);
    let cache_read_cost = line_cost(cache_read, effective_rates.cache_read_per_mtok_micro_usd);
    let cache_write_cost = line_cost(cache_write, effective_rates.cache_write_per_mtok_micro_usd);
    let output_cost = line_cost(output, effective_rates.output_per_mtok_micro_usd);

    Ok(Json(serde_json::json!({
        "id": row.try_get::<String, _>("id").unwrap_or_default(),
        "request_id": row.try_get::<String, _>("request_id").unwrap_or_default(),
        "ts": ts,
        "canonical_id": canonical_id,
        "rates": effective_rates,
        "tier_applied": active_tier.is_some(),
        "schedule_multiplier": {
            "numerator": mult_num,
            "denominator": mult_den,
        },
        "line_items": {
            "input_uncached": {
                "tokens": in_uncached,
                "rate_per_mtok_micro_usd": effective_rates.input_per_mtok_micro_usd,
                "subtotal_micro_usd": input_cost,
            },
            "cache_read": {
                "tokens": cache_read,
                "rate_per_mtok_micro_usd": effective_rates.cache_read_per_mtok_micro_usd,
                "subtotal_micro_usd": cache_read_cost,
            },
            "cache_write": {
                "tokens": cache_write,
                "rate_per_mtok_micro_usd": effective_rates.cache_write_per_mtok_micro_usd,
                "subtotal_micro_usd": cache_write_cost,
            },
            "output": {
                "tokens": output,
                "rate_per_mtok_micro_usd": effective_rates.output_per_mtok_micro_usd,
                "subtotal_micro_usd": output_cost,
            },
        },
        "list_cost_micro_usd": list_cost_micro_usd,
        "cost_micro_usd": cost_micro_usd,
        "plan_multiplier_bps": plan_multiplier_bps,
        "billing_coverage_mode": billing_coverage_mode,
    })))
}

pub(crate) async fn get_user_usage_events_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    get_usage_events_internal(&state, Some(user_id.to_string()), params).await
}

pub(crate) async fn get_admin_usage_events_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_events_internal(&state, None, params).await
}

pub(crate) async fn get_usage_events_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    params: UsageFilterParams,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let limit = params.limit.unwrap_or(50).clamp(1, 5000);
    
    let query_str = if owner_user_id.is_some() {
        "SELECT id, ts, request_id, client_key_id, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, cache_read_tokens, output_tokens, error_code
         FROM usage_events
         WHERE owner_user_id = ?
           AND (? IS NULL OR id < ?)
         ORDER BY id DESC
         LIMIT ?"
    } else {
        "SELECT id, ts, request_id, client_key_id, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, cache_read_tokens, output_tokens, error_code
         FROM usage_events
         WHERE (? IS NULL OR id < ?)
         ORDER BY id DESC
         LIMIT ?"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(query_str)
            .bind(uid)
            .bind(&params.after)
            .bind(&params.after)
            .bind(limit)
            .fetch_all(&state.auth.pool)
            .await
    } else {
        sqlx::query(query_str)
            .bind(&params.after)
            .bind(&params.after)
            .bind(limit)
            .fetch_all(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut events = Vec::new();
    let mut last_id = None;

    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        last_id = Some(id.clone());
        events.push(serde_json::json!({
            "id": id,
            "ts": r.try_get::<i64, _>("ts").unwrap_or(0),
            "request_id": r.try_get::<String, _>("request_id").unwrap_or_default(),
            "client_key_id": r.try_get::<String, _>("client_key_id").unwrap_or_default(),
            "canonical_id": r.try_get::<Option<String>, _>("canonical_id").unwrap_or_default(),
            "status": r.try_get::<i64, _>("status").unwrap_or(0),
            "cost_micro_usd": r.try_get::<i64, _>("cost_micro_usd").unwrap_or(0),
            "input_tokens": r.try_get::<Option<i64>, _>("input_uncached_tokens").unwrap_or_default(),
            "cached_tokens": r.try_get::<Option<i64>, _>("cache_read_tokens").unwrap_or_default(),
            "output_tokens": r.try_get::<Option<i64>, _>("output_tokens").unwrap_or_default(),
            "error_code": r.try_get::<Option<String>, _>("error_code").unwrap_or_default(),
        }));
    }

    Ok(Json(serde_json::json!({
        "events": events,
        "next_cursor": last_id,
    })))
}

pub(crate) async fn get_user_usage_export_csv_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    export_csv_internal(&state, Some(user_id.to_string())).await
}

pub(crate) async fn get_admin_usage_export_csv_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    export_csv_internal(&state, None).await
}

pub(crate) async fn export_csv_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_proto::headers::sanitize_csv_cell;
    
    let query_str = if owner_user_id.is_some() {
        "SELECT id, ts, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, cache_read_tokens, output_tokens, error_code
         FROM usage_events
         WHERE owner_user_id = ?
         ORDER BY id DESC
         LIMIT 5000"
    } else {
        "SELECT id, ts, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, cache_read_tokens, output_tokens, error_code
         FROM usage_events
         ORDER BY id DESC
         LIMIT 5000"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(query_str)
            .bind(uid)
            .fetch_all(&state.auth.pool)
            .await
    } else {
        sqlx::query(query_str).fetch_all(&state.auth.pool).await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut csv = String::from(
        "id,ts,canonical_id,status,cost_micro_usd,input_tokens,cached_tokens,output_tokens,error_code\r\n",
    );
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let ts: i64 = r.try_get("ts").unwrap_or(0);
        let canonical_id: String = r
            .try_get::<Option<String>, _>("canonical_id")
            .unwrap_or_default()
            .unwrap_or_default();
        let status: i64 = r.try_get("status").unwrap_or(0);
        let cost: i64 = r.try_get("cost_micro_usd").unwrap_or(0);
        let input: i64 = r
            .try_get::<Option<i64>, _>("input_uncached_tokens")
            .unwrap_or_default()
            .unwrap_or(0);
        let cached: i64 = r
            .try_get::<Option<i64>, _>("cache_read_tokens")
            .unwrap_or_default()
            .unwrap_or(0);
        let output: i64 = r
            .try_get::<Option<i64>, _>("output_tokens")
            .unwrap_or_default()
            .unwrap_or(0);
        let error_code: String = r
            .try_get::<Option<String>, _>("error_code")
            .unwrap_or_default()
            .unwrap_or_default();

        let line = format!(
            "{},{},{},{},{},{},{},{},{}\r\n",
            sanitize_csv_cell(&id),
            ts,
            sanitize_csv_cell(&canonical_id),
            status,
            cost,
            input,
            cached,
            output,
            sanitize_csv_cell(&error_code),
        );
        csv.push_str(&line);
    }

    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            "attachment; filename=\"usage_export.csv\"",
        )
        .body(axum::body::Body::from(csv))
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(response)
}

// ---------------------------------------------------------------------------
// Helpers, Logs Explorer, Cost Ledger, and Overview Analytics (USG-9, USG-10, AUD-2, UI-6)
// ---------------------------------------------------------------------------

pub(crate) fn format_cost_usd(micro_usd: i64) -> String {
    let dollars = micro_usd as f64 / 1_000_000.0;
    if dollars >= 10.0 {
        format!("${dollars:.2}")
    } else if dollars >= 0.01 {
        format!("${dollars:.4}")
    } else {
        format!("${dollars:.5}")
    }
}

pub(crate) fn format_tokens_compact(tokens: i64) -> String {
    if tokens >= 1_000_000_000 {
        format!("{:.2}B", tokens as f64 / 1_000_000_000.0)
    } else if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct LogsQuery {
    pub stream: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub model: Option<String>,
    pub client_key_id: Option<String>,
    pub service_account_id: Option<String>,
    pub status: Option<i64>,
    pub method: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

pub(crate) async fn get_user_usage_ledger_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LedgerQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let uid = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    get_usage_ledger_internal(&state, Some(uid.to_string()), query).await
}

pub(crate) async fn get_admin_usage_ledger_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LedgerQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_ledger_internal(&state, None, query).await
}

pub(crate) async fn get_usage_ledger_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    query: LedgerQuery,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        let limit = query.limit.unwrap_or(100).clamp(1, 1000);

    let agg_sql = if owner_user_id.is_some() {
        "SELECT COALESCE(SUM(input_uncached_tokens), 0) as input_uncached,
                COALESCE(SUM(cache_read_tokens), 0) as cache_read,
                COALESCE(SUM(cache_write_tokens), 0) as cache_write,
                COALESCE(SUM(output_tokens), 0) as output,
                COALESCE(SUM(cost_micro_usd), 0) as cost_micro_usd,
                COALESCE(SUM(list_cost_micro_usd), 0) as list_cost_micro_usd
         FROM usage_events
         WHERE owner_user_id = ?
           AND (? IS NULL OR ts >= ?)
           AND (? IS NULL OR ts <= ?)
           AND (? IS NULL OR canonical_id = ?)"
    } else {
        "SELECT COALESCE(SUM(input_uncached_tokens), 0) as input_uncached,
                COALESCE(SUM(cache_read_tokens), 0) as cache_read,
                COALESCE(SUM(cache_write_tokens), 0) as cache_write,
                COALESCE(SUM(output_tokens), 0) as output,
                COALESCE(SUM(cost_micro_usd), 0) as cost_micro_usd,
                COALESCE(SUM(list_cost_micro_usd), 0) as list_cost_micro_usd
         FROM usage_events
         WHERE (? IS NULL OR ts >= ?)
           AND (? IS NULL OR ts <= ?)
           AND (? IS NULL OR canonical_id = ?)"
    };

    let agg_row = if let Some(ref uid) = owner_user_id {
        sqlx::query(agg_sql)
            .bind(uid)
            .bind(query.from)
            .bind(query.from)
            .bind(query.to)
            .bind(query.to)
            .bind(query.model.as_deref())
            .bind(query.model.as_deref())
            .fetch_one(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(agg_sql)
            .bind(query.from)
            .bind(query.from)
            .bind(query.to)
            .bind(query.to)
            .bind(query.model.as_deref())
            .bind(query.model.as_deref())
            .fetch_one(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let input: i64 = agg_row.try_get("input_uncached").unwrap_or(0);
    let cache_read: i64 = agg_row.try_get("cache_read").unwrap_or(0);
    let cache_write: i64 = agg_row.try_get("cache_write").unwrap_or(0);
    let output: i64 = agg_row.try_get("output").unwrap_or(0);
    let total_tokens = input + cache_read + cache_write + output;
    let billed_cost: i64 = agg_row.try_get("cost_micro_usd").unwrap_or(0);
    let list_cost: i64 = agg_row.try_get("list_cost_micro_usd").unwrap_or(0);
    let plan_covered_savings = list_cost.saturating_sub(billed_cost);

    let row_sql = if owner_user_id.is_some() {
        "SELECT id, ts, request_id, client_key_id, canonical_id,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                cost_micro_usd, list_cost_micro_usd, plan_multiplier_bps, billing_coverage_mode
         FROM usage_events
         WHERE owner_user_id = ?
           AND (? IS NULL OR ts >= ?)
           AND (? IS NULL OR ts <= ?)
           AND (? IS NULL OR canonical_id = ?)
         ORDER BY id DESC LIMIT ?"
    } else {
        "SELECT id, ts, request_id, client_key_id, canonical_id,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                cost_micro_usd, list_cost_micro_usd, plan_multiplier_bps, billing_coverage_mode
         FROM usage_events
         WHERE (? IS NULL OR ts >= ?)
           AND (? IS NULL OR ts <= ?)
           AND (? IS NULL OR canonical_id = ?)
         ORDER BY id DESC LIMIT ?"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(row_sql)
            .bind(uid)
            .bind(query.from)
            .bind(query.from)
            .bind(query.to)
            .bind(query.to)
            .bind(query.model.as_deref())
            .bind(query.model.as_deref())
            .bind(limit)
            .fetch_all(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(row_sql)
            .bind(query.from)
            .bind(query.from)
            .bind(query.to)
            .bind(query.to)
            .bind(query.model.as_deref())
            .bind(query.model.as_deref())
            .bind(limit)
            .fetch_all(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let items = rows
        .into_iter()
        .map(|r| {
            let u_cost: i64 = r.try_get("cost_micro_usd").unwrap_or(0);
            let l_cost: i64 = r.try_get("list_cost_micro_usd").unwrap_or(u_cost);
            let u_in: i64 = r.try_get::<Option<i64>, _>("input_uncached_tokens").unwrap_or(None).unwrap_or(0);
            let u_cr: i64 = r.try_get::<Option<i64>, _>("cache_read_tokens").unwrap_or(None).unwrap_or(0);
            let u_cw: i64 = r.try_get::<Option<i64>, _>("cache_write_tokens").unwrap_or(None).unwrap_or(0);
            let u_out: i64 = r.try_get::<Option<i64>, _>("output_tokens").unwrap_or(None).unwrap_or(0);

            serde_json::json!({
                "id": r.try_get::<String, _>("id").unwrap_or_default(),
                "ts": r.try_get::<i64, _>("ts").unwrap_or(0),
                "request_id": r.try_get::<String, _>("request_id").unwrap_or_default(),
                "client_key_id": r.try_get::<String, _>("client_key_id").unwrap_or_default(),
                "canonical_id": r.try_get::<Option<String>, _>("canonical_id").unwrap_or_default(),
                "input_tokens": u_in,
                "cache_read_tokens": u_cr,
                "cache_write_tokens": u_cw,
                "output_tokens": u_out,
                "cost_micro_usd": u_cost,
                "list_cost_micro_usd": l_cost,
                "plan_multiplier_bps": r.try_get::<i64, _>("plan_multiplier_bps").unwrap_or(10_000),
                "billing_coverage_mode": r.try_get::<String, _>("billing_coverage_mode").unwrap_or_else(|_| "pay_as_you_go".to_string()),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({
        "summary": {
            "total_tokens": total_tokens,
            "total_tokens_formatted": format_tokens_compact(total_tokens),
            "input_tokens": input,
            "input_tokens_formatted": format_tokens_compact(input),
            "output_tokens": output,
            "output_tokens_formatted": format_tokens_compact(output),
            "cache_read_tokens": cache_read,
            "cache_read_tokens_formatted": format_tokens_compact(cache_read),
            "cache_write_tokens": cache_write,
            "cache_write_tokens_formatted": format_tokens_compact(cache_write),
            "billed_cost_micro_usd": billed_cost,
            "billed_cost_formatted": format_cost_usd(billed_cost),
            "list_cost_micro_usd": list_cost,
            "list_cost_formatted": format_cost_usd(list_cost),
            "plan_covered_savings_micro_usd": plan_covered_savings,
            "plan_covered_savings_formatted": format_cost_usd(plan_covered_savings),
        },
        "rows": items,
    })))
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct OverviewQuery {
    pub range: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
}

pub(crate) async fn get_user_overview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<OverviewQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let uid = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let body = get_overview_internal(&state, Some(uid.to_string()), query).await?;
    Ok(Json(body))
}

pub(crate) async fn get_overview_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    query: OverviewQuery,
) -> Result<serde_json::Value, (StatusCode, Json<serde_json::Value>)> {
        let now_ms = state.auth.clock.now_ms();
    let since_ms = if let Some(s) = query.since {
        s
    } else {
        match query.range.as_deref() {
            Some("24h") => now_ms - 86_400_000,
            Some("30d") => now_ms - 30 * 86_400_000,
            Some("all") => 0,
            _ => now_ms - 7 * 86_400_000,
        }
    };
    let until_ms = query.until.unwrap_or(now_ms);

    let kpi_sql = if owner_user_id.is_some() {
        "SELECT COALESCE(SUM(requests), 0) as total_requests,
                COALESCE(SUM(cost_micro_usd), 0) as total_cost,
                COALESCE(SUM(input_uncached_tokens + cache_read_tokens + cache_write_tokens + output_tokens), 0) as total_tokens
         FROM usage_rollups_hourly
         WHERE owner_user_id = ? AND hour_ms >= ? AND hour_ms <= ?"
    } else {
        "SELECT COALESCE(SUM(requests), 0) as total_requests,
                COALESCE(SUM(cost_micro_usd), 0) as total_cost,
                COALESCE(SUM(input_uncached_tokens + cache_read_tokens + cache_write_tokens + output_tokens), 0) as total_tokens
         FROM usage_rollups_hourly
         WHERE hour_ms >= ? AND hour_ms <= ?"
    };

    let kpi_row = if let Some(ref uid) = owner_user_id {
        sqlx::query(kpi_sql)
            .bind(uid)
            .bind(since_ms)
            .bind(until_ms)
            .fetch_one(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(kpi_sql)
            .bind(since_ms)
            .bind(until_ms)
            .fetch_one(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let total_requests: i64 = kpi_row.try_get("total_requests").unwrap_or(0);
    let total_cost: i64 = kpi_row.try_get("total_cost").unwrap_or(0);
    let total_tokens: i64 = kpi_row.try_get("total_tokens").unwrap_or(0);

    let ts_sql = if owner_user_id.is_some() {
        "SELECT hour_ms as bucket_ms,
                COALESCE(SUM(requests), 0) as requests,
                COALESCE(SUM(cost_micro_usd), 0) as cost_micro_usd,
                COALESCE(SUM(input_uncached_tokens + cache_read_tokens + cache_write_tokens + output_tokens), 0) as tokens
         FROM usage_rollups_hourly
         WHERE owner_user_id = ? AND hour_ms >= ? AND hour_ms <= ?
         GROUP BY hour_ms ORDER BY hour_ms ASC"
    } else {
        "SELECT hour_ms as bucket_ms,
                COALESCE(SUM(requests), 0) as requests,
                COALESCE(SUM(cost_micro_usd), 0) as cost_micro_usd,
                COALESCE(SUM(input_uncached_tokens + cache_read_tokens + cache_write_tokens + output_tokens), 0) as tokens
         FROM usage_rollups_hourly
         WHERE hour_ms >= ? AND hour_ms <= ?
         GROUP BY hour_ms ORDER BY hour_ms ASC"
    };

    let ts_rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(ts_sql)
            .bind(uid)
            .bind(since_ms)
            .bind(until_ms)
            .fetch_all(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(ts_sql)
            .bind(since_ms)
            .bind(until_ms)
            .fetch_all(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let series = ts_rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "bucket_ms": r.try_get::<i64, _>("bucket_ms").unwrap_or(0),
                "requests": r.try_get::<i64, _>("requests").unwrap_or(0),
                "cost_micro_usd": r.try_get::<i64, _>("cost_micro_usd").unwrap_or(0),
                "tokens": r.try_get::<i64, _>("tokens").unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();

    let models_sql = if owner_user_id.is_some() {
        "SELECT canonical_id,
                COALESCE(SUM(requests), 0) as requests,
                COALESCE(SUM(cost_micro_usd), 0) as cost_micro_usd,
                COALESCE(SUM(input_uncached_tokens), 0) as input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) as cache_read_tokens,
                COALESCE(SUM(cache_write_tokens), 0) as cache_write_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens
         FROM usage_rollups_hourly
         WHERE owner_user_id = ? AND hour_ms >= ? AND hour_ms <= ?
         GROUP BY canonical_id ORDER BY cost_micro_usd DESC LIMIT 50"
    } else {
        "SELECT canonical_id,
                COALESCE(SUM(requests), 0) as requests,
                COALESCE(SUM(cost_micro_usd), 0) as cost_micro_usd,
                COALESCE(SUM(input_uncached_tokens), 0) as input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) as cache_read_tokens,
                COALESCE(SUM(cache_write_tokens), 0) as cache_write_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens
         FROM usage_rollups_hourly
         WHERE hour_ms >= ? AND hour_ms <= ?
         GROUP BY canonical_id ORDER BY cost_micro_usd DESC LIMIT 50"
    };

    let model_rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(models_sql)
            .bind(uid)
            .bind(since_ms)
            .bind(until_ms)
            .fetch_all(&state.auth.reader_pool)
            .await
    } else {
        sqlx::query(models_sql)
            .bind(since_ms)
            .bind(until_ms)
            .fetch_all(&state.auth.reader_pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let models = model_rows
        .into_iter()
        .map(|r| {
            let cost: i64 = r.try_get("cost_micro_usd").unwrap_or(0);
            serde_json::json!({
                "canonical_id": r.try_get::<String, _>("canonical_id").unwrap_or_default(),
                "requests": r.try_get::<i64, _>("requests").unwrap_or(0),
                "cost_micro_usd": cost,
                "cost_formatted": format_cost_usd(cost),
                "input_tokens": r.try_get::<i64, _>("input_tokens").unwrap_or(0),
                "cache_read_tokens": r.try_get::<i64, _>("cache_read_tokens").unwrap_or(0),
                "cache_write_tokens": r.try_get::<i64, _>("cache_write_tokens").unwrap_or(0),
                "output_tokens": r.try_get::<i64, _>("output_tokens").unwrap_or(0),
                "badge": "[LLR]",
            })
        })
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "kpi": {
            "cost_micro_usd": total_cost,
            "cost_formatted": format_cost_usd(total_cost),
            "requests": total_requests,
            "requests_formatted": format_tokens_compact(total_requests),
            "tokens_total": total_tokens,
            "tokens_formatted": format_tokens_compact(total_tokens),
        },
        "series": series,
        "models": models,
    }))
}

pub(crate) async fn control_plane_logging_middleware(
    State(state): State<ApiAppState>,
    req: Request,
    next: Next,
) -> Response {
    let raw_path = req.uri().path_and_query().map_or_else(
        || req.uri().path().to_string(),
        |pq| pq.as_str().to_string(),
    );
    let path = llmroller_proto::scrub::scrub_text(&raw_path);
    let method = req.method().as_str().to_string();
    let is_cp = path.starts_with("/api/user/") || path.starts_with("/admin/api/");
    if !is_cp || path.contains("/logs") {
        return next.run(req).await;
    }

    let t0 = std::time::Instant::now();
    let headers = req.headers().clone();
    let peer_addr = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0)
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 0)));
    let ip_addr = Some(
        crate::auth::extract_client_ip(
            peer_addr,
            &headers,
            state.auth.config.trusted_proxies.as_slice(),
        )
        .to_string(),
    );
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(llmroller_proto::scrub::scrub_text);
    let actor_user_id = crate::auth::authenticate_session(&state.auth, &headers)
        .await
        .ok()
        .map(|s| s.user.id);
    let workspace_scope = if path.starts_with("/admin/") {
        "global".to_string()
    } else {
        "personal".to_string()
    };

    let response = next.run(req).await;

    if let Some(uid) = actor_user_id {
        let latency_ms = t0.elapsed().as_millis() as i64;
        let status = response.status().as_u16() as i64;
        let pool = state.auth.pool.clone();
        let log_id = llmroller_core::ids::RequestId::generate().to_string();
        let req_id = llmroller_core::ids::RequestId::generate().to_string();
        let now_ms = state.auth.clock.now_ms();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO control_plane_logs
                 (id, ts, request_id, actor_user_id, workspace_scope, method, path, status, latency_ms, ip_addr, user_agent)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&log_id)
            .bind(now_ms)
            .bind(&req_id)
            .bind(uid.to_string())
            .bind(&workspace_scope)
            .bind(&method)
            .bind(&path)
            .bind(status)
            .bind(latency_ms)
            .bind(ip_addr)
            .bind(user_agent)
            .execute(&pool)
            .await;
        });
    }

    response
}

// ---------------------------------------------------------------------------
// HTMX Partial Endpoints (SRDD Section 14.3)
// ---------------------------------------------------------------------------

