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
            ProviderKind::Custom | ProviderKind::OpenDesignAmr | ProviderKind::OpencodeGo => {
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

#[doc = "Spec: PRV-2, API-1, RTE-12"]
#[test]
fn test_openrouter_adapter_stateless_responses_and_messages_session_support() {
    let openrouter = get_adapter(ProviderKind::Openrouter);
    assert!(
        !openrouter.aux_endpoints().contains(&"responses_compact"),
        "OpenRouter /responses is stateless and does not expose /responses/compact"
    );
    assert!(openrouter.supports_session_body_on_endpoint(EndpointKind::Chat));
    assert!(openrouter.supports_session_body_on_endpoint(EndpointKind::Responses));
    assert!(openrouter.supports_session_body_on_endpoint(EndpointKind::Messages));

    let cc = get_adapter(ProviderKind::CommandCode);
    assert!(cc.supports_session_body_on_endpoint(EndpointKind::Chat));
    assert!(!cc.supports_session_body_on_endpoint(EndpointKind::Messages));
}

#[doc = "Spec: API-8, MDL-1, REL-5"]
#[test]
fn test_is_model_not_found_error_matching() {
    use crate::adapters::is_model_not_found_error;

    // Built-in phrasings
    assert!(is_model_not_found_error(
        404,
        b"{\"error\":{\"message\":\"No endpoints found for deepseek/deepseek-v4.1\"}}",
        None
    ));
    assert!(is_model_not_found_error(
        404,
        b"{\"error\":{\"code\":\"model_not_found\"}}",
        None
    ));
    assert!(is_model_not_found_error(
        404,
        b"{\"error\":\"The requested model does not exist.\"}",
        None
    ));
    assert!(is_model_not_found_error(
        404,
        b"model identifier is invalid",
        None
    ));
    assert!(is_model_not_found_error(
        404,
        b"Unknown model: claude-3-5-sonnet",
        None
    ));

    // Status other than 404 is not model_not_found
    assert!(!is_model_not_found_error(400, b"model_not_found", None));
    assert!(!is_model_not_found_error(404, b"page not found", None));

    // Custom classifier overrides
    let custom = ErrorClassifiers {
        model_not_found_substrings: vec!["custom_model_missing".to_string()],
        ..ErrorClassifiers::default()
    };
    assert!(is_model_not_found_error(
        404,
        b"custom_model_missing",
        Some(&custom)
    ));
}

#[doc = "Spec: RTE-8, API-6, F001, F017, F018 - initial SSE and embedded HTTP 200 error classification"]
#[test]
fn test_embedded_http_200_and_sse_error_frame_classification() {
    use crate::adapters::{is_overloaded_error, is_origin_timeout_status};

    // F001: Initial SSE frame with HTTP 200 carrying quota_exceeded or invalid_api_key
    let sse_quota = b"event: error\ndata: {\"error\":{\"code\":\"insufficient_quota\",\"message\":\"Out of credits\"}}\n\n";
    assert_eq!(
        classify_error_with_classifiers(200, sse_quota, None),
        ErrorClass::QuotaExhausted
    );

    let sse_auth = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}}\n\n";
    assert_eq!(
        classify_error_with_classifiers(200, sse_auth, None),
        ErrorClass::CredentialInvalid
    );

    let sse_bad_req = b"event: error\ndata: {\"error\":{\"type\":\"invalid_request_error\",\"message\":\"context_length_exceeded\"}}\n\n";
    assert_eq!(
        classify_error_with_classifiers(200, sse_bad_req, None),
        ErrorClass::RequestRejected
    );

    // F017: Anthropic two-line SSE overloaded_error and rate_limit_error frames
    let sse_overloaded = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
    assert_eq!(
        classify_error_with_classifiers(200, sse_overloaded, None),
        ErrorClass::Other(529)
    );
    assert!(is_overloaded_error(200, sse_overloaded));
    assert!(is_overloaded_error(529, b"Site is overloaded"));

    let sse_ratelimit = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Rate limited\"}}\n\n";
    assert_eq!(
        classify_error_with_classifiers(200, sse_ratelimit, None),
        ErrorClass::Other(429)
    );

    // F018: Origin timeout status detection (408, 504, 524)
    assert!(is_origin_timeout_status(408));
    assert!(is_origin_timeout_status(504));
    assert!(is_origin_timeout_status(524));
    assert!(!is_origin_timeout_status(502));
}
