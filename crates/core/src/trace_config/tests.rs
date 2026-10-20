//! Tests for TraceabilityConfig matcher, request-start evaluation, and auto-disable.
//!
//! Spec: Section 4.2, Section 6.10, Section 6.11, Section 9.3.3,
//! Requirements: TRC-1, TRC-7, AUD-1.

use super::*;
use crate::settings::{TraceabilityConfig, TraceabilityMatch};

#[doc = "Spec: TRC-1, TRC-7, AUD-1"]
#[test]
fn test_traceability_matcher_and_request_start_evaluation() {
    let mut config = TraceabilityConfig::default();

    // 1. Fresh instances have traceability_config.enabled == false
    assert!(!config.enabled);

    let user1 = UserId::generate();
    let user2 = UserId::generate();
    let key1 = ClientKeyId::generate();
    let key2 = ClientKeyId::generate();

    let now_ms = 1_700_000_000_000i64;

    // When disabled: never traces
    assert!(!should_trace_request(
        &config, &user1, &key1, "gpt-4o", now_ms
    ));

    // Enable with empty filter lists: matches ALL requests!
    config.enabled = true;
    assert!(should_trace_request(
        &config, &user1, &key1, "gpt-4o", now_ms
    ));
    assert!(should_trace_request(
        &config,
        &user2,
        &key2,
        "claude-sonnet-4.6",
        now_ms
    ));

    // 2. Filter list intersection matching:
    // Match only user1 AND gpt-4o
    config.match_filters = TraceabilityMatch {
        user_ids: vec![user1.to_string()],
        client_key_ids: vec![],
        models: vec!["gpt-4o".to_string()],
    };

    // user1 + gpt-4o -> matches!
    assert!(should_trace_request(
        &config, &user1, &key1, "gpt-4o", now_ms
    ));

    // user2 + gpt-4o -> does not match user list!
    assert!(!should_trace_request(
        &config, &user2, &key1, "gpt-4o", now_ms
    ));

    // user1 + claude -> does not match models list!
    assert!(!should_trace_request(
        &config,
        &user1,
        &key1,
        "claude-sonnet-4.6",
        now_ms
    ));

    // 3. auto_disable_at evaluation:
    config.auto_disable_at = Some(now_ms + 60_000); // 1 minute in the future
    assert!(should_trace_request(
        &config, &user1, &key1, "gpt-4o", now_ms
    ));
    assert!(!check_auto_disable_expired(&config, now_ms));

    // Once now >= auto_disable_at: does NOT trace and sweeper expires it
    assert!(!should_trace_request(
        &config,
        &user1,
        &key1,
        "gpt-4o",
        now_ms + 60_001
    ));
    assert!(check_auto_disable_expired(&config, now_ms + 60_001));
}
