#![forbid(unsafe_code)]
//! OAuth2 refresh-token exchange and GCP/Vertex JWT assertion token manager (`F069`, `PRV-3`).
//!
//! Spec: ./docs/srdd/2. SRDD - Supported Providers and Wire Protocols.md#21-supported-provider-presets
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#64-upstream-providers-and-keys-prv

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use llmroller_core::ids::UpstreamKeyId;
use serde::{Deserialize, Serialize};

/// Minimum remaining lifetime (`300_000 ms` = 5 minutes) before a cached OAuth2 / JWT
/// access token is proactively refreshed (`F069`).
pub const TOKEN_REFRESH_LEAD_MS: i64 = 300_000;

/// Default lifetime (`3_600_000 ms` = 1 hour) when an upstream token response omits `expires_in`.
pub const DEFAULT_TOKEN_TTL_MS: i64 = 3_600_000;

/// Structured OAuth2 / Service Account credential envelope stored inside an upstream key's
/// decrypted secret when the key uses dynamic token exchange instead of a static API key (`F069`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicCredentialSpec {
    /// Credential type: `"oauth2"` (`refresh_token` grant) or `"jwt_bearer"` (RFC 7523 assertion).
    #[serde(rename = "type")]
    pub kind: String,
    /// Static or initial access token, if already provisioned.
    #[serde(default)]
    pub access_token: Option<String>,
    /// Optional expiration epoch milliseconds for `access_token`.
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    /// OAuth2 refresh token used with `grant_type=refresh_token`.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// OAuth2 client identifier.
    #[serde(default)]
    pub client_id: Option<String>,
    /// OAuth2 client secret.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Token endpoint URL (e.g. `https://oauth2.googleapis.com/token`).
    #[serde(default)]
    pub token_url: Option<String>,
    /// Pre-signed or service-account assertion for `urn:ietf:params:oauth:grant-type:jwt-bearer`.
    #[serde(default)]
    pub assertion: Option<String>,
    /// Optional OAuth2 scope string.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Cached short-lived access token entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedAccessToken {
    pub access_token: String,
    pub expires_at_ms: i64,
}

impl CachedAccessToken {
    /// Whether this cached token remains valid with at least [`TOKEN_REFRESH_LEAD_MS`] headroom.
    #[must_use]
    pub const fn is_fresh(&self, now_ms: i64) -> bool {
        now_ms.saturating_add(TOKEN_REFRESH_LEAD_MS) < self.expires_at_ms
    }
}

/// In-memory concurrent cache and exchanger for OAuth2 and Service Account access tokens (`F069`).
#[derive(Debug, Default, Clone)]
pub struct TokenManager {
    cache: Arc<DashMap<UpstreamKeyId, CachedAccessToken>>,
}

impl TokenManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: Arc::new(DashMap::new()),
        }
    }

    /// Parses a decrypted key secret as a [`DynamicCredentialSpec`] when it starts with `{` and
    /// declares `"type": "oauth2"` or `"type": "jwt_bearer"`. Plaintext API keys return `None`.
    #[must_use]
    pub fn parse_dynamic_spec(raw_secret: &str) -> Option<DynamicCredentialSpec> {
        let trimmed = raw_secret.trim();
        if !trimmed.starts_with('{') {
            return None;
        }
        let spec: DynamicCredentialSpec = serde_json::from_str(trimmed).ok()?;
        if matches!(
            spec.kind.as_str(),
            "oauth2" | "oauth" | "jwt_bearer" | "service_account"
        ) {
            Some(spec)
        } else {
            None
        }
    }

    /// Inserts a cached access token directly (used by tests and pre-warmed OAuth flows).
    pub fn put_cached(&self, key_id: UpstreamKeyId, access_token: String, expires_at_ms: i64) {
        self.cache.insert(
            key_id,
            CachedAccessToken {
                access_token,
                expires_at_ms,
            },
        );
    }

    /// Evicts any cached token for `key_id`.
    pub fn invalidate(&self, key_id: &UpstreamKeyId) {
        self.cache.remove(key_id);
    }

    /// Resolves a bearer/header secret for `key_id`:
    /// - For standard static API keys, returns `raw_secret` with zero allocations or network calls.
    /// - For JSON OAuth2 / Service Account credentials, returns a cached valid `access_token` or
    ///   exchanges the `refresh_token` / `assertion` at `token_url` (`F069`).
    pub async fn resolve_secret(
        &self,
        key_id: UpstreamKeyId,
        raw_secret: &str,
        client: &reqwest::Client,
        allow_private_network: bool,
        now_ms: i64,
    ) -> Result<String, String> {
        let Some(spec) = Self::parse_dynamic_spec(raw_secret) else {
            return Ok(raw_secret.to_string());
        };

        if let Some(cached) = self.cache.get(&key_id)
            && cached.is_fresh(now_ms)
        {
            return Ok(cached.access_token.clone());
        }

        if let Some(ref inline_token) = spec.access_token
            && !inline_token.is_empty()
            && spec
                .expires_at_ms
                .is_none_or(|exp| now_ms.saturating_add(TOKEN_REFRESH_LEAD_MS) < exp)
        {
            let expires_at = spec
                .expires_at_ms
                .unwrap_or_else(|| now_ms.saturating_add(DEFAULT_TOKEN_TTL_MS));
            self.put_cached(key_id, inline_token.clone(), expires_at);
            return Ok(inline_token.clone());
        }

        let token_url = spec
            .token_url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .ok_or_else(|| "dynamic credential missing token_url".to_string())?;
        crate::ssrf::validate_url(token_url, allow_private_network)
            .map_err(|_| "token_url rejected by SSRF guard".to_string())?;

        let mut form: Vec<(&str, &str)> = Vec::new();
        if let Some(ref refresh_token) = spec.refresh_token {
            form.push(("grant_type", "refresh_token"));
            form.push(("refresh_token", refresh_token.as_str()));
            if let Some(ref cid) = spec.client_id {
                form.push(("client_id", cid.as_str()));
            }
            if let Some(ref csec) = spec.client_secret {
                form.push(("client_secret", csec.as_str()));
            }
        } else if let Some(ref assertion) = spec.assertion {
            form.push(("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"));
            form.push(("assertion", assertion.as_str()));
        } else {
            return Err("dynamic credential requires refresh_token or assertion".to_string());
        }
        if let Some(ref scope) = spec.scope {
            form.push(("scope", scope.as_str()));
        }

        let form_body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();

        let response = client
            .post(token_url)
            .header(
                http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form_body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("token exchange request failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "token exchange returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("invalid token exchange JSON: {e}"))?;
        let access_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "token response missing access_token".to_string())?
            .to_string();
        let ttl_ms = body
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .filter(|&s| s > 0)
            .map_or(DEFAULT_TOKEN_TTL_MS, |s| s.saturating_mul(1000));
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        self.put_cached(key_id, access_token.clone(), expires_at_ms);
        crate::metrics::record_oauth_refresh("refreshed");
        Ok(access_token)
    }

    /// Resolves a dynamic OAuth2 / JWT secret and, when a token exchange occurs, seals and
    /// persists the refreshed `access_token` (and any rotated `refresh_token`) back to SQLite (`FND-039`).
    #[allow(clippy::too_many_arguments)]
    pub async fn resolve_and_persist_secret(
        &self,
        key_id: UpstreamKeyId,
        provider_id: llmroller_core::ids::ProviderId,
        raw_secret: &str,
        client: &reqwest::Client,
        allow_private_network: bool,
        now_ms: i64,
        writer: Option<&llmroller_store::writer::WriterHandle>,
        master_key: Option<&llmroller_store::crypto::MasterKey>,
        master_key_id: Option<&str>,
        instance_id: Option<[u8; 16]>,
        entropy: &Arc<dyn llmroller_core::seams::EntropySource>,
    ) -> Result<String, String> {
        let Some(mut spec) = Self::parse_dynamic_spec(raw_secret) else {
            return Ok(raw_secret.to_string());
        };

        if let Some(cached) = self.cache.get(&key_id)
            && cached.is_fresh(now_ms)
        {
            return Ok(cached.access_token.clone());
        }

        if let Some(ref inline_token) = spec.access_token
            && !inline_token.is_empty()
            && spec
                .expires_at_ms
                .is_none_or(|exp| now_ms.saturating_add(TOKEN_REFRESH_LEAD_MS) < exp)
        {
            let expires_at = spec
                .expires_at_ms
                .unwrap_or_else(|| now_ms.saturating_add(DEFAULT_TOKEN_TTL_MS));
            self.put_cached(key_id, inline_token.clone(), expires_at);
            return Ok(inline_token.clone());
        }

        let token_url = spec
            .token_url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .ok_or_else(|| "dynamic credential missing token_url".to_string())?;
        crate::ssrf::validate_url(token_url, allow_private_network)
            .map_err(|_| "token_url rejected by SSRF guard".to_string())?;

        let mut form: Vec<(&str, &str)> = Vec::new();
        if let Some(ref refresh_token) = spec.refresh_token {
            form.push(("grant_type", "refresh_token"));
            form.push(("refresh_token", refresh_token.as_str()));
            if let Some(ref cid) = spec.client_id {
                form.push(("client_id", cid.as_str()));
            }
            if let Some(ref csec) = spec.client_secret {
                form.push(("client_secret", csec.as_str()));
            }
        } else if let Some(ref assertion) = spec.assertion {
            form.push(("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"));
            form.push(("assertion", assertion.as_str()));
        } else {
            return Err("dynamic credential requires refresh_token or assertion".to_string());
        }
        if let Some(ref scope) = spec.scope {
            form.push(("scope", scope.as_str()));
        }

        let form_body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();

        let response = client
            .post(token_url)
            .header(
                http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form_body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("token exchange request failed: {e}"))?;
        if !response.status().is_success() {
            crate::metrics::record_oauth_refresh("failed");
            return Err(format!(
                "token exchange returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("invalid token exchange JSON: {e}"))?;
        let access_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "token response missing access_token".to_string())?
            .to_string();
        let ttl_ms = body
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .filter(|&s| s > 0)
            .map_or(DEFAULT_TOKEN_TTL_MS, |s| s.saturating_mul(1000));
        let expires_at_ms = now_ms.saturating_add(ttl_ms);

        // Update spec with new access_token, expires_at_ms, and optional rotated refresh_token (FND-039)
        spec.access_token = Some(access_token.clone());
        spec.expires_at_ms = Some(expires_at_ms);
        if let Some(new_refresh) = body
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            spec.refresh_token = Some(new_refresh.to_string());
        }

        self.put_cached(key_id, access_token.clone(), expires_at_ms);
        crate::metrics::record_oauth_refresh("refreshed");

        if let (Some(writer), Some(mk), Some(mk_id), Some(inst_id)) =
            (writer, master_key, master_key_id, instance_id)
            && let Ok(updated_json) = serde_json::to_string(&spec)
        {
            use llmroller_store::crypto::SecretSealer as _;
            let sealer = llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
                mk.clone(),
                inst_id,
                Arc::clone(entropy),
            );
            let key_str = key_id.to_string();
            let prov_str = provider_id.to_string();
            if let Ok(sealed) = sealer.seal(
                "upstream_key",
                &[mk_id.as_bytes(), key_str.as_bytes(), prov_str.as_bytes()],
                updated_json.as_bytes(),
            ) {
                let last4: String = access_token
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                writer.queues.durable_state.lock().insert(
                    llmroller_store::writer::DurableKey::UpstreamKeySecretUpdate(key_str.clone()),
                    llmroller_store::writer::DurableOp::UpstreamKeySecretUpdate {
                        upstream_key_id: key_str,
                        secret_enc: sealed,
                        master_key_id: mk_id.to_string(),
                        secret_last4: if last4.is_empty() {
                            "oath".to_string()
                        } else {
                            last4
                        },
                    },
                );
            }
        }

        Ok(access_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[doc = "Spec: PRV-3, SEC-1; F069 dynamic OAuth2 and service-account token caching and refresh"]
    async fn token_manager_passes_static_keys_and_caches_dynamic_oauth_tokens() {
        let mgr = TokenManager::new();
        let key_id = UpstreamKeyId::generate();
        let client = reqwest::Client::new();

        // 1. Plain static API key passes through unchanged
        let plain = mgr
            .resolve_secret(key_id, "sk-static-key-123", &client, false, 1_000)
            .await
            .expect("static key");
        assert_eq!(plain, "sk-static-key-123");

        // 2. Dynamic OAuth2 spec with fresh inline access_token is cached and returned
        let oauth_json =
            r#"{"type":"oauth2","access_token":"ya29.fresh-token","expires_at_ms":900000}"#;
        let resolved = mgr
            .resolve_secret(key_id, oauth_json, &client, false, 100_000)
            .await
            .expect("inline oauth token");
        assert_eq!(resolved, "ya29.fresh-token");

        // 3. Cached token is served while > 5 min before expiry
        mgr.put_cached(key_id, "ya29.cached-v2".to_string(), 1_000_000);
        let from_cache = mgr
            .resolve_secret(key_id, oauth_json, &client, false, 600_000)
            .await
            .expect("cached token");
        assert_eq!(from_cache, "ya29.cached-v2");
    }
}
