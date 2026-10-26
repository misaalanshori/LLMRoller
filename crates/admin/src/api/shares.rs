use super::*;
use sqlx::Row as _;

pub(crate) async fn get_user_shares_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let outgoing_rows = sqlx::query(
        "SELECT id, provider_id, upstream_key_id, grantee_user_id, allowed_models, max_monthly_micro_usd, accepted_config_fingerprint, admin_approved_at, enabled, created_at, revoked_at FROM provider_shares WHERE owner_user_id = ? ORDER BY created_at DESC"
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut outgoing = Vec::new();
    for r in outgoing_rows {
        outgoing.push(serde_json::json!({
            "id": r.try_get::<String, _>("id").unwrap_or_default(),
            "provider_id": r.try_get::<String, _>("provider_id").unwrap_or_default(),
            "upstream_key_id": r.try_get::<Option<String>, _>("upstream_key_id").unwrap_or_default(),
            "grantee_user_id": r.try_get::<String, _>("grantee_user_id").unwrap_or_default(),
            "allowed_models": r.try_get::<Option<String>, _>("allowed_models").unwrap_or_default(),
            "max_monthly_micro_usd": r.try_get::<Option<i64>, _>("max_monthly_micro_usd").unwrap_or_default(),
            "accepted_config_fingerprint": r.try_get::<Option<String>, _>("accepted_config_fingerprint").unwrap_or_default(),
            "enabled": r.try_get::<i64, _>("enabled").unwrap_or_default() == 1,
            "created_at": r.try_get::<i64, _>("created_at").unwrap_or_default(),
        }));
    }

    let incoming_rows = sqlx::query(
        "SELECT s.id, s.provider_id, s.upstream_key_id, s.owner_user_id, s.allowed_models, s.max_monthly_micro_usd, s.accepted_config_fingerprint, s.admin_approved_at, s.enabled, p.name as provider_name, p.kind as provider_kind, p.share_config_fingerprint as current_fingerprint FROM provider_shares s JOIN providers p ON s.provider_id = p.id WHERE s.grantee_user_id = ? ORDER BY s.created_at DESC"
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut incoming = Vec::new();
    for r in incoming_rows {
        incoming.push(serde_json::json!({
            "id": r.try_get::<String, _>("id").unwrap_or_default(),
            "provider_id": r.try_get::<String, _>("provider_id").unwrap_or_default(),
            "provider_name": r.try_get::<String, _>("provider_name").unwrap_or_default(),
            "provider_kind": r.try_get::<String, _>("provider_kind").unwrap_or_default(),
            "upstream_key_id": r.try_get::<Option<String>, _>("upstream_key_id").unwrap_or_default(),
            "owner_user_id": r.try_get::<String, _>("owner_user_id").unwrap_or_default(),
            "allowed_models": r.try_get::<Option<String>, _>("allowed_models").unwrap_or_default(),
            "max_monthly_micro_usd": r.try_get::<Option<i64>, _>("max_monthly_micro_usd").unwrap_or_default(),
            "accepted_config_fingerprint": r.try_get::<Option<String>, _>("accepted_config_fingerprint").unwrap_or_default(),
            "current_fingerprint": r.try_get::<String, _>("current_fingerprint").unwrap_or_default(),
            "enabled": r.try_get::<i64, _>("enabled").unwrap_or_default() == 1,
        }));
    }

    Ok(Json(serde_json::json!({
        "outgoing": outgoing,
        "incoming": incoming,
    })))
}

pub(crate) async fn post_user_share_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateShareRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let share = llmroller_store::shares::create_provider_share_tx(
        &mut tx,
        &user_id,
        llmroller_store::shares::NewShare {
            provider_id: payload.provider_id,
            upstream_key_id: payload.upstream_key_id,
            grantee_user_id: payload.grantee_user_id,
            allowed_models: payload.allowed_models,
            max_monthly_micro_usd: payload.max_monthly_micro_usd,
        },
        now_ms,
    )
    .await
    .map_err(|e| match e {
        llmroller_store::shares::ShareError::NotFound => {
            map_price_error(StatusCode::NOT_FOUND, "provider or upstream key not found")
        }
        llmroller_store::shares::ShareError::Forbidden => {
            map_price_error(StatusCode::FORBIDDEN, "forbidden")
        }
        llmroller_store::shares::ShareError::ShareOverlap => {
            map_price_error(StatusCode::CONFLICT, "overlapping share already exists")
        }
        llmroller_store::shares::ShareError::Database(err) => {
            map_price_error(StatusCode::INTERNAL_SERVER_ERROR, err)
        }
    })?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "share_id": share.share_id.to_string() })),
    ))
}

pub(crate) async fn post_accept_share_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ShareId>,
    Json(payload): Json<AcceptShareRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    llmroller_store::shares::accept_provider_share_tx(
        &mut tx,
        &id,
        &user_id,
        &payload.fingerprint,
        now_ms,
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e:?}")))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if headers.contains_key("hx-request") {
        use axum::response::IntoResponse as _;
        Ok(Html("<span class=\"badge badge-success\">Accepted</span>").into_response())
    } else {
        use axum::response::IntoResponse as _;
        Ok(StatusCode::OK.into_response())
    }
}

pub(crate) async fn delete_user_share_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ShareId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();

    let share_info = {
        let mut tx = state
            .auth
            .pool
            .begin()
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let s = llmroller_store::shares::get_provider_share_tx(&mut tx, &id)
            .await
            .map_err(|e| map_price_error(StatusCode::NOT_FOUND, format!("{e:?}")))?;
        llmroller_store::shares::revoke_provider_share_tx(&mut tx, &id, &user_id, now_ms)
            .await
            .map_err(|e| map_price_error(StatusCode::NOT_FOUND, format!("{e:?}")))?;
        let _ = llmroller_store::writer::bump_config_version_tx(&mut tx)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        tx.commit()
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        s
    };
    if let Some(ref store) = state.pin_store {
        let ckids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM client_keys WHERE owner_user_id = ?")
                .bind(share_info.grantee_user_id.to_string())
                .fetch_all(&state.auth.pool)
                .await
                .unwrap_or_default();
        for ckid_str in ckids {
            if let Ok(ckid) = llmroller_core::ids::ClientKeyId::parse(&ckid_str) {
                store.invalidate_client_key(&ckid);
            }
        }
    }
    let _ = record_control_mutation(&state, vec![ConfigDomain::Shares]).await;
    if headers.contains_key("hx-request") {
        Ok(StatusCode::OK)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

// ---------------------------------------------------------------------------
// User Management Endpoints (/admin/api/users)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub(crate) struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub role: Option<String>,
    pub must_change_password: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PatchUserRequest {
    pub role: Option<String>,
    pub disabled: Option<bool>,
    pub can_use_global_providers: Option<bool>,
    pub global_budget_micro_usd_month: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeleteUserParams {
    pub confirm: Option<String>,
}

pub(crate) async fn post_admin_share_approve_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ShareId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let _admin_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    llmroller_store::shares::approve_provider_share_tx(&mut tx, &id, now_ms)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e:?}")))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(StatusCode::OK)
}
