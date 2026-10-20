#![forbid(unsafe_code)]
//! Client key generation, hashing, and management.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::str::FromStr;

use llmroller_core::ids::{ClientKeyId, UserId};
use llmroller_core::rbac::ActorScope;
use llmroller_core::seams::{EntropySource, SecretString, secret_string};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool, Transaction};
use thiserror::Error;

use crate::crypto::sha256_hex;

const BASE62_ALPHABET: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Client key errors.
#[derive(Debug, Error)]
pub enum ClientKeyError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("access denied: actor cannot manage client key")]
    Forbidden,
    #[error("client key not found: {0}")]
    NotFound(ClientKeyId),
    #[error("validation error: {0}")]
    Validation(String),
}

/// Stored client key entity. The raw token is not stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientKey {
    pub id: ClientKeyId,
    pub owner_user_id: UserId,
    pub label: String,
    pub prefix: String,
    pub key_hash: String,
    pub prev_key_hash: Option<String>,
    pub prev_key_expires_at: Option<i64>,
    pub routing_scope: String,
    pub allowed_models: Option<Vec<String>>,
    pub rate_limit_rpm: Option<i64>,
    pub budget_micro_usd_month: Option<i64>,
    pub allowed_cidrs: Option<Vec<String>>,
    pub constant_session_flagged_at: Option<i64>,
    pub constant_session_mode: String,
    pub disabled: bool,
    pub revoked_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// Parameters for creating a new client key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewClientKey {
    pub label: String,
    pub routing_scope: Option<String>,
    pub allowed_models: Option<Vec<String>>,
    pub rate_limit_rpm: Option<i64>,
    pub budget_micro_usd_month: Option<i64>,
    pub allowed_cidrs: Option<Vec<String>>,
    pub constant_session_mode: Option<String>,
    pub expires_at: Option<i64>,
}

/// Result returned from creating or rotating a client key, providing the plaintext token once.
#[derive(Debug)]
pub struct CreateClientKeyResult {
    pub client_key: ClientKey,
    pub raw_token: SecretString,
}

/// Parameters for updating an existing client key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientKeyPatch {
    pub label: Option<String>,
    pub routing_scope: Option<String>,
    pub allowed_models: Option<Option<Vec<String>>>,
    pub rate_limit_rpm: Option<Option<i64>>,
    pub budget_micro_usd_month: Option<Option<i64>>,
    pub allowed_cidrs: Option<Option<Vec<String>>>,
    pub constant_session_mode: Option<String>,
    pub disabled: Option<bool>,
    pub expires_at: Option<Option<i64>>,
}

/// Generate a new client key token string (`llr_` followed by 32 base62 characters), its prefix, and its SHA-256 hash.
#[must_use]
pub fn generate_client_key_token(entropy: &dyn EntropySource) -> (String, String, String) {
    let mut token = String::with_capacity(36);
    token.push_str("llr_");
    for _ in 0..32 {
        let idx = entropy.next_u64_bounded(62) as usize;
        token.push(BASE62_ALPHABET[idx] as char);
    }
    let prefix = token[..10].to_owned();
    let hash = sha256_hex(token.as_bytes());
    (token, prefix, hash)
}

/// Create a new client key for the given user inside an active transaction.
pub async fn create_client_key_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    owner_user_id: UserId,
    new_key: NewClientKey,
    entropy: &dyn EntropySource,
    now_ms: i64,
) -> Result<CreateClientKeyResult, ClientKeyError> {
    if !actor.is_admin() && !actor.is_system() && actor.user_id() != Some(owner_user_id) {
        return Err(ClientKeyError::Forbidden);
    }

    let routing_scope = new_key
        .routing_scope
        .unwrap_or_else(|| "inherit".to_string());
    validate_routing_scope(&routing_scope)?;

    let constant_session_mode = new_key
        .constant_session_mode
        .unwrap_or_else(|| "flag".to_string());
    validate_constant_session_mode(&constant_session_mode)?;

    let (raw_token, prefix, key_hash) = generate_client_key_token(entropy);
    let id = ClientKeyId::generate();

    let allowed_models_json = new_key
        .allowed_models
        .as_ref()
        .and_then(|m| serde_json::to_string(m).ok());
    let allowed_cidrs_json = new_key
        .allowed_cidrs
        .as_ref()
        .and_then(|c| serde_json::to_string(c).ok());

    sqlx::query(
        "INSERT INTO client_keys (
            id, owner_user_id, label, prefix, key_hash, prev_key_hash,
            prev_key_expires_at, routing_scope, allowed_models, rate_limit_rpm,
            budget_micro_usd_month, allowed_cidrs, constant_session_flagged_at,
            constant_session_mode, disabled, revoked_at, expires_at, created_at,
            last_used_at
        ) VALUES (?, ?, ?, ?, ?, NULL, NULL, ?, ?, ?, ?, ?, NULL, ?, 0, NULL, ?, ?, NULL)",
    )
    .bind(id.to_string())
    .bind(owner_user_id.to_string())
    .bind(&new_key.label)
    .bind(&prefix)
    .bind(&key_hash)
    .bind(&routing_scope)
    .bind(allowed_models_json)
    .bind(new_key.rate_limit_rpm)
    .bind(new_key.budget_micro_usd_month)
    .bind(allowed_cidrs_json)
    .bind(&constant_session_mode)
    .bind(new_key.expires_at)
    .bind(now_ms)
    .execute(&mut **tx)
    .await?;

    let client_key = ClientKey {
        id,
        owner_user_id,
        label: new_key.label,
        prefix,
        key_hash,
        prev_key_hash: None,
        prev_key_expires_at: None,
        routing_scope,
        allowed_models: new_key.allowed_models,
        rate_limit_rpm: new_key.rate_limit_rpm,
        budget_micro_usd_month: new_key.budget_micro_usd_month,
        allowed_cidrs: new_key.allowed_cidrs,
        constant_session_flagged_at: None,
        constant_session_mode,
        disabled: false,
        revoked_at: None,
        expires_at: new_key.expires_at,
        created_at: now_ms,
        last_used_at: None,
    };

    Ok(CreateClientKeyResult {
        client_key,
        raw_token: secret_string(&raw_token),
    })
}

/// Create a new client key for the given user.
pub async fn create_client_key(
    pool: &SqlitePool,
    actor: &ActorScope,
    owner_user_id: UserId,
    new_key: NewClientKey,
    entropy: &dyn EntropySource,
    now_ms: i64,
) -> Result<CreateClientKeyResult, ClientKeyError> {
    let mut tx = pool.begin().await?;
    let res = create_client_key_tx(&mut tx, actor, owner_user_id, new_key, entropy, now_ms).await?;
    tx.commit().await?;
    Ok(res)
}

/// Retrieve a client key by identifier within a transaction.
pub async fn get_client_key_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: ClientKeyId,
) -> Result<ClientKey, ClientKeyError> {
    let row = sqlx::query("SELECT * FROM client_keys WHERE id = ?")
        .bind(id.to_string())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ClientKeyError::NotFound(id))?;

    let key = client_key_from_row(&row)?;
    check_client_key_access(actor, &key)?;
    Ok(key)
}

/// Retrieve a client key by identifier.
pub async fn get_client_key(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: ClientKeyId,
) -> Result<ClientKey, ClientKeyError> {
    let mut tx = pool.begin().await?;
    let key = get_client_key_tx(&mut tx, actor, id).await?;
    tx.commit().await?;
    Ok(key)
}

/// List all client keys for a user.
pub async fn list_client_keys(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
) -> Result<Vec<ClientKey>, ClientKeyError> {
    if !actor.is_admin() && !actor.is_system() && actor.user_id() != Some(user_id) {
        return Err(ClientKeyError::Forbidden);
    }

    let rows =
        sqlx::query("SELECT * FROM client_keys WHERE owner_user_id = ? ORDER BY created_at ASC")
            .bind(user_id.to_string())
            .fetch_all(pool)
            .await?;

    rows.iter().map(client_key_from_row).collect()
}

/// Update an existing client key.
pub async fn patch_client_key_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: ClientKeyId,
    patch: ClientKeyPatch,
) -> Result<ClientKey, ClientKeyError> {
    let existing = get_client_key_tx(tx, actor, id).await?;

    let new_label = patch.label.unwrap_or(existing.label);
    let new_routing_scope = patch.routing_scope.unwrap_or(existing.routing_scope);
    validate_routing_scope(&new_routing_scope)?;

    let new_allowed_models = patch.allowed_models.unwrap_or(existing.allowed_models);
    let allowed_models_json = new_allowed_models
        .as_ref()
        .and_then(|m| serde_json::to_string(m).ok());

    let new_rpm = patch.rate_limit_rpm.unwrap_or(existing.rate_limit_rpm);
    let new_budget = patch
        .budget_micro_usd_month
        .unwrap_or(existing.budget_micro_usd_month);

    let new_allowed_cidrs = patch.allowed_cidrs.unwrap_or(existing.allowed_cidrs);
    let allowed_cidrs_json = new_allowed_cidrs
        .as_ref()
        .and_then(|c| serde_json::to_string(c).ok());

    let new_constant_mode = patch
        .constant_session_mode
        .unwrap_or(existing.constant_session_mode);
    validate_constant_session_mode(&new_constant_mode)?;

    let new_disabled = patch.disabled.unwrap_or(existing.disabled);
    let new_expires_at = patch.expires_at.unwrap_or(existing.expires_at);

    sqlx::query(
        "UPDATE client_keys SET
            label = ?, routing_scope = ?, allowed_models = ?, rate_limit_rpm = ?,
            budget_micro_usd_month = ?, allowed_cidrs = ?, constant_session_mode = ?,
            disabled = ?, expires_at = ?
         WHERE id = ?",
    )
    .bind(&new_label)
    .bind(&new_routing_scope)
    .bind(allowed_models_json)
    .bind(new_rpm)
    .bind(new_budget)
    .bind(allowed_cidrs_json)
    .bind(&new_constant_mode)
    .bind(if new_disabled { 1i64 } else { 0i64 })
    .bind(new_expires_at)
    .bind(id.to_string())
    .execute(&mut **tx)
    .await?;

    Ok(ClientKey {
        id,
        owner_user_id: existing.owner_user_id,
        label: new_label,
        prefix: existing.prefix,
        key_hash: existing.key_hash,
        prev_key_hash: existing.prev_key_hash,
        prev_key_expires_at: existing.prev_key_expires_at,
        routing_scope: new_routing_scope,
        allowed_models: new_allowed_models,
        rate_limit_rpm: new_rpm,
        budget_micro_usd_month: new_budget,
        allowed_cidrs: new_allowed_cidrs,
        constant_session_flagged_at: existing.constant_session_flagged_at,
        constant_session_mode: new_constant_mode,
        disabled: new_disabled,
        revoked_at: existing.revoked_at,
        expires_at: new_expires_at,
        created_at: existing.created_at,
        last_used_at: existing.last_used_at,
    })
}

/// Update an existing client key.
pub async fn patch_client_key(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: ClientKeyId,
    patch: ClientKeyPatch,
) -> Result<ClientKey, ClientKeyError> {
    let mut tx = pool.begin().await?;
    let key = patch_client_key_tx(&mut tx, actor, id, patch).await?;
    tx.commit().await?;
    Ok(key)
}

/// Delete a client key.
pub async fn delete_client_key_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: ClientKeyId,
) -> Result<(), ClientKeyError> {
    let existing = get_client_key_tx(tx, actor, id).await?;
    check_client_key_access(actor, &existing)?;

    sqlx::query("DELETE FROM client_keys WHERE id = ?")
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Delete a client key.
pub async fn delete_client_key(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: ClientKeyId,
) -> Result<(), ClientKeyError> {
    let mut tx = pool.begin().await?;
    delete_client_key_tx(&mut tx, actor, id).await?;
    tx.commit().await?;
    Ok(())
}

fn client_key_from_row(row: &SqliteRow) -> Result<ClientKey, ClientKeyError> {
    let id_str: String = row.try_get("id")?;
    let id =
        ClientKeyId::from_str(&id_str).map_err(|e| ClientKeyError::Validation(e.to_string()))?;
    let user_str: String = row.try_get("owner_user_id")?;
    let owner_user_id =
        UserId::from_str(&user_str).map_err(|e| ClientKeyError::Validation(e.to_string()))?;

    let allowed_models_raw: Option<String> = row.try_get("allowed_models")?;
    let allowed_models = allowed_models_raw.and_then(|r| serde_json::from_str(&r).ok());

    let allowed_cidrs_raw: Option<String> = row.try_get("allowed_cidrs")?;
    let allowed_cidrs = allowed_cidrs_raw.and_then(|r| serde_json::from_str(&r).ok());

    let disabled: i64 = row.try_get("disabled")?;

    Ok(ClientKey {
        id,
        owner_user_id,
        label: row.try_get("label")?,
        prefix: row.try_get("prefix")?,
        key_hash: row.try_get("key_hash")?,
        prev_key_hash: row.try_get("prev_key_hash")?,
        prev_key_expires_at: row.try_get("prev_key_expires_at")?,
        routing_scope: row.try_get("routing_scope")?,
        allowed_models,
        rate_limit_rpm: row.try_get("rate_limit_rpm")?,
        budget_micro_usd_month: row.try_get("budget_micro_usd_month")?,
        allowed_cidrs,
        constant_session_flagged_at: row.try_get("constant_session_flagged_at")?,
        constant_session_mode: row.try_get("constant_session_mode")?,
        disabled: disabled != 0,
        revoked_at: row.try_get("revoked_at")?,
        expires_at: row.try_get("expires_at")?,
        created_at: row.try_get("created_at")?,
        last_used_at: row.try_get("last_used_at")?,
    })
}

fn check_client_key_access(actor: &ActorScope, key: &ClientKey) -> Result<(), ClientKeyError> {
    if actor.is_admin() || actor.is_system() {
        return Ok(());
    }
    if actor.user_id().is_some_and(|uid| uid == key.owner_user_id) {
        return Ok(());
    }
    Err(ClientKeyError::Forbidden)
}

fn validate_routing_scope(scope: &str) -> Result<(), ClientKeyError> {
    match scope {
        "inherit" | "personal_first" | "blended" | "personal_only" | "own_only" | "global_only" => {
            Ok(())
        }
        other => Err(ClientKeyError::Validation(format!(
            "invalid routing_scope: {other:?}"
        ))),
    }
}

fn validate_constant_session_mode(mode: &str) -> Result<(), ClientKeyError> {
    match mode {
        "flag" | "fingerprint_generic_ids" => Ok(()),
        other => Err(ClientKeyError::Validation(format!(
            "invalid constant_session_mode: {other:?}"
        ))),
    }
}
