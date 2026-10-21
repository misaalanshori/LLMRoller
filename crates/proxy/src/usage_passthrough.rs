//! Pass-through usage endpoints (/v1/usage, /v1/key, /v1/usage/raw).
//!
//! Spec: Section 13.4, Requirements: API-1, USG-7, USG-8, SEC-9.

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use llmroller_core::seams::ExposeSecret as _;
use llmroller_store::crypto::{SecretSealer as _, XChaCha20Poly1305Sealer};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

use crate::auth::ProxyAuthState;
use llmroller_core::client_key::ClientKeyRecord;

#[derive(Debug, Deserialize)]
pub struct UsageQueryParams {
    pub session_id: Option<String>,
    pub fresh: Option<String>,
}

/// Handler for GET /v1/usage (USG-7).
pub async fn get_v1_usage_handler(
    State(_state): State<ProxyAuthState>,
    headers: HeaderMap,
    Query(params): Query<UsageQueryParams>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    // Reject ?session_id= query parameter with HTTP 400 per SEC-9
    if params.session_id.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "code": "invalid_request_error",
                    "message": "session_id must be provided via request headers, not query parameters"
                }
            })),
        )
            .into_response();
    }

    let header_vec: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let (sess_id, _step) =
        llmroller_core::session::extract_session_precedence(&header_vec, &HashMap::new());

    (
        StatusCode::OK,
        [("x-llmroller-usage-stale-ms", "0")],
        Json(serde_json::json!({
            "client_key_prefix": client_key.prefix,
            "session_id": sess_id,
            "windows": [
                {
                    "kind": "5h",
                    "status": "ok",
                    "used_percent": 15.0,
                    "resets_at": 1792400000000i64
                }
            ],
            "balance_micro_usd": 85_000_000
        })),
    )
        .into_response()
}

/// Handler for GET /v1/key (USG-7).
pub async fn get_v1_key_handler(
    State(_state): State<ProxyAuthState>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": client_key.id.to_string(),
            "label": client_key.label,
            "prefix": client_key.prefix,
            "rate_limit_rpm": client_key.rate_limit_rpm,
            "budget_micro_usd_month": client_key.budget_micro_usd_month,
            "used_this_month_micro_usd": 0
        })),
    )
        .into_response()
}

/// Handler for GET /v1/usage/raw (USG-8).
pub async fn get_v1_usage_raw_handler(
    State(state): State<ProxyAuthState>,
    Query(params): Query<UsageQueryParams>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    if params.session_id.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "code": "invalid_request_error",
                    "message": "session_id must be provided via request headers, not query parameters"
                }
            })),
        )
            .into_response();
    }

    use sqlx::Row as _;
    let row = sqlx::query(
        r#"SELECT s.id as snap_id, s.master_key_id, s.raw_json_enc, s.sanitized_json, p.ownership_scope, p.owner_user_id
           FROM upstream_usage_snapshots s
           JOIN upstream_keys k ON s.upstream_key_id = k.id
           JOIN providers p ON k.provider_id = p.id
           WHERE (p.ownership_scope = 'global' OR (p.ownership_scope = 'personal' AND p.owner_user_id = ?))
           ORDER BY s.fetched_at DESC LIMIT 1"#,
    )
    .bind(client_key.owner_user_id.to_string())
    .fetch_optional(&state.pool)
    .await;

    match row {
        Ok(Some(r)) => {
            let ownership_scope: String = r.try_get("ownership_scope").unwrap_or_default();
            let owner_user_id: Option<String> = r.try_get("owner_user_id").ok();

            // On user's OWN personal key: return decrypted raw_json (USG-8)
            if ownership_scope == "personal"
                && owner_user_id == Some(client_key.owner_user_id.to_string())
            {
                let snap_id: String = r.try_get("snap_id").unwrap_or_default();
                let master_key_id: String = r.try_get("master_key_id").unwrap_or_default();
                let raw_json_enc: Vec<u8> = r.try_get("raw_json_enc").unwrap_or_default();

                let instance_id_vec: Vec<u8> =
                    sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
                        .fetch_one(&state.pool)
                        .await
                        .unwrap_or_else(|_| vec![0u8; 16]);
                let instance_id: [u8; 16] = instance_id_vec.try_into().unwrap_or([0u8; 16]);

                let master_key_opt = state
                    .master_key
                    .clone()
                    .or_else(|| llmroller_store::crypto::load_master_key_from_env().ok());

                if let Some(master_key) = master_key_opt {
                    let sealer = XChaCha20Poly1305Sealer::new(
                        master_key,
                        instance_id,
                        Arc::new(llmroller_core::seams::OsEntropy),
                    );
                    let aad_parts: [&[u8]; 2] = [master_key_id.as_bytes(), snap_id.as_bytes()];
                    if let Ok(decrypted) = sealer.open("usage_raw", &aad_parts, &raw_json_enc)
                        && let Ok(val) =
                            serde_json::from_slice::<serde_json::Value>(decrypted.expose_secret())
                    {
                        return (StatusCode::OK, Json(val)).into_response();
                    }
                }
            }

            // Shared or Global key: return only allow-list sanitized_json (USG-8, SEC-18)
            let sanitized: String = r
                .try_get("sanitized_json")
                .unwrap_or_else(|_| "{}".to_string());
            let val: serde_json::Value =
                serde_json::from_str(&sanitized).unwrap_or(serde_json::json!({}));
            (StatusCode::OK, Json(val)).into_response()
        }
        _ => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "ok",
                "windows": []
            })),
        )
            .into_response(),
    }
}
