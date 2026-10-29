#![forbid(unsafe_code)]
//! Request and response header pipelines, HTTP smuggling validation, and token stripping.
//!
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-request-and-response-header-policies
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer

use http::HeaderMap;
use http::header::{self, HeaderName, HeaderValue};
use llmroller_core::provider::{ClientMode, StripHeaderTokens};
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
///
/// `cf-ipcountry`, `x-geo-country`, `x-geo-city` and `x-client-location` are here rather than
/// behind a per-provider switch (`#158`). A client-supplied **privacy** header has no
/// legitimate upstream purpose: it tells the provider something about the caller that the
/// caller chose to assert, and the provider may then treat that assertion as trustworthy.
/// These are our metadata, so they are stripped unconditionally, and the `cf-` prefix rule
/// below would catch `cf-ipcountry` even if it were removed from this list.
const INTERNAL_STRIP_REQUEST_HEADERS: [&str; 23] = [
    "host",
    "content-length",
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
    "x-geo-country",
    "x-geo-city",
    "x-client-location",
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
    _tier: PoolTier,
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

/// Comma-separated token headers that required-client mode merges instead of replacing, so the
/// provider's mandatory flags are present while the caller's own flags survive.
const MERGED_TOKEN_HEADERS: [&str; 1] = ["anthropic-beta"];

/// Apply one provider-configured header (`add_headers` or a preset's synthesized header) per the
/// provider's [`ClientMode`] (`TRN-0`, Sections 12.1.2 and 23.3).
///
/// `Transparent` only fills a header the caller did not send. `Required` replaces the caller's
/// values, except [`MERGED_TOKEN_HEADERS`], whose tokens are merged without duplicates.
pub fn apply_provider_header(
    out: &mut HeaderMap,
    name: HeaderName,
    value: HeaderValue,
    mode: ClientMode,
) {
    match mode {
        ClientMode::Transparent => {
            if !out.contains_key(&name) {
                out.insert(name, value);
            }
        }
        ClientMode::Required => {
            if MERGED_TOKEN_HEADERS.contains(&name.as_str())
                && out.contains_key(&name)
                && let Some(merged) = merge_header_tokens(out, &name, &value)
            {
                out.insert(name, merged);
            } else {
                out.insert(name, value);
            }
        }
    }
}

/// Union of the caller's and the provider's comma-separated tokens (case-insensitive, first
/// spelling and order kept). `None` when a value is not valid text, in which case the caller falls
/// back to replacing.
fn merge_header_tokens(
    out: &HeaderMap,
    name: &HeaderName,
    required: &HeaderValue,
) -> Option<HeaderValue> {
    let mut tokens: Vec<String> = Vec::new();
    let values = out.get_all(name).iter().chain(std::iter::once(required));
    for value in values {
        for token in value.to_str().ok()?.split(',') {
            let token = token.trim();
            if !token.is_empty() && !tokens.iter().any(|t| t.eq_ignore_ascii_case(token)) {
                tokens.push(token.to_string());
            }
        }
    }
    HeaderValue::from_str(&tokens.join(",")).ok()
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

/// User-Agent policy modes per Section 12.2. LLMRoller never invents a User-Agent of its own (TRN-0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UaPolicy {
    /// Forward the client's User-Agent header unchanged, and send none when the client sent none.
    #[default]
    Pass,
    /// The provider mandates a client identity (`UaProfile`): accepted client User-Agents pass
    /// through, any other or missing User-Agent is replaced with the profile's required value.
    Profile,
    /// Return HTTP 400 if the client sends an empty or generic library User-Agent.
    Reject,
}

impl From<llmroller_core::provider::UaPolicySetting> for UaPolicy {
    fn from(value: llmroller_core::provider::UaPolicySetting) -> Self {
        match value {
            llmroller_core::provider::UaPolicySetting::Pass => Self::Pass,
            llmroller_core::provider::UaPolicySetting::Profile => Self::Profile,
            llmroller_core::provider::UaPolicySetting::Reject => Self::Reject,
        }
    }
}

/// Required-client profile for providers that only accept one client identity (Section 23.3).
#[derive(Debug, PartialEq, Eq)]
pub struct UaProfile {
    /// Case-insensitive `User-Agent` prefixes forwarded unchanged (a trailing `*` is implied).
    pub accept: &'static [&'static str],
    /// `User-Agent` sent when the client's value is missing or not accepted. Empty `accept` means
    /// this value is always sent.
    pub required: &'static str,
}

const OPENCODE_GO_PROFILE: UaProfile = UaProfile {
    accept: &["opencode/", "claude-cli/", "codex-cli/", "hermes/"],
    required: "opencode/1.2.15",
};

/// Look up a built-in required-client profile by `ua_profile` name.
#[must_use]
pub fn ua_profile_by_name(name: &str) -> Option<&'static UaProfile> {
    match name {
        "opencode_go" => Some(&OPENCODE_GO_PROFILE),
        _ => None,
    }
}

/// Error returned when a User-Agent is rejected by policy.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid user-agent rejected by policy: {0:?}")]
pub struct UaPolicyRejectedError(pub String);

/// Resolve the effective outbound User-Agent per Section 12.2. `None` means send no User-Agent.
///
/// `profile` is only consulted by [`UaPolicy::Profile`]; without a resolvable profile that mode
/// behaves like [`UaPolicy::Pass`] rather than inventing an identity.
pub fn resolve_user_agent(
    policy: UaPolicy,
    incoming_ua: Option<&str>,
    profile: Option<&UaProfile>,
) -> Result<Option<String>, UaPolicyRejectedError> {
    let client_ua = incoming_ua.filter(|ua| !ua.trim().is_empty());
    match policy {
        UaPolicy::Pass => Ok(client_ua.map(str::to_string)),
        UaPolicy::Profile => Ok(match profile {
            None => client_ua.map(str::to_string),
            Some(p) => Some(match client_ua {
                Some(ua)
                    if p.accept
                        .iter()
                        .any(|prefix| ua.to_ascii_lowercase().starts_with(prefix)) =>
                {
                    ua.to_string()
                }
                _ => p.required.to_string(),
            }),
        }),
        UaPolicy::Reject => match client_ua {
            Some(ua) if !is_generic_library_ua(ua) => Ok(Some(ua.to_string())),
            Some(ua) => Err(UaPolicyRejectedError(ua.to_string())),
            None => Err(UaPolicyRejectedError(
                incoming_ua.map_or_else(|| "<empty>".to_string(), str::to_string),
            )),
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
    if let Some(ua) = inbound
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
    {
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
