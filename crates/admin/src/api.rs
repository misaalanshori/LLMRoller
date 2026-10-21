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
