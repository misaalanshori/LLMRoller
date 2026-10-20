//! Failover loop, routing constraints, response_links session inheritance, and terminal error selection.
//!
//! Spec: Section 10.4.1, Section 10.5.2, Section 10.5.3, Section 11.1.2,
//! Requirements: RTE-9, RTE-12, PRV-8.

use axum::http::StatusCode;
use bytes::Bytes;
use llmroller_core::ids::{ModelPk, UpstreamKeyId, UserId};

/// Routing constraint on upstream candidate selection per Section 10.4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingConstraint {
    Free,
    PinnedPreferred(UpstreamKeyId),
    HardKey(UpstreamKeyId),
}

/// Durable response link linking response_id to parent upstream_key and session_hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseLink {
    pub response_id: String,
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: ModelPk,
    pub session_hash: String,
    pub owner_user_id: UserId,
    pub created_at: i64,
}

/// Outcome of a single upstream attempt during the failover loop.
#[derive(Debug, Clone)]
pub enum TryOutcome {
    Success,
    RequestRejected { status: StatusCode, body: Bytes },
    Unclassified403 { body: Bytes },
    RateLimited { retry_after_secs: Option<u64> },
    QuotaExhausted { retry_after_secs: Option<u64> },
    PlanRestricted,
    BreakerOpen,
    WaitTimeout,
    HostFailure,
}

/// Selects the terminal error when all failover tries have failed (Section 10.5.3).
pub fn select_terminal_error(
    outcomes: &[TryOutcome],
) -> (StatusCode, &'static str, Option<u64>, Option<Bytes>) {
    if outcomes.is_empty() {
        return (StatusCode::BAD_GATEWAY, "upstream_unreachable", None, None);
    }

    // 1. If any try ended with RequestRejected or Unclassified403, return that response verbatim
    for o in outcomes {
        match o {
            TryOutcome::RequestRejected { status, body } => {
                return (*status, "request_rejected", None, Some(body.clone()));
            }
            TryOutcome::Unclassified403 { body } => {
                return (
                    StatusCode::FORBIDDEN,
                    "unclassified_403",
                    None,
                    Some(body.clone()),
                );
            }
            _ => {}
        }
    }

    // 2. Else if all tries ended in {http_429, QuotaExhausted, PlanRestricted, breaker_open}
    let all_rate_or_quota = outcomes.iter().all(|o| {
        matches!(
            o,
            TryOutcome::RateLimited { .. }
                | TryOutcome::QuotaExhausted { .. }
                | TryOutcome::PlanRestricted
                | TryOutcome::BreakerOpen
        )
    });
    if all_rate_or_quota {
        let mut min_retry: Option<u64> = None;
        for o in outcomes {
            let ra = match o {
                TryOutcome::RateLimited { retry_after_secs } => *retry_after_secs,
                TryOutcome::QuotaExhausted { retry_after_secs } => *retry_after_secs,
                _ => None,
            };
            if let Some(secs) = ra {
                min_retry = Some(min_retry.map_or(secs, |curr| curr.min(secs)));
            }
        }
        let final_retry = min_retry.map(|r| r.max(1)).or(Some(1));
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "upstream_rate_limited",
            final_retry,
            None,
        );
    }

    // 3. Else if any try ended in wait_timeout -> HTTP 504
    if outcomes
        .iter()
        .any(|o| matches!(o, TryOutcome::WaitTimeout))
    {
        return (StatusCode::GATEWAY_TIMEOUT, "upstream_timeout", None, None);
    }

    // 4. Otherwise HTTP 502 upstream_unreachable
    (StatusCode::BAD_GATEWAY, "upstream_unreachable", None, None)
}

/// Evaluates the hard-route decision table for previous_response_id per Section 10.4.1 (RTE-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardRouteDecision {
    RouteWithoutFailover(UpstreamKeyId),
    RateLimit429 { retry_after_secs: u64 },
    Forbidden403KeyUnavailable,
    Forbidden403ModelOrProtocolBlocked,
}

/// Summary of candidate check results for HardKey decision evaluation.
#[derive(Debug, Clone, Copy)]
pub struct HardRouteChecks {
    pub c1_c5_passed: bool,
    pub c6_c8_passed: bool,
    pub c6_failed: bool,
    pub c7_failed: bool,
    pub c8_failed: bool,
    pub c2_or_c3_failed: bool,
    pub c4_or_c5_failed: bool,
}

pub fn evaluate_hard_route_decision(
    checks: &HardRouteChecks,
    linked_key: UpstreamKeyId,
    retry_after: u64,
) -> HardRouteDecision {
    if checks.c4_or_c5_failed {
        return HardRouteDecision::Forbidden403ModelOrProtocolBlocked;
    }
    if checks.c2_or_c3_failed {
        return HardRouteDecision::Forbidden403KeyUnavailable;
    }
    if checks.c1_c5_passed && checks.c6_c8_passed {
        return HardRouteDecision::RouteWithoutFailover(linked_key);
    }
    if checks.c1_c5_passed && (checks.c6_failed || checks.c7_failed || checks.c8_failed) {
        return HardRouteDecision::RateLimit429 {
            retry_after_secs: retry_after.max(1),
        };
    }
    HardRouteDecision::Forbidden403KeyUnavailable
}
