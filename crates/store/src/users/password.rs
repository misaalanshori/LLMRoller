use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use llmroller_core::seams::{EntropySource, SecretString, secret_string};

use super::types::UserError;

/// Minimum accepted password length in Unicode scalar values.
pub const MIN_PASSWORD_CHARS: usize = 12;
/// Maximum accepted password length in bytes.
pub const MAX_PASSWORD_BYTES: usize = 1024;
const ARGON2_MEMORY_KIB: u32 = 19_456;
const ARGON2_TIME_COST: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;

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

/// How long a caller waits for an Argon2id permit before giving up.
///
/// Matches the control plane's login bound so both paths refuse at the same point. Kept
/// here because the permit is taken here.
pub const ARGON2_PERMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Acquire one Argon2id permit, bounded by `timeout`.
///
/// The permit is held across the blocking hash and dropped immediately after, so it
/// bounds concurrent Argon2id CPU work and never database work (`ISSUE-0180`,
/// `ISSUE-0181`). Returns `None` when the caller supplied no pool, meaning "hash
/// unbounded".
///
/// # Errors
///
/// Returns [`UserError::HashingBusy`] when no permit frees up inside `timeout`.
pub async fn acquire_hash_permit(
    semaphore: Option<&tokio::sync::Semaphore>,
    timeout: std::time::Duration,
) -> Result<Option<tokio::sync::SemaphorePermit<'_>>, UserError> {
    let Some(semaphore) = semaphore else {
        return Ok(None);
    };
    tokio::time::timeout(timeout, semaphore.acquire())
        .await
        .map_err(|_| UserError::HashingBusy)?
        .map(Some)
        .map_err(|_| UserError::HashingBusy)
}

/// Generate a cryptographically random temporary password shown once.
#[must_use]
pub fn generate_temporary_password(entropy: &dyn EntropySource) -> SecretString {
    let mut bytes = [0_u8; 24];
    entropy.fill_bytes(&mut bytes);
    secret_string(URL_SAFE_NO_PAD.encode(bytes))
}
