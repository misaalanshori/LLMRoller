#![forbid(unsafe_code)]
//! Provider and upstream key persistence, encryption, quotas, and fingerprinting.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/4. SRDD - Users, Roles, and Provider Ownership.md#43-consented-personal-provider-and-key-sharing-shr

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::Arc;

use llmroller_core::ids::{ProviderId, UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope};
use llmroller_core::seams::{EntropySource, ExposeSecret, SecretString};
use llmroller_core::snapshot::{ProviderRecord, UpstreamKeyRecord};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool, Transaction};
use thiserror::Error;

use crate::audit::AuditError;
use crate::crypto::{CryptoError, MasterKey, XChaCha20Poly1305Sealer, sha256_hex};

/// Maximum personal providers per user account per SEC-8.
pub const MAX_PERSONAL_PROVIDERS_PER_USER: usize = 20;

/// Maximum personal upstream keys per user account per SEC-8.
pub const MAX_PERSONAL_UPSTREAM_KEYS_PER_USER: usize = 50;

/// Provider repository errors.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("access denied: actor cannot manage requested resource")]
    Forbidden,
    #[error("provider not found: {0}")]
    ProviderNotFound(ProviderId),
    #[error("upstream key not found: {0}")]
    UpstreamKeyNotFound(UpstreamKeyId),
    #[error("resource quota exceeded for {resource}: limit is {limit}")]
    QuotaExceeded { resource: String, limit: usize },
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("validation error: {0}")]
    Validation(String),
}

/// Strongly typed stored provider entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    pub id: ProviderId,
    pub ownership_scope: OwnershipScope,
    pub owner_user_id: Option<UserId>,
    pub kind: ProviderKind,
    pub name: String,
    pub base_url: String,
    pub config_json: String,
    pub share_config_fingerprint: String,
    pub usage_url_cross_origin_ack: bool,
    pub allow_private_network: bool,
    pub enabled: bool,
    pub created_at: i64,
}

/// Parameters for creating a new provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProvider {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub config_json: String,
    pub ownership_scope: OwnershipScope,
    pub owner_user_id: Option<UserId>,
    pub usage_url_cross_origin_ack: bool,
    pub allow_private_network: bool,
    pub enabled: bool,
}

/// Patch parameters for updating a provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderPatch {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub config_json: Option<String>,
    pub usage_url_cross_origin_ack: Option<bool>,
    pub allow_private_network: Option<bool>,
    pub enabled: Option<bool>,
}

/// Strongly typed stored upstream key entity. Plaintext secret is never present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamKey {
    pub id: UpstreamKeyId,
    pub provider_id: ProviderId,
    pub owner_user_id: Option<UserId>,
    pub label: String,
    pub secret_enc: Vec<u8>,
    pub master_key_id: String,
    pub secret_last4: String,
    pub enabled: bool,
    pub priority: i64,
    pub strict_tier: i64,
    pub health: String,
    pub health_until: Option<i64>,
    pub health_version: i64,
    pub last_error: Option<String>,
    pub created_at: i64,
}

/// Parameters for creating a new upstream key.
#[derive(Debug, Clone)]
pub struct NewUpstreamKey {
    pub provider_id: ProviderId,
    pub label: String,
    pub secret: SecretString,
    pub priority: Option<i64>,
    pub strict_tier: Option<i64>,
    pub enabled: Option<bool>,
}

/// Patch parameters for updating an upstream key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpstreamKeyPatch {
    pub label: Option<String>,
    pub priority: Option<i64>,
    pub strict_tier: Option<i64>,
    pub enabled: Option<bool>,
}

/// Compute the canonical SHA-256 configuration fingerprint for a provider per SRDD Section 4.3.
#[must_use]
pub fn compute_share_config_fingerprint(
    kind: &str,
    base_url: &str,
    allow_private_network: bool,
    config_json: &str,
) -> String {
    let parsed: Value =
        serde_json::from_str(config_json).unwrap_or(Value::Object(serde_json::Map::new()));

    let mut map = BTreeMap::new();
    map.insert("kind".to_string(), Value::String(kind.to_owned()));
    map.insert("base_url".to_string(), Value::String(base_url.to_owned()));
    map.insert(
        "allow_private_network".to_string(),
        Value::Bool(allow_private_network),
    );

    let watched_keys = [
        "paths",
        "auth",
        "strip_headers",
        "add_headers",
        "ua_policy",
        "session_profile",
        "usage_profile",
        "response_header_allow",
    ];

    for key in watched_keys {
        if let Some(val) = parsed.get(key) {
            map.insert(key.to_string(), val.clone());
        } else {
            map.insert(key.to_string(), Value::Null);
        }
    }

    let canonical = serde_json::to_string(&map).unwrap_or_default();
    sha256_hex(canonical.as_bytes())
}

/// Create a new provider record in SQLite within an active transaction.
pub async fn create_provider_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    new_provider: NewProvider,
    now_ms: i64,
) -> Result<Provider, ProviderError> {
    // 1. URL SSRF and scheme validation per PRV-7 and SEC-7
    llmroller_core::ssrf::validate_url(&new_provider.base_url, new_provider.allow_private_network)
        .map_err(|e| ProviderError::Validation(e.to_string()))?;

    // 2. RBAC enforcement
    match new_provider.ownership_scope {
        OwnershipScope::Global => {
            if !actor.is_admin() && !actor.is_system() {
                return Err(ProviderError::Forbidden);
            }
            if new_provider.owner_user_id.is_some() {
                return Err(ProviderError::Validation(
                    "global provider must not have owner_user_id".to_owned(),
                ));
            }
        }
        OwnershipScope::Personal => {
            let owner_id = new_provider.owner_user_id.ok_or_else(|| {
                ProviderError::Validation("personal provider requires owner_user_id".to_owned())
            })?;
            if actor
                .user_id()
                .is_some_and(|uid| !actor.is_admin() && uid != owner_id)
            {
                return Err(ProviderError::Forbidden);
            }
            // Enforce SEC-8 quota: max 20 personal providers per user
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM providers WHERE ownership_scope = 'personal' AND owner_user_id = ?",
            )
            .bind(owner_id.to_string())
            .fetch_one(&mut **tx)
            .await?;

            if count as usize >= MAX_PERSONAL_PROVIDERS_PER_USER {
                return Err(ProviderError::QuotaExceeded {
                    resource: "providers".to_string(),
                    limit: MAX_PERSONAL_PROVIDERS_PER_USER,
                });
            }
        }
    }

    let provider_id = ProviderId::generate();
    let fingerprint = compute_share_config_fingerprint(
        new_provider.kind.as_str(),
        &new_provider.base_url,
        new_provider.allow_private_network,
        &new_provider.config_json,
    );

    sqlx::query(
        "INSERT INTO providers (
            id, ownership_scope, owner_user_id, kind, name, base_url,
            config_json, share_config_fingerprint, usage_url_cross_origin_ack,
            allow_private_network, enabled, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(provider_id.to_string())
    .bind(new_provider.ownership_scope.as_str())
    .bind(new_provider.owner_user_id.map(|u| u.to_string()))
    .bind(new_provider.kind.as_str())
    .bind(&new_provider.name)
    .bind(&new_provider.base_url)
    .bind(&new_provider.config_json)
    .bind(&fingerprint)
    .bind(if new_provider.usage_url_cross_origin_ack {
        1i64
    } else {
        0i64
    })
    .bind(if new_provider.allow_private_network {
        1i64
    } else {
        0i64
    })
    .bind(if new_provider.enabled { 1i64 } else { 0i64 })
    .bind(now_ms)
    .execute(&mut **tx)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref dbe) if dbe.is_unique_violation() => ProviderError::Conflict(
            format!("provider name {:?} already exists", new_provider.name),
        ),
        other => ProviderError::Sqlx(other),
    })?;

    Ok(Provider {
        id: provider_id,
        ownership_scope: new_provider.ownership_scope,
        owner_user_id: new_provider.owner_user_id,
        kind: new_provider.kind,
        name: new_provider.name,
        base_url: new_provider.base_url,
        config_json: new_provider.config_json,
        share_config_fingerprint: fingerprint,
        usage_url_cross_origin_ack: new_provider.usage_url_cross_origin_ack,
        allow_private_network: new_provider.allow_private_network,
        enabled: new_provider.enabled,
        created_at: now_ms,
    })
}

/// Create a new provider record in SQLite.
pub async fn create_provider(
    pool: &SqlitePool,
    actor: &ActorScope,
    new_provider: NewProvider,
    now_ms: i64,
) -> Result<Provider, ProviderError> {
    let mut tx = pool.begin().await?;
    let provider = create_provider_tx(&mut tx, actor, new_provider, now_ms).await?;
    tx.commit().await?;
    Ok(provider)
}

/// Retrieve a single provider by identifier within an active transaction.
pub async fn get_provider_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: ProviderId,
) -> Result<Provider, ProviderError> {
    let row = sqlx::query("SELECT * FROM providers WHERE id = ?")
        .bind(id.to_string())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ProviderError::ProviderNotFound(id))?;

    let provider = provider_from_row(&row)?;
    check_provider_access(actor, &provider)?;
    Ok(provider)
}

/// Retrieve a single provider by identifier, validating access for the calling actor.
pub async fn get_provider(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: ProviderId,
) -> Result<Provider, ProviderError> {
    let mut tx = pool.begin().await?;
    let provider = get_provider_tx(&mut tx, actor, id).await?;
    tx.commit().await?;
    Ok(provider)
}

/// List all providers visible to the calling actor.
pub async fn list_providers(
    pool: &SqlitePool,
    actor: &ActorScope,
) -> Result<Vec<Provider>, ProviderError> {
    let rows = if actor.is_admin() || actor.is_system() {
        sqlx::query("SELECT * FROM providers ORDER BY created_at ASC")
            .fetch_all(pool)
            .await?
    } else if let Some(user_id) = actor.user_id() {
        sqlx::query(
            "SELECT * FROM providers WHERE ownership_scope = 'global' OR owner_user_id = ? ORDER BY created_at ASC",
        )
        .bind(user_id.to_string())
        .fetch_all(pool)
        .await?
    } else {
        return Ok(Vec::new());
    };

    rows.iter().map(provider_from_row).collect()
}

/// Update an existing provider record within an active transaction.
pub async fn patch_provider_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: ProviderId,
    patch: ProviderPatch,
    _now_ms: i64,
) -> Result<Provider, ProviderError> {
    let existing = get_provider_tx(tx, actor, id).await?;

    let new_name = patch.name.unwrap_or(existing.name);
    let new_base_url = patch.base_url.unwrap_or(existing.base_url);
    let new_config_json = patch.config_json.unwrap_or(existing.config_json);
    let new_usage_ack = patch
        .usage_url_cross_origin_ack
        .unwrap_or(existing.usage_url_cross_origin_ack);
    let new_private_net = patch
        .allow_private_network
        .unwrap_or(existing.allow_private_network);
    let new_enabled = patch.enabled.unwrap_or(existing.enabled);

    let new_fingerprint = compute_share_config_fingerprint(
        existing.kind.as_str(),
        &new_base_url,
        new_private_net,
        &new_config_json,
    );

    llmroller_core::ssrf::validate_url(&new_base_url, new_private_net)
        .map_err(|e| ProviderError::Validation(e.to_string()))?;

    sqlx::query(
        "UPDATE providers SET
            name = ?, base_url = ?, config_json = ?,
            share_config_fingerprint = ?, usage_url_cross_origin_ack = ?,
            allow_private_network = ?, enabled = ?
         WHERE id = ?",
    )
    .bind(&new_name)
    .bind(&new_base_url)
    .bind(&new_config_json)
    .bind(&new_fingerprint)
    .bind(if new_usage_ack { 1i64 } else { 0i64 })
    .bind(if new_private_net { 1i64 } else { 0i64 })
    .bind(if new_enabled { 1i64 } else { 0i64 })
    .bind(id.to_string())
    .execute(&mut **tx)
    .await?;

    Ok(Provider {
        id,
        ownership_scope: existing.ownership_scope,
        owner_user_id: existing.owner_user_id,
        kind: existing.kind,
        name: new_name,
        base_url: new_base_url,
        config_json: new_config_json,
        share_config_fingerprint: new_fingerprint,
        usage_url_cross_origin_ack: new_usage_ack,
        allow_private_network: new_private_net,
        enabled: new_enabled,
        created_at: existing.created_at,
    })
}

/// Update an existing provider record, recomputing its share configuration fingerprint.
pub async fn patch_provider(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: ProviderId,
    patch: ProviderPatch,
    now_ms: i64,
) -> Result<Provider, ProviderError> {
    let mut tx = pool.begin().await?;
    let provider = patch_provider_tx(&mut tx, actor, id, patch, now_ms).await?;
    tx.commit().await?;
    Ok(provider)
}

/// Delete a provider and all its cascading personal assets within an active transaction.
pub async fn delete_provider_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: ProviderId,
) -> Result<(), ProviderError> {
    let existing = get_provider_tx(tx, actor, id).await?;
    check_provider_access(actor, &existing)?;

    sqlx::query("DELETE FROM providers WHERE id = ?")
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Delete a provider and all its cascading personal assets.
pub async fn delete_provider(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: ProviderId,
) -> Result<(), ProviderError> {
    let mut tx = pool.begin().await?;
    delete_provider_tx(&mut tx, actor, id).await?;
    tx.commit().await?;
    Ok(())
}

/// Create a new upstream key within an active transaction.
pub async fn create_upstream_key_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    master_key: &MasterKey,
    master_key_id: &str,
    entropy: Arc<dyn EntropySource>,
    new_key: NewUpstreamKey,
    now_ms: i64,
) -> Result<UpstreamKey, ProviderError> {
    let provider = get_provider_tx(tx, actor, new_key.provider_id).await?;

    // Quotas: max 50 personal keys per user (SEC-8)
    if let Some(owner_id) = provider.owner_user_id {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM upstream_keys WHERE owner_user_id = ?")
                .bind(owner_id.to_string())
                .fetch_one(&mut **tx)
                .await?;

        if count as usize >= MAX_PERSONAL_UPSTREAM_KEYS_PER_USER {
            return Err(ProviderError::QuotaExceeded {
                resource: "upstream_keys".to_string(),
                limit: MAX_PERSONAL_UPSTREAM_KEYS_PER_USER,
            });
        }
    }

    let priority = new_key.priority.unwrap_or(100);
    if !(1..=1000).contains(&priority) {
        return Err(ProviderError::Validation(
            "priority must be between 1 and 1000".to_owned(),
        ));
    }
    let strict_tier = new_key.strict_tier.unwrap_or(1);
    if !(1..=10).contains(&strict_tier) {
        return Err(ProviderError::Validation(
            "strict_tier must be between 1 and 10".to_owned(),
        ));
    }

    let key_id = UpstreamKeyId::generate();
    let secret_exposed = new_key.secret.expose_secret();
    let secret_last4 = if secret_exposed.len() > 4 {
        secret_exposed[secret_exposed.len() - 4..].to_string()
    } else {
        secret_exposed.to_string()
    };

    let instance_id_vec: Vec<u8> = sqlx::query_scalar(
        "SELECT instance_id FROM instance_meta WHERE singleton = 1",
    )
    .fetch_one(&mut **tx)
    .await?;
    let instance_id: [u8; 16] = instance_id_vec
        .try_into()
        .map_err(|_| ProviderError::Validation("corrupt instance_id in instance_meta".into()))?;

    let sealer = XChaCha20Poly1305Sealer::new(master_key.clone(), instance_id, entropy);

    let sealed_secret = sealer.try_seal(
        "upstream_key",
        &[
            master_key_id.as_bytes(),
            key_id.to_string().as_bytes(),
            new_key.provider_id.to_string().as_bytes(),
        ],
        secret_exposed.as_bytes(),
    )?;

    let enabled = new_key.enabled.unwrap_or(true);

    sqlx::query(
        "INSERT INTO upstream_keys (
            id, provider_id, owner_user_id, label, secret_enc, master_key_id,
            secret_last4, enabled, priority, strict_tier, health, health_until,
            health_version, last_error, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'healthy', NULL, 0, NULL, ?)",
    )
    .bind(key_id.to_string())
    .bind(new_key.provider_id.to_string())
    .bind(provider.owner_user_id.map(|u| u.to_string()))
    .bind(&new_key.label)
    .bind(&sealed_secret)
    .bind(master_key_id)
    .bind(&secret_last4)
    .bind(if enabled { 1i64 } else { 0i64 })
    .bind(priority)
    .bind(strict_tier)
    .bind(now_ms)
    .execute(&mut **tx)
    .await?;

    Ok(UpstreamKey {
        id: key_id,
        provider_id: new_key.provider_id,
        owner_user_id: provider.owner_user_id,
        label: new_key.label,
        secret_enc: sealed_secret,
        master_key_id: master_key_id.to_owned(),
        secret_last4,
        enabled,
        priority,
        strict_tier,
        health: "healthy".to_owned(),
        health_until: None,
        health_version: 0,
        last_error: None,
        created_at: now_ms,
    })
}

/// Create a new upstream key, encrypting its secret with XChaCha20-Poly1305 under domain "upstream_key".
pub async fn create_upstream_key(
    pool: &SqlitePool,
    actor: &ActorScope,
    master_key: &MasterKey,
    master_key_id: &str,
    entropy: Arc<dyn EntropySource>,
    new_key: NewUpstreamKey,
    now_ms: i64,
) -> Result<UpstreamKey, ProviderError> {
    let mut tx = pool.begin().await?;
    let key = create_upstream_key_tx(
        &mut tx,
        actor,
        master_key,
        master_key_id,
        entropy,
        new_key,
        now_ms,
    )
    .await?;
    tx.commit().await?;
    Ok(key)
}

/// Retrieve a single upstream key within an active transaction.
pub async fn get_upstream_key_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: UpstreamKeyId,
) -> Result<UpstreamKey, ProviderError> {
    let row = sqlx::query("SELECT * FROM upstream_keys WHERE id = ?")
        .bind(id.to_string())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ProviderError::UpstreamKeyNotFound(id))?;

    let key = key_from_row(&row)?;
    check_key_access(actor, &key)?;
    Ok(key)
}

/// Retrieve a single upstream key, verifying ownership access.
pub async fn get_upstream_key(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: UpstreamKeyId,
) -> Result<UpstreamKey, ProviderError> {
    let mut tx = pool.begin().await?;
    let key = get_upstream_key_tx(&mut tx, actor, id).await?;
    tx.commit().await?;
    Ok(key)
}

/// List all upstream keys under a given provider visible to the calling actor within an active transaction.
pub async fn list_upstream_keys_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    provider_id: ProviderId,
) -> Result<Vec<UpstreamKey>, ProviderError> {
    let _provider = get_provider_tx(tx, actor, provider_id).await?;

    let rows =
        sqlx::query("SELECT * FROM upstream_keys WHERE provider_id = ? ORDER BY created_at ASC")
            .bind(provider_id.to_string())
            .fetch_all(&mut **tx)
            .await?;

    rows.iter().map(key_from_row).collect()
}

/// List all upstream keys under a given provider visible to the calling actor.
pub async fn list_upstream_keys(
    pool: &SqlitePool,
    actor: &ActorScope,
    provider_id: ProviderId,
) -> Result<Vec<UpstreamKey>, ProviderError> {
    let mut tx = pool.begin().await?;
    let keys = list_upstream_keys_tx(&mut tx, actor, provider_id).await?;
    tx.commit().await?;
    Ok(keys)
}

/// Update an upstream key's label, priority, strict tier, or enabled state within an active transaction.
pub async fn patch_upstream_key_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: UpstreamKeyId,
    patch: UpstreamKeyPatch,
) -> Result<UpstreamKey, ProviderError> {
    let existing = get_upstream_key_tx(tx, actor, id).await?;

    let new_label = patch.label.unwrap_or(existing.label);
    let new_priority = patch.priority.unwrap_or(existing.priority);
    if !(1..=1000).contains(&new_priority) {
        return Err(ProviderError::Validation(
            "priority must be between 1 and 1000".to_owned(),
        ));
    }
    let new_strict_tier = patch.strict_tier.unwrap_or(existing.strict_tier);
    if !(1..=10).contains(&new_strict_tier) {
        return Err(ProviderError::Validation(
            "strict_tier must be between 1 and 10".to_owned(),
        ));
    }
    let new_enabled = patch.enabled.unwrap_or(existing.enabled);

    sqlx::query(
        "UPDATE upstream_keys SET label = ?, priority = ?, strict_tier = ?, enabled = ? WHERE id = ?",
    )
    .bind(&new_label)
    .bind(new_priority)
    .bind(new_strict_tier)
    .bind(if new_enabled { 1i64 } else { 0i64 })
    .bind(id.to_string())
    .execute(&mut **tx)
    .await?;

    Ok(UpstreamKey {
        id,
        provider_id: existing.provider_id,
        owner_user_id: existing.owner_user_id,
        label: new_label,
        secret_enc: existing.secret_enc,
        master_key_id: existing.master_key_id,
        secret_last4: existing.secret_last4,
        enabled: new_enabled,
        priority: new_priority,
        strict_tier: new_strict_tier,
        health: existing.health,
        health_until: existing.health_until,
        health_version: existing.health_version,
        last_error: existing.last_error,
        created_at: existing.created_at,
    })
}

/// Update an upstream key's label, priority, strict tier, or enabled state.
pub async fn patch_upstream_key(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: UpstreamKeyId,
    patch: UpstreamKeyPatch,
) -> Result<UpstreamKey, ProviderError> {
    let mut tx = pool.begin().await?;
    let key = patch_upstream_key_tx(&mut tx, actor, id, patch).await?;
    tx.commit().await?;
    Ok(key)
}

/// Delete an upstream key within an active transaction.
pub async fn delete_upstream_key_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: UpstreamKeyId,
) -> Result<(), ProviderError> {
    let existing = get_upstream_key_tx(tx, actor, id).await?;
    check_key_access(actor, &existing)?;

    sqlx::query("DELETE FROM upstream_keys WHERE id = ?")
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Delete an upstream key.
pub async fn delete_upstream_key(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: UpstreamKeyId,
) -> Result<(), ProviderError> {
    let mut tx = pool.begin().await?;
    delete_upstream_key_tx(&mut tx, actor, id).await?;
    tx.commit().await?;
    Ok(())
}

/// Load all providers into [`ProviderRecord`] projections for [`ConfigSnapshot`].
pub async fn load_provider_records_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
) -> Result<HashMap<ProviderId, ProviderRecord>, ProviderError> {
    let rows = sqlx::query("SELECT * FROM providers")
        .fetch_all(&mut **tx)
        .await?;

    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let p = provider_from_row(&row)?;
        map.insert(
            p.id,
            ProviderRecord {
                id: p.id,
                ownership_scope: p.ownership_scope,
                owner_user_id: p.owner_user_id,
                kind: p.kind,
                name: p.name,
                base_url: p.base_url,
                config_json: p.config_json,
                share_config_fingerprint: p.share_config_fingerprint,
                usage_url_cross_origin_ack: p.usage_url_cross_origin_ack,
                allow_private_network: p.allow_private_network,
                enabled: p.enabled,
            },
        );
    }
    Ok(map)
}

/// Load all upstream keys into [`UpstreamKeyRecord`] projections for [`ConfigSnapshot`].
pub async fn load_upstream_key_records_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
) -> Result<HashMap<UpstreamKeyId, UpstreamKeyRecord>, ProviderError> {
    let rows = sqlx::query("SELECT * FROM upstream_keys")
        .fetch_all(&mut **tx)
        .await?;

    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let k = key_from_row(&row)?;
        map.insert(
            k.id,
            UpstreamKeyRecord {
                id: k.id,
                provider_id: k.provider_id,
                owner_user_id: k.owner_user_id,
                label: k.label,
                secret_last4: k.secret_last4,
                enabled: k.enabled,
                priority: k.priority,
                strict_tier: k.strict_tier,
                health: k.health,
                health_version: k.health_version,
            },
        );
    }
    Ok(map)
}

fn provider_from_row(row: &SqliteRow) -> Result<Provider, ProviderError> {
    let id_str: String = row.try_get("id")?;
    let provider_id =
        ProviderId::from_str(&id_str).map_err(|e| ProviderError::Validation(e.to_string()))?;
    let scope_str: String = row.try_get("ownership_scope")?;
    let ownership_scope = match scope_str.as_str() {
        "global" => OwnershipScope::Global,
        "personal" => OwnershipScope::Personal,
        other => {
            return Err(ProviderError::Validation(format!(
                "invalid ownership_scope: {other}"
            )));
        }
    };
    let owner_user_id: Option<String> = row.try_get("owner_user_id")?;
    let owner_id = match owner_user_id {
        Some(uid) => {
            Some(UserId::from_str(&uid).map_err(|e| ProviderError::Validation(e.to_string()))?)
        }
        None => None,
    };
    let kind_str: String = row.try_get("kind")?;
    let kind =
        ProviderKind::from_str(&kind_str).map_err(|e| ProviderError::Validation(e.to_string()))?;
    let usage_ack: i64 = row.try_get("usage_url_cross_origin_ack")?;
    let private_net: i64 = row.try_get("allow_private_network")?;
    let enabled: i64 = row.try_get("enabled")?;

    Ok(Provider {
        id: provider_id,
        ownership_scope,
        owner_user_id: owner_id,
        kind,
        name: row.try_get("name")?,
        base_url: row.try_get("base_url")?,
        config_json: row.try_get("config_json")?,
        share_config_fingerprint: row.try_get("share_config_fingerprint")?,
        usage_url_cross_origin_ack: usage_ack != 0,
        allow_private_network: private_net != 0,
        enabled: enabled != 0,
        created_at: row.try_get("created_at")?,
    })
}

fn key_from_row(row: &SqliteRow) -> Result<UpstreamKey, ProviderError> {
    let id_str: String = row.try_get("id")?;
    let key_id =
        UpstreamKeyId::from_str(&id_str).map_err(|e| ProviderError::Validation(e.to_string()))?;
    let prov_str: String = row.try_get("provider_id")?;
    let provider_id =
        ProviderId::from_str(&prov_str).map_err(|e| ProviderError::Validation(e.to_string()))?;
    let owner_user_id: Option<String> = row.try_get("owner_user_id")?;
    let owner_id = match owner_user_id {
        Some(uid) => {
            Some(UserId::from_str(&uid).map_err(|e| ProviderError::Validation(e.to_string()))?)
        }
        None => None,
    };
    let enabled: i64 = row.try_get("enabled")?;

    Ok(UpstreamKey {
        id: key_id,
        provider_id,
        owner_user_id: owner_id,
        label: row.try_get("label")?,
        secret_enc: row.try_get("secret_enc")?,
        master_key_id: row.try_get("master_key_id")?,
        secret_last4: row.try_get("secret_last4")?,
        enabled: enabled != 0,
        priority: row.try_get("priority")?,
        strict_tier: row.try_get("strict_tier")?,
        health: row.try_get("health")?,
        health_until: row.try_get("health_until")?,
        health_version: row.try_get("health_version")?,
        last_error: row.try_get("last_error")?,
        created_at: row.try_get("created_at")?,
    })
}

fn check_provider_access(actor: &ActorScope, provider: &Provider) -> Result<(), ProviderError> {
    if actor.is_admin() || actor.is_system() {
        return Ok(());
    }
    match provider.ownership_scope {
        OwnershipScope::Global => Ok(()), // Global providers are readable by any authenticated user
        OwnershipScope::Personal => {
            if actor
                .user_id()
                .is_some_and(|uid| Some(uid) == provider.owner_user_id)
            {
                return Ok(());
            }
            Err(ProviderError::Forbidden)
        }
    }
}

fn check_key_access(actor: &ActorScope, key: &UpstreamKey) -> Result<(), ProviderError> {
    if actor.is_admin() || actor.is_system() {
        return Ok(());
    }
    if actor
        .user_id()
        .is_some_and(|uid| Some(uid) == key.owner_user_id)
    {
        return Ok(());
    }
    Err(ProviderError::Forbidden)
}
