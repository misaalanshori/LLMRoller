#![forbid(unsafe_code)]
//! Service Account machine principals, CIDR allow-lists, and child key grouping.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk (CLK-6)
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#91-complete-sqlite-ddl-migrations0001_initialsql
//! Spec: ./docs/srdd/14.j. SRDD - Service Accounts Directory and Machine Principal Management.md
//! Spec: ./docs/srdd/14.k. SRDD - Service Account Detail View, CIDR IP Restrictions, and Child Keys.md

use std::collections::HashMap;
use std::str::FromStr;

use llmroller_core::ids::{ServiceAccountId, UserId, validate_ulid};
use llmroller_core::rbac::ActorScope;
use llmroller_core::snapshot::ServiceAccountRecord;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool, Transaction};
use thiserror::Error;

/// Maximum number of CIDR entries accepted for one Service Account (`CLK-6`).
pub const MAX_SERVICE_ACCOUNT_CIDRS: usize = 64;

/// Service Account repository errors.
#[derive(Debug, Error)]
pub enum ServiceAccountError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("access denied: actor cannot manage service account")]
    Forbidden,
    #[error("service account not found: {0}")]
    NotFound(String),
    #[error("service account name already exists in this workspace")]
    NameTaken,
    #[error("validation error: {0}")]
    Validation(String),
}

/// Stored Service Account entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceAccount {
    pub id: String,
    pub owner_user_id: UserId,
    pub workspace_scope: String,
    pub name: String,
    pub description: Option<String>,
    pub allowed_cidrs: Option<Vec<String>>,
    pub disabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Parameters for creating a Service Account.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewServiceAccount {
    pub name: String,
    pub description: Option<String>,
    pub workspace_scope: Option<String>,
    pub allowed_cidrs: Option<Vec<String>>,
}

/// Mutable Service Account properties.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceAccountPatch {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub disabled: Option<bool>,
}

/// Normalize and validate a CIDR list (`CLK-6`, Section 14.k.4).
///
/// Bare IPv4/IPv6 hosts are expanded to `/32` and `/128` networks. Empty input
/// clears the list. More than 64 entries or any unparseable token is rejected.
///
/// # Errors
///
/// Returns [`ServiceAccountError::Validation`] naming the offending token.
pub fn normalize_cidrs(input: &[String]) -> Result<Option<Vec<String>>, ServiceAccountError> {
    let mut normalized = Vec::with_capacity(input.len());
    for token in input {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let network = match ipnet::IpNet::from_str(trimmed) {
            Ok(network) => network,
            Err(_) => match std::net::IpAddr::from_str(trimmed) {
                Ok(address) => ipnet::IpNet::from(address),
                Err(_) => {
                    return Err(ServiceAccountError::Validation(format!(
                        "invalid IPv4/IPv6 address or CIDR: {trimmed}"
                    )));
                }
            },
        };
        // Store the canonical network address so host bits cannot make two
        // equivalent allow-list entries look distinct.
        normalized.push(network.trunc().to_string());
    }
    if normalized.len() > MAX_SERVICE_ACCOUNT_CIDRS {
        return Err(ServiceAccountError::Validation(format!(
            "Maximum of {MAX_SERVICE_ACCOUNT_CIDRS} IPv4/IPv6 addresses or CIDRs allowed"
        )));
    }
    if normalized.is_empty() {
        Ok(None)
    } else {
        Ok(Some(normalized))
    }
}

/// Split a comma/newline separated CIDR textarea into normalized entries.
///
/// # Errors
///
/// Returns [`ServiceAccountError::Validation`] for invalid entries.
pub fn parse_cidrs_text(text: &str) -> Result<Option<Vec<String>>, ServiceAccountError> {
    let tokens: Vec<String> = text
        .split([',', '\r', '\n'])
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    normalize_cidrs(&tokens)
}

fn validate_name(name: &str) -> Result<(), ServiceAccountError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(ServiceAccountError::Validation(
            "service account name must be 1..=64 characters".to_string(),
        ));
    }
    Ok(())
}

fn validate_scope(scope: &str) -> Result<(), ServiceAccountError> {
    match scope {
        "personal" | "global" => Ok(()),
        other => Err(ServiceAccountError::Validation(format!(
            "invalid workspace_scope: {other:?}"
        ))),
    }
}

fn check_access(actor: &ActorScope, account: &ServiceAccount) -> Result<(), ServiceAccountError> {
    if actor.is_admin() || actor.is_system() {
        return Ok(());
    }
    if actor
        .user_id()
        .is_some_and(|uid| uid == account.owner_user_id)
    {
        Ok(())
    } else {
        Err(ServiceAccountError::NotFound(account.id.clone()))
    }
}

fn service_account_from_row(row: &SqliteRow) -> Result<ServiceAccount, ServiceAccountError> {
    let id: String = row.try_get("id")?;
    validate_ulid(&id).map_err(|e| ServiceAccountError::Validation(e.to_string()))?;
    let owner_str: String = row.try_get("owner_user_id")?;
    let owner_user_id =
        UserId::from_str(&owner_str).map_err(|e| ServiceAccountError::Validation(e.to_string()))?;
    let allowed_raw: Option<String> = row.try_get("allowed_cidrs")?;
    let allowed_cidrs = match allowed_raw {
        Some(raw) => serde_json::from_str(&raw).map_err(|e| {
            ServiceAccountError::Validation(format!("invalid allowed_cidrs JSON: {e}"))
        })?,
        None => None,
    };
    let disabled: i64 = row.try_get("disabled")?;
    Ok(ServiceAccount {
        id,
        owner_user_id,
        workspace_scope: row.try_get("workspace_scope")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        allowed_cidrs,
        disabled: disabled != 0,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Create a Service Account inside an active transaction.
///
/// # Errors
///
/// Returns [`ServiceAccountError::NameTaken`] when the unique
/// `(owner_user_id, workspace_scope, name)` index rejects the insert.
pub async fn create_service_account_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    owner_user_id: UserId,
    new_account: NewServiceAccount,
    now_ms: i64,
) -> Result<ServiceAccount, ServiceAccountError> {
    let workspace_scope = new_account
        .workspace_scope
        .clone()
        .unwrap_or_else(|| "personal".to_string());
    validate_scope(&workspace_scope)?;
    validate_name(&new_account.name)?;
    if workspace_scope == "global" && !(actor.is_admin() || actor.is_system()) {
        return Err(ServiceAccountError::Forbidden);
    }
    if !actor.is_admin() && !actor.is_system() && actor.user_id() != Some(owner_user_id) {
        return Err(ServiceAccountError::Forbidden);
    }
    let cidrs = match new_account.allowed_cidrs {
        Some(list) => normalize_cidrs(&list)?,
        None => None,
    };
    let cidrs_json = cidrs.as_ref().and_then(|c| serde_json::to_string(c).ok());

    let id = ServiceAccountId::generate().to_string();
    let result = sqlx::query(
        "INSERT INTO service_accounts (
            id, owner_user_id, workspace_scope, name, description, allowed_cidrs,
            disabled, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(&id)
    .bind(owner_user_id.to_string())
    .bind(&workspace_scope)
    .bind(new_account.name.trim())
    .bind(new_account.description.as_deref())
    .bind(cidrs_json)
    .bind(now_ms)
    .bind(now_ms)
    .execute(&mut **tx)
    .await;

    match result {
        Ok(_) => {}
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            return Err(ServiceAccountError::NameTaken);
        }
        Err(error) => return Err(ServiceAccountError::Sqlx(error)),
    }

    Ok(ServiceAccount {
        id,
        owner_user_id,
        workspace_scope,
        name: new_account.name.trim().to_string(),
        description: new_account.description,
        allowed_cidrs: cidrs,
        disabled: false,
        created_at: now_ms,
        updated_at: now_ms,
    })
}

/// Create a Service Account.
pub async fn create_service_account(
    pool: &SqlitePool,
    actor: &ActorScope,
    owner_user_id: UserId,
    new_account: NewServiceAccount,
    now_ms: i64,
) -> Result<ServiceAccount, ServiceAccountError> {
    let mut tx = pool.begin().await?;
    let account =
        create_service_account_tx(&mut tx, actor, owner_user_id, new_account, now_ms).await?;
    tx.commit().await?;
    Ok(account)
}

/// Retrieve one Service Account by identifier inside a transaction.
pub async fn get_service_account_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: &str,
) -> Result<ServiceAccount, ServiceAccountError> {
    let row = sqlx::query("SELECT * FROM service_accounts WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ServiceAccountError::NotFound(id.to_string()))?;
    let account = service_account_from_row(&row)?;
    check_access(actor, &account)?;
    Ok(account)
}

/// Retrieve one Service Account by identifier.
pub async fn get_service_account(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: &str,
) -> Result<ServiceAccount, ServiceAccountError> {
    let mut tx = pool.begin().await?;
    let account = get_service_account_tx(&mut tx, actor, id).await?;
    tx.commit().await?;
    Ok(account)
}

/// List Service Accounts visible to the actor, optionally filtered by scope.
///
/// # Errors
///
/// Returns [`ServiceAccountError::Forbidden`] when a standard user requests the
/// global workspace.
pub async fn list_service_accounts(
    pool: &SqlitePool,
    actor: &ActorScope,
    workspace_scope: Option<&str>,
    query: Option<&str>,
) -> Result<Vec<ServiceAccount>, ServiceAccountError> {
    let scope = workspace_scope.unwrap_or("personal");
    validate_scope(scope)?;
    if scope == "global" && !(actor.is_admin() || actor.is_system()) {
        return Err(ServiceAccountError::Forbidden);
    }
    let owner = actor.user_id();
    if scope == "personal" && owner.is_none() && !actor.is_admin() && !actor.is_system() {
        return Err(ServiceAccountError::Forbidden);
    }

    let like = query
        .filter(|q| !q.trim().is_empty())
        .map(|q| format!("%{}%", q.trim().to_lowercase()));

    let rows = sqlx::query(
        "SELECT * FROM service_accounts
         WHERE workspace_scope = ?
           AND (? IS NULL OR owner_user_id = ?)
           AND (? IS NULL OR LOWER(name) LIKE ?)
         ORDER BY LOWER(name) ASC",
    )
    .bind(scope)
    .bind(owner.map(|id| id.to_string()))
    .bind(owner.map(|id| id.to_string()))
    .bind(like.clone())
    .bind(like)
    .fetch_all(pool)
    .await?;

    rows.iter().map(service_account_from_row).collect()
}

/// Update a Service Account's name, description, or disabled state.
pub async fn patch_service_account_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: &str,
    patch: ServiceAccountPatch,
    now_ms: i64,
) -> Result<ServiceAccount, ServiceAccountError> {
    let existing = get_service_account_tx(tx, actor, id).await?;
    let name = patch.name.unwrap_or(existing.name.clone());
    validate_name(&name)?;
    let description = patch.description.unwrap_or(existing.description.clone());
    let disabled = patch.disabled.unwrap_or(existing.disabled);

    let result = sqlx::query(
        "UPDATE service_accounts SET name = ?, description = ?, disabled = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(name.trim())
    .bind(description.as_deref())
    .bind(if disabled { 1i64 } else { 0i64 })
    .bind(now_ms)
    .bind(id)
    .execute(&mut **tx)
    .await;
    match result {
        Ok(_) => {}
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            return Err(ServiceAccountError::NameTaken);
        }
        Err(error) => return Err(ServiceAccountError::Sqlx(error)),
    }

    Ok(ServiceAccount {
        name: name.trim().to_string(),
        description,
        disabled,
        updated_at: now_ms,
        ..existing
    })
}

/// Update a Service Account's name, description, or disabled state.
pub async fn patch_service_account(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: &str,
    patch: ServiceAccountPatch,
    now_ms: i64,
) -> Result<ServiceAccount, ServiceAccountError> {
    let mut tx = pool.begin().await?;
    let account = patch_service_account_tx(&mut tx, actor, id, patch, now_ms).await?;
    tx.commit().await?;
    Ok(account)
}

/// Replace the inherited CIDR allow-list for a Service Account.
pub async fn set_service_account_cidrs_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: &str,
    cidrs: Option<Vec<String>>,
    now_ms: i64,
) -> Result<ServiceAccount, ServiceAccountError> {
    let existing = get_service_account_tx(tx, actor, id).await?;
    let normalized = match cidrs {
        Some(list) => normalize_cidrs(&list)?,
        None => None,
    };
    let cidrs_json = normalized
        .as_ref()
        .and_then(|c| serde_json::to_string(c).ok());
    sqlx::query("UPDATE service_accounts SET allowed_cidrs = ?, updated_at = ? WHERE id = ?")
        .bind(cidrs_json)
        .bind(now_ms)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(ServiceAccount {
        allowed_cidrs: normalized,
        updated_at: now_ms,
        ..existing
    })
}

/// Replace the inherited CIDR allow-list for a Service Account.
pub async fn set_service_account_cidrs(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: &str,
    cidrs: Option<Vec<String>>,
    now_ms: i64,
) -> Result<ServiceAccount, ServiceAccountError> {
    let mut tx = pool.begin().await?;
    let account = set_service_account_cidrs_tx(&mut tx, actor, id, cidrs, now_ms).await?;
    tx.commit().await?;
    Ok(account)
}

/// Delete a Service Account and cascade to its child client keys.
pub async fn delete_service_account_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id: &str,
) -> Result<(), ServiceAccountError> {
    let existing = get_service_account_tx(tx, actor, id).await?;
    check_access(actor, &existing)?;
    sqlx::query("DELETE FROM service_accounts WHERE id = ?")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Delete a Service Account and cascade to its child client keys.
pub async fn delete_service_account(
    pool: &SqlitePool,
    actor: &ActorScope,
    id: &str,
) -> Result<(), ServiceAccountError> {
    let mut tx = pool.begin().await?;
    delete_service_account_tx(&mut tx, actor, id).await?;
    tx.commit().await?;
    Ok(())
}

/// Load every Service Account into the immutable configuration snapshot.
pub async fn load_service_account_records_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
) -> Result<HashMap<String, ServiceAccountRecord>, ServiceAccountError> {
    let rows = sqlx::query("SELECT * FROM service_accounts")
        .fetch_all(&mut **tx)
        .await?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        let account = service_account_from_row(&row)?;
        out.insert(
            account.id.clone(),
            ServiceAccountRecord {
                id: account.id,
                owner_user_id: account.owner_user_id,
                workspace_scope: account.workspace_scope,
                name: account.name,
                description: account.description,
                allowed_cidrs: account.allowed_cidrs,
                disabled: account.disabled,
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
