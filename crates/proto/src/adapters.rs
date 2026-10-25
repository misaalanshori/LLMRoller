#![forbid(unsafe_code)]
//! Provider adapter implementations, endpoint dispatch, and 5-class error classifier.
//!
//! Spec: ./docs/srdd/2. SRDD - Research Findings.md#23-target-provider-endpoints-protocols-and-quotas
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#74-portability-maintainability-observability-and-privacy-por-mnt-obs-priv
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#1052-normative-timeout-and-failover-eligibility-tables
//! Phase: ./docs/bigbang/big-bang-phases-2.md#26-sub-phase-26-built-in-provideradapter-implementations-5-class-error-classifier-and-preset-toml-files

use llmroller_core::provider::{ProviderKind, SessionProfile};

pub use llmroller_core::provider::ErrorClassifiers;

use crate::body_edit::ChatStreamMode;
use crate::headers::UaPolicy;

/// Upstream endpoint kinds for inference, discovery, usage, and auxiliary capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EndpointKind {
    #[default]
    Chat,
    Responses,
    Messages,
    Models,
    Usage,
    CountTokens,
    ResponsesCompact,
    Embeddings,
}

/// Ordered five-class upstream error categorization per Section 10.5.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// 1. Out-of-quota / credit limit reached.
    QuotaExhausted,
    /// 2. Request rejected by upstream validation or content moderation.
    RequestRejected,
    /// 3. Model restricted by upstream plan tier.
    PlanRestricted,
    /// 4. Credential rejected as invalid / revoked.
    CredentialInvalid,
    /// 5. Unclassified 403 response.
    Unclassified403,
    /// Other HTTP error status.
    Other(u16),
}

const QUOTA_EXHAUSTED_SUBSTRINGS: [&str; 9] = [
    "insufficient_quota",
    "quota_exceeded",
    "out of credits",
    "exceeded your current quota",
    "billing_not_active",
    "monthly_limit_reached",
    "credit_limit",
    "window_limit_reached",
    "insufficient_credits",
];

const REQUEST_REJECTED_SUBSTRINGS: [&str; 2] = ["flagged", "moderation"];

const PLAN_RESTRICTED_SUBSTRINGS: [&str; 2] = ["upgrade_required", "not available on your plan"];

const CREDENTIAL_INVALID_SUBSTRINGS: [&str; 3] =
    ["invalid_api_key", "invalid x-api-key", "unauthorized"];

pub const MODEL_NOT_FOUND_SUBSTRINGS: [&str; 6] = [
    "model_not_found",
    "does not exist",
    "no endpoints found",
    "model identifier is invalid",
    "model not found",
    "unknown model",
];

const EMBEDDED_REQUEST_REJECTED_SUBSTRINGS: [&str; 6] = [
    "invalid_request_error",
    "context_length_exceeded",
    "content_policy_violation",
    "content_filter",
    "invalid_value",
    "bad_request",
];

/// Extract the JSON payload from a multi-line SSE error frame (`event: error\ndata: {...}`),
/// or return the raw UTF-8 string when the input is not SSE-framed (`F017`).
#[must_use]
pub fn extract_sse_data_or_raw(body_8k: &[u8]) -> String {
    let raw = String::from_utf8_lossy(body_8k);
    let mut data_lines = Vec::new();
    let mut saw_sse_prefix = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("event:") {
            saw_sse_prefix = true;
        } else if let Some(rest) = trimmed.strip_prefix("data:") {
            saw_sse_prefix = true;
            data_lines.push(rest.trim());
        }
    }
    if saw_sse_prefix && !data_lines.is_empty() {
        data_lines.join("\n")
    } else {
        raw.into_owned()
    }
}

/// Whether an upstream 404 (or embedded 200 error) response matches model-not-found patterns (Section 10.5, MDL-1, REL-5, F001).
#[must_use]
pub fn is_model_not_found_error(
    status: u16,
    body_8k: &[u8],
    classifiers: Option<&ErrorClassifiers>,
) -> bool {
    if !matches!(status, 200 | 404) {
        return false;
    }
    let extracted = extract_sse_data_or_raw(body_8k);
    let body_str = extracted.to_ascii_lowercase();
    if let Some(custom) = classifiers
        && ErrorClassifiers::matches(&custom.model_not_found_substrings, &body_str)
    {
        return true;
    }
    MODEL_NOT_FOUND_SUBSTRINGS
        .iter()
        .any(|sub| body_str.contains(sub))
}

/// Whether an upstream response represents an overloaded status (`HTTP 529` or `overloaded_error`, `F017`, `F018`).
#[must_use]
pub fn is_overloaded_error(status: u16, body_8k: &[u8]) -> bool {
    if status == 529 {
        return true;
    }
    if matches!(status, 200 | 429 | 500..=599) {
        let extracted = extract_sse_data_or_raw(body_8k).to_ascii_lowercase();
        if extracted.contains("overloaded_error")
            || extracted.contains("site is overloaded")
            || extracted.contains("server_is_overloaded")
        {
            return true;
        }
    }
    false
}

/// Whether an HTTP status represents an upstream origin timeout (`408`, `504`, or Cloudflare `524`; `F018`).
#[must_use]
pub const fn is_origin_timeout_status(status: u16) -> bool {
    matches!(status, 408 | 504 | 524)
}

/// Classify an upstream error response against the five ordered classes per Section 10.5.2.
#[must_use]
pub fn classify_error(status: u16, body_8k: &[u8]) -> ErrorClass {
    classify_error_with_classifiers(status, body_8k, None)
}

/// Classify an upstream error response honoring per-provider `error_classifiers` (PRV-3, F001, F017, F018).
#[must_use]
pub fn classify_error_with_classifiers(
    status: u16,
    body_8k: &[u8],
    classifiers: Option<&ErrorClassifiers>,
) -> ErrorClass {
    let extracted = extract_sse_data_or_raw(body_8k);
    let body_str = extracted.to_ascii_lowercase();

    if let Some(custom) = classifiers {
        if matches!(status, 200 | 402 | 403 | 429)
            && ErrorClassifiers::matches(&custom.quota_exhausted_substrings, &body_str)
        {
            return ErrorClass::QuotaExhausted;
        }
        if matches!(status, 200 | 400 | 403 | 422)
            && ErrorClassifiers::matches(&custom.request_rejected_substrings, &body_str)
        {
            return ErrorClass::RequestRejected;
        }
        if matches!(status, 200 | 403)
            && ErrorClassifiers::matches(&custom.plan_restricted_substrings, &body_str)
        {
            return ErrorClass::PlanRestricted;
        }
        if matches!(status, 200 | 401 | 403)
            && ErrorClassifiers::matches(&custom.credential_invalid_substrings, &body_str)
        {
            return ErrorClass::CredentialInvalid;
        }
    }

    // 1. QuotaExhausted: Status in {200, 402, 403, 429} and body matches quota_exhausted_substrings
    if matches!(status, 200 | 402 | 403 | 429)
        && QUOTA_EXHAUSTED_SUBSTRINGS
            .iter()
            .any(|sub| body_str.contains(sub))
    {
        return ErrorClass::QuotaExhausted;
    }

    // 2. RequestRejected: Status 200/403 matching "flagged", "moderation", or content permission error,
    // embedded 200 invalid_request_error, or status in {400, 404, 408, 409, 413, 422, 499}
    if matches!(status, 200 | 403)
        && (REQUEST_REJECTED_SUBSTRINGS
            .iter()
            .any(|sub| body_str.contains(sub))
            || (body_str.contains("permission_error") && body_str.contains("content")))
    {
        return ErrorClass::RequestRejected;
    }
    if status == 200
        && EMBEDDED_REQUEST_REJECTED_SUBSTRINGS
            .iter()
            .any(|sub| body_str.contains(sub))
    {
        return ErrorClass::RequestRejected;
    }
    if matches!(status, 400 | 404 | 408 | 409 | 413 | 422 | 499) {
        return ErrorClass::RequestRejected;
    }

    // 3. PlanRestricted: Status 200/403 matching plan_restricted_substrings
    if matches!(status, 200 | 403)
        && PLAN_RESTRICTED_SUBSTRINGS
            .iter()
            .any(|sub| body_str.contains(sub))
    {
        return ErrorClass::PlanRestricted;
    }

    // 4. CredentialInvalid: Status 401, or status 200/403 matching credential_invalid_substrings
    if status == 401
        || (matches!(status, 200 | 403)
            && (CREDENTIAL_INVALID_SUBSTRINGS
                .iter()
                .any(|sub| body_str.contains(sub))
                || body_str.contains("authentication_error")))
    {
        return ErrorClass::CredentialInvalid;
    }

    // Embedded HTTP 200 rate_limit_error maps to Other(429) so rate-limit cooldown applies (F001, F017)
    if status == 200
        && (body_str.contains("rate_limit_error") || body_str.contains("rate_limit_exceeded"))
    {
        return ErrorClass::Other(429);
    }

    // Embedded HTTP 200 overloaded_error maps to Other(529) (F017, F018)
    if status == 200 && is_overloaded_error(status, body_8k) {
        return ErrorClass::Other(529);
    }

    // 5. Unclassified403: Any other status 403
    if status == 403 {
        return ErrorClass::Unclassified403;
    }

    ErrorClass::Other(status)
}

/// Provider adapter interface isolating provider-specific endpoints and behaviors per MNT-2.
pub trait ProviderAdapter: Send + Sync {
    /// Associated provider kind.
    fn kind(&self) -> ProviderKind;
    /// Default canonical base URL for this provider.
    fn default_base_url(&self) -> &'static str;
    /// Construct the full URL for the requested endpoint.
    fn build_endpoint_url(&self, base_url: &str, endpoint: EndpointKind) -> String;
    /// Supported auxiliary capability endpoints.
    fn aux_endpoints(&self) -> &[&'static str];
    /// Default streaming usage reporting mode.
    fn default_chat_stream_mode(&self) -> ChatStreamMode;
    /// Default User-Agent policy.
    fn default_ua_policy(&self) -> UaPolicy;
    /// Default outbound session hint profile (SRDD Section 11.2).
    fn default_session_profile(&self) -> SessionProfile;
    /// Whether this provider supports injecting `session_profile.body_field` on `endpoint` (`F-014`).
    fn supports_session_body_on_endpoint(&self, endpoint: EndpointKind) -> bool {
        matches!(endpoint, EndpointKind::Chat | EndpointKind::Responses)
            || (self.kind() == ProviderKind::Openrouter && endpoint == EndpointKind::Messages)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OpencodeGoAdapter;

impl ProviderAdapter for OpencodeGoAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpencodeGo
    }

    fn default_base_url(&self) -> &'static str {
        "https://opencode.ai/zen/go/v1"
    }

    fn build_endpoint_url(&self, base_url: &str, endpoint: EndpointKind) -> String {
        let trimmed = base_url.trim_end_matches('/');
        match endpoint {
            EndpointKind::Chat => format!("{trimmed}/chat/completions"),
            EndpointKind::Responses => format!("{trimmed}/responses"),
            EndpointKind::Messages => format!("{trimmed}/messages"),
            EndpointKind::Models => format!("{trimmed}/models"),
            EndpointKind::Usage => format!("{trimmed}/usage"),
            EndpointKind::CountTokens => format!("{trimmed}/messages/count_tokens"),
            EndpointKind::ResponsesCompact => format!("{trimmed}/responses/compact"),
            EndpointKind::Embeddings => format!("{trimmed}/embeddings"),
        }
    }

    fn aux_endpoints(&self) -> &[&'static str] {
        &[]
    }

    fn default_chat_stream_mode(&self) -> ChatStreamMode {
        ChatStreamMode::Inject
    }

    fn default_ua_policy(&self) -> UaPolicy {
        UaPolicy::Label
    }

    fn default_session_profile(&self) -> SessionProfile {
        SessionProfile {
            headers: vec!["x-opencode-session".to_string()],
            body_field: None,
            ..SessionProfile::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CommandCodeAdapter;

impl ProviderAdapter for CommandCodeAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::CommandCode
    }

    fn default_base_url(&self) -> &'static str {
        "https://api.commandcode.ai/provider/v1"
    }

    fn build_endpoint_url(&self, base_url: &str, endpoint: EndpointKind) -> String {
        let trimmed = base_url.trim_end_matches('/');
        match endpoint {
            EndpointKind::Chat => format!("{trimmed}/chat/completions"),
            EndpointKind::Responses => format!("{trimmed}/responses"),
            EndpointKind::Messages => format!("{trimmed}/messages"),
            EndpointKind::Models => format!("{trimmed}/models"),
            EndpointKind::Usage => {
                let origin = trimmed.trim_end_matches("/provider/v1");
                format!("{origin}/alpha/billing/credits")
            }
            EndpointKind::CountTokens => format!("{trimmed}/messages/count_tokens"),
            EndpointKind::ResponsesCompact => format!("{trimmed}/responses/compact"),
            EndpointKind::Embeddings => format!("{trimmed}/embeddings"),
        }
    }

    fn aux_endpoints(&self) -> &[&'static str] {
        &["count_tokens", "responses_compact"]
    }

    fn default_chat_stream_mode(&self) -> ChatStreamMode {
        ChatStreamMode::Native
    }

    fn default_ua_policy(&self) -> UaPolicy {
        UaPolicy::Pass
    }

    fn default_session_profile(&self) -> SessionProfile {
        SessionProfile {
            headers: vec!["x-session-affinity".to_string(), "x-session-id".to_string()],
            body_field: Some("prompt_cache_key".to_string()),
            ..SessionProfile::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OpenDesignAmrAdapter;

impl ProviderAdapter for OpenDesignAmrAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenDesignAmr
    }

    fn default_base_url(&self) -> &'static str {
        "https://amr-link.open-design.ai/v1"
    }

    fn build_endpoint_url(&self, base_url: &str, endpoint: EndpointKind) -> String {
        let trimmed = base_url.trim_end_matches('/');
        match endpoint {
            EndpointKind::Chat => format!("{trimmed}/chat/completions"),
            EndpointKind::Responses => format!("{trimmed}/responses"),
            EndpointKind::Messages => format!("{trimmed}/messages"),
            EndpointKind::Models => format!("{trimmed}/models"),
            EndpointKind::Usage => format!("{trimmed}/usage"),
            EndpointKind::CountTokens => format!("{trimmed}/messages/count_tokens"),
            EndpointKind::ResponsesCompact => format!("{trimmed}/responses/compact"),
            EndpointKind::Embeddings => format!("{trimmed}/embeddings"),
        }
    }

    fn aux_endpoints(&self) -> &[&'static str] {
        &[]
    }

    fn default_chat_stream_mode(&self) -> ChatStreamMode {
        ChatStreamMode::Inject
    }

    fn default_ua_policy(&self) -> UaPolicy {
        UaPolicy::Pass
    }

    fn default_session_profile(&self) -> SessionProfile {
        SessionProfile {
            headers: vec!["x-session-affinity".to_string(), "x-session-id".to_string()],
            body_field: Some("prompt_cache_key".to_string()),
            ..SessionProfile::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OpenrouterAdapter;

impl ProviderAdapter for OpenrouterAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Openrouter
    }

    fn default_base_url(&self) -> &'static str {
        "https://openrouter.ai/api/v1"
    }

    fn build_endpoint_url(&self, base_url: &str, endpoint: EndpointKind) -> String {
        let trimmed = base_url.trim_end_matches('/');
        match endpoint {
            EndpointKind::Chat => format!("{trimmed}/chat/completions"),
            EndpointKind::Responses => format!("{trimmed}/responses"),
            EndpointKind::Messages => format!("{trimmed}/messages"),
            EndpointKind::Models => format!("{trimmed}/models"),
            EndpointKind::Usage => format!("{trimmed}/key"),
            EndpointKind::CountTokens => format!("{trimmed}/messages/count_tokens"),
            EndpointKind::ResponsesCompact => format!("{trimmed}/responses/compact"),
            EndpointKind::Embeddings => format!("{trimmed}/embeddings"),
        }
    }

    fn aux_endpoints(&self) -> &[&'static str] {
        &[]
    }

    fn default_chat_stream_mode(&self) -> ChatStreamMode {
        ChatStreamMode::Native
    }

    fn default_ua_policy(&self) -> UaPolicy {
        UaPolicy::Pass
    }

    fn default_session_profile(&self) -> SessionProfile {
        SessionProfile {
            headers: vec!["x-session-id".to_string()],
            body_field: Some("session_id".to_string()),
            ..SessionProfile::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CustomAdapter;

impl ProviderAdapter for CustomAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Custom
    }

    fn default_base_url(&self) -> &'static str {
        "https://api.openai.com/v1"
    }

    fn build_endpoint_url(&self, base_url: &str, endpoint: EndpointKind) -> String {
        let trimmed = base_url.trim_end_matches('/');
        match endpoint {
            EndpointKind::Chat => format!("{trimmed}/chat/completions"),
            EndpointKind::Responses => format!("{trimmed}/responses"),
            EndpointKind::Messages => format!("{trimmed}/messages"),
            EndpointKind::Models => format!("{trimmed}/models"),
            EndpointKind::Usage => format!("{trimmed}/usage"),
            EndpointKind::CountTokens => format!("{trimmed}/messages/count_tokens"),
            EndpointKind::ResponsesCompact => format!("{trimmed}/responses/compact"),
            EndpointKind::Embeddings => format!("{trimmed}/embeddings"),
        }
    }

    fn aux_endpoints(&self) -> &[&'static str] {
        &[]
    }

    fn default_chat_stream_mode(&self) -> ChatStreamMode {
        ChatStreamMode::Inject
    }

    fn default_ua_policy(&self) -> UaPolicy {
        UaPolicy::Pass
    }

    fn default_session_profile(&self) -> SessionProfile {
        SessionProfile {
            headers: vec!["x-session-id".to_string(), "x-session-affinity".to_string()],
            body_field: None,
            ..SessionProfile::default()
        }
    }
}

static OPENCODE_GO: OpencodeGoAdapter = OpencodeGoAdapter;
static COMMAND_CODE: CommandCodeAdapter = CommandCodeAdapter;
static OPEN_DESIGN_AMR: OpenDesignAmrAdapter = OpenDesignAmrAdapter;
static OPENROUTER: OpenrouterAdapter = OpenrouterAdapter;
static CUSTOM: CustomAdapter = CustomAdapter;

/// Retrieve the static [`ProviderAdapter`] implementation for the given provider kind.
#[must_use]
pub fn get_adapter(kind: ProviderKind) -> &'static dyn ProviderAdapter {
    match kind {
        ProviderKind::OpencodeGo => &OPENCODE_GO,
        ProviderKind::CommandCode => &COMMAND_CODE,
        ProviderKind::OpenDesignAmr => &OPEN_DESIGN_AMR,
        ProviderKind::Openrouter => &OPENROUTER,
        ProviderKind::Custom => &CUSTOM,
    }
}

#[cfg(test)]
mod tests;
