use super::*;
use sqlx::Row as _;

/// `GET /admin/api/service-accounts` (`CLK-6`).
pub(crate) async fn get_admin_service_accounts_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<llmroller_store::service_accounts::ServiceAccount>>, MutationResponseError> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let accounts = list_service_accounts(&state.auth.pool, &actor, None, None)
        .await
        .map_err(map_service_account_error)?;
    Ok(Json(accounts))
}

/// `POST /admin/api/service-accounts` (`CLK-6`).
pub(crate) async fn post_admin_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<NewServiceAccount>,
) -> Result<
    (
        StatusCode,
        Json<llmroller_store::service_accounts::ServiceAccount>,
    ),
    MutationResponseError,
> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let owner = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();
    let account = execute_atomic_mutation(&state, vec![ConfigDomain::ServiceAccounts], move |tx| {
        Box::pin(async move {
            create_service_account_tx(tx, &actor, owner, payload, now_ms)
                .await
                .map_err(map_service_account_error)
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(account)))
}

/// `GET /admin/api/service-accounts/{id}` (`CLK-6`).
pub(crate) async fn get_admin_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<llmroller_store::service_accounts::ServiceAccount>, MutationResponseError> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let account =
        llmroller_store::service_accounts::get_service_account(&state.auth.pool, &actor, &id)
            .await
            .map_err(map_service_account_error)?;
    Ok(Json(account))
}

/// `PATCH /admin/api/service-accounts/{id}` (`CLK-6`).
pub(crate) async fn patch_admin_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<ServiceAccountPatch>,
) -> Result<Json<llmroller_store::service_accounts::ServiceAccount>, MutationResponseError> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let account = execute_atomic_mutation(&state, vec![ConfigDomain::ServiceAccounts], move |tx| {
        Box::pin(async move {
            patch_service_account_tx(tx, &actor, &id, payload, now_ms)
                .await
                .map_err(map_service_account_error)
        })
    })
    .await?;
    Ok(Json(account))
}

/// `PUT /admin/api/service-accounts/{id}/ip-restrictions` (`CLK-6`, Section 14.k.4).
pub(crate) async fn put_admin_service_account_ip_restrictions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<IpRestrictionsRequest>,
) -> Result<Json<llmroller_store::service_accounts::ServiceAccount>, MutationResponseError> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let cidrs = if let Some(text) = payload.allowed_cidrs_text {
        llmroller_store::service_accounts::parse_cidrs_text(&text)
            .map_err(map_service_account_error)?
    } else if payload.allowed_cidrs.is_empty() {
        None
    } else {
        Some(payload.allowed_cidrs)
    };
    let now_ms = state.auth.clock.now_ms();
    let account = execute_atomic_mutation(&state, vec![ConfigDomain::ServiceAccounts], move |tx| {
        Box::pin(async move {
            set_service_account_cidrs_tx(tx, &actor, &id, cidrs, now_ms)
                .await
                .map_err(map_service_account_error)
        })
    })
    .await?;
    Ok(Json(account))
}

/// `DELETE /admin/api/service-accounts/{id}` (`CLK-6`).
pub(crate) async fn collect_service_account_client_keys_tx(
    tx: &mut Transaction<'_, Sqlite>,
    service_account_id: &str,
) -> Vec<ClientKeyId> {
        let rows = sqlx::query("SELECT id FROM client_keys WHERE service_account_id = ?")
        .bind(service_account_id)
        .fetch_all(&mut **tx)
        .await
        .unwrap_or_default();
    rows.into_iter()
        .filter_map(|r| r.try_get::<String, _>("id").ok())
        .filter_map(|s| ClientKeyId::parse(&s).ok())
        .collect()
}

pub(crate) async fn delete_admin_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, MutationResponseError> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let mut tx_ro = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|_| writer_busy_error())?;
    let child_keys = collect_service_account_client_keys_tx(&mut tx_ro, &id).await;
    let _ = tx_ro.commit().await;
    let invalidations: Vec<llmroller_core::snapshot::CacheInvalidationEvent> = child_keys
        .iter()
        .copied()
        .map(llmroller_core::snapshot::CacheInvalidationEvent::ClientKeyChanged)
        .collect();
    execute_atomic_mutation_with_invalidations(
        &state,
        vec![ConfigDomain::ServiceAccounts, ConfigDomain::ClientKeys],
        invalidations,
        move |tx| {
            Box::pin(async move {
                delete_service_account_tx(tx, &actor, &id)
                    .await
                    .map_err(map_service_account_error)
            })
        },
    )
    .await?;
    if let Some(store) = &state.pin_store {
        let set: HashSet<ClientKeyId> = child_keys.into_iter().collect();
        store.invalidate_client_keys(&set);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/user/service-accounts/{id}/keys` (`CLK-6`).
pub(crate) async fn post_user_service_account_keys_handler(
    state: State<ApiAppState>,
    headers: HeaderMap,
    Path(service_account_id): Path<String>,
    Json(mut payload): Json<CreateClientKeyRequest>,
) -> Result<(StatusCode, Json<ClientKeyCreationResponse>), (StatusCode, Json<serde_json::Value>)> {
    payload.service_account_id = Some(service_account_id);
    post_user_client_key_handler(state, headers, Json(payload)).await
}

/// `GET /api/user/service-accounts` (`CLK-6`, Section 14.j).
pub(crate) async fn get_user_service_accounts_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<llmroller_store::service_accounts::ServiceAccount>>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let accounts = list_service_accounts(&state.auth.pool, &actor, Some("personal"), None)
        .await
        .map_err(map_service_account_error)?;
    Ok(Json(accounts))
}

/// `POST /api/user/service-accounts` (`CLK-6`, Section 14.j.4).
pub(crate) async fn post_user_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<NewServiceAccount>,
) -> Result<
    (
        StatusCode,
        Json<llmroller_store::service_accounts::ServiceAccount>,
    ),
    MutationResponseError,
> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let owner = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();
    let account = execute_atomic_mutation(&state, vec![ConfigDomain::ServiceAccounts], move |tx| {
        Box::pin(async move {
            create_service_account_tx(tx, &actor, owner, payload, now_ms)
                .await
                .map_err(map_service_account_error)
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(account)))
}

/// `GET /api/user/service-accounts/{id}` (`CLK-6`).
pub(crate) async fn get_user_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<llmroller_store::service_accounts::ServiceAccount>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let account =
        llmroller_store::service_accounts::get_service_account(&state.auth.pool, &actor, &id)
            .await
            .map_err(map_service_account_error)?;
    Ok(Json(account))
}

/// `PATCH /api/user/service-accounts/{id}` (`CLK-6`).
pub(crate) async fn patch_user_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<ServiceAccountPatch>,
) -> Result<Json<llmroller_store::service_accounts::ServiceAccount>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let account = execute_atomic_mutation(&state, vec![ConfigDomain::ServiceAccounts], move |tx| {
        Box::pin(async move {
            patch_service_account_tx(tx, &actor, &id, payload, now_ms)
                .await
                .map_err(map_service_account_error)
        })
    })
    .await?;
    Ok(Json(account))
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct IpRestrictionsRequest {
    #[serde(default)]
    pub allowed_cidrs: Vec<String>,
    /// Textarea form alternative accepted by HTMX clients (Section 14.k.4).
    #[serde(default)]
    pub allowed_cidrs_text: Option<String>,
}

/// `PUT /api/user/service-accounts/{id}/ip-restrictions` (`CLK-6`, Section 14.k.4).
pub(crate) async fn put_user_service_account_ip_restrictions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<IpRestrictionsRequest>,
) -> Result<Json<llmroller_store::service_accounts::ServiceAccount>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let cidrs = if let Some(text) = payload.allowed_cidrs_text {
        llmroller_store::service_accounts::parse_cidrs_text(&text)
            .map_err(map_service_account_error)?
    } else if payload.allowed_cidrs.is_empty() {
        None
    } else {
        Some(payload.allowed_cidrs)
    };
    let now_ms = state.auth.clock.now_ms();
    let account = execute_atomic_mutation(&state, vec![ConfigDomain::ServiceAccounts], move |tx| {
        Box::pin(async move {
            set_service_account_cidrs_tx(tx, &actor, &id, cidrs, now_ms)
                .await
                .map_err(map_service_account_error)
        })
    })
    .await?;
    Ok(Json(account))
}

/// `DELETE /api/user/service-accounts/{id}` (`CLK-6`).
pub(crate) async fn delete_user_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let mut tx_ro = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|_| writer_busy_error())?;
    let child_keys = collect_service_account_client_keys_tx(&mut tx_ro, &id).await;
    let _ = tx_ro.commit().await;
    let invalidations: Vec<llmroller_core::snapshot::CacheInvalidationEvent> = child_keys
        .iter()
        .copied()
        .map(llmroller_core::snapshot::CacheInvalidationEvent::ClientKeyChanged)
        .collect();
    execute_atomic_mutation_with_invalidations(
        &state,
        vec![ConfigDomain::ServiceAccounts, ConfigDomain::ClientKeys],
        invalidations,
        move |tx| {
            Box::pin(async move {
                delete_service_account_tx(tx, &actor, &id)
                    .await
                    .map_err(map_service_account_error)
            })
        },
    )
    .await?;
    if let Some(store) = &state.pin_store {
        let set: HashSet<ClientKeyId> = child_keys.into_iter().collect();
        store.invalidate_client_keys(&set);
    }
    Ok(StatusCode::NO_CONTENT)
}
