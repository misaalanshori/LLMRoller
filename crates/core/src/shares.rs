//! Consented provider and key sharing domain logic.
//!
//! Spec: Section 4.4, Section 4.6, Section 6.4, Section 8.2, Section 10.1,
//! Requirements: SHR-1, SHR-2, SHR-3, SHR-4, USR-2, USR-3, USR-4.

use serde::{Deserialize, Serialize};

use crate::ids::{ProviderId, ShareId, UpstreamKeyId, UserId};
use crate::provider::ProviderKind;

/// Configuration of a personal provider or key share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareConfig {
    pub share_id: ShareId,
    pub grantor_user_id: UserId,
    pub grantee_user_id: UserId,
    pub provider_id: ProviderId,
    pub upstream_key_id: Option<UpstreamKeyId>,
    pub allowed_models: Option<Vec<String>>,
    pub max_monthly_micro_usd: Option<i64>,
    pub accepted_config_fingerprint: Option<String>,
    pub admin_approved_at: Option<i64>,
    pub enabled: bool,
}

/// Reason why a share is currently ineligible for routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IneligibleReason {
    Disabled,
    GrantorDisabled,
    FingerprintMismatch,
    CustomProviderNeedsAdminApproval,
    ModelNotAllowed,
}

/// Resolves the effective share for a key per SRDD Section 10.1.
///
/// A key-level grant takes strict precedence over a provider-level grant. A grant
/// that is disabled, revoked, suspended by a fingerprint change, or awaiting admin
/// approval for a custom provider does not shadow a lower-precedence eligible grant.
/// Model restrictions are deliberately checked by `C2` after selection so an active
/// key-level grant remains authoritative even when it excludes the requested model.
pub fn effective_share<'a>(
    shares: &'a [ShareConfig],
    grantee_user_id: UserId,
    provider_id: ProviderId,
    key_id: UpstreamKeyId,
    provider_kind: ProviderKind,
    current_provider_fingerprint: &str,
    grantor_disabled: bool,
) -> Option<&'a ShareConfig> {
    let eligible = |share: &ShareConfig| {
        share.grantee_user_id == grantee_user_id
            && share.provider_id == provider_id
            && is_share_eligible(
                share,
                provider_kind,
                current_provider_fingerprint,
                grantor_disabled,
                None,
            )
            .is_ok()
    };

    shares
        .iter()
        .find(|share| share.upstream_key_id == Some(key_id) && eligible(share))
        .or_else(|| {
            shares
                .iter()
                .find(|share| share.upstream_key_id.is_none() && eligible(share))
        })
}

/// Evaluates whether a share is eligible for a given request.
pub fn is_share_eligible(
    share: &ShareConfig,
    provider_kind: ProviderKind,
    current_provider_fingerprint: &str,
    grantor_disabled: bool,
    model_canonical_id: Option<&str>,
) -> Result<(), IneligibleReason> {
    if !share.enabled {
        return Err(IneligibleReason::Disabled);
    }
    if grantor_disabled {
        return Err(IneligibleReason::GrantorDisabled);
    }
    match &share.accepted_config_fingerprint {
        Some(accepted) if accepted == current_provider_fingerprint => {}
        _ => return Err(IneligibleReason::FingerprintMismatch),
    }
    if provider_kind == ProviderKind::Custom && share.admin_approved_at.is_none() {
        return Err(IneligibleReason::CustomProviderNeedsAdminApproval);
    }
    if let (Some(allowed), Some(model)) = (&share.allowed_models, model_canonical_id)
        && !allowed.iter().any(|m| m == model)
    {
        return Err(IneligibleReason::ModelNotAllowed);
    }
    Ok(())
}

/// Error returned when two shares overlap for the same grantor, grantee, and target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareOverlapError;

/// Checks for overlapping shares between the same grantor and grantee.
pub fn check_share_overlap(
    existing: &[ShareConfig],
    new_grantor: &UserId,
    new_grantee: &UserId,
    new_provider: &ProviderId,
    new_key: Option<&UpstreamKeyId>,
) -> Result<(), ShareOverlapError> {
    for s in existing {
        if s.grantor_user_id == *new_grantor
            && s.grantee_user_id == *new_grantee
            && s.provider_id == *new_provider
            && s.enabled
        {
            if s.upstream_key_id.is_none() || new_key.is_none() {
                return Err(ShareOverlapError);
            }
            if s.upstream_key_id.as_ref() == new_key {
                return Err(ShareOverlapError);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub mod tests;
