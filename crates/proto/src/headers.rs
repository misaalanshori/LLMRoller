#![forbid(unsafe_code)]
//! Request and response header pipelines, HTTP smuggling validation, and token stripping.
//!
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-request-and-response-header-policies
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer

use http::HeaderMap;
use http::header::{self, HeaderName, HeaderValue};
use llmroller_core::provider::StripHeaderTokens;
use llmroller_core::rbac::PoolTier;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Smuggling and header format errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SmugglingError {
    #[error("request contains both Content-Length and Transfer-Encoding (SEC-15)")]
    BothContentLengthAndTransferEncoding,
    #[error("request contains multiple Host headers (SEC-15)")]
    MultipleHostHeaders,
    #[error("header {0:?} contains disallowed non-ASCII control characters (SEC-15)")]
    ControlCharacterInHeader(String),
}

/// Header pipeline processing errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HeaderPipelineError {
    #[error(transparent)]
    Smuggling(#[from] SmugglingError),
    #[error("invalid header value: {0}")]
    InvalidHeaderValue(String),
}

/// Standard hop-by-hop headers stripped unconditionally per RFC 9110 / Section 12.1.
const HOP_BY_HOP_HEADERS: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Internal and forwarding headers stripped from outbound requests (SEC-15, F011, F023).
const INTERNAL_STRIP_REQUEST_HEADERS: [&str; 21] = [
    "host",
    "content-length",
    "accept-encoding",
    "cookie",
    "set-cookie",
    "referer",
    "openai-organization",
    "openai-project",
    "anthropic-organization-id",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-port",
    "x-real-ip",
    "x-client-ip",
    "x-cluster-client-ip",
    "x-original-forwarded-for",
    "cf-connecting-ip",
    "cf-ipcountry",
    "true-client-ip",
];

/// Session-affinity headers stripped when routing to Shared or Global tiers per SEC-16.
const SESSION_HEADERS_STRIP_ON_POOLED: [&str; 11] = [
    "x-session-affinity",
    "x-session-id",
    "session-id",
    "session_id",
    "conversation-id",
    "x-bf-session-id",
    "x-opencode-session",
    "x-parent-session-id",
    "prompt-cache-key",
    "promptcachekey",
    "x-claude-code-session-id",
];

/// Base allowlisted headers preserved in response pipeline R3.
const RESPONSE_BASE_ALLOWLIST: [&str; 7] = [
    "content-type",
    "cache-control",
    "x-request-id",
    "request-id",
    "retry-after",
    "openai-processing-ms",
    "anthropic-version",
];

/// Validate a single header name and value for smuggling control characters per SEC-15.
pub fn validate_header_value_bytes(name: &str, val: &[u8]) -> Result<(), SmugglingError> {
    for &byte in val {
        if (byte < 0x20 && byte != 0x09) || byte == 0x7f {
            return Err(SmugglingError::ControlCharacterInHeader(name.to_string()));
        }
    }
    Ok(())
}

/// Validate that a request does not contain HTTP smuggling vectors per SEC-15.
pub fn validate_request_smuggling(headers: &HeaderMap) -> Result<(), SmugglingError> {
    // 1. Check Content-Length and Transfer-Encoding coexistence
    if headers.contains_key(header::CONTENT_LENGTH)
        && headers.contains_key(header::TRANSFER_ENCODING)
    {
        return Err(SmugglingError::BothContentLengthAndTransferEncoding);
    }

    // 2. Check multiple Host headers
    if headers.get_all(header::HOST).iter().count() > 1 {
        return Err(SmugglingError::MultipleHostHeaders);
    }

    // 3. Check for non-ASCII control characters in all header values
    for (name, val) in headers {
        validate_header_value_bytes(name.as_str(), val.as_bytes())?;
    }

    Ok(())
}

/// Apply the deterministic 4-rule request header pipeline to outbound requests per Section 12.1.
pub fn apply_request_header_pipeline(
    inbound: &HeaderMap,
    tier: PoolTier,
    auth_header: Option<(&str, &str)>,
    strip_headers: &[String],
    strip_header_tokens: &StripHeaderTokens,
) -> Result<HeaderMap, HeaderPipelineError> {
    validate_request_smuggling(inbound)?;

    // Parse any headers named inside Connection value
    let mut connection_named_headers = Vec::new();
    for conn_val in inbound.get_all(header::CONNECTION) {
        if let Ok(s) = conn_val.to_str() {
            for token in s.split(',') {
                let trimmed = token.trim().to_ascii_lowercase();
                if !trimmed.is_empty() {
                    connection_named_headers.push(trimmed);
                }
            }
        }
    }

    let mut out = HeaderMap::new();

    // Iterate through all inbound headers in order, preserving relative order of multi-value headers
    for (name, val) in inbound {
        let name_lower = name.as_str().to_ascii_lowercase();

        // Rule 1: Strip hop-by-hop, org, proxy internal, x-forwarded-*, cf-*, and x-llmroller-* (F011)
        if HOP_BY_HOP_HEADERS.contains(&name_lower.as_str())
            || INTERNAL_STRIP_REQUEST_HEADERS.contains(&name_lower.as_str())
            || connection_named_headers.contains(&name_lower)
            || name_lower.starts_with("x-llmroller-")
            || name_lower.starts_with("x-forwarded-")
            || name_lower.starts_with("cf-")
        {
            continue;
        }

        // On Shared or Global tiers: strip recognized session headers per SEC-16
        if tier != PoolTier::Own && SESSION_HEADERS_STRIP_ON_POOLED.contains(&name_lower.as_str()) {
            continue;
        }

        // Rule 2: Strip provider deny-list and token filters
        if strip_headers
            .iter()
            .any(|pattern| header_matches_glob(&name_lower, pattern))
        {
            continue;
        }

        // Rule 3: Strip client authentication headers (will be replaced below; F011)
        if matches!(
            name_lower.as_str(),
            "authorization" | "x-api-key" | "x-goog-api-key" | "api-key"
        ) {
            continue;
        }

        // Strip individual comma-separated tokens configured for this header
        if !strip_header_tokens.is_empty()
            && let Some(configured_tokens) = strip_header_tokens.for_header(&name_lower)
            && !configured_tokens.is_empty()
            && let Ok(val_str) = val.to_str()
        {
            let mut remaining_tokens = Vec::new();
            for token in val_str.split(',') {
                let token_trimmed = token.trim();
                if !configured_tokens
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(token_trimmed))
                {
                    remaining_tokens.push(token_trimmed);
                }
            }
            if remaining_tokens.is_empty() {
                continue; // Drop header if no tokens remain
            }
            let joined = remaining_tokens.join(", ");
            if let Ok(new_val) = HeaderValue::from_str(&joined) {
                out.append(name.clone(), new_val);
                continue;
            }
        }

        // Rule 4: Forward remaining headers preserving relative order
        out.append(name.clone(), val.clone());
    }

    // Force outbound accept-encoding to identity
    out.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity"),
    );

    // Rule 3: Replace credentials with decrypted upstream key
    if let Some((auth_name, auth_val)) = auth_header {
        let name = HeaderName::from_bytes(auth_name.as_bytes())
            .map_err(|e| HeaderPipelineError::InvalidHeaderValue(e.to_string()))?;
        let val = HeaderValue::from_str(auth_val)
            .map_err(|e| HeaderPipelineError::InvalidHeaderValue(e.to_string()))?;
        out.insert(name, val);
    }

    Ok(out)
}

/// Case-insensitive provider deny-list glob match (`strip_headers`).
///
/// Supports an optional single `*` anywhere in the pattern, e.g. `x-debug-*`.
fn header_matches_glob(name: &str, pattern: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    match pattern.split_once('*') {
        Some((prefix, suffix)) => {
            name.len() >= prefix.len() + suffix.len()
                && name.starts_with(prefix)
                && name.ends_with(suffix)
        }
        None => name == pattern,
    }
}

/// Apply the 5-step response header pipeline R1 through R5 per Section 12.1.1.
#[must_use]
pub fn apply_response_header_pipeline(
    upstream_headers: &HeaderMap,
    tier: PoolTier,
    is_admin: bool,
    request_id: &str,
    response_header_allow: &[String],
) -> HeaderMap {
    // Extract headers named in Connection
    let mut connection_named = Vec::new();
    for conn_val in upstream_headers.get_all(header::CONNECTION) {
        if let Ok(s) = conn_val.to_str() {
            for token in s.split(',') {
                let trimmed = token.trim().to_ascii_lowercase();
                if !trimmed.is_empty() {
                    connection_named.push(trimmed);
                }
            }
        }
    }

    let mut out = HeaderMap::new();

    for (name, val) in upstream_headers {
        let name_lower = name.as_str().to_ascii_lowercase();

        // R1: Remove all hop-by-hop headers and connection-named headers
        if HOP_BY_HOP_HEADERS.contains(&name_lower.as_str())
            || connection_named.contains(&name_lower)
        {
            continue;
        }

        // R2: Remove content-length, content-encoding, set-cookie, set-cookie2 unconditionally
        if name_lower == "content-length"
            || name_lower == "content-encoding"
            || name_lower == "set-cookie"
            || name_lower == "set-cookie2"
        {
            continue;
        }

        // R4: When chosen key is Shared or Global (and caller is not admin),
        // strip org, project, ratelimits, cf-ray, server, and via (overrides allowlist)
        if tier != PoolTier::Own
            && !is_admin
            && (name_lower == "openai-organization"
                || name_lower == "openai-project"
                || name_lower == "anthropic-organization-id"
                || name_lower.starts_with("x-ratelimit-")
                || name_lower.starts_with("anthropic-ratelimit-")
                || name_lower == "cf-ray"
                || name_lower == "server"
                || name_lower == "via")
        {
            continue;
        }

        // R3: Retain only allow-listed headers (case-insensitive per RFC 9110 / F-017)
        let is_base_allowed = RESPONSE_BASE_ALLOWLIST.contains(&name_lower.as_str());
        let is_config_allowed = response_header_allow
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(name.as_str()));
        let is_rate_limit_allowed = (tier == PoolTier::Own || is_admin)
            && (name_lower.starts_with("x-ratelimit-")
                || name_lower.starts_with("anthropic-ratelimit-"));

        if is_base_allowed || is_config_allowed || is_rate_limit_allowed {
            out.append(name.clone(), val.clone());
        }
    }

    // R5: Add x-llmroller-request-id and upstream scope header
    if let Ok(id_val) = HeaderValue::from_str(request_id) {
        out.insert(HeaderName::from_static("x-llmroller-request-id"), id_val);
    }

    if tier != PoolTier::Own {
        let scope_str = match tier {
            PoolTier::Global => "global",
            _ => "shared",
        };
        out.insert(
            HeaderName::from_static("x-llmroller-upstream-scope"),
            HeaderValue::from_static(scope_str),
        );
    }

    out
}

/// User-Agent policy modes per Section 12.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UaPolicy {
    /// Forward the client's User-Agent header unchanged, or send llmroller/1.0 if empty.
    #[default]
    Pass,
    /// Forward coding-agent User-Agent strings, and replace generic HTTP library strings with llmroller/1.0.
    Label,
    /// Return HTTP 400 if the client sends an empty or generic library User-Agent.
    Reject,
}

impl From<llmroller_core::provider::UaPolicySetting> for UaPolicy {
    fn from(value: llmroller_core::provider::UaPolicySetting) -> Self {
        match value {
            llmroller_core::provider::UaPolicySetting::Pass => Self::Pass,
            llmroller_core::provider::UaPolicySetting::Label => Self::Label,
            llmroller_core::provider::UaPolicySetting::Reject => Self::Reject,
        }
    }
}

/// Error returned when a User-Agent is rejected by policy.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid user-agent rejected by policy: {0:?}")]
pub struct UaPolicyRejectedError(pub String);

/// Resolve the effective outbound User-Agent header string per Section 12.2.
pub fn resolve_user_agent(
    policy: UaPolicy,
    incoming_ua: Option<&str>,
) -> Result<String, UaPolicyRejectedError> {
    const DEFAULT_UA: &str = "llmroller/1.0";
    match policy {
        UaPolicy::Pass => match incoming_ua {
            Some(ua) if !ua.trim().is_empty() => Ok(ua.to_string()),
            _ => Ok(DEFAULT_UA.to_string()),
        },
        UaPolicy::Label => match incoming_ua {
            Some(ua) if is_coding_agent_ua(ua) => Ok(ua.to_string()),
            _ => Ok(DEFAULT_UA.to_string()),
        },
        UaPolicy::Reject => match incoming_ua {
            Some(ua) if !ua.trim().is_empty() && !is_generic_library_ua(ua) => Ok(ua.to_string()),
            Some(ua) => Err(UaPolicyRejectedError(ua.to_string())),
            None => Err(UaPolicyRejectedError("<empty>".to_string())),
        },
    }
}

/// Extracts the original caller's client identifier from inbound headers (`x-client-name`, `x-title`,
/// or the primary product token from `user-agent`) for transparent proxy forwarding (`SRDD 12.1`).
///
/// Never returns `"llmroller"` or `"LLMRoller"` so the proxy remains transparent unless the caller
/// itself explicitly sent that identifier.
#[must_use]
pub fn extract_caller_client_name(inbound: &HeaderMap) -> Option<String> {
    for hdr in ["x-client-name", "x-title", "http-x-title", "x-app-name"] {
        if let Some(val) = inbound.get(hdr).and_then(|v| v.to_str().ok()) {
            let trimmed = val.trim();
            if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("llmroller") {
                return Some(trimmed.to_string());
            }
        }
    }
    if let Some(ua) = inbound.get(header::USER_AGENT).and_then(|v| v.to_str().ok()) {
        let trimmed = ua.trim();
        if !trimmed.is_empty() {
            let first_token = trimmed.split_whitespace().next().unwrap_or(trimmed);
            let product = first_token.split('/').next().unwrap_or(first_token).trim();
            if !product.is_empty() && !product.eq_ignore_ascii_case("llmroller") {
                return Some(product.to_string());
            }
        }
    }
    None
}

/// Determine whether a User-Agent identifies a recognized coding agent per Section 12.2.
#[must_use]
pub fn is_coding_agent_ua(ua: &str) -> bool {
    let lower = ua.to_ascii_lowercase();
    lower.starts_with("claude-cli/")
        || lower.starts_with("codex-cli/")
        || lower.starts_with("opencode/")
        || lower.starts_with("hermes/")
}

/// Determine whether a User-Agent identifies a generic HTTP library.
#[must_use]
pub fn is_generic_library_ua(ua: &str) -> bool {
    let lower = ua.to_ascii_lowercase();
    lower.starts_with("curl/")
        || lower.starts_with("python-requests/")
        || lower.starts_with("node-fetch/")
        || lower.starts_with("postman")
}

/// Scrub sensitive tokens and known canaries from logs and error strings per SEC-6.
#[must_use]
pub fn scrub_canary_tokens(input: &str, canaries: &[&str]) -> String {
    let mut result = input.to_string();
    for canary in canaries {
        if !canary.is_empty() {
            result = result.replace(canary, "[REDACTED_CANARY]");
        }
    }
    scrub_secret_prefixes(&result)
}

/// Allow-listed safe diagnostic request headers persisted on `usage_events.request_headers_json` (`USG-9`, `SEC-6`).
pub const SAFE_DIAGNOSTIC_HEADERS: [&str; 9] = [
    "anthropic-version",
    "content-length",
    "content-type",
    "openai-beta",
    "user-agent",
    "x-claude-code-session-id",
    "x-opencode-session",
    "x-session-id",
    "x-session-affinity",
];

/// Extract allow-listed diagnostic request headers as a compact JSON object (`<= 4 KiB`),
/// scrubbing any secret prefixes (`F-011`, `USG-9`, `SEC-6`).
#[must_use]
pub fn extract_safe_diagnostic_headers(headers: &HeaderMap) -> String {
    let mut map = std::collections::BTreeMap::new();
    for &name in &SAFE_DIAGNOSTIC_HEADERS {
        if let Some(val) = headers.get(name).and_then(|v| v.to_str().ok()) {
            let scrubbed = scrub_secret_prefixes(val);
            map.insert(name.to_string(), scrubbed);
        }
    }
    let json = serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string());
    if json.len() <= 4096 {
        json
    } else {
        "{}".to_string()
    }
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Scrub any tokens starting with `sk-`, `oc_sk_`, or `llr_` followed by `>= 8` token characters
/// anywhere in `input` (including after `=`, `:`, quotes, or inside JSON strings; `F-010`, `SEC-6`).
#[must_use]
pub fn scrub_secret_prefixes(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let prefix = if bytes[i..].starts_with(b"oc_sk_") {
            Some("oc_sk_")
        } else if bytes[i..].starts_with(b"sk-") {
            Some("sk-")
        } else if bytes[i..].starts_with(b"llr_") {
            Some("llr_")
        } else {
            None
        };
        if let Some(p) = prefix {
            let prev_is_token = i > 0 && is_token_byte(bytes[i - 1]);
            if !prev_is_token {
                let start_tail = i + p.len();
                let mut end_tail = start_tail;
                while end_tail < bytes.len() && is_token_byte(bytes[end_tail]) {
                    end_tail += 1;
                }
                if end_tail - start_tail >= 8 {
                    out.push_str("[REDACTED]");
                    i = end_tail;
                    continue;
                }
            }
        }
        let ch = input[i..].chars().next().expect("valid char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Sanitize CSV cells against formula injection per SEC-6.
/// Prefixes cells starting with '=', '+', '-', '@', '\t', '\r' with '\''.
#[must_use]
pub fn sanitize_csv_cell(cell: &str) -> String {
    if cell.starts_with('=')
        || cell.starts_with('+')
        || cell.starts_with('-')
        || cell.starts_with('@')
        || cell.starts_with('\t')
        || cell.starts_with('\r')
    {
        format!("'{cell}")
    } else {
        cell.to_string()
    }
}

#[cfg(test)]
mod tests;
