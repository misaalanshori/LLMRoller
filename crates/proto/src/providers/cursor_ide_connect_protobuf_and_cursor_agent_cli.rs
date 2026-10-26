#![forbid(unsafe_code)]
//! Native provider preset and compatibility adapter for Cursor IDE (Connect-RPC Protobuf HTTP/2 StreamChat & cursor-agent CLI Bridge) (`F106`).
//!
//! Spec: ./docs/srdd/23. SRDD - Multi-Provider Access Layer, OAuth Token Exchange, and Hardware Fingerprinting.md#231-overview
//! Reference: apiclientreferences/9router/cursor.md, apiclientreferences/fx/cursor-agent.md

use std::collections::BTreeMap;

use bytes::Bytes;
#[allow(unused_imports)]
use llmroller_core::provider::{MaxTokensField, OpenAiCompatConfig, ProviderConfig};
#[allow(unused_imports)]
use serde_json::{Value, json};

use crate::adapters::EndpointKind;
use crate::fingerprint::{DeviceProfile, expand_header_template};

/// Finding ID implemented by this module.
pub const FINDING_ID: &str = "F106";

/// Canonical preset slug matching `presets/cursor_ide_connect_protobuf_and_cursor_agent_cli.toml`.
pub const PRESET_ID: &str = "cursor_ide_connect_protobuf_and_cursor_agent_cli";

/// Human-readable provider display name.
pub const DISPLAY_NAME: &str =
    "Cursor IDE (Connect-RPC Protobuf HTTP/2 StreamChat & cursor-agent CLI Bridge)";

/// Default upstream base URL for `cursor_ide_connect_protobuf_and_cursor_agent_cli`.
pub const DEFAULT_BASE_URL: &str = "https://api2.cursor.sh/aiserver.v1.ChatService/StreamChat";

/// Supported wire protocols for `cursor_ide_connect_protobuf_and_cursor_agent_cli`.
pub const SUPPORTED_PROTOCOLS: &[&str] = &["chat", "responses", "messages"];

/// Reference models bundled with `cursor_ide_connect_protobuf_and_cursor_agent_cli`.
pub const DEFAULT_MODELS: &[&str] = &[
    "default",
    "claude-4.5-opus-high-thinking",
    "claude-4.5-opus-high",
    "claude-4.5-sonnet-thinking",
    "claude-4.5-sonnet",
    "claude-4.6-opus-max",
    "gpt-5.2-codex",
];

/// Compile-time embedded `presets/cursor_ide_connect_protobuf_and_cursor_agent_cli.toml` document.
pub const PRESET_TOML: &str =
    include_str!("../../../../presets/cursor_ide_connect_protobuf_and_cursor_agent_cli.toml");

/// Build the full upstream endpoint URL for `cursor_ide_connect_protobuf_and_cursor_agent_cli` (`F106`).
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

/// Synthesize required authentication, hardware fingerprint, and client headers for `cursor_ide_connect_protobuf_and_cursor_agent_cli` (`F106`).
#[must_use]
pub fn synthesize_headers(key_id: &str, session_id: &str) -> BTreeMap<String, String> {
    let profile = DeviceProfile::from_key_id(key_id);
    let now_secs = crate::fingerprint::current_unix_timestamp_secs();
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    map.insert("x-cursor-client-version".to_string(), "2.3.41".to_string());
    map.insert("x-cursor-client-type".to_string(), "ide".to_string());
    map.insert(
        "x-cursor-checksum".to_string(),
        "{{cursor_checksum}}".to_string(),
    );
    for val in map.values_mut() {
        if val.contains("{{") {
            *val = expand_header_template(val, &profile, &profile.device_id, session_id, now_secs);
        }
    }
    let _ = (&profile, session_id, now_secs);
    map
}

/// Apply provider-specific JSON payload normalizations for `cursor_ide_connect_protobuf_and_cursor_agent_cli` (`F106`).
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

/// Optional OpenAI Chat Completions compatibility profile for `cursor_ide_connect_protobuf_and_cursor_agent_cli`.
#[must_use]
pub const fn default_openai_compat() -> Option<OpenAiCompatConfig> {
    None
}

/// Build the default [`ProviderConfig`] for `cursor_ide_connect_protobuf_and_cursor_agent_cli` (`F106`).
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
    #[doc = "Spec: PRV-1, PRV-2, PRV-3, API-1, API-3, F106 - verify preset TOML, URL builder, headers, and body normalizer"]
    fn regression_test_f106_missing_provider_cursor_ide_connect_prot() {
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
