//! Content traceability mode configuration and request-start matcher.
//!
//! Spec: Section 4.2, Section 6.10, Section 6.11, Section 9.3.3,
//! Requirements: TRC-1, TRC-7, AUD-1.

use crate::ids::{ClientKeyId, UserId};
use crate::settings::TraceabilityConfig;

/// Evaluates whether an inbound request should be traced at request start (TRC-1).
///
/// Returns true iff:
/// 1. `config.enabled == true`
/// 2. `auto_disable_at` is None or `now_ms < auto_disable_at`
/// 3. Request matches all non-empty filter lists in `config.match_filters`:
///    - if `user_ids` is non-empty: caller user_id must be in `user_ids`
///    - if `client_key_ids` is non-empty: client_key_id must be in `client_key_ids`
///    - if `models` is non-empty: model canonical_id must be in `models`
pub fn should_trace_request(
    config: &TraceabilityConfig,
    user_id: &UserId,
    client_key_id: &ClientKeyId,
    canonical_model: &str,
    now_ms: i64,
) -> bool {
    if !config.enabled {
        return false;
    }

    if let Some(auto_disable) = config.auto_disable_at
        && now_ms >= auto_disable
    {
        return false;
    }

    let filters = &config.match_filters;

    if !filters.user_ids.is_empty() && !filters.user_ids.iter().any(|u| u == &user_id.to_string()) {
        return false;
    }

    if !filters.client_key_ids.is_empty()
        && !filters
            .client_key_ids
            .iter()
            .any(|k| k == &client_key_id.to_string())
    {
        return false;
    }

    if !filters.models.is_empty() && !filters.models.iter().any(|m| m == canonical_model) {
        return false;
    }

    true
}

/// Checks if auto_disable_at has arrived and returns true if sweeper should disable it (TRC-1).
pub fn check_auto_disable_expired(config: &TraceabilityConfig, now_ms: i64) -> bool {
    if config.enabled
        && let Some(auto_disable) = config.auto_disable_at
    {
        now_ms >= auto_disable
    } else {
        false
    }
}

#[cfg(test)]
pub mod tests;
