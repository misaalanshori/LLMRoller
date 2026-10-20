#![forbid(unsafe_code)]
//! Canonical AEAD sealing and master-key loading.
//!
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#922-canonical-aead-associated-data-aad-specification

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use llmroller_core::seams::{EntropySource, ExposeSecret, SecretBytes, secret_bytes};
use secrecy::SecretBox;
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroize;

/// Environment variable containing the base64 master key directly.
pub const MASTER_KEY_ENV: &str = "LLMROLLER_MASTER_KEY";

/// Environment variable pointing at a permission-restricted base64 master-key file.
pub const MASTER_KEY_FILE_ENV: &str = "LLMROLLER_MASTER_KEY_FILE";

/// Environment variable naming the active master key.
pub const MASTER_KEY_ID_ENV: &str = "LLMROLLER_MASTER_KEY_ID";

const MASTER_KEY_LEN: usize = 32;
const INSTANCE_ID_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const ENVELOPE_VERSION: u8 = 0x01;
const AAD_PREFIX: &[u8] = b"llmroller.aad.v1\0";

const INFO_INSTANCE_SECRET: &str = "llmroller.v1.instance_secret";
const INFO_TOTP_SECRET: &str = "llmroller.v1.totp_secret";
const INFO_UPSTREAM_KEY: &str = "llmroller.v1.upstream_key";
const INFO_TRACE_RECORD: &str = "llmroller.v1.trace_record";

/// Successful or failed sealed-envelope operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CryptoError {
    /// The domain is not one of the six encrypted column families.
    #[error("unsupported sealed domain: {0}")]
    UnknownDomain(String),
    /// The envelope version byte is not `0x01`.
    #[error("unsupported sealed-envelope version: {0:#04x}")]
    UnsupportedVersion(u8),
    /// The envelope cannot contain a version, nonce, ciphertext, and tag.
    #[error("truncated sealed envelope")]
    TruncatedEnvelope,
    /// AEAD authentication or decryption failed.
    #[error("sealed-envelope authentication failed")]
    Authentication,
    /// AEAD encryption failed.
    #[error("sealed-envelope encryption failed")]
    Encryption,
}

/// Master-key loading failures, all of which terminate startup with exit code `3`.
#[derive(Debug, Error)]
pub enum MasterKeyError {
    /// Neither `LLMROLLER_MASTER_KEY_FILE` nor `LLMROLLER_MASTER_KEY` was provided.
    #[error("LLMROLLER_MASTER_KEY_FILE or LLMROLLER_MASTER_KEY is required")]
    Missing,
    /// The key file could not be read.
    #[error("cannot read master key file {path}: {source}")]
    Io {
        /// File that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The key is not valid standard base64.
    #[error("master key is not valid base64")]
    InvalidBase64,
    /// The decoded key is not exactly 32 bytes.
    #[error("master key must decode to exactly 32 bytes, got {0}")]
    InvalidLength(usize),
    /// The key file grants permissions to the group or other users.
    #[error("master key file must not have group or other permission bits set")]
    InsecurePermissions,
    /// `LLMROLLER_MASTER_KEY_ID` was not provided.
    #[error("LLMROLLER_MASTER_KEY_ID is required")]
    MissingKeyId,
    /// `LLMROLLER_MASTER_KEY_ID` was empty.
    #[error("LLMROLLER_MASTER_KEY_ID must not be empty")]
    EmptyKeyId,
    /// An environment variable is not valid UTF-8.
    #[error("master-key environment variable is not valid UTF-8")]
    NonUnicodeEnvironment,
}

impl MasterKeyError {
    /// Startup exit code required by SEC-2.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        3
    }
}

/// A 32-byte master key that zeroizes on drop.
pub struct MasterKey {
    key: SecretBytes,
}

impl MasterKey {
    /// Wrap exactly 32 raw bytes.
    #[must_use]
    pub fn from_bytes(mut bytes: [u8; MASTER_KEY_LEN]) -> Self {
        let key = secret_bytes(bytes.to_vec());
        bytes.zeroize();
        Self { key }
    }

    /// Decode a standard-base64 32-byte key.
    ///
    /// # Errors
    ///
    /// Returns [`MasterKeyError::InvalidBase64`] or [`MasterKeyError::InvalidLength`].
    pub fn from_base64(encoded: &str) -> Result<Self, MasterKeyError> {
        let decoded = STANDARD
            .decode(encoded.trim())
            .map_err(|_| MasterKeyError::InvalidBase64)?;
        let bytes: [u8; MASTER_KEY_LEN] = decoded
            .try_into()
            .map_err(|decoded: Vec<u8>| MasterKeyError::InvalidLength(decoded.len()))?;
        Ok(Self::from_bytes(bytes))
    }

    /// Borrow the raw key bytes for HKDF.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.key.expose_secret()
    }
}

impl Clone for MasterKey {
    fn clone(&self) -> Self {
        Self {
            key: secret_bytes(self.as_bytes().to_vec()),
        }
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MasterKey")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

/// Whether Unix mode bits keep the key private to its owner.
#[must_use]
pub const fn private_permissions_are_valid(mode: u32) -> bool {
    mode & 0o077 == 0
}

/// Load a master key from an optional file, falling back to an inline value.
///
/// The file always wins when both sources are present. The file contents and
/// inline value are trimmed so a single trailing newline from
/// `llmroller generate-master-key` is accepted.
///
/// # Errors
///
/// Returns a [`MasterKeyError`] for missing, unreadable, insecurely permissioned,
/// malformed, or incorrectly sized key material.
pub fn load_master_key(
    file: Option<&Path>,
    inline: Option<&str>,
) -> Result<MasterKey, MasterKeyError> {
    if let Some(path) = file {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let metadata = fs::metadata(path).map_err(|source| MasterKeyError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            if !private_permissions_are_valid(metadata.permissions().mode()) {
                return Err(MasterKeyError::InsecurePermissions);
            }
        }

        let encoded = fs::read_to_string(path).map_err(|source| MasterKeyError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        return MasterKey::from_base64(&encoded);
    }

    if let Some(encoded) = inline {
        return MasterKey::from_base64(encoded);
    }

    Err(MasterKeyError::Missing)
}

/// Load the master key from the process environment.
///
/// `LLMROLLER_MASTER_KEY_FILE` takes precedence over `LLMROLLER_MASTER_KEY`.
///
/// # Errors
///
/// Returns a [`MasterKeyError`] with exit code `3`.
pub fn load_master_key_from_env() -> Result<MasterKey, MasterKeyError> {
    if let Some(path) = env::var_os(MASTER_KEY_FILE_ENV) {
        return load_master_key(Some(Path::new(&path)), None);
    }

    let inline = match env::var(MASTER_KEY_ENV) {
        Ok(value) => Some(value),
        Err(env::VarError::NotPresent) => None,
        Err(env::VarError::NotUnicode(_)) => return Err(MasterKeyError::NonUnicodeEnvironment),
    };
    load_master_key(None, inline.as_deref())
}

/// Load the active master-key identifier from `LLMROLLER_MASTER_KEY_ID`.
///
/// # Errors
///
/// Returns a [`MasterKeyError`] with exit code `3` when the identifier is
/// missing, empty, or not valid UTF-8.
pub fn load_master_key_id_from_env() -> Result<String, MasterKeyError> {
    match env::var(MASTER_KEY_ID_ENV) {
        Ok(value) if value.is_empty() => Err(MasterKeyError::EmptyKeyId),
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Err(MasterKeyError::MissingKeyId),
        Err(env::VarError::NotUnicode(_)) => Err(MasterKeyError::NonUnicodeEnvironment),
    }
}

/// Canonical length-prefixed Associated Data for a sealed domain.
#[must_use]
pub fn canonical_aad(domain: &str, fields: &[&[u8]]) -> Vec<u8> {
    let capacity = AAD_PREFIX.len()
        + 4
        + domain.len()
        + fields.iter().map(|field| 4 + field.len()).sum::<usize>();
    let mut aad = Vec::with_capacity(capacity);
    aad.extend_from_slice(AAD_PREFIX);
    push_length_prefixed(&mut aad, domain.as_bytes());
    for field in fields {
        push_length_prefixed(&mut aad, field);
    }
    aad
}

fn push_length_prefixed(output: &mut Vec<u8>, bytes: &[u8]) {
    let length = u32::try_from(bytes.len()).expect("AAD component exceeds u32::MAX bytes");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
}

/// HKDF info label for one of the six encrypted column families.
///
/// # Errors
///
/// Returns [`CryptoError::UnknownDomain`] for any other domain string.
pub fn info_label_for_domain(domain: &str) -> Result<&'static str, CryptoError> {
    match domain {
        "instance_secret" | "usage_raw" => Ok(INFO_INSTANCE_SECRET),
        "totp_secret" => Ok(INFO_TOTP_SECRET),
        "upstream_key" => Ok(INFO_UPSTREAM_KEY),
        "trace_block" | "trace_record" => Ok(INFO_TRACE_RECORD),
        other => Err(CryptoError::UnknownDomain(other.to_owned())),
    }
}

type ExpandedSubkey = SecretBox<[u8; MASTER_KEY_LEN]>;

fn expand_subkey(
    master_key: &MasterKey,
    instance_id: &[u8; INSTANCE_ID_LEN],
    info: &str,
) -> ExpandedSubkey {
    let hkdf = Hkdf::<Sha256>::new(Some(instance_id), master_key.as_bytes());
    let mut key = [0_u8; MASTER_KEY_LEN];
    hkdf.expand(info.as_bytes(), &mut key)
        .expect("32-byte HKDF-SHA256 output is always valid");
    let expanded = SecretBox::new(Box::new(key));
    key.zeroize();
    expanded
}

#[cfg(test)]
pub(crate) fn derive_subkey(
    master_key: &MasterKey,
    instance_id: &[u8; INSTANCE_ID_LEN],
    domain: &str,
) -> Result<SecretBytes, CryptoError> {
    let label = info_label_for_domain(domain)?;
    let expanded = expand_subkey(master_key, instance_id, label);
    Ok(secret_bytes(expanded.expose_secret().to_vec()))
}

struct Subkeys {
    instance_secret: ExpandedSubkey,
    totp_secret: ExpandedSubkey,
    upstream_key: ExpandedSubkey,
    trace_record: ExpandedSubkey,
}

impl Subkeys {
    fn derive(master_key: &MasterKey, instance_id: &[u8; INSTANCE_ID_LEN]) -> Self {
        Self {
            instance_secret: expand_subkey(master_key, instance_id, INFO_INSTANCE_SECRET),
            totp_secret: expand_subkey(master_key, instance_id, INFO_TOTP_SECRET),
            upstream_key: expand_subkey(master_key, instance_id, INFO_UPSTREAM_KEY),
            trace_record: expand_subkey(master_key, instance_id, INFO_TRACE_RECORD),
        }
    }

    fn for_domain(&self, domain: &str) -> Result<&ExpandedSubkey, CryptoError> {
        match domain {
            "instance_secret" | "usage_raw" => Ok(&self.instance_secret),
            "totp_secret" => Ok(&self.totp_secret),
            "upstream_key" => Ok(&self.upstream_key),
            "trace_block" | "trace_record" => Ok(&self.trace_record),
            other => Err(CryptoError::UnknownDomain(other.to_owned())),
        }
    }
}

/// Seam for sealing and opening encrypted column-family blobs.
pub trait SecretSealer: Send + Sync {
    /// Seal a plaintext under a domain and ordered canonical fields.
    fn seal(&self, domain: &str, fields: &[&[u8]], plain: &[u8]) -> Vec<u8>;

    /// Open a sealed envelope and return zeroizing plaintext bytes.
    ///
    /// # Errors
    ///
    /// Returns a [`CryptoError`] for unsupported domains, malformed envelopes,
    /// or authentication failures.
    fn open(
        &self,
        domain: &str,
        fields: &[&[u8]],
        sealed: &[u8],
    ) -> Result<SecretBytes, CryptoError>;
}

/// Production XChaCha20-Poly1305 sealer with pre-derived HKDF-SHA256 subkeys.
pub struct XChaCha20Poly1305Sealer {
    subkeys: Subkeys,
    entropy: Arc<dyn EntropySource>,
}

impl XChaCha20Poly1305Sealer {
    /// Derive the four domain subkeys from the master key and 16-byte instance id.
    #[must_use]
    pub fn new(
        master_key: MasterKey,
        instance_id: [u8; INSTANCE_ID_LEN],
        entropy: Arc<dyn EntropySource>,
    ) -> Self {
        Self {
            subkeys: Subkeys::derive(&master_key, &instance_id),
            entropy,
        }
    }

    /// Seal with an explicitly supplied nonce.
    ///
    /// This is reserved for fixed test vectors and deterministic store tests;
    /// production callers use [`SecretSealer::seal`] so every envelope gets a
    /// fresh 24-byte nonce.
    pub(crate) fn seal_with_nonce(
        &self,
        domain: &str,
        fields: &[&[u8]],
        plain: &[u8],
        nonce: [u8; NONCE_LEN],
    ) -> Result<Vec<u8>, CryptoError> {
        let subkey = self.subkeys.for_domain(domain)?;
        let cipher = XChaCha20Poly1305::new_from_slice(subkey.expose_secret())
            .expect("derived subkeys are always 32 bytes");
        let aad = canonical_aad(domain, fields);
        let ciphertext = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::Encryption)?;

        let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        sealed.push(ENVELOPE_VERSION);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }

    /// Seal while propagating programmer or cryptographic errors.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::UnknownDomain`] or [`CryptoError::Encryption`].
    pub fn try_seal(
        &self,
        domain: &str,
        fields: &[&[u8]],
        plain: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let mut nonce = [0_u8; NONCE_LEN];
        self.entropy.fill_bytes(&mut nonce);
        self.seal_with_nonce(domain, fields, plain, nonce)
    }
}

impl fmt::Debug for XChaCha20Poly1305Sealer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XChaCha20Poly1305Sealer")
            .field("subkeys", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl SecretSealer for XChaCha20Poly1305Sealer {
    fn seal(&self, domain: &str, fields: &[&[u8]], plain: &[u8]) -> Vec<u8> {
        self.try_seal(domain, fields, plain)
            .unwrap_or_else(|error| panic!("failed to seal {domain}: {error}"))
    }

    fn open(
        &self,
        domain: &str,
        fields: &[&[u8]],
        sealed: &[u8],
    ) -> Result<SecretBytes, CryptoError> {
        let Some((&version, rest)) = sealed.split_first() else {
            return Err(CryptoError::TruncatedEnvelope);
        };
        if version != ENVELOPE_VERSION {
            return Err(CryptoError::UnsupportedVersion(version));
        }
        if rest.len() < NONCE_LEN + TAG_LEN {
            return Err(CryptoError::TruncatedEnvelope);
        }

        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
        let nonce = XNonce::try_from(nonce).expect("nonce length is checked above");
        let subkey = self.subkeys.for_domain(domain)?;
        let cipher = XChaCha20Poly1305::new_from_slice(subkey.expose_secret())
            .expect("derived subkeys are always 32 bytes");
        let aad = canonical_aad(domain, fields);
        let plain = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::Authentication)?;
        Ok(secret_bytes(plain))
    }
}

#[cfg(test)]
mod tests;
