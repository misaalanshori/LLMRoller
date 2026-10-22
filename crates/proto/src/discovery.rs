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

/// Extract upstream model identifiers from an OpenAI or Anthropic `/models` response.
///
/// Both protocols return `{"data": [{"id": "..."}]}`; a bare JSON array of
/// strings or objects is also accepted so custom providers stay usable.
pub fn parse_models_body(body: &[u8]) -> Result<Vec<String>, DiscoveryParseError> {
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

    Ok(array.iter().filter_map(model_identifier).collect())
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
}
