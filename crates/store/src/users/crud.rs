use std::collections::HashMap;
use std::str::FromStr;

use llmroller_core::ids::{RequestId, UserId};
use llmroller_core::rbac::{ActorScope, Role};
use llmroller_core::seams::{EntropySource, ExposeSecret, SecretString};
use llmroller_core::snapshot::UserRecord;
use sqlx::sqlite::SqliteRow;
use sqlx::{Connection as _, Row, SqlitePool, Transaction};

use super::password::{generate_temporary_password, hash_password, verify_password};
use super::types::{
    NewUser, ResetAdminResult, User, UserError, UserLoginRecord, UserPatch,
    actor_user, ensure_admin, ensure_self_or_admin, role_from_str, user_from_row,
    validate_username,
};
use crate::audit::{self, AuditEntry};

fn map_insert_error(error: sqlx::Error) -> UserError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        UserError::DuplicateUsername
    } else {
        UserError::Sqlx(error)
    }
}

/// Create a standard user or administrator.
///
/// Spec: AUTH-4, AUTH-5, SEC-4
///
/// # Errors
///
/// Returns validation, duplicate-username, permission, entropy, or SQLite failures.
pub async fn create_user(
    pool: &SqlitePool,
    actor: &ActorScope,
    new_user: NewUser<'_>,
    entropy: &dyn EntropySource,
    now_ms: i64,
) -> Result<User, UserError> {
    let mut tx = pool.begin().await?;
    let user = create_user_tx(&mut tx, actor, new_user, entropy, now_ms).await?;
    tx.commit().await?;
    Ok(user)
}

/// Create a standard user or administrator inside an active transaction.
pub async fn create_user_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    new_user: NewUser<'_>,
    entropy: &dyn EntropySource,
    now_ms: i64,
) -> Result<User, UserError> {
    ensure_admin(actor)?;
    let username = validate_username(new_user.username)?;
    let password_hash = hash_password(new_user.password, entropy)?;
    let id = UserId::generate();
    let result = sqlx::query(
        "INSERT INTO users (
            id, username, password_hash, role, must_change_password,
            totp_enabled, can_use_global_providers, disabled, created_at
         ) VALUES (?, ?, ?, ?, ?, 0, 1, 0, ?)",
    )
    .bind(id.to_string())
    .bind(username)
    .bind(password_hash)
    .bind(new_user.role.as_str())
    .bind(i64::from(new_user.must_change_password))
    .bind(now_ms)
    .execute(&mut **tx)
    .await;
    result.map_err(map_insert_error)?;
    get_user_by_id_tx(tx, id).await
}

/// List all accounts in creation order.
///
/// Spec: AUTH-4
///
/// # Errors
///
/// Returns a permission or SQLite failure.
pub async fn list_users(pool: &SqlitePool, actor: &ActorScope) -> Result<Vec<User>, UserError> {
    ensure_admin(actor)?;
    let rows = sqlx::query(
        "SELECT id, username, role, must_change_password, totp_enabled,
                can_use_global_providers, global_budget_micro_usd_month, disabled,
                created_at, last_login_at
         FROM users ORDER BY created_at ASC, id ASC",
    )
    .fetch_all(pool)
    .await?;
    rows.iter().map(user_from_row).collect()
}

/// Load all user accounts into [`UserRecord`] projections for [`ConfigSnapshot`].
pub async fn load_user_records(
    pool: &SqlitePool,
) -> Result<HashMap<UserId, UserRecord>, UserError> {
    let rows = sqlx::query(
        "SELECT id, username, role, disabled, can_use_global_providers, global_budget_micro_usd_month, default_routing_scope
         FROM users",
    )
    .fetch_all(pool)
    .await?;
    parse_user_records(&rows)
}

/// Load all user accounts into [`UserRecord`] projections within an active transaction.
pub async fn load_user_records_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
) -> Result<HashMap<UserId, UserRecord>, UserError> {
    let rows = sqlx::query(
        "SELECT id, username, role, disabled, can_use_global_providers, global_budget_micro_usd_month, default_routing_scope
         FROM users",
    )
    .fetch_all(&mut **tx)
    .await?;
    parse_user_records(&rows)
}

fn parse_user_records(rows: &[SqliteRow]) -> Result<HashMap<UserId, UserRecord>, UserError> {
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let id_str: String = row.try_get("id")?;
        let user_id = UserId::from_str(&id_str).map_err(|_| UserError::Corrupt)?;
        let username: String = row.try_get("username")?;
        let role_str: String = row.try_get("role")?;
        let role = role_from_str(&role_str)?;
        let disabled: i64 = row.try_get("disabled")?;
        let can_use_global: i64 = row.try_get("can_use_global_providers")?;
        let global_budget: Option<i64> = row.try_get("global_budget_micro_usd_month")?;
        let default_scope_str: String = row
            .try_get("default_routing_scope")
            .unwrap_or_else(|_| "personal_first".to_string());
        let default_routing_scope = default_scope_str.parse().unwrap_or_default();

        map.insert(
            user_id,
            UserRecord {
                id: user_id,
                username,
                role,
                disabled: disabled != 0,
                can_use_global_providers: can_use_global != 0,
                global_budget_micro_usd_month: global_budget,
                default_routing_scope,
            },
        );
    }
    Ok(map)
}

/// Load one account by ULID without an authorization check.
///
/// This is used by session resolution after the session itself has been authenticated.
///
/// # Errors
///
/// Returns corruption or SQLite failures.
pub async fn get_user_by_id(pool: &SqlitePool, user_id: UserId) -> Result<User, UserError> {
    let row = sqlx::query(
        "SELECT id, username, role, must_change_password, totp_enabled,
                can_use_global_providers, global_budget_micro_usd_month, disabled,
                created_at, last_login_at
         FROM users WHERE id = ?",
    )
    .bind(user_id.to_string())
    .fetch_optional(pool)
    .await?
    .ok_or(UserError::NotFound)?;
    user_from_row(&row)
}

/// Load one account by ULID inside an active transaction.
pub async fn get_user_by_id_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    user_id: UserId,
) -> Result<User, UserError> {
    let row = sqlx::query(
        "SELECT id, username, role, must_change_password, totp_enabled,
                can_use_global_providers, global_budget_micro_usd_month, disabled,
                created_at, last_login_at
         FROM users WHERE id = ?",
    )
    .bind(user_id.to_string())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(UserError::NotFound)?;
    user_from_row(&row)
}

/// Load one account with self-or-admin authorization.
///
/// # Errors
///
/// Returns permission, not-found, corruption, or SQLite failures.
pub async fn get_user(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
) -> Result<User, UserError> {
    ensure_self_or_admin(actor, user_id)?;
    get_user_by_id(pool, user_id).await
}

/// Count accounts. `0` gates the permanent setup flow.
///
/// # Errors
///
/// Returns a SQLite failure.
pub async fn count_users(pool: &SqlitePool) -> Result<i64, UserError> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?)
}

/// Load login verifier material by username. No authorization is applied.
///
/// # Errors
///
/// Returns not-found, corruption, or SQLite failures.
pub async fn find_login_record(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<UserLoginRecord>, UserError> {
    let row = sqlx::query(
        "SELECT id, username, role, must_change_password, totp_enabled,
                can_use_global_providers, global_budget_micro_usd_month, disabled,
                created_at, last_login_at, password_hash, totp_secret_enc,
                totp_master_key_id, totp_last_step, totp_recovery_hashes_json
         FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(login_record_from_row).transpose()
}

/// Load login verifier material by account ID. No authorization is applied.
///
/// # Errors
///
/// Returns not-found, corruption, or SQLite failures.
pub async fn find_login_record_by_id(
    pool: &SqlitePool,
    user_id: UserId,
) -> Result<UserLoginRecord, UserError> {
    let row = sqlx::query(
        "SELECT id, username, role, must_change_password, totp_enabled,
                can_use_global_providers, global_budget_micro_usd_month, disabled,
                created_at, last_login_at, password_hash, totp_secret_enc,
                totp_master_key_id, totp_last_step, totp_recovery_hashes_json
         FROM users WHERE id = ?",
    )
    .bind(user_id.to_string())
    .fetch_optional(pool)
    .await?
    .ok_or(UserError::NotFound)?;
    login_record_from_row(&row)
}

/// Load login verifier material by account ID inside an active transaction.
pub async fn find_login_record_by_id_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    user_id: UserId,
) -> Result<UserLoginRecord, UserError> {
    let row = sqlx::query(
        "SELECT id, username, role, must_change_password, totp_enabled,
                can_use_global_providers, global_budget_micro_usd_month, disabled,
                created_at, last_login_at, password_hash, totp_secret_enc,
                totp_master_key_id, totp_last_step, totp_recovery_hashes_json
         FROM users WHERE id = ?",
    )
    .bind(user_id.to_string())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(UserError::NotFound)?;
    login_record_from_row(&row)
}

fn login_record_from_row(row: &SqliteRow) -> Result<UserLoginRecord, UserError> {
    let recovery_json: Option<String> = row.try_get("totp_recovery_hashes_json")?;
    let recovery_hashes = recovery_json
        .map(|value| serde_json::from_str::<Vec<String>>(&value))
        .transpose()
        .map_err(|_| UserError::Corrupt)?
        .unwrap_or_default();
    Ok(UserLoginRecord {
        user: user_from_row(row)?,
        password_hash: row.try_get("password_hash")?,
        totp_secret_enc: row.try_get("totp_secret_enc")?,
        totp_master_key_id: row.try_get("totp_master_key_id")?,
        totp_last_step: row.try_get("totp_last_step")?,
        recovery_hashes,
    })
}

/// Apply administrator-controlled account changes under the in-transaction
/// last-administrator guard.
///
/// Spec: AUTH-5
///
/// # Errors
///
/// Returns permission, not-found, last-administrator, or SQLite failures.
pub async fn patch_user(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
    patch: UserPatch,
    now_ms: i64,
) -> Result<User, UserError> {
    let mut tx = pool.begin().await?;
    let user = patch_user_tx(&mut tx, actor, user_id, patch, now_ms).await?;
    tx.commit().await?;
    Ok(user)
}

/// Apply administrator-controlled account changes inside an active transaction.
pub async fn patch_user_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    user_id: UserId,
    patch: UserPatch,
    _now_ms: i64,
) -> Result<User, UserError> {
    ensure_admin(actor)?;
    let target = sqlx::query("SELECT role, disabled FROM users WHERE id = ?")
        .bind(user_id.to_string())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(UserError::NotFound)?;
    let current_role: String = target.try_get("role")?;
    let current_disabled = target.try_get::<i64, _>("disabled")? != 0;
    let new_role = patch.role.unwrap_or(role_from_str(&current_role)?);
    let new_disabled = patch.disabled.unwrap_or(current_disabled);
    if current_role == Role::Admin.as_str()
        && !current_disabled
        && (new_role != Role::Admin || new_disabled)
    {
        ensure_another_enabled_admin(tx, user_id).await?;
    }

    let role_changed = patch.role.is_some();
    let disabled_changed = patch.disabled.is_some();
    let globals_changed = patch.can_use_global_providers.is_some();
    let budget_changed = patch.global_budget_micro_usd_month.is_some();
    let budget = patch.global_budget_micro_usd_month.flatten();
    let result = sqlx::query(
        "UPDATE users SET
            role = CASE WHEN ? THEN ? ELSE role END,
            disabled = CASE WHEN ? THEN ? ELSE disabled END,
            can_use_global_providers = CASE WHEN ? THEN ? ELSE can_use_global_providers END,
            global_budget_micro_usd_month = CASE WHEN ? THEN ? ELSE global_budget_micro_usd_month END
         WHERE id = ?",
    )
    .bind(i64::from(role_changed))
    .bind(new_role.as_str())
    .bind(i64::from(disabled_changed))
    .bind(i64::from(new_disabled))
    .bind(i64::from(globals_changed))
    .bind(i64::from(patch.can_use_global_providers.unwrap_or(true)))
    .bind(i64::from(budget_changed))
    .bind(budget)
    .bind(user_id.to_string())
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(UserError::NotFound);
    }
    if new_disabled {
        sqlx::query("DELETE FROM web_sessions WHERE user_id = ?")
            .bind(user_id.to_string())
            .execute(&mut **tx)
            .await?;
    }
    get_user_by_id_tx(tx, user_id).await
}

async fn ensure_another_enabled_admin(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    excluded: UserId,
) -> Result<(), UserError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users
         WHERE role = 'admin' AND disabled = 0 AND id != ?",
    )
    .bind(excluded.to_string())
    .fetch_one(&mut **tx)
    .await?;
    if count == 0 {
        Err(UserError::LastAdmin)
    } else {
        Ok(())
    }
}

/// Set an account role while preserving at least one enabled administrator.
///
/// # Errors
///
/// Returns permission, not-found, last-administrator, or SQLite failures.
pub async fn set_role(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
    role: Role,
    now_ms: i64,
) -> Result<User, UserError> {
    patch_user(
        pool,
        actor,
        user_id,
        UserPatch {
            role: Some(role),
            ..UserPatch::default()
        },
        now_ms,
    )
    .await
}

/// Enable or disable an account while preserving at least one enabled administrator.
///
/// # Errors
///
/// Returns permission, not-found, last-administrator, or SQLite failures.
pub async fn set_disabled(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
    disabled: bool,
    now_ms: i64,
) -> Result<User, UserError> {
    patch_user(
        pool,
        actor,
        user_id,
        UserPatch {
            disabled: Some(disabled),
            ..UserPatch::default()
        },
        now_ms,
    )
    .await
}

/// Delete an account, cascade personal resources, revoke sessions and shares,
/// retain usage history, and record `deleted_users`.
///
/// Spec: AUTH-6
///
/// # Errors
///
/// Returns permission, not-found, confirmation, last-administrator, or SQLite failures.
pub async fn delete_user(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
    confirmation: &str,
    now_ms: i64,
) -> Result<(), UserError> {
    let mut tx = pool.begin().await?;
    delete_user_tx(&mut tx, actor, user_id, confirmation, now_ms).await?;
    tx.commit().await?;
    Ok(())
}

/// Delete an account inside an active transaction.
pub async fn delete_user_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    user_id: UserId,
    confirmation: &str,
    now_ms: i64,
) -> Result<(), UserError> {
    ensure_admin(actor)?;
    let target = sqlx::query("SELECT username, role, disabled FROM users WHERE id = ?")
        .bind(user_id.to_string())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(UserError::NotFound)?;
    let username: String = target.try_get("username")?;
    let role: String = target.try_get("role")?;
    let disabled = target.try_get::<i64, _>("disabled")? != 0;
    if confirmation != username {
        return Err(UserError::ConfirmationMismatch);
    }
    if role == Role::Admin.as_str() && !disabled {
        ensure_another_enabled_admin(tx, user_id).await?;
    }

    sqlx::query(
        "UPDATE provider_shares SET enabled = 0, revoked_at = ?
         WHERE owner_user_id = ? OR grantee_user_id = ?",
    )
    .bind(now_ms)
    .bind(user_id.to_string())
    .bind(user_id.to_string())
    .execute(&mut **tx)
    .await?;
    for statement in [
        "DELETE FROM pins WHERE client_key_id IN
            (SELECT id FROM client_keys WHERE owner_user_id = ?)",
        "DELETE FROM response_links WHERE owner_user_id = ?",
        "DELETE FROM trace_records WHERE owner_user_id = ?",
        "DELETE FROM trace_blocks WHERE owner_user_id = ?",
        "DELETE FROM web_sessions WHERE user_id = ?",
        "DELETE FROM upstream_keys WHERE owner_user_id = ?",
        "DELETE FROM providers WHERE owner_user_id = ?",
        "DELETE FROM client_keys WHERE owner_user_id = ?",
        "DELETE FROM users WHERE id = ?",
    ] {
        sqlx::query(statement)
            .bind(user_id.to_string())
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("INSERT INTO deleted_users (user_id, username, deleted_at) VALUES (?, ?, ?)")
        .bind(user_id.to_string())
        .bind(&username)
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Set a password directly and revoke sessions.
///
/// # Errors
///
/// Returns permission, not-found, validation, entropy, or SQLite failures.
pub async fn set_password(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
    new_password: &str,
    must_change_password: bool,
    entropy: &dyn EntropySource,
    keep_session: Option<&str>,
) -> Result<User, UserError> {
    let mut tx = pool.begin().await?;
    let user = set_password_tx(
        &mut tx,
        actor,
        user_id,
        new_password,
        must_change_password,
        entropy,
        keep_session,
    )
    .await?;
    tx.commit().await?;
    Ok(user)
}

/// Set a password directly and revoke sessions inside an active transaction.
pub async fn set_password_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    user_id: UserId,
    new_password: &str,
    must_change_password: bool,
    entropy: &dyn EntropySource,
    keep_session: Option<&str>,
) -> Result<User, UserError> {
    ensure_self_or_admin(actor, user_id)?;
    let password_hash = hash_password(new_password, entropy)?;
    let result =
        sqlx::query("UPDATE users SET password_hash = ?, must_change_password = ? WHERE id = ?")
            .bind(password_hash)
            .bind(i64::from(must_change_password))
            .bind(user_id.to_string())
            .execute(&mut **tx)
            .await?;
    if result.rows_affected() == 0 {
        return Err(UserError::NotFound);
    }
    if let Some(keep) = keep_session {
        sqlx::query("DELETE FROM web_sessions WHERE user_id = ? AND id_hash != ?")
            .bind(user_id.to_string())
            .bind(keep)
            .execute(&mut **tx)
            .await?;
    } else {
        sqlx::query("DELETE FROM web_sessions WHERE user_id = ?")
            .bind(user_id.to_string())
            .execute(&mut **tx)
            .await?;
    }
    get_user_by_id_tx(tx, user_id).await
}

/// Change one's own password after verifying the current password.
///
/// # Errors
///
/// Returns permission, not-found, invalid-credentials, validation, entropy,
/// or SQLite failures.
pub async fn change_password(
    pool: &SqlitePool,
    actor: &ActorScope,
    current_password: &str,
    new_password: &str,
    entropy: &dyn EntropySource,
    keep_session: Option<&str>,
) -> Result<User, UserError> {
    let mut tx = pool.begin().await?;
    let user = change_password_tx(
        &mut tx,
        actor,
        current_password,
        new_password,
        entropy,
        keep_session,
    )
    .await?;
    tx.commit().await?;
    Ok(user)
}

/// Change one's own password inside an active transaction.
pub async fn change_password_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    current_password: &str,
    new_password: &str,
    entropy: &dyn EntropySource,
    keep_session: Option<&str>,
) -> Result<User, UserError> {
    let user_id = actor_user(actor)?;
    let record = find_login_record_by_id_tx(tx, user_id).await?;
    if !verify_password(current_password, &record.password_hash) {
        return Err(UserError::InvalidCredentials);
    }
    set_password_tx(
        tx,
        actor,
        user_id,
        new_password,
        false,
        entropy,
        keep_session,
    )
    .await
}

/// Generate a temporary password, mark the account for change, and revoke sessions.
///
/// # Errors
///
/// Returns permission, not-found, validation, entropy, or SQLite failures.
pub async fn reset_password_temporary(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
    entropy: &dyn EntropySource,
) -> Result<(User, SecretString), UserError> {
    ensure_admin(actor)?;
    let temporary_password = generate_temporary_password(entropy);
    let user = set_password(
        pool,
        actor,
        user_id,
        temporary_password.expose_secret(),
        true,
        entropy,
        None,
    )
    .await?;
    Ok((user, temporary_password))
}

/// Record a successful login.
///
/// # Errors
///
/// Returns not-found or SQLite failures.
pub async fn mark_login(pool: &SqlitePool, user_id: UserId, now_ms: i64) -> Result<(), UserError> {
    let result = sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
        .bind(now_ms)
        .bind(user_id.to_string())
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(UserError::NotFound);
    }
    Ok(())
}

/// Run the offline `reset-admin` recovery operation and append its audit row
/// in the same transaction.
///
/// Spec: AUTH-6
///
/// # Errors
///
/// Returns not-found, validation, entropy, audit, or SQLite failures.
pub async fn reset_admin(
    pool: &SqlitePool,
    username: &str,
    entropy: &dyn EntropySource,
    audit_hmac_key: &[u8],
    now_ms: i64,
) -> Result<ResetAdminResult, UserError> {
    let existing = find_login_record(pool, username)
        .await?
        .ok_or(UserError::NotFound)?;
    let temporary_password = generate_temporary_password(entropy);
    let password_hash = hash_password(temporary_password.expose_secret(), entropy)?;
    let mut connection = pool.acquire().await?;
    let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
    let current_id: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&mut *transaction)
        .await?;
    if current_id.as_deref() != Some(existing.user.id.to_string().as_str()) {
        return Err(UserError::NotFound);
    }
    sqlx::query(
        "UPDATE users SET password_hash = ?, role = 'admin', disabled = 0,
            must_change_password = 1
         WHERE id = ?",
    )
    .bind(password_hash)
    .bind(existing.user.id.to_string())
    .execute(&mut *transaction)
    .await?;
    sqlx::query("DELETE FROM web_sessions WHERE user_id = ?")
        .bind(existing.user.id.to_string())
        .execute(&mut *transaction)
        .await?;
    let _ = crate::writer::bump_config_version_tx(&mut transaction).await?;
    let audit_id = RequestId::generate().to_string();
    let detail = r#"{"system_task":"cli:reset-admin"}"#;
    audit::append(
        &mut transaction,
        audit_hmac_key,
        AuditEntry {
            id: &audit_id,
            ts: now_ms,
            actor_user_id: None,
            ip_addr: None,
            action: "admin.break_glass",
            target_type: "user",
            target_id: Some(existing.user.id.to_string().as_str()),
            detail_json: Some(detail),
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(ResetAdminResult {
        user_id: existing.user.id,
        temporary_password,
    })
}
