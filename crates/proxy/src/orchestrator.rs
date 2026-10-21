#![forbid(unsafe_code)]
//! Orchestrator for /v1/* inference endpoints and pre-flight body budget admission.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#53-orchestration-and-request-flow-state-machine
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-performance-and-resource-efficiency-perf
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#27-sub-phase-27-v1-orchestrator-v1-pre-flight-body-memory-budget-graceful-sigterm-drain-and-early-load-harness

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::header::{CONTENT_TYPE, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::stream;
use llmroller_core::accumulators::MonthReservationGuard;
use llmroller_core::client_key::ClientKeyRecord;
use llmroller_core::eligibility::{
    CallerContext, CandidatePoolTier, CandidateRoute as EligibilityRoute, PoolSelectionError,
    select_pool,
};
use llmroller_core::ids::{
    ClientKeyId, ModelPk, ProviderId, RequestId, ShareId, UpstreamKeyId, UserId,
};
use llmroller_core::pins::{PinKey, PinTicket};
use llmroller_core::pricing::{
    CostSource, PriceEntry, Rates, compute_request_cost, estimate_input_tokens,
};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{PoolTier, RoutingScope};
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::seams::{Clock as _, OsEntropy, SystemClock};
use llmroller_core::snapshot::{
    ConfigSnapshot, ProviderRecord, SnapshotModel, SnapshotModelRoute, UpstreamKeyRecord,
};
use llmroller_core::strategy::{RouteId, RoutingStrategy, WrrGroupKey};
use llmroller_core::windows::{Reservation, estimate_reservation_cost_micro_usd};
use llmroller_proto::adapters::EndpointKind;
use llmroller_proto::body_edit::{ChatStreamMode, edit_body, scan_top_level};
use llmroller_proto::json_usage::extract_json_usage;
use llmroller_proto::sse_tap::SseUsageTap;
use llmroller_store::crypto::{SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_store::usage_writer::{UsageAttemptRecord, UsageEventRecord};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::auth::ProxyAuthState;
use crate::forward::{PreFirstFrameBuffer, SseChunkForwarder};
use crate::lifecycle::{RequestLifecycle, Termination};

/// Maximum idle time while waiting for an upstream chunk.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Pre-flight admission error when memory budget is saturated.
#[derive(Debug, Error)]
pub enum GatewayBusyError {
    #[error("gateway busy: preflight body memory budget saturated")]
    Timeout,
    #[error("body exceeds maximum allowed size: {0} bytes")]
    BodyTooLarge(usize),
}

impl GatewayBusyError {
    #[must_use]
    pub const fn status_code(&self) -> StatusCode {
        match self {
            Self::Timeout => StatusCode::SERVICE_UNAVAILABLE,
            Self::BodyTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }

    #[must_use]
    pub const fn retry_after_secs(&self) -> u64 {
        1
    }
}

/// Pre-flight body memory budget semaphore tracking pre-first-byte buffered bodies per SEC-8.
#[derive(Debug, Clone)]
pub struct PreflightBudgetSemaphore {
    pub budget_mb: usize,
    pub max_body_mb: usize,
    pub semaphore: Arc<Semaphore>,
}

impl PreflightBudgetSemaphore {
    #[must_use]
    pub fn new(budget_mb: usize, max_body_mb: usize) -> Self {
        let permits = (budget_mb * 1024 * 1024) / 4096;
        Self {
            budget_mb,
            max_body_mb,
            semaphore: Arc::new(Semaphore::new(permits)),
        }
    }

    /// Acquire permits for the given body size with a timeout.
    pub async fn acquire(
        &self,
        body_len: usize,
        timeout: Duration,
    ) -> Result<OwnedSemaphorePermit, GatewayBusyError> {
        let max_bytes = self.max_body_mb * 1024 * 1024;
        if body_len > max_bytes {
            return Err(GatewayBusyError::BodyTooLarge(body_len));
        }

        let permits_needed = body_len.div_ceil(4096);
        let permit = tokio::time::timeout(
            timeout,
            self.semaphore
                .clone()
                .acquire_many_owned(permits_needed as u32),
        )
        .await
        .map_err(|_| GatewayBusyError::Timeout)?
        .map_err(|_| GatewayBusyError::Timeout)?;

        Ok(permit)
    }
}

/// Handler for POST /v1/chat/completions.
pub async fn post_chat_completions(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    body: Bytes,
) -> Response {
    handle_inference_request(
        state,
        EndpointKind::Chat,
        headers,
        body,
        client_key.map(|e| e.0),
    )
    .await
}

/// Handler for POST /v1/responses.
pub async fn post_responses(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    body: Bytes,
) -> Response {
    handle_inference_request(
        state,
        EndpointKind::Responses,
        headers,
        body,
        client_key.map(|e| e.0),
    )
    .await
}

/// Handler for POST /v1/messages.
pub async fn post_messages(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    body: Bytes,
) -> Response {
    handle_inference_request(
        state,
        EndpointKind::Messages,
        headers,
        body,
        client_key.map(|e| e.0),
    )
    .await
}

/// Handler for POST /v1/messages/count_tokens.
pub async fn post_messages_count_tokens(
    State(_state): State<ProxyAuthState>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    let est = llmroller_core::pricing::estimate_input_tokens(&body);
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        Json(serde_json::json!({
            "input_tokens": est
        })),
    )
        .into_response()
}

/// Handler for POST /v1/responses/compact.
pub async fn post_responses_compact(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    body: Bytes,
) -> Response {
    handle_inference_request(
        state,
        EndpointKind::Responses,
        headers,
        body,
        client_key.map(|e| e.0),
    )
    .await
}

/// Handler for GET /v1/responses/{id}.
pub async fn get_response_by_id(
    State(state): State<ProxyAuthState>,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    use sqlx::Row as _;
    let query_result = sqlx::query(
        "SELECT response_id, owner_user_id, status FROM response_links WHERE response_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await;

    match query_result {
        Ok(Some(row)) => {
            let owner_str: String = match row.try_get("owner_user_id") {
                Ok(s) => s,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            if owner_str != client_key.owner_user_id.to_string() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
                )
                    .into_response();
            }
            let resp_id: String = row.try_get("response_id").unwrap_or(id);
            let status: String = row
                .try_get("status")
                .unwrap_or_else(|_| "completed".to_string());
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "id": resp_id,
                    "object": "response",
                    "status": status
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
        )
            .into_response(),
    }
}

/// Handler for DELETE /v1/responses/{id}.
pub async fn delete_response_by_id(
    State(state): State<ProxyAuthState>,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    use sqlx::Row as _;
    let row = sqlx::query("SELECT owner_user_id FROM response_links WHERE response_id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await;

    match row {
        Ok(Some(r)) => {
            let owner_str: String = r.try_get("owner_user_id").unwrap_or_default();
            if owner_str != client_key.owner_user_id.to_string() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
                )
                    .into_response();
            }
            let _ = sqlx::query("DELETE FROM response_links WHERE response_id = ?")
                .bind(&id)
                .execute(&state.pool)
                .await;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "id": id,
                    "object": "response",
                    "deleted": true
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
        )
            .into_response(),
    }
}

/// Handler for GET /v1/responses/{id}/input_items.
pub async fn get_response_input_items(
    State(state): State<ProxyAuthState>,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    use sqlx::Row as _;
    let row = sqlx::query("SELECT owner_user_id FROM response_links WHERE response_id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await;

    match row {
        Ok(Some(r)) => {
            let owner_str: String = r.try_get("owner_user_id").unwrap_or_default();
            if owner_str != client_key.owner_user_id.to_string() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "object": "list",
                    "data": []
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
        )
            .into_response(),
    }
}

/// Handler for POST /v1/responses/{id}/cancel.
pub async fn post_response_cancel(
    State(state): State<ProxyAuthState>,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    use sqlx::Row as _;
    let row = sqlx::query("SELECT owner_user_id FROM response_links WHERE response_id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await;

    match row {
        Ok(Some(r)) => {
            let owner_str: String = r.try_get("owner_user_id").unwrap_or_default();
            if owner_str != client_key.owner_user_id.to_string() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
                )
                    .into_response();
            }
            let _ =
                sqlx::query("UPDATE response_links SET status = 'cancelled' WHERE response_id = ?")
                    .bind(&id)
                    .execute(&state.pool)
                    .await;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "id": id,
                    "object": "response",
                    "status": "cancelled"
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
        )
            .into_response(),
    }
}

/// One fully-resolved routing candidate built exclusively from the immutable snapshot.
#[derive(Debug, Clone)]
struct RouteCandidate {
    upstream_key_id: UpstreamKeyId,
    provider_id: ProviderId,
    model_pk: ModelPk,
    upstream_model_id: String,
    base_url: String,
    allow_private_network: bool,
    pool_tier: CandidatePoolTier,
    priority: i64,
    provider_kind: ProviderKind,
    share_id: Option<ShareId>,
}

impl RouteCandidate {
    fn route_id(&self) -> RouteId {
        RouteId {
            upstream_key_id: self.upstream_key_id,
            model_pk: self.model_pk,
        }
    }

    fn pool_tier(&self) -> PoolTier {
        match self.pool_tier {
            CandidatePoolTier::Own => PoolTier::Own,
            CandidatePoolTier::Shared => PoolTier::Shared,
            CandidatePoolTier::Global => PoolTier::Global,
            CandidatePoolTier::Blended => PoolTier::Blended,
        }
    }
}

/// Compute the Unix wall-clock time in milliseconds.
fn now_ms() -> i64 {
    SystemClock::new().now_ms()
}

/// Parse a client key routing scope, failing closed to the safest default.
fn parse_scope(scope: &str) -> RoutingScope {
    scope.parse().unwrap_or_default()
}

/// Whether a discovered route is currently routable by health state.
fn route_healthy(route: &SnapshotModelRoute, now: i64) -> bool {
    match route.route_health.as_str() {
        "healthy" => true,
        "cooling_down" | "exhausted" => route.route_health_until.is_none_or(|until| now >= until),
        _ => false,
    }
}

/// Collect every route for a model visible to the caller, strictly classified by tier.
fn collect_candidates(
    snapshot: &ConfigSnapshot,
    model: &SnapshotModel,
    caller: &ClientKeyRecord,
    scope: RoutingScope,
    now: i64,
) -> Vec<RouteCandidate> {
    let Some(routes) = snapshot.routes_by_model.get(&model.id) else {
        return Vec::new();
    };

    let mut candidates = Vec::with_capacity(routes.len());
    for route in routes {
        if !route.enabled || route.status != llmroller_core::catalog::RouteStatus::Active {
            continue;
        }
        let Some(key) = snapshot.upstream_keys.get(&route.upstream_key_id) else {
            continue;
        };
        if !key.enabled {
            continue;
        }
        let Some(provider) = snapshot.providers.get(&key.provider_id) else {
            continue;
        };
        if !provider.enabled {
            continue;
        }

        let (tier, share_id) = match provider.ownership_scope {
            llmroller_core::rbac::OwnershipScope::Global => (CandidatePoolTier::Global, None),
            llmroller_core::rbac::OwnershipScope::Personal => {
                if provider.owner_user_id == Some(caller.owner_user_id) {
                    (CandidatePoolTier::Own, None)
                } else if let Some(share) =
                    snapshot.effective_share(caller.owner_user_id, provider.id, key.id)
                {
                    (CandidatePoolTier::Shared, Some(share.share_id))
                } else {
                    // A foreign personal key without a share grant is invisible (SEC-17).
                    continue;
                }
            }
        };

        if !scope.allows_tier(candidate_tier_as_pool(tier)) {
            continue;
        }
        if !route_healthy(route, now) {
            continue;
        }

        candidates.push(RouteCandidate {
            upstream_key_id: key.id,
            provider_id: provider.id,
            model_pk: model.id,
            upstream_model_id: route.upstream_model_id.clone(),
            base_url: provider.base_url.clone(),
            allow_private_network: provider.allow_private_network,
            pool_tier: tier,
            priority: route
                .priority_override
                .unwrap_or(key.priority)
                .clamp(1, 1000),
            provider_kind: provider.kind,
            share_id,
        });
    }

    candidates.sort_by_key(|candidate| {
        (
            candidate_tier_as_pool(candidate.pool_tier).code(),
            candidate.priority,
        )
    });
    candidates
}

const fn candidate_tier_as_pool(tier: CandidatePoolTier) -> PoolTier {
    match tier {
        CandidatePoolTier::Own => PoolTier::Own,
        CandidatePoolTier::Shared => PoolTier::Shared,
        CandidatePoolTier::Global => PoolTier::Global,
        CandidatePoolTier::Blended => PoolTier::Blended,
    }
}

/// Build the C1-C8 eligibility projection for one candidate.
fn eligibility_route(
    snapshot: &ConfigSnapshot,
    runtime: &RuntimeState,
    candidate: &RouteCandidate,
    model: &SnapshotModel,
    caller: &ClientKeyRecord,
    now: i64,
) -> EligibilityRoute {
    let provider = snapshot
        .providers
        .get(&candidate.provider_id)
        .cloned()
        .unwrap_or(ProviderRecord {
            id: candidate.provider_id,
            ownership_scope: llmroller_core::rbac::OwnershipScope::Global,
            owner_user_id: None,
            kind: candidate.provider_kind,
            name: String::new(),
            base_url: candidate.base_url.clone(),
            config_json: String::new(),
            share_config_fingerprint: String::new(),
            usage_url_cross_origin_ack: false,
            allow_private_network: candidate.allow_private_network,
            enabled: false,
        });
    let key = snapshot
        .upstream_keys
        .get(&candidate.upstream_key_id)
        .cloned()
        .unwrap_or(UpstreamKeyRecord {
            id: candidate.upstream_key_id,
            provider_id: candidate.provider_id,
            owner_user_id: None,
            label: String::new(),
            secret_last4: String::new(),
            secret_enc: Vec::new(),
            master_key_id: String::new(),
            enabled: false,
            priority: candidate.priority,
            strict_tier: 1,
            health: "invalid".to_string(),
            health_version: 0,
        });
    let route = snapshot.routes_by_model.get(&model.id).and_then(|routes| {
        routes
            .iter()
            .find(|route| route.upstream_key_id == candidate.upstream_key_id)
    });

    EligibilityRoute {
        upstream_key_id: candidate.upstream_key_id,
        model_pk: model.id,
        provider_id: candidate.provider_id,
        provider_kind: candidate.provider_kind,
        provider_ownership: provider.ownership_scope,
        provider_owner_id: provider.owner_user_id,
        provider_fingerprint: provider.share_config_fingerprint.clone(),
        provider_enabled: provider.enabled,
        key_enabled: key.enabled,
        route_enabled: route.is_some_and(|route| route.enabled),
        route_status: route.map_or(llmroller_core::catalog::RouteStatus::Retired, |route| {
            route.status
        }),
        protocols: route.map_or_else(Vec::new, |route| route.protocols.clone()),
        protocol_blocked: route.is_some_and(|route| !route_healthy(route, now)),
        key_health_ok: runtime.is_key_routable(candidate.upstream_key_id, now),
        route_health_ok: route.is_some_and(|route| route_healthy(route, now)),
        breaker_closed: runtime.is_origin_admitted(candidate.provider_id, now),
        user_pref_enabled: snapshot
            .user_route_prefs
            .get(&(caller.owner_user_id, model.id))
            .filter(|pref| pref.upstream_key_id == candidate.upstream_key_id)
            .map(|pref| pref.enabled),
        priority: candidate.priority,
        budget_admitted: runtime.key_budget_admitted(candidate.upstream_key_id, now),
        budget_exhausted_due_to_monthly_cap: false,
    }
}

/// Select the next candidate in the tier group with the model's configured strategy.
#[allow(clippy::too_many_arguments)]
fn select_next_candidate(
    runtime: &RuntimeState,
    strategy: RoutingStrategy,
    caller: UserId,
    model_pk: ModelPk,
    protocol: &str,
    tier: CandidatePoolTier,
    eligible: &[RouteCandidate],
    tried: &[UpstreamKeyId],
    now: i64,
) -> Option<RouteCandidate> {
    let candidates: Vec<(RouteId, i64)> = eligible
        .iter()
        .filter(|candidate| !tried.contains(&candidate.upstream_key_id))
        .map(|candidate| (candidate.route_id(), candidate.priority))
        .collect();
    if candidates.is_empty() {
        return None;
    }

    let group_key = WrrGroupKey {
        user_id: caller,
        model_pk,
        protocol: protocol.to_string(),
        pool_tier: candidate_tier_as_pool(tier).code(),
    };
    let selector = runtime.selectors.selector(strategy);
    let selected = selector.select_next(
        &group_key,
        &candidates,
        now,
        &OsEntropy,
        Some(runtime.load_tracker.as_ref()),
    )?;
    eligible
        .iter()
        .find(|candidate| candidate.upstream_key_id == selected.upstream_key_id)
        .cloned()
}

/// Immutable routing context needed to reselect a pin after a pre-first-byte failure.
struct PinFailureContext<'a> {
    runtime: &'a RuntimeState,
    strategy: RoutingStrategy,
    caller: UserId,
    model_pk: ModelPk,
    protocol: &'a str,
    selected_tier: CandidatePoolTier,
    tier_candidates: &'a [RouteCandidate],
    eligible_keys: &'a HashSet<UpstreamKeyId>,
    now: i64,
}

impl PinFailureContext<'_> {
    fn select(&self, tried: &[UpstreamKeyId]) -> Option<(UpstreamKeyId, CandidatePoolTier)> {
        select_next_candidate(
            self.runtime,
            self.strategy,
            self.caller,
            self.model_pk,
            self.protocol,
            self.selected_tier,
            self.tier_candidates,
            tried,
            self.now,
        )
        .map(|candidate| (candidate.upstream_key_id, candidate.pool_tier))
    }
}

/// Advance the sticky pin past a key that failed before the first valid frame (RTE-8).
fn note_pre_first_byte_failure(
    state: &ProxyAuthState,
    pin_key: Option<&PinKey>,
    ticket: Option<&PinTicket>,
    failed_key: UpstreamKeyId,
    context: &PinFailureContext<'_>,
) -> Option<PinTicket> {
    let (pin_key, ticket) = (pin_key?, ticket?);
    state.pin_store.pre_first_byte_failure(
        pin_key,
        ticket,
        failed_key,
        context.now,
        |key_id| context.eligible_keys.contains(&key_id),
        |tried| context.select(tried),
    )
}

/// Compute the monthly and per-window reservation estimate for an attempt.
fn estimate_cost_micro_usd(
    snapshot: &ConfigSnapshot,
    candidate: &RouteCandidate,
    input_tokens: i64,
) -> i64 {
    let fallback = Rates::from(&snapshot.settings.unpriced_fallback_rates);
    let rates = snapshot
        .price_for(
            &candidate.provider_id,
            candidate.provider_kind,
            &candidate.upstream_model_id,
        )
        .map_or(fallback, |entry| entry.base_rates);
    estimate_reservation_cost_micro_usd(input_tokens, 0.0, &rates, None, None)
}

/// Fail-closed AEAD unsealing using only the immutable snapshot.
fn unseal_upstream_key(
    state: &ProxyAuthState,
    snapshot: &ConfigSnapshot,
    key: &UpstreamKeyRecord,
) -> Result<String, ()> {
    let Some(master_key) = state.master_key.clone() else {
        state
            .runtime_state
            .quarantine_key(key.id, "master_key_unavailable");
        return Err(());
    };
    let sealer =
        XChaCha20Poly1305Sealer::new(master_key, snapshot.instance_id, Arc::new(OsEntropy));
    let master_key_id = key.master_key_id.as_bytes();
    let key_id = key.id.to_string();
    let provider_id = key.provider_id.to_string();
    match sealer.open(
        "upstream_key",
        &[master_key_id, key_id.as_bytes(), provider_id.as_bytes()],
        &key.secret_enc,
    ) {
        Ok(plaintext) => {
            use llmroller_core::seams::ExposeSecret as _;
            String::from_utf8(plaintext.expose_secret().to_vec()).map_err(|_| {
                state
                    .runtime_state
                    .quarantine_key(key.id, "invalid_utf8_secret");
            })
        }
        Err(_) => {
            state
                .runtime_state
                .quarantine_key(key.id, "aead_unseal_failed");
            Err(())
        }
    }
}

/// Deterministic in-memory pin key; the raw session string never leaves the process.
fn session_pin_key(
    headers: &HeaderMap,
    client_key: &ClientKeyRecord,
    canonical_id: &str,
) -> Option<PinKey> {
    let header_vec: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let (raw_session, _) =
        llmroller_core::session::extract_session_precedence(&header_vec, &HashMap::new());
    if raw_session.is_empty() {
        return None;
    }

    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"llmroller.pin.v1\0");
    hasher.update(client_key.id.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(raw_session.as_bytes());
    let session_hash = llmroller_core::session::hex_lower(&hasher.finalize());

    Some(PinKey {
        client_key_id: client_key.id,
        canonical_id: canonical_id.to_string(),
        session_hash,
    })
}

/// Extract a `Retry-After` delay in milliseconds from an upstream response.
fn retry_after_ms(headers: &HeaderMap) -> Option<i64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|seconds| *seconds >= 0)
        .map(|seconds| seconds.saturating_mul(1000))
}

fn gate_error_response(error: &llmroller_core::accumulators::GateG7Error) -> Response {
    use llmroller_core::accumulators::GateG7Error;
    let (code, used, cap, retry_after) = match error {
        GateG7Error::ClientKeyBudgetExhausted {
            retry_after_secs,
            used_micro_usd,
            budget_micro_usd_month,
        } => (
            "client_key_budget_exhausted",
            *used_micro_usd,
            *budget_micro_usd_month,
            Some(*retry_after_secs),
        ),
        GateG7Error::UserGlobalBudgetExhausted {
            used_micro_usd,
            budget_micro_usd_month,
        } => (
            "user_budget_exhausted",
            *used_micro_usd,
            *budget_micro_usd_month,
            None,
        ),
        GateG7Error::ProviderShareCapExhausted {
            used_micro_usd,
            max_monthly_micro_usd,
        } => (
            "provider_share_budget_exhausted",
            *used_micro_usd,
            *max_monthly_micro_usd,
            None,
        ),
    };

    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({
            "error": {
                "code": code,
                "message": "monthly budget cap exhausted",
                "used_micro_usd": used,
                "budget_micro_usd": cap
            }
        })),
    )
        .into_response();
    if let Some(seconds) = retry_after
        && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
    {
        response.headers_mut().insert("retry-after", value);
    }
    response
}

/// Static usage attribution shared by the non-streaming and streaming finalizers.
#[derive(Debug, Clone)]
struct UsageContext {
    request_id: RequestId,
    client_key_id: ClientKeyId,
    owner_user_id: UserId,
    canonical_model: String,
    upstream_key_id: String,
    provider_id: String,
    provider_scope: String,
    share_id: Option<String>,
    upstream_model_id: String,
    endpoint_str: String,
    session_hash: Option<String>,
    pin_outcome: Option<String>,
    failovers: i64,
}

impl UsageContext {
    #[allow(clippy::too_many_arguments)]
    fn event(
        &self,
        ts: i64,
        status: u16,
        stream: bool,
        incomplete: bool,
        termination: &str,
        usage: &llmroller_proto::json_usage::ExtractedUsage,
        cost_micro_usd: i64,
        cost_source: CostSource,
        ttfb_ms: Option<i64>,
        latency_ms: Option<i64>,
    ) -> UsageEventRecord {
        UsageEventRecord {
            id: llmroller_core::ids::UsageEventId::generate().to_string(),
            ts,
            request_id: self.request_id,
            client_key_id: self.client_key_id,
            owner_user_id: self.owner_user_id,
            upstream_key_id: Some(self.upstream_key_id.clone()),
            provider_id: Some(self.provider_id.clone()),
            provider_scope: Some(self.provider_scope.clone()),
            share_id: self.share_id.clone(),
            canonical_id: Some(self.canonical_model.clone()),
            upstream_model_id: Some(self.upstream_model_id.clone()),
            endpoint: self.endpoint_str.clone(),
            protocol: Some(self.endpoint_str.clone()),
            session_hash: self.session_hash.clone(),
            status: i64::from(status),
            stream,
            incomplete,
            termination: termination.to_string(),
            trace_dropped: false,
            input_uncached_tokens: usage.input_uncached_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cost_micro_usd,
            cost_source,
            latency_ms,
            ttfb_ms,
            pin_outcome: self.pin_outcome.clone(),
            failovers: self.failovers,
            error_code: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn attempt_record(
    request_id: RequestId,
    attempt_no: i64,
    ts: i64,
    key_id: UpstreamKeyId,
    outcome: llmroller_core::usage::AttemptOutcome,
    http_status: Option<u16>,
    bytes_sent: usize,
    charged_micro_usd: i64,
    charge_basis: &str,
) -> UsageAttemptRecord {
    UsageAttemptRecord {
        request_id,
        attempt_no,
        ts,
        upstream_key_id: key_id.to_string(),
        share_id: None,
        outcome,
        http_status: http_status.map(i64::from),
        bytes_sent: bytes_sent as i64,
        charged_micro_usd,
        charge_basis: charge_basis.to_string(),
    }
}

/// Owns all post-stream accounting and finalizes exactly once on EOF or client drop.
struct StreamFinalizer {
    state: ProxyAuthState,
    usage: UsageContext,
    tap: SseUsageTap,
    reservation: Option<Reservation>,
    client_guard: Option<MonthReservationGuard>,
    user_guard: Option<MonthReservationGuard>,
    price_entry: Option<PriceEntry>,
    fallback_rates: Rates,
    attempts: Vec<UsageAttemptRecord>,
    started_ms: i64,
    ttfb_ms: Option<i64>,
    finalized: bool,
}

impl StreamFinalizer {
    fn finalize(&mut self, termination: Termination) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        let finished_ms = now_ms();
        let extracted = self.tap.finalize_usage();
        let cost = compute_request_cost(
            extracted
                .provider_cost_micro_usd
                .map(|micro| micro as f64 / 1_000_000.0),
            extracted.to_token_usage(),
            self.price_entry.as_ref(),
            self.fallback_rates,
            finished_ms,
        );

        if let Some(reservation) = self.reservation.take() {
            reservation.commit(cost.cost_micro_usd, finished_ms);
        }
        if let Some(guard) = self.client_guard.take() {
            guard.commit(cost.cost_micro_usd);
        }
        if let Some(guard) = self.user_guard.take() {
            guard.commit(cost.cost_micro_usd);
        }

        self.attempts.push(attempt_record(
            self.usage.request_id,
            0,
            finished_ms,
            self.usage
                .upstream_key_id
                .parse::<UpstreamKeyId>()
                .unwrap_or_else(|_| UpstreamKeyId::generate()),
            llmroller_core::usage::AttemptOutcome::Served,
            Some(200),
            0,
            cost.cost_micro_usd,
            cost.cost_source.as_str(),
        ));

        if let Some(writer) = &self.state.writer {
            let event = self.usage.event(
                finished_ms,
                200,
                true,
                termination.is_incomplete(),
                termination.as_str(),
                &extracted,
                cost.cost_micro_usd,
                cost.cost_source,
                self.ttfb_ms,
                Some(finished_ms - self.started_ms),
            );
            writer
                .queues
                .usage_writer
                .enqueue(event, std::mem::take(&mut self.attempts));
        }
    }
}

impl Drop for StreamFinalizer {
    fn drop(&mut self) {
        self.finalize(Termination::ClientGone);
    }
}

struct StreamState {
    upstream:
        std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    pending: VecDeque<Bytes>,
    forwarder: SseChunkForwarder,
    finalizer: Option<StreamFinalizer>,
    lifecycle: RequestLifecycle,
    done: bool,
}

/// Core inference request handler coordinating admission, editing, and forwarding.
pub async fn handle_inference_request(
    state: ProxyAuthState,
    endpoint: EndpointKind,
    headers: HeaderMap,
    body: Bytes,
    client_key: Option<ClientKeyRecord>,
) -> Response {
    let endpoint_str = match endpoint {
        EndpointKind::Chat => "chat",
        EndpointKind::Responses => "responses",
        EndpointKind::Messages => "messages",
        _ => "chat",
    };
    let protocol = endpoint_str;

    // 1. Acquire preflight memory budget permit (up to 2,000 ms)
    let permit = match state
        .budget_semaphore
        .acquire(body.len(), Duration::from_millis(2000))
        .await
    {
        Ok(permit) => permit,
        Err(error) => {
            crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
            return (
                error.status_code(),
                [("retry-after", error.retry_after_secs().to_string())],
                Json(serde_json::json!({
                    "error": {
                        "code": "gateway_busy",
                        "message": error.to_string()
                    }
                })),
            )
                .into_response();
        }
    };

    // 2. Scan and validate body envelope and duplicate keys
    let scan = match scan_top_level(&body) {
        Ok(scan) => scan,
        Err(error) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "code": "invalid_request_error",
                        "message": error.to_string()
                    }
                })),
            )
                .into_response();
        }
    };

    let Some((ref model_str, _)) = scan.model else {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "code": "invalid_request_error",
                    "message": "model is required in request body"
                }
            })),
        )
            .into_response();
    };

    let is_streaming = scan.stream.as_ref().is_some_and(|(stream, _)| *stream);
    let now = now_ms();
    let request_id = RequestId::generate();

    let snapshot = state.snapshot.load();
    // 3. Canonical model resolution is a pure in-memory lookup (PERF-1).
    let model = match snapshot.resolve_model(model_str) {
        Ok(model) => model.clone(),
        Err(llmroller_core::snapshot::SnapshotResolveError::NotFound) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": {
                        "code": "model_not_found",
                        "message": format!("model {model_str} not found in catalog")
                    }
                })),
            )
                .into_response();
        }
        Err(llmroller_core::snapshot::SnapshotResolveError::Ambiguous) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": {
                        "code": "ambiguous_model",
                        "message": format!("model {model_str} matches more than one catalog entry")
                    }
                })),
            )
                .into_response();
        }
        Err(llmroller_core::snapshot::SnapshotResolveError::KillSwitched) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": {
                        "code": "model_disabled",
                        "message": format!("model {model_str} is disabled")
                    }
                })),
            )
                .into_response();
        }
        Err(llmroller_core::snapshot::SnapshotResolveError::RedirectLoop) => {
            crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {
                        "code": "model_redirect_loop",
                        "message": "catalog redirect chain contains a cycle"
                    }
                })),
            )
                .into_response();
        }
    };

    let Some(caller) = client_key.clone() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    // 4. Gate G6: client key model allow-list.
    if let Some(ref allowed) = caller.allowed_models
        && !allowed
            .iter()
            .any(|entry| entry == &model.canonical_id || entry == model_str)
    {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": {
                    "code": "permission_error",
                    "message": format!("model {} not permitted on this key", model.canonical_id)
                }
            })),
        )
            .into_response();
    }

    let scope = parse_scope(&caller.routing_scope);
    let candidates = collect_candidates(&snapshot, &model, &caller, scope, now);
    if candidates.is_empty() {
        crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "code": "no_healthy_key",
                    "message": "no eligible upstream route available for this caller"
                }
            })),
        )
            .into_response();
    }

    let eligibility: Vec<EligibilityRoute> = candidates
        .iter()
        .map(|candidate| {
            eligibility_route(
                &snapshot,
                &state.runtime_state,
                candidate,
                &model,
                &caller,
                now,
            )
        })
        .collect();
    let shares = snapshot
        .shares_by_grantee
        .get(&caller.owner_user_id)
        .map_or(&[][..], Vec::as_slice);
    let owner_disabled = snapshot
        .users
        .get(&caller.owner_user_id)
        .is_none_or(|user| user.disabled);
    let can_use_global = snapshot
        .users
        .get(&caller.owner_user_id)
        .is_some_and(|user| user.can_use_global_providers);
    let context = CallerContext {
        caller_user_id: &caller.owner_user_id,
        can_use_global,
        owner_disabled,
        model_exposed: model.exposed,
        merge_state: model.merge_state,
        inbound_protocol: protocol,
        model_canonical_id: &model.canonical_id,
    };

    let (selected_tier, eligible_pool) = match select_pool(&eligibility, &context, shares, scope) {
        Ok(selection) => selection,
        Err(PoolSelectionError::MonthlyCapExhausted) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": {
                        "code": "no_eligible_route",
                        "message": "monthly budget capacity exhausted"
                    }
                })),
            )
                .into_response();
        }
        Err(PoolSelectionError::NoEligibleRoute) => {
            crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": {
                        "code": "no_healthy_key",
                        "message": "no eligible upstream route available for this caller"
                    }
                })),
            )
                .into_response();
        }
    };

    let eligible_keys: HashSet<UpstreamKeyId> = eligible_pool
        .iter()
        .map(|candidate| candidate.upstream_key_id)
        .collect();
    let tier_candidates: Vec<RouteCandidate> = eligible_pool
        .iter()
        .filter_map(|eligible| {
            candidates
                .iter()
                .find(|candidate| candidate.upstream_key_id == eligible.upstream_key_id)
                .cloned()
        })
        .collect();
    if tier_candidates.is_empty() {
        crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "code": "no_healthy_key",
                    "message": "no eligible upstream route available for this caller"
                }
            })),
        )
            .into_response();
    }

    // Stage 1: Gate G7 monthly caps are reserved before any upstream connection.
    let estimate = estimate_cost_micro_usd(&snapshot, &tier_candidates[0], 0).max(
        estimate_reservation_cost_micro_usd(
            estimate_input_tokens(&body),
            0.0,
            &Rates::from(&snapshot.settings.unpriced_fallback_rates),
            None,
            None,
        ),
    );
    let client_guard = match state.runtime_state.accumulators.try_reserve(
        llmroller_core::accumulators::BudgetScopeId::ClientKey(caller.id),
        caller.budget_micro_usd_month,
        estimate,
        now,
    ) {
        Ok(guard) => Some(guard),
        Err(error) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return gate_error_response(&error);
        }
    };
    let user_cap = snapshot
        .users
        .get(&caller.owner_user_id)
        .and_then(|user| user.global_budget_micro_usd_month);
    let user_guard = match state.runtime_state.accumulators.try_reserve(
        llmroller_core::accumulators::BudgetScopeId::User(caller.owner_user_id),
        user_cap,
        estimate,
        now,
    ) {
        Ok(guard) => Some(guard),
        Err(error) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return gate_error_response(&error);
        }
    };
    let mut client_guard = client_guard;
    let mut user_guard = user_guard;

    // Stage 6: sticky pin acquisition for an identified session.
    let strategy = model
        .routing_strategy
        .unwrap_or(snapshot.settings.default_routing_strategy);
    let pin_key = session_pin_key(&headers, &caller, &model.canonical_id);
    let pin_ttl_ms = snapshot.settings.pin_ttl_secs.saturating_mul(1000);
    let clock = SystemClock::new();
    let mut pin_ticket: Option<PinTicket> = None;
    let mut ordered: Vec<RouteCandidate> = Vec::new();

    if let Some(pin_key) = &pin_key {
        let ticket = state.pin_store.acquire(
            pin_key,
            &clock,
            pin_ttl_ms,
            snapshot.settings.repin_on_personal_recovery,
            snapshot.settings.recovery_repin_min_idle_secs,
            scope,
            Some(selected_tier),
            |key_id| eligible_keys.contains(&key_id),
            |tried| {
                select_next_candidate(
                    &state.runtime_state,
                    strategy,
                    caller.owner_user_id,
                    model.id,
                    protocol,
                    selected_tier,
                    &tier_candidates,
                    tried,
                    now,
                )
                .map(|candidate| (candidate.upstream_key_id, candidate.pool_tier))
            },
        );
        if let Some(ticket) = ticket {
            if let Some(candidate) = tier_candidates
                .iter()
                .find(|candidate| candidate.upstream_key_id == ticket.upstream_key_id)
            {
                ordered.push(candidate.clone());
            }
            pin_ticket = Some(ticket);
        }
    }

    // Stage 7: one selector call per attempt. Precomputing the whole ordering
    // would advance cyclic strategies once per candidate and starve later keys.
    let pin_context = PinFailureContext {
        runtime: state.runtime_state.as_ref(),
        strategy,
        caller: caller.owner_user_id,
        model_pk: model.id,
        protocol,
        selected_tier,
        tier_candidates: &tier_candidates,
        eligible_keys: &eligible_keys,
        now,
    };
    let mut pinned_order = ordered;
    let mut tried: Vec<UpstreamKeyId> = Vec::new();

    let mut attempts: Vec<UsageAttemptRecord> = Vec::new();
    let mut last_status = StatusCode::BAD_GATEWAY;
    let mut last_retry_after_ms: Option<i64> = None;
    let mut failovers = 0_i64;
    let mut permit = Some(permit);
    let mut attempt_no: usize = 0;

    loop {
        let candidate = if let Some(candidate) = pinned_order.pop() {
            candidate
        } else {
            match select_next_candidate(
                &state.runtime_state,
                strategy,
                caller.owner_user_id,
                model.id,
                protocol,
                selected_tier,
                &tier_candidates,
                &tried,
                now,
            ) {
                Some(candidate) => candidate,
                None => break,
            }
        };
        tried.push(candidate.upstream_key_id);
        attempt_no += 1;
        let Some(key) = snapshot
            .upstream_keys
            .get(&candidate.upstream_key_id)
            .cloned()
        else {
            continue;
        };
        let secret = match unseal_upstream_key(&state, &snapshot, &key) {
            Ok(secret) => secret,
            Err(()) => {
                attempts.push(attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    llmroller_core::usage::AttemptOutcome::ConnectError,
                    None,
                    body.len(),
                    0,
                    "none",
                ));
                last_status = StatusCode::SERVICE_UNAVAILABLE;
                continue;
            }
        };

        let attempt_estimate = estimate_reservation_cost_micro_usd(
            estimate_input_tokens(&body),
            0.0,
            &snapshot
                .price_for(
                    &candidate.provider_id,
                    candidate.provider_kind,
                    &candidate.upstream_model_id,
                )
                .map_or(
                    Rates::from(&snapshot.settings.unpriced_fallback_rates),
                    |entry| entry.base_rates,
                ),
            None,
            None,
        );
        let reservation = match state.runtime_state.reserve_key_windows(
            &snapshot,
            candidate.upstream_key_id,
            attempt_estimate,
            now,
        ) {
            Ok(reservation) => reservation,
            Err(_) => {
                attempts.push(attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    llmroller_core::usage::AttemptOutcome::Http429,
                    Some(429),
                    body.len(),
                    0,
                    "none",
                ));
                last_status = StatusCode::TOO_MANY_REQUESTS;
                continue;
            }
        };

        let path = match endpoint {
            EndpointKind::Chat => "/chat/completions",
            EndpointKind::Responses => "/responses",
            EndpointKind::Messages => "/messages",
            _ => "/chat/completions",
        };
        let target_url = format!("{}{path}", candidate.base_url.trim_end_matches('/'));
        if crate::ssrf::validate_url(&target_url, candidate.allow_private_network).is_err() {
            attempts.push(attempt_record(
                request_id,
                attempt_no as i64,
                now,
                candidate.upstream_key_id,
                llmroller_core::usage::AttemptOutcome::ConnectError,
                None,
                body.len(),
                0,
                "none",
            ));
            last_status = StatusCode::BAD_GATEWAY;
            continue;
        }
        let client = match crate::ssrf::create_guarded_client(candidate.allow_private_network) {
            Ok(client) => client,
            Err(_) => continue,
        };

        let (edited_body, strip_injected_usage) = match edit_body(
            body.clone(),
            candidate.pool_tier(),
            Some(&candidate.upstream_model_id),
            if is_streaming {
                ChatStreamMode::Inject
            } else {
                ChatStreamMode::Native
            },
            None,
        ) {
            Ok(edited) => edited,
            Err(_) => (body.clone(), false),
        };

        let outbound_headers = match llmroller_proto::headers::apply_request_header_pipeline(
            &headers,
            candidate.pool_tier(),
            Some(("authorization", &format!("Bearer {secret}"))),
            &[],
            &[],
        ) {
            Ok(headers) => headers,
            Err(_) => headers.clone(),
        };

        let mut request_builder = client.post(&target_url).body(edited_body.clone());
        for (name, value) in &outbound_headers {
            request_builder = request_builder.header(name, value);
        }

        let attempt_started = now_ms();
        let response = match request_builder.send().await {
            Ok(response) => response,
            Err(_) => {
                state.runtime_state.record_upstream_failure(
                    candidate.provider_id,
                    candidate.upstream_key_id,
                    llmroller_core::usage::AttemptOutcome::ConnectError,
                    None,
                    now,
                );
                attempts.push(attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    llmroller_core::usage::AttemptOutcome::ConnectError,
                    Some(502),
                    body.len(),
                    0,
                    "none",
                ));
                if let Some(replacement) = note_pre_first_byte_failure(
                    &state,
                    pin_key.as_ref(),
                    pin_ticket.as_ref(),
                    candidate.upstream_key_id,
                    &pin_context,
                ) && let Some(next) = tier_candidates
                    .iter()
                    .find(|entry| entry.upstream_key_id == replacement.upstream_key_id)
                {
                    pinned_order.push(next.clone());
                    pin_ticket = Some(replacement);
                }
                failovers += 1;
                last_status = StatusCode::BAD_GATEWAY;
                continue;
            }
        };

        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS
            || status == StatusCode::UNAUTHORIZED
            || status == StatusCode::PAYMENT_REQUIRED
            || status == StatusCode::FORBIDDEN
            || status.is_server_error()
        {
            last_status = status;
            last_retry_after_ms = retry_after_ms(response.headers());
            let outcome = llmroller_core::usage::AttemptOutcome::from_http_status(status.as_u16());
            state.runtime_state.record_upstream_failure(
                candidate.provider_id,
                candidate.upstream_key_id,
                outcome,
                last_retry_after_ms,
                now,
            );
            attempts.push(attempt_record(
                request_id,
                attempt_no as i64,
                now,
                candidate.upstream_key_id,
                outcome,
                Some(status.as_u16()),
                body.len(),
                0,
                "none",
            ));
            if let Some(replacement) = note_pre_first_byte_failure(
                &state,
                pin_key.as_ref(),
                pin_ticket.as_ref(),
                candidate.upstream_key_id,
                &pin_context,
            ) && let Some(next) = tier_candidates
                .iter()
                .find(|entry| entry.upstream_key_id == replacement.upstream_key_id)
            {
                pinned_order.push(next.clone());
                pin_ticket = Some(replacement);
            }
            failovers += 1;
            continue;
        }

        if is_streaming {
            if status.is_success() {
                let upstream_headers = response.headers().clone();
                let mut buffer = PreFirstFrameBuffer::new(status, upstream_headers.clone());
                let mut upstream = Box::pin(response.bytes_stream());
                let mut tap = SseUsageTap::new(endpoint);
                let mut first_chunk: Option<Bytes> = None;
                let mut failure = false;

                loop {
                    let next = tokio::time::timeout(STREAM_IDLE_TIMEOUT, upstream.next()).await;
                    match next {
                        Ok(Some(Ok(chunk))) => {
                            tap.feed_chunk(&chunk);
                            match buffer.push_chunk(&chunk) {
                                Ok(Some(bytes)) => {
                                    first_chunk = Some(bytes);
                                    break;
                                }
                                Ok(None) => {}
                                Err(_) => {
                                    failure = true;
                                    break;
                                }
                            }
                        }
                        Ok(Some(Err(_))) | Err(_) => {
                            failure = true;
                            break;
                        }
                        Ok(None) => match buffer.flush_remaining() {
                            Some(bytes) => {
                                first_chunk = Some(bytes);
                                break;
                            }
                            None => {
                                failure = true;
                                break;
                            }
                        },
                    }
                }

                if failure {
                    state.runtime_state.record_upstream_failure(
                        candidate.provider_id,
                        candidate.upstream_key_id,
                        llmroller_core::usage::AttemptOutcome::ErrorFrame,
                        None,
                        now,
                    );
                    attempts.push(attempt_record(
                        request_id,
                        attempt_no as i64,
                        now,
                        candidate.upstream_key_id,
                        llmroller_core::usage::AttemptOutcome::ErrorFrame,
                        Some(200),
                        body.len(),
                        0,
                        "none",
                    ));
                    if let Some(replacement) = note_pre_first_byte_failure(
                        &state,
                        pin_key.as_ref(),
                        pin_ticket.as_ref(),
                        candidate.upstream_key_id,
                        &pin_context,
                    ) && let Some(next) = tier_candidates
                        .iter()
                        .find(|entry| entry.upstream_key_id == replacement.upstream_key_id)
                    {
                        pinned_order.push(next.clone());
                        pin_ticket = Some(replacement);
                    }
                    failovers += 1;
                    last_status = StatusCode::BAD_GATEWAY;
                    continue;
                }

                let ttfb_ms = (now_ms() - attempt_started).max(0);
                state.runtime_state.record_upstream_success(
                    candidate.provider_id,
                    candidate.upstream_key_id,
                    Some(candidate.route_id()),
                    Some(ttfb_ms as u64),
                    now,
                );
                if let (Some(pin_key), Some(ticket)) = (&pin_key, &pin_ticket) {
                    let _ = state
                        .pin_store
                        .first_valid_frame(pin_key, ticket, now, pin_ttl_ms);
                }
                drop(permit.take());

                let usage = UsageContext {
                    request_id,
                    client_key_id: caller.id,
                    owner_user_id: caller.owner_user_id,
                    canonical_model: model.canonical_id.clone(),
                    upstream_key_id: candidate.upstream_key_id.to_string(),
                    provider_id: candidate.provider_id.to_string(),
                    provider_scope: candidate.pool_tier().to_string(),
                    share_id: candidate.share_id.map(|share| share.to_string()),
                    upstream_model_id: candidate.upstream_model_id.clone(),
                    endpoint_str: endpoint_str.to_string(),
                    session_hash: pin_key.as_ref().map(|key| key.session_hash.clone()),
                    pin_outcome: pin_ticket
                        .as_ref()
                        .map(|ticket| ticket.outcome.as_str().to_string()),
                    failovers,
                };
                let finalizer = StreamFinalizer {
                    state: state.clone(),
                    usage,
                    tap,
                    reservation,
                    client_guard: client_guard.take(),
                    user_guard: user_guard.take(),
                    price_entry: snapshot
                        .price_for(
                            &candidate.provider_id,
                            candidate.provider_kind,
                            &candidate.upstream_model_id,
                        )
                        .cloned(),
                    fallback_rates: Rates::from(&snapshot.settings.unpriced_fallback_rates),
                    attempts: std::mem::take(&mut attempts),
                    started_ms: attempt_started,
                    ttfb_ms: Some(ttfb_ms),
                    finalized: false,
                };

                let mut pending = VecDeque::new();
                if let Some(bytes) = first_chunk {
                    for part in SseChunkForwarder::new(strip_injected_usage).process_chunk(&bytes) {
                        pending.push_back(part);
                    }
                }

                let lifecycle = RequestLifecycle::new(request_id, attempt_started, None);
                let stream_state = StreamState {
                    upstream,
                    pending,
                    forwarder: SseChunkForwarder::new(strip_injected_usage),
                    finalizer: Some(finalizer),
                    lifecycle,
                    done: false,
                };
                let metered = stream::unfold(stream_state, |mut stream_state| async move {
                    loop {
                        if let Some(bytes) = stream_state.pending.pop_front() {
                            stream_state.lifecycle.record_first_frame(now_ms());
                            return Some((Ok::<Bytes, std::io::Error>(bytes), stream_state));
                        }
                        if stream_state.done {
                            return None;
                        }
                        match stream_state.upstream.next().await {
                            Some(Ok(chunk)) => {
                                if let Some(finalizer) = &mut stream_state.finalizer {
                                    finalizer.tap.feed_chunk(&chunk);
                                }
                                let parts = stream_state.forwarder.process_chunk(&chunk);
                                stream_state.pending.extend(parts);
                            }
                            Some(Err(_)) => {
                                if let Some(finalizer) = &mut stream_state.finalizer {
                                    finalizer.finalize(Termination::UpstreamTruncated);
                                }
                                return None;
                            }
                            None => {
                                if let Some(tail) = stream_state.forwarder.finish() {
                                    stream_state.pending.push_back(tail);
                                }
                                if let Some(finalizer) = &mut stream_state.finalizer {
                                    finalizer.finalize(Termination::Completed);
                                }
                                stream_state.done = true;
                                if let Some(bytes) = stream_state.pending.pop_front() {
                                    return Some((
                                        Ok::<Bytes, std::io::Error>(bytes),
                                        stream_state,
                                    ));
                                }
                                return None;
                            }
                        }
                    }
                });

                let mut response_headers = llmroller_proto::headers::apply_response_header_pipeline(
                    &upstream_headers,
                    candidate.pool_tier(),
                    true,
                    &request_id.to_string(),
                    &[],
                );
                response_headers
                    .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
                let mut builder = Response::builder().status(status);
                for (name, value) in response_headers {
                    if let Some(name) = name {
                        builder = builder.header(name, value);
                    }
                }
                crate::metrics::record_request_metric(endpoint_str, "2xx", "stream");
                return builder
                    .body(Body::from_stream(metered))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
            // Non-retryable client error: return the upstream status verbatim.
            let headers_out = llmroller_proto::headers::apply_response_header_pipeline(
                response.headers(),
                candidate.pool_tier(),
                false,
                &request_id.to_string(),
                &[],
            );
            let bytes = response.bytes().await.unwrap_or_default();
            drop(reservation);
            let mut builder = Response::builder().status(status);
            for (name, value) in headers_out {
                if let Some(name) = name {
                    builder = builder.header(name, value);
                }
            }
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return builder
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }

        // Non-streaming response.
        let response_headers = llmroller_proto::headers::apply_response_header_pipeline(
            response.headers(),
            candidate.pool_tier(),
            false,
            &request_id.to_string(),
            &[],
        );
        let status_value = response.status();
        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let body_bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => {
                attempts.push(attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    llmroller_core::usage::AttemptOutcome::ConnectError,
                    Some(502),
                    body.len(),
                    0,
                    "none",
                ));
                failovers += 1;
                last_status = StatusCode::BAD_GATEWAY;
                continue;
            }
        };

        if !status_value.is_success() {
            // Non-retryable upstream response (e.g. 400): pass through unchanged.
            let mut builder = Response::builder().status(status_value);
            for (name, value) in response_headers {
                if let Some(name) = name {
                    builder = builder.header(name, value);
                }
            }
            if let Some(content_type) = content_type {
                builder = builder.header(CONTENT_TYPE, content_type);
            }
            drop(reservation);
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return builder
                .body(Body::from(body_bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }

        let json_value =
            serde_json::from_slice(&body_bytes).unwrap_or_else(|_| serde_json::json!({}));
        let extracted = extract_json_usage(endpoint, &json_value).unwrap_or_default();
        let price_entry = snapshot
            .price_for(
                &candidate.provider_id,
                candidate.provider_kind,
                &candidate.upstream_model_id,
            )
            .cloned();
        let cost = compute_request_cost(
            extracted
                .provider_cost_micro_usd
                .map(|micro| micro as f64 / 1_000_000.0),
            extracted.to_token_usage(),
            price_entry.as_ref(),
            Rates::from(&snapshot.settings.unpriced_fallback_rates),
            now,
        );

        if let Some(reservation) = reservation {
            reservation.commit(cost.cost_micro_usd, now);
        }
        if let Some(guard) = client_guard.take() {
            guard.commit(cost.cost_micro_usd);
        }
        if let Some(guard) = user_guard.take() {
            guard.commit(cost.cost_micro_usd);
        }

        state.runtime_state.record_upstream_success(
            candidate.provider_id,
            candidate.upstream_key_id,
            Some(candidate.route_id()),
            None,
            now,
        );
        if let (Some(pin_key), Some(ticket)) = (&pin_key, &pin_ticket) {
            let _ = state
                .pin_store
                .first_valid_frame(pin_key, ticket, now, pin_ttl_ms);
        }
        drop(permit.take());

        attempts.push(attempt_record(
            request_id,
            attempt_no as i64,
            now,
            candidate.upstream_key_id,
            llmroller_core::usage::AttemptOutcome::Served,
            Some(status_value.as_u16()),
            body.len(),
            cost.cost_micro_usd,
            cost.cost_source.as_str(),
        ));

        if let Some(writer) = &state.writer {
            let usage = UsageContext {
                request_id,
                client_key_id: caller.id,
                owner_user_id: caller.owner_user_id,
                canonical_model: model.canonical_id.clone(),
                upstream_key_id: candidate.upstream_key_id.to_string(),
                provider_id: candidate.provider_id.to_string(),
                provider_scope: candidate.pool_tier().to_string(),
                share_id: candidate.share_id.map(|share| share.to_string()),
                upstream_model_id: candidate.upstream_model_id.clone(),
                endpoint_str: endpoint_str.to_string(),
                session_hash: pin_key.as_ref().map(|key| key.session_hash.clone()),
                pin_outcome: pin_ticket
                    .as_ref()
                    .map(|ticket| ticket.outcome.as_str().to_string()),
                failovers,
            };
            let event = usage.event(
                now,
                status_value.as_u16(),
                false,
                false,
                "completed",
                &extracted,
                cost.cost_micro_usd,
                cost.cost_source,
                None,
                Some(now_ms() - attempt_started),
            );
            writer
                .queues
                .usage_writer
                .enqueue(event, std::mem::take(&mut attempts));
        }

        crate::metrics::record_request_metric(endpoint_str, "2xx", "completed");
        let mut builder = Response::builder().status(status_value);
        for (name, value) in response_headers {
            if let Some(name) = name {
                builder = builder.header(name, value);
            }
        }
        return builder
            .body(Body::from(body_bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
    if let Some(writer) = &state.writer {
        let usage = UsageContext {
            request_id,
            client_key_id: caller.id,
            owner_user_id: caller.owner_user_id,
            canonical_model: model.canonical_id.clone(),
            upstream_key_id: String::new(),
            provider_id: String::new(),
            provider_scope: String::new(),
            share_id: None,
            upstream_model_id: model.canonical_id.clone(),
            endpoint_str: endpoint_str.to_string(),
            session_hash: None,
            pin_outcome: None,
            failovers,
        };
        let event = usage.event(
            now,
            last_status.as_u16(),
            is_streaming,
            true,
            "upstream_error",
            &llmroller_proto::json_usage::ExtractedUsage::default(),
            0,
            CostSource::EstimatedIncomplete,
            None,
            Some(now_ms() - now),
        );
        writer
            .queues
            .usage_writer
            .enqueue(event, std::mem::take(&mut attempts));
    }

    let mut response = (
        last_status,
        Json(serde_json::json!({
            "error": {
                "code": if last_status == StatusCode::TOO_MANY_REQUESTS { "upstream_rate_limited" } else { "upstream_error" },
                "message": "all upstream attempts failed"
            }
        })),
    )
        .into_response();
    if let Some(delay) = last_retry_after_ms
        && let Ok(value) = HeaderValue::from_str(&(delay / 1000).max(1).to_string())
    {
        response.headers_mut().insert("retry-after", value);
    }
    response
}
