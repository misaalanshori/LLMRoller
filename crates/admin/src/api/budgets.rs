use super::*;
use sqlx::Row as _;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct CreateBudgetWindowRequest {
    pub scope_kind: String,
    pub model_pk: Option<String>,
    pub window_kind: String,
    pub period_ms: Option<i64>,
    pub anchor_at: Option<i64>,
    pub upstream_window_kind: Option<String>,
    pub limit_micro_usd: i64,
    pub reserve_pct: Option<i64>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct PatchBudgetWindowRequest {
    pub window_kind: Option<String>,
    pub period_ms: Option<i64>,
    pub anchor_at: Option<i64>,
    pub upstream_window_kind: Option<String>,
    pub limit_micro_usd: Option<i64>,
    pub reserve_pct: Option<i64>,
}

/// Validate a budget-window definition against the schema checks in Section 9.1
/// before it reaches SQLite so callers get `422` instead of a constraint error.
pub(crate) fn validate_budget_window(
    scope_kind: &str,
    model_pk: Option<&str>,
    window_kind: &str,
    period_ms: Option<i64>,
    limit_micro_usd: i64,
    reserve_pct: i64,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !matches!(scope_kind, "upstream_key" | "route") {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "scope_kind must be upstream_key or route",
        ));
    }
    if !matches!(
        window_kind,
        "rolling" | "anchored_fixed" | "anchored_calendar_month" | "anchored_provider"
    ) {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid window_kind",
        ));
    }
    if scope_kind == "route" && model_pk.is_none() {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "route windows require model_pk",
        ));
    }
    if scope_kind == "upstream_key" && model_pk.is_some() {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "upstream_key windows must not set model_pk",
        ));
    }
    match (window_kind, period_ms) {
        ("anchored_calendar_month", None) => {}
        ("anchored_calendar_month", Some(_)) => {
            return Err(map_price_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "anchored_calendar_month windows derive their period and must not set period_ms",
            ));
        }
        (_, Some(period)) if (3_600_000..=2_678_400_000).contains(&period) => {}
        (_, _) => {
            return Err(map_price_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "period_ms must be between 1 hour and 31 days",
            ));
        }
    }
    if limit_micro_usd <= 0 {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "limit_micro_usd must be positive",
        ));
    }
    if !(0..=50).contains(&reserve_pct) {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "reserve_pct must be between 0 and 50",
        ));
    }
    Ok(())
}

/// Ensure the caller may manage windows on `upstream_key_id` (`USR-3`, `SEC-17`).
pub(crate) async fn authorize_budget_window_key(
    state: &ApiAppState,
    actor: &llmroller_core::rbac::ActorScope,
    upstream_key_id: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
        let row = sqlx::query(
        "SELECT k.owner_user_id, p.ownership_scope
         FROM upstream_keys k JOIN providers p ON p.id = k.provider_id
         WHERE k.id = ?",
    )
    .bind(upstream_key_id)
    .fetch_optional(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "upstream key not found"))?;
    if actor.is_admin() || actor.is_system() {
        return Ok(());
    }
    let scope: String = row.try_get("ownership_scope").unwrap_or_default();
    let owner: Option<String> = row.try_get("owner_user_id").unwrap_or_default();
    let caller = actor.user_id().map(|id| id.to_string());
    if scope == "personal" && owner == caller {
        return Ok(());
    }
    Err(map_price_error(
        StatusCode::NOT_FOUND,
        "upstream key not found",
    ))
}

pub(crate) async fn list_budget_windows_internal(
    state: &ApiAppState,
    actor: &llmroller_core::rbac::ActorScope,
    upstream_key_id: &str,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        authorize_budget_window_key(state, actor, upstream_key_id).await?;
    let rows = sqlx::query(
        "SELECT id, scope_kind, upstream_key_id, model_pk, window_kind, period_ms,
                anchor_at, upstream_window_kind, limit_micro_usd, reserve_pct
         FROM budget_windows WHERE upstream_key_id = ? ORDER BY id ASC",
    )
    .bind(upstream_key_id)
    .fetch_all(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let windows = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.try_get::<String, _>("id").unwrap_or_default(),
                "scope_kind": row.try_get::<String, _>("scope_kind").unwrap_or_default(),
                "upstream_key_id": row.try_get::<String, _>("upstream_key_id").unwrap_or_default(),
                "model_pk": row.try_get::<Option<String>, _>("model_pk").unwrap_or_default(),
                "window_kind": row.try_get::<String, _>("window_kind").unwrap_or_default(),
                "period_ms": row.try_get::<Option<i64>, _>("period_ms").unwrap_or_default(),
                "anchor_at": row.try_get::<Option<i64>, _>("anchor_at").unwrap_or_default(),
                "upstream_window_kind": row
                    .try_get::<Option<String>, _>("upstream_window_kind")
                    .unwrap_or_default(),
                "limit_micro_usd": row.try_get::<i64, _>("limit_micro_usd").unwrap_or(0),
                "reserve_pct": row.try_get::<i64, _>("reserve_pct").unwrap_or(2),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({ "data": windows })))
}

pub(crate) async fn create_budget_window_internal(
    state: &ApiAppState,
    actor: &llmroller_core::rbac::ActorScope,
    upstream_key_id: String,
    payload: CreateBudgetWindowRequest,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    authorize_budget_window_key(state, actor, &upstream_key_id).await?;
    validate_budget_window(
        &payload.scope_kind,
        payload.model_pk.as_deref(),
        &payload.window_kind,
        payload.period_ms,
        payload.limit_micro_usd,
        payload.reserve_pct.unwrap_or(2),
    )?;
    let id = llmroller_core::ids::BudgetWindowId::generate().to_string();
    let model_pk = payload.model_pk.clone();
    let window_kind = payload.window_kind.clone();
    let scope_kind = payload.scope_kind.clone();
    let created_id = id.clone();
    execute_atomic_mutation(state, vec![ConfigDomain::BudgetWindows], move |tx| {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO budget_windows
                 (id, scope_kind, upstream_key_id, model_pk, window_kind, period_ms, anchor_at,
                  upstream_window_kind, limit_micro_usd, reserve_pct)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&created_id)
            .bind(&scope_kind)
            .bind(&upstream_key_id)
            .bind(&model_pk)
            .bind(&window_kind)
            .bind(payload.period_ms)
            .bind(payload.anchor_at)
            .bind(&payload.upstream_window_kind)
            .bind(payload.limit_micro_usd)
            .bind(payload.reserve_pct.unwrap_or(2))
            .execute(&mut **tx)
            .await
            .map_err(|e| map_price_error(StatusCode::UNPROCESSABLE_ENTITY, e))?;
            Ok(serde_json::json!({ "id": created_id }))
        })
    })
    .await
    .map(|body| (StatusCode::CREATED, Json(body)))
}

pub(crate) async fn delete_budget_window_internal(
    state: &ApiAppState,
    actor: &llmroller_core::rbac::ActorScope,
    id: String,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let key: Option<String> =
        sqlx::query_scalar("SELECT upstream_key_id FROM budget_windows WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.auth.reader_pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let key = key.ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "window not found"))?;
    authorize_budget_window_key(state, actor, &key).await?;
    execute_atomic_mutation(state, vec![ConfigDomain::BudgetWindows], move |tx| {
        Box::pin(async move {
            sqlx::query("DELETE FROM budget_windows WHERE id = ?")
                .bind(&id)
                .execute(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok(())
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn get_user_budget_windows_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    list_budget_windows_internal(&state, &actor, &id).await
}

pub(crate) async fn post_user_budget_window_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<CreateBudgetWindowRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    create_budget_window_internal(&state, &actor, id, payload).await
}

pub(crate) async fn delete_user_budget_window_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    delete_budget_window_internal(&state, &actor, id).await
}

pub(crate) async fn get_admin_budget_windows_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    list_budget_windows_internal(&state, &actor, &id).await
}

pub(crate) async fn post_admin_budget_window_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<CreateBudgetWindowRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    create_budget_window_internal(&state, &actor, id, payload).await
}

pub(crate) async fn delete_admin_budget_window_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    delete_budget_window_internal(&state, &actor, id).await
}

pub(crate) async fn patch_budget_window_internal(
    state: &ApiAppState,
    actor: &llmroller_core::rbac::ActorScope,
    id: String,
    payload: PatchBudgetWindowRequest,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        let row = sqlx::query(
        "SELECT scope_kind, upstream_key_id, model_pk, window_kind, period_ms, anchor_at,
                upstream_window_kind, limit_micro_usd, reserve_pct
         FROM budget_windows WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "window not found"))?;

    let upstream_key_id: String = row.try_get("upstream_key_id").unwrap_or_default();
    authorize_budget_window_key(state, actor, &upstream_key_id).await?;

    let scope_kind: String = row.try_get("scope_kind").unwrap_or_default();
    let model_pk: Option<String> = row.try_get("model_pk").unwrap_or_default();
    let window_kind: String = row
        .try_get::<String, _>("window_kind")
        .unwrap_or_else(|_| "rolling".to_string());
    let window_kind = payload.window_kind.unwrap_or(window_kind);
    let period_ms = payload.period_ms.or_else(|| {
        row.try_get::<Option<i64>, _>("period_ms")
            .unwrap_or_default()
    });
    let anchor_at = payload.anchor_at.or_else(|| {
        row.try_get::<Option<i64>, _>("anchor_at")
            .unwrap_or_default()
    });
    let upstream_window_kind = payload.upstream_window_kind.or_else(|| {
        row.try_get::<Option<String>, _>("upstream_window_kind")
            .unwrap_or_default()
    });
    let limit_micro_usd = payload
        .limit_micro_usd
        .unwrap_or_else(|| row.try_get::<i64, _>("limit_micro_usd").unwrap_or(0));
    let reserve_pct = payload
        .reserve_pct
        .unwrap_or_else(|| row.try_get::<i64, _>("reserve_pct").unwrap_or(2));

    validate_budget_window(
        &scope_kind,
        model_pk.as_deref(),
        &window_kind,
        period_ms,
        limit_micro_usd,
        reserve_pct,
    )?;

    let updated = serde_json::json!({
        "id": id.clone(),
        "scope_kind": scope_kind,
        "upstream_key_id": upstream_key_id,
        "model_pk": model_pk,
        "window_kind": window_kind,
        "period_ms": period_ms,
        "anchor_at": anchor_at,
        "upstream_window_kind": upstream_window_kind,
        "limit_micro_usd": limit_micro_usd,
        "reserve_pct": reserve_pct,
    });
    let response = updated.clone();
    execute_atomic_mutation(state, vec![ConfigDomain::BudgetWindows], move |tx| {
        Box::pin(async move {
            sqlx::query(
                "UPDATE budget_windows SET window_kind = ?, period_ms = ?, anchor_at = ?,
                        upstream_window_kind = ?, limit_micro_usd = ?, reserve_pct = ?
                 WHERE id = ?",
            )
            .bind(&window_kind)
            .bind(period_ms)
            .bind(anchor_at)
            .bind(&upstream_window_kind)
            .bind(limit_micro_usd)
            .bind(reserve_pct)
            .bind(&id)
            .execute(&mut **tx)
            .await
            .map_err(|e| map_price_error(StatusCode::UNPROCESSABLE_ENTITY, e))?;
            Ok(())
        })
    })
    .await?;
    Ok(Json(response))
}

pub(crate) async fn patch_user_budget_window_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<PatchBudgetWindowRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    patch_budget_window_internal(&state, &actor, id, payload).await
}

pub(crate) async fn patch_admin_budget_window_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<PatchBudgetWindowRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    patch_budget_window_internal(&state, &actor, id, payload).await
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateBudgetAdjustmentRequest {
    pub delta_micro_usd: i64,
    pub effective_at: Option<i64>,
    pub window_instance_start: Option<i64>,
    pub note: Option<String>,
}

pub(crate) async fn get_budget_window_adjustments_internal(
    state: &ApiAppState,
    actor: &llmroller_core::rbac::ActorScope,
    id: String,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        let row = sqlx::query("SELECT upstream_key_id FROM budget_windows WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.auth.reader_pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "window not found"))?;

    let upstream_key_id: String = row.try_get("upstream_key_id").unwrap_or_default();
    authorize_budget_window_key(state, actor, &upstream_key_id).await?;

    let adjustments =
        llmroller_store::budgets::list_budget_adjustments_for_window(&state.auth.reader_pool, &id)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let list: Vec<serde_json::Value> = adjustments
        .into_iter()
        .map(|a| {
            serde_json::json!({
                "id": a.id,
                "budget_window_id": a.budget_window_id,
                "actor_user_id": a.actor_user_id,
                "delta_micro_usd": a.delta_micro_usd,
                "effective_at": a.effective_at,
                "window_instance_start": a.window_instance_start,
                "source": a.source,
                "note": a.note,
                "created_at": a.created_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "adjustments": list })))
}

pub(crate) async fn post_budget_window_adjustment_internal(
    state: &ApiAppState,
    actor: &llmroller_core::rbac::ActorScope,
    id: String,
    payload: CreateBudgetAdjustmentRequest,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
        let row = sqlx::query("SELECT upstream_key_id FROM budget_windows WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.auth.reader_pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "window not found"))?;

    let upstream_key_id: String = row.try_get("upstream_key_id").unwrap_or_default();
    authorize_budget_window_key(state, actor, &upstream_key_id).await?;

    let now = state.auth.clock.now_ms();
    let effective_at = payload.effective_at.unwrap_or(now);
    let adj_id = llmroller_core::ids::RequestId::generate().to_string();
    let actor_id = actor.user_id().map(|u| u.to_string());

    let response = serde_json::json!({
        "id": adj_id.clone(),
        "budget_window_id": id.clone(),
        "actor_user_id": actor_id.clone(),
        "delta_micro_usd": payload.delta_micro_usd,
        "effective_at": effective_at,
        "window_instance_start": payload.window_instance_start,
        "source": "manual_calibration",
        "note": payload.note.clone(),
        "created_at": now,
    });

    let window_id_clone = id.clone();
    let adj_id_clone = adj_id.clone();
    let note_clone = payload.note;
    let delta = payload.delta_micro_usd;
    let win_inst = payload.window_instance_start;

    execute_atomic_mutation(state, vec![ConfigDomain::BudgetWindows], move |tx| {
        Box::pin(async move {
            llmroller_store::budgets::insert_budget_adjustment_tx(
                tx,
                &adj_id_clone,
                &window_id_clone,
                actor_id.as_deref(),
                delta,
                effective_at,
                win_inst,
                note_clone.as_deref(),
                now,
            )
            .await
            .map_err(|e| map_price_error(StatusCode::UNPROCESSABLE_ENTITY, e))?;
            Ok(())
        })
    })
    .await?;

    Ok(Json(response))
}

pub(crate) async fn get_user_budget_window_adjustments_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    get_budget_window_adjustments_internal(&state, &actor, id).await
}

pub(crate) async fn post_user_budget_window_adjustment_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<CreateBudgetAdjustmentRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    post_budget_window_adjustment_internal(&state, &actor, id, payload).await
}

pub(crate) async fn get_admin_budget_window_adjustments_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    get_budget_window_adjustments_internal(&state, &actor, id).await
}

pub(crate) async fn post_admin_budget_window_adjustment_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<CreateBudgetAdjustmentRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    post_budget_window_adjustment_internal(&state, &actor, id, payload).await
}

