//! Pass-through usage endpoints (/v1/usage, /v1/key, /v1/usage/raw).
//!
//! Spec: Section 13.4, Requirements: API-1, USG-7, USG-8, SEC-9.

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use llmroller_core::accumulators::BudgetScopeId;
use llmroller_core::seams::{Clock as _, ExposeSecret as _, SystemClock};
use llmroller_core::snapshot::{ConfigSnapshot, UpstreamKeyRecord};
use llmroller_store::crypto::{SecretSealer as _, XChaCha20Poly1305Sealer};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

use crate::auth::ProxyAuthState;
use llmroller_core::client_key::ClientKeyRecord;
use llmroller_core::windows::BudgetCounter as _;

#[derive(Debug, Deserialize)]
pub struct UsageQueryParams {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub upstream_key_id: Option<String>,
    pub fresh: Option<String>,
}

fn not_found(key_id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": {
                "code": "not_found_error",
                "message": format!("upstream key '{key_id}' not found")
            }
        })),
    )
        .into_response()
}

/// Whether the caller may inspect usage for this upstream key (SEC-12).
fn accessible_key<'a>(
    snapshot: &'a ConfigSnapshot,
    key_id: &llmroller_core::ids::UpstreamKeyId,
    caller: &ClientKeyRecord,
) -> Option<&'a UpstreamKeyRecord> {
    let key = snapshot.upstream_keys.get(key_id)?;
    let provider = snapshot.providers.get(&key.provider_id)?;
    match provider.ownership_scope {
        llmroller_core::rbac::OwnershipScope::Global => Some(key),
        llmroller_core::rbac::OwnershipScope::Personal => {
            (provider.owner_user_id == Some(caller.owner_user_id)).then_some(key)
        }
    }
}

/// Resolve the upstream key whose windows a usage response should describe.
/// Requested upstream key or model identifier that is not visible to the caller.
struct UsageKeyNotFound(String);

fn resolve_usage_key(
    snapshot: &ConfigSnapshot,
    caller: &ClientKeyRecord,
    params: &UsageQueryParams,
    now: i64,
) -> Result<Option<llmroller_core::ids::UpstreamKeyId>, UsageKeyNotFound> {
    if let Some(raw) = &params.upstream_key_id {
        let Ok(key_id) = llmroller_core::ids::UpstreamKeyId::parse(raw) else {
            return Err(UsageKeyNotFound(raw.clone()));
        };
        return match accessible_key(snapshot, &key_id, caller) {
            Some(key) => Ok(Some(key.id)),
            None => Err(UsageKeyNotFound(raw.clone())),
        };
    }

    let scope = crate::orchestrator::parse_scope(&caller.routing_scope);
    if let Some(raw_model) = &params.model {
        let model = snapshot
            .resolve_model(raw_model)
            .map_err(|_| UsageKeyNotFound(raw_model.clone()))?;
        if let Some(allowed) = &caller.allowed_models
            && !allowed.iter().any(|entry| entry == &model.canonical_id)
        {
            return Err(UsageKeyNotFound(raw_model.clone()));
        }
        let mut candidates =
            crate::orchestrator::collect_candidates(snapshot, model, caller, scope, now);
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.priority()));
        return Ok(candidates
            .first()
            .map(|candidate| candidate.upstream_key_id()));
    }

    // Default: the caller's highest-priority route across every visible model.
    let mut best: Option<(i64, llmroller_core::ids::UpstreamKeyId)> = None;
    for model in snapshot.models.values() {
        if model.exposed == Some(false) {
            continue;
        }
        if let Some(allowed) = &caller.allowed_models
            && !allowed.iter().any(|entry| entry == &model.canonical_id)
        {
            continue;
        }
        if snapshot
            .user_route_prefs
            .get(&(caller.owner_user_id, model.id))
            .is_some_and(|preference| !preference.enabled)
        {
            continue;
        }
        for candidate in
            crate::orchestrator::collect_candidates(snapshot, model, caller, scope, now)
        {
            if best.is_none_or(|(priority, _)| candidate.priority() > priority) {
                best = Some((candidate.priority(), candidate.upstream_key_id()));
            }
        }
    }
    Ok(best.map(|(_, key_id)| key_id))
}

/// Build live budget-window gauges for one upstream key.
fn usage_windows(
    state: &ProxyAuthState,
    snapshot: &ConfigSnapshot,
    key_id: Option<llmroller_core::ids::UpstreamKeyId>,
    now: i64,
) -> Vec<serde_json::Value> {
    let Some(key_id) = key_id else {
        return Vec::new();
    };
    let windows: &[llmroller_core::snapshot::SnapshotBudgetWindow] = snapshot
        .budget_windows
        .get(&key_id)
        .map_or(&[], Vec::as_slice);
    windows
        .iter()
        .filter_map(|window| {
            let counter = state.runtime_state.window_counter(&window.id)?;
            let used = counter.effective_used(now);
            let limit = counter.limit_micro_usd();
            let used_percent = if limit > 0 {
                (used as f64) * 100.0 / (limit as f64)
            } else {
                0.0
            };
            let status = if limit > 0 && used >= limit {
                "exhausted"
            } else if limit > 0 && used.saturating_mul(100) >= limit.saturating_mul(90) {
                "warning"
            } else {
                "ok"
            };
            Some(serde_json::json!({
                "id": window.id,
                "kind": window.window_kind,
                "status": status,
                "used_percent": used_percent,
                "used_micro_usd": used,
                "limit_micro_usd": limit
            }))
        })
        .collect()
}

/// Handler for GET /v1/usage (USG-7).
pub async fn get_v1_usage_handler(
    State(state): State<ProxyAuthState>,
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

    let now = SystemClock::new().now_ms();
    let snapshot = state.snapshot.load();
    let target_key = match resolve_usage_key(&snapshot, &client_key, &params, now) {
        Ok(key) => key,
        Err(UsageKeyNotFound(requested)) => return not_found(&requested),
    };
    let used_micro = state
        .runtime_state
        .accumulators
        .usage_for(&BudgetScopeId::ClientKey(client_key.id), now);
    let windows = usage_windows(&state, &snapshot, target_key, now);

    (
        StatusCode::OK,
        [("x-llmroller-usage-stale-ms", "0")],
        Json(serde_json::json!({
            "object": "usage_snapshot",
            "source": if target_key.is_some() { "local" } else { "none" },
            "fetched_at": now,
            "stale": false,
            "client_key_prefix": client_key.prefix,
            "session_id": sess_id,
            "upstream_key_id": target_key.map(|key| key.to_string()),
            "windows": windows,
            "credits": serde_json::Value::Null,
            "client_key_allowance": {
                "monthly_cap_micro_usd": client_key.budget_micro_usd_month,
                "used_this_month_micro_usd": used_micro
            }
        })),
    )
        .into_response()
}

/// Handler for GET /v1/key and GET /v1/auth/key (USG-7).
pub async fn get_v1_key_handler(
    State(state): State<ProxyAuthState>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    let now = SystemClock::new().now_ms();
    let used_micro = state
        .runtime_state
        .accumulators
        .usage_for(&BudgetScopeId::ClientKey(client_key.id), now);
    let usage_usd = (used_micro as f64) / 1_000_000.0;
    let limit_usd = client_key
        .budget_micro_usd_month
        .map(|cap| (cap as f64) / 1_000_000.0);
    let limit_remaining_usd = limit_usd.map(|limit| (limit - usage_usd).max(0.0));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "object": "client_key_info",
            "id": client_key.id.to_string(),
            "client_key_id": client_key.id.to_string(),
            "label": client_key.label,
            "prefix": client_key.prefix,
            "routing_scope": client_key.routing_scope,
            "rate_limit_rpm": client_key.rate_limit_rpm,
            "budget_micro_usd_month": client_key.budget_micro_usd_month,
            "monthly_cap_micro_usd": client_key.budget_micro_usd_month,
            "used_this_month_micro_usd": used_micro,
            "data": {
                "label": client_key.prefix,
                "usage": usage_usd,
                "usage_daily": serde_json::Value::Null,
                "usage_weekly": serde_json::Value::Null,
                "usage_monthly": usage_usd,
                "limit": limit_usd,
                "limit_remaining": limit_remaining_usd,
                "limit_reset": serde_json::Value::Null,
                "is_free_tier": false,
                "rate_limit": {
                    "requests": client_key.rate_limit_rpm.unwrap_or(0),
                    "interval": "1m"
                },
                "llmroller": {
                    "used_this_month_micro_usd": used_micro,
                    "monthly_cap_micro_usd": client_key.budget_micro_usd_month,
                    "routing_scope": client_key.routing_scope
                }
            }
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
