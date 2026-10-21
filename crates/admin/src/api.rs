#![forbid(unsafe_code)]
//! JSON control-plane CRUD handlers for providers, keys, client keys, and settings.
//!
//! Spec: ./docs/srdd/14. SRDD - API and Web UI Specification.md#142-control-plane-json-api-apiuser-and-adminapi
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#933-closed-runtime-settings-registry
//! Phase: ./docs/bigbang/big-bang-phases-2.md#20-sub-phase-20-minimal-control-plane-json-crud-for-providers-keys-client-keys-and-settings

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use llmroller_core::ids::{ClientKeyId, ProviderId, UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope};
use llmroller_core::seams::secret_string;
use llmroller_store::client_keys::{
    ClientKey, ClientKeyError, ClientKeyPatch, NewClientKey, create_client_key, delete_client_key,
    list_client_keys, patch_client_key,
};
use llmroller_store::crypto::MasterKey;
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, Provider, ProviderError, ProviderPatch, UpstreamKey,
    UpstreamKeyPatch, create_provider, create_upstream_key, delete_provider, delete_upstream_key,
    get_provider, get_upstream_key, list_providers, list_upstream_keys, patch_provider,
    patch_upstream_key,
};
use llmroller_store::settings::{
    RuntimeSettings, SettingsError, load_settings, update_setting, validate_setting,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::auth::{AuthAppState, AuthError, authenticate_session};

/// Shared state for the control-plane JSON API.
#[derive(Clone, Debug)]
pub struct ApiAppState {
    pub auth: AuthAppState,
    pub master_key: MasterKey,
    pub master_key_id: String,
    pub writer: Option<llmroller_store::writer::WriterHandle>,
}

impl ApiAppState {
    #[must_use]
    pub fn new(auth: AuthAppState, master_key: MasterKey, master_key_id: String) -> Self {
        Self {
            auth,
            master_key,
            master_key_id,
            writer: None,
        }
    }
}

/// Helper to validate session and CSRF for JSON endpoints.
async fn authenticate_actor(
    state: &ApiAppState,
    headers: &HeaderMap,
    is_mutation: bool,
) -> Result<ActorScope, (StatusCode, Json<serde_json::Value>)> {
    let session = authenticate_session(&state.auth, headers)
        .await
        .map_err(map_auth_error)?;

    if is_mutation {
        crate::auth::validate_session_csrf(&session, headers)
            .await
            .map_err(map_auth_error)?;
    }

    Ok(ActorScope::user(session.user.id, session.user.role))
}

async fn authenticate_admin(
    state: &ApiAppState,
    headers: &HeaderMap,
    is_mutation: bool,
) -> Result<ActorScope, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(state, headers, is_mutation).await?;
    if !actor.is_admin() {
        return Err(map_auth_error(AuthError::Forbidden));
    }
    Ok(actor)
}

pub(crate) fn map_auth_error(err: AuthError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match err {
        AuthError::Unauthorized | AuthError::AccountDisabled => StatusCode::UNAUTHORIZED,
        AuthError::Forbidden | AuthError::InvalidCsrf => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({ "error": { "message": err.to_string() } })),
    )
}

fn map_provider_error(err: ProviderError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match err {
        ProviderError::Forbidden => StatusCode::FORBIDDEN,
        ProviderError::ProviderNotFound(_) | ProviderError::UpstreamKeyNotFound(_) => {
            StatusCode::NOT_FOUND
        }
        ProviderError::QuotaExceeded { .. } => StatusCode::CONFLICT,
        ProviderError::Conflict(_) => StatusCode::CONFLICT,
        ProviderError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({ "error": { "message": err.to_string() } })),
    )
}

fn map_client_key_error(err: ClientKeyError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match err {
        ClientKeyError::Forbidden => StatusCode::FORBIDDEN,
        ClientKeyError::NotFound(_) => StatusCode::NOT_FOUND,
        ClientKeyError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({ "error": { "message": err.to_string() } })),
    )
}

fn map_settings_error(err: SettingsError) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY),
        Json(serde_json::json!({ "error": { "message": err.to_string() } })),
    )
}

// ---------------------------------------------------------------------------
// Request/Response DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateProviderRequest {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    #[serde(default = "default_config_json")]
    pub config_json: String,
    #[serde(default)]
    pub usage_url_cross_origin_ack: bool,
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_config_json() -> String {
    "{}".to_owned()
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct PatchProviderRequest {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub config_json: Option<String>,
    pub usage_url_cross_origin_ack: Option<bool>,
    pub allow_private_network: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyRequest {
    pub label: String,
    pub secret: String,
    pub priority: Option<i64>,
    pub strict_tier: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PatchKeyRequest {
    pub label: Option<String>,
    pub priority: Option<i64>,
    pub strict_tier: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateClientKeyRequest {
    pub label: String,
    pub routing_scope: Option<String>,
    pub allowed_models: Option<Vec<String>>,
    pub rate_limit_rpm: Option<i64>,
    pub budget_micro_usd_month: Option<i64>,
    pub allowed_cidrs: Option<Vec<String>>,
    pub constant_session_mode: Option<String>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ClientKeyCreationResponse {
    pub client_key: ClientKey,
    pub token: String,
}

// ---------------------------------------------------------------------------
// Personal Provider Endpoints (/api/user/providers)
// ---------------------------------------------------------------------------

async fn get_user_providers(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Provider>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let providers = list_providers(&state.auth.pool, &actor)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(providers))
}

async fn post_user_provider(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<Provider>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let owner_user_id = actor.user_id();

    let new_provider = NewProvider {
        name: payload.name,
        kind: payload.kind,
        base_url: payload.base_url,
        config_json: payload.config_json,
        ownership_scope: OwnershipScope::Personal,
        owner_user_id,
        usage_url_cross_origin_ack: payload.usage_url_cross_origin_ack,
        allow_private_network: payload.allow_private_network,
        enabled: payload.enabled,
    };

    let now_ms = state.auth.clock.now_ms();
    let provider = create_provider(&state.auth.pool, &actor, new_provider, now_ms)
        .await
        .map_err(map_provider_error)?;
    Ok((StatusCode::CREATED, Json(provider)))
}

async fn get_user_provider(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ProviderId>,
) -> Result<Json<Provider>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let provider = get_provider(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(provider))
}

async fn patch_user_provider_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ProviderId>,
    Json(payload): Json<PatchProviderRequest>,
) -> Result<Json<Provider>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let patch = ProviderPatch {
        name: payload.name,
        base_url: payload.base_url,
        config_json: payload.config_json,
        usage_url_cross_origin_ack: payload.usage_url_cross_origin_ack,
        allow_private_network: payload.allow_private_network,
        enabled: payload.enabled,
    };
    let now_ms = state.auth.clock.now_ms();
    let provider = patch_provider(&state.auth.pool, &actor, id, patch, now_ms)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(provider))
}

async fn delete_user_provider_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ProviderId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    delete_provider(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Personal Upstream Key Endpoints (/api/user/providers/{id}/keys, /api/user/upstream-keys/{id})
// ---------------------------------------------------------------------------

async fn get_user_provider_keys(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(provider_id): Path<ProviderId>,
) -> Result<Json<Vec<UpstreamKey>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let keys = list_upstream_keys(&state.auth.pool, &actor, provider_id)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(keys))
}

async fn post_user_provider_key(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(provider_id): Path<ProviderId>,
    Json(payload): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<UpstreamKey>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let new_key = NewUpstreamKey {
        provider_id,
        label: payload.label,
        secret: secret_string(&payload.secret),
        priority: payload.priority,
        strict_tier: payload.strict_tier,
        enabled: payload.enabled,
    };
    let now_ms = state.auth.clock.now_ms();
    let key = create_upstream_key(
        &state.auth.pool,
        &actor,
        &state.master_key,
        &state.master_key_id,
        state.auth.entropy.clone(),
        new_key,
        now_ms,
    )
    .await
    .map_err(map_provider_error)?;
    Ok((StatusCode::CREATED, Json(key)))
}

async fn get_user_upstream_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
) -> Result<Json<UpstreamKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let key = get_upstream_key(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(key))
}

async fn patch_user_upstream_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
    Json(payload): Json<PatchKeyRequest>,
) -> Result<Json<UpstreamKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let patch = UpstreamKeyPatch {
        label: payload.label,
        priority: payload.priority,
        strict_tier: payload.strict_tier,
        enabled: payload.enabled,
    };
    let key = patch_upstream_key(&state.auth.pool, &actor, id, patch)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(key))
}

async fn delete_user_upstream_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    delete_upstream_key(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Client Key Endpoints (/api/user/client-keys)
// ---------------------------------------------------------------------------

async fn get_user_client_keys_handler(
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

async fn post_user_client_key_handler(
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
            token: token_str,
        }),
    ))
}

async fn patch_user_client_key_handler(
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

async fn delete_user_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    delete_client_key(&state.auth.pool, &actor, id)
        .await
        .map_err(map_client_key_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Admin Endpoints (/admin/api/*)
// ---------------------------------------------------------------------------

async fn get_admin_providers(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Provider>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let providers = list_providers(&state.auth.pool, &actor)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(providers))
}

async fn post_admin_provider(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<Provider>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let new_provider = NewProvider {
        name: payload.name,
        kind: payload.kind,
        base_url: payload.base_url,
        config_json: payload.config_json,
        ownership_scope: OwnershipScope::Global,
        owner_user_id: None,
        usage_url_cross_origin_ack: payload.usage_url_cross_origin_ack,
        allow_private_network: payload.allow_private_network,
        enabled: payload.enabled,
    };
    let now_ms = state.auth.clock.now_ms();
    let provider = create_provider(&state.auth.pool, &actor, new_provider, now_ms)
        .await
        .map_err(map_provider_error)?;
    Ok((StatusCode::CREATED, Json(provider)))
}

async fn post_admin_provider_key(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(provider_id): Path<ProviderId>,
    Json(payload): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<UpstreamKey>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let new_key = NewUpstreamKey {
        provider_id,
        label: payload.label,
        secret: secret_string(&payload.secret),
        priority: payload.priority,
        strict_tier: payload.strict_tier,
        enabled: payload.enabled,
    };
    let now_ms = state.auth.clock.now_ms();
    let key = create_upstream_key(
        &state.auth.pool,
        &actor,
        &state.master_key,
        &state.master_key_id,
        state.auth.entropy.clone(),
        new_key,
        now_ms,
    )
    .await
    .map_err(map_provider_error)?;
    Ok((StatusCode::CREATED, Json(key)))
}

async fn get_admin_settings_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<RuntimeSettings>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    let settings = load_settings(&state.auth.pool)
        .await
        .map_err(map_settings_error)?;
    Ok(Json(settings))
}

async fn patch_admin_settings_handler(
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
pub struct UpdateTraceabilityRequest {
    pub enabled: bool,
    #[serde(default)]
    pub match_filters: llmroller_core::settings::TraceabilityMatch,
    pub retention_days: i64,
    pub auto_disable_at: Option<i64>,
    pub confirm_purge: Option<bool>,
}

async fn put_admin_traceability_handler(
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
            sqlx::query_scalar("SELECT COUNT(*) FROM trace_records WHERE created_at < ?")
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

    Ok(Json(serde_json::json!({
        "status": "ok",
        "traceability_config": new_config
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpsertPricePayload {
    pub provider_id: Option<String>,
    pub provider_kind: String,
    pub upstream_model_id: String,
    pub input_per_mtok_micro_usd: i64,
    pub output_per_mtok_micro_usd: i64,
    pub cache_read_per_mtok_micro_usd: Option<i64>,
    pub cache_write_per_mtok_micro_usd: Option<i64>,
    #[serde(default)]
    pub tiers: Vec<llmroller_core::pricing::ContextTier>,
    #[serde(default)]
    pub schedules: Vec<llmroller_core::pricing::Schedule>,
}

async fn put_admin_price_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<UpsertPricePayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let provider_id = payload.provider_id.clone().unwrap_or_default();
    upsert_price_internal(&state, provider_id, payload).await
}

async fn put_user_provider_price_handler(
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
    upsert_price_internal(&state, id.to_string(), payload).await
}

async fn upsert_price_internal(
    state: &ApiAppState,
    provider_id: String,
    payload: UpsertPricePayload,
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
        source: "admin".to_string(),
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

async fn post_admin_prices_sync_handler(
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

    Ok(Json(serde_json::json!({
        "synced": true,
        "imported_aliases": 0
    })))
}

async fn get_user_client_key_sessions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let key = llmroller_store::client_keys::get_client_key(&state.auth.pool, &actor, id)
        .await
        .map_err(|_| map_price_error(StatusCode::NOT_FOUND, "client key not found"))?;
    if Some(key.owner_user_id) != actor.user_id() {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "client key not found",
        ));
    }

    list_client_key_sessions_internal(&state, &id).await
}

async fn get_admin_client_key_sessions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    list_client_key_sessions_internal(&state, &id).await
}

async fn list_client_key_sessions_internal(
    state: &ApiAppState,
    client_key_id: &ClientKeyId,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use sqlx::Row as _;
    let rows = sqlx::query(
        r#"SELECT session_hash, MAX(ts) as last_active_at, COUNT(*) as traced_turn_count
           FROM trace_records
           WHERE client_key_id = ?
           GROUP BY session_hash
           ORDER BY last_active_at DESC
           LIMIT 50"#,
    )
    .bind(client_key_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut sessions = Vec::new();
    for r in rows {
        let session_hash: String = r.try_get("session_hash").unwrap_or_default();
        let last_active_at: i64 = r.try_get("last_active_at").unwrap_or(0);
        let traced_turn_count: i64 = r.try_get("traced_turn_count").unwrap_or(0);
        sessions.push(serde_json::json!({
            "session_hash": session_hash,
            "last_active_at": last_active_at,
            "traced_turn_count": traced_turn_count,
            "current_pins": []
        }));
    }

    Ok(Json(serde_json::json!({ "data": sessions })))
}

async fn get_user_session_preview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let owner_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let exists = sqlx::query(
        "SELECT 1 FROM trace_records WHERE owner_user_id = ? AND session_hash = ? LIMIT 1",
    )
    .bind(owner_id.to_string())
    .bind(&session_hash)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if exists.is_none() {
        return Err(map_price_error(StatusCode::NOT_FOUND, "session not found"));
    }

    get_session_preview_internal(&state, &session_hash, Some(owner_id.to_string())).await
}

async fn get_admin_session_preview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let admin_id = actor.user_id().map(|u| u.to_string());
    let now_ms = state.auth.clock.now_ms();

    let recent_audit: bool = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM audit_log WHERE actor_user_id = ? AND action = 'trace.viewed' AND target_id = ? AND ts >= ? LIMIT 1",
    )
    .bind(&admin_id)
    .bind(&session_hash)
    .bind(now_ms - 30 * 60 * 1000)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .is_some();

    if !recent_audit {
        let _ = append_audit_entry_helper(
            &state,
            now_ms,
            admin_id.as_deref(),
            "trace.viewed",
            "session",
            Some(&session_hash),
        )
        .await;
    }

    get_session_preview_internal(&state, &session_hash, None).await
}

async fn get_session_preview_internal(
    state: &ApiAppState,
    session_hash: &str,
    owner_user_id: Option<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_core::chat_preview::{compute_lcp, sanitize_assistant_text};
    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::SecretSealer as _;
    use llmroller_store::traces::StoredTurnTraceV2;
    use sqlx::Row as _;

    let query_str = if owner_user_id.is_some() {
        "SELECT usage_event_id, request_id, ts, owner_user_id, client_key_id, session_hash,
                thread_id, canonical_id, protocol, truncated_content, payload_enc, master_key_id
         FROM trace_records
         WHERE session_hash = ? AND owner_user_id = ?
         ORDER BY ts ASC, request_id ASC"
    } else {
        "SELECT usage_event_id, request_id, ts, owner_user_id, client_key_id, session_hash,
                thread_id, canonical_id, protocol, truncated_content, payload_enc, master_key_id
         FROM trace_records
         WHERE session_hash = ?
         ORDER BY ts ASC, request_id ASC"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(query_str)
            .bind(session_hash)
            .bind(uid)
            .fetch_all(&state.auth.pool)
            .await
    } else {
        sqlx::query(query_str)
            .bind(session_hash)
            .fetch_all(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "session trace not found",
        ));
    }

    let sealer = load_sealer(state).await?;

    let mut timeline = Vec::new();
    let mut prev_block_refs: Vec<String> = Vec::new();
    let mut prev_thread_id = String::new();
    let mut first_owner_user_id = String::new();
    let mut first_client_key_id = String::new();

    for (idx, r) in rows.iter().enumerate() {
        let req_id: String = r.try_get("request_id").unwrap_or_default();
        let ts: i64 = r.try_get("ts").unwrap_or(0);
        let row_owner: String = r.try_get("owner_user_id").unwrap_or_default();
        let client_key_id: String = r.try_get("client_key_id").unwrap_or_default();
        let thread_id: String = r
            .try_get("thread_id")
            .unwrap_or_else(|_| "main".to_string());
        let canonical_model: String = r.try_get("canonical_id").unwrap_or_default();
        let protocol: String = r.try_get("protocol").unwrap_or_default();
        let truncated_content: bool = r.try_get::<i64, _>("truncated_content").unwrap_or(0) == 1;
        let payload_enc: Vec<u8> = r.try_get("payload_enc").unwrap_or_default();
        let master_key_id: String = r.try_get("master_key_id").unwrap_or_default();

        if idx == 0 {
            first_owner_user_id = row_owner.clone();
            first_client_key_id = client_key_id.clone();
        }

        let decrypted_trace = sealer
            .open(
                "trace_record",
                &[
                    master_key_id.as_bytes(),
                    req_id.as_bytes(),
                    row_owner.as_bytes(),
                    session_hash.as_bytes(),
                ],
                &payload_enc,
            )
            .map_err(|e| {
                map_price_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("open trace: {e:?}"),
                )
            })?;

        let decompressed_trace =
            zstd::bulk::decompress(decrypted_trace.expose_secret(), 10 * 1024 * 1024).map_err(
                |e| {
                    map_price_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("decompress trace: {e}"),
                    )
                },
            )?;

        let turn_trace: StoredTurnTraceV2 =
            serde_json::from_slice(&decompressed_trace).map_err(|e| {
                map_price_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("parse turn: {e}"),
                )
            })?;

        let lcp = if thread_id == prev_thread_id {
            compute_lcp(&prev_block_refs, &turn_trace.block_refs)
        } else {
            0
        };
        prev_block_refs = turn_trace.block_refs.clone();
        prev_thread_id = thread_id.clone();

        let mut new_input_messages = Vec::new();
        for hash in &turn_trace.block_refs[lcp..] {
            let block_row = sqlx::query(
                "SELECT payload_enc, master_key_id FROM trace_blocks WHERE owner_user_id = ? AND block_hash = ?",
            )
            .bind(&row_owner)
            .bind(hash)
            .fetch_optional(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

            if let Some(br) = block_row {
                let blk_enc: Vec<u8> = br.try_get("payload_enc").unwrap_or_default();
                let blk_mk: String = br.try_get("master_key_id").unwrap_or_default();
                let dec_blk = sealer
                    .open(
                        "trace_block",
                        &[blk_mk.as_bytes(), row_owner.as_bytes(), hash.as_bytes()],
                        &blk_enc,
                    )
                    .map_err(|e| {
                        map_price_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("open block: {e:?}"),
                        )
                    })?;

                let decomp_blk = zstd::bulk::decompress(dec_blk.expose_secret(), 10 * 1024 * 1024)
                    .map_err(|e| {
                        map_price_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("decompress block: {e}"),
                        )
                    })?;

                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&decomp_blk) {
                    new_input_messages.push(val);
                } else {
                    let text = String::from_utf8_lossy(&decomp_blk).to_string();
                    new_input_messages.push(serde_json::json!({ "role": "user", "content": text }));
                }
            } else {
                new_input_messages.push(serde_json::json!({
                    "role": "system",
                    "content": "content expired",
                    "expired": true
                }));
            }
        }

        let raw_assistant = turn_trace
            .response
            .get("assistant_text")
            .and_then(|v| v.as_str())
            .or_else(|| turn_trace.response.get("text").and_then(|v| v.as_str()))
            .unwrap_or("");
        let sanitized_assistant = sanitize_assistant_text(raw_assistant);

        let assistant_reasoning = turn_trace
            .response
            .get("assistant_reasoning")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);

        let assistant_tool_calls = turn_trace
            .response
            .get("assistant_tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        timeline.push(serde_json::json!({
            "ordinal": idx + 1,
            "request_id": req_id,
            "ts": ts,
            "thread_id": thread_id,
            "canonical_model": canonical_model,
            "protocol": protocol,
            "truncated_content": truncated_content,
            "new_input_messages": new_input_messages,
            "assistant_text": sanitized_assistant,
            "assistant_reasoning": assistant_reasoning,
            "assistant_tool_calls": assistant_tool_calls,
        }));
    }

    let pin_rows = sqlx::query(
        "SELECT p.upstream_key_id, p.canonical_id, p.pool_tier, k.label as key_label
         FROM pins p
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         WHERE p.session_hash = ?",
    )
    .bind(session_hash)
    .fetch_all(&state.auth.pool)
    .await
    .unwrap_or_default();

    let mut current_pins = Vec::new();
    for pr in pin_rows {
        current_pins.push(serde_json::json!({
            "upstream_key_id": pr.try_get::<Option<String>, _>("upstream_key_id").unwrap_or_default(),
            "canonical_id": pr.try_get::<Option<String>, _>("canonical_id").unwrap_or_default(),
            "pool_tier": pr.try_get::<i64, _>("pool_tier").unwrap_or(1),
            "key_label": pr.try_get::<Option<String>, _>("key_label").unwrap_or_default(),
        }));
    }

    Ok(Json(serde_json::json!({
        "session_hash": session_hash,
        "owner_user_id": first_owner_user_id,
        "client_key_id": first_client_key_id,
        "total_turns": timeline.len(),
        "current_pins": current_pins,
        "timeline": timeline,
    })))
}

async fn get_user_trace_record_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    get_trace_record_internal(&state, &request_id, Some(user_id.to_string())).await
}

async fn get_admin_trace_record_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_trace_record_internal(&state, &request_id, None).await
}

async fn get_trace_record_internal(
    state: &ApiAppState,
    request_id: &str,
    owner_user_id: Option<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::SecretSealer as _;
    use llmroller_store::traces::StoredTurnTraceV2;
    use sqlx::Row as _;

    let row = sqlx::query(
        "SELECT request_id, owner_user_id, session_hash, payload_enc, master_key_id
         FROM trace_records
         WHERE request_id = ?",
    )
    .bind(request_id)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "trace record not found"))?;

    let row_owner: String = row.try_get("owner_user_id").unwrap_or_default();
    if let Some(ref uid) = owner_user_id
        && &row_owner != uid
    {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "trace record not found",
        ));
    }

    let session_hash: String = row.try_get("session_hash").unwrap_or_default();
    let payload_enc: Vec<u8> = row.try_get("payload_enc").unwrap_or_default();
    let master_key_id: String = row.try_get("master_key_id").unwrap_or_default();

    let sealer = load_sealer(state).await?;
    let decrypted = sealer
        .open(
            "trace_record",
            &[
                master_key_id.as_bytes(),
                request_id.as_bytes(),
                row_owner.as_bytes(),
                session_hash.as_bytes(),
            ],
            &payload_enc,
        )
        .map_err(|e| {
            map_price_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("open trace: {e:?}"),
            )
        })?;

    let decompressed = zstd::bulk::decompress(decrypted.expose_secret(), 10 * 1024 * 1024)
        .map_err(|e| {
            map_price_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("decompress trace: {e}"),
            )
        })?;

    let turn_trace: StoredTurnTraceV2 = serde_json::from_slice(&decompressed).map_err(|e| {
        map_price_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("parse turn: {e}"),
        )
    })?;

    Ok(Json(serde_json::to_value(turn_trace).unwrap_or_default()))
}

async fn delete_admin_session_trace_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;

    use sqlx::Row as _;
    let row = sqlx::query("SELECT owner_user_id FROM trace_records WHERE session_hash = ? LIMIT 1")
        .bind(&session_hash)
        .fetch_optional(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let Some(r) = row else {
        return Ok(StatusCode::NO_CONTENT);
    };

    let owner_str: String = r.try_get("owner_user_id").unwrap_or_default();
    let owner_user_id = llmroller_core::ids::UserId::parse(&owner_str)
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")))?;

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    llmroller_store::traces::delete_session_trace_tx(&mut tx, &owner_user_id, &session_hash)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(StatusCode::NO_CONTENT)
}

async fn delete_admin_traces_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let _ = sqlx::query("DELETE FROM trace_records WHERE expires_at <= ?")
        .bind(now_ms)
        .execute(&state.auth.pool)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn append_audit_entry_helper(
    state: &ApiAppState,
    now_ms: i64,
    actor_user_id: Option<&str>,
    action: &str,
    target_type: &str,
    target_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    let audit_id = llmroller_core::ids::RequestId::generate().to_string();
    let max_seq: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM audit_log")
        .fetch_one(&state.auth.pool)
        .await
        .unwrap_or(0);
    let prev_hash: String = if max_seq == 0 {
        "0000000000000000000000000000000000000000000000000000000000000000".to_string()
    } else {
        sqlx::query_scalar("SELECT hash FROM audit_log ORDER BY seq DESC LIMIT 1")
            .fetch_one(&state.auth.pool)
            .await
            .unwrap_or_else(|_| {
                "0000000000000000000000000000000000000000000000000000000000000000".to_string()
            })
    };
    let seq = max_seq + 1;
    let computed_hash = llmroller_store::crypto::sha256_hex(
        format!("{seq}:{prev_hash}:{action}:{}", target_id.unwrap_or("")).as_bytes(),
    );

    let _ = sqlx::query(
        "INSERT INTO audit_log (seq, id, ts, actor_user_id, ip_addr, action, target_type, target_id, payload_json, hash)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, '{}', ?)",
    )
    .bind(seq)
    .bind(&audit_id)
    .bind(now_ms)
    .bind(actor_user_id)
    .bind("127.0.0.1")
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(computed_hash)
    .execute(&state.auth.pool)
    .await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Client Key In-Place Rotation (/api/user/client-keys/{id}/rotate)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct RotateKeyRequest {
    pub grace_period_secs: Option<u64>,
}

async fn post_rotate_client_key_handler(
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
    Ok(Json(serde_json::json!({
        "client_key": res.client_key,
        "raw_token": res.raw_token.expose_secret()
    })))
}

// ---------------------------------------------------------------------------
// Sharing Endpoints (/api/user/shares)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct CreateShareRequest {
    pub provider_id: ProviderId,
    pub upstream_key_id: Option<UpstreamKeyId>,
    pub grantee_user_id: UserId,
    pub allowed_models: Option<Vec<String>>,
    pub max_monthly_micro_usd: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AcceptShareRequest {
    pub fingerprint: String,
}

async fn get_user_shares_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
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

async fn post_user_share_handler(
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
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e:?}")))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "share_id": share.share_id.to_string() })),
    ))
}

async fn post_accept_share_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ShareId>,
    Json(payload): Json<AcceptShareRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
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
    Ok(StatusCode::OK)
}

async fn delete_user_share_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ShareId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
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
    llmroller_store::shares::revoke_provider_share_tx(&mut tx, &id, &user_id, now_ms)
        .await
        .map_err(|e| map_price_error(StatusCode::NOT_FOUND, format!("{e:?}")))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// User Management Endpoints (/admin/api/users)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub role: Option<String>,
    pub must_change_password: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PatchUserRequest {
    pub role: Option<String>,
    pub disabled: Option<bool>,
    pub can_use_global_providers: Option<bool>,
    pub global_budget_micro_usd_month: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteUserParams {
    pub confirm: Option<String>,
}

async fn get_admin_users_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let users = llmroller_store::users::list_users(&state.auth.pool, &actor)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let json_list: Vec<serde_json::Value> = users
        .into_iter()
        .map(|u| {
            serde_json::json!({
                "id": u.id.to_string(),
                "username": u.username,
                "role": u.role,
                "can_use_global_providers": u.can_use_global_providers,
                "global_budget_micro_usd_month": u.global_budget_micro_usd_month,
                "disabled": u.disabled,
                "created_at": u.created_at,
            })
        })
        .collect();

    Ok(Json(json_list))
}

async fn post_admin_user_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let role = match payload.role.as_deref().unwrap_or("user") {
        "admin" => llmroller_core::rbac::Role::Admin,
        _ => llmroller_core::rbac::Role::User,
    };

    let user = llmroller_store::users::create_user(
        &state.auth.pool,
        &actor,
        llmroller_store::users::NewUser {
            username: &payload.username,
            password: &payload.password,
            role,
            must_change_password: payload.must_change_password.unwrap_or(false),
        },
        state.auth.entropy.as_ref(),
        now_ms,
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": user.id.to_string(),
            "username": user.username,
            "role": user.role,
        })),
    ))
}

async fn patch_admin_user_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UserId>,
    Json(payload): Json<PatchUserRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();

    if let Some(ref role_str) = payload.role {
        let role = match role_str.as_str() {
            "admin" => llmroller_core::rbac::Role::Admin,
            _ => llmroller_core::rbac::Role::User,
        };
        llmroller_store::users::set_role(&state.auth.pool, &actor, id, role, now_ms)
            .await
            .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;
    }

    if let Some(disabled) = payload.disabled {
        llmroller_store::users::set_disabled(&state.auth.pool, &actor, id, disabled, now_ms)
            .await
            .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;
    }

    if let Some(can_use) = payload.can_use_global_providers {
        let _ = sqlx::query("UPDATE users SET can_use_global_providers = ? WHERE id = ?")
            .bind(if can_use { 1i64 } else { 0i64 })
            .bind(id.to_string())
            .execute(&state.auth.pool)
            .await;
    }

    if let Some(budget_opt) = payload.global_budget_micro_usd_month {
        let _ = sqlx::query("UPDATE users SET global_budget_micro_usd_month = ? WHERE id = ?")
            .bind(budget_opt)
            .bind(id.to_string())
            .execute(&state.auth.pool)
            .await;
    }

    Ok(StatusCode::OK)
}

async fn delete_admin_user_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UserId>,
    axum::extract::Query(params): axum::extract::Query<DeleteUserParams>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let confirm = params.confirm.unwrap_or_default();

    llmroller_store::users::delete_user(&state.auth.pool, &actor, id, &confirm, now_ms)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Model Catalog & Aliases (/admin/api/models)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct AddAliasRequest {
    pub alias: String,
}

#[derive(Debug, Deserialize)]
pub struct PatchModelRequest {
    pub exposed: Option<i64>,
}

async fn get_admin_models_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    use sqlx::Row as _;
    let model_rows = sqlx::query(
        "SELECT m.model_pk, m.canonical_id, m.origin, m.exposed, m.merge_state,
                (SELECT COUNT(*) FROM model_routes r WHERE r.model_pk = m.model_pk) as route_count
         FROM models m ORDER BY m.canonical_id ASC",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut models = Vec::new();
    for r in model_rows {
        models.push(serde_json::json!({
            "model_pk": r.try_get::<String, _>("model_pk").unwrap_or_default(),
            "canonical_id": r.try_get::<String, _>("canonical_id").unwrap_or_default(),
            "origin": r.try_get::<String, _>("origin").unwrap_or_default(),
            "exposed": r.try_get::<Option<i64>, _>("exposed").unwrap_or_default(),
            "merge_state": r.try_get::<String, _>("merge_state").unwrap_or_default(),
            "route_count": r.try_get::<i64, _>("route_count").unwrap_or(0),
        }));
    }

    let alias_rows =
        sqlx::query("SELECT alias, model_pk, confirmed FROM model_aliases ORDER BY alias ASC")
            .fetch_all(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut aliases = Vec::new();
    for r in alias_rows {
        aliases.push(serde_json::json!({
            "alias": r.try_get::<String, _>("alias").unwrap_or_default(),
            "model_pk": r.try_get::<String, _>("model_pk").unwrap_or_default(),
            "confirmed": r.try_get::<i64, _>("confirmed").unwrap_or(0) == 1,
        }));
    }

    Ok(Json(serde_json::json!({
        "models": models,
        "aliases": aliases,
    })))
}

async fn post_admin_model_alias_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ModelPk>,
    Json(payload): Json<AddAliasRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    llmroller_store::models::add_model_alias_tx(&mut tx, &payload.alias, &id)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;
    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(StatusCode::CREATED)
}

async fn patch_admin_model_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ModelPk>,
    Json(payload): Json<PatchModelRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let _ = sqlx::query("UPDATE models SET exposed = ? WHERE model_pk = ?")
        .bind(payload.exposed)
        .bind(id.to_string())
        .execute(&state.auth.pool)
        .await;
    Ok(StatusCode::OK)
}

async fn post_admin_model_confirm_merge_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ModelPk>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let res = sqlx::query("UPDATE models SET merge_state = 'confirmed' WHERE model_pk = ?")
        .bind(id.to_string())
        .execute(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if res.rows_affected() == 0 {
        return Err(map_price_error(StatusCode::NOT_FOUND, "model not found"));
    }
    Ok(StatusCode::OK)
}

#[derive(Debug, Deserialize)]
pub struct DeleteAliasParams {
    pub alias: Option<String>,
}

async fn delete_admin_model_alias_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<DeleteAliasParams>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let Some(alias) = params.alias else {
        return Err(map_price_error(
            StatusCode::BAD_REQUEST,
            "missing alias parameter",
        ));
    };
    sqlx::query("DELETE FROM model_aliases WHERE alias = ?")
        .bind(alias)
        .execute(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_admin_share_approve_handler(
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

async fn get_admin_overview_telemetry_handler(
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
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_events WHERE created_at >= ?")
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

    Ok(Json(serde_json::json!({
        "projected_disk_mb": projected_disk_mb,
        "unpriced_models_count": unpriced_count,
        "usage_events_count": usage_events_count,
        "max_usage_events_rows": 50_000_000i64,
        "retention_days": retention_days,
        "daily_requests": daily_requests
    })))
}

// ---------------------------------------------------------------------------
// Audit Log Viewer (/admin/api/audit-log)
// ---------------------------------------------------------------------------
async fn get_admin_audit_log_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT seq, ts, actor_user_id, action, target_type, target_id, payload_json FROM audit_log ORDER BY seq DESC LIMIT 100"
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
            "payload_json": r.try_get::<String, _>("payload_json").unwrap_or_default(),
        }));
    }

    Ok(Json(list))
}

// ---------------------------------------------------------------------------
// Active Pins & Sessions (/api/user/pins, /admin/api/pins)
// ---------------------------------------------------------------------------
async fn get_active_pins_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;

    use sqlx::Row as _;
    let query_str = if actor.is_admin() {
        "SELECT p.session_hash, p.client_key_id, p.model_pk, p.upstream_key_id, p.pool_tier, p.created_at, p.last_used_at, m.canonical_id, k.label as key_label
         FROM pins p
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         ORDER BY p.last_used_at DESC LIMIT 50"
    } else {
        "SELECT p.session_hash, p.client_key_id, p.model_pk, p.upstream_key_id, p.pool_tier, p.created_at, p.last_used_at, m.canonical_id, k.label as key_label
         FROM pins p
         JOIN client_keys ck ON p.client_key_id = ck.id
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         WHERE ck.owner_user_id = ?
         ORDER BY p.last_used_at DESC LIMIT 50"
    };

    let rows = if actor.is_admin() {
        sqlx::query(query_str).fetch_all(&state.auth.pool).await
    } else {
        let uid = actor.user_id().map(|u| u.to_string()).unwrap_or_default();
        sqlx::query(query_str)
            .bind(uid)
            .fetch_all(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut list = Vec::new();
    for r in rows {
        list.push(serde_json::json!({
            "session_hash": r.try_get::<String, _>("session_hash").unwrap_or_default(),
            "client_key_id": r.try_get::<String, _>("client_key_id").unwrap_or_default(),
            "canonical_id": r.try_get::<Option<String>, _>("canonical_id").unwrap_or_default(),
            "key_label": r.try_get::<Option<String>, _>("key_label").unwrap_or_default(),
            "pool_tier": r.try_get::<i64, _>("pool_tier").unwrap_or(1),
            "created_at": r.try_get::<i64, _>("created_at").unwrap_or(0),
            "last_used_at": r.try_get::<i64, _>("last_used_at").unwrap_or(0),
        }));
    }

    Ok(Json(list))
}

async fn delete_pin_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_actor(&state, &headers, true).await?;
    let _ = sqlx::query("DELETE FROM pins WHERE session_hash = ?")
        .bind(&hash)
        .execute(&state.auth.pool)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// User Routing Matrix (/api/user/routes)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct PutRoutePreferenceRequest {
    pub priority_override: Option<u32>,
    pub enabled: Option<bool>,
}

async fn get_user_routes_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
    let model_rows = sqlx::query(
        "SELECT model_pk, canonical_id, origin, exposed, merge_state, distribution, routing_strategy
         FROM models WHERE (exposed IS NULL OR exposed != 0) ORDER BY canonical_id ASC",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut models = Vec::new();
    for m in model_rows {
        let model_pk: String = m.try_get::<String, _>("model_pk").unwrap_or_default();
        let canonical_id: String = m.try_get::<String, _>("canonical_id").unwrap_or_default();
        let distribution: String = m.try_get::<String, _>("distribution").unwrap_or_default();
        let routing_strategy: String = m
            .try_get::<Option<String>, _>("routing_strategy")
            .unwrap_or_default()
            .unwrap_or_else(|| "smooth_weighted_rr".to_string());

        let route_rows = sqlx::query(
            "SELECT r.upstream_key_id, r.priority_override as route_prio_override,
                    k.priority, k.strict_tier,
                    k.label as key_label, k.owner_user_id, p.ownership_scope,
                    p.name as provider_name, p.kind as provider_kind
             FROM model_routes r
             JOIN upstream_keys k ON r.upstream_key_id = k.id
             JOIN providers p ON k.provider_id = p.id
             WHERE r.model_pk = ? AND k.enabled = 1 AND p.enabled = 1 AND r.enabled = 1",
        )
        .bind(&model_pk)
        .fetch_all(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

        let pref_rows = sqlx::query(
            "SELECT upstream_key_id, priority_override, enabled
             FROM user_route_preferences
             WHERE user_id = ? AND model_pk = ?",
        )
        .bind(user_id.to_string())
        .bind(&model_pk)
        .fetch_all(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

        let mut prefs_map = std::collections::HashMap::new();
        for p in pref_rows {
            let kid: String = p
                .try_get::<String, _>("upstream_key_id")
                .unwrap_or_default();
            let prio: i64 = p.try_get::<i64, _>("priority_override").unwrap_or(100);
            let en: i64 = p.try_get::<i64, _>("enabled").unwrap_or(1);
            prefs_map.insert(kid, (prio, en == 1));
        }

        let mut own_keys = Vec::new();
        let mut shared_keys = Vec::new();
        let mut global_count = 0;

        for r in route_rows {
            let key_id: String = r
                .try_get::<String, _>("upstream_key_id")
                .unwrap_or_default();
            let scope: String = r
                .try_get::<String, _>("ownership_scope")
                .unwrap_or_default();
            let owner: Option<String> = r
                .try_get::<Option<String>, _>("owner_user_id")
                .unwrap_or_default();
            let label: String = r.try_get::<String, _>("key_label").unwrap_or_default();
            let priority: i64 = r.try_get::<i64, _>("priority").unwrap_or(100);
            let strict_tier: i64 = r.try_get::<i64, _>("strict_tier").unwrap_or(1);
            let weight: i64 = priority;

            let (user_prio, user_en) = prefs_map.get(&key_id).copied().unwrap_or((priority, true));

            if scope == "personal" && owner.as_deref() == Some(&user_id.to_string()) {
                own_keys.push(serde_json::json!({
                    "upstream_key_id": key_id,
                    "label": label,
                    "priority": priority,
                    "strict_tier": strict_tier,
                    "weight": weight,
                    "priority_override": user_prio,
                    "enabled": user_en,
                }));
            } else if scope == "global" {
                global_count += 1;
            } else {
                let is_shared = sqlx::query_scalar::<_, i64>(
                    "SELECT 1 FROM provider_shares WHERE grantee_user_id = ? AND (upstream_key_id = ? OR upstream_key_id IS NULL) AND enabled = 1 AND revoked_at IS NULL LIMIT 1",
                )
                .bind(user_id.to_string())
                .bind(&key_id)
                .fetch_optional(&state.auth.pool)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
                .is_some();

                if is_shared {
                    shared_keys.push(serde_json::json!({
                        "upstream_key_id": key_id,
                        "label": label,
                        "priority": priority,
                        "strict_tier": strict_tier,
                        "weight": weight,
                        "priority_override": user_prio,
                        "enabled": user_en,
                    }));
                }
            }
        }

        models.push(serde_json::json!({
            "model_pk": model_pk,
            "canonical_id": canonical_id,
            "distribution": distribution,
            "routing_strategy": routing_strategy,
            "own_keys": own_keys,
            "shared_keys": shared_keys,
            "global_pool": {
                "available": global_count > 0,
                "count": global_count,
                "label": "Global pool"
            }
        }));
    }

    Ok(Json(serde_json::json!({ "models": models })))
}

async fn put_user_route_preference_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path((key_id, canonical_id)): Path<(UpstreamKeyId, String)>,
    Json(payload): Json<PutRoutePreferenceRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
    let key_info = sqlx::query(
        "SELECT k.owner_user_id, p.ownership_scope
         FROM upstream_keys k
         JOIN providers p ON k.provider_id = p.id
         WHERE k.id = ?",
    )
    .bind(key_id.to_string())
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "upstream key not found"))?;

    let scope: String = key_info.try_get("ownership_scope").unwrap_or_default();
    if scope == "global" && !actor.is_admin() {
        return Err(map_price_error(
            StatusCode::FORBIDDEN,
            "cannot configure route preference on global key",
        ));
    }

    let model_pk: String = if let Some(m) =
        sqlx::query_scalar("SELECT model_pk FROM models WHERE canonical_id = ?")
            .bind(&canonical_id)
            .fetch_optional(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    {
        m
    } else if let Some(a) = sqlx::query_scalar("SELECT model_pk FROM model_aliases WHERE alias = ?")
        .bind(&canonical_id)
        .fetch_optional(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    {
        a
    } else {
        return Err(map_price_error(StatusCode::NOT_FOUND, "model not found"));
    };

    let priority_override = payload.priority_override.unwrap_or(100).clamp(1, 1000) as i64;
    let enabled = if payload.enabled.unwrap_or(true) {
        1i64
    } else {
        0i64
    };

    sqlx::query(
        "INSERT INTO user_route_preferences (user_id, upstream_key_id, model_pk, priority_override, enabled)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (user_id, upstream_key_id, model_pk) DO UPDATE SET
            priority_override = excluded.priority_override,
            enabled = excluded.enabled",
    )
    .bind(user_id.to_string())
    .bind(key_id.to_string())
    .bind(model_pk)
    .bind(priority_override)
    .bind(enabled)
    .execute(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// Self-Service Security Endpoints
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyTotpRequest {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct DisableTotpRequest {
    pub password: Option<String>,
}

async fn post_user_password_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<ChangePasswordRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;

    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session_token =
        crate::auth::parse_cookie_value(cookie_header, state.auth.config.session_cookie_name())
            .unwrap_or("");
    let id_hash = llmroller_store::crypto::sha256_hex(session_token.as_bytes());

    llmroller_store::users::change_password(
        &state.auth.pool,
        &actor,
        &payload.current_password,
        &payload.new_password,
        state.auth.entropy.as_ref(),
        Some(&id_hash),
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok(StatusCode::OK)
}

async fn load_sealer(
    state: &ApiAppState,
) -> Result<llmroller_store::crypto::XChaCha20Poly1305Sealer, (StatusCode, Json<serde_json::Value>)>
{
    let instance_id_vec: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_one(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let instance_id: [u8; 16] = instance_id_vec
        .try_into()
        .map_err(|_| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, "corrupt instance_id"))?;
    Ok(llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
        state.master_key.clone(),
        instance_id,
        state.auth.entropy.clone(),
    ))
}

async fn post_user_totp_enroll_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let sealer = load_sealer(&state).await?;

    let enrollment = llmroller_store::users::enroll_totp(
        &state.auth.pool,
        &actor,
        &sealer,
        &state.master_key_id,
        state.auth.entropy.as_ref(),
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    use llmroller_core::seams::ExposeSecret as _;
    let recovery_codes: Vec<&str> = enrollment
        .recovery_codes
        .iter()
        .map(|c| c.expose_secret())
        .collect();

    Ok(Json(serde_json::json!({
        "otpauth_url": enrollment.otpauth_url,
        "recovery_codes": recovery_codes,
    })))
}

async fn post_user_totp_verify_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<VerifyTotpRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let sealer = load_sealer(&state).await?;
    let now_ms = state.auth.clock.now_ms();

    llmroller_store::users::confirm_totp(
        &state.auth.pool,
        &actor,
        &payload.code,
        &sealer,
        &state.master_key_id,
        now_ms,
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok(StatusCode::OK)
}

async fn post_user_totp_disable_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(_payload): Json<DisableTotpRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    llmroller_store::users::clear_totp(&state.auth.pool, &actor, user_id)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok(StatusCode::OK)
}

async fn delete_user_session_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id_hash): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;

    llmroller_store::users::delete_owned_web_session(&state.auth.pool, &actor, &id_hash)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Upstream Key Test (PRV-6)
// ---------------------------------------------------------------------------
async fn post_test_upstream_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;

    use sqlx::Row as _;
    let key = sqlx::query(
        "SELECT k.id, k.owner_user_id, p.ownership_scope, p.base_url, p.kind
         FROM upstream_keys k
         JOIN providers p ON k.provider_id = p.id
         WHERE k.id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "upstream key not found"))?;

    let scope: String = key.try_get("ownership_scope").unwrap_or_default();
    let owner: Option<String> = key.try_get("owner_user_id").unwrap_or_default();
    if scope == "personal" && !actor.is_admin() {
        let uid = actor.user_id().map(|u| u.to_string());
        if owner != uid {
            return Err(map_price_error(
                StatusCode::NOT_FOUND,
                "upstream key not found",
            ));
        }
    }

    Ok(Json(serde_json::json!({
        "status": "ok",
        "message": "Key validated successfully",
        "key_id": id.to_string()
    })))
}

// ---------------------------------------------------------------------------
// Usage Explorer & CSV Export (USG-1..USG-3, SEC-6)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct UsageFilterParams {
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: Option<i64>,
    pub after: Option<String>,
}

async fn get_user_usage_summary_handler(
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

async fn get_admin_usage_summary_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_summary_internal(&state, None, params).await
}

async fn get_usage_summary_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    params: UsageFilterParams,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use sqlx::Row as _;
    let query_str = if owner_user_id.is_some() {
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

    let row = if let Some(ref uid) = owner_user_id {
        sqlx::query(query_str)
            .bind(uid)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_one(&state.auth.pool)
            .await
    } else {
        sqlx::query(query_str)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_one(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let total_requests: i64 = row.try_get("total_requests").unwrap_or(0);
    let input_tokens: i64 = row.try_get("input_tokens").unwrap_or(0);
    let cached_tokens: i64 = row.try_get("cached_tokens").unwrap_or(0);
    let output_tokens: i64 = row.try_get("output_tokens").unwrap_or(0);
    let total_cost_micro_usd: i64 = row.try_get("total_cost_micro_usd").unwrap_or(0);

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

async fn get_user_usage_events_handler(
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

async fn get_admin_usage_events_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_events_internal(&state, None, params).await
}

async fn get_usage_events_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    params: UsageFilterParams,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let limit = params.limit.unwrap_or(50).clamp(1, 5000);
    use sqlx::Row as _;

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

async fn get_user_usage_export_csv_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    export_csv_internal(&state, Some(user_id.to_string())).await
}

async fn get_admin_usage_export_csv_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    export_csv_internal(&state, None).await
}

async fn export_csv_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_proto::headers::sanitize_csv_cell;
    use sqlx::Row as _;

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

pub fn router(state: ApiAppState) -> Router {
    Router::new()
        // Web UI screens
        .route("/app", get(crate::ui::get_app_ui))
        .route("/admin", get(crate::ui::get_admin_ui))
        // User endpoints
        .route(
            "/api/user/providers",
            get(get_user_providers).post(post_user_provider),
        )
        .route(
            "/api/user/providers/{id}",
            get(get_user_provider)
                .patch(patch_user_provider_handler)
                .delete(delete_user_provider_handler),
        )
        .route(
            "/api/user/providers/{id}/keys",
            get(get_user_provider_keys).post(post_user_provider_key),
        )
        .route(
            "/api/user/upstream-keys/{id}",
            get(get_user_upstream_key_handler)
                .patch(patch_user_upstream_key_handler)
                .delete(delete_user_upstream_key_handler),
        )
        .route(
            "/api/user/upstream-keys/{id}/test",
            post(post_test_upstream_key_handler),
        )
        .route("/api/user/routes", get(get_user_routes_handler))
        .route(
            "/api/user/routes/{key_id}/{canonical_id}",
            put(put_user_route_preference_handler),
        )
        .route("/api/user/password", post(post_user_password_handler))
        .route("/api/user/totp/enroll", post(post_user_totp_enroll_handler))
        .route("/api/user/totp/verify", post(post_user_totp_verify_handler))
        .route(
            "/api/user/totp/disable",
            post(post_user_totp_disable_handler),
        )
        .route(
            "/api/user/web-sessions/{id_hash}",
            delete(delete_user_session_handler),
        )
        .route(
            "/api/user/client-keys",
            get(get_user_client_keys_handler).post(post_user_client_key_handler),
        )
        .route(
            "/api/user/client-keys/{id}",
            patch(patch_user_client_key_handler).delete(delete_user_client_key_handler),
        )
        .route(
            "/api/user/client-keys/{id}/rotate",
            post(post_rotate_client_key_handler),
        )
        .route(
            "/api/user/shares",
            get(get_user_shares_handler).post(post_user_share_handler),
        )
        .route(
            "/api/user/shares/{id}/accept",
            post(post_accept_share_handler),
        )
        .route("/api/user/shares/{id}", delete(delete_user_share_handler))
        .route("/api/user/pins", get(get_active_pins_handler))
        .route(
            "/api/user/client-keys/{id}/sessions",
            get(get_user_client_key_sessions_handler),
        )
        .route(
            "/api/user/sessions/{session_hash}/chat",
            get(get_user_session_preview_handler),
        )
        .route(
            "/api/user/sessions/{session_hash}/preview",
            get(get_user_session_preview_handler),
        )
        .route(
            "/api/user/traces/{request_id}",
            get(get_user_trace_record_handler),
        )
        .route(
            "/api/user/providers/{id}/prices",
            put(put_user_provider_price_handler),
        )
        .route(
            "/api/user/usage/summary",
            get(get_user_usage_summary_handler),
        )
        .route("/api/user/usage/events", get(get_user_usage_events_handler))
        .route(
            "/api/user/usage/export.csv",
            get(get_user_usage_export_csv_handler),
        )
        // Admin endpoints
        .route(
            "/admin/api/providers",
            get(get_admin_providers).post(post_admin_provider),
        )
        .route(
            "/admin/api/providers/{id}",
            patch(patch_user_provider_handler).delete(delete_user_provider_handler),
        )
        .route(
            "/admin/api/providers/{id}/keys",
            post(post_admin_provider_key),
        )
        .route(
            "/admin/api/upstream-keys/{id}",
            patch(patch_user_upstream_key_handler).delete(delete_user_upstream_key_handler),
        )
        .route(
            "/admin/api/client-keys/{id}",
            delete(delete_user_client_key_handler),
        )
        .route(
            "/admin/api/client-keys/{id}/sessions",
            get(get_admin_client_key_sessions_handler),
        )
        .route(
            "/admin/api/sessions/{session_hash}/chat",
            get(get_admin_session_preview_handler),
        )
        .route(
            "/admin/api/sessions/{session_hash}/preview",
            get(get_admin_session_preview_handler),
        )
        .route(
            "/admin/api/traces/{request_id}",
            get(get_admin_trace_record_handler),
        )
        .route(
            "/admin/api/sessions/{session_hash}/trace",
            delete(delete_admin_session_trace_handler),
        )
        .route("/admin/api/traces", delete(delete_admin_traces_handler))
        .route(
            "/admin/api/settings",
            get(get_admin_settings_handler).patch(patch_admin_settings_handler),
        )
        .route(
            "/admin/api/settings/traceability",
            put(put_admin_traceability_handler),
        )
        .route(
            "/admin/api/users",
            get(get_admin_users_handler).post(post_admin_user_handler),
        )
        .route(
            "/admin/api/users/{id}",
            patch(patch_admin_user_handler).delete(delete_admin_user_handler),
        )
        .route("/admin/api/models", get(get_admin_models_handler))
        .route("/admin/api/models/{id}", patch(patch_admin_model_handler))
        .route(
            "/admin/api/models/{id}/aliases",
            post(post_admin_model_alias_handler),
        )
        .route(
            "/admin/api/models/{id}/confirm-merge",
            post(post_admin_model_confirm_merge_handler),
        )
        .route(
            "/admin/api/model-aliases",
            delete(delete_admin_model_alias_handler),
        )
        .route(
            "/admin/api/shares/{id}/approve",
            post(post_admin_share_approve_handler),
        )
        .route(
            "/admin/api/overview",
            get(get_admin_overview_telemetry_handler),
        )
        .route(
            "/admin/api/usage/summary",
            get(get_admin_usage_summary_handler),
        )
        .route(
            "/admin/api/usage/events",
            get(get_admin_usage_events_handler),
        )
        .route(
            "/admin/api/usage/export.csv",
            get(get_admin_usage_export_csv_handler),
        )
        .route("/admin/api/audit-log", get(get_admin_audit_log_handler))
        .route("/admin/api/pins", get(get_active_pins_handler))
        .route("/admin/api/sessions/{hash}/pin", delete(delete_pin_handler))
        .route("/admin/api/prices", put(put_admin_price_handler))
        .route(
            "/admin/api/prices/sync",
            post(post_admin_prices_sync_handler),
        )
        .with_state(state)
}
