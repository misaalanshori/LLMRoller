#![forbid(unsafe_code)]
//! Data plane and metrics HTTP listener routers.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#22-sub-phase-22-client-key-llr_-auth-rotation-grace-model-request-gates-g1g6-and-listeners

use std::sync::atomic::Ordering;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::auth::{ProxyAuthState, client_key_auth_middleware};
use crate::metrics::metrics_handler;
use llmroller_core::client_key::ClientKeyRecord;
use llmroller_core::seams::Clock as _;

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
            "/v1/auth/key",
            get(crate::usage_passthrough::get_v1_key_handler),
        )
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

/// Canonical models paired with their display name and route availability.
type VisibleModels = (Vec<(String, Option<String>, bool)>, Vec<(String, String)>);

/// Project the caller-visible models and aliases from the immutable snapshot (`MDL-7`).
fn visible_models(
    snapshot: &llmroller_core::snapshot::ConfigSnapshot,
    client_key: &ClientKeyRecord,
    now: i64,
) -> VisibleModels {
    use std::collections::HashSet;

    let scope = crate::orchestrator::parse_scope(&client_key.routing_scope);
    let mut visible: Vec<(String, Option<String>, bool)> = Vec::new();
    let mut visible_ids = HashSet::new();

    for model in snapshot.models.values() {
        if model.exposed == Some(false) {
            continue;
        }
        if let Some(allowed) = &client_key.allowed_models
            && !allowed.iter().any(|entry| entry == &model.canonical_id)
        {
            continue;
        }
        let has_routes = snapshot
            .routes_by_model
            .get(&model.id)
            .is_some_and(|routes| !routes.is_empty());
        let reachable = crate::orchestrator::collect_visible_candidates(
            snapshot, model, client_key, scope, now,
        );
        // Models whose every route is invisible to this caller are omitted; exposed
        // models with no route at all remain listed as `available: false` (MDL-7).
        if has_routes && reachable.is_empty() {
            continue;
        }
        let available =
            !crate::orchestrator::collect_candidates(snapshot, model, client_key, scope, now)
                .is_empty();
        visible_ids.insert(model.id);
        visible.push((
            model.canonical_id.clone(),
            model.display_name.clone(),
            available,
        ));
    }

    visible.sort_by(|left, right| left.0.cmp(&right.0));

    let mut aliases = Vec::new();
    for (alias, model_pk) in snapshot.aliases.iter() {
        if !visible_ids.contains(model_pk) {
            continue;
        }
        let Some(model) = snapshot.models.get(model_pk) else {
            continue;
        };
        if let Some(allowed) = &client_key.allowed_models
            && !allowed
                .iter()
                .any(|entry| entry == alias || entry == &model.canonical_id)
        {
            continue;
        }
        aliases.push((alias.clone(), model.canonical_id.clone()));
    }
    aliases.sort();

    (visible, aliases)
}

async fn v1_models_handler(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    let snapshot = state.snapshot.load();
    let now = llmroller_core::seams::SystemClock::new().now_ms();
    let (models, aliases) = visible_models(&snapshot, &client_key, now);

    if headers.contains_key("anthropic-version") {
        let items: Vec<serde_json::Value> = models
            .into_iter()
            .map(|(id, display_name, _)| {
                let display_name = display_name.unwrap_or_else(|| id.clone());
                serde_json::json!({
                    "type": "model",
                    "id": id,
                    "display_name": display_name,
                    "created_at": "2026-01-01T00:00:00Z"
                })
            })
            .collect();
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "data": items,
                "has_more": false,
                "first_id": null,
                "last_id": null
            })),
        )
            .into_response();
    }

    let mut data = Vec::new();
    for (id, _, available) in models {
        let model_aliases: Vec<&str> = aliases
            .iter()
            .filter(|(_, canonical)| canonical == &id)
            .map(|(alias, _)| alias.as_str())
            .collect();
        data.push(serde_json::json!({
            "id": id,
            "object": "model",
            "created": 1700000000,
            "owned_by": "llmroller",
            "llmroller": {
                "available": available,
                "aliases": model_aliases
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
