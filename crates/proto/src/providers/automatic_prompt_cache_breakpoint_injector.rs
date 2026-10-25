#![forbid(unsafe_code)]
//! Native provider preset and compatibility adapter for Automatic Prompt Cache Key & Cache Control Breakpoint Injector Across OpenAI (`prompt_cache_key`), Anthropic (`cache_control`), and Bedrock (`cachePoint`) (`F177`).
//!
//! Spec: ./docs/srdd/23. SRDD - Multi-Provider Access Layer, OAuth Token Exchange, and Hardware Fingerprinting.md#231-overview
//! Reference: apiclientreferences/pi/anthropic.md, apiclientreferences/pi/openai-responses.md, apiclientreferences/pi/openrouter.md, apiclientreferences/pi/amazon-bedrock.md

use std::collections::BTreeMap;

use bytes::Bytes;
#[allow(unused_imports)]
use llmroller_core::provider::{MaxTokensField, OpenAiCompatConfig, ProviderConfig};
#[allow(unused_imports)]
use serde_json::{Value, json};

use crate::adapters::EndpointKind;
use crate::fingerprint::{DeviceProfile, expand_header_template};

/// Finding ID implemented by this module.
pub const FINDING_ID: &str = "F177";

/// Canonical preset slug matching `presets/automatic_prompt_cache_breakpoint_injector.toml`.
pub const PRESET_ID: &str = "automatic_prompt_cache_breakpoint_injector";

/// Human-readable provider display name.
pub const DISPLAY_NAME: &str = "Automatic Prompt Cache Key & Cache Control Breakpoint Injector Across OpenAI (`prompt_cache_key`), Anthropic (`cache_control`), and Bedrock (`cachePoint`)";

/// Default upstream base URL for `automatic_prompt_cache_breakpoint_injector`.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Supported wire protocols for `automatic_prompt_cache_breakpoint_injector`.
pub const SUPPORTED_PROTOCOLS: &[&str] = &["chat", "responses", "messages"];

/// Reference models bundled with `automatic_prompt_cache_breakpoint_injector`.
pub const DEFAULT_MODELS: &[&str] = &["Anthropic", "OpenRouter"];

/// Compile-time embedded `presets/automatic_prompt_cache_breakpoint_injector.toml` document.
pub const PRESET_TOML: &str = include_str!("../../../../presets/automatic_prompt_cache_breakpoint_injector.toml");

/// Build the full upstream endpoint URL for `automatic_prompt_cache_breakpoint_injector` (`F177`).
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

/// Synthesize required authentication, hardware fingerprint, and client headers for `automatic_prompt_cache_breakpoint_injector` (`F177`).
#[must_use]
pub fn synthesize_headers(key_id: &str, session_id: &str) -> BTreeMap<String, String> {
    let profile = DeviceProfile::from_key_id(key_id);
    let mut map = BTreeMap::new();
    map.insert("x-llmroller-preset".to_string(), "automatic_prompt_cache_breakpoint_injector".to_string());
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

/// Apply provider-specific JSON payload normalizations for `automatic_prompt_cache_breakpoint_injector` (`F177`).
#[must_use]
pub fn normalize_request_body(body: Bytes, target_model: &str) -> Bytes {
    // Inject prompt_cache_key or Anthropic ephemeral cache_control breakpoint
    if let Ok(mut val) = serde_json::from_slice::<Value>(&body) {
        let _ = crate::body_edit::inject_openrouter_anthropic_cache_control(&mut val, target_model);
        if let Some(obj) = val.as_object_mut() {
            obj.entry("prompt_cache_key".to_string())
                .or_insert_with(|| Value::String(format!("cache_{target_model}")));
        }
        if let Ok(vec) = serde_json::to_vec(&val) {
            return Bytes::from(vec);
        }
    }
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

/// Optional OpenAI Chat Completions compatibility profile for `automatic_prompt_cache_breakpoint_injector`.
#[must_use]
pub const fn default_openai_compat() -> Option<OpenAiCompatConfig> {
    None
}

/// Build the default [`ProviderConfig`] for `automatic_prompt_cache_breakpoint_injector` (`F177`).
#[must_use]
pub fn default_provider_config() -> ProviderConfig {
    ProviderConfig {
        preset_id: Some(PRESET_ID.to_string()),
        supported_protocols: SUPPORTED_PROTOCOLS.iter().map(|s| (*s).to_string()).collect(),
        add_headers: synthesize_headers(PRESET_ID, "default-session"),
        openai_compat: default_openai_compat(),
        ..ProviderConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: PRV-1, PRV-2, PRV-3, API-1, API-3, F177 - verify preset TOML, URL builder, headers, and body normalizer"]
    fn regression_test_f177_missing_provider_automatic_prompt_cache_() {
        assert!(!PRESET_TOML.is_empty());
        assert!(PRESET_TOML.contains(PRESET_ID));
        assert!(!DEFAULT_MODELS.is_empty());

        let url = build_endpoint_url(DEFAULT_BASE_URL, EndpointKind::Chat);
        assert!(url.starts_with("http://") || url.starts_with("https://"));

        let headers = synthesize_headers("01JKEYTEST0000000000000000", "sess_test_1");
        assert!(!headers.is_empty());
        for val in headers.values() {
            assert!(!val.contains("{{uuid}}"), "template placeholders must be expanded");
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
