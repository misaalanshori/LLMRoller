use std::str::FromStr;

use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role};
use llmroller_core::seams::SecretString;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use thiserror::Error;

use super::password::MIN_PASSWORD_CHARS;
use crate::audit;
use crate::crypto::CryptoError;

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
    /// The shared Argon2id pool stayed exhausted past the permit bound.
    #[error("password hashing capacity is exhausted")]
    HashingBusy,
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

pub(super) fn role_from_str(value: &str) -> Result<Role, UserError> {
    match value {
        "admin" => Ok(Role::Admin),
        "user" => Ok(Role::User),
        _ => Err(UserError::Corrupt),
    }
}

pub(super) fn user_from_row(row: &SqliteRow) -> Result<User, UserError> {
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

pub(super) fn ensure_admin(actor: &ActorScope) -> Result<(), UserError> {
    if actor.is_admin() || actor.is_system() {
        Ok(())
    } else {
        Err(UserError::Forbidden)
    }
}

pub(super) fn ensure_self_or_admin(actor: &ActorScope, target: UserId) -> Result<(), UserError> {
    if actor.is_admin() || actor.is_system() || actor.user_id() == Some(target) {
        Ok(())
    } else {
        Err(UserError::Forbidden)
    }
}

pub(super) fn actor_user(actor: &ActorScope) -> Result<UserId, UserError> {
    actor.user_id().ok_or(UserError::Forbidden)
}
