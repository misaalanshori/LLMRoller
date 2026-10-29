use super::*;
use sqlx::Row as _;

pub(crate) async fn get_admin_settings_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<RuntimeSettings>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    let settings = load_settings(&state.auth.pool)
        .await
        .map_err(map_settings_error)?;
    Ok(Json(settings))
}

pub(crate) async fn patch_admin_settings_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<HashMap<String, serde_json::Value>>,
) -> Result<Json<RuntimeSettings>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();

    // 1. Validate all keys and values first
    for (k, v) in &payload {
        let val_str = serde_json::to_string(v).unwrap_or_default();
        validate_setting(k, &val_str).map_err(map_settings_error)?;
    }

    // 2. Apply updates
    for (k, v) in &payload {
        let val_str = serde_json::to_string(v).unwrap_or_default();
        update_setting(&state.auth.pool, k, &val_str, now_ms)
            .await
            .map_err(map_settings_error)?;
    }

    let updated = load_settings(&state.auth.pool)
        .await
        .map_err(map_settings_error)?;
    Ok(Json(updated))
}

// ---------------------------------------------------------------------------
// Router Setup
// ---------------------------------------------------------------------------

/// Build the JSON control-plane router.
pub(crate) fn map_price_error(
    status: StatusCode,
    msg: impl std::fmt::Display,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({ "error": { "message": msg.to_string() } })),
    )
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateTraceabilityRequest {
    pub enabled: bool,
    #[serde(default)]
    pub match_filters: llmroller_core::settings::TraceabilityMatch,
    pub retention_days: i64,
    pub auto_disable_at: Option<i64>,
    pub confirm_purge: Option<bool>,
}

pub(crate) async fn put_admin_traceability_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdateTraceabilityRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let settings = llmroller_store::settings::load_settings(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let now_ms = state.auth.clock.now_ms();

    // Confirm purge guard (Section 6.10, TRC-1)
    if payload.retention_days < settings.traceability_config.retention_days
        && payload.confirm_purge != Some(true)
    {
        let new_cutoff = now_ms - payload.retention_days * 86_400_000;
        let affected_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM trace_records WHERE ts < ?")
                .bind(new_cutoff)
                .fetch_one(&state.auth.pool)
                .await
                .unwrap_or(0);

        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": {
                    "code": "purge_confirmation_required",
                    "message": "Lowering retention days requires explicit purge confirmation",
                    "affected_rows": affected_rows
                }
            })),
        ));
    }

    let new_config = llmroller_core::settings::TraceabilityConfig {
        enabled: payload.enabled,
        match_filters: payload.match_filters,
        retention_days: payload.retention_days,
        auto_disable_at: payload.auto_disable_at,
    };

    let config_json_str = serde_json::to_string(&new_config)
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    llmroller_store::settings::update_setting(
        &state.auth.pool,
        "traceability_config",
        &config_json_str,
        now_ms,
    )
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Lowering retention must actually delete, not just record the new window. The periodic
    // sweep cannot cover this: it deletes on `expires_at`, and `expires_at` was stamped from
    // the retention in force when each record was written, so lowering retention never makes an
    // existing row expire (`ISSUE-0194`).
    //
    // `confirm_purge: true` is the operator consenting to exactly this, and the guard above has
    // already refused the request without it.
    //
    // Only on a *reduction*: widening the window deletes nothing, and running the purge then
    // would use the new (larger) window to delete records the operator never agreed to lose.
    let purged = if payload.retention_days < settings.traceability_config.retention_days {
        let purge_cutoff = now_ms - payload.retention_days * 86_400_000;
        purge_trace_records_and_orphan_blocks(&state, purge_cutoff).await?
    } else {
        0
    };

    Ok(Json(serde_json::json!({
        "status": "ok",
        "traceability_config": new_config,
        "purged_records": purged,
    })))
}

/// Delete trace records older than `cutoff_ms`, then collect the blocks they alone referenced.
///
/// The order matters and so does the pairing: deleting records leaves `trace_blocks` rows
/// behind, and a block is only collectable once **no** remaining record references it —
/// `trace_blocks` is content-addressed (`ADR-9`), so a blanket delete would corrupt every live
/// trace that happens to share a block with an expired one.
///
/// Both steps reuse the existing retention primitives, which already carry the reference check
/// and the batching: `sweep_orphaned_trace_blocks` deletes in `LIMIT` chunks guarded by an
/// `NOT EXISTS` against `trace_record_blocks`, the indexable join table
/// (`idx_trace_record_blocks_ref`). So this is one indexed query per chunk, not a JSON walk.
///
/// The record delete is not batched, matching `sweep_expired_trace_records`, which is a single
/// `DELETE` by `expires_at`; the orphan sweep is chunked so a large block table cannot produce
/// one unbounded statement.
async fn purge_trace_records_and_orphan_blocks(
    state: &ApiAppState,
    cutoff_ms: i64,
) -> Result<i64, MutationResponseError> {
    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    // `trace_record_blocks` cascades with its records, so the reference set shrinks atomically
    // with the purge.
    let purged = sqlx::query("DELETE FROM trace_records WHERE ts < ?")
        .bind(cutoff_ms)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .rows_affected();
    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Same transaction boundary is impossible across a chunked sweep without holding the write
    // lock for an unbounded time, so the sweep runs immediately after the committed purge. A
    // crash between them leaves orphaned blocks, which the periodic sweep collects — the safe
    // direction, since the alternative (blocks deleted, records still present) would corrupt
    // live traces.
    let mut collected = 0_u64;
    loop {
        let deleted = llmroller_store::retention::sweep_orphaned_trace_blocks(
            &state.auth.pool,
            i64::MAX,
            1_000,
        )
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        collected = collected.saturating_add(deleted);
        if deleted < 1_000 {
            break;
        }
    }
    let _ = collected;
    Ok(i64::try_from(purged).unwrap_or(i64::MAX))
}

fn default_price_provider_kind() -> String {
    "custom".to_string()
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpsertPricePayload {
    pub provider_id: Option<String>,
    #[serde(default = "default_price_provider_kind")]
    pub provider_kind: String,
    pub upstream_model_id: String,
    pub input_per_mtok_micro_usd: i64,
    pub output_per_mtok_micro_usd: i64,
    pub cache_read_per_mtok_micro_usd: Option<i64>,
    pub cache_write_per_mtok_micro_usd: Option<i64>,
    /// `None` (field absent) leaves the stored tiers alone; `[]` clears them. See
    /// `UpsertPriceParams::tiers` for why the distinction is load-bearing.
    #[serde(default)]
    pub tiers: Option<Vec<llmroller_core::pricing::ContextTier>>,
    /// `None` (field absent) leaves the stored schedules alone; `[]` clears them.
    #[serde(default)]
    pub schedules: Option<Vec<llmroller_core::pricing::Schedule>>,
}

pub(crate) async fn get_admin_prices_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    let rows = sqlx::query(
        "SELECT provider_id, provider_kind, upstream_model_id, input_per_mtok_micro_usd,
                output_per_mtok_micro_usd, cache_read_per_mtok_micro_usd,
                cache_write_per_mtok_micro_usd, tiers_json, schedule_json, source
         FROM prices ORDER BY upstream_model_id ASC, provider_id ASC",
    )
    .fetch_all(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        // `tiers_json`/`schedule_json` are what the cost resolver reads (`ISSUE-0163`). Reporting
        // them is not decoration: without them an admin sees a cost produced by a context tier and
        // cannot inspect the tier that produced it.
        //
        // A NULL column means "no tiers configured", which is `[]`. A *populated* column that will
        // not parse is a different thing — a stored value the resolver would also choke on — and
        // it is reported as `null` rather than silently flattened to `[]`. Flattening would claim
        // "this model has no tiers" about a row that has unreadable ones, which is exactly the
        // invented-fact failure cycle 46 removed from the breakdown.
        out.push(serde_json::json!({
            "provider_id": r.try_get::<String, _>("provider_id").unwrap_or_default(),
            "provider_kind": r.try_get::<String, _>("provider_kind").unwrap_or_default(),
            "upstream_model_id": r.try_get::<String, _>("upstream_model_id").unwrap_or_default(),
            "input_per_mtok_micro_usd": r.try_get::<i64, _>("input_per_mtok_micro_usd").unwrap_or(0),
            "output_per_mtok_micro_usd": r.try_get::<i64, _>("output_per_mtok_micro_usd").unwrap_or(0),
            "cache_read_per_mtok_micro_usd": r.try_get::<Option<i64>, _>("cache_read_per_mtok_micro_usd").unwrap_or_default().unwrap_or(0),
            "cache_write_per_mtok_micro_usd": r.try_get::<Option<i64>, _>("cache_write_per_mtok_micro_usd").unwrap_or_default().unwrap_or(0),
            "tiers": decode_price_json::<llmroller_core::pricing::ContextTier>(
                r.try_get::<Option<String>, _>("tiers_json").ok().flatten().as_deref(),
            ),
            "schedules": decode_price_json::<llmroller_core::pricing::Schedule>(
                r.try_get::<Option<String>, _>("schedule_json").ok().flatten().as_deref(),
            ),
            "source": r.try_get::<String, _>("source").unwrap_or_default(),
        }));
    }
    Ok(Json(out))
}

/// Project a stored `*_json` price column into the admin API's array shape.
///
/// `None` (a NULL column) becomes `[]`, because "no tiers" is a real configuration. A column that
/// is present but unparseable becomes `null`, so the admin API never claims a row has no tiers when
/// in fact it has unreadable ones (`ISSUE-0163`, and the cycle-46 rule that a missing financial fact
/// is reported rather than defaulted).
fn decode_price_json<T: serde::de::DeserializeOwned + serde::Serialize>(
    raw: Option<&str>,
) -> serde_json::Value {
    match raw {
        None => serde_json::json!([]),
        Some(text) => serde_json::from_str::<Vec<T>>(text)
            .map(|items| serde_json::to_value(items).unwrap_or(serde_json::Value::Null))
            .unwrap_or(serde_json::Value::Null),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct DeletePriceParams {
    pub provider_id: Option<String>,
    pub upstream_model_id: String,
}

pub(crate) async fn delete_user_provider_price_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ProviderId>,
    axum::extract::Query(params): axum::extract::Query<DeletePriceParams>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let p = llmroller_store::providers::get_provider(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    if p.owner_user_id != actor.user_id() {
        return Err(map_price_error(StatusCode::FORBIDDEN, "not provider owner"));
    }
    let provider_id = id.to_string();
    let upstream_model_id = params.upstream_model_id;
    // `execute_atomic_mutation` is generic over its closure's return type, so the affected
    // flag travels back through it with no helper change and no captured cell.
    let deleted = execute_atomic_mutation(&state, vec![ConfigDomain::Prices], move |tx| {
        Box::pin(async move {
            llmroller_store::prices::delete_price_tx(tx, &provider_id, &upstream_model_id)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        })
    })
    .await?;
    Ok(delete_status(deleted))
}

pub(crate) async fn delete_admin_price_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<DeletePriceParams>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let provider_id = params.provider_id.unwrap_or_default();
    let upstream_model_id = params.upstream_model_id;
    let deleted = execute_atomic_mutation(&state, vec![ConfigDomain::Prices], move |tx| {
        Box::pin(async move {
            llmroller_store::prices::delete_price_tx(tx, &provider_id, &upstream_model_id)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        })
    })
    .await?;
    Ok(delete_status(deleted))
}

/// `204` when the delete removed a row, `404` when nothing matched.
///
/// Without this a delete of a non-existent `(provider_id, upstream_model_id)` pair reported
/// success, so an operator removing a stale price had no way to learn it was never there
/// (`ISSUE-0192`).
const fn delete_status(deleted: bool) -> StatusCode {
    if deleted {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

pub(crate) async fn put_admin_price_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<UpsertPricePayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let provider_id = payload.provider_id.clone().unwrap_or_default();
    upsert_price_internal(&state, provider_id, payload, "admin").await
}

pub(crate) async fn put_user_provider_price_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ProviderId>,
    Json(payload): Json<UpsertPricePayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let p = llmroller_store::providers::get_provider(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    if p.owner_user_id != actor.user_id() {
        return Err(map_price_error(StatusCode::FORBIDDEN, "not provider owner"));
    }
    upsert_price_internal(&state, id.to_string(), payload, "user").await
}

pub(crate) async fn upsert_price_internal(
    state: &ApiAppState,
    provider_id: String,
    payload: UpsertPricePayload,
    source: &str,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let rates = llmroller_core::pricing::Rates {
        input_per_mtok_micro_usd: payload.input_per_mtok_micro_usd,
        output_per_mtok_micro_usd: payload.output_per_mtok_micro_usd,
        cache_read_per_mtok_micro_usd: payload.cache_read_per_mtok_micro_usd.unwrap_or(0),
        cache_write_per_mtok_micro_usd: payload.cache_write_per_mtok_micro_usd.unwrap_or(0),
    };

    let params = llmroller_store::prices::UpsertPriceParams {
        provider_id,
        provider_kind: payload.provider_kind,
        upstream_model_id: payload.upstream_model_id,
        rates,
        tiers: payload.tiers,
        schedules: payload.schedules,
        source: source.to_string(),
        now_ms: state.auth.clock.now_ms(),
    };

    llmroller_store::prices::upsert_price_tx(&mut tx, params)
        .await
        .map_err(|e| match e {
            llmroller_store::prices::PriceError::Validation(msg) => {
                map_price_error(StatusCode::UNPROCESSABLE_ENTITY, msg)
            }
            llmroller_store::prices::PriceError::Database(err) => {
                map_price_error(StatusCode::INTERNAL_SERVER_ERROR, err)
            }
        })?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

/// Parse the provider price catalogue served by `price_sync_catalog_url`.
///
/// Expected shape: `{ <provider>: { models: { <upstream_id>: { cost: { input, output,
/// cache_read, cache_write, input_over_200k?, output_over_200k?, cache_read_over_200k?,
/// cache_write_over_200k? }, alias?, aliases? } } } }`. Returns one insert template per
/// upstream model ID plus every alias the catalogue declares for it (F012).
pub(crate) fn parse_catalog_prices(
    catalog: &serde_json::Value,
) -> (
    HashMap<String, llmroller_store::prices::UpsertPriceParams>,
    HashMap<String, Vec<String>>,
) {
    let micro = |usd_per_mtok: f64| (usd_per_mtok * 1_000_000.0).round() as i64;
    let mut model_costs: HashMap<String, llmroller_store::prices::UpsertPriceParams> =
        HashMap::new();
    let mut catalog_aliases: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(providers) = catalog.as_object() {
        for provider in providers.values() {
            let Some(models) = provider.get("models").and_then(|m| m.as_object()) else {
                continue;
            };
            for (model_id, model) in models {
                // Catalog aliases (`alias` or `aliases`) are imported into
                // `model_aliases` so clients can call the upstream spelling (F012).
                let mut aliases = Vec::new();
                if let Some(alias) = model.get("alias").and_then(serde_json::Value::as_str) {
                    aliases.push(alias.to_string());
                }
                if let Some(list) = model.get("aliases").and_then(serde_json::Value::as_array) {
                    aliases.extend(
                        list.iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_owned),
                    );
                }
                if !aliases.is_empty() {
                    catalog_aliases
                        .entry(model_id.clone())
                        .or_default()
                        .extend(aliases);
                }

                let Some(cost) = model.get("cost") else {
                    continue;
                };
                let read = |key: &str| cost.get(key).and_then(serde_json::Value::as_f64);
                // Context-length tiers: providers publish `*_over_200k` rates for
                // prompts above the standard context window (`USG-3`, Section 9.3.2).
                let mut tiers = Vec::new();
                if let (Some(input_over), Some(output_over)) =
                    (read("input_over_200k"), read("output_over_200k"))
                {
                    tiers.push(llmroller_core::pricing::ContextTier {
                        min_total_input_tokens: 200_000,
                        max_total_input_tokens: None,
                        input_per_mtok_micro_usd: micro(input_over),
                        output_per_mtok_micro_usd: micro(output_over),
                        cache_read_per_mtok_micro_usd: read("cache_read_over_200k")
                            .map_or(0, micro),
                        cache_write_per_mtok_micro_usd: read("cache_write_over_200k")
                            .map_or(0, micro),
                    });
                }
                let entry = model_costs.entry(model_id.clone()).or_insert_with(|| {
                    llmroller_store::prices::UpsertPriceParams {
                        provider_id: String::new(),
                        provider_kind: String::new(),
                        upstream_model_id: model_id.clone(),
                        rates: llmroller_core::pricing::Rates::default(),
                        tiers: Some(Vec::new()),
                        schedules: Some(Vec::new()),
                        source: "synced".to_string(),
                        now_ms: 0,
                    }
                });
                let in_rate = read("input").unwrap_or(0.0);
                let out_rate = read("output")
                    .or_else(|| read("internal_reasoning"))
                    .unwrap_or(0.0);
                let cr_rate = read("cache_read")
                    .or_else(|| read("input_cache_read"))
                    .unwrap_or(0.0);
                let cw_rate = read("cache_write")
                    .or_else(|| read("input_cache_write"))
                    .unwrap_or(0.0);
                let rates = llmroller_core::pricing::Rates {
                    input_per_mtok_micro_usd: micro(in_rate),
                    output_per_mtok_micro_usd: micro(out_rate),
                    cache_read_per_mtok_micro_usd: micro(cr_rate),
                    cache_write_per_mtok_micro_usd: micro(cw_rate),
                };
                entry.rates = rates;
                entry.tiers = Some(tiers.clone());
                if let Ok(norm_id) = llmroller_core::catalog::normalize_model_id(model_id)
                    && &norm_id != model_id
                {
                    model_costs.entry(norm_id.clone()).or_insert_with(|| {
                        llmroller_store::prices::UpsertPriceParams {
                            provider_id: String::new(),
                            provider_kind: String::new(),
                            upstream_model_id: norm_id,
                            rates,
                            tiers: Some(tiers),
                            schedules: Some(Vec::new()),
                            source: "synced".to_string(),
                            now_ms: 0,
                        }
                    });
                }
            }
        }
    }
    // Also support OpenRouter /api/v1/models {"data": [{"id": "...", "pricing": {...}}]} format (F-068).
    if let Some(data_arr) = catalog.get("data").and_then(serde_json::Value::as_array) {
        let parse_or_price = |p: &serde_json::Value, key: &str| -> Option<i64> {
            let v = p.get(key)?;
            let raw = v
                .as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))?;
            if !raw.is_finite() || raw < 0.0 {
                return None;
            }
            // OpenRouter PublicPricing reports USD per token (e.g. "0.000003" = $3/MTok = 3_000_000 micro-USD/MTok).
            let micro_mtok = if raw > 0.0 && raw < 0.01 {
                (raw * 1_000_000_000_000.0).round() as i64
            } else {
                (raw * 1_000_000.0).round() as i64
            };
            Some(micro_mtok)
        };
        for item in data_arr {
            let Some(raw_id) = item.get("id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(pricing) = item.get("pricing") else {
                continue;
            };
            let Some(in_micro) =
                parse_or_price(pricing, "prompt").or_else(|| parse_or_price(pricing, "input"))
            else {
                continue;
            };
            let Some(out_micro) = parse_or_price(pricing, "completion")
                .or_else(|| parse_or_price(pricing, "output"))
                .or_else(|| parse_or_price(pricing, "internal_reasoning"))
            else {
                continue;
            };
            let cr_micro = parse_or_price(pricing, "input_cache_read")
                .or_else(|| parse_or_price(pricing, "cache_read"))
                .unwrap_or(0);
            let cw_micro = parse_or_price(pricing, "input_cache_write")
                .or_else(|| parse_or_price(pricing, "cache_write"))
                .unwrap_or(0);
            let rates = llmroller_core::pricing::Rates {
                input_per_mtok_micro_usd: in_micro,
                output_per_mtok_micro_usd: out_micro,
                cache_read_per_mtok_micro_usd: cr_micro,
                cache_write_per_mtok_micro_usd: cw_micro,
            };
            for key_id in [
                Some(raw_id.to_string()),
                llmroller_core::catalog::normalize_model_id(raw_id).ok(),
            ]
            .into_iter()
            .flatten()
            {
                model_costs.insert(
                    key_id.clone(),
                    llmroller_store::prices::UpsertPriceParams {
                        provider_id: String::new(),
                        provider_kind: String::new(),
                        upstream_model_id: key_id,
                        rates,
                        tiers: Some(Vec::new()),
                        schedules: Some(Vec::new()),
                        source: "synced".to_string(),
                        now_ms: 0,
                    },
                );
            }
        }
    }
    (model_costs, catalog_aliases)
}

/// Read a catalog body in bounded chunks, refusing anything over `SEC-8`'s ceiling.
///
/// The ceiling is [`llmroller_proto::discovery::MAX_DISCOVERY_BYTES`] -- the same 2 MiB
/// `SEC-8` applies to every remote catalog this codebase reads (`/models`, `/usage`). One
/// number for "upstream body too large", not a second locally-tuned constant.
///
/// The `.timeout(...)` on the request bounds *time*, not *bytes*: a fast CDN-hosted
/// multi-hundred-megabyte JSON arrives comfortably inside ten seconds. This bounds bytes
/// (`ISSUE-0179`).
///
/// Generic over the chunk source so the ceiling is testable without a network, a TLS
/// endpoint, or a hostile server.
///
/// # Errors
///
/// Returns `Err` with an actionable message when the body exceeds the ceiling or the
/// transport fails mid-stream. A truncated body must never be returned as if complete: a
/// partial catalog that parsed would import partial prices and report success.
pub(crate) async fn read_catalog_body<S>(source: &mut S) -> Result<Vec<u8>, String>
where
    S: CatalogChunkSource,
{
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = source.next_chunk().await? {
        if body.len().saturating_add(chunk.len()) > llmroller_proto::discovery::MAX_DISCOVERY_BYTES
        {
            return Err(format!(
                "price catalog response exceeded {} bytes; refusing to read further",
                llmroller_proto::discovery::MAX_DISCOVERY_BYTES
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A source of body chunks for [`read_catalog_body`].
///
/// A trait with one production impl is normally a type parameter looking for a reason. This
/// one earns it: it is what makes the `SEC-8` ceiling testable without a network, a TLS
/// endpoint or a hostile server, and the two test impls in `catalog_sync_tests` are the
/// proof. The alternative -- a generic over `Stream<Item = Result<Bytes, E>>` -- is what
/// `reqwest::Response` already is, but naming that bound needs `futures-core`, which this
/// crate does not depend on; adding a dependency to delete a trait is a worse trade.
pub(crate) trait CatalogChunkSource {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String>;
}

impl CatalogChunkSource for reqwest::Response {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.chunk()
            .await
            .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
            .map_err(|e| e.to_string())
    }
}

pub(crate) async fn post_admin_prices_sync_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let settings = llmroller_store::settings::load_settings(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if !settings.price_sync_catalog_url.enabled {
        return Ok(Json(serde_json::json!({
            "synced": false,
            "reason": "disabled"
        })));
    }

    let url = settings.price_sync_catalog_url.url.trim();
    llmroller_core::ssrf::validate_url(url, false)
        .map_err(|_| map_price_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid catalog URL"))?;

    let client = llmroller_proxy::ssrf::create_guarded_client(false)
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_GATEWAY, e))?
        .error_for_status()
        .map_err(|e| map_price_error(StatusCode::BAD_GATEWAY, e))?;
    // Byte-bounded read: the request timeout bounds time, not size (`ISSUE-0179`).
    let body = read_catalog_body(&mut response)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_GATEWAY, e))?;
    let catalog: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
        map_price_error(
            StatusCode::BAD_GATEWAY,
            format!("price catalog response is not valid JSON: {e}"),
        )
    })?;

    let (model_costs, catalog_aliases) = parse_catalog_prices(&catalog);

    let routes = sqlx::query(
        "SELECT k.provider_id, p.kind, r.upstream_model_id, r.model_pk
         FROM model_routes r
         JOIN upstream_keys k ON r.upstream_key_id = k.id
         JOIN providers p ON k.provider_id = p.id",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut synced = 0_usize;
    let mut imported_aliases = 0_usize;
    let mut seen = std::collections::HashSet::new();
    for route in routes {
        let provider_id: String = route.try_get("provider_id").unwrap_or_default();
        let kind: String = route.try_get("kind").unwrap_or_default();
        let model_id: String = route.try_get("upstream_model_id").unwrap_or_default();
        if !seen.insert((provider_id.clone(), kind.clone(), model_id.clone())) {
            continue;
        }

        // Import confirmed aliases for models that carry them in the catalog. The
        // alias must not collide with a canonical ID or an existing alias (MDL-6).
        if let Some(aliases) = catalog_aliases.get(&model_id) {
            let model_pk: String = route.try_get("model_pk").unwrap_or_default();
            for alias in aliases {
                if alias == &model_id {
                    continue;
                }
                let collides: Option<i64> =
                    sqlx::query_scalar("SELECT 1 FROM models WHERE canonical_id = ?")
                        .bind(alias)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                if collides.is_some() {
                    continue;
                }
                let result = sqlx::query(
                    "INSERT OR IGNORE INTO model_aliases (alias, model_pk, confirmed) VALUES (?, ?, 1)",
                )
                .bind(alias)
                .bind(&model_pk)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                imported_aliases += usize::try_from(result.rows_affected()).unwrap_or(0);
            }
        }

        let norm_id = llmroller_core::catalog::normalize_model_id(&model_id).ok();
        let Some(template) = model_costs
            .get(&model_id)
            .or_else(|| norm_id.as_deref().and_then(|n| model_costs.get(n)))
        else {
            continue;
        };
        let manual: Option<String> = sqlx::query_scalar(
            "SELECT source FROM prices WHERE provider_id = ? AND provider_kind = ? AND upstream_model_id = ?",
        )
        .bind(&provider_id)
        .bind(&kind)
        .bind(&model_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        if matches!(manual.as_deref(), Some("manual" | "admin" | "user")) {
            continue;
        }
        llmroller_store::prices::upsert_price_tx(
            &mut tx,
            llmroller_store::prices::UpsertPriceParams {
                provider_id,
                provider_kind: kind,
                upstream_model_id: model_id,
                rates: template.rates,
                tiers: template.tiers.clone(),
                schedules: template.schedules.clone(),
                source: "synced".to_string(),
                now_ms: state.auth.clock.now_ms(),
            },
        )
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        synced += 1;
    }
    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "synced": true,
        "prices_synced": synced,
        "imported_aliases": imported_aliases
    })))
}

pub(crate) fn chrono_like_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
