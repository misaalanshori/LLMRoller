#![forbid(unsafe_code)]
//! Upstream `/models` discovery payload parsing and trigger messages.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#66-model-discovery-catalog-and-merging-mdl
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#74-portability-maintainability-observability-and-privacy-por-mnt-obs-priv

use http::{HeaderName, HeaderValue, header};
use llmroller_core::ids::UpstreamKeyId;
use llmroller_core::provider::{ProviderConfig, ProviderEndpoint, ProviderKind};
use serde_json::Value;

use crate::adapters::{EndpointKind, get_adapter};

/// Maximum accepted upstream `/models` response size before discovery is `partial` (MDL-1).
pub const MAX_DISCOVERY_BYTES: usize = 2 * 1024 * 1024;

/// A request to refresh the discovered model catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryTarget {
    /// Discover models for one upstream key, for example immediately after key creation.
    Key(UpstreamKeyId),
    /// Discover models for every enabled upstream key, used by manual admin refresh.
    AllEnabled,
}

/// Failure modes when decoding an upstream `/models` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryParseError {
    /// The response body was not valid JSON.
    #[error("upstream /models response is not valid JSON")]
    InvalidJson,
    /// The JSON did not contain a `data` or `models` array.
    #[error("upstream /models response has no model array")]
    MissingModelArray,
}

/// Per-model token pricing extracted from an upstream `/models` payload (in micro-USD per MTok; `MDL-1`, `USG-2`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredPricing {
    pub input_micro_usd_per_mtok: i64,
    pub output_micro_usd_per_mtok: i64,
    pub cache_read_micro_usd_per_mtok: i64,
    pub cache_write_micro_usd_per_mtok: i64,
    pub reasoning_micro_usd_per_mtok: Option<i64>,
}

/// Rich model descriptor extracted from an upstream `/models` response (`F-015`, `MDL-1`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub canonical_slug: Option<String>,
    pub context_window: Option<i64>,
    pub discovered_pricing: Option<DiscoveredPricing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_protocols: Option<Vec<String>>,
}

fn parse_usd_per_token_to_micro_per_mtok(val: Option<&Value>) -> Option<i64> {
    let raw_f64 = match val? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !raw_f64.is_finite() || raw_f64 < 0.0 {
        return None;
    }
    // OpenRouter PublicPricing reports USD per single token (e.g. "0.000003" = $3/MTok = 3_000_000 micro-USD/MTok).
    Some((raw_f64 * 1_000_000_000_000.0).round() as i64)
}

fn parse_discovered_model(value: &Value) -> Option<DiscoveredModel> {
    let id = model_identifier(value)?;
    let Value::Object(obj) = value else {
        return Some(DiscoveredModel {
            id,
            canonical_slug: None,
            context_window: None,
            discovered_pricing: None,
            supported_protocols: None,
        });
    };
    // Skip models explicitly marked disabled by the upstream catalog (`F020`, e.g. OpenDesign AMR)
    if obj.get("enabled").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let canonical_slug = obj
        .get("canonical_slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let context_window = obj
        .get("context_length")
        .or_else(|| obj.get("context_window"))
        .or_else(|| obj.get("max_context_length"))
        .or_else(|| obj.get("metadata").and_then(|m| m.get("context_limit")))
        .or_else(|| obj.get("metadata").and_then(|m| m.get("context_length")))
        .or_else(|| obj.get("top_provider").and_then(|tp| tp.get("context_length")))
        .and_then(Value::as_i64)
        .filter(|&n| n > 0);
    let supported_protocols = obj
        .get("supported_endpoints")
        .or_else(|| obj.get("supported_protocols"))
        .or_else(|| obj.get("endpoints"))
        .and_then(Value::as_array)
        .map(|arr| {
            let mut protos = Vec::new();
            for item in arr.iter().filter_map(Value::as_str) {
                let lower = item.trim().to_ascii_lowercase();
                let mapped = if lower.contains("chat") {
                    Some("chat")
                } else if lower.contains("response") {
                    Some("responses")
                } else if lower.contains("message") || lower.contains("anthropic") {
                    Some("messages")
                } else {
                    None
                };
                if let Some(p) = mapped
                    && !protos.iter().any(|existing| existing == p)
                {
                    protos.push(p.to_string());
                }
            }
            protos
        })
        .filter(|v| !v.is_empty());
    let discovered_pricing = obj.get("pricing").and_then(|pricing| {
        let input = parse_usd_per_token_to_micro_per_mtok(pricing.get("prompt"))?;
        let output = parse_usd_per_token_to_micro_per_mtok(pricing.get("completion"))?;
        let cache_read =
            parse_usd_per_token_to_micro_per_mtok(pricing.get("input_cache_read")).unwrap_or(0);
        let cache_write =
            parse_usd_per_token_to_micro_per_mtok(pricing.get("input_cache_write")).unwrap_or(0);
        let reasoning = parse_usd_per_token_to_micro_per_mtok(pricing.get("internal_reasoning"));
        Some(DiscoveredPricing {
            input_micro_usd_per_mtok: input,
            output_micro_usd_per_mtok: output,
            cache_read_micro_usd_per_mtok: cache_read,
            cache_write_micro_usd_per_mtok: cache_write,
            reasoning_micro_usd_per_mtok: reasoning,
        })
    });
    Some(DiscoveredModel {
        id,
        canonical_slug,
        context_window,
        discovered_pricing,
        supported_protocols,
    })
}

/// Extract rich model descriptors (including OpenRouter `pricing` and `context_length`) from `/models` (`F-015`).
pub fn parse_models_response(body: &[u8]) -> Result<Vec<DiscoveredModel>, DiscoveryParseError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| DiscoveryParseError::InvalidJson)?;
    let array = match value {
        Value::Array(items) => items,
        Value::Object(mut object) => {
            let data = object
                .remove("data")
                .or_else(|| object.remove("models"))
                .ok_or(DiscoveryParseError::MissingModelArray)?;
            match data {
                Value::Array(items) => items,
                _ => return Err(DiscoveryParseError::MissingModelArray),
            }
        }
        _ => return Err(DiscoveryParseError::MissingModelArray),
    };

    Ok(array.iter().filter_map(parse_discovered_model).collect())
}

/// Extract upstream model identifiers from an OpenAI or Anthropic `/models` response.
///
/// Both protocols return `{"data": [{"id": "..."}]}`; a bare JSON array of
/// strings or objects is also accepted so custom providers stay usable.
pub fn parse_models_body(body: &[u8]) -> Result<Vec<String>, DiscoveryParseError> {
    Ok(parse_models_response(body)?
        .into_iter()
        .map(|m| m.id)
        .collect())
}

fn model_identifier(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => Some(id.clone()),
        Value::Object(object) => object
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                object
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
        _ => None,
    }
}

/// Resolve the discovery URL from provider configuration, falling back to the adapter preset.
#[must_use]
pub fn discovery_endpoint(kind: ProviderKind, base_url: &str, config: &ProviderConfig) -> String {
    if let Some(path) = config
        .paths
        .for_endpoint(ProviderEndpoint::Models)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        if path.starts_with("http://") || path.starts_with("https://") {
            return path.to_string();
        }
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        return format!("{}{path}", base_url.trim_end_matches('/'));
    }
    get_adapter(kind).build_endpoint_url(base_url, EndpointKind::Models)
}

/// Build the outbound headers for a discovery request using the provider profile.
///
/// The configured `auth.models` scheme wins over `auth.default`, matching the
/// data-plane credential injection rules (`PRV-3`).
#[must_use]
pub fn discovery_headers(config: &ProviderConfig, secret: &str) -> http::HeaderMap {
    let scheme = config.auth.for_endpoint(ProviderEndpoint::Models);
    let (auth_header, auth_prefix) = scheme
        .as_ref()
        .map_or(("authorization", "Bearer "), |scheme| {
            (scheme.header.as_str(), scheme.prefix.as_str())
        });

    let mut headers = http::HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));

    for (name, value) in &config.add_headers {
        let lower = name.to_ascii_lowercase();
        if lower == "host" || lower == "content-length" || lower == auth_header {
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            tracing::warn!(header = %name, "ignoring invalid provider add_headers name");
            continue;
        };
        let Ok(value) = HeaderValue::from_str(value) else {
            tracing::warn!(header = %name, "ignoring invalid provider add_headers value");
            continue;
        };
        headers.insert(name, value);
    }

    let auth_value = format!("{auth_prefix}{secret}");
    if let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(auth_header.as_bytes()),
        HeaderValue::from_str(&auth_value),
    ) {
        headers.insert(name, value);
    }

    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: MDL-1 — parses OpenAI and Anthropic /models list envelopes"]
    fn parses_openai_and_anthropic_list_shapes() {
        let openai = br#"{"object":"list","data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]}"#;
        assert_eq!(
            parse_models_body(openai).expect("openai list"),
            vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()]
        );

        let anthropic = br#"{"data":[{"id":"claude-3-5-sonnet","type":"model"}],"has_more":false}"#;
        assert_eq!(
            parse_models_body(anthropic).expect("anthropic list"),
            vec!["claude-3-5-sonnet".to_string()]
        );
    }

    #[test]
    #[doc = "Spec: MDL-1 — accepts bare arrays and alternate model keys for custom providers"]
    fn parses_bare_arrays_and_names() {
        let bare = br#"["model-a",{"name":"model-b"},{"id":"model-c"}]"#;
        assert_eq!(
            parse_models_body(bare).expect("bare array"),
            vec![
                "model-a".to_string(),
                "model-b".to_string(),
                "model-c".to_string()
            ]
        );
        let alternate = br#"{"models":[{"id":"model-d"}]}"#;
        assert_eq!(
            parse_models_body(alternate).expect("models key"),
            vec!["model-d".to_string()]
        );
    }

    #[test]
    #[doc = "Spec: MDL-1 — rejects malformed discovery payloads instead of clearing the catalog"]
    fn rejects_malformed_payloads() {
        assert_eq!(
            parse_models_body(b"not json"),
            Err(DiscoveryParseError::InvalidJson)
        );
        assert_eq!(
            parse_models_body(br#"{"object":"list"}"#),
            Err(DiscoveryParseError::MissingModelArray)
        );
        assert_eq!(
            parse_models_body(br#"{"data":42}"#),
            Err(DiscoveryParseError::MissingModelArray)
        );
    }

    #[test]
    #[doc = "Spec: PRV-3, MDL-1 — honors configured models path and auth scheme"]
    fn uses_configured_models_path_and_auth() {
        let config = ProviderConfig::from_json(
            r#"{
                "paths":{"models":"vendor/models"},
                "auth":{"models":{"header":"x-api-key","prefix":""}},
                "add_headers":{"x-tenant":"acme"}
            }"#,
        );
        assert_eq!(
            discovery_endpoint(ProviderKind::Custom, "https://api.example/v1/", &config),
            "https://api.example/v1/vendor/models"
        );
        let headers = discovery_headers(&config, "sk-secret");
        assert_eq!(
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("sk-secret")
        );
        assert_eq!(
            headers
                .get("x-tenant")
                .and_then(|value| value.to_str().ok()),
            Some("acme")
        );
        assert!(!headers.contains_key(header::AUTHORIZATION));
    }

    #[test]
    #[doc = "Spec: MDL-1 — falls back to the adapter preset /models URL"]
    fn falls_back_to_adapter_models_endpoint() {
        let config = ProviderConfig::default();
        assert_eq!(
            discovery_endpoint(
                ProviderKind::Openrouter,
                "https://openrouter.ai/api/v1",
                &config
            ),
            "https://openrouter.ai/api/v1/models"
        );
        let headers = discovery_headers(&config, "sk-secret");
        assert_eq!(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer sk-secret")
        );
    }

    #[test]
    #[doc = "Spec: MDL-1, USG-2"]
    fn discovery_parses_openrouter_public_pricing_fields() {
        let raw = br#"{"data":[{"id":"anthropic/claude-sonnet-4.6","canonical_slug":"anthropic/claude-4.6-sonnet","context_length":200000,"pricing":{"prompt":"0.000003","completion":"0.000015","input_cache_read":"0.0000003","input_cache_write":"0.00000375","internal_reasoning":"0.000015"}}]}"#;
        let models = parse_models_response(raw).expect("openrouter models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "anthropic/claude-sonnet-4.6");
        assert_eq!(
            models[0].canonical_slug.as_deref(),
            Some("anthropic/claude-4.6-sonnet")
        );
        assert_eq!(models[0].context_window, Some(200_000));
        let pricing = models[0]
            .discovered_pricing
            .as_ref()
            .expect("discovered pricing");
        assert_eq!(pricing.input_micro_usd_per_mtok, 3_000_000);
        assert_eq!(pricing.output_micro_usd_per_mtok, 15_000_000);
        assert_eq!(pricing.cache_read_micro_usd_per_mtok, 300_000);
        assert_eq!(pricing.cache_write_micro_usd_per_mtok, 3_750_000);
        assert_eq!(pricing.reasoning_micro_usd_per_mtok, Some(15_000_000));
    }

    #[test]
    #[doc = "Spec: MDL-1, F020 - filters enabled:false models and extracts metadata.context_limit and supported_endpoints"]
    fn discovery_filters_disabled_models_and_extracts_nested_context_limit() {
        let raw = br#"{
            "data": [
                {"id": "claude-sonnet-4-6", "enabled": true, "metadata": {"context_limit": 200000}, "supported_endpoints": ["/v1/messages"]},
                {"id": "disabled-beta-model", "enabled": false, "metadata": {"context_limit": 128000}},
                {"id": "openai/gpt-5.4", "top_provider": {"context_length": 400000}, "supported_protocols": ["chat", "responses"]}
            ]
        }"#;
        let models = parse_models_response(raw).expect("parse models");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "claude-sonnet-4-6");
        assert_eq!(models[0].context_window, Some(200_000));
        assert_eq!(
            models[0].supported_protocols.as_deref(),
            Some(&["messages".to_string()][..])
        );
        assert_eq!(models[1].id, "openai/gpt-5.4");
        assert_eq!(models[1].context_window, Some(400_000));
        assert_eq!(
            models[1].supported_protocols.as_deref(),
            Some(&["chat".to_string(), "responses".to_string()][..])
        );
    }

    #[test]
    #[doc = "Spec: PRV-2, MDL-1, F097 - open_design_amr preset anthropic-version header and discovery parser"]
    fn regression_test_f097_open_design_amr_preset_missing_anthropic() {
        let toml = ProviderKind::OpenDesignAmr.preset_toml();
        assert!(toml.contains("\"anthropic-version\" = \"2023-06-01\""));
        let raw = br#"{"data":[{"id":"claude-opus-4-6","enabled":true,"metadata":{"context_limit":200000,"output_limit":16384}},{"id":"claude-disabled","enabled":false,"metadata":{"context_limit":100000}}]}"#;
        let models = parse_models_response(raw).expect("amr models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-opus-4-6");
        assert_eq!(models[0].context_window, Some(200_000));
    }
}
