#![forbid(unsafe_code)]
//! Orchestrator for /v1/* inference endpoints and pre-flight body budget admission.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#53-orchestration-and-request-flow-state-machine
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-performance-and-resource-efficiency-perf
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#27-sub-phase-27-v1-orchestrator-v1-pre-flight-body-memory-budget-graceful-sigterm-drain-and-early-load-harness

use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_compression::tokio::bufread::{
    BrotliDecoder, DeflateDecoder, GzipDecoder, ZlibDecoder, ZstdDecoder,
};
use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE, HeaderName, HeaderValue};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::stream;
use llmroller_core::accumulators::{BudgetScopeId, GateG7Error, MonthReservationGuard};
use llmroller_core::breaker::OriginKey;
use llmroller_core::client_key::ClientKeyRecord;
use llmroller_core::eligibility::{
    CallerContext, CandidatePoolTier, CandidateRoute as EligibilityRoute, PoolSelectionError,
    select_pool,
};
use llmroller_core::health::StrikeResult;
use llmroller_core::ids::{
    ClientKeyId, ModelPk, ProviderId, RequestId, ShareId, UpstreamKeyId, UsageEventId, UserId,
};
use llmroller_core::pins::{PinEntry, PinKey, PinOutcome, PinTicket};
use llmroller_core::pricing::{
    CostSource, PriceEntry, Rates, compute_request_cost, estimate_input_tokens,
};
use llmroller_core::provider::{ProviderAuthScheme, ProviderEndpoint, ProviderKind};
use llmroller_core::rbac::{PoolTier, Role, RoutingScope};
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::seams::{Clock as _, EntropySource, ExposeSecret as _, OsEntropy, SystemClock};
use llmroller_core::snapshot::{
    ConfigSnapshot, ProviderRecord, SnapshotModel, SnapshotModelRoute, UpstreamKeyRecord,
};
use llmroller_core::strategy::{RouteId, RoutingStrategy, WrrGroupKey};
use llmroller_core::windows::{Reservation, estimate_reservation_cost_micro_usd};
use llmroller_proto::adapters::{
    EndpointKind, ErrorClass, classify_error_with_classifiers, get_adapter,
};
use llmroller_proto::affinity::{apply_outbound_session_affinity, compute_salted_affinity_token};
use llmroller_proto::body_edit::{
    ChatStreamMode, edit_body, edit_body_with_session_limit, scan_top_level,
};
use llmroller_proto::json_usage::extract_json_usage;
use llmroller_proto::sse_tap::SseUsageTap;
use llmroller_proto::trace_tap::{
    NormalizedTurnOutput, TraceTap, prepare_request_blocks, replace_binary_blocks,
};
use llmroller_store::crypto::{SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_store::pins::ResponseLinkRow;
use llmroller_store::traces::{SealJob, StoredTurnTraceV2};
use llmroller_store::usage_writer::{UsageAttemptRecord, UsageEventRecord};
use llmroller_store::writer::{DurableKey, DurableOp, WriterHandle};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::{ReaderStream, StreamReader};

use crate::auth::ProxyAuthState;
use crate::failover::{TryOutcome, select_terminal_error};
use crate::forward::{ForwardError, PreFirstFrameBuffer, SseChunkForwarder, decompress_chunk};
use crate::lifecycle::{RequestLifecycle, Termination};

/// Maximum upstream tries per request; mirrors the `usage_attempts.attempt_no` bound.
const MAX_UPSTREAM_ATTEMPTS: usize = 3;

/// Maximum bytes retained from an upstream error body for classification and passthrough.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Size of the body prefix passed to the ordered error classifier (Section 10.5).
const ERROR_CLASSIFY_BYTES: usize = 8 * 1024;

/// Fallback lifetime of a `PlanRestricted` route block until the next `/models` refresh.
const PROTOCOL_BLOCK_MS: i64 = 6 * 60 * 60 * 1_000;

/// How long an auxiliary endpoint probe result is cached (API-8).
const AUX_UNSUPPORTED_TTL_MS: i64 = 60 * 60 * 1_000;

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

/// Builds a protocol-aware OpenAI or Anthropic error envelope (`API-1`, `API-2`, `API-8`).
#[must_use]
pub fn build_protocol_error_response(
    is_messages_path: bool,
    status: StatusCode,
    error_type: &str,
    code: &str,
    message: &str,
    request_id: Option<&str>,
    retry_after_secs: Option<u64>,
) -> Response {
    let body = if is_messages_path {
        serde_json::json!({
            "type": "error",
            "error": {
                "type": error_type,
                "code": code,
                "message": message,
            },
            "request_id": request_id,
        })
    } else {
        serde_json::json!({
            "error": {
                "message": message,
                "type": error_type,
                "param": serde_json::Value::Null,
                "code": code,
            }
        })
    };
    let mut response = (status, Json(body)).into_response();
    if let Some(rid) = request_id
        && let Ok(val) = HeaderValue::from_str(rid)
    {
        response.headers_mut().insert("x-request-id", val);
    }
    if let Some(secs) = retry_after_secs
        && let Ok(val) = HeaderValue::from_str(&secs.max(1).to_string())
    {
        response.headers_mut().insert("retry-after", val);
    }
    response
}

/// Maps a stable LLMRoller error code onto the OpenAI `type` vocabulary.
///
/// The OpenAI schema only requires a string, but clients branch on the
/// conventional values, so keep the vocabulary stable and predictable.
#[must_use]
fn openai_error_type_for_code(code: &str) -> &'static str {
    match code {
        // Authentication / authorization.
        "authentication_error"
        | "permission_error"
        | "model_disabled"
        | "ip_restricted"
        | "service_account_disabled"
        | "client_key_paused" => "permission_error",
        // Credits / rate shaping.
        "rate_limit_error"
        | "client_key_budget_exhausted"
        | "user_budget_exhausted"
        | "provider_share_budget_exhausted"
        | "rate_limit_exceeded" => "rate_limit_error",
        // Request shape.
        "invalid_request_error"
        | "invalid_encoding"
        | "model_not_found"
        | "model_redirect_loop"
        | "ambiguous_model_id"
        | "response_not_found" => "invalid_request_error",
        // Missing resources.
        "not_found_error" => "not_found_error",
        // Provider/transport failures.
        "upstream_unreachable"
        | "upstream_error"
        | "gateway_busy"
        | "no_eligible_route"
        | "upstream_truncated"
        | "timeout_error" => "server_error",
        _ => "server_error",
    }
}

/// Maps a stable LLMRoller error code onto the Anthropic `error.type` enum
/// (`AnthropicMessagesErrorResponse`, OpenRouter OpenAPI).
///
/// `server_error` is not a member of that enum, so transport failures must be
/// reported as `api_error` and saturation as `overloaded_error`.
#[must_use]
fn anthropic_error_type_for_code(code: &str) -> &'static str {
    match code {
        "authentication_error" => "authentication_error",
        "permission_error"
        | "model_disabled"
        | "ip_restricted"
        | "service_account_disabled"
        | "client_key_paused" => "permission_error",
        "rate_limit_error"
        | "client_key_budget_exhausted"
        | "user_budget_exhausted"
        | "provider_share_budget_exhausted"
        | "rate_limit_exceeded" => "rate_limit_error",
        "invalid_request_error"
        | "invalid_encoding"
        | "model_not_found"
        | "model_redirect_loop"
        | "ambiguous_model_id"
        | "response_not_found" => "invalid_request_error",
        "not_found_error" => "not_found_error",
        "gateway_busy" | "no_eligible_route" => "overloaded_error",
        "timeout_error" => "timeout_error",
        _ => "api_error",
    }
}

/// Builds a schema-conformant error envelope for the request's wire protocol.
///
/// This is the single entry point every data-plane error return must use so
/// OpenAI callers always receive `type`/`param` and Anthropic callers always
/// receive `type`/`code`/`request_id` (`F-021`), and so `Retry-After` can never
/// be forgotten on a 429/503 (`F-022`).
fn protocol_error(
    is_messages_path: bool,
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
    request_id: Option<&str>,
    retry_after_secs: Option<u64>,
) -> Response {
    let message = message.into();
    let error_type = if is_messages_path {
        anthropic_error_type_for_code(code)
    } else {
        openai_error_type_for_code(code)
    };
    build_protocol_error_response(
        is_messages_path,
        status,
        error_type,
        code,
        &message,
        request_id,
        retry_after_secs,
    )
}

fn json_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    protocol_error(false, status, code, message, None, None)
}

/// Anthropic errors always carry a concrete `request_id`, so generate one when
/// the caller has not created it yet (schema requires the field).
fn messages_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    let request_id = RequestId::generate().to_string();
    protocol_error(true, status, code, message, Some(&request_id), None)
}

fn response_not_found(response_id: &str) -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        "response_not_found",
        format!("response '{response_id}' not found"),
    )
}

fn permission_error(message: impl Into<String>) -> Response {
    json_error(StatusCode::FORBIDDEN, "permission_error", message)
}

fn rate_limit_error(retry_after_secs: u64) -> Response {
    protocol_error(
        false,
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limit_error",
        "the upstream key that holds this response is temporarily unavailable",
        None,
        Some(retry_after_secs),
    )
}

/// Load one durable response link for a routed Responses operation.
async fn load_response_link(
    state: &ProxyAuthState,
    response_id: &str,
) -> Result<Option<ResponseLinkRow>, sqlx::Error> {
    if let Some(writer) = &state.writer {
        let guard = writer.queues.durable_state.lock();
        match guard.get(&llmroller_store::writer::DurableKey::ResponseLink(
            response_id.to_string(),
        )) {
            Some(llmroller_store::writer::DurableOp::ResponseLinkUpsert(row)) => {
                return Ok(Some(row.clone()));
            }
            Some(llmroller_store::writer::DurableOp::ResponseLinkDelete { .. }) => {
                return Ok(None);
            }
            _ => {}
        }
    }
    let mut tx = state.pool.begin().await?;
    let link = llmroller_store::pins::get_response_link_tx(&mut tx, response_id).await?;
    tx.commit().await?;
    Ok(link)
}

/// Project the C1-C8 hard-route checks for a linked upstream key (RTE-12).
fn hard_route_checks(
    snapshot: &ConfigSnapshot,
    runtime: &RuntimeState,
    link: &ResponseLinkRow,
    caller: &ClientKeyRecord,
    protocol: &str,
    now: i64,
) -> (crate::failover::HardRouteChecks, u64) {
    use crate::failover::HardRouteChecks;

    let route = snapshot
        .routes_by_model
        .get(&link.model_pk)
        .and_then(|routes| {
            routes
                .iter()
                .find(|route| route.upstream_key_id == link.upstream_key_id)
        });
    let key = snapshot.upstream_keys.get(&link.upstream_key_id);
    let provider = key.and_then(|key| snapshot.providers.get(&key.provider_id));
    let model = snapshot.models.get(&link.model_pk);
    let scope = parse_scope(&caller.routing_scope);

    let c1 = key.is_some_and(|key| key.enabled)
        && provider.is_some_and(|provider| provider.enabled)
        && route.is_some_and(|route| {
            route.enabled && route.status == llmroller_core::catalog::RouteStatus::Active
        });

    let c2 = match (provider, key) {
        (Some(provider), Some(key)) => match provider.ownership_scope {
            llmroller_core::rbac::OwnershipScope::Global => {
                snapshot
                    .users
                    .get(&caller.owner_user_id)
                    .is_some_and(|user| user.can_use_global_providers)
                    && scope.allows_tier(PoolTier::Global)
            }
            llmroller_core::rbac::OwnershipScope::Personal => {
                if provider.owner_user_id == Some(caller.owner_user_id) {
                    scope.allows_tier(PoolTier::Own)
                } else {
                    let grantor_disabled = provider.owner_user_id.is_none_or(|owner| {
                        snapshot.users.get(&owner).is_none_or(|user| user.disabled)
                    });
                    let shared = snapshot
                        .effective_share(
                            caller.owner_user_id,
                            provider.id,
                            key.id,
                            provider.kind,
                            &provider.share_config_fingerprint,
                            grantor_disabled,
                        )
                        .is_some_and(|share| {
                            share.allowed_models.as_ref().is_none_or(|allowed| {
                                model.is_some_and(|model| allowed.contains(&model.canonical_id))
                            })
                        });
                    shared && scope.allows_tier(PoolTier::Shared)
                }
            }
        },
        _ => false,
    };

    let c3 = provider.is_none_or(|provider| {
        provider.ownership_scope != llmroller_core::rbac::OwnershipScope::Personal
            || provider.owner_user_id.is_none_or(|owner| {
                snapshot
                    .users
                    .get(&owner)
                    .is_some_and(|user| !user.disabled)
            })
    });

    let c4 = model.is_some_and(|model| {
        if model.exposed == Some(false) {
            return false;
        }
        if route.is_some_and(|route| is_conflict_minority(model, route)) {
            return false;
        }
        let is_global = provider.is_some_and(|provider| {
            provider.ownership_scope == llmroller_core::rbac::OwnershipScope::Global
        });
        if is_global {
            if model.exposed.is_none()
                && snapshot.settings.global_exposure_mode
                    == llmroller_core::settings::GlobalExposureMode::Intersection
                && !present_on_all_global_providers(snapshot, model)
            {
                return false;
            }
        } else {
            let pref_enabled = key.is_some_and(|key| {
                snapshot
                    .user_route_prefs
                    .get(&(caller.owner_user_id, key.id, model.id))
                    .is_none_or(|preference| preference.enabled)
            });
            if !pref_enabled {
                return false;
            }
        }
        true
    });

    let c5 = route.is_some_and(|route| {
        route.protocols.iter().any(|value| value == protocol)
            && route_healthy(route, now)
            && !runtime.is_route_protocol_blocked(
                route.upstream_key_id,
                route.model_pk,
                protocol,
                now,
            )
    });

    let c6 = key.is_some_and(|key| runtime.is_key_routable(key.id, now))
        && route.is_some_and(|route| route_healthy(route, now));
    let c7 = provider.is_some_and(|provider| {
        runtime.is_origin_admitted(
            &OriginKey::from_base_url(&provider.base_url, provider.allow_private_network),
            now,
        )
    });
    let c8 = key
        .is_some_and(|key| runtime.key_budget_admitted(snapshot, key.id, Some(link.model_pk), now));

    let retry_after = {
        let mut until = now.saturating_add(60_000);
        if let Some(key) = key {
            let health = runtime.key_health_entry(key.id);
            for candidate in [health.cooldown_until_ms, health.exhausted_until_ms]
                .into_iter()
                .flatten()
            {
                if candidate > now {
                    until = until.min(candidate);
                }
            }
        }
        if let Some(until_route) = route.and_then(|route| route.route_health_until)
            && until_route > now
        {
            until = until.min(until_route);
        }
        u64::try_from((until - now) / 1000).unwrap_or(60).max(1)
    };

    (
        HardRouteChecks {
            c1_c5_passed: c1 && c2 && c3 && c4 && c5,
            c6_c8_passed: c6 && c7 && c8,
            c6_failed: !c6,
            c7_failed: !c7,
            c8_failed: !c8,
            c2_or_c3_failed: !c2 || !c3,
            c4_or_c5_failed: !c4 || !c5,
        },
        retry_after,
    )
}

/// Classify a provider/key pair into the caller-visible candidate pool tier.
fn candidate_tier(
    snapshot: &ConfigSnapshot,
    provider: &ProviderRecord,
    key: &UpstreamKeyRecord,
    caller_user_id: UserId,
) -> Option<(CandidatePoolTier, Option<ShareId>)> {
    match provider.ownership_scope {
        llmroller_core::rbac::OwnershipScope::Global => Some((CandidatePoolTier::Global, None)),
        llmroller_core::rbac::OwnershipScope::Personal => {
            if provider.owner_user_id == Some(caller_user_id) {
                Some((CandidatePoolTier::Own, None))
            } else {
                let grantor_disabled = provider.owner_user_id.is_none_or(|owner| {
                    snapshot.users.get(&owner).is_none_or(|user| user.disabled)
                });
                // A foreign personal key without an eligible share grant is invisible (SEC-17).
                snapshot
                    .effective_share(
                        caller_user_id,
                        provider.id,
                        key.id,
                        provider.kind,
                        &provider.share_config_fingerprint,
                        grantor_disabled,
                    )
                    .map(|share| (CandidatePoolTier::Shared, Some(share.share_id)))
            }
        }
    }
}

/// Whether a pending-review route belongs to a minority vendor prefix (MDL-2, `C4`).
fn is_conflict_minority(model: &SnapshotModel, route: &SnapshotModelRoute) -> bool {
    model.merge_state == llmroller_core::catalog::MergeState::PendingReview
        && route.vendor_prefix.as_ref() != model.majority_vendor_prefix.as_ref()
}

/// Whether every enabled global provider with a usable key serves the model (MDL-3).
fn present_on_all_global_providers(snapshot: &ConfigSnapshot, model: &SnapshotModel) -> bool {
    let usable_global_providers: HashSet<ProviderId> = snapshot
        .providers
        .values()
        .filter(|provider| {
            provider.enabled
                && provider.ownership_scope == llmroller_core::rbac::OwnershipScope::Global
        })
        .filter(|provider| {
            snapshot
                .upstream_keys
                .values()
                .any(|key| key.provider_id == provider.id && key.enabled && key.health != "invalid")
        })
        .map(|provider| provider.id)
        .collect();

    if usable_global_providers.is_empty() {
        return false;
    }

    let served: HashSet<ProviderId> = snapshot
        .routes_by_model
        .get(&model.id)
        .into_iter()
        .flatten()
        .filter(|route| {
            route.enabled && route.status == llmroller_core::catalog::RouteStatus::Active
        })
        .filter_map(|route| snapshot.upstream_keys.get(&route.upstream_key_id))
        .filter(|key| key.enabled)
        .filter_map(|key| snapshot.providers.get(&key.provider_id))
        .filter(|provider| {
            provider.enabled
                && provider.ownership_scope == llmroller_core::rbac::OwnershipScope::Global
        })
        .map(|provider| provider.id)
        .collect();

    usable_global_providers
        .iter()
        .all(|provider_id| served.contains(provider_id))
}

/// Build the single hard-route candidate for a durable response link (RTE-12).
fn linked_candidate(
    snapshot: &ConfigSnapshot,
    link: &ResponseLinkRow,
    caller: &ClientKeyRecord,
) -> Option<RouteCandidate> {
    let route = snapshot
        .routes_by_model
        .get(&link.model_pk)?
        .iter()
        .find(|route| route.upstream_key_id == link.upstream_key_id)?;
    let key = snapshot.upstream_keys.get(&link.upstream_key_id)?;
    let provider = snapshot.providers.get(&key.provider_id)?;
    let (pool_tier, share_id) = candidate_tier(snapshot, provider, key, caller.owner_user_id)?;

    Some(RouteCandidate {
        upstream_key_id: key.id,
        provider_id: provider.id,
        model_pk: link.model_pk,
        upstream_model_id: route.upstream_model_id.clone(),
        base_url: provider.base_url.clone(),
        allow_private_network: provider.allow_private_network,
        config: provider.config.clone(),
        pool_tier,
        ownership_scope: provider.ownership_scope,
        priority: route
            .priority_override
            .unwrap_or(key.priority)
            .clamp(1, 1000),
        provider_kind: provider.kind,
        share_id,
    })
}

/// Whether the Responses body opted into upstream storage (`store` defaults to true).
fn response_store_flag(scan: &llmroller_proto::body_edit::TopLevelScan, body: &[u8]) -> bool {
    scan.fields
        .iter()
        .find(|field| field.key == "store")
        .and_then(|field| body.get(field.val_start..field.val_end))
        .and_then(|raw| std::str::from_utf8(raw).ok())
        .is_none_or(|raw| raw.trim() != "false")
}

/// Durable `response_links` TTL: 30 days when stored, 1 hour otherwise.
fn response_link_ttl_ms(store_flag: bool) -> i64 {
    if store_flag {
        30 * 24 * 60 * 60 * 1_000
    } else {
        60 * 60 * 1_000
    }
}

/// Coalesced durable response-link write (RTE-12).
#[allow(clippy::too_many_arguments)]
fn enqueue_response_link(
    state: &ProxyAuthState,
    owner_user_id: UserId,
    client_key_id: ClientKeyId,
    model_pk: ModelPk,
    upstream_key_id: UpstreamKeyId,
    session_hash: &str,
    response_id: &str,
    status: &str,
    store_flag: bool,
    created_at: i64,
) {
    let Some(writer) = &state.writer else {
        return;
    };
    let link = ResponseLinkRow {
        response_id: response_id.to_string(),
        owner_user_id,
        client_key_id,
        upstream_key_id,
        model_pk,
        session_hash: session_hash.to_string(),
        status: status.to_string(),
        store_flag,
        created_at,
        updated_at: created_at,
        expires_at: created_at.saturating_add(response_link_ttl_ms(store_flag)),
    };
    writer.queues.durable_state.lock().insert(
        DurableKey::ResponseLink(response_id.to_string()),
        DurableOp::ResponseLinkUpsert(link),
    );
}

/// Coalesced durable confirmation of a live session pin (RTE-10, REL-1).
fn enqueue_pin_upsert(
    state: &ProxyAuthState,
    pin_key: &PinKey,
    model_pk: ModelPk,
    entry: &PinEntry,
) {
    let Some(writer) = &state.writer else {
        return;
    };
    writer.queues.durable_state.lock().insert(
        DurableKey::Pin(
            pin_key.client_key_id.to_string(),
            pin_key.session_hash.clone(),
            model_pk.to_string(),
        ),
        DurableOp::PinUpsert {
            client_key_id: pin_key.client_key_id.to_string(),
            session_hash: pin_key.session_hash.clone(),
            model_pk: model_pk.to_string(),
            upstream_key_id: entry.upstream_key_id.to_string(),
            pool_tier: entry.pool_tier as u8,
            created_at: entry.created_ms,
            last_used_at: entry.last_used_ms,
            expires_at: entry.expires_ms,
            repin_count: i64::from(entry.repin_count),
        },
    );
}

/// Coalesced durable key-health projection (REL-1).
///
/// The latest in-memory projection wins because the queue is keyed per upstream
/// key; SQLite therefore never observes an out-of-order transition.
fn enqueue_key_health(
    writer: Option<&WriterHandle>,
    runtime: &RuntimeState,
    key_id: UpstreamKeyId,
    now_ms: i64,
) {
    let Some(writer) = writer else {
        return;
    };
    let entry = runtime.key_health_entry(key_id);
    let (health, health_until) = llmroller_store::health::project_health(&entry, now_ms);
    writer.queues.durable_state.lock().insert(
        DurableKey::HealthVersion(key_id.to_string()),
        DurableOp::HealthVersionUpdate {
            upstream_key_id: key_id.to_string(),
            health: health.to_string(),
            health_until,
            health_version: i64::try_from(runtime.health.current_version()).unwrap_or(i64::MAX),
            last_error: entry.quarantine_reason.clone(),
        },
    );
}

/// Coalesced durable `PlanRestricted` protocol block (API-8, REL-1).
fn enqueue_route_protocol_block(
    state: &ProxyAuthState,
    key_id: UpstreamKeyId,
    model_pk: ModelPk,
    protocol: &str,
    reason: &str,
    until_ms: i64,
) {
    let Some(writer) = &state.writer else {
        return;
    };
    writer.queues.durable_state.lock().insert(
        DurableKey::RouteProtocolBlock(
            key_id.to_string(),
            model_pk.to_string(),
            protocol.to_string(),
        ),
        DurableOp::RouteProtocolBlockUpsert {
            upstream_key_id: key_id.to_string(),
            model_pk: model_pk.to_string(),
            protocol: protocol.to_string(),
            reason: reason.to_string(),
            until_ms,
        },
    );
}

/// Coalesced durable route health update (API-8, REL-5, F-038).
fn enqueue_route_health_update(
    state: &ProxyAuthState,
    key_id: UpstreamKeyId,
    model_pk: ModelPk,
    route_health: &str,
    route_health_until: Option<i64>,
    last_error: Option<String>,
) {
    let Some(writer) = &state.writer else {
        return;
    };
    writer.queues.durable_state.lock().insert(
        DurableKey::RouteHealth(key_id.to_string(), model_pk.to_string()),
        DurableOp::RouteHealthUpdate {
            upstream_key_id: key_id.to_string(),
            model_pk: model_pk.to_string(),
            route_health: route_health.to_string(),
            route_health_until,
            health_version: 1,
            last_error,
        },
    );
}

/// Extract the upstream response id from a non-streaming `responses` payload.
fn json_response_id(value: &serde_json::Value) -> Option<&str> {
    value.get("id").and_then(|id| id.as_str())
}

/// Extract `response.created` (or any response object) id from the first SSE frame.
fn sse_response_id(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let candidate = value
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(|id| id.as_str())
            .or_else(|| value.get("id").and_then(|id| id.as_str()));
        if let Some(id) = candidate {
            return Some(id.to_string());
        }
    }
    None
}

/// Forward one state-bound Responses operation to the exact linked upstream key.
async fn forward_state_bound_response_op(
    state: &ProxyAuthState,
    caller: &ClientKeyRecord,
    response_id: &str,
    method: Method,
    subpath: &str,
    query_str: Option<&str>,
) -> Response {
    use crate::failover::{HardRouteDecision, evaluate_hard_route_decision};

    let now = now_ms();
    let link = match load_response_link(state, response_id).await {
        Ok(Some(link)) => link,
        Ok(None) => return response_not_found(response_id),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if link.owner_user_id != caller.owner_user_id || now > link.expires_at {
        return response_not_found(response_id);
    }

    let snapshot = state.snapshot.load();
    let Some(key) = snapshot.upstream_keys.get(&link.upstream_key_id).cloned() else {
        return permission_error(
            "the upstream key that holds this response is no longer available",
        );
    };
    let Some(provider) = snapshot.providers.get(&key.provider_id).cloned() else {
        return permission_error(
            "the upstream key that holds this response is no longer available",
        );
    };

    let (mut checks, retry_after) = hard_route_checks(
        &snapshot,
        &state.runtime_state,
        &link,
        caller,
        "responses",
        now,
    );
    // State-bound operations skip budget check C8 per SRDD Section 10.5 & 14.1 (`RTE-12`).
    checks.c8_failed = false;
    checks.c6_c8_passed = !checks.c6_failed && !checks.c7_failed;
    match evaluate_hard_route_decision(&checks, link.upstream_key_id, retry_after) {
        HardRouteDecision::RouteWithoutFailover(_) => {}
        HardRouteDecision::RateLimit429 { retry_after_secs } => {
            return rate_limit_error(retry_after_secs);
        }
        HardRouteDecision::Forbidden403KeyUnavailable => {
            return permission_error(
                "the upstream key that holds this response is no longer available",
            );
        }
        HardRouteDecision::Forbidden403ModelOrProtocolBlocked => {
            return permission_error(
                "the model or protocol that holds this response is no longer available",
            );
        }
    }

    let secret = match unseal_upstream_key(state, &snapshot, &key) {
        Ok(secret) => secret,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let resp_base_path = provider
        .config
        .paths
        .responses
        .as_deref()
        .unwrap_or("/responses");
    let mut url = format!(
        "{}/{}/{}{subpath}",
        provider.base_url.trim_end_matches('/'),
        resp_base_path.trim_start_matches('/'),
        response_id
    );
    if let Some(q) = query_str.filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(q);
    }
    if crate::ssrf::validate_url(&url, provider.allow_private_network).is_err() {
        return json_error(
            StatusCode::BAD_GATEWAY,
            "upstream_unreachable",
            "the linked upstream origin is not permitted",
        );
    }
    let auth_scheme = provider
        .config
        .auth
        .for_endpoint(ProviderEndpoint::Responses);
    let (auth_header, auth_prefix) = auth_scheme
        .as_ref()
        .map_or(("authorization", "Bearer "), |scheme| {
            (scheme.header.as_str(), scheme.prefix.as_str())
        });
    let client = state.select_http_client(provider.allow_private_network);
    let request_id = RequestId::generate().to_string();
    let upstream = match client
        .request(method.clone(), &url)
        .header(auth_header, format!("{auth_prefix}{secret}"))
        .header("x-request-id", &request_id)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => {
            return json_error(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "the linked upstream key could not be reached",
            );
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let candidate_tier = if provider.owner_user_id.is_none() {
        CandidatePoolTier::Global
    } else if provider.owner_user_id == Some(caller.owner_user_id) {
        CandidatePoolTier::Own
    } else {
        CandidatePoolTier::Shared
    };
    let pool_tier = candidate_tier_as_pool(candidate_tier);
    let is_admin = snapshot
        .users
        .get(&caller.owner_user_id)
        .is_some_and(|user| user.role == Role::Admin);

    let filtered_headers = llmroller_proto::headers::apply_response_header_pipeline(
        upstream.headers(),
        pool_tier,
        is_admin,
        &request_id,
        &provider.config.response_header_allow,
    );
    let body = match upstream.bytes().await {
        Ok(body) => body,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };

    if status.is_success() {
        if method == Method::DELETE && subpath.is_empty() {
            if let Some(writer) = &state.writer {
                writer.queues.durable_state.lock().insert(
                    DurableKey::ResponseLink(response_id.to_string()),
                    DurableOp::ResponseLinkDelete {
                        response_id: response_id.to_string(),
                    },
                );
            } else if sqlx::query("DELETE FROM response_links WHERE response_id = ?")
                .bind(response_id)
                .execute(&state.pool)
                .await
                .is_err()
            {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        } else if subpath == "/cancel" {
            if let Some(writer) = &state.writer {
                writer.queues.durable_state.lock().insert(
                    DurableKey::ResponseLink(response_id.to_string()),
                    DurableOp::ResponseLinkMarkIncomplete {
                        response_id: response_id.to_string(),
                        updated_at: now_ms(),
                    },
                );
            } else if sqlx::query(
                "UPDATE response_links SET status = 'incomplete' WHERE response_id = ?",
            )
            .bind(response_id)
            .execute(&state.pool)
            .await
            .is_err()
            {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }

    let canonical_id = snapshot
        .models
        .get(&link.model_pk)
        .map_or_else(|| "gpt-4o".to_string(), |m| m.canonical_id.clone());

    if !link.session_hash.is_empty() {
        let pin_key = PinKey {
            client_key_id: caller.id,
            canonical_id: canonical_id.clone(),
            session_hash: link.session_hash.clone(),
        };
        let pin_ttl_ms = snapshot.settings.pin_ttl_secs.saturating_mul(1000);
        state.pin_store.upsert_confirmed(
            pin_key,
            link.upstream_key_id,
            candidate_tier,
            now,
            pin_ttl_ms,
        );
    }

    if let Some(writer) = &state.writer {
        let tier_str = match pool_tier {
            PoolTier::Own => "own",
            PoolTier::Shared => "shared",
            PoolTier::Global | PoolTier::Blended => "global",
        };
        let rid = RequestId::from_str(&request_id).unwrap_or_else(|_| RequestId::generate());
        writer.queues.usage_writer.enqueue(
            UsageEventRecord {
                id: UsageEventId::generate().to_string(),
                ts: now,
                request_id: rid,
                client_key_id: caller.id,
                owner_user_id: caller.owner_user_id,
                upstream_key_id: Some(link.upstream_key_id.to_string()),
                provider_id: Some(key.provider_id.to_string()),
                provider_scope: Some(tier_str.to_string()),
                share_id: None,
                canonical_id: Some(canonical_id),
                upstream_model_id: None,
                endpoint: "responses".to_string(),
                protocol: Some("responses".to_string()),
                session_hash: if link.session_hash.is_empty() {
                    None
                } else {
                    Some(link.session_hash.clone())
                },
                status: i64::from(status.as_u16()),
                stream: false,
                incomplete: false,
                termination: "completed".to_string(),
                trace_dropped: false,
                input_uncached_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: 0,
                cost_micro_usd: 0,
                cost_source: CostSource::PriceBook,
                latency_ms: Some((now_ms() - now).max(0)),
                ttfb_ms: None,
                pin_outcome: None,
                failovers: 0,
                error_code: if status.is_success() {
                    None
                } else {
                    Some("upstream_error".to_string())
                },
                list_cost_micro_usd: 0,
                plan_multiplier_bps: 10_000,
                billing_coverage_mode: "pay_as_you_go".to_string(),
                service_account_id: caller.service_account_id.clone().map(|sa| sa.to_string()),
                stop_reason: None,
                request_headers_json: None,
                metadata_json: None,
            },
            Vec::new(),
        );
    }

    let mut builder = Response::builder().status(status);
    for (name, value) in &filtered_headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Handler for POST /v1/messages/count_tokens.
pub async fn post_messages_count_tokens(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    Extension(caller): Extension<ClientKeyRecord>,
    body: Bytes,
) -> Response {
    let now = now_ms();
    let scan = match scan_top_level(&body) {
        Ok(scan) => scan,
        Err(error) => {
            return messages_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                error.to_string(),
            );
        }
    };
    let Some((ref model_str, _)) = scan.model else {
        return messages_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model is required in request body",
        );
    };
    let snapshot = state.snapshot.load();
    let model = match snapshot.resolve_model(model_str) {
        Ok(model) => model.clone(),
        Err(llmroller_core::snapshot::SnapshotResolveError::KillSwitched) => {
            return messages_error(
                StatusCode::FORBIDDEN,
                "model_disabled",
                format!("model {model_str} is disabled"),
            );
        }
        Err(_) => {
            return messages_error(
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("model {model_str} not found in catalog"),
            );
        }
    };
    if let Some(ref allowed) = caller.allowed_models
        && !allowed
            .iter()
            .any(|entry| entry == &model.canonical_id || entry == model_str)
    {
        return messages_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("model {} not permitted on this key", model.canonical_id),
        );
    }

    let scope = parse_scope(&caller.routing_scope);
    let mut candidates: Vec<RouteCandidate> =
        collect_candidates(&snapshot, &model, &caller, scope, now)
            .into_iter()
            .filter(|candidate| {
                snapshot
                    .routes_by_model
                    .get(&model.id)
                    .and_then(|routes| {
                        routes.iter().find(|route| {
                            route.upstream_key_id == candidate.upstream_key_id
                                && route.protocols.iter().any(|value| value == "messages")
                        })
                    })
                    .is_some()
                    && !state.runtime_state.is_route_protocol_blocked(
                        candidate.upstream_key_id,
                        candidate.model_pk,
                        "messages",
                        now,
                    )
            })
            .collect();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.priority));
    let had_candidates = !candidates.is_empty();
    candidates.retain(|candidate| {
        !state.runtime_state.is_aux_endpoint_unsupported(
            candidate.upstream_key_id,
            "count_tokens",
            now,
        )
    });

    if candidates.is_empty() {
        if had_candidates {
            return aux_endpoint_unsupported_error(true);
        }
        return messages_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "no eligible upstream supports native Anthropic token counting",
        );
    }

    let mut aux_unsupported_seen = false;
    for candidate in candidates {
        let Some(key) = snapshot
            .upstream_keys
            .get(&candidate.upstream_key_id)
            .cloned()
        else {
            continue;
        };
        let secret = match unseal_upstream_key(&state, &snapshot, &key) {
            Ok(secret) => secret,
            Err(()) => continue,
        };
        let url = get_adapter(candidate.provider_kind)
            .build_endpoint_url(&candidate.base_url, EndpointKind::CountTokens);
        if crate::ssrf::validate_url(&url, candidate.allow_private_network).is_err() {
            continue;
        }
        let client = state.select_http_client(candidate.allow_private_network);
        let edited = match edit_body(
            body.clone(),
            candidate.pool_tier(),
            Some(&candidate.upstream_model_id),
            ChatStreamMode::Native,
            None,
        ) {
            Ok((body, _)) => body,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        let outbound = match build_upstream_headers(
            &headers,
            &candidate,
            &secret,
            EndpointKind::CountTokens,
            None,
        ) {
            Ok(headers) => headers,
            Err(response) => return *response,
        };

        let response = match client
            .post(&url)
            .body(edited)
            .headers(outbound)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                continue;
            }
        };
        let status =
            StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        if matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
        ) {
            state.runtime_state.mark_aux_endpoint_unsupported(
                candidate.upstream_key_id,
                "count_tokens",
                now.saturating_add(AUX_UNSUPPORTED_TTL_MS),
            );
            aux_unsupported_seen = true;
            continue;
        }
        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        };
        let mut builder = Response::builder().status(status);
        if let Some(content_type) = content_type {
            builder = builder.header(CONTENT_TYPE, content_type);
        }
        return builder
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    if aux_unsupported_seen {
        return aux_endpoint_unsupported_error(true);
    }

    messages_error(
        StatusCode::NOT_FOUND,
        "not_found_error",
        "no eligible upstream supports native Anthropic token counting",
    )
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
        EndpointKind::ResponsesCompact,
        headers,
        body,
        client_key.map(|e| e.0),
    )
    .await
}

/// Handler for GET /v1/responses/{id}.
pub async fn get_response_by_id(
    State(state): State<ProxyAuthState>,
    uri: axum::http::Uri,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    forward_state_bound_response_op(&state, &client_key, &id, Method::GET, "", uri.query()).await
}

/// Handler for DELETE /v1/responses/{id}.
pub async fn delete_response_by_id(
    State(state): State<ProxyAuthState>,
    uri: axum::http::Uri,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    forward_state_bound_response_op(&state, &client_key, &id, Method::DELETE, "", uri.query()).await
}

/// Handler for GET /v1/responses/{id}/input_items.
pub async fn get_response_input_items(
    State(state): State<ProxyAuthState>,
    uri: axum::http::Uri,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    forward_state_bound_response_op(
        &state,
        &client_key,
        &id,
        Method::GET,
        "/input_items",
        uri.query(),
    )
    .await
}

/// Handler for POST /v1/responses/{id}/cancel.
pub async fn post_response_cancel(
    State(state): State<ProxyAuthState>,
    uri: axum::http::Uri,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    forward_state_bound_response_op(
        &state,
        &client_key,
        &id,
        Method::POST,
        "/cancel",
        uri.query(),
    )
    .await
}

/// One fully-resolved routing candidate built exclusively from the immutable snapshot.
#[derive(Debug, Clone)]
pub(crate) struct RouteCandidate {
    upstream_key_id: UpstreamKeyId,
    provider_id: ProviderId,
    model_pk: ModelPk,
    upstream_model_id: String,
    base_url: String,
    allow_private_network: bool,
    /// Parsed provider configuration for endpoint paths, auth, and header profiles.
    config: llmroller_core::provider::ProviderConfig,
    pool_tier: CandidatePoolTier,
    ownership_scope: llmroller_core::rbac::OwnershipScope,
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

    pub(crate) fn upstream_key_id(&self) -> UpstreamKeyId {
        self.upstream_key_id
    }

    pub(crate) fn priority(&self) -> i64 {
        self.priority
    }

    fn pool_tier(&self) -> PoolTier {
        match self.pool_tier {
            CandidatePoolTier::Own => PoolTier::Own,
            CandidatePoolTier::Shared => PoolTier::Shared,
            CandidatePoolTier::Global => PoolTier::Global,
            CandidatePoolTier::Blended => PoolTier::Blended,
        }
    }

    /// Configured session hint profile, falling back to the provider preset default.
    fn session_profile(&self) -> llmroller_core::provider::SessionProfile {
        self.config
            .session_profile
            .clone()
            .unwrap_or_else(|| get_adapter(self.provider_kind).default_session_profile())
    }
}

/// Compute the Unix wall-clock time in milliseconds.
fn now_ms() -> i64 {
    SystemClock::new().now_ms()
}

/// Auxiliary endpoints are probed and cached independently from primary inference routes (API-8).
const fn aux_endpoint_label(endpoint: EndpointKind) -> Option<&'static str> {
    match endpoint {
        EndpointKind::ResponsesCompact => Some("responses_compact"),
        EndpointKind::CountTokens => Some("count_tokens"),
        _ => None,
    }
}

fn aux_endpoint_unsupported_error(is_messages: bool) -> Response {
    let request_id = RequestId::generate().to_string();
    protocol_error(
        is_messages,
        StatusCode::NOT_FOUND,
        "not_found_error",
        "no eligible upstream supports this auxiliary endpoint",
        Some(&request_id),
        None,
    )
}

/// Interpret an out-of-band `GET /models` verification probe response (API-8).
///
/// Only an explicit auth rejection invalidates the key; 2xx clears the strikes.
/// Transport errors and unrelated statuses leave the recorded strikes intact so
/// a transient probe failure cannot quarantine a healthy key.
fn credential_probe_verdict(status: StatusCode) -> Option<bool> {
    if status.is_success() {
        Some(true)
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        Some(false)
    } else {
        None
    }
}

/// Fire the single-flight credential verification probe for a struck key (API-8).
fn spawn_credential_probe(
    state: &ProxyAuthState,
    base_url: &str,
    allow_private_network: bool,
    secret: &str,
    key_id: UpstreamKeyId,
) {
    if !state.runtime_state.try_begin_credential_probe(key_id) {
        return;
    }
    let client = state.select_http_client(allow_private_network).clone();
    let runtime = Arc::clone(&state.runtime_state);
    let writer = state.writer.clone();
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let authorization = format!("Bearer {secret}");
    tokio::spawn(async move {
        let verdict = client
            .get(&url)
            .header("authorization", authorization)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .ok()
            .and_then(|response| credential_probe_verdict(response.status()));
        if let Some(probe_success) = verdict {
            runtime.record_credential_probe_result(key_id, probe_success);
            enqueue_key_health(
                writer.as_ref(),
                &runtime,
                key_id,
                SystemClock::new().now_ms(),
            );
        }
        runtime.finish_credential_probe(key_id);
    });
}

/// Parse a client key routing scope, failing closed to the safest default.
pub(crate) fn parse_scope(scope: &str) -> RoutingScope {
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
pub(crate) fn collect_candidates(
    snapshot: &ConfigSnapshot,
    model: &SnapshotModel,
    caller: &ClientKeyRecord,
    scope: RoutingScope,
    now: i64,
) -> Vec<RouteCandidate> {
    collect_routes(snapshot, model, caller, scope, now, true)
}

/// Collect caller-visible routes regardless of transient health (`C1`..`C5` only).
pub(crate) fn collect_visible_candidates(
    snapshot: &ConfigSnapshot,
    model: &SnapshotModel,
    caller: &ClientKeyRecord,
    scope: RoutingScope,
    now: i64,
) -> Vec<RouteCandidate> {
    if model.exposed == Some(false) {
        return Vec::new();
    }

    let intersection_blocked = model.exposed.is_none()
        && snapshot.settings.global_exposure_mode
            == llmroller_core::settings::GlobalExposureMode::Intersection
        && !present_on_all_global_providers(snapshot, model);

    let mut candidates = collect_routes(snapshot, model, caller, scope, now, false);
    candidates.retain(|candidate| {
        if intersection_blocked && candidate.pool_tier == CandidatePoolTier::Global {
            return false;
        }
        if matches!(
            candidate.pool_tier,
            CandidatePoolTier::Own | CandidatePoolTier::Shared
        ) && snapshot
            .user_route_prefs
            .get(&(caller.owner_user_id, candidate.upstream_key_id, model.id))
            .is_some_and(|preference| !preference.enabled)
        {
            return false;
        }
        let minority = snapshot
            .routes_by_model
            .get(&model.id)
            .into_iter()
            .flatten()
            .find(|route| route.upstream_key_id == candidate.upstream_key_id)
            .is_some_and(|route| is_conflict_minority(model, route));
        !minority
    });
    candidates
}

fn collect_routes(
    snapshot: &ConfigSnapshot,
    model: &SnapshotModel,
    caller: &ClientKeyRecord,
    scope: RoutingScope,
    now: i64,
    require_healthy: bool,
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

        let (tier, share_id) = match candidate_tier(snapshot, provider, key, caller.owner_user_id) {
            Some(tier) => tier,
            // A foreign personal key without a share grant is invisible (SEC-17).
            None => continue,
        };

        if !scope.allows_tier(candidate_tier_as_pool(tier)) {
            continue;
        }
        if require_healthy && !route_healthy(route, now) {
            continue;
        }

        // `RTE-5`: user preferences override weights only on Own/Shared routes.
        let user_priority = if matches!(tier, CandidatePoolTier::Own | CandidatePoolTier::Shared) {
            snapshot
                .user_route_prefs
                .get(&(caller.owner_user_id, key.id, model.id))
                .map(|preference| preference.priority_override)
        } else {
            None
        };

        candidates.push(RouteCandidate {
            upstream_key_id: key.id,
            provider_id: provider.id,
            model_pk: model.id,
            upstream_model_id: route.upstream_model_id.clone(),
            base_url: provider.base_url.clone(),
            allow_private_network: provider.allow_private_network,
            config: provider.config.clone(),
            pool_tier: tier,
            ownership_scope: provider.ownership_scope,
            priority: user_priority
                .or(route.priority_override)
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

/// Persisted `usage_events.provider_scope` label for a candidate.
const fn provider_scope_label(candidate: &RouteCandidate) -> &'static str {
    match (candidate.ownership_scope, candidate.share_id.is_some()) {
        (llmroller_core::rbac::OwnershipScope::Global, _) => "global",
        (llmroller_core::rbac::OwnershipScope::Personal, true) => "shared",
        (llmroller_core::rbac::OwnershipScope::Personal, false) => "personal",
    }
}

/// Map a wire endpoint to its independently configurable provider profile section.
const fn provider_endpoint(endpoint: EndpointKind) -> Option<ProviderEndpoint> {
    match endpoint {
        EndpointKind::Chat => Some(ProviderEndpoint::Chat),
        EndpointKind::Responses | EndpointKind::ResponsesCompact => {
            Some(ProviderEndpoint::Responses)
        }
        EndpointKind::Messages | EndpointKind::CountTokens => Some(ProviderEndpoint::Messages),
        EndpointKind::Models => Some(ProviderEndpoint::Models),
        EndpointKind::Usage => Some(ProviderEndpoint::Usage),
    }
}

/// Build an upstream URL from the provider's configured path, falling back to the adapter preset.
fn endpoint_url(candidate: &RouteCandidate, endpoint: EndpointKind) -> String {
    if let Some(configured) = provider_endpoint(endpoint)
        && let Some(path) = candidate.config.paths.for_endpoint(configured)
        && !path.trim().is_empty()
    {
        let path = path.trim();
        if path.starts_with("https://") || path.starts_with("http://") {
            return path.to_string();
        }
        let separated = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        return format!("{}{separated}", candidate.base_url.trim_end_matches('/'));
    }
    get_adapter(candidate.provider_kind).build_endpoint_url(&candidate.base_url, endpoint)
}

/// Resolve the configured credential header for an endpoint.
fn auth_scheme_for(
    candidate: &RouteCandidate,
    endpoint: EndpointKind,
) -> Option<ProviderAuthScheme> {
    candidate
        .config
        .auth
        .for_endpoint(provider_endpoint(endpoint)?)
}

/// Build the complete outbound request header map for one provider attempt.
///
/// Applies the inbound sanitization pipeline, provider strip/token filters, the
/// configured credential scheme, User-Agent policy, `add_headers`, and the native
/// Anthropic protocol default (`API-2`, `API-3`, `PRV-3`).
fn build_upstream_headers(
    inbound: &HeaderMap,
    candidate: &RouteCandidate,
    secret: &str,
    endpoint: EndpointKind,
    session: Option<&ResolvedSession>,
) -> Result<HeaderMap, Box<Response>> {
    use llmroller_proto::headers::{
        HeaderPipelineError, UaPolicy, apply_request_header_pipeline, resolve_user_agent,
    };

    // Native Anthropic endpoints default to `x-api-key`; all other protocols use
    // an OpenAI-compatible Bearer token until the provider overrides `auth`.
    let default_scheme = if matches!(endpoint, EndpointKind::Messages | EndpointKind::CountTokens) {
        ("x-api-key", "")
    } else {
        ("authorization", "Bearer ")
    };
    let resolved_scheme = auth_scheme_for(candidate, endpoint);
    let (auth_header, auth_prefix) = resolved_scheme.as_ref().map_or(default_scheme, |scheme| {
        (scheme.header.as_str(), scheme.prefix.as_str())
    });
    let auth_value = format!("{auth_prefix}{secret}");
    let is_messages = matches!(endpoint, EndpointKind::Messages | EndpointKind::CountTokens);
    let mut outbound = apply_request_header_pipeline(
        inbound,
        candidate.pool_tier(),
        Some((auth_header, &auth_value)),
        &candidate.config.strip_headers,
        &candidate.config.strip_header_tokens,
    )
    .map_err(|error| {
        Box::new(match error {
            HeaderPipelineError::Smuggling(reason) => protocol_error(
                is_messages,
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                reason.to_string(),
                None,
                None,
            ),
            HeaderPipelineError::InvalidHeaderValue(reason) => protocol_error(
                is_messages,
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_provider_config",
                reason,
                None,
                None,
            ),
        })
    })?;

    let policy = candidate
        .config
        .ua_policy
        .map(UaPolicy::from)
        .unwrap_or_else(|| get_adapter(candidate.provider_kind).default_ua_policy());
    let incoming_ua = inbound
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let resolved_ua = resolve_user_agent(policy, incoming_ua).map_err(|reason| {
        Box::new(protocol_error(
            is_messages,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            reason.to_string(),
            None,
            None,
        ))
    })?;
    if let Ok(value) = HeaderValue::from_str(&resolved_ua) {
        outbound.insert(header::USER_AGENT, value);
    }

    for (name, value) in &candidate.config.add_headers {
        let lower = name.to_ascii_lowercase();
        if lower == "host"
            || lower == "content-length"
            || lower == "authorization"
            || lower == auth_header
        {
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
        outbound.insert(name, value);
    }

    // Section 11.2: write the provider's affinity fields. On Own-tier keys the
    // client's raw session is forwarded and profile fields are written only when
    // absent; Shared/Global keys always receive the tenant-salted token (SEC-16).
    if let Some(session) = session {
        let profile = candidate.session_profile();
        apply_outbound_session_affinity(
            &mut outbound,
            candidate.pool_tier(),
            &profile,
            &session.salted_token,
            &session.raw_session,
        );
    }

    if matches!(endpoint, EndpointKind::Messages | EndpointKind::CountTokens) {
        outbound
            .entry(HeaderName::from_static("anthropic-version"))
            .or_insert_with(|| HeaderValue::from_static("2023-06-01"));
    }

    Ok(outbound)
}

/// Estimated input charge recorded on post-write failures (USG-1).
const fn failure_charge(
    outcome: llmroller_core::usage::AttemptOutcome,
    estimate: i64,
) -> (i64, &'static str) {
    use llmroller_core::usage::AttemptOutcome;
    match outcome {
        AttemptOutcome::Http5xx | AttemptOutcome::WaitTimeout | AttemptOutcome::ErrorFrame => {
            (estimate, "estimated_input")
        }
        _ => (0, "none"),
    }
}

/// Monthly cap charged by a candidate's pool tier (USR-5, SHR-1).
///
/// Global candidates consume the caller's user-global budget; shared candidates
/// consume the grantor's per-share cap. Own and blended candidates charge no
/// tier cap here (the client-key cap is reserved once per request).
fn candidate_cap_scope(
    snapshot: &ConfigSnapshot,
    owner_user_id: UserId,
    candidate: &RouteCandidate,
) -> Option<(BudgetScopeId, Option<i64>)> {
    match candidate.pool_tier {
        CandidatePoolTier::Global => Some((
            BudgetScopeId::User(owner_user_id),
            snapshot
                .users
                .get(&owner_user_id)
                .and_then(|user| user.global_budget_micro_usd_month),
        )),
        CandidatePoolTier::Shared => {
            let share_id = candidate.share_id?;
            snapshot
                .shares_by_grantee
                .get(&owner_user_id)?
                .iter()
                .find(|share| share.share_id == share_id)
                .map(|share| {
                    (
                        BudgetScopeId::ProviderShare(share_id),
                        share.max_monthly_micro_usd,
                    )
                })
        }
        CandidatePoolTier::Own | CandidatePoolTier::Blended => None,
    }
}

/// Tier-scoped monthly reservations acquired for one candidate attempt.
#[derive(Debug, Default)]
struct CandidateCapGuards {
    user: Option<MonthReservationGuard>,
    share: Option<MonthReservationGuard>,
}

impl CandidateCapGuards {
    fn commit(&mut self, actual_micro_usd: i64) {
        if let Some(guard) = self.user.take() {
            guard.commit(actual_micro_usd);
        }
        if let Some(guard) = self.share.take() {
            guard.commit(actual_micro_usd);
        }
    }
}

/// Reserves the monthly caps that the candidate's tier actually charges.
fn reserve_candidate_caps(
    registry: &llmroller_core::accumulators::MonthlyAccumulatorRegistry,
    snapshot: &ConfigSnapshot,
    owner_user_id: UserId,
    candidate: &RouteCandidate,
    estimate_micro_usd: i64,
    now_ms: i64,
) -> Result<CandidateCapGuards, GateG7Error> {
    let Some((scope_id, cap)) = candidate_cap_scope(snapshot, owner_user_id, candidate) else {
        return Ok(CandidateCapGuards::default());
    };
    let guard = registry.try_reserve(scope_id, cap, estimate_micro_usd, now_ms)?;
    Ok(match candidate.pool_tier {
        CandidatePoolTier::Global => CandidateCapGuards {
            user: Some(guard),
            share: None,
        },
        CandidatePoolTier::Shared => CandidateCapGuards {
            user: None,
            share: Some(guard),
        },
        CandidatePoolTier::Own | CandidatePoolTier::Blended => CandidateCapGuards::default(),
    })
}

/// Build the C1-C8 eligibility projection for one candidate.
fn eligibility_route(
    snapshot: &ConfigSnapshot,
    runtime: &RuntimeState,
    candidate: &RouteCandidate,
    model: &SnapshotModel,
    caller: &ClientKeyRecord,
    protocol: &str,
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
            config: candidate.config.clone(),
            share_config_fingerprint: String::new(),
            usage_url_cross_origin_ack: false,
            allow_private_network: candidate.allow_private_network,
            enabled: false,
            billing_coverage_mode: "pay_as_you_go".to_string(),
            plan_multiplier_bps: 10_000,
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

    let tier_cap_exhausted = candidate_cap_scope(snapshot, caller.owner_user_id, candidate)
        .is_some_and(|(scope, cap)| runtime.accumulators.is_exhausted(&scope, cap, now));
    let windows_admitted = runtime.key_budget_admitted(
        snapshot,
        candidate.upstream_key_id,
        Some(candidate.model_pk),
        now,
    );

    EligibilityRoute {
        upstream_key_id: candidate.upstream_key_id,
        model_pk: model.id,
        provider_id: candidate.provider_id,
        provider_kind: candidate.provider_kind,
        provider_ownership: provider.ownership_scope,
        provider_owner_id: provider.owner_user_id,
        provider_owner_disabled: provider
            .owner_user_id
            .is_some_and(|owner_id| snapshot.users.get(&owner_id).is_none_or(|u| u.disabled)),
        provider_fingerprint: provider.share_config_fingerprint.clone(),
        provider_enabled: provider.enabled,
        key_enabled: key.enabled,
        route_enabled: route.is_some_and(|route| route.enabled),
        route_status: route.map_or(llmroller_core::catalog::RouteStatus::Retired, |route| {
            route.status
        }),
        protocols: route.map_or_else(Vec::new, |route| route.protocols.clone()),
        protocol_blocked: route.is_some_and(|route| {
            !route_healthy(route, now)
                || runtime.is_route_protocol_blocked(
                    candidate.upstream_key_id,
                    candidate.model_pk,
                    protocol,
                    now,
                )
        }),
        key_health_ok: runtime.is_key_routable(candidate.upstream_key_id, now),
        route_health_ok: route.is_some_and(|route| route_healthy(route, now)),
        breaker_closed: runtime.is_origin_admitted(
            &OriginKey::from_base_url(&candidate.base_url, candidate.allow_private_network),
            now,
        ),
        user_pref_enabled: snapshot
            .user_route_prefs
            .get(&(caller.owner_user_id, candidate.upstream_key_id, model.id))
            .map(|pref| pref.enabled),
        is_conflict_minority: route.is_some_and(|route| is_conflict_minority(model, route)),
        priority: candidate.priority,
        budget_admitted: windows_admitted && !tier_cap_exhausted,
        budget_exhausted_due_to_monthly_cap: tier_cap_exhausted,
    }
}

/// Select the next candidate in the tier group with the model's configured strategy.
#[allow(clippy::too_many_arguments)]
fn select_next_candidate(
    runtime: &RuntimeState,
    strategy: RoutingStrategy,
    caller: UserId,
    model_pk: ModelPk,
    canonical_id: &str,
    protocol: &str,
    tier: CandidatePoolTier,
    eligible: &[RouteCandidate],
    tried: &[UpstreamKeyId],
    now: i64,
    entropy: &dyn EntropySource,
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
        entropy,
        Some(runtime.load_tracker.as_ref()),
    )?;
    let candidate = eligible
        .iter()
        .find(|candidate| candidate.upstream_key_id == selected.upstream_key_id)?
        .clone();
    // Every route selection increments the bounded-cardinality counter from
    // SRDD Section 10.3.3 step 4 (`OBS-1`).
    crate::metrics::record_routing_selection(
        candidate.provider_kind.as_str(),
        &candidate.provider_id.to_string(),
        protocol,
        "selected",
        strategy.as_str(),
        canonical_id,
    );
    Some(candidate)
}

/// Immutable routing context needed to reselect a pin after a pre-first-byte failure.
struct PinFailureContext<'a> {
    runtime: &'a RuntimeState,
    strategy: RoutingStrategy,
    caller: UserId,
    model_pk: ModelPk,
    canonical_id: &'a str,
    protocol: &'a str,
    selected_tier: CandidatePoolTier,
    tier_candidates: &'a [RouteCandidate],
    eligible_keys: &'a HashSet<UpstreamKeyId>,
    now: i64,
    entropy: &'a dyn EntropySource,
}

impl PinFailureContext<'_> {
    fn select(&self, tried: &[UpstreamKeyId]) -> Option<(UpstreamKeyId, CandidatePoolTier)> {
        select_next_candidate(
            self.runtime,
            self.strategy,
            self.caller,
            self.model_pk,
            self.canonical_id,
            self.protocol,
            self.selected_tier,
            self.tier_candidates,
            tried,
            self.now,
            self.entropy,
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
    max_output_tokens: Option<i64>,
) -> i64 {
    let fallback = Rates::from(&snapshot.settings.unpriced_fallback_rates);
    let rates = snapshot
        .price_for(
            &candidate.provider_id,
            candidate.provider_kind,
            &candidate.upstream_model_id,
        )
        .map_or(fallback, |entry| entry.base_rates);
    let raw_est =
        estimate_reservation_cost_micro_usd(input_tokens, 0.0, &rates, max_output_tokens, None);
    let (mode, bps) = snapshot
        .providers
        .get(&candidate.provider_id)
        .map(|p| (p.billing_coverage_mode.as_str(), p.plan_multiplier_bps))
        .unwrap_or(("pay_as_you_go", 10_000));
    llmroller_core::pricing::apply_plan_multiplier(raw_est, bps, mode)
}

/// Extract the caller's requested output-token ceiling for the pre-flight
/// reservation (`LIM-1`): `max_output_tokens` (Responses), `max_completion_tokens`
/// (o-series Chat), or `max_tokens` (Chat/Messages), in that precedence order.
fn requested_max_output_tokens(body: &[u8]) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    ["max_output_tokens", "max_completion_tokens", "max_tokens"]
        .iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(serde_json::Value::as_i64)
                .filter(|tokens| *tokens > 0)
        })
}

/// Fail-closed AEAD unsealing using only the immutable snapshot.
fn unseal_upstream_key(
    state: &ProxyAuthState,
    snapshot: &ConfigSnapshot,
    key: &UpstreamKeyRecord,
) -> Result<String, ()> {
    let now_ms = SystemClock::new().now_ms();
    let Some(master_key) = state.master_key.clone() else {
        state
            .runtime_state
            .quarantine_key(key.id, "master_key_unavailable");
        enqueue_key_health(state.writer.as_ref(), &state.runtime_state, key.id, now_ms);
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
        Ok(plaintext) => String::from_utf8(plaintext.expose_secret().to_vec()).map_err(|_| {
            state
                .runtime_state
                .quarantine_key(key.id, "invalid_utf8_secret");
            enqueue_key_health(state.writer.as_ref(), &state.runtime_state, key.id, now_ms);
        }),
        Err(_) => {
            state
                .runtime_state
                .quarantine_key(key.id, "aead_unseal_failed");
            enqueue_key_health(state.writer.as_ref(), &state.runtime_state, key.id, now_ms);
            Err(())
        }
    }
}

/// Deterministic fallback pin hash used when no instance session secret is configured.
fn fallback_session_hash(client_key_id: ClientKeyId, raw_session: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"llmroller.pin.v1\0");
    hasher.update(client_key_id.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(raw_session.as_bytes());
    llmroller_core::session::hex_lower(&hasher.finalize())
}

/// Tenant-scoped session hash (`RTE-1`): HMAC-SHA256 with the instance secret.
fn session_hash(
    session_hmac_secret: Option<&[u8]>,
    owner_user_id: &UserId,
    client_key_id: ClientKeyId,
    raw_session: &str,
) -> String {
    session_hmac_secret.map_or_else(
        || fallback_session_hash(client_key_id, raw_session),
        |secret| llmroller_core::session::compute_session_hash(secret, owner_user_id, raw_session),
    )
}

/// Collect Step 7 body session fields from the parsed request body.
fn session_body_fields(body: &serde_json::Value) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for name in ["session_id", "prompt_cache_key", "promptCacheKey"] {
        if let Some(value) = body.get(name).and_then(serde_json::Value::as_str) {
            fields.insert(name.to_string(), value.to_string());
        }
    }
    if let Some(value) = body
        .get("metadata")
        .and_then(|metadata| metadata.get("session_id"))
        .and_then(serde_json::Value::as_str)
    {
        fields.insert("metadata.session_id".to_string(), value.to_string());
    }
    fields
}

/// Inputs shared by session pin resolution.
struct SessionPinContext<'a> {
    client_key: &'a ClientKeyRecord,
    canonical_id: &'a str,
    protocol: &'a str,
    session_hmac_secret: Option<&'a [u8]>,
    runtime_state: &'a RuntimeState,
    now_ms: i64,
}

/// Resolved inbound session plus its tenant-salted outbound affinity token.
struct ResolvedSession {
    pin_key: PinKey,
    raw_session: String,
    salted_token: String,
}

impl ResolvedSession {
    /// The token written to provider affinity fields for the chosen tier.
    fn outbound_token(&self, tier: PoolTier) -> &str {
        if tier == PoolTier::Own && !self.raw_session.is_empty() {
            &self.raw_session
        } else {
            &self.salted_token
        }
    }
}

/// Deterministic in-memory pin key; the raw session string never leaves the process.
///
/// Implements the eight-step inbound session precedence (`RTE-1`), the Step 8
/// canonical opening-message fingerprint fallback, and the 21-slot constant
/// session detector for generic session sources (`RTE-1`, `RTE-2`). It also
/// derives the outbound affinity token used by SRDD Section 11.2.
fn resolve_session(
    headers: &HeaderMap,
    body: &[u8],
    context: &SessionPinContext<'_>,
) -> Option<ResolvedSession> {
    let SessionPinContext {
        client_key,
        canonical_id,
        protocol,
        session_hmac_secret,
        runtime_state,
        now_ms,
    } = *context;
    let header_vec: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    let body_fields = session_body_fields(&parsed);
    let (raw_session, step) =
        llmroller_core::session::extract_session_precedence(&header_vec, &body_fields);

    let canonical = if raw_session.is_empty() || step.is_generic() {
        Some(llmroller_core::session::extract_canonical_sys_and_user1(
            protocol, &parsed,
        ))
    } else {
        None
    };

    let raw_hash = if raw_session.is_empty() {
        None
    } else {
        Some(session_hash(
            session_hmac_secret,
            &client_key.owner_user_id,
            client_key.id,
            &raw_session,
        ))
    };

    if let (Some(session_hash), true, Some((sys, usr))) =
        (raw_hash.as_ref(), step.is_generic(), canonical.as_ref())
    {
        let newly_flagged = runtime_state.constant_session_detector.record_and_evaluate(
            client_key.id,
            session_hash.clone(),
            &llmroller_core::session::PromptContext {
                sys,
                usr,
                has_claude_code_agent_id: headers.contains_key("x-claude-code-agent-id"),
                step,
            },
            now_ms,
        );
        if newly_flagged {
            crate::metrics::record_constant_session_suspected();
        }
    }

    let use_step8 = raw_session.is_empty()
        || (step.is_generic() && client_key.constant_session_mode == "fingerprint_generic_ids");
    let session_hash = if use_step8 {
        let (sys, usr) = canonical.as_ref()?;
        llmroller_core::session::compute_step8_fingerprint(
            &client_key.id,
            canonical_id,
            llmroller_core::session::protocol_code(protocol),
            sys,
            usr,
        )
    } else {
        raw_hash?
    };

    // Step 8 has no client-supplied seed, so the canonical fingerprint is the
    // affinity seed. This also applies when a generic client ID is deliberately
    // fingerprinted via `constant_session_mode == "fingerprint_generic_ids"`.
    // The instance secret is always present in production; an empty key keeps
    // unit-level fixtures deterministic.
    let affinity_seed = if use_step8 {
        session_hash.as_str()
    } else {
        raw_session.as_str()
    };
    let salted_token = compute_salted_affinity_token(
        session_hmac_secret.unwrap_or(b""),
        &client_key.owner_user_id,
        affinity_seed,
    );
    let outbound_raw_session = if use_step8 {
        String::new()
    } else {
        raw_session
    };

    Some(ResolvedSession {
        pin_key: PinKey {
            client_key_id: client_key.id,
            canonical_id: canonical_id.to_string(),
            session_hash,
        },
        raw_session: outbound_raw_session,
        salted_token,
    })
}

#[cfg(test)]
fn session_pin_key(
    headers: &HeaderMap,
    body: &[u8],
    context: &SessionPinContext<'_>,
) -> Option<PinKey> {
    resolve_session(headers, body, context).map(|session| session.pin_key)
}

/// Extract a `Retry-After` delay in milliseconds from an upstream response.
/// Read at most [`MAX_ERROR_BODY_BYTES`] from a non-success upstream response.
async fn read_bounded_error_body(response: reqwest::Response) -> Bytes {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::with_capacity(ERROR_CLASSIFY_BYTES);
    while let Some(Ok(chunk)) = stream.next().await {
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(buffer.len());
        if remaining == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if buffer.len() >= MAX_ERROR_BODY_BYTES {
            break;
        }
    }
    Bytes::from(buffer)
}

/// Return a classified upstream error body verbatim with the filtered response headers.
fn upstream_error_response(
    status: StatusCode,
    upstream_headers: &HeaderMap,
    body: Bytes,
    candidate: &RouteCandidate,
    request_id: &str,
    is_admin: bool,
) -> Response {
    let headers_out = llmroller_proto::headers::apply_response_header_pipeline(
        upstream_headers,
        candidate.pool_tier(),
        is_admin,
        request_id,
        &candidate.config.response_header_allow,
    );
    let mut builder = Response::builder().status(status);
    for (name, value) in headers_out {
        if let Some(name) = name {
            builder = builder.header(name, value);
        }
    }
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn retry_after_ms(headers: &HeaderMap) -> Option<i64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|seconds| *seconds >= 0)
        .map(|seconds| seconds.saturating_mul(1000))
}

fn gate_error_response(
    error: &llmroller_core::accumulators::GateG7Error,
    is_messages: bool,
) -> Response {
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

    let message = format!("monthly budget cap exhausted (used {used} of {cap} micro-USD)");
    let mut response = protocol_error(
        is_messages,
        StatusCode::TOO_MANY_REQUESTS,
        code,
        message.clone(),
        None,
        // `F-022`: every 429 must advertise a retry horizon.
        Some(retry_after.unwrap_or(60)),
    );
    // Surface the counters the control plane and clients use to explain the cap.
    let body = if is_messages {
        serde_json::json!({
            "type": "error",
            "error": {
                "type": "rate_limit_error",
                "code": code,
                "message": message,
                "used_micro_usd": used,
                "budget_micro_usd": cap,
            },
            "request_id": serde_json::Value::Null,
        })
    } else {
        serde_json::json!({
            "error": {
                "message": message,
                "type": "rate_limit_error",
                "param": serde_json::Value::Null,
                "code": code,
                "used_micro_usd": used,
                "budget_micro_usd": cap,
            }
        })
    };
    if let Ok(bytes) = serde_json::to_vec(&body) {
        *response.body_mut() = Body::from(bytes);
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    response
}

/// Request content prepared when the turn finalizes, off the TTFB path (PERF-5).
struct PreparedTraceRequest {
    skeleton: serde_json::Value,
    blocks: Vec<(String, Vec<u8>)>,
    block_refs: Vec<String>,
    dropped_blocks: u32,
    expires_at: i64,
}

/// Streaming collector paired with the original request bytes until the turn finalizes.
/// Preparation is deliberately deferred so trace mode does not add TTFB work.
struct TraceCapture {
    request_body: Bytes,
    retention_days: i64,
    tap: TraceTap,
}

impl TraceCapture {
    fn new(request_body: Bytes, retention_days: i64, first_chunk: Option<&Bytes>) -> Self {
        let (mut tap, _receiver) = TraceTap::new();
        if let Some(chunk) = first_chunk {
            tap.feed_sse_chunk(chunk);
        }
        Self {
            request_body,
            retention_days,
            tap,
        }
    }
}

/// Extract the sub-agent thread identifier for trace records (TRC-4).
///
/// Claude Code subagents share the parent session/pin but add their agent id;
/// other compatible clients use `x-subagent-id`. Header values are trimmed and
/// capped at 256 UTF-8 bytes before persisting.
fn extract_trace_thread_id(headers: &HeaderMap) -> String {
    const MAX_THREAD_ID_BYTES: usize = 256;

    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let Some(raw) = value("x-claude-code-agent-id").or_else(|| value("x-subagent-id")) else {
        return "main".to_string();
    };

    let mut end = raw.len().min(MAX_THREAD_ID_BYTES);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    raw[..end].to_string()
}

/// Build the request skeleton and content-addressed blocks for one turn (TRC-2, TRC-4).
fn prepare_trace_request(
    body: &[u8],
    retention_days: i64,
    now_ms: i64,
) -> Option<PreparedTraceRequest> {
    let mut request: serde_json::Value = serde_json::from_slice(body).ok()?;
    replace_binary_blocks(&mut request);
    let prepared = prepare_request_blocks(&request);
    let retention_ms = retention_days.max(1).saturating_mul(86_400_000);
    Some(PreparedTraceRequest {
        skeleton: prepared.skeleton,
        blocks: prepared.blocks,
        block_refs: prepared.block_refs,
        dropped_blocks: prepared.dropped_blocks,
        expires_at: now_ms.saturating_add(retention_ms),
    })
}

/// Static attribution required to seal one trace record.
struct TraceJobMeta<'a> {
    usage_event_id: &'a str,
    request_id: RequestId,
    ts: i64,
    owner_user_id: UserId,
    client_key_id: ClientKeyId,
    session_hash: &'a str,
    thread_id: &'a str,
    canonical_id: &'a str,
    protocol: &'a str,
    response_raw: Option<serde_json::Value>,
}

/// Submit a sealed turn to the sealer pool. Returns `false` when tracing is unavailable
/// or the bounded pool rejected the job, which is reflected in `usage_events.trace_dropped`.
fn submit_trace_job(
    state: &ProxyAuthState,
    meta: TraceJobMeta<'_>,
    prepared: PreparedTraceRequest,
    output: &NormalizedTurnOutput,
    tap_dropped: bool,
) -> bool {
    let (Some(pool), Some(master_key_id)) = (&state.sealer_pool, &state.master_key_id) else {
        return false;
    };

    let truncated_content = output.truncated || prepared.dropped_blocks > 0 || tap_dropped;
    let response = serde_json::to_value(output).unwrap_or_else(|_| serde_json::json!({}));
    let turn_trace = StoredTurnTraceV2 {
        v: 2,
        request_skeleton: prepared.skeleton,
        block_refs: prepared.block_refs,
        response,
        response_raw: meta.response_raw,
        truncation: truncated_content.then(|| "head_tail".to_string()),
    };

    pool.try_submit(SealJob {
        master_key_id: master_key_id.clone(),
        usage_event_id: meta.usage_event_id.to_string(),
        request_id: meta.request_id,
        ts: meta.ts,
        owner_user_id: meta.owner_user_id,
        client_key_id: meta.client_key_id,
        session_hash: meta.session_hash.to_string(),
        thread_id: meta.thread_id.to_string(),
        canonical_id: meta.canonical_id.to_string(),
        protocol: meta.protocol.to_string(),
        truncated_content,
        turn_trace,
        blocks: prepared.blocks,
        expires_at: prepared.expires_at,
    })
}

/// Static usage attribution shared by the non-streaming and streaming finalizers.
#[derive(Debug, Clone)]
struct UsageContext {
    request_id: RequestId,
    client_key_id: ClientKeyId,
    owner_user_id: UserId,
    canonical_model: String,
    upstream_key_id: Option<String>,
    provider_id: Option<String>,
    provider_scope: Option<String>,
    share_id: Option<String>,
    upstream_model_id: String,
    endpoint_str: String,
    session_hash: Option<String>,
    pin_outcome: Option<String>,
    failovers: i64,
    error_code: Option<String>,
    billing_coverage_mode: String,
    plan_multiplier_bps: i64,
    service_account_id: Option<String>,
    request_headers_json: Option<String>,
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
        stop_reason: Option<String>,
        ttfb_ms: Option<i64>,
        latency_ms: Option<i64>,
    ) -> UsageEventRecord {
        let list_cost_micro_usd = cost_micro_usd;
        let effective_cost = llmroller_core::pricing::apply_plan_multiplier(
            list_cost_micro_usd,
            self.plan_multiplier_bps,
            &self.billing_coverage_mode,
        );
        UsageEventRecord {
            id: llmroller_core::ids::UsageEventId::generate().to_string(),
            ts,
            request_id: self.request_id,
            client_key_id: self.client_key_id,
            owner_user_id: self.owner_user_id,
            upstream_key_id: self.upstream_key_id.clone(),
            provider_id: self.provider_id.clone(),
            provider_scope: self.provider_scope.clone(),
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
            cost_micro_usd: effective_cost,
            cost_source,
            latency_ms,
            ttfb_ms,
            pin_outcome: self.pin_outcome.clone(),
            failovers: self.failovers,
            error_code: self.error_code.clone(),
            list_cost_micro_usd,
            plan_multiplier_bps: self.plan_multiplier_bps,
            billing_coverage_mode: self.billing_coverage_mode.clone(),
            service_account_id: self.service_account_id.clone(),
            stop_reason,
            request_headers_json: self.request_headers_json.clone(),
            metadata_json: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn attempt_record(
    request_id: RequestId,
    attempt_no: i64,
    ts: i64,
    key_id: UpstreamKeyId,
    share_id: Option<String>,
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
        share_id,
        outcome,
        http_status: http_status.map(i64::from),
        bytes_sent: bytes_sent as i64,
        charged_micro_usd,
        charge_basis: charge_basis.to_string(),
    }
}

/// Owns all post-stream accounting and finalizes exactly once on EOF or client drop.
/// Deferred status write for a response link created on a streamed `response.created` frame.
struct ResponseLinkFinalizer {
    response_id: String,
    owner_user_id: UserId,
    client_key_id: ClientKeyId,
    upstream_key_id: UpstreamKeyId,
    model_pk: ModelPk,
    session_hash: String,
    store_flag: bool,
    created_at: i64,
}

struct StreamFinalizer {
    state: ProxyAuthState,
    usage: UsageContext,
    attempt_key_id: UpstreamKeyId,
    http_status: u16,
    tap: SseUsageTap,
    thread_id: String,
    reservation: Option<Reservation>,
    client_guard: Option<MonthReservationGuard>,
    cap_guards: CandidateCapGuards,
    price_entry: Option<PriceEntry>,
    fallback_rates: Rates,
    attempts: Vec<UsageAttemptRecord>,
    response_link: Option<ResponseLinkFinalizer>,
    trace: Option<TraceCapture>,
    estimated_input_tokens: i64,
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
        let mut extracted = self.tap.finalize_usage();
        let is_incomplete = termination.is_incomplete();
        if is_incomplete
            && extracted.input_uncached_tokens == 0
            && extracted.cache_read_tokens == 0
            && extracted.cache_write_tokens == 0
        {
            extracted.input_uncached_tokens = self.estimated_input_tokens;
        }
        let mut cost = compute_request_cost(
            extracted
                .provider_cost_micro_usd
                .map(|micro| micro as f64 / 1_000_000.0),
            extracted.to_token_usage(),
            self.price_entry.as_ref(),
            self.fallback_rates,
            self.started_ms,
        );
        if is_incomplete && extracted.provider_cost_micro_usd.is_none() {
            cost.cost_source = CostSource::EstimatedIncomplete;
        }
        let effective_cost = llmroller_core::pricing::apply_plan_multiplier(
            cost.cost_micro_usd,
            self.usage.plan_multiplier_bps,
            &self.usage.billing_coverage_mode,
        );

        if let Some(reservation) = self.reservation.take() {
            reservation.commit(effective_cost, finished_ms);
        }
        if let Some(guard) = self.client_guard.take() {
            guard.commit(effective_cost);
        }
        self.cap_guards.commit(effective_cost);

        let served_attempt = attempt_record(
            self.usage.request_id,
            self.attempts.len() as i64 + 1,
            finished_ms,
            self.attempt_key_id,
            self.usage.share_id.clone(),
            llmroller_core::usage::AttemptOutcome::Served,
            Some(self.http_status),
            0,
            effective_cost,
            "actual",
        );
        self.attempts.push(served_attempt);

        if let Some(writer) = &self.state.writer {
            writer.queues.durable_state.lock().insert(
                llmroller_store::writer::DurableKey::ClientKeyTouch(
                    self.usage.client_key_id.to_string(),
                ),
                llmroller_store::writer::DurableOp::ClientKeyTouchLastUsed {
                    client_key_id: self.usage.client_key_id.to_string(),
                    last_used_at: finished_ms,
                },
            );
            let mut event = self.usage.event(
                finished_ms,
                self.http_status,
                true,
                termination.is_incomplete(),
                termination.as_str(),
                &extracted,
                cost.cost_micro_usd,
                cost.cost_source,
                self.tap.stop_reason.clone(),
                self.ttfb_ms,
                Some(finished_ms - self.started_ms),
            );
            if let Some(trace) = self.trace.take() {
                let accepted =
                    prepare_trace_request(&trace.request_body, trace.retention_days, finished_ms)
                        .is_some_and(|prepared| {
                            submit_trace_job(
                                &self.state,
                                TraceJobMeta {
                                    usage_event_id: &event.id,
                                    request_id: self.usage.request_id,
                                    ts: finished_ms,
                                    owner_user_id: self.usage.owner_user_id,
                                    client_key_id: self.usage.client_key_id,
                                    session_hash: self
                                        .usage
                                        .session_hash
                                        .as_deref()
                                        .unwrap_or_default(),
                                    thread_id: &self.thread_id,
                                    canonical_id: &self.usage.canonical_model,
                                    protocol: &self.usage.endpoint_str,
                                    response_raw: None,
                                },
                                prepared,
                                &trace.tap.output,
                                trace
                                    .tap
                                    .trace_dropped
                                    .load(std::sync::atomic::Ordering::Acquire),
                            )
                        });
                event.trace_dropped = !accepted;
            }
            writer
                .queues
                .usage_writer
                .enqueue(event, std::mem::take(&mut self.attempts));
        }

        let ep = match self.usage.endpoint_str.as_str() {
            "chat" => "chat",
            "responses" => "responses",
            "messages" => "messages",
            "responses_compact" => "responses_compact",
            _ => "chat",
        };
        crate::metrics::record_tokens(
            ep,
            "2xx",
            "stream",
            &self.usage.canonical_model,
            "input",
            extracted.input_uncached_tokens.max(0) as u64,
        );
        crate::metrics::record_tokens(
            ep,
            "2xx",
            "stream",
            &self.usage.canonical_model,
            "output",
            extracted.output_tokens.max(0) as u64,
        );
        if extracted.cache_read_tokens > 0 {
            crate::metrics::record_tokens(
                ep,
                "2xx",
                "stream",
                &self.usage.canonical_model,
                "cache_read",
                extracted.cache_read_tokens as u64,
            );
        }
        if cost.cost_micro_usd > 0 {
            crate::metrics::record_cost(
                ep,
                "2xx",
                "stream",
                &self.usage.canonical_model,
                cost.cost_micro_usd as u64,
            );
        }

        if let Some(link) = &self.response_link {
            let status = if termination.is_incomplete() {
                "incomplete"
            } else {
                "completed"
            };
            enqueue_response_link(
                &self.state,
                link.owner_user_id,
                link.client_key_id,
                link.model_pk,
                link.upstream_key_id,
                &link.session_hash,
                &link.response_id,
                status,
                link.store_flag,
                link.created_at,
            );
        }
    }
}

impl Drop for StreamFinalizer {
    fn drop(&mut self) {
        self.finalize(Termination::ClientGone);
    }
}

/// RAII counter for live SSE response bodies. The graceful-shutdown
/// supervisor waits for this to reach zero before cutting streams (REL-1).
#[derive(Debug)]
pub struct ActiveStreamGuard(Arc<AtomicUsize>);

impl ActiveStreamGuard {
    #[must_use]
    pub fn acquire(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Holds one in-flight slot on the selected route until the attempt or stream finishes.
struct InFlightGuard {
    tracker: Arc<llmroller_core::runtime_state::LoadTracker>,
    route: RouteId,
}

impl InFlightGuard {
    fn new(tracker: Arc<llmroller_core::runtime_state::LoadTracker>, route: RouteId) -> Self {
        tracker.begin(route);
        Self { tracker, route }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.tracker.end(self.route);
    }
}

fn upstream_content_encoding(headers: &HeaderMap) -> Option<String> {
    headers
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("identity"))
        .map(ToOwned::to_owned)
}

fn decode_upstream_body(encoding: Option<&str>, body: Bytes) -> Result<Bytes, ForwardError> {
    match encoding {
        Some(encoding) => decompress_chunk(encoding, &body).map(Bytes::from),
        None => Ok(body),
    }
}

type UpstreamByteStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

/// Decode a possibly compressed upstream body as a stateful stream so chunk boundaries
/// never matter (TRN-3). Unsupported encodings fail the stream and trigger failover.
fn decode_upstream_stream(
    response: reqwest::Response,
    encoding: Option<String>,
) -> UpstreamByteStream {
    let raw = response
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let Some(encoding) = encoding else {
        return Box::pin(raw);
    };
    let normalized = encoding.to_ascii_lowercase();
    let reader = StreamReader::new(raw);
    match normalized.as_str() {
        "gzip" => Box::pin(ReaderStream::new(GzipDecoder::new(reader))),
        "deflate" => Box::pin(ReaderStream::new(DeflateDecoder::new(reader))),
        "zlib" => Box::pin(ReaderStream::new(ZlibDecoder::new(reader))),
        "br" => Box::pin(ReaderStream::new(BrotliDecoder::new(reader))),
        "zstd" => Box::pin(ReaderStream::new(ZstdDecoder::new(reader))),
        _ => {
            let message = format!("unsupported content-encoding: {encoding}");
            Box::pin(stream::once(async move {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    message,
                ))
            }))
        }
    }
}

struct StreamState {
    upstream: UpstreamByteStream,
    pending: VecDeque<Bytes>,
    forwarder: SseChunkForwarder,
    finalizer: Option<StreamFinalizer>,
    lifecycle: RequestLifecycle,
    /// Gap allowed between upstream bytes after the first valid frame.
    idle_timeout: Duration,
    /// Set by the shutdown supervisor before it wakes parked streams.
    shutdown_cut: Arc<AtomicBool>,
    shutdown_notify: Option<Arc<tokio::sync::Notify>>,
    /// Held for exactly the lifetime of the response body stream.
    _stream_guard: ActiveStreamGuard,
    /// Keeps P2C in-flight load accurate until the response body ends.
    _in_flight: InFlightGuard,
    done: bool,
}

impl StreamState {
    /// Finalize accounting and the lifecycle guard exactly once for a terminal step.
    fn finish(&mut self, termination: Termination) {
        if let Some(finalizer) = &mut self.finalizer {
            finalizer.finalize(termination);
        }
        self.lifecycle.set_termination(termination);
        self.lifecycle.finalize();
        self.done = true;
    }
}

/// Outcome of one post-first-frame poll of the upstream body.
enum StreamStep {
    Item(Option<Result<Bytes, std::io::Error>>),
    Idle,
    Shutdown,
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
        EndpointKind::ResponsesCompact => "responses_compact",
        EndpointKind::Messages => "messages",
        _ => "chat",
    };
    let protocol = match endpoint {
        // The compact endpoint speaks the Responses wire protocol but has its own upstream path.
        EndpointKind::ResponsesCompact => "responses",
        _ => endpoint_str,
    };
    let aux_label = aux_endpoint_label(endpoint);
    // `F-021`, `F-022`: one protocol flag and one request id are established
    // before any early return so every error envelope is schema-conformant.
    let is_messages = matches!(endpoint, EndpointKind::Messages | EndpointKind::CountTokens);
    let request_id = RequestId::generate();
    let request_id_str = request_id.to_string();
    // `SEC-6`, `USG-9` (`F-011`): persist only the closed allow-list of
    // diagnostic request headers, scrubbed of secrets, for every event path.
    let request_headers_json = Some(llmroller_proto::headers::extract_safe_diagnostic_headers(
        &headers,
    ));

    let encoding = headers
        .get(axum::http::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();
    let body = if !encoding.is_empty() && encoding != "identity" {
        match crate::forward::decompress_chunk(encoding, &body) {
            Ok(decompressed) => Bytes::from(decompressed),
            Err(e) => {
                crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                return protocol_error(
                    is_messages,
                    StatusCode::BAD_REQUEST,
                    "invalid_encoding",
                    format!("failed to decompress request body: {e}"),
                    Some(&request_id_str),
                    None,
                );
            }
        }
    } else {
        body
    };

    // 1. Acquire preflight memory budget permit (up to 2,000 ms)
    let permit = match state
        .budget_semaphore
        .acquire(body.len(), Duration::from_millis(2000))
        .await
    {
        Ok(permit) => permit,
        Err(error) => {
            crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
            return protocol_error(
                is_messages,
                error.status_code(),
                "gateway_busy",
                error.to_string(),
                Some(&request_id_str),
                Some(error.retry_after_secs()),
            );
        }
    };

    // 2. Scan and validate body envelope and duplicate keys
    let scan = match scan_top_level(&body) {
        Ok(scan) => scan,
        Err(error) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return protocol_error(
                is_messages,
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                error.to_string(),
                Some(&request_id_str),
                None,
            );
        }
    };

    let Some((ref model_str, _)) = scan.model else {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return protocol_error(
            is_messages,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model is required in request body",
            Some(&request_id_str),
            None,
        );
    };

    // Compaction is a one-shot operation; force the non-streaming path even if the
    // client supplied `stream: true` (API-1).
    let is_streaming = endpoint != EndpointKind::ResponsesCompact
        && scan.stream.as_ref().is_some_and(|(stream, _)| *stream);
    let store_flag = response_store_flag(&scan, &body);
    let now = now_ms();

    let snapshot = state.snapshot.load();
    // 3. Canonical model resolution is a pure in-memory lookup (PERF-1).
    let model = match snapshot.resolve_model(model_str) {
        Ok(model) => model.clone(),
        Err(llmroller_core::snapshot::SnapshotResolveError::NotFound) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return protocol_error(
                is_messages,
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("model {model_str} not found in catalog"),
                Some(&request_id_str),
                None,
            );
        }
        Err(llmroller_core::snapshot::SnapshotResolveError::Ambiguous) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return protocol_error(
                is_messages,
                StatusCode::CONFLICT,
                "ambiguous_model_id",
                format!("model {model_str} matches more than one catalog entry"),
                Some(&request_id_str),
                None,
            );
        }
        Err(llmroller_core::snapshot::SnapshotResolveError::KillSwitched) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return protocol_error(
                is_messages,
                StatusCode::FORBIDDEN,
                "model_disabled",
                format!("model {model_str} is disabled"),
                Some(&request_id_str),
                None,
            );
        }
        Err(llmroller_core::snapshot::SnapshotResolveError::RedirectLoop) => {
            crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
            return protocol_error(
                is_messages,
                StatusCode::INTERNAL_SERVER_ERROR,
                "model_redirect_loop",
                "catalog redirect chain contains a cycle",
                Some(&request_id_str),
                None,
            );
        }
    };

    let Some(caller) = client_key.clone() else {
        return protocol_error(
            is_messages,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "authentication required",
            Some(&request_id_str),
            None,
        );
    };
    // Response header policy R4 depends on the caller role, not the request path (SEC-16).
    let is_admin = snapshot
        .users
        .get(&caller.owner_user_id)
        .is_some_and(|user| user.role == Role::Admin);

    // 4. Gate G6: client key model allow-list.
    if let Some(ref allowed) = caller.allowed_models
        && !allowed
            .iter()
            .any(|entry| entry == &model.canonical_id || entry == model_str)
    {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return protocol_error(
            is_messages,
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("model {} not permitted on this key", model.canonical_id),
            Some(&request_id_str),
            None,
        );
    }

    let scope = parse_scope(&caller.routing_scope);
    // Trace mode is decided exactly once at request start (TRC-1).
    let trace_config = &snapshot.settings.traceability_config;
    let trace_enabled = state.sealer_pool.is_some()
        && state.master_key_id.is_some()
        && llmroller_core::trace_config::should_trace_request(
            trace_config,
            &caller.owner_user_id,
            &caller.id,
            &model.canonical_id,
            now,
        );
    let mut trace_request = if trace_enabled {
        Some(body.clone())
    } else {
        None
    };
    let trace_retention_days = trace_config.retention_days;
    let trace_thread_id = extract_trace_thread_id(&headers);
    // 5. Response-link session inheritance and HardKey affinity (RTE-12, Section 11.1.2).
    let hard_link: Option<ResponseLinkRow> = if matches!(
        endpoint,
        EndpointKind::Responses | EndpointKind::ResponsesCompact
    ) {
        match scan.previous_response_id.as_ref() {
            Some((previous_id, _)) => match load_response_link(&state, previous_id).await {
                Ok(Some(link)) if link.owner_user_id != caller.owner_user_id => {
                    crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                    return response_not_found(previous_id);
                }
                Ok(Some(link)) if now <= link.expires_at => Some(link),
                Ok(_) => None,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            },
            None => None,
        }
    } else {
        None
    };

    let (selected_tier, tier_candidates) = if let Some(link) = &hard_link {
        use crate::failover::{HardRouteDecision, evaluate_hard_route_decision};

        let (checks, retry_after) = hard_route_checks(
            &snapshot,
            &state.runtime_state,
            link,
            &caller,
            protocol,
            now,
        );
        match evaluate_hard_route_decision(&checks, link.upstream_key_id, retry_after) {
            HardRouteDecision::RouteWithoutFailover(_) => {
                let Some(candidate) = linked_candidate(&snapshot, link, &caller) else {
                    return permission_error(
                        "the upstream key that holds previous_response_id is no longer available to this client key",
                    );
                };
                let tier = candidate.pool_tier;
                (tier, vec![candidate])
            }
            HardRouteDecision::RateLimit429 { retry_after_secs } => {
                crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                return rate_limit_error(retry_after_secs);
            }
            HardRouteDecision::Forbidden403KeyUnavailable => {
                state.pin_store.invalidate_key(&link.upstream_key_id);
                return permission_error(
                    "the upstream key that holds previous_response_id is no longer available to this client key",
                );
            }
            HardRouteDecision::Forbidden403ModelOrProtocolBlocked => {
                return permission_error(
                    "the model or protocol that holds previous_response_id is no longer available",
                );
            }
        }
    } else {
        let candidates = collect_candidates(&snapshot, &model, &caller, scope, now);
        let had_candidates = !candidates.is_empty();
        let candidates: Vec<RouteCandidate> = candidates
            .into_iter()
            .filter(|candidate| {
                aux_label.is_none_or(|label| {
                    !state.runtime_state.is_aux_endpoint_unsupported(
                        candidate.upstream_key_id,
                        label,
                        now,
                    )
                })
            })
            .collect();
        if candidates.is_empty() {
            crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
            if aux_label.is_some() && had_candidates {
                return aux_endpoint_unsupported_error(is_messages);
            }
            return protocol_error(
                is_messages,
                StatusCode::SERVICE_UNAVAILABLE,
                "no_eligible_route",
                "no eligible upstream route available for this caller",
                Some(&request_id_str),
                Some(1),
            );
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
                    protocol,
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
            present_on_all_global_providers: present_on_all_global_providers(&snapshot, &model),
            global_exposure_mode: snapshot.settings.global_exposure_mode,
            inbound_protocol: protocol,
            model_canonical_id: &model.canonical_id,
        };

        let (selected_tier, eligible_pool) =
            match select_pool(&eligibility, &context, shares, scope) {
                Ok(selection) => selection,
                Err(PoolSelectionError::MonthlyCapExhausted) => {
                    crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                    return protocol_error(
                        is_messages,
                        StatusCode::TOO_MANY_REQUESTS,
                        "client_key_budget_exhausted",
                        "monthly budget capacity exhausted",
                        Some(&request_id_str),
                        Some(60),
                    );
                }
                Err(PoolSelectionError::NoEligibleRoute) => {
                    crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
                    return protocol_error(
                        is_messages,
                        StatusCode::SERVICE_UNAVAILABLE,
                        "no_eligible_route",
                        "no eligible upstream route available for this caller",
                        Some(&request_id_str),
                        Some(1),
                    );
                }
            };

        let tier_candidates: Vec<RouteCandidate> = eligible_pool
            .iter()
            .filter_map(|eligible| {
                candidates
                    .iter()
                    .find(|candidate| candidate.upstream_key_id == eligible.upstream_key_id)
                    .cloned()
            })
            .collect();
        (selected_tier, tier_candidates)
    };

    if tier_candidates.is_empty() {
        crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
        return protocol_error(
            is_messages,
            StatusCode::SERVICE_UNAVAILABLE,
            "no_eligible_route",
            "no eligible upstream route available for this caller",
            Some(&request_id_str),
            Some(1),
        );
    }

    let eligible_keys: HashSet<UpstreamKeyId> = tier_candidates
        .iter()
        .map(|candidate| candidate.upstream_key_id)
        .collect();

    // Stage 1: Gate G7 monthly caps are reserved before any upstream connection.
    let requested_output = requested_max_output_tokens(&body);
    let estimate = estimate_cost_micro_usd(
        &snapshot,
        &tier_candidates[0],
        estimate_input_tokens(&body),
        requested_output,
    )
    .max(estimate_reservation_cost_micro_usd(
        estimate_input_tokens(&body),
        0.0,
        &Rates::from(&snapshot.settings.unpriced_fallback_rates),
        requested_output,
        None,
    ));
    let client_guard = match state.runtime_state.accumulators.try_reserve(
        llmroller_core::accumulators::BudgetScopeId::ClientKey(caller.id),
        caller.budget_micro_usd_month,
        estimate,
        now,
    ) {
        Ok(guard) => Some(guard),
        Err(error) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return gate_error_response(&error, is_messages);
        }
    };
    let mut client_guard = client_guard;

    // Stage 6: sticky pin acquisition for an identified session.
    let strategy = model
        .routing_strategy
        .unwrap_or(snapshot.settings.default_routing_strategy);
    let session_hmac_secret = state
        .session_hmac_secret
        .as_ref()
        .map(|secret| secret.expose_secret());
    let resolved_session = resolve_session(
        &headers,
        &body,
        &SessionPinContext {
            client_key: &caller,
            canonical_id: &model.canonical_id,
            protocol,
            session_hmac_secret,
            runtime_state: &state.runtime_state,
            now_ms: now,
        },
    );
    let mut pin_key = resolved_session
        .as_ref()
        .map(|session| session.pin_key.clone());
    let pin_ttl_ms = snapshot.settings.pin_ttl_secs.saturating_mul(1000);
    let clock = SystemClock::new();
    let mut pin_ticket: Option<PinTicket> = None;
    let mut ordered: Vec<RouteCandidate> = Vec::new();

    if let Some(link) = &hard_link {
        // HardKey affinity inherits the parent session_hash and disables repinning (RTE-12).
        pin_key = if link.session_hash.is_empty() {
            None
        } else {
            Some(PinKey {
                client_key_id: caller.id,
                canonical_id: model.canonical_id.clone(),
                session_hash: link.session_hash.clone(),
            })
        };
        if let (Some(pin_key), Some(candidate)) = (pin_key.as_ref(), tier_candidates.first()) {
            let ticket = state.pin_store.acquire(
                pin_key,
                &clock,
                pin_ttl_ms,
                false,
                snapshot.settings.recovery_repin_min_idle_secs,
                scope,
                Some(selected_tier),
                |key_id| key_id == candidate.upstream_key_id,
                |_tried| Some((candidate.upstream_key_id, candidate.pool_tier)),
            );
            if let Some(mut ticket) = ticket {
                ticket.outcome = PinOutcome::ResponseLink;
                ordered.push(candidate.clone());
                pin_ticket = Some(ticket);
            }
        }
    } else if let Some(pin_key) = &pin_key {
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
                    &model.canonical_id,
                    protocol,
                    selected_tier,
                    &tier_candidates,
                    tried,
                    now,
                    state.entropy.as_ref(),
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
        canonical_id: &model.canonical_id,
        protocol,
        selected_tier,
        tier_candidates: &tier_candidates,
        eligible_keys: &eligible_keys,
        now,
        entropy: state.entropy.as_ref(),
    };
    let mut pinned_order = ordered;
    let mut tried: Vec<UpstreamKeyId> = Vec::new();

    let mut attempts: Vec<UsageAttemptRecord> = Vec::new();
    let mut try_outcomes: Vec<TryOutcome> = Vec::new();
    let mut aux_unsupported_seen = false;
    let mut last_retry_after_ms: Option<i64> = None;
    let mut last_cap_error: Option<GateG7Error> = None;
    let mut failovers = 0_i64;
    let mut permit = Some(permit);
    let mut attempt_no: usize = 0;

    loop {
        if attempt_no >= MAX_UPSTREAM_ATTEMPTS
            || (attempt_no >= 1 && now_ms().saturating_sub(now) >= 60_000)
        {
            break;
        }
        let candidate = if let Some(candidate) = pinned_order.pop() {
            candidate
        } else {
            match select_next_candidate(
                &state.runtime_state,
                strategy,
                caller.owner_user_id,
                model.id,
                &model.canonical_id,
                protocol,
                selected_tier,
                &tier_candidates,
                &tried,
                now,
                state.entropy.as_ref(),
            ) {
                Some(candidate) => candidate,
                None => break,
            }
        };
        tried.push(candidate.upstream_key_id);
        let candidate_share_id = candidate.share_id.map(|s| s.to_string());
        attempt_no += 1;
        let origin = OriginKey::from_base_url(&candidate.base_url, candidate.allow_private_network);
        let Some(key) = snapshot
            .upstream_keys
            .get(&candidate.upstream_key_id)
            .cloned()
        else {
            try_outcomes.push(TryOutcome::Unavailable);
            continue;
        };
        let secret = match unseal_upstream_key(&state, &snapshot, &key) {
            Ok(secret) => secret,
            Err(()) => {
                try_outcomes.push(TryOutcome::Unavailable);
                attempts.push(attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    llmroller_core::usage::AttemptOutcome::ConnectError,
                    None,
                    body.len(),
                    0,
                    "none",
                ));
                continue;
            }
        };

        let candidate_rates = snapshot
            .price_for(
                &candidate.provider_id,
                candidate.provider_kind,
                &candidate.upstream_model_id,
            )
            .map_or(
                Rates::from(&snapshot.settings.unpriced_fallback_rates),
                |entry| entry.base_rates,
            );
        let input_tokens_est = estimate_input_tokens(&body);
        let (cand_cov_mode, cand_mult_bps) = snapshot
            .providers
            .get(&candidate.provider_id)
            .map(|p| (p.billing_coverage_mode.as_str(), p.plan_multiplier_bps))
            .unwrap_or(("pay_as_you_go", 10_000));
        let attempt_estimate = llmroller_core::pricing::apply_plan_multiplier(
            estimate_reservation_cost_micro_usd(
                input_tokens_est,
                0.0,
                &candidate_rates,
                requested_output,
                None,
            ),
            cand_mult_bps,
            cand_cov_mode,
        );
        let input_only_estimate = llmroller_core::pricing::apply_plan_multiplier(
            (i128::from(input_tokens_est.max(0))
                * i128::from(candidate_rates.input_per_mtok_micro_usd.max(0))
                + 999_999)
                .div_euclid(1_000_000) as i64,
            cand_mult_bps,
            cand_cov_mode,
        );
        let mut cap_guards = match reserve_candidate_caps(
            &state.runtime_state.accumulators,
            &snapshot,
            caller.owner_user_id,
            &candidate,
            attempt_estimate,
            now,
        ) {
            Ok(guards) => guards,
            Err(error) => {
                attempts.push(attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    llmroller_core::usage::AttemptOutcome::Http429,
                    Some(429),
                    body.len(),
                    0,
                    "none",
                ));
                last_cap_error = Some(error);
                try_outcomes.push(TryOutcome::RateLimited {
                    retry_after_secs: Some(1),
                });
                continue;
            }
        };
        let mut reservation = match state.runtime_state.reserve_key_windows(
            &snapshot,
            candidate.upstream_key_id,
            Some(candidate.model_pk),
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
                    candidate_share_id.clone(),
                    llmroller_core::usage::AttemptOutcome::Http429,
                    Some(429),
                    body.len(),
                    0,
                    "none",
                ));
                try_outcomes.push(TryOutcome::RateLimited {
                    retry_after_secs: Some(1),
                });
                continue;
            }
        };

        let target_url = endpoint_url(&candidate, endpoint);
        if crate::ssrf::validate_url(&target_url, candidate.allow_private_network).is_err() {
            attempts.push(attempt_record(
                request_id,
                attempt_no as i64,
                now,
                candidate.upstream_key_id,
                candidate_share_id.clone(),
                llmroller_core::usage::AttemptOutcome::ConnectError,
                None,
                body.len(),
                0,
                "none",
            ));
            try_outcomes.push(TryOutcome::HostFailure);
            continue;
        }
        let client = state.select_http_client(candidate.allow_private_network);

        // Claim this origin for the attempt. The guard releases a half-open probe
        // slot if the attempt is cancelled before recording an outcome (REL-5).
        let Some(_origin_permit) = state.runtime_state.acquire_origin_permit(&origin, now) else {
            try_outcomes.push(TryOutcome::BreakerOpen);
            continue;
        };

        // Count the attempt on the selected route for the whole upstream call. Streaming
        // responses move this guard into StreamState so P2C sees load until the body ends.
        let mut in_flight = Some(InFlightGuard::new(
            Arc::clone(&state.runtime_state.load_tracker),
            candidate.route_id(),
        ));

        // Only Chat Completions understands `stream_options`; native Messages and
        // Responses streams must pass through byte-identically (`API-4`, `TRN-2`).
        let chat_stream_mode = if is_streaming && endpoint == EndpointKind::Chat {
            candidate.config.usage_reporting.chat_stream.map_or_else(
                || get_adapter(candidate.provider_kind).default_chat_stream_mode(),
                ChatStreamMode::from,
            )
        } else {
            ChatStreamMode::Native
        };
        let session_profile = candidate.session_profile();
        let session_token = resolved_session
            .as_ref()
            .map(|session| session.outbound_token(candidate.pool_tier()));
        let session_field = session_profile
            .body_field
            .as_deref()
            .zip(session_token)
            .filter(|_| matches!(endpoint, EndpointKind::Chat | EndpointKind::Responses));
        let (edited_body, strip_injected_usage) = match edit_body_with_session_limit(
            body.clone(),
            candidate.pool_tier(),
            Some(&candidate.upstream_model_id),
            chat_stream_mode,
            session_field,
            session_profile.max_len,
        ) {
            Ok(edited) => edited,
            Err(_) => (body.clone(), false),
        };

        let outbound_headers = match build_upstream_headers(
            &headers,
            &candidate,
            &secret,
            endpoint,
            resolved_session.as_ref(),
        ) {
            Ok(headers) => headers,
            Err(response) => return *response,
        };

        let mut request_builder = client.post(&target_url).body(edited_body.clone());
        for (name, value) in &outbound_headers {
            request_builder = request_builder.header(name, value);
        }

        let attempt_started = now_ms();
        let activity_timeout = Duration::from_millis(candidate.config.first_activity_timeout_ms());
        let send_res = tokio::time::timeout(activity_timeout, request_builder.send()).await;
        let mut response = match send_res {
            Ok(Ok(response)) => response,
            Ok(Err(_)) | Err(_) => {
                let is_timeout = send_res.is_err();
                let attempt_outcome = if is_timeout {
                    llmroller_core::usage::AttemptOutcome::WaitTimeout
                } else {
                    llmroller_core::usage::AttemptOutcome::ConnectError
                };
                let try_outcome = if is_timeout {
                    TryOutcome::WaitTimeout
                } else {
                    TryOutcome::HostFailure
                };
                let _ = state.runtime_state.record_upstream_failure(
                    &origin,
                    candidate.upstream_key_id,
                    caller.id,
                    attempt_outcome,
                    None,
                    now,
                );
                enqueue_key_health(
                    state.writer.as_ref(),
                    &state.runtime_state,
                    candidate.upstream_key_id,
                    now,
                );
                let (charged_micro_usd, charge_basis) =
                    failure_charge(attempt_outcome, input_only_estimate);
                if charged_micro_usd > 0
                    && let Some(res) = reservation.take()
                {
                    res.commit(charged_micro_usd, now);
                }
                let att = attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    attempt_outcome,
                    Some(502),
                    body.len(),
                    charged_micro_usd,
                    charge_basis,
                );
                attempts.push(att);
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
                crate::metrics::record_failover();
                try_outcomes.push(try_outcome);
                continue;
            }
        };

        let mut status = response.status();

        // For status 429: if Retry-After <= 2 s, sleep Retry-After once on attempt 1
        // and retry the same key to preserve prompt cache (Section 10.5, RTE-8).
        if status == StatusCode::TOO_MANY_REQUESTS && attempt_no == 1 {
            let delay = retry_after_ms(response.headers());
            if let Some(delay_ms) = delay
                && delay_ms <= 2000
            {
                attempts.push(attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    llmroller_core::usage::AttemptOutcome::Http429,
                    Some(status.as_u16()),
                    body.len(),
                    0,
                    "none",
                ));
                tokio::time::sleep(Duration::from_millis(delay_ms as u64)).await;

                let mut retry_builder = client.post(&target_url).body(edited_body.clone());
                for (name, value) in &outbound_headers {
                    retry_builder = retry_builder.header(name, value);
                }
                if let Ok(retry_resp) = retry_builder.send().await {
                    response = retry_resp;
                    status = response.status();
                }
            }
        }

        if !status.is_success() {
            last_retry_after_ms = retry_after_ms(response.headers());
            let upstream_headers = response.headers().clone();
            let error_body = read_bounded_error_body(response).await;
            // Auxiliary endpoints have independent support. A 404/405/501 means this
            // key does not implement the endpoint; cache that fact without treating it
            // as a credential or host failure (API-8).
            if let Some(label) = aux_label
                && matches!(
                    status,
                    StatusCode::NOT_FOUND
                        | StatusCode::METHOD_NOT_ALLOWED
                        | StatusCode::NOT_IMPLEMENTED
                )
            {
                state.runtime_state.mark_aux_endpoint_unsupported(
                    candidate.upstream_key_id,
                    label,
                    now.saturating_add(AUX_UNSUPPORTED_TTL_MS),
                );
                aux_unsupported_seen = true;
                attempts.push(attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    llmroller_core::usage::AttemptOutcome::from_http_status(status.as_u16()),
                    Some(status.as_u16()),
                    body.len(),
                    0,
                    "none",
                ));
                continue;
            }
            let classify_prefix = &error_body[..error_body.len().min(ERROR_CLASSIFY_BYTES)];
            let class = classify_error_with_classifiers(
                status.as_u16(),
                classify_prefix,
                Some(&candidate.config.error_classifiers),
            );
            let retry_after_secs =
                last_retry_after_ms.map(|delay| u64::try_from((delay / 1000).max(1)).unwrap_or(1));

            let is_model_not_found_404 = llmroller_proto::adapters::is_model_not_found_error(
                status.as_u16(),
                classify_prefix,
                Some(&candidate.config.error_classifiers),
            );
            if is_model_not_found_404 {
                let until = now.saturating_add(600_000);
                state.runtime_state.block_route_protocol(
                    candidate.upstream_key_id,
                    candidate.model_pk,
                    protocol,
                    until,
                );
                enqueue_route_protocol_block(
                    &state,
                    candidate.upstream_key_id,
                    candidate.model_pk,
                    protocol,
                    "model_not_found_404",
                    until,
                );
                enqueue_route_health_update(
                    &state,
                    candidate.upstream_key_id,
                    candidate.model_pk,
                    "unavailable",
                    Some(until),
                    Some("model_not_found_404".to_string()),
                );
                if let Some(ref tx) = state.discovery_tx {
                    let _ = tx.send(llmroller_proto::discovery::DiscoveryTarget::Key(
                        candidate.upstream_key_id,
                    ));
                }
                try_outcomes.push(TryOutcome::Unavailable);
                let att = attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    llmroller_core::usage::AttemptOutcome::from_http_status(status.as_u16()),
                    Some(status.as_u16()),
                    body.len(),
                    0,
                    "none",
                );
                attempts.push(att);
                failovers += 1;
                crate::metrics::record_failover();
                continue;
            }

            match class {
                ErrorClass::RequestRejected | ErrorClass::Unclassified403 => {
                    if class == ErrorClass::Unclassified403 {
                        crate::metrics::record_unclassified_403(&candidate.provider_id.to_string());
                    }
                    // The upstream response is forwarded verbatim with zero health
                    // effect; moderation 403s must never quarantine or fail over.
                    let outcome =
                        llmroller_core::usage::AttemptOutcome::from_http_status(status.as_u16());
                    let att = attempt_record(
                        request_id,
                        attempt_no as i64,
                        now,
                        candidate.upstream_key_id,
                        candidate_share_id.clone(),
                        outcome,
                        Some(status.as_u16()),
                        body.len(),
                        0,
                        "none",
                    );
                    attempts.push(att);

                    if let Some(writer) = &state.writer {
                        let (billing_coverage_mode, plan_multiplier_bps) = snapshot
                            .providers
                            .get(&candidate.provider_id)
                            .map(|p| (p.billing_coverage_mode.clone(), p.plan_multiplier_bps))
                            .unwrap_or_else(|| ("pay_as_you_go".to_string(), 10_000));
                        let usage = UsageContext {
                            request_id,
                            client_key_id: caller.id,
                            owner_user_id: caller.owner_user_id,
                            canonical_model: model.canonical_id.clone(),
                            upstream_key_id: Some(candidate.upstream_key_id.to_string()),
                            provider_id: Some(candidate.provider_id.to_string()),
                            provider_scope: Some(provider_scope_label(&candidate).to_string()),
                            share_id: candidate.share_id.map(|share| share.to_string()),
                            upstream_model_id: candidate.upstream_model_id.clone(),
                            endpoint_str: endpoint_str.to_string(),
                            session_hash: pin_key.as_ref().map(|key| key.session_hash.clone()),
                            pin_outcome: pin_ticket
                                .as_ref()
                                .map(|ticket| ticket.outcome.as_str().to_string()),
                            failovers,
                            error_code: Some(if class == ErrorClass::Unclassified403 {
                                "upstream_error".to_string()
                            } else {
                                "invalid_request_error".to_string()
                            }),
                            billing_coverage_mode,
                            plan_multiplier_bps,
                            service_account_id: caller.service_account_id.map(|sa| sa.to_string()),
                            request_headers_json: request_headers_json.clone(),
                        };
                        let event = usage.event(
                            now,
                            status.as_u16(),
                            is_streaming,
                            false,
                            "completed",
                            &llmroller_proto::json_usage::ExtractedUsage::default(),
                            0,
                            CostSource::PriceBook,
                            None,
                            None,
                            Some(now_ms() - attempt_started),
                        );
                        writer
                            .queues
                            .usage_writer
                            .enqueue(event, std::mem::take(&mut attempts));
                    }

                    crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                    return upstream_error_response(
                        status,
                        &upstream_headers,
                        error_body,
                        &candidate,
                        &request_id.to_string(),
                        is_admin,
                    );
                }
                ErrorClass::PlanRestricted => {
                    let until = now.saturating_add(PROTOCOL_BLOCK_MS);
                    state.runtime_state.block_route_protocol(
                        candidate.upstream_key_id,
                        candidate.model_pk,
                        protocol,
                        until,
                    );
                    enqueue_route_protocol_block(
                        &state,
                        candidate.upstream_key_id,
                        candidate.model_pk,
                        protocol,
                        "plan_restricted",
                        until,
                    );
                    try_outcomes.push(TryOutcome::PlanRestricted);
                    attempts.push(attempt_record(
                        request_id,
                        attempt_no as i64,
                        now,
                        candidate.upstream_key_id,
                        candidate_share_id.clone(),
                        llmroller_core::usage::AttemptOutcome::Http403,
                        Some(status.as_u16()),
                        body.len(),
                        0,
                        "none",
                    ));
                }
                ErrorClass::QuotaExhausted => {
                    state.runtime_state.record_quota_exhausted(
                        candidate.upstream_key_id,
                        last_retry_after_ms,
                        now,
                    );
                    enqueue_key_health(
                        state.writer.as_ref(),
                        &state.runtime_state,
                        candidate.upstream_key_id,
                        now,
                    );
                    try_outcomes.push(TryOutcome::QuotaExhausted { retry_after_secs });
                    attempts.push(attempt_record(
                        request_id,
                        attempt_no as i64,
                        now,
                        candidate.upstream_key_id,
                        candidate_share_id.clone(),
                        llmroller_core::usage::AttemptOutcome::from_http_status(status.as_u16()),
                        Some(status.as_u16()),
                        body.len(),
                        0,
                        "none",
                    ));
                }
                ErrorClass::CredentialInvalid => {
                    let outcome = if status == StatusCode::UNAUTHORIZED {
                        llmroller_core::usage::AttemptOutcome::Http401
                    } else {
                        llmroller_core::usage::AttemptOutcome::Http403
                    };
                    let strike = state.runtime_state.record_upstream_failure(
                        &origin,
                        candidate.upstream_key_id,
                        caller.id,
                        outcome,
                        last_retry_after_ms,
                        now,
                    );
                    enqueue_key_health(
                        state.writer.as_ref(),
                        &state.runtime_state,
                        candidate.upstream_key_id,
                        now,
                    );
                    if strike == Some(StrikeResult::ProbeRequired) {
                        spawn_credential_probe(
                            &state,
                            &candidate.base_url,
                            candidate.allow_private_network,
                            &secret,
                            candidate.upstream_key_id,
                        );
                    }
                    try_outcomes.push(TryOutcome::HostFailure);
                    attempts.push(attempt_record(
                        request_id,
                        attempt_no as i64,
                        now,
                        candidate.upstream_key_id,
                        candidate_share_id.clone(),
                        outcome,
                        Some(status.as_u16()),
                        body.len(),
                        0,
                        "none",
                    ));
                }
                ErrorClass::Other(_) if status == StatusCode::TOO_MANY_REQUESTS => {
                    let outcome = llmroller_core::usage::AttemptOutcome::Http429;
                    let _ = state.runtime_state.record_upstream_failure(
                        &origin,
                        candidate.upstream_key_id,
                        caller.id,
                        outcome,
                        last_retry_after_ms,
                        now,
                    );
                    enqueue_key_health(
                        state.writer.as_ref(),
                        &state.runtime_state,
                        candidate.upstream_key_id,
                        now,
                    );
                    try_outcomes.push(TryOutcome::RateLimited { retry_after_secs });
                    attempts.push(attempt_record(
                        request_id,
                        attempt_no as i64,
                        now,
                        candidate.upstream_key_id,
                        candidate_share_id.clone(),
                        outcome,
                        Some(status.as_u16()),
                        body.len(),
                        0,
                        "none",
                    ));
                }
                ErrorClass::Other(_) if status.is_server_error() => {
                    let outcome = llmroller_core::usage::AttemptOutcome::Http5xx;
                    let _ = state.runtime_state.record_upstream_failure(
                        &origin,
                        candidate.upstream_key_id,
                        caller.id,
                        outcome,
                        last_retry_after_ms,
                        now,
                    );
                    enqueue_key_health(
                        state.writer.as_ref(),
                        &state.runtime_state,
                        candidate.upstream_key_id,
                        now,
                    );
                    try_outcomes.push(TryOutcome::HostFailure);
                    let (charged_micro_usd, charge_basis) =
                        failure_charge(outcome, input_only_estimate);
                    if charged_micro_usd > 0
                        && let Some(res) = reservation.take()
                    {
                        res.commit(charged_micro_usd, now);
                    }
                    let att = attempt_record(
                        request_id,
                        attempt_no as i64,
                        now,
                        candidate.upstream_key_id,
                        candidate_share_id.clone(),
                        outcome,
                        Some(status.as_u16()),
                        body.len(),
                        charged_micro_usd,
                        charge_basis,
                    );
                    attempts.push(att);
                }
                ErrorClass::Other(_) => {
                    let outcome =
                        llmroller_core::usage::AttemptOutcome::from_http_status(status.as_u16());
                    let att = attempt_record(
                        request_id,
                        attempt_no as i64,
                        now,
                        candidate.upstream_key_id,
                        candidate_share_id.clone(),
                        outcome,
                        Some(status.as_u16()),
                        body.len(),
                        0,
                        "none",
                    );
                    attempts.push(att);

                    if let Some(writer) = &state.writer {
                        let (billing_coverage_mode, plan_multiplier_bps) = snapshot
                            .providers
                            .get(&candidate.provider_id)
                            .map(|p| (p.billing_coverage_mode.clone(), p.plan_multiplier_bps))
                            .unwrap_or_else(|| ("pay_as_you_go".to_string(), 10_000));
                        let usage = UsageContext {
                            request_id,
                            client_key_id: caller.id,
                            owner_user_id: caller.owner_user_id,
                            canonical_model: model.canonical_id.clone(),
                            upstream_key_id: Some(candidate.upstream_key_id.to_string()),
                            provider_id: Some(candidate.provider_id.to_string()),
                            provider_scope: Some(provider_scope_label(&candidate).to_string()),
                            share_id: candidate.share_id.map(|share| share.to_string()),
                            upstream_model_id: candidate.upstream_model_id.clone(),
                            endpoint_str: endpoint_str.to_string(),
                            session_hash: pin_key.as_ref().map(|key| key.session_hash.clone()),
                            pin_outcome: pin_ticket
                                .as_ref()
                                .map(|ticket| ticket.outcome.as_str().to_string()),
                            failovers,
                            error_code: Some("upstream_error".to_string()),
                            billing_coverage_mode,
                            plan_multiplier_bps,
                            service_account_id: caller.service_account_id.map(|sa| sa.to_string()),
                            request_headers_json: request_headers_json.clone(),
                        };
                        let event = usage.event(
                            now,
                            status.as_u16(),
                            is_streaming,
                            false,
                            "completed",
                            &llmroller_proto::json_usage::ExtractedUsage::default(),
                            0,
                            CostSource::PriceBook,
                            None,
                            None,
                            Some(now_ms() - attempt_started),
                        );
                        writer
                            .queues
                            .usage_writer
                            .enqueue(event, std::mem::take(&mut attempts));
                    }

                    // Non-retryable status (for example 405 or a redirect): pass it
                    // through without health effects or failover.
                    let metric_class = if status.is_client_error() {
                        "4xx"
                    } else {
                        "5xx"
                    };
                    crate::metrics::record_request_metric(endpoint_str, metric_class, "error");
                    return upstream_error_response(
                        status,
                        &upstream_headers,
                        error_body,
                        &candidate,
                        &request_id.to_string(),
                        is_admin,
                    );
                }
            }

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
            crate::metrics::record_failover();
            continue;
        }

        if is_streaming {
            if status.is_success() {
                let upstream_headers = response.headers().clone();
                let content_encoding = upstream_content_encoding(&upstream_headers);
                let mut buffer = PreFirstFrameBuffer::new(status, upstream_headers.clone());
                let mut upstream = decode_upstream_stream(response, content_encoding);
                let mut tap = SseUsageTap::new(endpoint);
                let mut first_chunk: Option<Bytes> = None;
                let mut failure = false;
                let mut frame_error_payload: Option<String> = None;

                let frame_timeout =
                    Duration::from_millis(candidate.config.first_valid_frame_timeout_ms());
                loop {
                    let next = tokio::time::timeout(frame_timeout, upstream.next()).await;
                    match next {
                        Ok(Some(Ok(chunk))) => {
                            tap.feed_chunk(&chunk);
                            match buffer.push_chunk(&chunk) {
                                Ok(Some(bytes)) => {
                                    first_chunk = Some(bytes);
                                    break;
                                }
                                Ok(None) => {}
                                Err(crate::forward::ForwardError::UpstreamError(err_msg)) => {
                                    frame_error_payload = Some(err_msg);
                                    failure = true;
                                    break;
                                }
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
                    if let Some(ref err_msg) = frame_error_payload {
                        let frame_class =
                            llmroller_proto::adapters::classify_error_with_classifiers(
                                200,
                                err_msg.as_bytes(),
                                Some(&candidate.config.error_classifiers),
                            );
                        match frame_class {
                            ErrorClass::RequestRejected => {
                                let (charged_micro_usd, charge_basis) = failure_charge(
                                    llmroller_core::usage::AttemptOutcome::from_http_status(400),
                                    input_only_estimate,
                                );
                                attempts.push(attempt_record(
                                    request_id,
                                    attempt_no as i64,
                                    now,
                                    candidate.upstream_key_id,
                                    candidate_share_id.clone(),
                                    llmroller_core::usage::AttemptOutcome::from_http_status(400),
                                    Some(400),
                                    body.len(),
                                    charged_micro_usd,
                                    charge_basis,
                                ));
                                crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                                let rej_status = if err_msg.contains("flagged")
                                    || err_msg.contains("moderation")
                                {
                                    StatusCode::FORBIDDEN
                                } else {
                                    StatusCode::BAD_REQUEST
                                };
                                return protocol_error(
                                    is_messages,
                                    rej_status,
                                    "invalid_request_error",
                                    err_msg.clone(),
                                    Some(&request_id_str),
                                    None,
                                );
                            }
                            ErrorClass::PlanRestricted => {
                                let until = now.saturating_add(PROTOCOL_BLOCK_MS);
                                state.runtime_state.block_route_protocol(
                                    candidate.upstream_key_id,
                                    candidate.model_pk,
                                    protocol,
                                    until,
                                );
                                enqueue_route_protocol_block(
                                    &state,
                                    candidate.upstream_key_id,
                                    candidate.model_pk,
                                    protocol,
                                    "plan_restricted",
                                    until,
                                );
                                try_outcomes.push(TryOutcome::PlanRestricted);
                            }
                            ErrorClass::QuotaExhausted => {
                                state.runtime_state.record_quota_exhausted(
                                    candidate.upstream_key_id,
                                    None,
                                    now,
                                );
                                enqueue_key_health(
                                    state.writer.as_ref(),
                                    &state.runtime_state,
                                    candidate.upstream_key_id,
                                    now,
                                );
                                try_outcomes.push(TryOutcome::QuotaExhausted {
                                    retry_after_secs: None,
                                });
                            }
                            ErrorClass::CredentialInvalid => {
                                let strike = state.runtime_state.record_upstream_failure(
                                    &origin,
                                    candidate.upstream_key_id,
                                    caller.id,
                                    llmroller_core::usage::AttemptOutcome::Http401,
                                    None,
                                    now,
                                );
                                enqueue_key_health(
                                    state.writer.as_ref(),
                                    &state.runtime_state,
                                    candidate.upstream_key_id,
                                    now,
                                );
                                if strike == Some(StrikeResult::ProbeRequired) {
                                    spawn_credential_probe(
                                        &state,
                                        &candidate.base_url,
                                        candidate.allow_private_network,
                                        &secret,
                                        candidate.upstream_key_id,
                                    );
                                }
                                try_outcomes.push(TryOutcome::HostFailure);
                            }
                            _ => {
                                let _ = state.runtime_state.record_upstream_failure(
                                    &origin,
                                    candidate.upstream_key_id,
                                    caller.id,
                                    llmroller_core::usage::AttemptOutcome::ErrorFrame,
                                    None,
                                    now,
                                );
                                enqueue_key_health(
                                    state.writer.as_ref(),
                                    &state.runtime_state,
                                    candidate.upstream_key_id,
                                    now,
                                );
                                try_outcomes.push(TryOutcome::HostFailure);
                            }
                        }
                    } else {
                        let _ = state.runtime_state.record_upstream_failure(
                            &origin,
                            candidate.upstream_key_id,
                            caller.id,
                            llmroller_core::usage::AttemptOutcome::ErrorFrame,
                            None,
                            now,
                        );
                        enqueue_key_health(
                            state.writer.as_ref(),
                            &state.runtime_state,
                            candidate.upstream_key_id,
                            now,
                        );
                        try_outcomes.push(TryOutcome::HostFailure);
                    }

                    let (charged_micro_usd, charge_basis) = failure_charge(
                        llmroller_core::usage::AttemptOutcome::ErrorFrame,
                        input_only_estimate,
                    );
                    if charged_micro_usd > 0
                        && let Some(res) = reservation.take()
                    {
                        res.commit(charged_micro_usd, now);
                    }
                    let att = attempt_record(
                        request_id,
                        attempt_no as i64,
                        now,
                        candidate.upstream_key_id,
                        candidate_share_id.clone(),
                        llmroller_core::usage::AttemptOutcome::ErrorFrame,
                        Some(status.as_u16()),
                        body.len(),
                        charged_micro_usd,
                        charge_basis,
                    );
                    attempts.push(att);
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
                    crate::metrics::record_failover();
                    continue;
                }

                let ttfb_ms = (now_ms() - attempt_started).max(0);
                crate::metrics::record_upstream_ttfb(
                    endpoint_str,
                    &candidate.provider_kind.to_string(),
                    ttfb_ms as f64 / 1000.0,
                );
                state.runtime_state.record_upstream_success(
                    &origin,
                    candidate.upstream_key_id,
                    Some(candidate.route_id()),
                    Some(ttfb_ms as u64),
                    now,
                );
                enqueue_key_health(
                    state.writer.as_ref(),
                    &state.runtime_state,
                    candidate.upstream_key_id,
                    now,
                );
                if let (Some(pin_key), Some(ticket)) = (&pin_key, &pin_ticket)
                    && let Some(entry) = state
                        .pin_store
                        .first_valid_frame(pin_key, ticket, now, pin_ttl_ms)
                {
                    enqueue_pin_upsert(&state, pin_key, candidate.model_pk, &entry);
                }
                drop(permit.take());

                let response_link = if matches!(endpoint, EndpointKind::Responses) {
                    first_chunk
                        .as_ref()
                        .and_then(|bytes| sse_response_id(bytes))
                        .map(|response_id| {
                            let session_hash = pin_key
                                .as_ref()
                                .map_or_else(String::new, |key| key.session_hash.clone());
                            enqueue_response_link(
                                &state,
                                caller.owner_user_id,
                                caller.id,
                                model.id,
                                candidate.upstream_key_id,
                                &session_hash,
                                &response_id,
                                "in_progress",
                                store_flag,
                                now,
                            );
                            ResponseLinkFinalizer {
                                response_id,
                                owner_user_id: caller.owner_user_id,
                                client_key_id: caller.id,
                                upstream_key_id: candidate.upstream_key_id,
                                model_pk: model.id,
                                session_hash,
                                store_flag,
                                created_at: now,
                            }
                        })
                } else {
                    None
                };

                let (billing_coverage_mode, plan_multiplier_bps) = snapshot
                    .providers
                    .get(&candidate.provider_id)
                    .map(|p| (p.billing_coverage_mode.clone(), p.plan_multiplier_bps))
                    .unwrap_or_else(|| ("pay_as_you_go".to_string(), 10_000));
                let usage = UsageContext {
                    request_id,
                    client_key_id: caller.id,
                    owner_user_id: caller.owner_user_id,
                    canonical_model: model.canonical_id.clone(),
                    upstream_key_id: Some(candidate.upstream_key_id.to_string()),
                    provider_id: Some(candidate.provider_id.to_string()),
                    provider_scope: Some(provider_scope_label(&candidate).to_string()),
                    share_id: candidate.share_id.map(|share| share.to_string()),
                    upstream_model_id: candidate.upstream_model_id.clone(),
                    endpoint_str: endpoint_str.to_string(),
                    session_hash: pin_key.as_ref().map(|key| key.session_hash.clone()),
                    pin_outcome: pin_ticket
                        .as_ref()
                        .map(|ticket| ticket.outcome.as_str().to_string()),
                    failovers,
                    error_code: None,
                    billing_coverage_mode,
                    plan_multiplier_bps,
                    service_account_id: caller.service_account_id.map(|sa| sa.to_string()),
                    request_headers_json: request_headers_json.clone(),
                };
                let trace = trace_request.clone().map(|request_body| {
                    TraceCapture::new(request_body, trace_retention_days, first_chunk.as_ref())
                });
                let finalizer = StreamFinalizer {
                    state: state.clone(),
                    usage,
                    attempt_key_id: candidate.upstream_key_id,
                    http_status: status.as_u16(),
                    tap,
                    thread_id: trace_thread_id.clone(),
                    reservation,
                    client_guard: client_guard.take(),
                    cap_guards,
                    price_entry: snapshot
                        .price_for(
                            &candidate.provider_id,
                            candidate.provider_kind,
                            &candidate.upstream_model_id,
                        )
                        .cloned(),
                    fallback_rates: Rates::from(&snapshot.settings.unpriced_fallback_rates),
                    attempts: std::mem::take(&mut attempts),
                    response_link,
                    trace,
                    estimated_input_tokens: input_tokens_est,
                    started_ms: attempt_started,
                    ttfb_ms: Some(ttfb_ms),
                    finalized: false,
                };

                let lifecycle = RequestLifecycle::new(request_id, attempt_started, None);
                let mut stream_state = StreamState {
                    upstream,
                    pending: VecDeque::new(),
                    forwarder: SseChunkForwarder::new(strip_injected_usage),
                    finalizer: Some(finalizer),
                    lifecycle,
                    idle_timeout: Duration::from_millis(candidate.config.stream_idle_timeout_ms()),
                    shutdown_cut: state.shutdown_cut.clone(),
                    shutdown_notify: state.shutdown_notify.clone(),
                    _stream_guard: ActiveStreamGuard::acquire(state.open_streams.clone()),
                    _in_flight: in_flight.take().expect("in-flight guard"),
                    done: false,
                };
                // Feed the pre-commit chunk through the SAME forwarder that owns the
                // stream state. A temporary forwarder would discard an incomplete
                // trailing frame, corrupting the first client-visible SSE event (F015).
                if let Some(bytes) = first_chunk {
                    for part in stream_state.forwarder.process_chunk(&bytes) {
                        stream_state.pending.push_back(part);
                    }
                }
                let metered = stream::unfold(stream_state, |mut stream_state| async move {
                    loop {
                        if let Some(bytes) = stream_state.pending.pop_front() {
                            stream_state.lifecycle.record_first_frame(now_ms());
                            return Some((Ok::<Bytes, std::io::Error>(bytes), stream_state));
                        }
                        if stream_state.done {
                            return None;
                        }

                        let step = {
                            let next = stream_state.upstream.next();
                            tokio::pin!(next);
                            let idle = tokio::time::sleep(stream_state.idle_timeout);
                            tokio::pin!(idle);

                            if let Some(notify) = stream_state.shutdown_notify.clone() {
                                let notified = notify.notified();
                                tokio::pin!(notified);
                                // Register before re-checking the flag so a cut that lands
                                // between the check and the select is never lost.
                                let _ = notified.as_mut().enable();
                                if stream_state.shutdown_cut.load(Ordering::Acquire) {
                                    StreamStep::Shutdown
                                } else {
                                    tokio::select! {
                                        biased;
                                        _ = &mut notified => StreamStep::Shutdown,
                                        item = &mut next => StreamStep::Item(item),
                                        _ = &mut idle => StreamStep::Idle,
                                    }
                                }
                            } else if stream_state.shutdown_cut.load(Ordering::Acquire) {
                                StreamStep::Shutdown
                            } else {
                                tokio::select! {
                                    biased;
                                    item = &mut next => StreamStep::Item(item),
                                    _ = &mut idle => StreamStep::Idle,
                                }
                            }
                        };

                        match step {
                            StreamStep::Shutdown => {
                                stream_state.finish(Termination::ShutdownCut);
                                return None;
                            }
                            StreamStep::Idle => {
                                stream_state.finish(Termination::IdleTimeout);
                                return None;
                            }
                            StreamStep::Item(Some(Ok(chunk))) => {
                                if let Some(finalizer) = &mut stream_state.finalizer {
                                    finalizer.tap.feed_chunk(&chunk);
                                    if let Some(trace) = &mut finalizer.trace {
                                        trace.tap.feed_sse_chunk(&chunk);
                                    }
                                }
                                let parts = stream_state.forwarder.process_chunk(&chunk);
                                stream_state.pending.extend(parts);
                            }
                            StreamStep::Item(Some(Err(_))) => {
                                stream_state.finish(Termination::UpstreamTruncated);
                                return None;
                            }
                            StreamStep::Item(None) => {
                                if let Some(tail) = stream_state.forwarder.finish() {
                                    stream_state.pending.push_back(tail);
                                }
                                let clean_eof = stream_state
                                    .finalizer
                                    .as_ref()
                                    .is_none_or(|f| f.tap.is_complete_at_eof());
                                stream_state.finish(if clean_eof {
                                    Termination::Completed
                                } else {
                                    Termination::UpstreamTruncated
                                });
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
                    is_admin,
                    &request_id.to_string(),
                    &candidate.config.response_header_allow,
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
                is_admin,
                &request_id.to_string(),
                &candidate.config.response_header_allow,
            );
            let content_encoding = upstream_content_encoding(response.headers());
            let bytes = response.bytes().await.unwrap_or_default();
            let bytes =
                decode_upstream_body(content_encoding.as_deref(), bytes).unwrap_or_default();
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
            is_admin,
            &request_id.to_string(),
            &candidate.config.response_header_allow,
        );
        let status_value = response.status();
        let content_type = response.headers().get(CONTENT_TYPE).cloned();
        let content_encoding = upstream_content_encoding(response.headers());
        let nonstream_timeout =
            Duration::from_millis(candidate.config.nonstream_response_timeout_ms());
        let body_read = tokio::time::timeout(nonstream_timeout, response.bytes()).await;
        let body_bytes = match body_read {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) | Err(_) => {
                let is_timeout = body_read.is_err();
                let attempt_outcome = if is_timeout {
                    llmroller_core::usage::AttemptOutcome::WaitTimeout
                } else {
                    llmroller_core::usage::AttemptOutcome::ConnectError
                };
                let try_outcome = if is_timeout {
                    TryOutcome::WaitTimeout
                } else {
                    TryOutcome::HostFailure
                };
                let (charged_micro_usd, charge_basis) =
                    failure_charge(attempt_outcome, input_only_estimate);
                if charged_micro_usd > 0
                    && let Some(res) = reservation.take()
                {
                    res.commit(charged_micro_usd, now);
                }
                let att = attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    attempt_outcome,
                    Some(502),
                    body.len(),
                    charged_micro_usd,
                    charge_basis,
                );
                attempts.push(att);
                failovers += 1;
                crate::metrics::record_failover();
                try_outcomes.push(try_outcome);
                continue;
            }
        };
        let body_bytes = match decode_upstream_body(content_encoding.as_deref(), body_bytes) {
            Ok(bytes) => bytes,
            Err(_) => {
                attempts.push(attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    llmroller_core::usage::AttemptOutcome::ErrorFrame,
                    Some(status_value.as_u16()),
                    body.len(),
                    0,
                    "none",
                ));
                failovers += 1;
                crate::metrics::record_failover();
                try_outcomes.push(TryOutcome::HostFailure);
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

        let json_value: serde_json::Value =
            serde_json::from_slice(&body_bytes).unwrap_or_else(|_| serde_json::json!({}));
        // RTE-8: Upstream errors embedded inside HTTP 200 JSON bodies trigger classification and failover.
        if json_value
            .get("error")
            .is_some_and(|e| !e.is_null() && (e.is_object() || e.is_string()))
        {
            let classify_prefix = &body_bytes[..body_bytes.len().min(ERROR_CLASSIFY_BYTES)];
            let class = classify_error_with_classifiers(
                200,
                classify_prefix,
                Some(&candidate.config.error_classifiers),
            );
            if matches!(
                class,
                ErrorClass::QuotaExhausted
                    | ErrorClass::PlanRestricted
                    | ErrorClass::CredentialInvalid
            ) {
                if class == ErrorClass::QuotaExhausted {
                    state.runtime_state.record_quota_exhausted(
                        candidate.upstream_key_id,
                        None,
                        now,
                    );
                }
                let att = attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    llmroller_core::usage::AttemptOutcome::ErrorFrame,
                    Some(status_value.as_u16()),
                    body.len(),
                    input_only_estimate,
                    "estimated_input",
                );
                attempts.push(att);
                failovers += 1;
                crate::metrics::record_failover();
                try_outcomes.push(TryOutcome::HostFailure);
                continue;
            }
        }
        if matches!(endpoint, EndpointKind::Responses)
            && let Some(response_id) = json_response_id(&json_value)
        {
            enqueue_response_link(
                &state,
                caller.owner_user_id,
                caller.id,
                model.id,
                candidate.upstream_key_id,
                pin_key.as_ref().map_or("", |key| key.session_hash.as_str()),
                response_id,
                "completed",
                store_flag,
                now,
            );
        }
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
            attempt_started,
        );
        let effective_cost = llmroller_core::pricing::apply_plan_multiplier(
            cost.cost_micro_usd,
            cand_mult_bps,
            cand_cov_mode,
        );

        if let Some(reservation) = reservation {
            reservation.commit(effective_cost, now);
        }
        if let Some(guard) = client_guard.take() {
            guard.commit(effective_cost);
        }
        cap_guards.commit(effective_cost);

        let attempt_latency = (now - attempt_started).max(0) as u64;
        state.runtime_state.record_upstream_success(
            &origin,
            candidate.upstream_key_id,
            Some(candidate.route_id()),
            Some(attempt_latency),
            now,
        );
        enqueue_key_health(
            state.writer.as_ref(),
            &state.runtime_state,
            candidate.upstream_key_id,
            now,
        );
        if let (Some(pin_key), Some(ticket)) = (&pin_key, &pin_ticket)
            && let Some(entry) = state
                .pin_store
                .first_valid_frame(pin_key, ticket, now, pin_ttl_ms)
        {
            enqueue_pin_upsert(&state, pin_key, candidate.model_pk, &entry);
        }
        drop(permit.take());

        let served_att = attempt_record(
            request_id,
            attempt_no as i64,
            now,
            candidate.upstream_key_id,
            candidate_share_id.clone(),
            llmroller_core::usage::AttemptOutcome::Served,
            Some(status_value.as_u16()),
            body.len(),
            effective_cost,
            "actual",
        );
        attempts.push(served_att);

        let trace_output = trace_request.take().map(|request_body| {
            let (mut tap, _receiver) = TraceTap::new();
            tap.feed_json_response(&json_value);
            (request_body, tap)
        });

        if let Some(writer) = &state.writer {
            let (billing_coverage_mode, plan_multiplier_bps) = snapshot
                .providers
                .get(&candidate.provider_id)
                .map(|p| (p.billing_coverage_mode.clone(), p.plan_multiplier_bps))
                .unwrap_or_else(|| ("pay_as_you_go".to_string(), 10_000));
            let usage = UsageContext {
                request_id,
                client_key_id: caller.id,
                owner_user_id: caller.owner_user_id,
                canonical_model: model.canonical_id.clone(),
                upstream_key_id: Some(candidate.upstream_key_id.to_string()),
                provider_id: Some(candidate.provider_id.to_string()),
                provider_scope: Some(provider_scope_label(&candidate).to_string()),
                share_id: candidate.share_id.map(|share| share.to_string()),
                upstream_model_id: candidate.upstream_model_id.clone(),
                endpoint_str: endpoint_str.to_string(),
                session_hash: pin_key.as_ref().map(|key| key.session_hash.clone()),
                pin_outcome: pin_ticket
                    .as_ref()
                    .map(|ticket| ticket.outcome.as_str().to_string()),
                failovers,
                error_code: None,
                billing_coverage_mode,
                plan_multiplier_bps,
                service_account_id: caller.service_account_id.map(|sa| sa.to_string()),
                request_headers_json: request_headers_json.clone(),
            };
            let mut event = usage.event(
                now,
                status_value.as_u16(),
                false,
                false,
                "completed",
                &extracted,
                cost.cost_micro_usd,
                cost.cost_source,
                llmroller_proto::json_usage::extract_json_stop_reason(endpoint, &json_value),
                None,
                Some(now_ms() - attempt_started),
            );
            if let Some((request_body, tap)) = trace_output {
                let raw_size = serde_json::to_vec(&json_value).map_or(usize::MAX, |raw| raw.len());
                let response_raw = (raw_size
                    <= llmroller_proto::trace_tap::MAX_RESPONSE_OUTPUT_BYTES)
                    .then(|| json_value.clone());
                let accepted = prepare_trace_request(&request_body, trace_retention_days, now)
                    .is_some_and(|prepared| {
                        submit_trace_job(
                            &state,
                            TraceJobMeta {
                                usage_event_id: &event.id,
                                request_id,
                                ts: now,
                                owner_user_id: caller.owner_user_id,
                                client_key_id: caller.id,
                                session_hash: pin_key
                                    .as_ref()
                                    .map_or("", |key| key.session_hash.as_str()),
                                thread_id: &trace_thread_id,
                                canonical_id: &model.canonical_id,
                                protocol: endpoint_str,
                                response_raw,
                            },
                            prepared,
                            &tap.output,
                            tap.trace_dropped.load(std::sync::atomic::Ordering::Acquire),
                        )
                    });
                event.trace_dropped = !accepted;
            }
            writer.queues.durable_state.lock().insert(
                llmroller_store::writer::DurableKey::ClientKeyTouch(caller.id.to_string()),
                llmroller_store::writer::DurableOp::ClientKeyTouchLastUsed {
                    client_key_id: caller.id.to_string(),
                    last_used_at: now,
                },
            );
            writer
                .queues
                .usage_writer
                .enqueue(event, std::mem::take(&mut attempts));
        }

        crate::metrics::record_request_metric(endpoint_str, "2xx", "completed");
        crate::metrics::record_tokens(
            endpoint_str,
            "2xx",
            "completed",
            &model.canonical_id,
            "input",
            extracted.input_uncached_tokens.max(0) as u64,
        );
        crate::metrics::record_tokens(
            endpoint_str,
            "2xx",
            "completed",
            &model.canonical_id,
            "output",
            extracted.output_tokens.max(0) as u64,
        );
        if extracted.cache_read_tokens > 0 {
            crate::metrics::record_tokens(
                endpoint_str,
                "2xx",
                "completed",
                &model.canonical_id,
                "cache_read",
                extracted.cache_read_tokens as u64,
            );
        }
        if cost.cost_micro_usd > 0 {
            crate::metrics::record_cost(
                endpoint_str,
                "2xx",
                "completed",
                &model.canonical_id,
                cost.cost_micro_usd as u64,
            );
        }
        let latency_duration = (now_ms() - attempt_started).max(0);
        crate::metrics::record_upstream_ttfb(
            endpoint_str,
            &candidate.provider_kind.to_string(),
            latency_duration as f64 / 1000.0,
        );
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

    if let Some(error) = last_cap_error.take() {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return gate_error_response(&error, is_messages);
    }

    let (terminal_status, terminal_code, terminal_retry_after, terminal_body) =
        if aux_unsupported_seen {
            (StatusCode::NOT_FOUND, "not_found_error", None, None)
        } else {
            select_terminal_error(&try_outcomes)
        };
    let status_class = if terminal_status.is_client_error() {
        "4xx"
    } else {
        "5xx"
    };
    crate::metrics::record_request_metric(endpoint_str, status_class, "error");
    if let Some(writer) = &state.writer {
        let usage = UsageContext {
            request_id,
            client_key_id: caller.id,
            owner_user_id: caller.owner_user_id,
            canonical_model: model.canonical_id.clone(),
            upstream_key_id: None,
            provider_id: None,
            provider_scope: Some("none".to_string()),
            share_id: None,
            upstream_model_id: model.canonical_id.clone(),
            endpoint_str: endpoint_str.to_string(),
            session_hash: None,
            pin_outcome: None,
            failovers,
            error_code: Some(terminal_code.to_string()),
            billing_coverage_mode: "pay_as_you_go".to_string(),
            plan_multiplier_bps: 10_000,
            service_account_id: caller.service_account_id.map(|sa| sa.to_string()),
            request_headers_json: request_headers_json.clone(),
        };
        let event = usage.event(
            now,
            terminal_status.as_u16(),
            is_streaming,
            true,
            Termination::UpstreamTruncated.as_str(),
            &llmroller_proto::json_usage::ExtractedUsage::default(),
            0,
            CostSource::EstimatedIncomplete,
            None,
            None,
            Some(now_ms() - now),
        );
        writer
            .queues
            .usage_writer
            .enqueue(event, std::mem::take(&mut attempts));
    }

    let mut response = terminal_body.map_or_else(
        || {
            protocol_error(
                is_messages,
                terminal_status,
                terminal_code,
                "all upstream attempts failed",
                Some(&request_id_str),
                terminal_retry_after,
            )
        },
        |body| {
            (
                terminal_status,
                [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                body,
            )
                .into_response()
        },
    );
    if let Some(secs) = terminal_retry_after {
        if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
            response.headers_mut().insert("retry-after", value);
        }
    } else if let Some(delay) = last_retry_after_ms
        && let Ok(value) = HeaderValue::from_str(&(delay / 1000).max(1).to_string())
    {
        response.headers_mut().insert("retry-after", value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmroller_core::accumulators::MonthlyAccumulatorRegistry;
    use llmroller_core::rbac::Role;
    use llmroller_core::shares::ShareConfig;
    use llmroller_core::snapshot::UserRecord;

    #[test]
    #[doc = "Spec: LIM-1 — the pre-flight reservation honors the caller's output-token ceiling"]
    fn requested_max_output_tokens_reads_every_protocol_spelling() {
        let responses = br#"{"model":"m","input":"hi","max_output_tokens":4096}"#;
        assert_eq!(requested_max_output_tokens(responses), Some(4096));
        let chat = br#"{"model":"m","messages":[],"max_completion_tokens":2048}"#;
        assert_eq!(requested_max_output_tokens(chat), Some(2048));
        let messages = br#"{"model":"m","messages":[],"max_tokens":512}"#;
        assert_eq!(requested_max_output_tokens(messages), Some(512));
        // Precedence: the Responses ceiling wins when several are present.
        let both = br#"{"max_tokens":1,"max_output_tokens":9}"#;
        assert_eq!(requested_max_output_tokens(both), Some(9));
        // Absent, zero, negative, or non-JSON bodies fall back to the 8192 default.
        assert_eq!(requested_max_output_tokens(br#"{"messages":[]}"#), None);
        assert_eq!(requested_max_output_tokens(br#"{"max_tokens":0}"#), None);
        assert_eq!(requested_max_output_tokens(br#"{"max_tokens":-5}"#), None);
        assert_eq!(requested_max_output_tokens(b"not json"), None);
    }

    #[test]
    #[doc = "Spec: REL-1 — F02 open_streams tracks live SSE bodies"]
    fn active_stream_guard_tracks_body_lifetime() {
        let counter = Arc::new(AtomicUsize::new(0));
        let first = ActiveStreamGuard::acquire(counter.clone());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        {
            let _second = ActiveStreamGuard::acquire(counter.clone());
            assert_eq!(counter.load(Ordering::SeqCst), 2);
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        drop(first);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    #[doc = "Spec: TRC-4 — F09 tags subagent traces with their thread id"]
    fn subagent_thread_id_prefers_claude_agent_header() {
        let mut headers = HeaderMap::new();
        assert_eq!(extract_trace_thread_id(&headers), "main");

        headers.insert("x-subagent-id", HeaderValue::from_static("subagent-7"));
        assert_eq!(extract_trace_thread_id(&headers), "subagent-7");

        headers.insert(
            "x-claude-code-agent-id",
            HeaderValue::from_static("claude-agent-2"),
        );
        assert_eq!(extract_trace_thread_id(&headers), "claude-agent-2");

        headers.insert("x-claude-code-agent-id", HeaderValue::from_static("  "));
        assert_eq!(extract_trace_thread_id(&headers), "subagent-7");
    }

    fn tier_candidate(tier: CandidatePoolTier, share_id: Option<ShareId>) -> RouteCandidate {
        RouteCandidate {
            upstream_key_id: UpstreamKeyId::generate(),
            provider_id: ProviderId::generate(),
            model_pk: ModelPk::generate(),
            upstream_model_id: "model".to_string(),
            base_url: "https://example.test".to_string(),
            allow_private_network: false,
            config: llmroller_core::provider::ProviderConfig::default(),
            pool_tier: tier,
            ownership_scope: llmroller_core::rbac::OwnershipScope::Global,
            priority: 1,
            provider_kind: ProviderKind::OpencodeGo,
            share_id,
        }
    }

    fn tier_snapshot(
        user_id: UserId,
        share_id: ShareId,
        user_cap: Option<i64>,
        share_cap: Option<i64>,
    ) -> ConfigSnapshot {
        ConfigSnapshot {
            users: Arc::new(HashMap::from([(
                user_id,
                UserRecord {
                    id: user_id,
                    username: "caller".to_string(),
                    role: Role::User,
                    disabled: false,
                    can_use_global_providers: true,
                    global_budget_micro_usd_month: user_cap,
                },
            )])),
            shares_by_grantee: Arc::new(HashMap::from([(
                user_id,
                vec![ShareConfig {
                    share_id,
                    grantor_user_id: UserId::generate(),
                    grantee_user_id: user_id,
                    provider_id: ProviderId::generate(),
                    upstream_key_id: None,
                    allowed_models: None,
                    max_monthly_micro_usd: share_cap,
                    accepted_config_fingerprint: None,
                    admin_approved_at: None,
                    enabled: true,
                }],
            )])),
            ..ConfigSnapshot::default()
        }
    }

    #[test]
    #[doc = "Spec: USR-5 — F05 own-tier candidates never charge the user-global cap"]
    fn own_tier_reserves_no_user_global_cap() {
        let user_id = UserId::generate();
        let share_id = ShareId::generate();
        let snapshot = tier_snapshot(user_id, share_id, Some(0), Some(0));
        let registry = MonthlyAccumulatorRegistry::new();
        let candidate = tier_candidate(CandidatePoolTier::Own, None);

        let guards =
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000)
                .expect("own tier must ignore the exhausted global cap");
        drop(guards);
        assert_eq!(registry.usage_for(&BudgetScopeId::User(user_id), 1_000), 0);
    }

    #[test]
    #[doc = "Spec: USR-5 — F05 global-tier candidates reserve the user-global cap"]
    fn global_tier_reserves_user_global_cap() {
        let user_id = UserId::generate();
        let share_id = ShareId::generate();
        let snapshot = tier_snapshot(user_id, share_id, Some(0), Some(1_000_000));
        let registry = MonthlyAccumulatorRegistry::new();
        let candidate = tier_candidate(CandidatePoolTier::Global, None);

        assert!(matches!(
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000),
            Err(GateG7Error::UserGlobalBudgetExhausted { .. })
        ));

        let snapshot = tier_snapshot(user_id, share_id, Some(1_000_000), Some(0));
        let guards =
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000)
                .expect("global tier has headroom");
        drop(guards);
        assert!(
            !registry.is_exhausted(&BudgetScopeId::User(user_id), Some(1_000_000), 1_000),
            "a dropped reservation releases its headroom"
        );
    }

    #[test]
    #[doc = "Spec: SHR-1 — F05 shared-tier candidates reserve the grantor share cap"]
    fn shared_tier_reserves_only_the_share_cap() {
        let user_id = UserId::generate();
        let share_id = ShareId::generate();
        let snapshot = tier_snapshot(user_id, share_id, Some(1_000_000), Some(0));
        let registry = MonthlyAccumulatorRegistry::new();
        let candidate = tier_candidate(CandidatePoolTier::Shared, Some(share_id));

        assert!(matches!(
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000),
            Err(GateG7Error::ProviderShareCapExhausted { .. })
        ));

        let snapshot = tier_snapshot(user_id, share_id, Some(0), Some(1_000_000));
        let guards =
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000)
                .expect("share cap has headroom");
        drop(guards);
        assert!(
            registry.usage_for(&BudgetScopeId::ProviderShare(share_id), 1_000) == 0,
            "a dropped share reservation must not commit spend"
        );
    }

    fn session_test_key(mode: &str) -> ClientKeyRecord {
        ClientKeyRecord {
            id: ClientKeyId::generate(),
            owner_user_id: UserId::generate(),
            label: "session-test".to_string(),
            prefix: "llr_test".to_string(),
            key_hash: [0; 32],
            prev_key_hash: None,
            prev_key_expires_at: None,
            routing_scope: "inherit".to_string(),
            allowed_models: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: mode.to_string(),
            service_account_id: None,
            disabled: false,
            paused_at: None,
            revoked_at: None,
            expires_at: None,
        }
    }

    fn session_body(prompt_cache_key: Option<&str>, opening: &str) -> Vec<u8> {
        let mut body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "stable system prompt"},
                {"role": "user", "content": opening}
            ]
        });
        if let Some(key) = prompt_cache_key {
            body["prompt_cache_key"] = serde_json::Value::String(key.to_string());
        }
        serde_json::to_vec(&body).expect("serialize body")
    }

    fn test_session_pin(
        key: &ClientKeyRecord,
        body: &[u8],
        session_hmac_secret: Option<&[u8]>,
        runtime: &RuntimeState,
        now_ms: i64,
    ) -> Option<PinKey> {
        session_pin_key(
            &HeaderMap::new(),
            body,
            &SessionPinContext {
                client_key: key,
                canonical_id: "gpt-4o",
                protocol: "chat",
                session_hmac_secret,
                runtime_state: runtime,
                now_ms,
            },
        )
    }

    fn test_resolved_session(
        key: &ClientKeyRecord,
        body: &[u8],
        runtime: &RuntimeState,
    ) -> Option<ResolvedSession> {
        resolve_session(
            &HeaderMap::new(),
            body,
            &SessionPinContext {
                client_key: key,
                canonical_id: "gpt-4o",
                protocol: "chat",
                session_hmac_secret: Some(b"instance-session-secret-32-bytes"),
                runtime_state: runtime,
                now_ms: 1_000,
            },
        )
    }

    #[test]
    #[doc = "Spec: RTE-1, SEC-16 - forced Step 8 affinity is salted even on Own tier"]
    fn fingerprint_generic_ids_uses_salted_affinity_not_the_raw_client_id() {
        let runtime = RuntimeState::new();
        let key = session_test_key("fingerprint_generic_ids");

        let resolved = test_resolved_session(
            &key,
            &session_body(Some("generic-cache-key"), "opening one"),
            &runtime,
        )
        .expect("generic session must resolve");

        assert!(
            resolved.pin_key.session_hash.starts_with("fp:"),
            "fingerprint_generic_ids must switch the pin to Step 8"
        );
        assert!(
            resolved.raw_session.is_empty(),
            "a Step 8 affinity must not forward the raw generic client ID"
        );
        assert!(
            resolved.salted_token.starts_with("llr_"),
            "Step 8 must always write the tenant-salted token"
        );
        assert_eq!(
            resolved.outbound_token(PoolTier::Own),
            resolved.salted_token
        );
    }

    #[test]
    #[doc = "Spec: RTE-1 - F06 body fields and Step 8 fingerprint drive session pins"]
    fn session_pin_key_uses_body_fields_and_step8_fingerprint() {
        let runtime = RuntimeState::new();
        let key = session_test_key("flag");

        let first = test_session_pin(
            &key,
            &session_body(Some("cache-key"), "opening one"),
            None,
            &runtime,
            1_000,
        )
        .expect("body prompt_cache_key must produce a pin");
        let second = test_session_pin(
            &key,
            &session_body(Some("cache-key"), "opening two"),
            None,
            &runtime,
            2_000,
        )
        .expect("body prompt_cache_key must produce a pin");
        assert_eq!(first.session_hash, second.session_hash);
        assert_eq!(
            first.session_hash,
            fallback_session_hash(key.id, "cache-key"),
            "Step 7 keeps the tenant-scoped raw session hash"
        );

        let step8_first = test_session_pin(
            &key,
            &session_body(None, "same first question"),
            None,
            &runtime,
            3_000,
        )
        .expect("Step 8 must produce a fingerprint pin");
        let step8_second = test_session_pin(
            &key,
            &session_body(None, "same first question"),
            None,
            &runtime,
            4_000,
        )
        .expect("Step 8 must produce a fingerprint pin");
        assert!(step8_first.session_hash.starts_with("fp:"));
        assert_eq!(step8_first.session_hash, step8_second.session_hash);
        assert_ne!(step8_first.session_hash, first.session_hash);

        let hmac_pin = test_session_pin(
            &key,
            &session_body(Some("cache-key"), "opening one"),
            Some(b"instance-session-secret-32-bytes"),
            &runtime,
            5_000,
        )
        .expect("HMAC session hash must produce a pin");
        assert_eq!(hmac_pin.session_hash.len(), 32);
        assert_ne!(hmac_pin.session_hash, first.session_hash);
    }

    #[test]
    #[doc = "Spec: RTE-1 - F06 constant detector observes generic session sources"]
    fn session_pin_key_records_constant_session_anomaly() {
        let runtime = RuntimeState::new();
        let key = session_test_key("flag");

        for minute in 0..21 {
            let body = serde_json::json!({
                "model": "gpt-4o",
                "session_id": "static-session",
                "messages": [
                    {"role": "system", "content": format!("opening system {minute}")},
                    {"role": "user", "content": format!("opening user {minute}")}
                ]
            });
            let pin = test_session_pin(
                &key,
                &serde_json::to_vec(&body).expect("serialize body"),
                None,
                &runtime,
                1_000_000 + minute * 60_000,
            )
            .expect("generic session ID must produce a pin");
            assert_eq!(
                pin.session_hash,
                fallback_session_hash(key.id, "static-session")
            );
        }

        assert!(
            runtime
                .constant_session_detector
                .is_flagged(key.id, &fallback_session_hash(key.id, "static-session")),
            "21 distinct openings within an hour must flag the static session ID"
        );
    }

    #[tokio::test]
    #[doc = "Spec: API-1, API-2, API-8, RTE-7, RTE-8, RTE-9, RTE-12, RTE-13, RTE-14, REL-3, REL-5, USG-1, USG-2, USG-3, USG-9, USG-10, LIM-1, SHR-2, SHR-6, CLK-3, CLK-7, MDL-1, MDL-4, AUD-1, OBS-1, PRV-3, SEC-9, TRN-3"]
    async fn error_envelope_and_failure_charge_and_origin_key_conformance() {
        use http_body_util::BodyExt;
        let msg_resp = build_protocol_error_response(
            true,
            StatusCode::NOT_FOUND,
            "not_found_error",
            "model_not_found",
            "Unknown model",
            Some("req_1"),
            None,
        );
        assert_eq!(msg_resp.status(), StatusCode::NOT_FOUND);
        let msg_body = msg_resp
            .into_body()
            .collect()
            .await
            .expect("collect msg response body")
            .to_bytes();
        let msg_json: serde_json::Value =
            serde_json::from_slice(&msg_body).expect("parse msg error json");
        assert_eq!(msg_json["type"], "error");
        assert_eq!(msg_json["error"]["type"], "not_found_error");
        assert_eq!(msg_json["error"]["code"], "model_not_found");
        assert_eq!(msg_json["error"]["message"], "Unknown model");
        assert_eq!(msg_json["request_id"], "req_1");

        let oai_resp = build_protocol_error_response(
            false,
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "no_eligible_route",
            "No route",
            Some("req_2"),
            Some(5),
        );
        assert_eq!(oai_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            oai_resp
                .headers()
                .get("retry-after")
                .expect("retry-after header")
                .to_str()
                .expect("ASCII retry-after value"),
            "5"
        );
        let oai_body = oai_resp
            .into_body()
            .collect()
            .await
            .expect("collect oai response body")
            .to_bytes();
        let oai_json: serde_json::Value =
            serde_json::from_slice(&oai_body).expect("parse oai error json");
        assert_eq!(oai_json["error"]["type"], "server_error");
        assert_eq!(oai_json["error"]["code"], "no_eligible_route");
        assert_eq!(oai_json["error"]["message"], "No route");
        assert!(oai_json["error"]["param"].is_null());

        assert_eq!(
            failure_charge(llmroller_core::usage::AttemptOutcome::WaitTimeout, 1200),
            (1200, "estimated_input")
        );
        assert_eq!(
            failure_charge(llmroller_core::usage::AttemptOutcome::Http429, 1200),
            (0, "none")
        );

        let origin = OriginKey::from_base_url("http://127.0.0.1:18080/v1", true);
        assert_eq!(origin.host, "127.0.0.1");
        assert_eq!(origin.port, 18080);

        let mut tap = SseUsageTap::new(EndpointKind::Chat);
        tap.feed_chunk(
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\n",
        );
        assert!(!tap.is_complete_at_eof());
        tap.feed_chunk(b"data: [DONE]\n\n");
        assert!(tap.is_complete_at_eof());
    }
}
