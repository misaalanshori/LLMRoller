#![forbid(unsafe_code)]
//! Request and response header pipelines, HTTP smuggling validation, and token stripping.
//!
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-request-and-response-header-policies
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer

use http::HeaderMap;
use http::header::{self, HeaderName, HeaderValue};
use llmroller_core::rbac::OwnershipScope;
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

/// Internal and forwarding headers stripped from outbound requests.
const INTERNAL_STRIP_REQUEST_HEADERS: [&str; 10] = [
    "host",
    "content-length",
    "cookie",
    "set-cookie",
    "openai-organization",
    "openai-project",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
];

/// Session-affinity headers stripped when routing to Shared or Global tiers per SEC-16.
const SESSION_HEADERS_STRIP_ON_POOLED: [&str; 7] = [
    "x-session-affinity",
    "x-session-id",
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
    tier: OwnershipScope,
    auth_header: Option<(&str, &str)>,
    strip_headers: &[String],
    strip_header_tokens: &[String],
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

        // Rule 1: Strip hop-by-hop, org, proxy internal, and x-llmroller-*
        if HOP_BY_HOP_HEADERS.contains(&name_lower.as_str())
            || INTERNAL_STRIP_REQUEST_HEADERS.contains(&name_lower.as_str())
            || connection_named_headers.contains(&name_lower)
            || name_lower.starts_with("x-llmroller-")
            || name_lower == "x-real-ip"
        {
            continue;
        }

        // On Shared or Global tiers: strip recognized session headers per SEC-16
        if tier != OwnershipScope::Personal
            && SESSION_HEADERS_STRIP_ON_POOLED.contains(&name_lower.as_str())
        {
            continue;
        }

        // Rule 2: Strip provider deny-list and token filters
        if strip_headers.iter().any(|pattern| {
            if pattern.ends_with('*') {
                name_lower.starts_with(&pattern[..pattern.len() - 1])
            } else {
                name_lower == *pattern
            }
        }) {
            continue;
        }

        // Rule 3: Strip client authentication headers (will be replaced below)
        if name_lower == "authorization" || name_lower == "x-api-key" {
            continue;
        }

        // Strip individual comma-separated tokens from matching headers
        if !strip_header_tokens.is_empty()
            && let Ok(val_str) = val.to_str()
        {
            let mut remaining_tokens = Vec::new();
            for token in val_str.split(',') {
                let token_trimmed = token.trim();
                if !strip_header_tokens
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

/// Apply the 5-step response header pipeline R1 through R5 per Section 12.1.1.
#[must_use]
pub fn apply_response_header_pipeline(
    upstream_headers: &HeaderMap,
    tier: OwnershipScope,
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
        if tier != OwnershipScope::Personal
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

        // R3: Retain only allow-listed headers
        let is_base_allowed = RESPONSE_BASE_ALLOWLIST.contains(&name_lower.as_str());
        let is_config_allowed = response_header_allow
            .iter()
            .any(|allowed| allowed.to_ascii_lowercase() == name_lower);
        let is_rate_limit_allowed = (tier == OwnershipScope::Personal || is_admin)
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

    if tier != OwnershipScope::Personal {
        let scope_str = match tier {
            OwnershipScope::Global => "global",
            _ => "shared",
        };
        out.insert(
            HeaderName::from_static("x-llmroller-upstream-scope"),
            HeaderValue::from_static(scope_str),
        );
    }

    out
}

#[cfg(test)]
mod tests;
