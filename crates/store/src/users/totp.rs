use std::collections::BTreeSet;

use llmroller_core::ids::UserId;
use llmroller_core::rbac::ActorScope;
use llmroller_core::seams::{EntropySource, ExposeSecret, secret_string};
use sqlx::{SqlitePool, Transaction};
use totp_rs::{Algorithm as TotpAlgorithm, Builder as TotpBuilder, Totp};

use super::crud::{find_login_record_by_id, find_login_record_by_id_tx};
use super::types::{
    TotpEnrollment, TotpVerification, UserError, actor_user, ensure_admin,
};
use crate::crypto::{SecretSealer, XChaCha20Poly1305Sealer, sha256_hex};

/// Number of one-time TOTP recovery codes issued at enrollment.
pub const TOTP_RECOVERY_CODE_COUNT: usize = 10;
const TOTP_SECRET_BYTES: usize = 20;

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

    let code_hash = recovery_hash(code);
    let mut tx = pool.begin().await?;
    let current_json: String = sqlx::query_scalar(
        "SELECT totp_recovery_hashes_json FROM users WHERE id = ?",
    )
    .bind(user_id.to_string())
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(UserError::NotFound)?;
    let current_hashes: Vec<String> = serde_json::from_str(&current_json).unwrap_or_default();
    if !current_hashes.contains(&code_hash) {
        return Err(UserError::InvalidTotp);
    }
    let remaining = current_hashes
        .into_iter()
        .filter(|item| item != &code_hash)
        .collect::<Vec<_>>();
    let remaining_json = serde_json::to_string(&remaining)
        .map_err(|error| UserError::PasswordHash(error.to_string()))?;
    let res = sqlx::query(
        "UPDATE users SET totp_recovery_hashes_json = ? WHERE id = ? AND totp_recovery_hashes_json = ?",
    )
    .bind(&remaining_json)
    .bind(user_id.to_string())
    .bind(&current_json)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() != 1 {
        return Err(UserError::TotpReplay);
    }
    tx.commit().await?;
    Ok(TotpVerification::Recovery)
}

/// Administratively clear TOTP configuration for an account.
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

/// Administratively clear TOTP configuration inside an active transaction.
pub async fn clear_totp_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    user_id: UserId,
) -> Result<(), UserError> {
    ensure_admin(actor)?;
    let result = sqlx::query(
        "UPDATE users
         SET totp_secret_enc = NULL,
             totp_master_key_id = NULL,
             totp_enabled = 0,
             totp_last_step = NULL,
             totp_recovery_hashes_json = '[]'
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

fn timestamp_seconds(now_ms: i64) -> u64 {
    u64::try_from(now_ms / 1000).unwrap_or(0)
}
