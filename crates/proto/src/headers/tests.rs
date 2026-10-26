//! Tests for request and response header pipelines, smuggling rejection, and strip_header_tokens.
//!
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-request-and-response-header-policies
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer

use http::{HeaderMap, HeaderValue, header};
use llmroller_core::provider::StripHeaderTokens;
use llmroller_core::rbac::PoolTier;

use crate::headers::{
    SmugglingError, UaPolicy, apply_request_header_pipeline, apply_response_header_pipeline,
    resolve_user_agent, sanitize_csv_cell, scrub_canary_tokens, ua_profile_by_name,
    validate_header_value_bytes, validate_request_smuggling,
};

/// Spec: API-2, API-3, SEC-8, SEC-15
/// Verify HTTP smuggling rejection:
/// 1. Both Content-Length and Transfer-Encoding -> 400
/// 2. Duplicate Host headers -> 400
/// 3. Control characters in header values -> 400
#[doc = "Spec: API-2, API-3, SEC-8, SEC-15"]
#[test]
fn test_http_smuggling_guards() {
    // 1. Both Content-Length and Transfer-Encoding
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("42"));
    headers.insert(
        header::TRANSFER_ENCODING,
        HeaderValue::from_static("chunked"),
    );
    let err = validate_request_smuggling(&headers).expect_err("CL + TE must be rejected");
    assert_eq!(err, SmugglingError::BothContentLengthAndTransferEncoding);

    // 2. Multiple Host headers
    let mut headers = HeaderMap::new();
    headers.append(header::HOST, HeaderValue::from_static("api.openai.com"));
    headers.append(header::HOST, HeaderValue::from_static("evil.com"));
    let err = validate_request_smuggling(&headers).expect_err("multiple Host must be rejected");
    assert_eq!(err, SmugglingError::MultipleHostHeaders);

    // 3. Control characters in header value
    let err = validate_header_value_bytes("x-custom", b"hello\x07world")
        .expect_err("control char must be rejected");
    assert!(matches!(err, SmugglingError::ControlCharacterInHeader(_)));

    // 4. Valid headers pass cleanly
    let mut valid = HeaderMap::new();
    valid.insert(header::HOST, HeaderValue::from_static("api.openai.com"));
    valid.insert(header::CONTENT_LENGTH, HeaderValue::from_static("100"));
    valid.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer token"),
    );
    assert!(validate_request_smuggling(&valid).is_ok());
}

/// Spec: API-2, API-3, SEC-8, SEC-15
/// Verify request header pipeline:
/// 1. On Own-tier keys, x-claude-code-session-id passes through verbatim.
/// 2. Multiple anthropic-beta headers preserve their per-name relative order.
/// 3. strip_header_tokens removes configured comma-separated tokens.
/// 4. Hop-by-hop and org headers are stripped.
/// 5. accept-encoding is forced to identity.
#[doc = "Spec: API-2, API-3, SEC-8, SEC-15"]
#[test]
fn test_request_header_pipeline() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("llmroller.internal"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("close, x-foo"));
    headers.insert(
        header::HeaderName::from_static("x-foo"),
        HeaderValue::from_static("hop_val"),
    );
    headers.insert(header::COOKIE, HeaderValue::from_static("session=123"));
    headers.insert(
        header::HeaderName::from_static("openai-organization"),
        HeaderValue::from_static("org-123"),
    );
    headers.insert(
        header::HeaderName::from_static("x-claude-code-session-id"),
        HeaderValue::from_static("sess-abc-456"),
    );
    headers.append(
        header::HeaderName::from_static("anthropic-beta"),
        HeaderValue::from_static("prompt-caching-2024-07-31"),
    );
    headers.append(
        header::HeaderName::from_static("anthropic-beta"),
        HeaderValue::from_static("unsupported-beta, computer-use-2024-10-22"),
    );

    let strip_tokens = StripHeaderTokens::from(vec!["unsupported-beta".to_string()]);
    let strip_headers = vec![];
    let auth_header = Some(("authorization", "Bearer sk-upstream-secret-real"));

    // Case A: Own/Personal tier
    let out_own = apply_request_header_pipeline(
        &headers,
        PoolTier::Own,
        auth_header,
        &strip_headers,
        &strip_tokens,
    )
    .expect("apply pipeline");

    // Host, Connection, x-foo, Cookie, openai-organization stripped
    assert!(out_own.get(header::HOST).is_none());
    assert!(out_own.get(header::CONNECTION).is_none());
    assert!(out_own.get("x-foo").is_none());
    assert!(out_own.get(header::COOKIE).is_none());
    assert!(out_own.get("openai-organization").is_none());

    // accept-encoding forced to identity
    assert_eq!(
        out_own
            .get(header::ACCEPT_ENCODING)
            .expect("accept-encoding header"),
        "identity"
    );

    // Authorization replaced with upstream credential
    assert_eq!(
        out_own.get(header::AUTHORIZATION).expect("auth header"),
        "Bearer sk-upstream-secret-real"
    );

    // On Own tier: x-claude-code-session-id preserved
    assert_eq!(
        out_own
            .get("x-claude-code-session-id")
            .expect("session header"),
        "sess-abc-456"
    );

    // anthropic-beta preserves relative order, with "unsupported-beta" stripped
    let betas: Vec<&str> = out_own
        .get_all("anthropic-beta")
        .iter()
        .map(|v| v.to_str().expect("str"))
        .collect();
    assert_eq!(betas.len(), 2);
    assert_eq!(betas[0], "prompt-caching-2024-07-31");
    assert_eq!(betas[1], "computer-use-2024-10-22");

    // Case B: Global tier: session headers stripped per SEC-16
    let out_global = apply_request_header_pipeline(
        &headers,
        PoolTier::Global,
        auth_header,
        &strip_headers,
        &strip_tokens,
    )
    .expect("apply pipeline global");

    assert!(out_global.get("x-claude-code-session-id").is_none());
}

/// Spec: API-3, TRN-1
/// Verify `strip_header_tokens` is scoped to the configured header: a token
/// listed for `anthropic-beta` must not be removed from `openai-beta`.
#[doc = "Spec: API-3, TRN-1"]
#[test]
fn test_strip_header_tokens_are_scoped_per_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::HeaderName::from_static("anthropic-beta"),
        HeaderValue::from_static("keep-alpha, drop-anthropic"),
    );
    headers.insert(
        header::HeaderName::from_static("openai-beta"),
        HeaderValue::from_static("keep-beta, drop-openai, drop-anthropic"),
    );
    headers.insert(
        header::HeaderName::from_static("x-other"),
        HeaderValue::from_static("drop-anthropic"),
    );

    let per_header: StripHeaderTokens = serde_json::from_str(
        r#"{"anthropic-beta":["drop-anthropic"],"openai-beta":["drop-openai"]}"#,
    )
    .expect("canonical per-header token map");

    let out = apply_request_header_pipeline(
        &headers,
        PoolTier::Own,
        Some(("authorization", "Bearer secret")),
        &[],
        &per_header,
    )
    .expect("apply pipeline");

    assert_eq!(
        out.get("anthropic-beta").expect("anthropic-beta"),
        "keep-alpha"
    );
    assert_eq!(
        out.get("openai-beta").expect("openai-beta"),
        "keep-beta, drop-anthropic",
        "tokens configured for another header must not be stripped"
    );
    assert_eq!(
        out.get("x-other").expect("unlisted header"),
        "drop-anthropic",
        "headers without configured tokens pass through unchanged"
    );
}

/// Spec: API-2, API-3, SEC-8, SEC-15
/// Verify response header pipeline R1 through R5:
/// 1. Hop-by-hop headers removed (R1)
/// 2. content-length, content-encoding, set-cookie removed unconditionally (R2)
/// 3. Allowlisted headers kept (R3)
/// 4. On Shared/Global tier for standard user: openai-org, x-ratelimit-*, cf-ray, server, via stripped (R4)
/// 5. x-llmroller-request-id and scope header added (R5)
#[doc = "Spec: API-2, API-3, SEC-8, SEC-15"]
#[test]
fn test_response_header_pipeline() {
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    resp_headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("1024"));
    resp_headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    resp_headers.insert(header::SET_COOKIE, HeaderValue::from_static("tracking=xyz"));
    resp_headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    resp_headers.insert(
        header::HeaderName::from_static("openai-organization"),
        HeaderValue::from_static("org-secret"),
    );
    resp_headers.insert(
        header::HeaderName::from_static("x-ratelimit-remaining-requests"),
        HeaderValue::from_static("500"),
    );
    resp_headers.insert(
        header::HeaderName::from_static("anthropic-ratelimit-tokens"),
        HeaderValue::from_static("10000"),
    );
    resp_headers.insert(
        header::HeaderName::from_static("cf-ray"),
        HeaderValue::from_static("12345-IAD"),
    );
    resp_headers.insert(header::SERVER, HeaderValue::from_static("cloudflare"));
    resp_headers.insert(
        header::HeaderName::from_static("via"),
        HeaderValue::from_static("1.1 vegur"),
    );
    resp_headers.insert(
        header::HeaderName::from_static("x-request-id"),
        HeaderValue::from_static("req-upstream-001"),
    );

    // Case A: Global tier, non-admin user
    let out_global = apply_response_header_pipeline(
        &resp_headers,
        PoolTier::Global,
        false, // not admin
        "01M5CE420F8JP4KE2B3JC4NX88",
        &[],
    );

    // R1: Connection removed
    assert!(out_global.get(header::CONNECTION).is_none());

    // R2: content-length, content-encoding, set-cookie removed
    assert!(out_global.get(header::CONTENT_LENGTH).is_none());
    assert!(out_global.get(header::CONTENT_ENCODING).is_none());
    assert!(out_global.get(header::SET_COOKIE).is_none());

    // R3: content-type and x-request-id retained
    assert_eq!(
        out_global
            .get(header::CONTENT_TYPE)
            .expect("content-type header"),
        "text/event-stream"
    );
    assert_eq!(
        out_global.get("x-request-id").expect("x-request-id header"),
        "req-upstream-001"
    );

    // R4: On Global tier: org, ratelimit, cf-ray, server, via stripped
    assert!(out_global.get("openai-organization").is_none());
    assert!(out_global.get("x-ratelimit-remaining-requests").is_none());
    assert!(out_global.get("anthropic-ratelimit-tokens").is_none());
    assert!(out_global.get("cf-ray").is_none());
    assert!(out_global.get(header::SERVER).is_none());
    assert!(out_global.get("via").is_none());

    // R5: x-llmroller-request-id and scope header added
    assert_eq!(
        out_global
            .get("x-llmroller-request-id")
            .expect("request-id header"),
        "01M5CE420F8JP4KE2B3JC4NX88"
    );
    assert_eq!(
        out_global
            .get("x-llmroller-upstream-scope")
            .expect("scope header"),
        "global"
    );

    // Case B: Own tier (personal key owner) -> rate limits preserved!
    let out_own = apply_response_header_pipeline(
        &resp_headers,
        PoolTier::Own,
        false,
        "01M5CE420F8JP4KE2B3JC4NX88",
        &[],
    );
    assert_eq!(
        out_own
            .get("x-ratelimit-remaining-requests")
            .expect("ratelimit reqs"),
        "500"
    );
    assert_eq!(
        out_own
            .get("anthropic-ratelimit-tokens")
            .expect("ratelimit tokens"),
        "10000"
    );
    assert!(out_own.get("x-llmroller-upstream-scope").is_none());
}

/// Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#122-user-agent-policy | Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer
/// Verify User-Agent policy modes: pass, profile, reject. LLMRoller has no User-Agent of its own (TRN-0).
#[doc = "Spec: API-3, TRN-0"]
#[test]
fn test_user_agent_policy_modes() {
    // 1. Pass mode forwards the client's UA, and sends none when the client sent none.
    assert_eq!(
        resolve_user_agent(UaPolicy::Pass, Some("custom-client/1.0"), None).expect("pass UA"),
        Some("custom-client/1.0".to_string())
    );
    assert_eq!(
        resolve_user_agent(UaPolicy::Pass, None, None).expect("no UA"),
        None
    );

    // 2. Profile mode: accepted client UAs pass through; everything else gets the profile's UA.
    let profile = ua_profile_by_name("opencode_go").expect("opencode_go profile");
    assert_eq!(
        resolve_user_agent(UaPolicy::Profile, Some("claude-cli/1.0.5"), Some(profile))
            .expect("coding agent"),
        Some("claude-cli/1.0.5".to_string())
    );
    assert_eq!(
        resolve_user_agent(UaPolicy::Profile, Some("curl/7.81.0"), Some(profile))
            .expect("curl replaced"),
        Some("opencode/1.2.15".to_string())
    );
    assert_eq!(
        resolve_user_agent(UaPolicy::Profile, None, Some(profile)).expect("missing replaced"),
        Some("opencode/1.2.15".to_string())
    );
    // Profile mode without a resolvable profile behaves like pass and never invents an identity.
    assert_eq!(
        resolve_user_agent(UaPolicy::Profile, Some("curl/7.81.0"), None).expect("no profile"),
        Some("curl/7.81.0".to_string())
    );
    assert_eq!(
        resolve_user_agent(UaPolicy::Profile, None, None).expect("no profile, no UA"),
        None
    );
    assert!(ua_profile_by_name("no_such_profile").is_none());

    // 3. Reject mode: Generic HTTP libraries or empty are rejected with error (maps to HTTP 400)
    assert!(resolve_user_agent(UaPolicy::Reject, Some("curl/7.81.0"), None).is_err());
    assert!(resolve_user_agent(UaPolicy::Reject, None, None).is_err());
    assert!(resolve_user_agent(UaPolicy::Reject, Some("   "), None).is_err());
    assert_eq!(
        resolve_user_agent(UaPolicy::Reject, Some("opencode/2.0"), None)
            .expect("valid agent in reject mode"),
        Some("opencode/2.0".to_string())
    );

    // 4. No built-in profile may carry an llmroller identifier (TRN-0).
    assert!(!profile.required.to_ascii_lowercase().contains("llmroller"));
}

/// Spec: SEC-6
/// Verify canary secret scrubbing and CSV formula injection prevention.
#[doc = "Spec: SEC-6"]
#[test]
fn test_canary_scrubber_and_csv_sanitizer() {
    let canary = "sk-canary-9988776655443322";
    let text = format!(
        "upstream returned error: invalid key {canary} and also token sk-live-1234567890abcdef"
    );
    let scrubbed = scrub_canary_tokens(&text, &[canary]);

    assert!(!scrubbed.contains(canary));
    assert!(!scrubbed.contains("sk-live-1234567890abcdef"));
    assert!(scrubbed.contains("[REDACTED_CANARY]"));
    assert!(scrubbed.contains("[REDACTED]"));

    // CSV formula injection protection
    assert_eq!(sanitize_csv_cell("=1+1"), "'=1+1");
    assert_eq!(
        sanitize_csv_cell("+cmd|' /C calc'!A0"),
        "'+cmd|' /C calc'!A0"
    );
    assert_eq!(sanitize_csv_cell("-5+2"), "'-5+2");
    assert_eq!(sanitize_csv_cell("@SUM(A1:A10)"), "'@SUM(A1:A10)");
    assert_eq!(sanitize_csv_cell("\tleading_tab"), "'\tleading_tab");
    assert_eq!(sanitize_csv_cell("normal_text"), "normal_text");
}

#[doc = "Spec: SEC-6, USG-9, TRN-3, SEC-16"]
#[test]
fn test_scrub_secret_prefixes_oc_sk_and_safe_diagnostic_headers() {
    use crate::headers::{extract_safe_diagnostic_headers, scrub_secret_prefixes};

    // F-010: scrubs oc_sk_, sk-, and llr_ even immediately after `=` or `:` or inside JSON
    let msg = "key=oc_sk_abcdef123456 and header:sk-or-v1-998877665544 and token=\"llr_abcd1234efgh5678\"";
    let scrubbed = scrub_secret_prefixes(msg);
    assert!(!scrubbed.contains("oc_sk_abcdef123456"));
    assert!(!scrubbed.contains("sk-or-v1-998877665544"));
    assert!(!scrubbed.contains("llr_abcd1234efgh5678"));
    assert_eq!(
        scrubbed,
        "key=[REDACTED] and header:[REDACTED] and token=\"[REDACTED]\""
    );

    // F-011: extract_safe_diagnostic_headers retains allow-listed headers and omits Authorization
    let mut headers = HeaderMap::new();
    headers.insert(header::USER_AGENT, HeaderValue::from_static("opencode/1.2"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer llr_secret12345678"),
    );
    let diag_json = extract_safe_diagnostic_headers(&headers);
    assert!(diag_json.contains("\"user-agent\":\"opencode/1.2\""));
    assert!(diag_json.contains("\"content-type\":\"application/json\""));
    assert!(!diag_json.contains("llr_secret12345678"));

    // F-017: mixed-case response_header_allow entries match upstream headers
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::HeaderName::from_static("x-custom-upstream"),
        HeaderValue::from_static("ok"),
    );
    let out = apply_response_header_pipeline(
        &resp_headers,
        PoolTier::Own,
        false,
        "01M5CE420F8JP4KE2B3JC4NX88",
        &["X-Custom-Upstream".to_string()],
    );
    assert_eq!(out.get("x-custom-upstream").expect("allowed header"), "ok");
}

#[doc = "Spec: SEC-15, F011, F023 - strips x-forwarded-*, cf-*, org headers, custom auth headers, and replaces accept-encoding with identity"]
#[test]
fn test_request_header_pipeline_strips_proxy_forwarding_org_and_accept_encoding() {
    let mut inbound = HeaderMap::new();
    inbound.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.7"));
    inbound.insert("x-forwarded-proto", HeaderValue::from_static("https"));
    inbound.insert("cf-connecting-ip", HeaderValue::from_static("198.51.100.7"));
    inbound.insert("true-client-ip", HeaderValue::from_static("198.51.100.7"));
    inbound.insert(
        "openai-organization",
        HeaderValue::from_static("org-client123"),
    );
    inbound.insert(
        "anthropic-organization-id",
        HeaderValue::from_static("org-anth123"),
    );
    inbound.insert("x-goog-api-key", HeaderValue::from_static("AIzaClientKey"));
    inbound.insert(
        "accept-encoding",
        HeaderValue::from_static("gzip, br, zstd"),
    );

    let out = apply_request_header_pipeline(
        &inbound,
        PoolTier::Global,
        Some(("authorization", "Bearer sk-upstream")),
        &[],
        &StripHeaderTokens::default(),
    )
    .expect("pipeline");

    assert!(!out.contains_key("x-forwarded-for"));
    assert!(!out.contains_key("x-forwarded-proto"));
    assert!(!out.contains_key("cf-connecting-ip"));
    assert!(!out.contains_key("true-client-ip"));
    assert!(!out.contains_key("openai-organization"));
    assert!(!out.contains_key("anthropic-organization-id"));
    assert!(!out.contains_key("x-goog-api-key"));
    assert_eq!(
        out.get("accept-encoding").and_then(|v| v.to_str().ok()),
        Some("identity")
    );
}

/// Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#73-transparency-and-compatibility-trn--com | Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer
/// TRN-0: an opencode-shaped request reaches the upstream with every client header intact (unknown
/// ones included), in order, and no proxy marker is added.
#[doc = "Spec: TRN-0, API-3"]
#[test]
fn test_trn0_real_client_headers_pass_through_without_proxy_markers() {
    let mut inbound = HeaderMap::new();
    inbound.append(
        "authorization",
        "Bearer llr_client".parse().expect("header value"),
    );
    inbound.append(
        "user-agent",
        "opencode/1.2.15".parse().expect("header value"),
    );
    inbound.append(
        "x-opencode-session",
        "ses_abc".parse().expect("header value"),
    );
    inbound.append("x-opencode-client", "cli".parse().expect("header value"));
    inbound.append(
        "x-some-future-agent-header",
        "v1".parse().expect("header value"),
    );
    inbound.append("anthropic-beta", "a".parse().expect("header value"));
    inbound.append("anthropic-beta", "b".parse().expect("header value"));

    let out = apply_request_header_pipeline(
        &inbound,
        PoolTier::Own,
        Some(("authorization", "Bearer sk-upstream")),
        &[],
        &StripHeaderTokens::default(),
    )
    .expect("pipeline");

    for name in [
        "user-agent",
        "x-opencode-session",
        "x-opencode-client",
        "x-some-future-agent-header",
    ] {
        assert_eq!(out.get(name), inbound.get(name), "{name} must pass through");
    }
    let betas: Vec<_> = out.get_all("anthropic-beta").iter().collect();
    assert_eq!(
        betas,
        inbound.get_all("anthropic-beta").iter().collect::<Vec<_>>()
    );
    assert_eq!(
        out.get("authorization").expect("auth header"),
        "Bearer sk-upstream"
    );
    for (name, value) in &out {
        let line = format!("{name}: {}", value.to_str().unwrap_or_default()).to_ascii_lowercase();
        assert!(!line.contains("llmroller"), "proxy marker leaked: {line}");
        assert!(!matches!(
            name.as_str(),
            "via" | "forwarded" | "x-forwarded-for"
        ));
    }
}
