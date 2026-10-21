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
}

impl ProviderConfig {
    /// Parse a provider configuration document, defaulting to an empty profile.
    #[must_use]
    pub fn from_json(config_json: &str) -> Self {
        serde_json::from_str(config_json).unwrap_or_default()
    }
}
