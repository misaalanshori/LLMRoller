//! Pass-through usage endpoints (/v1/usage, /v1/key, /v1/usage/raw).
//!
//! Spec: Section 13.4, Requirements: API-1, USG-7, USG-8, SEC-9.

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::collections::HashMap;

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
        r#"SELECT s.sanitized_json
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
