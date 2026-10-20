#![forbid(unsafe_code)]
//! Data plane and metrics HTTP listener routers.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#22-sub-phase-22-client-key-llr_-auth-rotation-grace-model-request-gates-g1g6-and-listeners

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use sqlx::{Row as _, SqlitePool};

use crate::auth::{ProxyAuthState, client_key_auth_middleware};
use crate::metrics::metrics_handler;
use llmroller_core::client_key::ClientKeyRecord;

/// Build the data plane router serving `/v1/*`, `/healthz`, and `/readyz`.
pub fn data_plane_router(state: ProxyAuthState) -> Router {
    let authed_v1 = Router::new()
        .route("/v1/models", get(v1_models_handler))
        .route(
            "/v1/chat/completions",
            post(crate::orchestrator::post_chat_completions),
        )
        .route("/v1/responses", post(crate::orchestrator::post_responses))
        .route(
            "/v1/responses/compact",
            post(crate::orchestrator::post_responses_compact),
        )
        .route(
            "/v1/responses/{id}",
            get(crate::orchestrator::get_response_by_id)
                .delete(crate::orchestrator::delete_response_by_id),
        )
        .route(
            "/v1/responses/{id}/input_items",
            get(crate::orchestrator::get_response_input_items),
        )
        .route(
            "/v1/responses/{id}/cancel",
            post(crate::orchestrator::post_response_cancel),
        )
        .route("/v1/messages", post(crate::orchestrator::post_messages))
        .route(
            "/v1/messages/count_tokens",
            post(crate::orchestrator::post_messages_count_tokens),
        )
        .route(
            "/v1/usage",
            get(crate::usage_passthrough::get_v1_usage_handler),
        )
        .route("/v1/key", get(crate::usage_passthrough::get_v1_key_handler))
        .route(
            "/v1/usage/raw",
            get(crate::usage_passthrough::get_v1_usage_raw_handler),
        )
        .layer(from_fn_with_state(
            state.clone(),
            client_key_auth_middleware,
        ));

    Router::new()
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .merge(authed_v1)
        .with_state(state)
}

/// Build the loopback metrics router serving `/metrics`.
pub fn metrics_router() -> Router {
    Router::new().route("/metrics", get(metrics_handler))
}

async fn healthz_handler() -> Response {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn readyz_handler(State(state): State<ProxyAuthState>) -> Response {
    if state.is_ready.load(Ordering::SeqCst) {
        (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ready" })),
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "not_ready" })),
        )
            .into_response()
    }
}

struct ExposedModelDto {
    id: String,
    display_name: Option<String>,
    available: bool,
    aliases: Vec<String>,
}

async fn fetch_models_for_client_key(
    pool: &SqlitePool,
    client_key: &ClientKeyRecord,
) -> Result<(Vec<ExposedModelDto>, Vec<(String, String)>), sqlx::Error> {
    let rows = sqlx::query(
        r#"SELECT m.model_pk, m.canonical_id, m.display_name, m.exposed,
                  (SELECT COUNT(*) FROM model_routes mr
                   JOIN upstream_keys k ON mr.upstream_key_id = k.id
                   WHERE mr.model_pk = m.model_pk AND mr.status = 'active' AND k.enabled = 1) as active_route_count
           FROM models m
           WHERE m.exposed IS NOT 0"#,
    )
    .fetch_all(pool)
    .await?;

    let aliases_rows = sqlx::query(
        r#"SELECT ma.alias, m.canonical_id
           FROM model_aliases ma
           JOIN models m ON ma.model_pk = m.model_pk
           WHERE ma.confirmed = 1 AND m.exposed IS NOT 0"#,
    )
    .fetch_all(pool)
    .await?;

    let mut model_aliases_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut alias_pairs = Vec::new();
    for a in aliases_rows {
        let alias: String = a.try_get("alias")?;
        let canonical_id: String = a.try_get("canonical_id")?;
        if let Some(allowed) = &client_key.allowed_models
            && !allowed.contains(&alias)
            && !allowed.contains(&canonical_id)
        {
            continue;
        }
        model_aliases_map
            .entry(canonical_id.clone())
            .or_default()
            .push(alias.clone());
        alias_pairs.push((alias, canonical_id));
    }

    let mut out = Vec::new();
    for r in rows {
        let canonical_id: String = r.try_get("canonical_id")?;
        if let Some(allowed) = &client_key.allowed_models
            && !allowed.contains(&canonical_id)
        {
            continue;
        }
        let active_route_count: i64 = r.try_get("active_route_count")?;
        let exposed: Option<i64> = r.try_get("exposed")?;
        let display_name: Option<String> = r.try_get("display_name")?;

        let available = active_route_count > 0;
        // If exposed is NULL (automatic), only include if available
        if exposed.is_none() && !available {
            continue;
        }

        let aliases = model_aliases_map.remove(&canonical_id).unwrap_or_default();
        out.push(ExposedModelDto {
            id: canonical_id,
            display_name,
            available,
            aliases,
        });
    }

    Ok((out, alias_pairs))
}

async fn v1_models_handler(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    let models_result = fetch_models_for_client_key(&state.pool, &client_key).await;
    match models_result {
        Ok((models, aliases)) => {
            if headers.contains_key("anthropic-version") {
                let items: Vec<serde_json::Value> = models
                    .into_iter()
                    .filter(|m| m.available)
                    .map(|m| {
                        serde_json::json!({
                            "type": "model",
                            "id": m.id,
                            "display_name": m.display_name.unwrap_or(m.id),
                            "created_at": "2026-01-01T00:00:00Z"
                        })
                    })
                    .collect();
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "data": items,
                        "has_more": false,
                        "first_id": null,
                        "last_id": null
                    })),
                )
                    .into_response()
            } else {
                let mut data = Vec::new();
                for m in models {
                    data.push(serde_json::json!({
                        "id": m.id,
                        "object": "model",
                        "created": 1700000000,
                        "owned_by": "llmroller",
                        "llmroller": {
                            "available": m.available,
                            "aliases": m.aliases
                        }
                    }));
                }
                for (alias, canonical) in aliases {
                    data.push(serde_json::json!({
                        "id": alias,
                        "object": "model",
                        "created": 1700000000,
                        "owned_by": "llmroller",
                        "llmroller": {
                            "alias_of": canonical
                        }
                    }));
                }
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "object": "list",
                        "data": data
                    })),
                )
                    .into_response()
            }
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "failed to list models" })),
        )
            .into_response(),
    }
}
