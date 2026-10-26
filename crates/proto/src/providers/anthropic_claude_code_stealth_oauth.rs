#![forbid(unsafe_code)]
//! Native provider preset and compatibility adapter for Anthropic Claude Code (Stealth OAuth Mode with cch Billing Header & Tool Prefixing) (`F104`).
//!
//! Spec: ./docs/srdd/23. SRDD - Multi-Provider Access Layer, OAuth Token Exchange, and Hardware Fingerprinting.md#231-overview
//! Reference: apiclientreferences/pi/anthropic.md, apiclientreferences/fx/anthropic.md, apiclientreferences/opencode/anthropic.md, apiclientreferences/9router/claude.md

use std::collections::BTreeMap;

use bytes::Bytes;
#[allow(unused_imports)]
use llmroller_core::provider::{MaxTokensField, OpenAiCompatConfig, ProviderConfig};
#[allow(unused_imports)]
use serde_json::{Value, json};

use crate::adapters::EndpointKind;
use crate::fingerprint::{DeviceProfile, expand_header_template};

/// Finding ID implemented by this module.
pub const FINDING_ID: &str = "F104";

/// Canonical preset slug matching `presets/anthropic_claude_code_stealth_oauth.toml`.
pub const PRESET_ID: &str = "anthropic_claude_code_stealth_oauth";

/// Human-readable provider display name.
pub const DISPLAY_NAME: &str =
    "Anthropic Claude Code (Stealth OAuth Mode with cch Billing Header & Tool Prefixing)";

/// Default upstream base URL for `anthropic_claude_code_stealth_oauth`.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1/messages?beta=true";

/// Supported wire protocols for `anthropic_claude_code_stealth_oauth`.
pub const SUPPORTED_PROTOCOLS: &[&str] = &["messages"];

/// Reference models bundled with `anthropic_claude_code_stealth_oauth`.
pub const DEFAULT_MODELS: &[&str] = &[
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "claude-opus-4-5-20251101",
    "claude-sonnet-4-5-20250929",
    "claude-haiku-4-5-20251001",
];

/// Compile-time embedded `presets/anthropic_claude_code_stealth_oauth.toml` document.
pub const PRESET_TOML: &str =
    include_str!("../../../../presets/anthropic_claude_code_stealth_oauth.toml");

/// Build the full upstream endpoint URL for `anthropic_claude_code_stealth_oauth` (`F104`).
#[must_use]
pub fn build_endpoint_url(base_url: &str, endpoint: EndpointKind) -> String {
    let raw = if base_url.trim().is_empty() {
        DEFAULT_BASE_URL
    } else {
        base_url.trim().trim_end_matches('/')
    };
    let default_model = DEFAULT_MODELS.first().copied().unwrap_or("gpt-4o");
    let interpolated = raw
        .replace("{model}", default_model)
        .replace("{region}", "us-east-1")
        .replace("{project}", "default-project")
        .replace("{location}", "us-central1")
        .replace("{account_id}", "default-account")
        .replace("{deployment}", default_model);
    let trimmed = interpolated.trim_end_matches('/');

    if trimmed.contains(":streamGenerateContent") {
        let root = trimmed
            .split("/models/")
            .next()
            .and_then(|p| p.split("/v1internal").next())
            .unwrap_or(trimmed);
        return match endpoint {
            EndpointKind::Models => format!("{root}/v1beta/models"),
            EndpointKind::Usage => format!("{root}/v1beta/usage"),
            EndpointKind::Embeddings => {
                format!("{root}/v1beta/models/{default_model}:embedContent")
            }
            _ => trimmed.to_string(),
        };
    }
    if trimmed.contains("/converse-stream") {
        let root = trimmed.split("/model/").next().unwrap_or(trimmed);
        return match endpoint {
            EndpointKind::Models => format!("{root}/foundation-models"),
            EndpointKind::Usage => format!("{root}/usage"),
            _ => trimmed.to_string(),
        };
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

/// Synthesize required authentication, hardware fingerprint, and client headers for `anthropic_claude_code_stealth_oauth` (`F104`).
#[must_use]
pub fn synthesize_headers(key_id: &str, session_id: &str) -> BTreeMap<String, String> {
    let profile = DeviceProfile::from_key_id(key_id);
    let now_secs = crate::fingerprint::current_unix_timestamp_secs();
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    map.insert(
        "user-agent".to_string(),
        "claude-cli/2.1.63 (external, cli)".to_string(),
    );
    map.insert("x-app".to_string(), "cli".to_string());
    map.insert(
        "anthropic-dangerous-direct-browser-access".to_string(),
        "true".to_string(),
    );
    map.insert("anthropic-beta".to_string(), "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05".to_string());
    map.insert("x-stainless-lang".to_string(), "js".to_string());
    map.insert(
        "x-stainless-package-version".to_string(),
        "0.74.0".to_string(),
    );
    map.insert("x-stainless-os".to_string(), "MacOS".to_string());
    map.insert("x-stainless-arch".to_string(), "arm64".to_string());
    map.insert("x-stainless-runtime".to_string(), "node".to_string());
    map.insert(
        "x-stainless-runtime-version".to_string(),
        "v24.3.0".to_string(),
    );
    for val in map.values_mut() {
        if val.contains("{{") {
            *val = expand_header_template(val, &profile, &profile.device_id, session_id, now_secs);
        }
    }
    let _ = (&profile, session_id, now_secs);
    map
}

/// Apply provider-specific JSON payload normalizations for `anthropic_claude_code_stealth_oauth` (`F104`).
#[must_use]
pub fn normalize_request_body(body: Bytes, target_model: &str) -> Bytes {
    if let Ok(mut doc) = serde_json::from_slice::<Value>(&body)
        && let Some(obj) = doc.as_object_mut()
    {
        if !target_model.is_empty() {
            obj.insert("model".to_string(), Value::String(target_model.to_string()));
        }
        let profile = DeviceProfile::from_key_id(PRESET_ID);
        let billing_line = profile.claude_billing_header(target_model);
        match obj.get_mut("system") {
            Some(Value::String(s)) => {
                if !s.contains("x-anthropic-billing-header:") {
                    *s = format!(
                        "{billing_line}
{s}"
                    );
                }
            }
            Some(Value::Array(arr)) => {
                let has_billing = arr.iter().any(|b| {
                    b.get("text")
                        .and_then(|t| t.as_str())
                        .is_some_and(|t| t.contains("x-anthropic-billing-header:"))
                });
                if !has_billing {
                    arr.insert(0, json!({"type": "text", "text": billing_line}));
                }
            }
            None => {
                obj.insert(
                    "system".to_string(),
                    json!([{"type": "text", "text": billing_line}]),
                );
            }
            _ => {}
        }
        if let Ok(vec) = serde_json::to_vec(&doc) {
            return Bytes::from(vec);
        }
    }
    body
}

/// Optional OpenAI Chat Completions compatibility profile for `anthropic_claude_code_stealth_oauth`.
#[must_use]
pub const fn default_openai_compat() -> Option<OpenAiCompatConfig> {
    None
}

/// Build the default [`ProviderConfig`] for `anthropic_claude_code_stealth_oauth` (`F104`).
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
    #[doc = "Spec: PRV-1, PRV-2, PRV-3, API-1, API-3, F104 - verify preset TOML, URL builder, headers, and body normalizer"]
    fn regression_test_f104_missing_provider_anthropic_claude_code_s() {
        assert!(!PRESET_TOML.is_empty());
        assert!(PRESET_TOML.contains(PRESET_ID));
        assert!(!DEFAULT_MODELS.is_empty());

        let url = build_endpoint_url(DEFAULT_BASE_URL, EndpointKind::Chat);
        assert!(url.starts_with("http://") || url.starts_with("https://"));

        let headers = synthesize_headers("01JKEYTEST0000000000000000", "sess_test_1");
        assert!(!headers.contains_key("x-llmroller-preset"));
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
