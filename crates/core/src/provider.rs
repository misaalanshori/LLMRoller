#![forbid(unsafe_code)]
//! Provider kinds and definitions.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

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
    /// All recognized built-in provider kinds.
    pub const ALL: [Self; 5] = [
        Self::OpencodeGo,
        Self::CommandCode,
        Self::OpenDesignAmr,
        Self::Openrouter,
        Self::Custom,
    ];

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

    /// Authoritative preset definition document embedded at compile time (PRV-2).
    #[must_use]
    pub fn preset_toml(self) -> &'static str {
        match self {
            Self::Openrouter => include_str!("../../../presets/openrouter.toml"),
            Self::OpencodeGo => include_str!("../../../presets/opencode_go.toml"),
            Self::CommandCode => include_str!("../../../presets/command_code.toml"),
            Self::OpenDesignAmr => include_str!("../../../presets/open_design_amr.toml"),
            Self::Custom => include_str!("../../../presets/custom.toml"),
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
    Embeddings,
    CountTokens,
    ResponsesCompact,
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
    #[serde(default)]
    pub embeddings: Option<String>,
    #[serde(default)]
    pub count_tokens: Option<String>,
    #[serde(default)]
    pub responses_compact: Option<String>,
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
            ProviderEndpoint::Embeddings => self.embeddings.as_deref(),
            ProviderEndpoint::CountTokens => self.count_tokens.as_deref(),
            ProviderEndpoint::ResponsesCompact => self.responses_compact.as_deref(),
        }
    }
}

/// Auxiliary endpoint support mode from SRDD Section 9.3.1.
///
/// Legacy configuration files used booleans; `true` maps to [`Self::Supported`]
/// and `false` maps to [`Self::Unsupported`] during deserialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuxEndpointMode {
    #[default]
    Probe,
    Supported,
    Unsupported,
}

impl<'de> Deserialize<'de> for AuxEndpointMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Bool(bool),
            Name(String),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Bool(true) => Ok(Self::Supported),
            Repr::Bool(false) => Ok(Self::Unsupported),
            Repr::Name(name) => match name.as_str() {
                "probe" => Ok(Self::Probe),
                "supported" => Ok(Self::Supported),
                "unsupported" => Ok(Self::Unsupported),
                other => Err(serde::de::Error::unknown_variant(
                    other,
                    &["probe", "supported", "unsupported"],
                )),
            },
        }
    }
}

/// Optional auxiliary capability overrides (`PRV-3`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderAuxEndpoints {
    #[serde(default)]
    pub count_tokens: Option<AuxEndpointMode>,
    #[serde(default)]
    pub responses_compact: Option<AuxEndpointMode>,
}

/// Default maximum byte length of an outbound session affinity value.
pub const DEFAULT_SESSION_MAX_LEN: usize = 256;

const fn default_session_max_len() -> usize {
    DEFAULT_SESSION_MAX_LEN
}

/// Outbound provider session hint profile (SRDD Section 11.2).
///
/// `headers` are written with the resolved affinity token, and `body_field` is
/// inserted into the surgical JSON body when the endpoint supports it. Missing
/// configuration falls back to the adapter preset default. The canonical
/// Section 9.3.1 names (`headers_if_absent` / `body_field_if_absent`) are
/// accepted as aliases for the legacy names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProfile {
    #[serde(default, alias = "headers_if_absent")]
    pub headers: Vec<String>,
    #[serde(default, alias = "body_field_if_absent")]
    pub body_field: Option<String>,
    #[serde(default = "default_session_max_len")]
    pub max_len: usize,
}

impl Default for SessionProfile {
    fn default() -> Self {
        Self {
            headers: Vec::new(),
            body_field: None,
            max_len: DEFAULT_SESSION_MAX_LEN,
        }
    }
}

/// Header name and value prefix used to inject an upstream credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAuthScheme {
    pub header: String,
    #[serde(default)]
    pub prefix: String,
}

/// Shorthand credential schemes accepted by SRDD Section 9.3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthShorthand {
    Bearer,
    XApiKey,
    XApiKeyOrBearer,
}

/// Per-protocol credential configuration: either a canonical shorthand string
/// or an explicit `{header, prefix}` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProtocolAuthConfig {
    Shorthand(AuthShorthand),
    Explicit(ProviderAuthScheme),
}

impl ProtocolAuthConfig {
    /// Resolve this configuration to a concrete header and value prefix.
    ///
    /// `x_api_key_or_bearer` prefers the native Anthropic `x-api-key` header for
    /// Messages endpoints and OpenAI-compatible Bearer auth elsewhere.
    #[must_use]
    pub fn resolve(&self, endpoint: ProviderEndpoint) -> ProviderAuthScheme {
        match self {
            Self::Explicit(scheme) => scheme.clone(),
            Self::Shorthand(AuthShorthand::Bearer) => ProviderAuthScheme {
                header: "authorization".to_string(),
                prefix: "Bearer ".to_string(),
            },
            Self::Shorthand(AuthShorthand::XApiKey) => ProviderAuthScheme {
                header: "x-api-key".to_string(),
                prefix: String::new(),
            },
            Self::Shorthand(AuthShorthand::XApiKeyOrBearer) => {
                if matches!(
                    endpoint,
                    ProviderEndpoint::Messages | ProviderEndpoint::CountTokens
                ) {
                    ProviderAuthScheme {
                        header: "x-api-key".to_string(),
                        prefix: String::new(),
                    }
                } else {
                    ProviderAuthScheme {
                        header: "authorization".to_string(),
                        prefix: "Bearer ".to_string(),
                    }
                }
            }
        }
    }
}

/// Per-protocol credential injection schemes (`PRV-3`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderAuth {
    #[serde(default)]
    pub default: Option<ProtocolAuthConfig>,
    #[serde(default)]
    pub chat: Option<ProtocolAuthConfig>,
    #[serde(default)]
    pub responses: Option<ProtocolAuthConfig>,
    #[serde(default)]
    pub messages: Option<ProtocolAuthConfig>,
    #[serde(default)]
    pub models: Option<ProtocolAuthConfig>,
    #[serde(default)]
    pub usage: Option<ProtocolAuthConfig>,
    #[serde(default)]
    pub embeddings: Option<ProtocolAuthConfig>,
}

impl ProviderAuth {
    /// Resolve the configured scheme for an endpoint, falling back to `auth.default`.
    #[must_use]
    pub fn for_endpoint(&self, endpoint: ProviderEndpoint) -> Option<ProviderAuthScheme> {
        let specific = match endpoint {
            ProviderEndpoint::Chat => self.chat.as_ref(),
            ProviderEndpoint::Responses | ProviderEndpoint::ResponsesCompact => {
                self.responses.as_ref()
            }
            ProviderEndpoint::Messages | ProviderEndpoint::CountTokens => self.messages.as_ref(),
            ProviderEndpoint::Models => self.models.as_ref(),
            ProviderEndpoint::Usage => self.usage.as_ref(),
            ProviderEndpoint::Embeddings => self.embeddings.as_ref().or(self.chat.as_ref()),
        };
        specific
            .or(self.default.as_ref())
            .map(|config| config.resolve(endpoint))
    }
}

/// Per-header comma-separated token stripping rules (`API-3`, `TRN-1`).
///
/// The canonical SRDD Section 9.3.1 shape is a map from lowercase header name
/// to the tokens removed from that header. Legacy configuration stored a flat
/// list, which continues to apply to the beta headers (`anthropic-beta` and
/// `openai-beta`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct StripHeaderTokens(BTreeMap<String, Vec<String>>);

impl StripHeaderTokens {
    /// Tokens configured for a header name, matched case-insensitively.
    #[must_use]
    pub fn for_header(&self, header_name: &str) -> Option<&[String]> {
        self.0
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(header_name))
            .map(|(_, tokens)| tokens.as_slice())
    }

    /// Whether any header has token-stripping rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<String>> for StripHeaderTokens {
    fn from(tokens: Vec<String>) -> Self {
        let mut map = BTreeMap::new();
        if !tokens.is_empty() {
            map.insert("anthropic-beta".to_string(), tokens.clone());
            map.insert("openai-beta".to_string(), tokens);
        }
        Self(map)
    }
}

impl<'de> Deserialize<'de> for StripHeaderTokens {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            PerHeader(BTreeMap<String, Vec<String>>),
            Legacy(Vec<String>),
        }

        Ok(match Repr::deserialize(deserializer)? {
            Repr::PerHeader(map) => Self(map),
            Repr::Legacy(tokens) => Self::from(tokens),
        })
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

/// Provider `/usage` profile from SRDD Section 9.3.1 (`PRV-3`, `USG-4`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UsageProfile {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub parser_kind: Option<String>,
}

/// User-Agent policy configured per provider (`API-3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UaPolicySetting {
    Pass,
    /// Required-client profile (`ua_profile`, SRDD 12.2). `label` is the pre-rename spelling that
    /// existing stored provider configs may still carry.
    #[serde(alias = "label")]
    Profile,
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
    #[serde(default)]
    pub model_not_found_substrings: Vec<String>,
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

/// Per-provider timeout overrides (SRDD Section 9.3.1 and Section 10).
///
/// A missing or non-positive `stream_idle_timeout_ms` falls back to the
/// 300,000 ms default. The remaining fields mirror the canonical schema and
/// are consumed by the failover and pre-first-frame timeout paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTimeouts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_ms: Option<i64>,
    #[serde(default)]
    pub timeout_failover: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_activity_timeout_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_valid_frame_timeout_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonstream_response_timeout_ms: Option<i64>,
}

/// Default gap allowed between bytes after the first valid streaming frame.
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: i64 = 300_000;

/// Current canonical `providers.config_json` schema version.
pub const PROVIDER_CONFIG_SCHEMA_VERSION: u32 = 1;

const fn default_schema_version() -> u32 {
    PROVIDER_CONFIG_SCHEMA_VERSION
}

const fn default_inherit_preset_defaults() -> bool {
    true
}

/// Parsed `providers.config_json` projection used by the data plane (`PRV-3`).
///
/// Unknown keys are preserved by the database but ignored here so older or newer
/// control-plane versions can coexist. Missing values fall back to the adapter
/// preset default at the call site.
/// Declarative JSON body edit operation (`F024`, `PRV-3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyEditOp {
    Set,
    Default,
    Delete,
    Move,
}

/// Declarative JSON body edit rule supporting dot paths, null-to-object promotion, and `*` wildcards (`F024`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyEditRule {
    pub op: BodyEditOp,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Preferred max output tokens field name for OpenAI Chat Completions compatibility (`F007`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    MaxTokens,
    MaxCompletionTokens,
}

/// Provider-level OpenAI Chat Completions compatibility normalizations (`F007`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiCompatConfig {
    #[serde(default)]
    pub rewrite_developer_role_to_system: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    #[serde(default)]
    pub strip_store: bool,
    #[serde(default)]
    pub strip_reasoning_effort: bool,
    #[serde(default)]
    pub normalize_mistral_tool_ids: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_inherit_preset_defaults")]
    pub inherit_preset_defaults: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_protocols: Vec<String>,
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
    pub strip_header_tokens: StripHeaderTokens,
    #[serde(default)]
    pub add_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub response_header_allow: Vec<String>,
    #[serde(default)]
    pub ua_policy: Option<UaPolicySetting>,
    /// Required-client profile name used by `ua_policy = profile` (SRDD 12.2, 23.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ua_profile: Option<String>,
    #[serde(default)]
    pub session_profile: Option<SessionProfile>,
    #[serde(default)]
    pub usage_profile: Option<UsageProfile>,
    #[serde(default)]
    pub error_classifiers: ErrorClassifiers,
    #[serde(default)]
    pub timeouts: ProviderTimeouts,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_edits: Vec<BodyEditRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_compat: Option<OpenAiCompatConfig>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            schema_version: PROVIDER_CONFIG_SCHEMA_VERSION,
            inherit_preset_defaults: true,
            preset_id: None,
            supported_protocols: Vec::new(),
            paths: ProviderPaths::default(),
            aux_endpoints: ProviderAuxEndpoints::default(),
            auth: ProviderAuth::default(),
            usage_reporting: UsageReporting::default(),
            strip_headers: Vec::new(),
            strip_header_tokens: StripHeaderTokens::default(),
            add_headers: BTreeMap::new(),
            response_header_allow: Vec::new(),
            ua_policy: None,
            ua_profile: None,
            session_profile: None,
            usage_profile: None,
            error_classifiers: ErrorClassifiers::default(),
            timeouts: ProviderTimeouts::default(),
            body_edits: Vec::new(),
            openai_compat: None,
        }
    }
}

impl ProviderConfig {
    /// Parse a provider configuration document, defaulting to an empty profile.
    ///
    /// Database rows written by older versions or by hand may be corrupt or
    /// schema-incompatible. Snapshot loading must stay fail-open; control-plane
    /// write boundaries use [`Self::try_from_json`] to reject bad documents.
    #[must_use]
    pub fn from_json(config_json: &str) -> Self {
        Self::try_from_json(config_json).unwrap_or_default()
    }

    /// Parse a provider configuration document, reporting deserialization errors.
    pub fn try_from_json(config_json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(config_json)
    }

    /// Effective post-first-frame SSE idle timeout in milliseconds.
    #[must_use]
    pub fn stream_idle_timeout_ms(&self) -> u64 {
        self.timeouts
            .stream_idle_timeout_ms
            .filter(|ms| *ms > 0)
            .map_or(DEFAULT_STREAM_IDLE_TIMEOUT_MS as u64, |ms| ms as u64)
    }

    /// Effective first-activity timeout in milliseconds (SRDD Section 10.5.2).
    #[must_use]
    pub fn first_activity_timeout_ms(&self) -> u64 {
        self.timeouts
            .first_activity_timeout_ms
            .filter(|ms| *ms > 0)
            .map_or(60_000, |ms| ms as u64)
    }

    /// Effective first-valid-frame timeout in milliseconds (SRDD Section 10.5.2).
    #[must_use]
    pub fn first_valid_frame_timeout_ms(&self) -> u64 {
        self.timeouts
            .first_valid_frame_timeout_ms
            .filter(|ms| *ms > 0)
            .map_or(180_000, |ms| ms as u64)
    }

    /// Effective non-streaming response body timeout in milliseconds (SRDD Section 10.5.2).
    #[must_use]
    pub fn nonstream_response_timeout_ms(&self) -> u64 {
        self.timeouts
            .nonstream_response_timeout_ms
            .filter(|ms| *ms > 0)
            .map_or(600_000, |ms| ms as u64)
    }

    /// Whether post-send timeouts are permitted to fail over to another key (`F003`, SRDD Section 10.5.2).
    #[must_use]
    pub const fn timeout_failover(&self) -> bool {
        self.timeouts.timeout_failover
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
                max_len: DEFAULT_SESSION_MAX_LEN,
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

    /// Verbatim `providers.config_json` document from SRDD Section 9.3.1.
    const SRDD_9_3_1_CONFIG: &str = r#"{
      "schema_version": 1,
      "inherit_preset_defaults": true,
      "paths": {
        "chat": "/chat/completions",
        "responses": "/responses",
        "messages": "/messages",
        "models": "/models"
      },
      "aux_endpoints": {
        "count_tokens": "probe",
        "responses_compact": "probe"
      },
      "auth": {
        "chat": "bearer",
        "responses": "bearer",
        "messages": "x_api_key_or_bearer"
      },
      "usage_reporting": {
        "chat_stream": "inject"
      },
      "strip_headers": ["x-experimental-*"],
      "strip_header_tokens": {
        "anthropic-beta": []
      },
      "add_headers": {},
      "response_header_allow": [],
      "ua_policy": "profile",
      "ua_profile": "opencode_go",
      "session_profile": {
        "headers_if_absent": ["x-opencode-session"],
        "body_field_if_absent": null,
        "max_len": 256
      },
      "usage_profile": {
        "enabled": true,
        "endpoints": ["https://opencode.ai/zen/go/v1/usage"],
        "parser_kind": "opencode_go"
      },
      "timeouts": {
        "timeout_failover": false,
        "first_activity_timeout_ms": 60000,
        "first_valid_frame_timeout_ms": 180000,
        "nonstream_response_timeout_ms": 600000
      },
      "error_classifiers": {
        "quota_exhausted_substrings": ["quota_exceeded", "window_limit_reached", "insufficient_credits"],
        "request_rejected_substrings": ["flagged", "moderation"],
        "plan_restricted_substrings": ["upgrade_required", "not available on your plan"],
        "credential_invalid_substrings": ["invalid_api_key", "invalid x-api-key", "unauthorized"],
        "model_not_found_substrings": ["model_not_found", "does not exist", "unknown model"]
      }
    }"#;

    #[doc = "Spec: PRV-3, API-2, API-3, RTE-1, RTE-9, TRN-1 - canonical SRDD Section 9.3.1 JSON parses without silent wipeout"]
    #[test]
    fn provider_config_parses_srdd_9_3_1_canonical_document() {
        let config = ProviderConfig::try_from_json(SRDD_9_3_1_CONFIG)
            .expect("canonical SRDD provider config must deserialize");

        assert_eq!(config.schema_version, 1);
        assert!(config.inherit_preset_defaults);
        assert_eq!(config.paths.chat.as_deref(), Some("/chat/completions"));
        assert_eq!(config.paths.responses.as_deref(), Some("/responses"));
        assert_eq!(config.paths.messages.as_deref(), Some("/messages"));
        assert_eq!(config.paths.models.as_deref(), Some("/models"));
        assert_eq!(
            config.aux_endpoints.count_tokens,
            Some(AuxEndpointMode::Probe)
        );
        assert_eq!(
            config.aux_endpoints.responses_compact,
            Some(AuxEndpointMode::Probe)
        );

        let chat_auth = config
            .auth
            .for_endpoint(ProviderEndpoint::Chat)
            .expect("chat auth");
        assert_eq!(chat_auth.header, "authorization");
        assert_eq!(chat_auth.prefix, "Bearer ");
        let messages_auth = config
            .auth
            .for_endpoint(ProviderEndpoint::Messages)
            .expect("messages auth");
        assert_eq!(messages_auth.header, "x-api-key");
        assert_eq!(messages_auth.prefix, "");

        assert_eq!(
            config.usage_reporting.chat_stream,
            Some(ChatStreamSetting::Inject)
        );
        assert_eq!(config.strip_headers, vec!["x-experimental-*".to_string()]);
        assert_eq!(
            config
                .strip_header_tokens
                .for_header("anthropic-beta")
                .map(<[String]>::len),
            Some(0)
        );
        assert_eq!(config.ua_policy, Some(UaPolicySetting::Profile));
        assert_eq!(config.ua_profile.as_deref(), Some("opencode_go"));

        // Pre-rename stored configs spell the policy `label`; they must keep parsing.
        let legacy = ProviderConfig::try_from_json(r#"{"ua_policy":"label"}"#)
            .expect("legacy label spelling");
        assert_eq!(legacy.ua_policy, Some(UaPolicySetting::Profile));

        let session = config.session_profile.as_ref().expect("session profile");
        assert_eq!(session.headers, vec!["x-opencode-session".to_string()]);
        assert_eq!(session.body_field, None);
        assert_eq!(session.max_len, 256);

        let usage = config.usage_profile.as_ref().expect("usage profile");
        assert!(usage.enabled);
        assert_eq!(
            usage.endpoints,
            vec!["https://opencode.ai/zen/go/v1/usage".to_string()]
        );
        assert_eq!(usage.parser_kind.as_deref(), Some("opencode_go"));

        assert!(!config.timeouts.timeout_failover);
        assert_eq!(config.timeouts.first_activity_timeout_ms, Some(60_000));
        assert_eq!(config.timeouts.first_valid_frame_timeout_ms, Some(180_000));
        assert_eq!(config.timeouts.nonstream_response_timeout_ms, Some(600_000));
        assert_eq!(config.stream_idle_timeout_ms(), 300_000);

        let classifiers = &config.error_classifiers;
        assert_eq!(
            classifiers.quota_exhausted_substrings,
            vec![
                "quota_exceeded",
                "window_limit_reached",
                "insufficient_credits"
            ]
        );
        assert_eq!(
            classifiers.request_rejected_substrings,
            vec!["flagged", "moderation"]
        );
        assert_eq!(
            classifiers.plan_restricted_substrings,
            vec!["upgrade_required", "not available on your plan"]
        );
        assert_eq!(
            classifiers.credential_invalid_substrings,
            vec!["invalid_api_key", "invalid x-api-key", "unauthorized"]
        );
        assert_eq!(
            classifiers.model_not_found_substrings,
            vec!["model_not_found", "does not exist", "unknown model"]
        );
    }

    #[doc = "Spec: PRV-3, API-3 - legacy boolean aux modes, explicit auth objects, flat token list, and session aliases remain accepted"]
    #[test]
    fn provider_config_accepts_legacy_shapes() {
        let config = ProviderConfig::try_from_json(
            r#"{
                "aux_endpoints": {"count_tokens": true, "responses_compact": false},
                "auth": {"chat": {"header": "x-goog-api-key", "prefix": ""}},
                "strip_header_tokens": ["legacy-beta"],
                "session_profile": {"headers": ["x-session-id"], "body_field": "session_id"}
            }"#,
        )
        .expect("legacy config must deserialize");

        assert_eq!(
            config.aux_endpoints.count_tokens,
            Some(AuxEndpointMode::Supported)
        );
        assert_eq!(
            config.aux_endpoints.responses_compact,
            Some(AuxEndpointMode::Unsupported)
        );
        let chat_auth = config
            .auth
            .for_endpoint(ProviderEndpoint::Chat)
            .expect("chat auth");
        assert_eq!(chat_auth.header, "x-goog-api-key");
        assert_eq!(chat_auth.prefix, "");
        assert_eq!(
            config.strip_header_tokens.for_header("anthropic-beta"),
            Some(["legacy-beta".to_string()].as_slice())
        );
        assert_eq!(
            config.strip_header_tokens.for_header("openai-beta"),
            Some(["legacy-beta".to_string()].as_slice())
        );
        let session = config.session_profile.as_ref().expect("session profile");
        assert_eq!(session.max_len, DEFAULT_SESSION_MAX_LEN);
    }

    #[doc = "Spec: PRV-3, API-2 - invalid config_json is reported at write boundaries but tolerated on read"]
    #[test]
    #[doc = "Spec: PRV-2"]
    fn preset_toml_loads_embedded_presets() {
        for kind in [
            ProviderKind::Openrouter,
            ProviderKind::OpencodeGo,
            ProviderKind::CommandCode,
            ProviderKind::OpenDesignAmr,
            ProviderKind::Custom,
        ] {
            let toml = kind.preset_toml();
            assert!(!toml.is_empty(), "preset for {kind:?} must not be empty");
            assert!(toml.contains("kind ="), "preset must declare kind");
        }
    }

    #[test]
    fn provider_config_try_from_json_reports_errors() {
        let invalid = r#"{"auth":{"chat":"carrier_pigeon"}}"#;
        assert!(
            ProviderConfig::try_from_json(invalid).is_err(),
            "unknown auth shorthand must be rejected"
        );
        assert_eq!(
            ProviderConfig::from_json(invalid),
            ProviderConfig::default(),
            "read-side parsing stays fail-open"
        );
    }
}
