#![forbid(unsafe_code)]
//! Provider kinds and definitions.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Supported provider kinds matching the SQLite schema CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpencodeGo,
    CommandCode,
    OpenDesignAmr,
    Openrouter,
    Custom,
}

impl ProviderKind {
    /// Lowercase string representation matching the database enum.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpencodeGo => "opencode_go",
            Self::CommandCode => "command_code",
            Self::OpenDesignAmr => "open_design_amr",
            Self::Openrouter => "openrouter",
            Self::Custom => "custom",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Error returned when parsing an unrecognized provider kind string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidProviderKindError(pub String);

impl std::fmt::Display for InvalidProviderKindError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "invalid provider kind {:?}; expected opencode_go, command_code, open_design_amr, openrouter, or custom",
            self.0
        )
    }
}

impl std::error::Error for InvalidProviderKindError {}

impl std::str::FromStr for ProviderKind {
    type Err = InvalidProviderKindError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "opencode_go" => Ok(Self::OpencodeGo),
            "command_code" => Ok(Self::CommandCode),
            "open_design_amr" => Ok(Self::OpenDesignAmr),
            "openrouter" => Ok(Self::Openrouter),
            "custom" => Ok(Self::Custom),
            other => Err(InvalidProviderKindError(other.to_owned())),
        }
    }
}

/// Endpoint families whose provider configuration is independently editable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderEndpoint {
    Chat,
    Responses,
    Messages,
    Models,
    Usage,
}

/// Protocol paths configured in `providers.config_json` (`PRV-3`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderPaths {
    #[serde(default)]
    pub chat: Option<String>,
    #[serde(default)]
    pub responses: Option<String>,
    #[serde(default)]
    pub messages: Option<String>,
    #[serde(default)]
    pub models: Option<String>,
    #[serde(default)]
    pub usage: Option<String>,
}

impl ProviderPaths {
    #[must_use]
    pub fn for_endpoint(&self, endpoint: ProviderEndpoint) -> Option<&str> {
        match endpoint {
            ProviderEndpoint::Chat => self.chat.as_deref(),
            ProviderEndpoint::Responses => self.responses.as_deref(),
            ProviderEndpoint::Messages => self.messages.as_deref(),
            ProviderEndpoint::Models => self.models.as_deref(),
            ProviderEndpoint::Usage => self.usage.as_deref(),
        }
    }
}

/// Optional auxiliary capability overrides (`PRV-3`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderAuxEndpoints {
    #[serde(default)]
    pub count_tokens: Option<bool>,
    #[serde(default)]
    pub responses_compact: Option<bool>,
}

/// Outbound provider session hint profile (SRDD Section 11.2).
///
/// `headers` are written with the resolved affinity token, and `body_field` is
/// inserted into the surgical JSON body when the endpoint supports it. Missing
/// configuration falls back to the adapter preset default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProfile {
    #[serde(default)]
    pub headers: Vec<String>,
    #[serde(default)]
    pub body_field: Option<String>,
}

/// Header name and value prefix used to inject an upstream credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAuthScheme {
    pub header: String,
    #[serde(default)]
    pub prefix: String,
}

/// Per-protocol credential injection schemes (`PRV-3`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderAuth {
    #[serde(default)]
    pub default: Option<ProviderAuthScheme>,
    #[serde(default)]
    pub chat: Option<ProviderAuthScheme>,
    #[serde(default)]
    pub responses: Option<ProviderAuthScheme>,
    #[serde(default)]
    pub messages: Option<ProviderAuthScheme>,
    #[serde(default)]
    pub models: Option<ProviderAuthScheme>,
    #[serde(default)]
    pub usage: Option<ProviderAuthScheme>,
}

impl ProviderAuth {
    /// Resolve the configured scheme for an endpoint, falling back to `auth.default`.
    #[must_use]
    pub fn for_endpoint(&self, endpoint: ProviderEndpoint) -> Option<&ProviderAuthScheme> {
        let specific = match endpoint {
            ProviderEndpoint::Chat => self.chat.as_ref(),
            ProviderEndpoint::Responses => self.responses.as_ref(),
            ProviderEndpoint::Messages => self.messages.as_ref(),
            ProviderEndpoint::Models => self.models.as_ref(),
            ProviderEndpoint::Usage => self.usage.as_ref(),
        };
        specific.or(self.default.as_ref())
    }
}

/// Streaming usage-reporting modes for Chat Completions (`API-4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatStreamSetting {
    Inject,
    Native,
    None,
}

/// Usage-reporting configuration (`PRV-3`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UsageReporting {
    #[serde(default)]
    pub chat_stream: Option<ChatStreamSetting>,
}

/// User-Agent policy configured per provider (`API-3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UaPolicySetting {
    Pass,
    Label,
    Reject,
}

/// Per-provider error-classification overrides configured in `providers.config_json`
/// (`PRV-3`, `API-8`).
///
/// Substrings are matched case-insensitively against the first 8 KiB of the upstream
/// body and are evaluated before the built-in preset patterns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorClassifiers {
    #[serde(default)]
    pub quota_exhausted_substrings: Vec<String>,
    #[serde(default)]
    pub request_rejected_substrings: Vec<String>,
    #[serde(default)]
    pub plan_restricted_substrings: Vec<String>,
    #[serde(default)]
    pub credential_invalid_substrings: Vec<String>,
}

impl ErrorClassifiers {
    /// Extract `error_classifiers` from a provider `config_json` document, if present.
    #[must_use]
    pub fn from_provider_config(config_json: &str) -> Self {
        serde_json::from_str::<serde_json::Value>(config_json)
            .ok()
            .and_then(|value| value.get("error_classifiers").cloned())
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    /// Whether any configured substring appears in an already-lowercased body.
    #[must_use]
    pub fn matches(list: &[String], body_lowercase: &str) -> bool {
        list.iter()
            .map(|sub| sub.to_ascii_lowercase())
            .any(|sub| !sub.is_empty() && body_lowercase.contains(&sub))
    }
}

/// Per-provider timeout overrides (SRDD Section 10, `stream_idle_timeout_ms`).
///
/// A missing or non-positive value falls back to the 300,000 ms default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTimeouts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_ms: Option<i64>,
}

/// Default gap allowed between bytes after the first valid streaming frame.
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: i64 = 300_000;

/// Parsed `providers.config_json` projection used by the data plane (`PRV-3`).
///
/// Unknown keys are preserved by the database but ignored here so older or newer
/// control-plane versions can coexist. Missing values fall back to the adapter
/// preset default at the call site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    #[serde(default)]
    pub paths: ProviderPaths,
    #[serde(default)]
    pub aux_endpoints: ProviderAuxEndpoints,
    #[serde(default)]
    pub auth: ProviderAuth,
    #[serde(default)]
    pub usage_reporting: UsageReporting,
    #[serde(default)]
    pub strip_headers: Vec<String>,
    #[serde(default)]
    pub strip_header_tokens: Vec<String>,
    #[serde(default)]
    pub add_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub response_header_allow: Vec<String>,
    #[serde(default)]
    pub ua_policy: Option<UaPolicySetting>,
    #[serde(default)]
    pub session_profile: Option<SessionProfile>,
    #[serde(default)]
    pub error_classifiers: ErrorClassifiers,
    #[serde(default)]
    pub timeouts: ProviderTimeouts,
}

impl ProviderConfig {
    /// Parse a provider configuration document, defaulting to an empty profile.
    #[must_use]
    pub fn from_json(config_json: &str) -> Self {
        serde_json::from_str(config_json).unwrap_or_default()
    }

    /// Effective post-first-frame SSE idle timeout in milliseconds.
    #[must_use]
    pub fn stream_idle_timeout_ms(&self) -> u64 {
        self.timeouts
            .stream_idle_timeout_ms
            .filter(|ms| *ms > 0)
            .map_or(DEFAULT_STREAM_IDLE_TIMEOUT_MS as u64, |ms| ms as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[doc = "Spec: PRV-3, SEC-16 - session_profile is editable and absence falls back to preset defaults"]
    #[test]
    fn provider_config_parses_optional_session_profile() {
        let configured = ProviderConfig::from_json(
            r#"{"session_profile":{"headers":["x-session-id"],"body_field":"session_id"}}"#,
        );
        assert_eq!(
            configured.session_profile,
            Some(SessionProfile {
                headers: vec!["x-session-id".to_string()],
                body_field: Some("session_id".to_string()),
            })
        );

        let absent = ProviderConfig::from_json("{}");
        assert_eq!(
            absent.session_profile, None,
            "missing session_profile must fall back to the adapter preset default"
        );

        let off = ProviderConfig::from_json(r#"{"session_profile":{"headers":[]}}"#);
        assert_eq!(
            off.session_profile,
            Some(SessionProfile::default()),
            "an explicit empty profile disables affinity injection"
        );
    }

    #[doc = "Spec: API-6, API-7 - a provider may override the post-first-frame idle timeout"]
    #[test]
    fn provider_config_reads_stream_idle_timeout_override() {
        assert_eq!(
            ProviderConfig::from_json("{}").stream_idle_timeout_ms(),
            300_000,
            "missing timeouts must use the SRDD default"
        );
        assert_eq!(
            ProviderConfig::from_json(r#"{"timeouts":{"stream_idle_timeout_ms":200}}"#)
                .stream_idle_timeout_ms(),
            200
        );
        assert_eq!(
            ProviderConfig::from_json(r#"{"timeouts":{"stream_idle_timeout_ms":0}}"#)
                .stream_idle_timeout_ms(),
            300_000,
            "non-positive overrides must not disable the timeout"
        );
    }
}
