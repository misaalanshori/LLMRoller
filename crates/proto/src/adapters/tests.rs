//! Tests for ProviderAdapter implementations, 5-class error classifier, and presets.
//!
//! Spec: ./docs/srdd/2. SRDD - Research Findings.md#23-target-provider-endpoints-protocols-and-quotas
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#74-portability-maintainability-observability-and-privacy-por-mnt-obs-priv
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#1052-normative-timeout-and-failover-eligibility-tables
//! Phase: ./docs/bigbang/big-bang-phases-2.md#26-sub-phase-26-built-in-provideradapter-implementations-5-class-error-classifier-and-preset-toml-files

use crate::adapters::{
    EndpointKind, ErrorClass, ErrorClassifiers, classify_error, classify_error_with_classifiers,
    get_adapter,
};
use crate::body_edit::ChatStreamMode;
use crate::headers::UaPolicy;
use llmroller_core::provider::ProviderKind;

/// Spec: PRV-2, PRV-3, MNT-2, PRV-6, Partial: API-8
/// Verify adapter contract for URL construction, capabilities, presets, and Stage-1 key testing.
#[doc = "Spec: PRV-2, PRV-3, MNT-2, PRV-6, Partial: API-8"]
#[test]
fn test_provider_adapter_contracts() {
    for kind in [
        ProviderKind::OpencodeGo,
        ProviderKind::CommandCode,
        ProviderKind::OpenDesignAmr,
        ProviderKind::Openrouter,
        ProviderKind::Custom,
    ] {
        let adapter = get_adapter(kind);
        assert_eq!(adapter.kind(), kind);

        let base_url = adapter.default_base_url();
        assert!(base_url.starts_with("https://"));

        // 1. URL construction across endpoints
        let chat_url = adapter.build_endpoint_url(base_url, EndpointKind::Chat);
        assert!(chat_url.contains("/chat/completions"));

        let models_url = adapter.build_endpoint_url(base_url, EndpointKind::Models);
        assert!(models_url.contains("/models"));

        // Stage-1 /models key testing URL verification (PRV-6)
        assert_eq!(models_url, format!("{base_url}/models"));

        // 2. Capabilities and policies
        let aux = adapter.aux_endpoints();
        assert!(
            aux.contains(&"count_tokens") || aux.contains(&"responses_compact") || aux.is_empty()
        );

        let ua_policy = adapter.default_ua_policy();
        match kind {
            ProviderKind::OpencodeGo => assert_eq!(ua_policy, UaPolicy::Label),
            _ => assert_eq!(ua_policy, UaPolicy::Pass),
        }

        let stream_mode = adapter.default_chat_stream_mode();
        match kind {
            ProviderKind::Custom | ProviderKind::OpenDesignAmr => {
                assert_eq!(stream_mode, ChatStreamMode::Inject)
            }
            _ => assert_eq!(stream_mode, ChatStreamMode::Native),
        }

        // 3. Per-preset outbound session profile (Section 11.2)
        let profile = adapter.default_session_profile();
        match kind {
            ProviderKind::OpencodeGo => {
                assert_eq!(profile.headers, vec!["x-opencode-session".to_string()]);
                assert_eq!(profile.body_field, None);
            }
            ProviderKind::Openrouter => {
                assert_eq!(profile.headers, vec!["x-session-id".to_string()]);
                assert_eq!(profile.body_field.as_deref(), Some("session_id"));
            }
            ProviderKind::CommandCode | ProviderKind::OpenDesignAmr => {
                assert_eq!(
                    profile.headers,
                    vec!["x-session-affinity".to_string(), "x-session-id".to_string()]
                );
                assert_eq!(profile.body_field.as_deref(), Some("prompt_cache_key"));
            }
            ProviderKind::Custom => {
                assert_eq!(
                    profile.headers,
                    vec!["x-session-id".to_string(), "x-session-affinity".to_string()]
                );
                assert_eq!(
                    profile.body_field, None,
                    "custom providers must opt in to body affinity edits"
                );
            }
        }
    }
}

/// Spec: PRV-2, PRV-3, MNT-2, Partial: PRV-6, Partial: API-8
/// Verify ordered 5-class error classification over first 8 KiB of response bodies.
#[doc = "Spec: PRV-2, PRV-3, MNT-2, Partial: PRV-6, Partial: API-8"]
#[test]
fn test_five_class_error_classifier_ordering() {
    // 1. QuotaExhausted: 402, 403, 429 matching quota substrings
    let body_429 = b"{\"error\":{\"code\":\"insufficient_quota\",\"message\":\"You exceeded your current quota\"}}";
    assert_eq!(classify_error(429, body_429), ErrorClass::QuotaExhausted);

    let body_402 = b"out of credits, please top up";
    assert_eq!(classify_error(402, body_402), ErrorClass::QuotaExhausted);

    let body_403_quota = b"{\"error\":\"monthly_limit_reached\"}";
    assert_eq!(
        classify_error(403, body_403_quota),
        ErrorClass::QuotaExhausted
    );

    // 2. RequestRejected: 403 matching "flagged", "moderation", or 400/413/422/499
    let body_flagged =
        b"{\"error\":{\"message\":\"Your prompt was flagged by content moderation\"}}";
    assert_eq!(
        classify_error(403, body_flagged),
        ErrorClass::RequestRejected
    );

    let body_400 = b"{\"error\":\"invalid model name\"}";
    assert_eq!(classify_error(400, body_400), ErrorClass::RequestRejected);

    let body_422 = b"{\"error\":\"unprocessable entity\"}";
    assert_eq!(classify_error(422, body_422), ErrorClass::RequestRejected);

    // 3. PlanRestricted: 403 matching "upgrade_required"
    let body_upgrade = b"{\"error\":\"upgrade_required to use this model on Command Code\"}";
    assert_eq!(
        classify_error(403, body_upgrade),
        ErrorClass::PlanRestricted
    );

    // 4. CredentialInvalid: 401 or 403 matching "invalid_api_key"
    let body_401 = b"{\"error\":\"invalid_api_key provided\"}";
    assert_eq!(classify_error(401, body_401), ErrorClass::CredentialInvalid);

    let body_403_unauth = b"unauthorized access with this key";
    assert_eq!(
        classify_error(403, body_403_unauth),
        ErrorClass::CredentialInvalid
    );

    // 5. Unclassified403: any other 403
    let body_other_403 = b"{\"error\":\"forbidden action on account\"}";
    assert_eq!(
        classify_error(403, body_other_403),
        ErrorClass::Unclassified403
    );
}

#[doc = "Spec: PRV-3"]
#[test]
fn test_per_provider_error_classifiers_override_builtin_patterns() {
    let custom = ErrorClassifiers {
        quota_exhausted_substrings: vec!["TEAPOT_QUOTA_GONE".to_string()],
        request_rejected_substrings: vec!["custom_content_block".to_string()],
        plan_restricted_substrings: vec!["tier_too_low".to_string()],
        credential_invalid_substrings: vec!["revoked_by_tenant".to_string()],
        ..ErrorClassifiers::default()
    };

    assert_eq!(
        classify_error_with_classifiers(429, b"{\"error\":\"teapot_quota_gone\"}", Some(&custom)),
        ErrorClass::QuotaExhausted
    );
    assert_eq!(
        classify_error_with_classifiers(403, b"custom_content_block", Some(&custom)),
        ErrorClass::RequestRejected
    );
    assert_eq!(
        classify_error_with_classifiers(403, b"tier_too_low", Some(&custom)),
        ErrorClass::PlanRestricted
    );
    assert_eq!(
        classify_error_with_classifiers(403, b"revoked_by_tenant", Some(&custom)),
        ErrorClass::CredentialInvalid
    );

    // Built-in fallback remains intact when no custom pattern matches.
    assert_eq!(
        classify_error_with_classifiers(429, b"insufficient_credits", Some(&custom)),
        ErrorClass::QuotaExhausted
    );

    let parsed = ErrorClassifiers::from_provider_config(
        r#"{"error_classifiers":{"quota_exhausted_substrings":["window_limit_reached"]}}"#,
    );
    assert_eq!(
        parsed.quota_exhausted_substrings,
        vec!["window_limit_reached"]
    );
}
