#![forbid(unsafe_code)]
//! Human accounts, password hashing, TOTP, and web sessions.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Partial: AUTH-1, AUTH-4, AUTH-5, AUTH-6, AUTH-7, AUTH-8, AUTH-9
//! Partial: SEC-4, SEC-12

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;

use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use llmroller_core::ids::{RequestId, UserId};
use llmroller_core::rbac::{ActorScope, Role};
use llmroller_core::seams::{EntropySource, ExposeSecret, SecretString, secret_string};
use llmroller_core::snapshot::UserRecord;
use sqlx::sqlite::SqliteRow;
use sqlx::{Connection as _, Row, SqlitePool, Transaction};
use thiserror::Error;
use totp_rs::{Algorithm as TotpAlgorithm, Builder as TotpBuilder, Totp};

use crate::audit::{self, AuditEntry};
use crate::crypto::{CryptoError, SecretSealer, XChaCha20Poly1305Sealer, sha256_hex};

/// Minimum accepted password length in Unicode scalar values.
pub const MIN_PASSWORD_CHARS: usize = 12;
/// Maximum accepted password length in bytes.
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Number of one-time TOTP recovery codes issued at enrollment.
pub const TOTP_RECOVERY_CODE_COUNT: usize = 10;
const ARGON2_MEMORY_KIB: u32 = 19_456;
const ARGON2_TIME_COST: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;
const TOTP_SECRET_BYTES: usize = 20;

/// Public account projection. Password and TOTP material never appear here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// ULID primary key.
    pub id: UserId,
    /// Unique login name.
    pub username: String,
    /// Administration role.
    pub role: Role,
    /// Forces a password change after an administrative reset.
    pub must_change_password: bool,
    /// Whether TOTP second factor is active.
    pub totp_enabled: bool,
    /// Whether global providers may serve this user.
    pub can_use_global_providers: bool,
    /// Optional monthly global budget in micro-USD.
    pub global_budget_micro_usd_month: Option<i64>,
    /// Account suspension flag.
    pub disabled: bool,
    /// Creation time in UTC epoch milliseconds.
    pub created_at: i64,
    /// Most recent successful login time.
    pub last_login_at: Option<i64>,
}

/// Input for creating a user or the first administrator.
#[derive(Debug, Clone, Copy)]
pub struct NewUser<'a> {
    /// Login name.
    pub username: &'a str,
    /// Plaintext password, validated before hashing.
    pub password: &'a str,
    /// Initial role.
    pub role: Role,
    /// Mark the account as requiring a password change.
    pub must_change_password: bool,
}

/// Optional account fields changed by an administrator.
#[derive(Debug, Clone, Copy, Default)]
pub struct UserPatch {
    /// New role.
    pub role: Option<Role>,
    /// New suspension flag.
    pub disabled: Option<bool>,
    /// New global-provider permission.
    pub can_use_global_providers: Option<bool>,
    /// New global budget. The outer `Option` means unchanged; inner `None` clears it.
    pub global_budget_micro_usd_month: Option<Option<i64>>,
}

/// Login-time account record including verifier material.
#[derive(Clone)]
pub struct UserLoginRecord {
    /// Public account fields.
    pub user: User,
    /// Argon2id PHC string.
    pub password_hash: String,
    /// Sealed raw TOTP secret.
    pub totp_secret_enc: Option<Vec<u8>>,
    /// Master-key identifier used for `totp_secret_enc`.
    pub totp_master_key_id: Option<String>,
    /// Last accepted TOTP time step.
    pub totp_last_step: Option<i64>,
    /// SHA-256 hashes of unused recovery codes.
    pub recovery_hashes: Vec<String>,
}

impl std::fmt::Debug for UserLoginRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserLoginRecord")
            .field("user", &self.user)
            .field("password_hash", &"[redacted]")
            .field(
                "totp_secret_enc",
                &self.totp_secret_enc.as_ref().map(|_| "[sealed]"),
            )
            .field("totp_master_key_id", &self.totp_master_key_id)
            .field("totp_last_step", &self.totp_last_step)
            .field("recovery_hash_count", &self.recovery_hashes.len())
            .finish()
    }
}

/// A persisted web session row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSession {
    /// SHA-256 of the opaque session cookie token.
    pub id_hash: String,
    /// Owning account.
    pub user_id: UserId,
    /// SHA-256 of the per-session CSRF token.
    pub csrf_hash: String,
    /// Best-effort client address.
    pub ip_addr: Option<String>,
    /// Best-effort user agent.
    pub user_agent: Option<String>,
    /// Creation time in UTC epoch milliseconds.
    pub created_at: i64,
    /// Last authenticated request time.
    pub last_seen_at: i64,
    /// Absolute expiry time.
    pub expires_at: i64,
}

/// Session and its authenticated account.
#[derive(Debug, Clone)]
pub struct SessionWithUser {
    /// Session row.
    pub session: WebSession,
    /// Owning account.
    pub user: User,
}

/// New session row values.
#[derive(Debug, Clone, Copy)]
pub struct NewWebSession<'a> {
    /// SHA-256 of the session token.
    pub id_hash: &'a str,
    /// Owning account.
    pub user_id: UserId,
    /// SHA-256 of the CSRF token.
    pub csrf_hash: &'a str,
    /// Optional client address.
    pub ip_addr: Option<&'a str>,
    /// Optional user agent.
    pub user_agent: Option<&'a str>,
    /// Creation time.
    pub created_at: i64,
    /// Absolute expiry time.
    pub expires_at: i64,
}

/// One-time TOTP enrollment response.
#[derive(Debug)]
pub struct TotpEnrollment {
    /// Base32 secret for manual entry.
    pub secret_base32: SecretString,
    /// `otpauth://` provisioning URI.
    pub otpauth_url: String,
    /// One-time recovery codes shown exactly once.
    pub recovery_codes: Vec<SecretString>,
}

/// Result of consuming a TOTP or recovery code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotpVerification {
    /// A time-based code advanced the replay watermark.
    Totp,
    /// A recovery code was consumed.
    Recovery,
}

/// Break-glass administrator reset result.
#[derive(Debug)]
pub struct ResetAdminResult {
    /// Target account.
    pub user_id: UserId,
    /// Temporary password shown once to the operator.
    pub temporary_password: SecretString,
}

/// User repository failures.
#[derive(Debug, Error)]
pub enum UserError {
    /// SQLite failure.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// Sealed-secret failure.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// Audit append failure.
    #[error(transparent)]
    Audit(#[from] audit::AuditError),
    /// Stored row could not be decoded.
    #[error("user row contains corrupt data")]
    Corrupt,
    /// Username failed validation.
    #[error("invalid username: {0}")]
    InvalidUsername(&'static str),
    /// Password failed validation.
    #[error("password must contain at least {MIN_PASSWORD_CHARS} characters")]
    WeakPassword,
    /// Password exceeded the request hashing bound.
    #[error("password is too long")]
    PasswordTooLong,
    /// Password hashing failed.
    #[error("password hashing failed: {0}")]
    PasswordHash(String),
    /// Username already exists.
    #[error("username already exists")]
    DuplicateUsername,
    /// Account does not exist.
    #[error("user not found")]
    NotFound,
    /// Caller lacks permission.
    #[error("permission denied")]
    Forbidden,
    /// Operation would leave zero enabled administrators.
    #[error("operation would remove the last administrator")]
    LastAdmin,
    /// Confirmation string did not match the target username.
    #[error("confirmation does not match the target username")]
    ConfirmationMismatch,
    /// Supplied password is wrong.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// Stored TOTP ciphertext belongs to a different master key.
    #[error("TOTP ciphertext was sealed under a different master key")]
    KeyMismatch,
    /// TOTP is not enrolled.
    #[error("TOTP is not enrolled")]
    TotpNotEnrolled,
    /// TOTP is already enabled.
    #[error("TOTP is already enabled")]
    TotpAlreadyEnabled,
    /// TOTP or recovery code was rejected.
    #[error("invalid TOTP or recovery code")]
    InvalidTotp,
    /// A TOTP time step was already consumed.
    #[error("TOTP code was already used")]
    TotpReplay,
}

impl UserError {
    /// Process exit code for CLI failures.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::NotFound => 10,
            Self::Crypto(_) => 3,
            _ => 13,
        }
    }
}

/// Build the exact Argon2id configuration required by `SEC-4`.
fn argon2() -> Result<Argon2<'static>, UserError> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        None,
    )
    .map_err(|error| UserError::PasswordHash(error.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Validate a candidate password before hashing.
///
/// # Errors
///
/// Returns [`UserError::WeakPassword`] below the minimum or
/// [`UserError::PasswordTooLong`] above the request bound.
pub fn validate_password(password: &str) -> Result<(), UserError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(UserError::WeakPassword);
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(UserError::PasswordTooLong);
    }
    Ok(())
}

/// Hash a password with Argon2id, a fresh 128-bit salt, and the mandated parameters.
///
/// # Errors
///
/// Returns a validation, entropy, or hashing failure.
pub fn hash_password(password: &str, entropy: &dyn EntropySource) -> Result<String, UserError> {
    validate_password(password)?;
    let mut salt_bytes = [0_u8; 16];
    entropy.fill_bytes(&mut salt_bytes);
    argon2()?
        .hash_password_with_salt(password.as_bytes(), &salt_bytes)
        .map(|hash| hash.to_string())
        .map_err(|error| UserError::PasswordHash(error.to_string()))
}

/// Verify a candidate password against a stored PHC string.
#[must_use]
pub fn verify_password(password: &str, password_hash: &str) -> bool {
    if password.len() > MAX_PASSWORD_BYTES {
        return false;
    }
    let Ok(parsed) = PasswordHash::new(password_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Generate a cryptographically random temporary password shown once.
#[must_use]
pub fn generate_temporary_password(entropy: &dyn EntropySource) -> SecretString {
    let mut bytes = [0_u8; 24];
    entropy.fill_bytes(&mut bytes);
    secret_string(URL_SAFE_NO_PAD.encode(bytes))
}

/// Validate username syntax and length bounds.
pub fn validate_username(username: &str) -> Result<&str, UserError> {
    if username.is_empty() {
        return Err(UserError::InvalidUsername("must not be empty"));
    }
    if username.len() > 64 {
        return Err(UserError::InvalidUsername("must be at most 64 bytes"));
    }
    if !username
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(UserError::InvalidUsername(
            "must contain only ASCII letters, digits, '_', '-', or '.'",
        ));
    }
    Ok(username)
}

fn role_from_str(value: &str) -> Result<Role, UserError> {
    match value {
        "admin" => Ok(Role::Admin),
        "user" => Ok(Role::User),
        _ => Err(UserError::Corrupt),
    }
}

fn user_from_row(row: &SqliteRow) -> Result<User, UserError> {
    let id: String = row.try_get("id")?;
    let role: String = row.try_get("role")?;
    Ok(User {
        id: UserId::from_str(&id).map_err(|_| UserError::Corrupt)?,
        username: row.try_get("username")?,
        role: role_from_str(&role)?,
        must_change_password: row.try_get::<i64, _>("must_change_password")? != 0,
        totp_enabled: row.try_get::<i64, _>("totp_enabled")? != 0,
        can_use_global_providers: row.try_get::<i64, _>("can_use_global_providers")? != 0,
        global_budget_micro_usd_month: row.try_get("global_budget_micro_usd_month")?,
        disabled: row.try_get::<i64, _>("disabled")? != 0,
        created_at: row.try_get("created_at")?,
        last_login_at: row.try_get("last_login_at")?,
    })
}

fn ensure_admin(actor: &ActorScope) -> Result<(), UserError> {
    if actor.is_admin() || actor.is_system() {
        Ok(())
    } else {
        Err(UserError::Forbidden)
    }
}

fn ensure_self_or_admin(actor: &ActorScope, target: UserId) -> Result<(), UserError> {
    if actor.is_admin() || actor.is_system() || actor.user_id() == Some(target) {
        Ok(())
    } else {
        Err(UserError::Forbidden)
    }
}

fn actor_user(actor: &ActorScope) -> Result<UserId, UserError> {
    actor.user_id().ok_or(UserError::Forbidden)
}

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

/// Insert a web session row.
///
/// # Errors
///
/// Returns SQLite failures.
pub async fn insert_web_session(
    pool: &SqlitePool,
    session: NewWebSession<'_>,
) -> Result<(), UserError> {
    sqlx::query(
        "INSERT INTO web_sessions
            (id_hash, user_id, csrf_hash, ip_addr, user_agent, created_at, last_seen_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(session.id_hash)
    .bind(session.user_id.to_string())
    .bind(session.csrf_hash)
    .bind(session.ip_addr)
    .bind(session.user_agent)
    .bind(session.created_at)
    .bind(session.created_at)
    .bind(session.expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Load a session and its owning account.
///
/// # Errors
///
/// Returns corruption or SQLite failures.
pub async fn load_web_session(
    pool: &SqlitePool,
    id_hash: &str,
) -> Result<Option<SessionWithUser>, UserError> {
    let session = sqlx::query(
        "SELECT id_hash, user_id, csrf_hash, ip_addr, user_agent,
                created_at, last_seen_at, expires_at
         FROM web_sessions WHERE id_hash = ?",
    )
    .bind(id_hash)
    .fetch_optional(pool)
    .await?;
    let Some(session) = session else {
        return Ok(None);
    };
    let user_id: String = session.try_get("user_id")?;
    let user_id = UserId::from_str(&user_id).map_err(|_| UserError::Corrupt)?;
    let user = get_user_by_id(pool, user_id).await?;
    Ok(Some(SessionWithUser {
        session: WebSession {
            id_hash: session.try_get("id_hash")?,
            user_id,
            csrf_hash: session.try_get("csrf_hash")?,
            ip_addr: session.try_get("ip_addr")?,
            user_agent: session.try_get("user_agent")?,
            created_at: session.try_get("created_at")?,
            last_seen_at: session.try_get("last_seen_at")?,
            expires_at: session.try_get("expires_at")?,
        },
        user,
    }))
}

/// Update the last-seen timestamp for an active session.
///
/// # Errors
///
/// Returns SQLite failures.
pub async fn touch_web_session(
    pool: &SqlitePool,
    id_hash: &str,
    now_ms: i64,
) -> Result<(), UserError> {
    sqlx::query("UPDATE web_sessions SET last_seen_at = ? WHERE id_hash = ?")
        .bind(now_ms)
        .bind(id_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete one session by its hashed identifier.
///
/// # Errors
///
/// Returns SQLite failures.
pub async fn delete_web_session(pool: &SqlitePool, id_hash: &str) -> Result<(), UserError> {
    sqlx::query("DELETE FROM web_sessions WHERE id_hash = ?")
        .bind(id_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete all sessions for an account, optionally preserving one.
///
/// # Errors
///
/// Returns SQLite failures.
pub async fn delete_user_web_sessions(
    pool: &SqlitePool,
    user_id: UserId,
    keep: Option<&str>,
) -> Result<(), UserError> {
    if let Some(keep) = keep {
        sqlx::query("DELETE FROM web_sessions WHERE user_id = ? AND id_hash != ?")
            .bind(user_id.to_string())
            .bind(keep)
            .execute(pool)
            .await?;
    } else {
        sqlx::query("DELETE FROM web_sessions WHERE user_id = ?")
            .bind(user_id.to_string())
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Delete one session belonging to a user, enforcing self-or-admin access.
///
/// # Errors
///
/// Returns permission or SQLite failures.
pub async fn delete_owned_web_session(
    pool: &SqlitePool,
    actor: &ActorScope,
    id_hash: &str,
) -> Result<(), UserError> {
    let mut tx = pool.begin().await?;
    delete_owned_web_session_tx(&mut tx, actor, id_hash).await?;
    tx.commit().await?;
    Ok(())
}

/// Delete one owned session inside an active transaction.
pub async fn delete_owned_web_session_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id_hash: &str,
) -> Result<(), UserError> {
    let user_id: String = sqlx::query_scalar("SELECT user_id FROM web_sessions WHERE id_hash = ?")
        .bind(id_hash)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(UserError::NotFound)?;
    let user_id = UserId::from_str(&user_id).map_err(|_| UserError::Corrupt)?;
    ensure_self_or_admin(actor, user_id)?;
    sqlx::query("DELETE FROM web_sessions WHERE id_hash = ?")
        .bind(id_hash)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn build_totp(secret: &[u8]) -> Result<Totp, UserError> {
    TotpBuilder::new()
        .with_algorithm(TotpAlgorithm::SHA1)
        .with_digits(6)
        .with_skew(1)
        .with_step_duration(30)
        .with_secret(secret.to_vec())
        .build()
        .map_err(|error| UserError::PasswordHash(error.to_string()))
}

fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|character| *character != '-' && !character.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

fn recovery_hash(code: &str) -> String {
    sha256_hex(normalize_recovery_code(code).as_bytes())
}

/// Enroll a TOTP secret and issue one-time recovery codes.
///
/// Spec: AUTH-9
///
/// # Errors
///
/// Returns permission, not-found, already-enabled, crypto, entropy, or SQLite failures.
pub async fn enroll_totp(
    pool: &SqlitePool,
    actor: &ActorScope,
    sealer: &XChaCha20Poly1305Sealer,
    key_id: &str,
    entropy: &dyn EntropySource,
) -> Result<TotpEnrollment, UserError> {
    let mut tx = pool.begin().await?;
    let enrollment = enroll_totp_tx(&mut tx, actor, sealer, key_id, entropy).await?;
    tx.commit().await?;
    Ok(enrollment)
}

/// Enroll a TOTP secret inside an active transaction.
pub async fn enroll_totp_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    sealer: &XChaCha20Poly1305Sealer,
    key_id: &str,
    entropy: &dyn EntropySource,
) -> Result<TotpEnrollment, UserError> {
    let user_id = actor_user(actor)?;
    let record = find_login_record_by_id_tx(tx, user_id).await?;
    if record.user.totp_enabled {
        return Err(UserError::TotpAlreadyEnabled);
    }
    let mut secret = [0_u8; TOTP_SECRET_BYTES];
    entropy.fill_bytes(&mut secret);
    let totp = build_totp(&secret)?;
    let user_id_text = user_id.to_string();
    let fields = [key_id.as_bytes(), user_id_text.as_bytes()];
    let sealed = sealer.try_seal("totp_secret", &fields, &secret)?;
    let mut recovery_codes = Vec::with_capacity(TOTP_RECOVERY_CODE_COUNT);
    let mut recovery_hashes = BTreeSet::new();
    while recovery_hashes.len() < TOTP_RECOVERY_CODE_COUNT {
        let mut bytes = [0_u8; 8];
        entropy.fill_bytes(&mut bytes);
        let raw = bytes
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>();
        let formatted = format!(
            "{}-{}-{}-{}",
            &raw[0..4],
            &raw[4..8],
            &raw[8..12],
            &raw[12..16]
        );
        if recovery_hashes.insert(recovery_hash(&formatted)) {
            recovery_codes.push(secret_string(formatted));
        }
    }
    let hashes_json = serde_json::to_string(&recovery_hashes)
        .map_err(|error| UserError::PasswordHash(error.to_string()))?;
    let result = sqlx::query(
        "UPDATE users SET totp_secret_enc = ?, totp_master_key_id = ?,
            totp_enabled = 0, totp_last_step = NULL, totp_recovery_hashes_json = ?
         WHERE id = ?",
    )
    .bind(sealed)
    .bind(key_id)
    .bind(hashes_json)
    .bind(user_id.to_string())
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(UserError::NotFound);
    }
    let secret_base32 = totp.secret().to_base32();
    let otpauth_url = format!(
        "otpauth://totp/LLMRoller:{}?secret={}&issuer=LLMRoller&algorithm=SHA1&digits=6&period=30",
        record.user.username, secret_base32
    );
    Ok(TotpEnrollment {
        secret_base32: secret_string(secret_base32),
        otpauth_url,
        recovery_codes,
    })
}

/// Confirm enrollment with a current code and activate TOTP.
///
/// Spec: AUTH-9
///
/// # Errors
///
/// Returns permission, not-found, not-enrolled, crypto, invalid-code, replay,
/// or SQLite failures.
pub async fn confirm_totp(
    pool: &SqlitePool,
    actor: &ActorScope,
    code: &str,
    sealer: &XChaCha20Poly1305Sealer,
    key_id: &str,
    now_ms: i64,
) -> Result<(), UserError> {
    let mut tx = pool.begin().await?;
    confirm_totp_tx(&mut tx, actor, code, sealer, key_id, now_ms).await?;
    tx.commit().await?;
    Ok(())
}

/// Confirm TOTP enrollment inside an active transaction.
pub async fn confirm_totp_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    code: &str,
    sealer: &XChaCha20Poly1305Sealer,
    key_id: &str,
    now_ms: i64,
) -> Result<(), UserError> {
    let user_id = actor_user(actor)?;
    let record = find_login_record_by_id_tx(tx, user_id).await?;
    if record.user.totp_enabled {
        return Err(UserError::TotpAlreadyEnabled);
    }
    let sealed = record
        .totp_secret_enc
        .as_deref()
        .ok_or(UserError::TotpNotEnrolled)?;
    let key_matches = record
        .totp_master_key_id
        .as_deref()
        .is_some_and(|value| value == key_id);
    if !key_matches {
        return Err(UserError::KeyMismatch);
    }
    let user_id_text = user_id.to_string();
    let fields = [key_id.as_bytes(), user_id_text.as_bytes()];
    let secret = sealer.open("totp_secret", &fields, sealed)?;
    let totp = build_totp(secret.expose_secret())?;
    let step = totp
        .check(code, timestamp_seconds(now_ms))
        .ok_or(UserError::InvalidTotp)?;
    let step_i64 = i64::try_from(step).map_err(|_| UserError::Corrupt)?;
    let result = sqlx::query(
        "UPDATE users SET totp_enabled = 1, totp_last_step = ?
         WHERE id = ? AND totp_enabled = 0
           AND (totp_last_step IS NULL OR totp_last_step < ?)",
    )
    .bind(step_i64)
    .bind(user_id.to_string())
    .bind(step_i64)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(UserError::TotpReplay);
    }
    Ok(())
}

/// Verify a login TOTP or one-time recovery code and advance replay protection.
///
/// Spec: AUTH-9
///
/// # Errors
///
/// Returns not-enrolled, crypto, invalid-code, replay, corruption, or SQLite failures.
pub async fn verify_login_totp(
    pool: &SqlitePool,
    user_id: UserId,
    code: &str,
    sealer: &XChaCha20Poly1305Sealer,
    key_id: &str,
    now_ms: i64,
) -> Result<TotpVerification, UserError> {
    let record = find_login_record_by_id(pool, user_id).await?;
    if !record.user.totp_enabled {
        return Err(UserError::TotpNotEnrolled);
    }
    let sealed = record
        .totp_secret_enc
        .as_deref()
        .ok_or(UserError::TotpNotEnrolled)?;
    let key_matches = record
        .totp_master_key_id
        .as_deref()
        .is_some_and(|value| value == key_id);
    if !key_matches {
        return Err(UserError::KeyMismatch);
    }
    let user_id_text = user_id.to_string();
    let fields = [key_id.as_bytes(), user_id_text.as_bytes()];
    let secret = sealer.open("totp_secret", &fields, sealed)?;
    let totp = build_totp(secret.expose_secret())?;
    if let Some(step) = totp.check(code, timestamp_seconds(now_ms)) {
        let step_i64 = i64::try_from(step).map_err(|_| UserError::Corrupt)?;
        let result = sqlx::query(
            "UPDATE users SET totp_last_step = ?
             WHERE id = ? AND (totp_last_step IS NULL OR totp_last_step < ?)",
        )
        .bind(step_i64)
        .bind(user_id.to_string())
        .bind(step_i64)
        .execute(pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(UserError::TotpReplay);
        }
        return Ok(TotpVerification::Totp);
    }

    let candidate_hash = recovery_hash(code);
    let mut connection = pool.acquire().await?;
    let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
    let json: Option<String> =
        sqlx::query_scalar("SELECT totp_recovery_hashes_json FROM users WHERE id = ?")
            .bind(user_id.to_string())
            .fetch_optional(&mut *transaction)
            .await?
            .flatten();
    let mut hashes: Vec<String> = json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| UserError::Corrupt)?
        .unwrap_or_default();
    let Some(position) = hashes.iter().position(|hash| hash == &candidate_hash) else {
        return Err(UserError::InvalidTotp);
    };
    hashes.remove(position);
    let updated = serde_json::to_string(&hashes)
        .map_err(|error| UserError::PasswordHash(error.to_string()))?;
    sqlx::query("UPDATE users SET totp_recovery_hashes_json = ? WHERE id = ?")
        .bind(updated)
        .bind(user_id.to_string())
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(TotpVerification::Recovery)
}

/// Clear TOTP material for self or another account.
///
/// Spec: AUTH-9
///
/// # Errors
///
/// Returns permission, not-found, or SQLite failures.
pub async fn clear_totp(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
) -> Result<(), UserError> {
    let mut tx = pool.begin().await?;
    clear_totp_tx(&mut tx, actor, user_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Clear TOTP material inside an active transaction.
pub async fn clear_totp_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    user_id: UserId,
) -> Result<(), UserError> {
    ensure_self_or_admin(actor, user_id)?;
    let result = sqlx::query(
        "UPDATE users SET totp_secret_enc = NULL, totp_master_key_id = NULL,
            totp_enabled = 0, totp_last_step = NULL, totp_recovery_hashes_json = NULL
         WHERE id = ?",
    )
    .bind(user_id.to_string())
    .execute(&mut **tx)
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

fn timestamp_seconds(now_ms: i64) -> u64 {
    u64::try_from(now_ms / 1000).unwrap_or(0)
}

#[cfg(test)]
mod tests;
