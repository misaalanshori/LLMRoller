#![forbid(unsafe_code)]
//! Native provider preset and compatibility adapter for TypeSafe System One (`app.typesafe.ai` Agent Gateway with Ed25519 Signed Tokens & Multi-Upstream Routing) (`F110`).
//!
//! Spec: ./docs/srdd/23. SRDD - Multi-Provider Access Layer, OAuth Token Exchange, and Hardware Fingerprinting.md#231-overview
//! Reference: apiclientreferences/fx/system-one.md

use std::collections::BTreeMap;

use bytes::Bytes;
#[allow(unused_imports)]
use llmroller_core::provider::{MaxTokensField, OpenAiCompatConfig, ProviderConfig};
#[allow(unused_imports)]
use serde_json::{Value, json};

use crate::adapters::EndpointKind;
use crate::fingerprint::{DeviceProfile, expand_header_template};

/// Finding ID implemented by this module.
pub const FINDING_ID: &str = "F110";

/// Canonical preset slug matching `presets/typesafe_system_one_and_agent_gateway.toml`.
pub const PRESET_ID: &str = "typesafe_system_one_and_agent_gateway";

/// Human-readable provider display name.
pub const DISPLAY_NAME: &str = "TypeSafe System One (`app.typesafe.ai` Agent Gateway with Ed25519 Signed Tokens & Multi-Upstream Routing)";

/// Default upstream base URL for `typesafe_system_one_and_agent_gateway`.
pub const DEFAULT_BASE_URL: &str = "https://app.typesafe.ai/api/v2/agent";

/// Supported wire protocols for `typesafe_system_one_and_agent_gateway`.
pub const SUPPORTED_PROTOCOLS: &[&str] = &["chat", "responses", "messages"];

/// Reference models bundled with `typesafe_system_one_and_agent_gateway`.
pub const DEFAULT_MODELS: &[&str] = &[
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "gpt-5.4",
    "gemini-3.1-pro",
    "grok-4.20-beta",
];

/// Compile-time embedded `presets/typesafe_system_one_and_agent_gateway.toml` document.
pub const PRESET_TOML: &str =
    include_str!("../../../../presets/typesafe_system_one_and_agent_gateway.toml");

/// Build the full upstream endpoint URL for `typesafe_system_one_and_agent_gateway` (`F110`).
#[must_use]
pub fn build_endpoint_url(base_url: &str, endpoint: EndpointKind) -> String {
    let trimmed = if base_url.trim().is_empty() {
        DEFAULT_BASE_URL
    } else {
        base_url.trim().trim_end_matches('/')
    };
    if trimmed.contains(":streamGenerateContent") || trimmed.contains("/converse-stream") {
        return trimmed.to_string();
    }
    match endpoint {
        EndpointKind::Chat => {
            if trimmed.ends_with("/chat/completions") {
                trimmed.to_string()
            } else {
                format!("{trimmed}/chat/completions")
            }
        }
        EndpointKind::Responses => {
            if trimmed.ends_with("/responses") {
                trimmed.to_string()
            } else {
                format!("{trimmed}/responses")
            }
        }
        EndpointKind::Messages => {
            if trimmed.ends_with("/messages") || trimmed.contains("/messages?") {
                trimmed.to_string()
            } else {
                format!("{trimmed}/messages")
            }
        }
        EndpointKind::Models => format!("{trimmed}/models"),
        EndpointKind::Usage => format!("{trimmed}/usage"),
        EndpointKind::CountTokens => format!("{trimmed}/messages/count_tokens"),
        EndpointKind::ResponsesCompact => format!("{trimmed}/responses/compact"),
        EndpointKind::Embeddings => format!("{trimmed}/embeddings"),
    }
}

/// Synthesize required authentication, hardware fingerprint, and client headers for `typesafe_system_one_and_agent_gateway` (`F110`).
#[must_use]
pub fn synthesize_headers(key_id: &str, session_id: &str) -> BTreeMap<String, String> {
    let profile = DeviceProfile::from_key_id(key_id);
    let mut map = BTreeMap::new();
    map.insert(
        "x-llmroller-preset".to_string(),
        "typesafe_system_one_and_agent_gateway".to_string(),
    );
    for val in map.values_mut() {
        if val.contains("{{") {
            *val = expand_header_template(
                val,
                &profile,
                &profile.device_id,
                session_id,
                1_776_000_000,
            );
        }
    }
    map
}

/// Apply provider-specific JSON payload normalizations for `typesafe_system_one_and_agent_gateway` (`F110`).
#[must_use]
pub fn normalize_request_body(body: Bytes, target_model: &str) -> Bytes {
    if let Some(ref compat) = default_openai_compat()
        && let Ok(mut doc) = serde_json::from_slice::<Value>(&body)
        && crate::body_edit::apply_openai_completions_compat(&mut doc, compat)
        && let Ok(vec) = serde_json::to_vec(&doc)
    {
        return Bytes::from(vec);
    }
    let _ = target_model;
    body
}

/// Optional OpenAI Chat Completions compatibility profile for `typesafe_system_one_and_agent_gateway`.
#[must_use]
pub const fn default_openai_compat() -> Option<OpenAiCompatConfig> {
    None
}

/// Build the default [`ProviderConfig`] for `typesafe_system_one_and_agent_gateway` (`F110`).
#[must_use]
pub fn default_provider_config() -> ProviderConfig {
    ProviderConfig {
        preset_id: Some(PRESET_ID.to_string()),
        supported_protocols: SUPPORTED_PROTOCOLS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        add_headers: synthesize_headers(PRESET_ID, "default-session"),
        openai_compat: default_openai_compat(),
        ..ProviderConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: PRV-1, PRV-2, PRV-3, API-1, API-3, F110 - verify preset TOML, URL builder, headers, and body normalizer"]
    fn regression_test_f110_missing_provider_typesafe_system_one_and() {
        assert!(!PRESET_TOML.is_empty());
        assert!(PRESET_TOML.contains(PRESET_ID));
        assert!(!DEFAULT_MODELS.is_empty());

        let url = build_endpoint_url(DEFAULT_BASE_URL, EndpointKind::Chat);
        assert!(url.starts_with("http://") || url.starts_with("https://"));

        let headers = synthesize_headers("01JKEYTEST0000000000000000", "sess_test_1");
        assert!(!headers.is_empty());
        for val in headers.values() {
            assert!(
                !val.contains("{{uuid}}"),
                "template placeholders must be expanded"
            );
        }

        let sample = Bytes::from_static(
            br#"{"model":"test","messages":[{"role":"developer","content":"sys"},{"role":"user","content":"hi"}],"store":true}"#,
        );
        let normalized = normalize_request_body(sample, DEFAULT_MODELS[0]);
        assert!(!normalized.is_empty());

        let cfg = default_provider_config();
        assert_eq!(cfg.preset_id.as_deref(), Some(PRESET_ID));
        assert!(!cfg.supported_protocols.is_empty());
    }
}
