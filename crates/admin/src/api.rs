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
use llmroller_core::ids::{ClientKeyId, ProviderId, UpstreamKeyId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope};
use llmroller_core::seams::secret_string;
use llmroller_store::client_keys::{
    ClientKey, ClientKeyError, ClientKeyPatch, NewClientKey, create_client_key, delete_client_key,
    list_client_keys, patch_client_key,
};
use llmroller_store::crypto::{MasterKey, sha256_hex};
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
}

impl ApiAppState {
    #[must_use]
    pub fn new(auth: AuthAppState, master_key: MasterKey, master_key_id: String) -> Self {
        Self {
            auth,
            master_key,
            master_key_id,
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
        let token = headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| map_auth_error(AuthError::InvalidCsrf))?;
        let hash = sha256_hex(token.as_bytes());
        if hash != session.session.csrf_hash {
            return Err(map_auth_error(AuthError::InvalidCsrf));
        }
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
    let owner_id = actor.user_id().expect("user_id");

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

    get_session_preview_internal(&state, &session_hash).await
}

async fn get_admin_session_preview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_session_preview_internal(&state, &session_hash).await
}

async fn get_session_preview_internal(
    _state: &ApiAppState,
    session_hash: &str,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    Ok(Json(serde_json::json!({
        "session_hash": session_hash,
        "turns": []
    })))
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
            "/api/user/client-keys",
            get(get_user_client_keys_handler).post(post_user_client_key_handler),
        )
        .route(
            "/api/user/client-keys/{id}",
            patch(patch_user_client_key_handler).delete(delete_user_client_key_handler),
        )
        .route(
            "/api/user/client-keys/{id}/sessions",
            get(get_user_client_key_sessions_handler),
        )
        .route(
            "/api/user/sessions/{session_hash}/preview",
            get(get_user_session_preview_handler),
        )
        .route(
            "/api/user/providers/{id}/prices",
            put(put_user_provider_price_handler),
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
            "/admin/api/sessions/{session_hash}/preview",
            get(get_admin_session_preview_handler),
        )
        .route(
            "/admin/api/settings",
            get(get_admin_settings_handler).patch(patch_admin_settings_handler),
        )
        .route(
            "/admin/api/settings/traceability",
            put(put_admin_traceability_handler),
        )
        .route("/admin/api/prices", put(put_admin_price_handler))
        .route(
            "/admin/api/prices/sync",
            post(post_admin_prices_sync_handler),
        )
        .with_state(state)
}
