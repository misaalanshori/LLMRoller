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
        .route("/v1/embeddings", post(crate::orchestrator::post_embeddings))
        .route("/embeddings", post(crate::orchestrator::post_embeddings))
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

    // Resolve through `resolve_caller_scope`, not the raw parser: `inherit` has no
    // arm in `RoutingScope::from_str`, so the raw parser silently widens it to
    // `PersonalFirst` — broader than `PersonalOnly`/`OwnOnly`/`GlobalOnly`. The
    // advertised catalogue must agree with what the data plane will actually route
    // (`CLK-3`, `F-048`, `ISSUE-0009`).
    let scope = crate::orchestrator::resolve_caller_scope(client_key, snapshot);
    let mut visible: Vec<(String, Option<String>, bool, Option<i64>)> = Vec::new();
    let mut visible_ids = HashSet::new();

    for model in snapshot.models.values() {
        if model.exposed == Some(false) {
            continue;
        }
        if let Some(allowed) = &client_key.allowed_models
            && !llmroller_core::client_key::is_model_allowed_by_patterns(
                allowed,
                &model.canonical_id,
                &model.canonical_id,
            )
            && !allowed.iter().any(|entry| {
                snapshot
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
    let now = state.clock.now_ms();
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
            rate_limit_tpm: None,
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

    // -----------------------------------------------------------------------
    // `inherit` must resolve against the user's default routing scope
    // (`CLK-3`, `F-048`, `ISSUE-0009`)
    // -----------------------------------------------------------------------

    use llmroller_core::ids::{ProviderId, UpstreamKeyId};
    use llmroller_core::provider::{ProviderConfig, ProviderKind};
    use llmroller_core::rbac::{OwnershipScope, Role, RoutingScope};
    use llmroller_core::snapshot::{ProviderRecord, SnapshotModelRoute, UpstreamKeyRecord};

    /// Build a one-model snapshot whose only route is a global-tier provider key.
    ///
    /// `owner` owns nothing, so `own` and `shared` tiers are empty for every caller;
    /// the single global route is the only way the model can be reachable.
    fn snapshot_with_global_route(user: UserId, canonical: &str) -> (ConfigSnapshot, ModelPk) {
        let model = model(canonical, None);
        let model_pk = model.id;

        let provider = ProviderRecord {
            id: ProviderId::generate(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            kind: ProviderKind::Custom,
            name: "global-provider".to_string(),
            base_url: "https://upstream.test".to_string(),
            config_json: "{}".to_string(),
            config: ProviderConfig::default(),
            share_config_fingerprint: String::new(),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
            billing_coverage_mode: "list_price".to_string(),
            plan_multiplier_bps: 10_000,
        };
        let provider_id = provider.id;
        let key = UpstreamKeyRecord {
            id: UpstreamKeyId::generate(),
            provider_id,
            owner_user_id: None,
            label: "global-key".to_string(),
            secret_last4: "0000".to_string(),
            secret_enc: Vec::new(),
            master_key_id: "key-0".to_string(),
            enabled: true,
            priority: 1,
            strict_tier: 0,
            health: "healthy".to_string(),
            health_version: 0,
        };
        let key_id = key.id;

        let route = SnapshotModelRoute {
            upstream_key_id: key_id,
            model_pk,
            upstream_model_id: canonical.to_string(),
            vendor_prefix: None,
            protocols: vec!["chat".to_string()],
            status: llmroller_core::catalog::RouteStatus::Active,
            enabled: true,
            priority_override: None,
            route_health: "healthy".to_string(),
            route_health_until: None,
        };

        let user_record = llmroller_core::snapshot::UserRecord {
            id: user,
            username: "u".to_string(),
            role: Role::User,
            disabled: false,
            can_use_global_providers: true,
            global_budget_micro_usd_month: None,
            default_routing_scope: RoutingScope::PersonalFirst,
        };

        let mut snapshot = ConfigSnapshot::default();
        snapshot.models = Arc::new(HashMap::from([(model_pk, model)]));
        snapshot.routes_by_model = Arc::new(HashMap::from([(model_pk, vec![route])]));
        snapshot.providers = Arc::new(HashMap::from([(provider_id, provider)]));
        snapshot.upstream_keys = Arc::new(HashMap::from([(key_id, key)]));
        snapshot.users = Arc::new(HashMap::from([(user, user_record)]));
        (snapshot, model_pk)
    }

    fn key_for(user: UserId, routing_scope: &str) -> ClientKeyRecord {
        ClientKeyRecord {
            id: ClientKeyId::generate(),
            owner_user_id: user,
            label: "test".to_string(),
            prefix: "llr_test".to_string(),
            key_hash: [0; 32],
            prev_key_hash: None,
            prev_key_expires_at: None,
            routing_scope: routing_scope.to_string(),
            allowed_models: None,
            rate_limit_rpm: None,
            rate_limit_tpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: "off".to_string(),
            service_account_id: None,
            disabled: false,
            paused_at: None,
            revoked_at: None,
            expires_at: None,
        }
    }

    fn set_user_default(snapshot: &mut ConfigSnapshot, user: UserId, scope: RoutingScope) {
        let mut users = (*snapshot.users).clone();
        if let Some(record) = users.get_mut(&user) {
            record.default_routing_scope = scope;
        }
        snapshot.users = Arc::new(users);
    }

    /// An `inherit` key must be resolved against the owner's default scope.
    ///
    /// With `default_routing_scope = GlobalOnly` and the model reachable only through
    /// a global route, the model must be listed. Pre-fix `inherit` parses to
    /// `PersonalFirst`, which does allow the global tier — so this test passes before
    /// the fix and is the *converse* direction; the widening test below is the real
    /// red. Kept because it pins the intended resolution directly.
    #[test]
    #[doc = "Spec: CLK-3, MDL-7 — ISSUE-0009 inherit resolves against the user default"]
    fn visible_models_resolves_inherit_against_the_user_default() {
        let user = UserId::generate();
        let (mut snapshot, _) = snapshot_with_global_route(user, "global-only-model");
        set_user_default(&mut snapshot, user, RoutingScope::GlobalOnly);
        let key = key_for(user, "inherit");

        let (visible, _) = visible_models(&snapshot, &key, 0);
        assert!(
            visible
                .iter()
                .any(|(id, _, _, _)| id == "global-only-model"),
            "a GlobalOnly user's inherit key must see the global-only model; got {:?}",
            visible.iter().map(|v| v.0.as_str()).collect::<Vec<_>>()
        );
    }

    /// The security-relevant direction: `inherit` must **not** widen a strict scope.
    ///
    /// An `OwnOnly` user owns no route, so nothing is reachable. Pre-fix `inherit`
    /// becomes `PersonalFirst`, which allows the global tier, so the model is listed —
    /// advertising a model the data plane will refuse for this user.
    #[test]
    #[doc = "Spec: CLK-3, MDL-7 — ISSUE-0009 inherit does not widen a strict user scope"]
    fn visible_models_does_not_widen_a_strict_user_scope() {
        let user = UserId::generate();
        let (mut snapshot, _) = snapshot_with_global_route(user, "global-only-model");
        set_user_default(&mut snapshot, user, RoutingScope::OwnOnly);
        let key = key_for(user, "inherit");

        let (visible, _) = visible_models(&snapshot, &key, 0);
        assert!(
            !visible
                .iter()
                .any(|(id, _, _, _)| id == "global-only-model"),
            "an OwnOnly user's inherit key must not see a global-only model; the scope \
             was widened to the permissive default. Got {:?}",
            visible.iter().map(|v| v.0.as_str()).collect::<Vec<_>>()
        );
    }

    /// An explicit key scope must win over the user default.
    #[test]
    #[doc = "Spec: CLK-3, MDL-7 — ISSUE-0009 an explicit key scope overrides inherit"]
    fn visible_models_honours_an_explicit_scope_over_inherit() {
        let user = UserId::generate();
        let (mut snapshot, _) = snapshot_with_global_route(user, "global-only-model");
        // The user's own default would exclude the global tier.
        set_user_default(&mut snapshot, user, RoutingScope::OwnOnly);
        // ... but the key says global_only explicitly, so it must be honoured.
        let key = key_for(user, "global_only");

        let (visible, _) = visible_models(&snapshot, &key, 0);
        assert!(
            visible
                .iter()
                .any(|(id, _, _, _)| id == "global-only-model"),
            "an explicit global_only key must see the global-only model regardless of the \
             user default; got {:?}",
            visible.iter().map(|v| v.0.as_str()).collect::<Vec<_>>()
        );
    }

    /// Pre-fix evidence for the widening: the raw parser turns `inherit` into
    /// `PersonalFirst`, which permits the global tier an `OwnOnly` user excludes.
    ///
    /// This asserts the *parser*, deliberately, so the record of what the bypass did
    /// survives the fix. After the fix no production path calls `parse_scope` with an
    /// `"inherit"` key, but the parser's behaviour is unchanged and worth pinning.
    #[test]
    #[doc = "Spec: CLK-3 — ISSUE-0009 the raw parser widens inherit to personal_first"]
    fn raw_parser_treats_inherit_as_personal_first() {
        use llmroller_core::rbac::PoolTier;

        let widened = crate::orchestrator::parse_scope("inherit");
        assert_eq!(
            widened,
            RoutingScope::PersonalFirst,
            "the raw parser has no inherit arm, so it falls back to the default"
        );
        assert!(
            widened.allows_tier(PoolTier::Global),
            "personal_first allows the global tier, which a stricter user scope would \
             exclude: this is the widening the bypass caused"
        );
        assert!(
            !RoutingScope::OwnOnly.allows_tier(PoolTier::Global),
            "own_only excludes the global tier, so widening it is a real change of reach"
        );
    }

    /// Pins the current, known-permissive fallback for an unrecognized scope string.
    ///
    /// `RoutingScope::from_str` rejects unknown values and the caller falls back to
    /// `PersonalFirst`, which is the most permissive scope. This is a deliberate
    /// pin, not an endorsement: an invalid value should be rejected when the client
    /// key is written, not guessed at read time. Recorded as an out-of-scope finding
    /// for the client-key write path.
    #[test]
    #[doc = "Spec: CLK-3 — ISSUE-0009 invalid scope falls back to personal_first (pinned)"]
    fn invalid_routing_scope_falls_back_to_personal_first() {
        let user = UserId::generate();
        let (mut snapshot, _) = snapshot_with_global_route(user, "global-only-model");
        set_user_default(&mut snapshot, user, RoutingScope::GlobalOnly);
        // A typo the write path should have rejected.
        let key = key_for(user, "personalfirst");

        let (visible, _) = visible_models(&snapshot, &key, 0);
        assert!(
            visible
                .iter()
                .any(|(id, _, _, _)| id == "global-only-model"),
            "PINNED BEHAVIOUR: an invalid routing_scope currently falls back to \
             personal_first, which allows the global tier, so the model is listed. \
             Changing this is a validation-policy decision for the client-key write \
             path, not a read-path tweak. Got {:?}",
            visible.iter().map(|v| v.0.as_str()).collect::<Vec<_>>()
        );
    }
}
