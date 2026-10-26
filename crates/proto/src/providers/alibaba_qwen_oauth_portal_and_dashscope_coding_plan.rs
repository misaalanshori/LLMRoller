#![forbid(unsafe_code)]
//! Native provider preset and compatibility adapter for Alibaba Qwen Code OAuth (`portal.qwen.ai`) & DashScope Coding Plan (`coding.dashscope.aliyuncs.com` / `dashscope-intl.aliyuncs.com`) (`F113`).
//!
//! Spec: ./docs/srdd/23. SRDD - Multi-Provider Access Layer, OAuth Token Exchange, and Hardware Fingerprinting.md#231-overview
//! Reference: apiclientreferences/9router/qwen.md, apiclientreferences/opencode/alibaba.md

use std::collections::BTreeMap;

use bytes::Bytes;
#[allow(unused_imports)]
use llmroller_core::provider::{MaxTokensField, OpenAiCompatConfig, ProviderConfig};
#[allow(unused_imports)]
use serde_json::{Value, json};

use crate::adapters::EndpointKind;
use crate::fingerprint::{DeviceProfile, expand_header_template};

/// Finding ID implemented by this module.
pub const FINDING_ID: &str = "F113";

/// Canonical preset slug matching `presets/alibaba_qwen_oauth_portal_and_dashscope_coding_plan.toml`.
pub const PRESET_ID: &str = "alibaba_qwen_oauth_portal_and_dashscope_coding_plan";

/// Human-readable provider display name.
pub const DISPLAY_NAME: &str = "Alibaba Qwen Code OAuth (`portal.qwen.ai`) & DashScope Coding Plan (`coding.dashscope.aliyuncs.com` / `dashscope-intl.aliyuncs.com`)";

/// Default upstream base URL for `alibaba_qwen_oauth_portal_and_dashscope_coding_plan`.
pub const DEFAULT_BASE_URL: &str = "https://portal.qwen.ai/v1/chat/completions";

/// Supported wire protocols for `alibaba_qwen_oauth_portal_and_dashscope_coding_plan`.
pub const SUPPORTED_PROTOCOLS: &[&str] = &["chat", "responses", "messages"];

/// Reference models bundled with `alibaba_qwen_oauth_portal_and_dashscope_coding_plan`.
pub const DEFAULT_MODELS: &[&str] = &[
    "qwen3-coder-plus",
    "qwen3-coder-flash",
    "qwen3-coder-next",
    "qwen3.5-plus",
    "qwen3.6-plus",
    "qwq-32b",
];

/// Compile-time embedded `presets/alibaba_qwen_oauth_portal_and_dashscope_coding_plan.toml` document.
pub const PRESET_TOML: &str =
    include_str!("../../../../presets/alibaba_qwen_oauth_portal_and_dashscope_coding_plan.toml");

/// Build the full upstream endpoint URL for `alibaba_qwen_oauth_portal_and_dashscope_coding_plan` (`F113`).
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

/// Synthesize required authentication, hardware fingerprint, and client headers for `alibaba_qwen_oauth_portal_and_dashscope_coding_plan` (`F113`).
#[must_use]
pub fn synthesize_headers(key_id: &str, session_id: &str) -> BTreeMap<String, String> {
    let profile = DeviceProfile::from_key_id(key_id);
    let now_secs = crate::fingerprint::current_unix_timestamp_secs();
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    map.insert(
        "user-agent".to_string(),
        "qwen-code/0.1.0 ({{os}}/{{arch}})".to_string(),
    );
    for val in map.values_mut() {
        if val.contains("{{") {
            *val = expand_header_template(val, &profile, &profile.device_id, session_id, now_secs);
        }
    }
    let _ = (&profile, session_id, now_secs);
    map
}

/// Apply provider-specific JSON payload normalizations for `alibaba_qwen_oauth_portal_and_dashscope_coding_plan` (`F113`).
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

/// Optional OpenAI Chat Completions compatibility profile for `alibaba_qwen_oauth_portal_and_dashscope_coding_plan`.
#[must_use]
pub const fn default_openai_compat() -> Option<OpenAiCompatConfig> {
    None
}

/// Build the default [`ProviderConfig`] for `alibaba_qwen_oauth_portal_and_dashscope_coding_plan` (`F113`).
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
    #[doc = "Spec: PRV-1, PRV-2, PRV-3, API-1, API-3, F113 - verify preset TOML, URL builder, headers, and body normalizer"]
    fn regression_test_f113_missing_provider_alibaba_qwen_oauth_port() {
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
