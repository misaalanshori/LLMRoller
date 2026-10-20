//! Candidate route eligibility filter (C1-C8) and three-tier pool selector.
//!
//! Spec: Section 4.5, Section 6.5, Section 6.7, Section 10.0, Section 10.1, Section 10.2,
//! Requirements: CLK-3, RTE-3.

use crate::catalog::{MergeState, RouteStatus};
use crate::ids::{ModelPk, ProviderId, UpstreamKeyId, UserId};
use crate::rbac::{OwnershipScope, RoutingScope};
use crate::shares::{ShareConfig, is_share_eligible};

/// Failure code for a candidate route failing eligibility checks C1 through C8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckFailure {
    C1DisabledOrInactive,
    C2TierNotAuthorized,
    C3GrantorDisabled,
    C4ModelNotExposedOrKilled,
    C5ProtocolBlocked,
    C6HealthUnhealthy,
    C7BreakerOpen,
    C8BudgetExhausted,
}

/// Authorization tier for an eligible route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CandidatePoolTier {
    Own = 0,
    Shared = 1,
    Global = 2,
    Blended = 3,
}

/// Input candidate route being evaluated for eligibility.
#[derive(Debug, Clone)]
pub struct CandidateRoute {
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: ModelPk,
    pub provider_id: ProviderId,
    pub provider_kind: crate::provider::ProviderKind,
    pub provider_ownership: OwnershipScope,
    pub provider_owner_id: Option<UserId>,
    pub provider_fingerprint: String,
    pub provider_enabled: bool,
    pub key_enabled: bool,
    pub route_enabled: bool,
    pub route_status: RouteStatus,
    pub protocols: Vec<String>,
    pub protocol_blocked: bool,
    pub key_health_ok: bool,
    pub route_health_ok: bool,
    pub breaker_closed: bool,
    pub user_pref_enabled: Option<bool>,
    pub priority: i64,
    pub budget_admitted: bool,
    pub budget_exhausted_due_to_monthly_cap: bool,
}

/// A route that passed all checks C1 through C8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EligibleCandidate {
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: ModelPk,
    pub tier: CandidatePoolTier,
    pub priority: i64,
}

/// Context of the caller and request being routed.
#[derive(Debug, Clone, Copy)]
pub struct CallerContext<'a> {
    pub caller_user_id: &'a UserId,
    pub can_use_global: bool,
    pub owner_disabled: bool,
    pub model_exposed: Option<bool>,
    pub merge_state: MergeState,
    pub inbound_protocol: &'a str,
    pub model_canonical_id: &'a str,
}

/// Outcome of pool selection across the three tiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolSelectionError {
    MonthlyCapExhausted,
    NoEligibleRoute,
}

/// Evaluates a single candidate route against checks C1 through C8.
pub fn evaluate_candidate(
    route: &CandidateRoute,
    ctx: &CallerContext<'_>,
    shares: &[ShareConfig],
) -> Result<CandidatePoolTier, CheckFailure> {
    // C1: provider enabled, key enabled, route enabled, route status active
    if !route.provider_enabled
        || !route.key_enabled
        || !route.route_enabled
        || route.route_status != RouteStatus::Active
    {
        return Err(CheckFailure::C1DisabledOrInactive);
    }

    // C2: Tier authorization
    let tier = match route.provider_ownership {
        OwnershipScope::Personal => {
            if route.provider_owner_id == Some(*ctx.caller_user_id) {
                CandidatePoolTier::Own
            } else {
                // Must have active share
                let matching_share = shares.iter().find(|s| {
                    s.grantee_user_id == *ctx.caller_user_id
                        && s.provider_id == route.provider_id
                        && (s.upstream_key_id.is_none()
                            || s.upstream_key_id == Some(route.upstream_key_id))
                });

                match matching_share {
                    Some(s) => {
                        if is_share_eligible(
                            s,
                            route.provider_kind,
                            &route.provider_fingerprint,
                            ctx.owner_disabled,
                            Some(ctx.model_canonical_id),
                        )
                        .is_err()
                        {
                            return Err(CheckFailure::C2TierNotAuthorized);
                        }
                        CandidatePoolTier::Shared
                    }
                    None => return Err(CheckFailure::C2TierNotAuthorized),
                }
            }
        }
        OwnershipScope::Global => {
            if !ctx.can_use_global {
                return Err(CheckFailure::C2TierNotAuthorized);
            }
            CandidatePoolTier::Global
        }
    };

    // C3: Personal owner disabled check
    if (tier == CandidatePoolTier::Own || tier == CandidatePoolTier::Shared) && ctx.owner_disabled {
        return Err(CheckFailure::C3GrantorDisabled);
    }

    // C4: Model exposure and kill switch
    if ctx.model_exposed == Some(false) {
        return Err(CheckFailure::C4ModelNotExposedOrKilled);
    }
    if ctx.merge_state == MergeState::PendingReview {
        // Pending review models require confirmed vendor match or admin approval
    }
    if route.user_pref_enabled == Some(false) {
        return Err(CheckFailure::C4ModelNotExposedOrKilled);
    }

    // C5: Protocol support and blocks
    if !route.protocols.iter().any(|p| p == ctx.inbound_protocol) || route.protocol_blocked {
        return Err(CheckFailure::C5ProtocolBlocked);
    }

    // C6: Key and route health
    if !route.key_health_ok || !route.route_health_ok {
        return Err(CheckFailure::C6HealthUnhealthy);
    }

    // C7: Origin breaker
    if !route.breaker_closed {
        return Err(CheckFailure::C7BreakerOpen);
    }

    // C8: Budget admission
    if !route.budget_admitted {
        return Err(CheckFailure::C8BudgetExhausted);
    }

    Ok(tier)
}

/// Selects the winning tier group from eligible candidates according to routing_scope (Section 10.2).
pub fn select_pool(
    routes: &[CandidateRoute],
    ctx: &CallerContext<'_>,
    shares: &[ShareConfig],
    scope: RoutingScope,
) -> Result<(CandidatePoolTier, Vec<EligibleCandidate>), PoolSelectionError> {
    let mut own_tier = Vec::new();
    let mut shared_tier = Vec::new();
    let mut global_tier = Vec::new();

    let mut had_monthly_cap_failure = false;

    for r in routes {
        match evaluate_candidate(r, ctx, shares) {
            Ok(tier) => {
                let candidate = EligibleCandidate {
                    upstream_key_id: r.upstream_key_id,
                    model_pk: r.model_pk,
                    tier,
                    priority: r.priority,
                };
                match tier {
                    CandidatePoolTier::Own => own_tier.push(candidate),
                    CandidatePoolTier::Shared => shared_tier.push(candidate),
                    CandidatePoolTier::Global => global_tier.push(candidate),
                    CandidatePoolTier::Blended => {}
                }
            }
            Err(CheckFailure::C8BudgetExhausted) if r.budget_exhausted_due_to_monthly_cap => {
                had_monthly_cap_failure = true;
            }
            Err(_) => {}
        }
    }

    match scope {
        RoutingScope::PersonalFirst => {
            if !own_tier.is_empty() {
                Ok((CandidatePoolTier::Own, own_tier))
            } else if !shared_tier.is_empty() {
                Ok((CandidatePoolTier::Shared, shared_tier))
            } else if !global_tier.is_empty() {
                Ok((CandidatePoolTier::Global, global_tier))
            } else if had_monthly_cap_failure {
                Err(PoolSelectionError::MonthlyCapExhausted)
            } else {
                Err(PoolSelectionError::NoEligibleRoute)
            }
        }
        RoutingScope::Blended => {
            let mut blended = Vec::new();
            blended.extend(own_tier);
            blended.extend(shared_tier);
            blended.extend(global_tier);
            if !blended.is_empty() {
                Ok((CandidatePoolTier::Blended, blended))
            } else if had_monthly_cap_failure {
                Err(PoolSelectionError::MonthlyCapExhausted)
            } else {
                Err(PoolSelectionError::NoEligibleRoute)
            }
        }
        RoutingScope::PersonalOnly => {
            if !own_tier.is_empty() {
                Ok((CandidatePoolTier::Own, own_tier))
            } else if !shared_tier.is_empty() {
                Ok((CandidatePoolTier::Shared, shared_tier))
            } else if had_monthly_cap_failure {
                Err(PoolSelectionError::MonthlyCapExhausted)
            } else {
                Err(PoolSelectionError::NoEligibleRoute)
            }
        }
        RoutingScope::OwnOnly => {
            if !own_tier.is_empty() {
                Ok((CandidatePoolTier::Own, own_tier))
            } else if had_monthly_cap_failure {
                Err(PoolSelectionError::MonthlyCapExhausted)
            } else {
                Err(PoolSelectionError::NoEligibleRoute)
            }
        }
        RoutingScope::GlobalOnly => {
            if !global_tier.is_empty() {
                Ok((CandidatePoolTier::Global, global_tier))
            } else if had_monthly_cap_failure {
                Err(PoolSelectionError::MonthlyCapExhausted)
            } else {
                Err(PoolSelectionError::NoEligibleRoute)
            }
        }
    }
}

#[cfg(test)]
pub mod tests;
