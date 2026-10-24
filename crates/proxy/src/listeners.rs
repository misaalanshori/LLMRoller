#![forbid(unsafe_code)]
//! Data plane and metrics HTTP listener routers.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#22-sub-phase-22-client-key-llr_-auth-rotation-grace-model-request-gates-g1g6-and-listeners

use std::sync::atomic::Ordering;

use axum::extract::{DefaultBodyLimit, Extension, State};
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
    let max_body_bytes = state
        .budget_semaphore
        .max_body_mb
        .saturating_mul(1024 * 1024);
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
        .route("/v1/v1/messages", post(crate::orchestrator::post_messages))
        .route(
            "/v1/v1/messages/count_tokens",
            post(crate::orchestrator::post_messages_count_tokens),
        )
        .route("/messages", post(crate::orchestrator::post_messages))
        .route(
            "/messages/count_tokens",
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
        .layer(DefaultBodyLimit::max(max_body_bytes))
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
    let writer_healthy = state
        .writer
        .as_ref()
        .is_none_or(|w| !w.is_sqlite_full() && !w.is_durable_queue_full());
    let not_draining = !state.shutdown_cut.load(Ordering::Acquire);
    if state.is_ready.load(Ordering::SeqCst) && not_draining && writer_healthy {
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

/// Canonical models paired with their display name, route availability, and the
/// discovered context window (if the upstream advertised one).
type VisibleModels = (
    Vec<(String, Option<String>, bool, Option<i64>)>,
    Vec<(String, String, Option<i64>)>,
);

/// Adds the discovered `context_window` only when it is actually known, so the
/// proxy never fabricates a capability value (`F-031`, `MDL-1`).
fn with_context_window(
    mut value: serde_json::Value,
    context_window: Option<i64>,
) -> serde_json::Value {
    if let Some(context_window) = context_window {
        value["context_window"] = serde_json::json!(context_window);
    }
    value
}

/// Project the caller-visible models and aliases from the immutable snapshot (`MDL-7`).
fn visible_models(
    snapshot: &llmroller_core::snapshot::ConfigSnapshot,
    client_key: &ClientKeyRecord,
    now: i64,
) -> VisibleModels {
    use std::collections::HashSet;

    let scope = crate::orchestrator::parse_scope(&client_key.routing_scope);
    let mut visible: Vec<(String, Option<String>, bool, Option<i64>)> = Vec::new();
    let mut visible_ids = HashSet::new();

    for model in snapshot.models.values() {
        if model.exposed == Some(false) {
            continue;
        }
        if let Some(allowed) = &client_key.allowed_models
            && !allowed.iter().any(|entry| {
                entry == &model.canonical_id
                    || snapshot
                        .aliases
                        .get(entry)
                        .is_some_and(|target_pk| *target_pk == model.id)
            })
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
            model.context_window,
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
        aliases.push((
            alias.clone(),
            model.canonical_id.clone(),
            model.context_window,
        ));
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
        let mut items: Vec<serde_json::Value> = models
            .into_iter()
            .map(|(id, display_name, _, context_window)| {
                let display_name = display_name.unwrap_or_else(|| id.clone());
                with_context_window(
                    serde_json::json!({
                        "type": "model",
                        "id": id,
                        "display_name": display_name,
                        "created_at": "2026-01-01T00:00:00Z"
                    }),
                    context_window,
                )
            })
            .collect();
        for (alias, canonical, context_window) in aliases {
            items.push(with_context_window(
                serde_json::json!({
                    "type": "model",
                    "id": alias,
                    "display_name": canonical,
                    "created_at": "2026-01-01T00:00:00Z"
                }),
                context_window,
            ));
        }
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
    for (id, _, available, context_window) in models {
        let model_aliases: Vec<&str> = aliases
            .iter()
            .filter(|(_, canonical, _)| canonical == &id)
            .map(|(alias, _, _)| alias.as_str())
            .collect();
        let mut item = serde_json::json!({
            "id": id,
            "object": "model",
            "created": 1700000000,
            "owned_by": "llmroller",
            "llmroller": {
                "available": available,
                "aliases": model_aliases
            }
        });
        if let Some(context_window) = context_window {
            item["context_window"] = serde_json::json!(context_window);
            item["llmroller"]["context_window"] = serde_json::json!(context_window);
        }
        data.push(item);
    }
    for (alias, canonical, context_window) in aliases {
        data.push(with_context_window(
            serde_json::json!({
                "id": alias,
                "object": "model",
                "created": 1700000000,
                "owned_by": "llmroller",
                "llmroller": {
                    "alias_of": canonical
                }
            }),
            context_window,
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use llmroller_core::catalog::{MergeState, ModelOrigin};
    use llmroller_core::ids::{ClientKeyId, ModelPk, UserId};
    use llmroller_core::snapshot::{ConfigSnapshot, SnapshotModel};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn model(canonical: &str, context_window: Option<i64>) -> SnapshotModel {
        SnapshotModel {
            id: ModelPk::generate(),
            canonical_id: canonical.to_string(),
            display_name: None,
            context_window,
            exposed: None,
            merge_state: MergeState::Confirmed,
            majority_vendor_prefix: None,
            distribution: "weighted".to_string(),
            routing_strategy: None,
            origin: ModelOrigin::Global,
            owner_user_id: None,
        }
    }

    #[test]
    #[doc = "Spec: API-1, MDL-1, MDL-4"]
    fn models_endpoint_emits_discovered_context_windows_not_a_fixed_constant() {
        let mut snapshot = ConfigSnapshot::default();
        let mut models = HashMap::new();
        for model in [
            model("large-context", Some(1_000_000)),
            model("small-context", Some(16_384)),
        ] {
            models.insert(model.id, model);
        }
        snapshot.models = Arc::new(models);
        let client_key = ClientKeyRecord {
            id: ClientKeyId::generate(),
            owner_user_id: UserId::generate(),
            label: "test".to_string(),
            prefix: "llr_test".to_string(),
            key_hash: [0; 32],
            prev_key_hash: None,
            prev_key_expires_at: None,
            routing_scope: "personal_first".to_string(),
            allowed_models: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: "off".to_string(),
            service_account_id: None,
            disabled: false,
            paused_at: None,
            revoked_at: None,
            expires_at: None,
        };

        let (visible, _) = visible_models(&snapshot, &client_key, 0);
        assert_eq!(
            visible
                .iter()
                .map(|(id, _, _, context)| (id.as_str(), *context))
                .collect::<Vec<_>>(),
            vec![
                ("large-context", Some(1_000_000)),
                ("small-context", Some(16_384)),
            ]
        );
        assert!(
            with_context_window(serde_json::json!({"id": "x"}), None)
                .get("context_window")
                .is_none()
        );
    }
}
