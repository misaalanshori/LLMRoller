#![forbid(unsafe_code)]
//! Client key generation, grace rotation, and ordered request gates G1-G6.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#100-ordered-request-gates-g1-through-g7

use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;

use ipnet::IpNet;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::ids::{ClientKeyId, UserId};
use crate::seams::EntropySource;
use crate::snapshot::ConfigSnapshot;

const BASE62_ALPHABET: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Role of a client key hash entry in the in-memory lookup table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashRole {
    /// Active, primary key hash.
    Current,
    /// Predecessor key hash accepted during the rotation grace window.
    Previous,
}

/// In-memory projection of a client key for hot-path gate evaluation per Table 1.6.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientKeyRecord {
    pub id: ClientKeyId,
    pub owner_user_id: UserId,
    pub label: String,
    pub prefix: String,
    pub key_hash: [u8; 32],
    pub prev_key_hash: Option<[u8; 32]>,
    pub prev_key_expires_at: Option<i64>,
    pub routing_scope: String,
    pub allowed_models: Option<Vec<String>>,
    pub rate_limit_rpm: Option<i64>,
    pub budget_micro_usd_month: Option<i64>,
    pub allowed_cidrs: Option<Vec<String>>,
    pub constant_session_mode: String,
    pub service_account_id: Option<String>,
    pub disabled: bool,
    pub paused_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub expires_at: Option<i64>,
}

/// Gate failure classification matching SRDD Section 10.0 deterministic failure codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateFailure {
    /// G1: Token hash unknown or predecessor grace window expired.
    G1AuthenticationError,
    /// G2: Client key is disabled, revoked, or expired.
    G2KeyDisabledOrExpired,
    /// G2: Client key was reversibly paused (`paused_at IS NOT NULL`).
    G2ClientKeyPaused,
    /// G2: Parent Service Account is disabled (`CLK-6`).
    G2ServiceAccountDisabled,
    /// G3: Owner user account is disabled.
    G3OwnerDisabled,
    /// G4: Client IP address falls outside allowed CIDRs.
    G4CidrForbidden,
    /// G5: Request rate exceeds per-minute token bucket limit.
    G5RateLimitExceeded { retry_after_secs: u64 },
    /// G6: Requested model is not known to the instance catalog.
    G6ModelNotFound { model: String },
    /// G6: Requested model is not permitted by client key allowed_models.
    G6ModelForbidden { model: String },
}

/// Token bucket state tracking requests-per-minute per client key for Gate G5.
#[derive(Debug, Default)]
pub struct ClientKeyRpmLimiter {
    // client_key_id -> (available_tokens, last_refill_ms)
    buckets: Mutex<HashMap<ClientKeyId, (f64, i64)>>,
}

impl ClientKeyRpmLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to acquire one permit for the given client key at `now_ms`.
    ///
    /// # Errors
    ///
    /// Returns the number of seconds to wait (`Retry-After`) if the bucket is exhausted.
    pub fn try_acquire(&self, key_id: &ClientKeyId, rpm: i64, now_ms: i64) -> Result<(), u64> {
        let rpm_f64 = rpm as f64;
        let mut buckets = self.buckets.lock();
        let (tokens, last_refill) = buckets.entry(*key_id).or_insert_with(|| (rpm_f64, now_ms));

        // Refill tokens based on elapsed milliseconds
        let elapsed_ms = (now_ms - *last_refill).max(0) as f64;
        let refill = elapsed_ms * (rpm_f64 / 60_000.0);
        *tokens = (*tokens + refill).min(rpm_f64);
        *last_refill = now_ms;

        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            let needed = 1.0 - *tokens;
            let wait_ms = needed / (rpm_f64 / 60_000.0);
            let wait_secs = (wait_ms / 1000.0).ceil() as u64;
            Err(wait_secs.max(1))
        }
    }
}

/// Generate a new 36-character client key token (`llr_` + 32 base62 chars), its prefix, and binary SHA-256 hash.
#[must_use]
pub fn generate_client_key(entropy: &dyn EntropySource) -> (String, String, [u8; 32]) {
    let mut token = String::with_capacity(36);
    token.push_str("llr_");
    for _ in 0..32 {
        let idx = entropy.next_u64_bounded(62) as usize;
        token.push(BASE62_ALPHABET[idx] as char);
    }
    let prefix = token[..10].to_owned();
    let hash: [u8; 32] = sha2::Sha256::digest(token.as_bytes()).into();
    (token, prefix, hash)
}

/// Rotate an in-memory client key record in place with a grace period per CLK-2.
pub fn rotate_client_key_in_place(
    record: &mut ClientKeyRecord,
    entropy: &dyn EntropySource,
    grace_period_secs: i64,
    now_ms: i64,
) -> (String, [u8; 32]) {
    let (new_token, new_prefix, new_hash) = generate_client_key(entropy);
    let old_hash = record.key_hash;

    record.prev_key_hash = Some(old_hash);
    record.prev_key_expires_at = Some(now_ms + grace_period_secs * 1000);
    record.key_hash = new_hash;
    record.prefix = new_prefix;

    (new_token, new_hash)
}

/// Pure evaluation of ordered request gates G1 through G6 per SRDD Section 10.0.
///
/// # Errors
///
/// Returns the specific [`GateFailure`] variant of the first failing gate.
pub fn evaluate_request_gates(
    token_hash: &[u8; 32],
    client_ip: Option<IpAddr>,
    requested_model: Option<&str>,
    known_models: Option<&[String]>,
    snapshot: &ConfigSnapshot,
    rpm_limiter: Option<&ClientKeyRpmLimiter>,
    now_ms: i64,
) -> Result<(ClientKeyRecord, HashRole), GateFailure> {
    // Gate G1: Constant-time hash lookup in ConfigSnapshot (SEC-3)
    let Some((key_record, hash_role)) = snapshot.client_keys_by_hash.get(token_hash) else {
        return Err(GateFailure::G1AuthenticationError);
    };
    use subtle::ConstantTimeEq as _;
    let expected_hash = match hash_role {
        HashRole::Current => key_record.key_hash,
        HashRole::Previous => key_record
            .prev_key_hash
            .ok_or(GateFailure::G1AuthenticationError)?,
    };
    if !bool::from(expected_hash.ct_eq(token_hash)) {
        return Err(GateFailure::G1AuthenticationError);
    }

    if *hash_role == HashRole::Previous {
        if let Some(exp) = key_record.prev_key_expires_at {
            if now_ms >= exp {
                return Err(GateFailure::G1AuthenticationError);
            }
        } else {
            return Err(GateFailure::G1AuthenticationError);
        }
    }

    // Gate G2: reversible pause takes precedence over terminal key states so the
    // control plane can surface `client_key_paused` for a paused key (`CLK-7`).
    if key_record.paused_at.is_some() {
        return Err(GateFailure::G2ClientKeyPaused);
    }
    if key_record.disabled
        || key_record.revoked_at.is_some()
        || key_record.expires_at.is_some_and(|exp| now_ms >= exp)
    {
        return Err(GateFailure::G2KeyDisabledOrExpired);
    }

    // Gate G2: parent Service Account disabled (`CLK-6`).
    let service_account = key_record
        .service_account_id
        .as_ref()
        .and_then(|id| snapshot.service_accounts.get(id));
    if let Some(sa) = service_account
        && sa.disabled
    {
        return Err(GateFailure::G2ServiceAccountDisabled);
    }

    // Gate G3: Owner user account disabled
    let Some(owner) = snapshot.users.get(&key_record.owner_user_id) else {
        return Err(GateFailure::G3OwnerDisabled);
    };
    if owner.disabled {
        return Err(GateFailure::G3OwnerDisabled);
    }

    // Gate G4: Client IP within the inherited Service Account CIDRs and the
    // key-level CIDR list. Both lists must admit the caller when non-empty
    // (`CLK-6`, SRDD Section 10.0).
    let inherited_cidrs = service_account.and_then(|sa| sa.allowed_cidrs.as_ref());
    let key_cidrs = key_record.allowed_cidrs.as_ref();
    if [inherited_cidrs, key_cidrs]
        .into_iter()
        .flatten()
        .any(|cidrs| !cidrs.is_empty())
    {
        let Some(ip) = client_ip else {
            return Err(GateFailure::G4CidrForbidden);
        };
        for allowed_cidrs in [inherited_cidrs, key_cidrs].into_iter().flatten() {
            if allowed_cidrs.is_empty() {
                continue;
            }
            let matches_any = allowed_cidrs
                .iter()
                .any(|cidr_str| parse_ip_or_cidr(cidr_str).is_some_and(|net| net.contains(&ip)));
            if !matches_any {
                return Err(GateFailure::G4CidrForbidden);
            }
        }
    }

    // Gate G5: Rate limit RPM token bucket
    if let (Some(rpm), Some(limiter)) = (key_record.rate_limit_rpm, rpm_limiter)
        && let Err(retry_after_secs) = limiter.try_acquire(&key_record.id, rpm, now_ms)
    {
        return Err(GateFailure::G5RateLimitExceeded { retry_after_secs });
    }

    // Gate G6: Model resolution and allowed models restriction
    if let Some(model) = requested_model {
        if let Some(catalog) = known_models
            && !catalog.iter().any(|m| m == model)
        {
            return Err(GateFailure::G6ModelNotFound {
                model: model.to_owned(),
            });
        }
        if let Some(ref allowed) = key_record.allowed_models
            && !is_model_allowed_by_patterns(allowed, model, model)
        {
            return Err(GateFailure::G6ModelForbidden {
                model: model.to_owned(),
            });
        }
    }

    Ok((key_record.clone(), *hash_role))
}

/// Evaluate whether a requested or canonical model matches a client key's `allowed_models`
/// list, supporting exact matches, case-insensitive canonical matches, `"*"`, and prefix
/// wildcards such as `"gpt-4o*"` or `"anthropic/*"` (`F043`, `CLK-1`).
#[must_use]
pub fn is_model_allowed_by_patterns(
    allowed: &[String],
    requested_model: &str,
    canonical_id: &str,
) -> bool {
    let req_norm = crate::catalog::normalize_model_id(requested_model)
        .unwrap_or_else(|_| requested_model.to_ascii_lowercase());
    let can_norm = crate::catalog::normalize_model_id(canonical_id)
        .unwrap_or_else(|_| canonical_id.to_ascii_lowercase());
    for pattern in allowed {
        let pat = pattern.trim();
        if pat.is_empty() {
            continue;
        }
        if pat == "*" {
            return true;
        }
        if let Some(prefix) = pat.strip_suffix('*') {
            let p_low = prefix.to_ascii_lowercase();
            let p_stripped = p_low.rsplit_once('/').map_or(p_low.as_str(), |(_, s)| s);
            if requested_model.to_ascii_lowercase().starts_with(&p_low)
                || canonical_id.to_ascii_lowercase().starts_with(&p_low)
                || req_norm.starts_with(&p_low)
                || can_norm.starts_with(&p_low)
                || (!p_stripped.is_empty()
                    && (canonical_id.to_ascii_lowercase().starts_with(p_stripped)
                        || can_norm.starts_with(p_stripped)))
            {
                return true;
            }
        } else if pat.eq_ignore_ascii_case(requested_model)
            || pat.eq_ignore_ascii_case(canonical_id)
            || crate::catalog::normalize_model_id(pat)
                .is_ok_and(|p_norm| p_norm == req_norm || p_norm == can_norm)
        {
            return true;
        }
    }
    false
}

///Parse an `allowed_cidrs` entry as either a CIDR prefix (`192.0.2.0/24`) or a bare IPv4/IPv6 address (`203.0.113.5`, `2001:db8::1`; `F-050`, `CLK-6`).
#[must_use]
pub fn parse_ip_or_cidr(s: &str) -> Option<IpNet> {
    let trimmed = s.trim();
    IpNet::from_str(trimmed)
        .or_else(|_| trimmed.parse::<IpAddr>().map(IpNet::from))
        .ok()
}

#[cfg(test)]
mod tests;
