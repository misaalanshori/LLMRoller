use super::*;

pub(crate) async fn get_user_client_keys_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ClientKey>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let keys = list_client_keys(&state.auth.pool, &actor, user_id)
        .await
        .map_err(map_client_key_error)?;
    Ok(Json(keys))
}

pub(crate) async fn post_user_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateClientKeyRequest>,
) -> Result<(StatusCode, Json<ClientKeyCreationResponse>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let new_key = NewClientKey {
        label: payload.label,
        service_account_id: payload.service_account_id,
        routing_scope: payload.routing_scope,
        allowed_models: payload.allowed_models,
        rate_limit_rpm: payload.rate_limit_rpm,
        budget_micro_usd_month: payload.budget_micro_usd_month,
        allowed_cidrs: payload.allowed_cidrs,
        constant_session_mode: payload.constant_session_mode,
        expires_at: payload.expires_at,
    };
    let now_ms = state.auth.clock.now_ms();
    let res = create_client_key(
        &state.auth.pool,
        &actor,
        user_id,
        new_key,
        state.auth.entropy.as_ref(),
        now_ms,
    )
    .await
    .map_err(map_client_key_error)?;

    use llmroller_core::seams::ExposeSecret as _;
    let token_str = res.raw_token.expose_secret().to_string();
    Ok((
        StatusCode::CREATED,
        Json(ClientKeyCreationResponse {
            client_key: res.client_key,
            raw_token: token_str.clone(),
            token: token_str,
        }),
    ))
}

pub(crate) async fn patch_user_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
    Json(payload): Json<ClientKeyPatch>,
) -> Result<Json<ClientKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let key = patch_client_key(&state.auth.pool, &actor, id, payload)
        .await
        .map_err(map_client_key_error)?;
    Ok(Json(key))
}

pub(crate) async fn delete_user_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    delete_client_key(&state.auth.pool, &actor, id)
        .await
        .map_err(map_client_key_error)?;
    if headers.contains_key("hx-request") {
        Ok(StatusCode::OK)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

/// `POST /api/user/client-keys/{id}/pause` (`CLK-7`).
pub(crate) async fn post_pause_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<ClientKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let key = execute_atomic_mutation(&state, vec![ConfigDomain::ClientKeys], move |tx| {
        Box::pin(async move {
            set_client_key_pause_tx(tx, &actor, id, true, now_ms)
                .await
                .map_err(map_client_key_error)
        })
    })
    .await?;
    Ok(Json(key))
}

/// `POST /api/user/client-keys/{id}/resume` (`CLK-7`).
pub(crate) async fn post_resume_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<ClientKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let key = execute_atomic_mutation(&state, vec![ConfigDomain::ClientKeys], move |tx| {
        Box::pin(async move {
            set_client_key_pause_tx(tx, &actor, id, false, now_ms)
                .await
                .map_err(map_client_key_error)
        })
    })
    .await?;
    Ok(Json(key))
}

pub(crate) fn map_service_account_error(
    err: ServiceAccountError,
) -> (StatusCode, Json<serde_json::Value>) {
    let (status, code) = match &err {
        ServiceAccountError::Forbidden => (StatusCode::FORBIDDEN, "permission_denied"),
        ServiceAccountError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
        ServiceAccountError::NameTaken => (StatusCode::CONFLICT, "service_account_name_taken"),
        ServiceAccountError::Validation(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "validation_error")
        }
        ServiceAccountError::QuotaExceeded { .. } => {
            (StatusCode::CONFLICT, "resource_quota_exceeded")
        }
        ServiceAccountError::Sqlx(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    };
    (
        status,
        Json(serde_json::json!({"error": {"code": code, "message": err.to_string()}})),
    )
}

pub(crate) async fn post_rotate_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
    Json(payload): Json<RotateKeyRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let grace = payload.grace_period_secs.unwrap_or(86400) as i64;
    let now_ms = state.auth.clock.now_ms();
    let res = llmroller_store::client_keys::rotate_client_key(
        &state.auth.pool,
        &actor,
        id,
        grace,
        state.auth.entropy.as_ref(),
        now_ms,
    )
    .await
    .map_err(map_client_key_error)?;

    use llmroller_core::seams::ExposeSecret as _;
    let raw = res.raw_token.expose_secret();
    Ok(Json(serde_json::json!({
        "client_key": res.client_key,
        "raw_token": raw,
        "token": raw
    })))
}

// ---------------------------------------------------------------------------
// Sharing Endpoints (/api/user/shares)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub(crate) struct CreateShareRequest {
    pub provider_id: ProviderId,
    pub upstream_key_id: Option<UpstreamKeyId>,
    pub grantee_user_id: UserId,
    pub allowed_models: Option<Vec<String>>,
    pub max_monthly_micro_usd: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AcceptShareRequest {
    pub fingerprint: String,
}
