#![forbid(unsafe_code)]
//! Instance metadata and sealed instance-secret bootstrap.
//!
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#91-per-connection-pragma-builder-and-complete-31-table-sqlite-ddl-schema
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#552-ordered-startup-sequence-llmroller-serve

use std::fmt;
use std::sync::Arc;

use llmroller_core::seams::{EntropySource, ExposeSecret, SecretBytes, secret_bytes};
use sqlx::SqlitePool;
use thiserror::Error;
use zeroize::Zeroize;

use crate::crypto::{CryptoError, MasterKey, SecretSealer, XChaCha20Poly1305Sealer};

const INSTANCE_ID_LEN: usize = 16;
const INSTANCE_SECRET_LEN: usize = 32;
const SESSION_SECRET: &str = "session_hmac_secret";
const AUDIT_SECRET: &str = "audit_hmac_key";

/// Bootstrap, startup-guard, and instance-secret failures.
#[derive(Debug, Error)]
pub enum InstanceError {
    /// SQLite could not read or write instance state.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// An offline master-key rotation has not completed.
    #[error("instance master-key rotation is in progress")]
    RotationInProgress,
    /// The process key identifier disagrees with the active database key.
    #[error("master key id {provided:?} does not match active master key id {active:?}")]
    MasterKeyIdMismatch {
        /// Identifier supplied by the process environment.
        provided: String,
        /// Identifier stored in `instance_meta`.
        active: String,
    },
    /// The database has a schema but no initialised instance row.
    ///
    /// Distinct from [`InstanceError::CorruptMetadata`] because the remedy differs: this is
    /// an uninitialised (or partially restored) database, not a damaged one.
    #[error("database is not an initialized LLMRoller instance (instance_meta is empty)")]
    NotInitialized,
    /// The metadata row or its 16-byte instance id is malformed.
    #[error("instance_meta is missing or corrupt")]
    CorruptMetadata,
    /// A required instance secret row is absent.
    #[error("required instance secret {name} is missing")]
    MissingSecret {
        /// Canonical secret name.
        name: &'static str,
    },
    /// A secret is not the required 256-bit value.
    #[error("instance secret {name} has {length} bytes, expected 32")]
    InvalidSecretLength {
        /// Canonical secret name.
        name: String,
        /// Decoded plaintext length.
        length: usize,
    },
    /// A sealed secret row was written under another master key.
    #[error("instance secret {name} uses master key {found:?}, expected {expected:?}")]
    SecretKeyIdMismatch {
        /// Canonical secret name.
        name: String,
        /// Master-key id stored on the secret row.
        found: String,
        /// Active master-key id from `instance_meta`.
        expected: String,
    },
    /// Sealed-envelope decoding or authentication failed.
    #[error(transparent)]
    Sealed(#[from] CryptoError),
}

impl InstanceError {
    /// CLI/startup exit code required by Sections 5.5.1 and 5.5.2.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Sqlx(_) => 13,
            Self::NotInitialized => 10,
            Self::RotationInProgress
            | Self::MasterKeyIdMismatch { .. }
            | Self::CorruptMetadata
            | Self::MissingSecret { .. }
            | Self::InvalidSecretLength { .. }
            | Self::SecretKeyIdMismatch { .. }
            | Self::Sealed(_) => 3,
        }
    }
}

/// Stable per-instance secrets and metadata required by the running server.
pub struct InstanceState {
    /// 16-byte `instance_meta.instance_id` used as the HKDF salt.
    pub instance_id: [u8; INSTANCE_ID_LEN],
    /// Master-key identifier currently marked active in the database.
    pub active_master_key_id: String,
    /// Persistent key for tenant-scoped session hashes.
    pub session_hmac_secret: SecretBytes,
    /// Persistent key for the `audit_log` HMAC chain.
    pub audit_hmac_key: SecretBytes,
}

impl fmt::Debug for InstanceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstanceState")
            .field("instance_id", &self.instance_id)
            .field("active_master_key_id", &self.active_master_key_id)
            .field("session_hmac_secret", &"[REDACTED]")
            .field("audit_hmac_key", &"[REDACTED]")
            .finish()
    }
}

/// Load existing instance state or atomically bootstrap a new instance.
///
/// The first call generates the 16-byte instance id and both random 256-bit
/// secrets, then seals each secret under the supplied master key and id. Later
/// calls refuse to run while a rotation is in progress, when the process key id
/// differs from `instance_meta.active_master_key_id`, or when either secret
/// fails authentication.
///
/// This function's *only* unique responsibility is the bootstrap branch: everything after
/// "a row exists" is delegated to [`verify_instance_state`], so the security-critical
/// verification (rotation guard, key-id agreement, AAD unseal, exact-32-byte check) exists
/// once. Two copies of a security check diverge -- that is what duplicated validation does,
/// not a hypothesis (`ISSUE-0183`).
///
/// # Errors
///
/// Returns an [`InstanceError`] whose [`InstanceError::exit_code`] is `3` for
/// startup-guard/cryptographic failures and `13` for SQLite failures.
pub async fn load_or_initialize(
    pool: &SqlitePool,
    key_id: &str,
    master_key: &MasterKey,
    entropy: Arc<dyn EntropySource>,
    now_ms: i64,
) -> Result<InstanceState, InstanceError> {
    // The metadata row is read here and *passed down*, so the initialised path issues the
    // same two queries it always did and the presence test is not paid for twice. This runs
    // at every startup, so the query count on this path is part of the contract.
    let meta = read_instance_meta(pool).await?;

    let Some(row) = meta else {
        // Without this guard, a database with orphaned secrets silently bootstraps a *second*
        // identity on top of the first and there is no longer any way to tell which rows
        // belong to which generation of key material.
        let existing_secrets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_secrets")
            .fetch_one(pool)
            .await?;
        if existing_secrets != 0 {
            return Err(InstanceError::CorruptMetadata);
        }
        return initialize(pool, key_id, master_key, entropy, now_ms).await;
    };

    verify_instance_state(pool, row, key_id, master_key, entropy).await
}

/// Load existing instance state **without ever creating it**.
///
/// This is the read-only sibling of [`load_or_initialize`]: identical verification (rotation
/// guard, master-key id agreement, per-secret AAD check, exact 32-byte length), but an absent
/// `instance_meta` row is [`InstanceError::NotInitialized`] rather than a bootstrap.
///
/// It exists because a *diagnostic* must not initialise the thing it is diagnosing. Before
/// this, `llmroller doctor` had to call [`load_or_initialize`] to obtain an
/// [`InstanceState`] (and with it the `audit_hmac_key`), so on a database missing
/// `instance_meta` the diagnostic silently *created* the row it was supposed to report as
/// missing (`ISSUE-0183`). Splitting the read from the bootstrap is what lets the
/// initialiser stay strict and the diagnostic stay read-only.
///
/// # Errors
///
/// Returns [`InstanceError::NotInitialized`] when `instance_meta` has no row, plus every
/// verification failure [`load_or_initialize`] can return.
pub async fn load_instance_state(
    pool: &SqlitePool,
    key_id: &str,
    master_key: &MasterKey,
    entropy: Arc<dyn EntropySource>,
) -> Result<InstanceState, InstanceError> {
    let Some(row) = read_instance_meta(pool).await? else {
        return Err(InstanceError::NotInitialized);
    };
    verify_instance_state(pool, row, key_id, master_key, entropy).await
}

/// The `instance_meta` singleton row, or `None` when the database is not initialised.
///
/// `(instance_id, active_master_key_id, rotation_state)`.
type MetaRow = (Vec<u8>, String, String);

async fn read_instance_meta(pool: &SqlitePool) -> Result<Option<MetaRow>, InstanceError> {
    let meta: Option<MetaRow> = sqlx::query_as(
        "SELECT instance_id, active_master_key_id, rotation_state \
         FROM instance_meta WHERE singleton = 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(meta)
}

/// The single security-critical verification body.
///
/// Rotation guard, master-key id agreement, per-secret AAD-bound unseal, exact 32-byte length,
/// and the two required-secret checks. Both public entry points funnel through here; the
/// *only* difference between them is what they do when [`read_instance_meta`] returns
/// `None`, decided by the caller before reaching this point.
async fn verify_instance_state(
    pool: &SqlitePool,
    row: MetaRow,
    key_id: &str,
    master_key: &MasterKey,
    entropy: Arc<dyn EntropySource>,
) -> Result<InstanceState, InstanceError> {
    let (instance_id_bytes, active_master_key_id, rotation_state) = row;

    if rotation_state != "idle" {
        return Err(InstanceError::RotationInProgress);
    }
    if active_master_key_id != key_id {
        return Err(InstanceError::MasterKeyIdMismatch {
            provided: key_id.to_owned(),
            active: active_master_key_id,
        });
    }

    let instance_id = decode_instance_id(&instance_id_bytes)?;
    let sealer = XChaCha20Poly1305Sealer::new(master_key.clone(), instance_id, entropy);
    let rows: Vec<(String, Vec<u8>, String)> =
        sqlx::query_as("SELECT name, sealed, master_key_id FROM instance_secrets ORDER BY name")
            .fetch_all(pool)
            .await?;

    let mut session_hmac_secret = None;
    let mut audit_hmac_key = None;
    for (name, sealed, row_key_id) in rows {
        if row_key_id != active_master_key_id {
            return Err(InstanceError::SecretKeyIdMismatch {
                name,
                found: row_key_id,
                expected: active_master_key_id,
            });
        }
        let plain = sealer.open(
            "instance_secret",
            &[active_master_key_id.as_bytes(), name.as_bytes()],
            &sealed,
        )?;
        let length = plain.expose_secret().len();
        let target = match name.as_str() {
            SESSION_SECRET => &mut session_hmac_secret,
            AUDIT_SECRET => &mut audit_hmac_key,
            _ => return Err(InstanceError::CorruptMetadata),
        };
        if length != INSTANCE_SECRET_LEN {
            return Err(InstanceError::InvalidSecretLength { name, length });
        }
        *target = Some(plain);
    }

    let session_hmac_secret = session_hmac_secret.ok_or(InstanceError::MissingSecret {
        name: SESSION_SECRET,
    })?;
    let audit_hmac_key =
        audit_hmac_key.ok_or(InstanceError::MissingSecret { name: AUDIT_SECRET })?;

    Ok(InstanceState {
        instance_id,
        active_master_key_id,
        session_hmac_secret,
        audit_hmac_key,
    })
}

async fn initialize(
    pool: &SqlitePool,
    key_id: &str,
    master_key: &MasterKey,
    entropy: Arc<dyn EntropySource>,
    now_ms: i64,
) -> Result<InstanceState, InstanceError> {
    let mut instance_id = [0_u8; INSTANCE_ID_LEN];
    let mut session_secret = [0_u8; INSTANCE_SECRET_LEN];
    let mut audit_secret = [0_u8; INSTANCE_SECRET_LEN];
    entropy.fill_bytes(&mut instance_id);
    entropy.fill_bytes(&mut session_secret);
    entropy.fill_bytes(&mut audit_secret);

    let sealer =
        XChaCha20Poly1305Sealer::new(master_key.clone(), instance_id, Arc::clone(&entropy));
    let session_hmac_secret = secret_bytes(session_secret.to_vec());
    let audit_hmac_key = secret_bytes(audit_secret.to_vec());
    session_secret.zeroize();
    audit_secret.zeroize();

    let sealed_session = sealer.try_seal(
        "instance_secret",
        &[key_id.as_bytes(), SESSION_SECRET.as_bytes()],
        session_hmac_secret.expose_secret(),
    )?;
    let sealed_audit = sealer.try_seal(
        "instance_secret",
        &[key_id.as_bytes(), AUDIT_SECRET.as_bytes()],
        audit_hmac_key.expose_secret(),
    )?;

    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO instance_meta \
         (singleton, instance_id, active_master_key_id, updated_at) \
         VALUES (1, ?, ?, ?)",
    )
    .bind(instance_id.as_slice())
    .bind(key_id)
    .bind(now_ms)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO instance_secrets (name, sealed, master_key_id, created_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(SESSION_SECRET)
    .bind(sealed_session)
    .bind(key_id)
    .bind(now_ms)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO instance_secrets (name, sealed, master_key_id, created_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(AUDIT_SECRET)
    .bind(sealed_audit)
    .bind(key_id)
    .bind(now_ms)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    Ok(InstanceState {
        instance_id,
        active_master_key_id: key_id.to_owned(),
        session_hmac_secret,
        audit_hmac_key,
    })
}

fn decode_instance_id(bytes: &[u8]) -> Result<[u8; INSTANCE_ID_LEN], InstanceError> {
    bytes.try_into().map_err(|_| InstanceError::CorruptMetadata)
}

#[cfg(test)]
mod tests;
