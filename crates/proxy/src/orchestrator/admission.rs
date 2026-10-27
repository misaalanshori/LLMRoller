use super::*;

#[allow(dead_code)]
pub(crate) struct AdmittedRequest {
    pub(crate) state: ProxyAuthState,
    pub(crate) endpoint: EndpointKind,
    pub(crate) endpoint_str: &'static str,
    pub(crate) protocol: &'static str,
    pub(crate) aux_label: Option<&'static str>,
    pub(crate) is_messages: bool,
    pub(crate) request_id: RequestId,
    pub(crate) request_id_str: String,
    pub(crate) request_headers_json: Option<String>,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    pub(crate) permit: OwnedSemaphorePermit,
    pub(crate) scan: llmroller_proto::body_edit::TopLevelScan,
    pub(crate) model: SnapshotModel,
    pub(crate) caller: ClientKeyRecord,
    pub(crate) is_admin: bool,
    pub(crate) scope: RoutingScope,
    pub(crate) snapshot: Arc<ConfigSnapshot>,
    pub(crate) selected_tier: CandidatePoolTier,
    pub(crate) tier_candidates: Vec<RouteCandidate>,
    pub(crate) eligible_keys: HashSet<UpstreamKeyId>,
    pub(crate) resolved_session: Option<ResolvedSession>,
    pub(crate) hard_link: Option<ResponseLinkRow>,
    pub(crate) pin_ticket: Option<PinTicket>,
    pub(crate) ordered: Vec<RouteCandidate>,
    pub(crate) strategy: RoutingStrategy,
    pub(crate) client_guard: Option<MonthReservationGuard>,
    pub(crate) now: i64,
    pub(crate) is_streaming: bool,
    pub(crate) requested_output: Option<i64>,
    pub(crate) pin_key: Option<PinKey>,
    pub(crate) trace_thread_id: String,
    pub(crate) trace_request: Option<Bytes>,
    pub(crate) trace_retention_days: i64,
    pub(crate) store_flag: bool,
    pub(crate) pin_ttl_ms: i64,
}

/// Failure modes of the reserve-then-decompress sequence (`SEC-8`, `PERF-3`).
#[derive(Debug)]
enum ReserveDecompressError {
    /// The preflight budget could not accommodate the reservation.
    Busy(GatewayBusyError),
    /// The body claimed an encoding it does not actually use.
    InvalidEncoding(crate::forward::ForwardError),
}

/// What a reservation actually covers, so the caller can reconcile it down.
///
/// The preflight permit plus the body it covers. The permit is held for the request's
/// lifetime and released by its `Drop` on every error path (`SEC-8`, `ISSUE-0147`).
#[derive(Debug)]
struct ReservedBody {
    permit: OwnedSemaphorePermit,
    body: Bytes,
}

impl ReservedBody {
    /// Shrink the reservation from the worst case down to `actual` bytes.
    ///
    /// Releasing the surplus is what keeps the common case — a compressed body that
    /// inflates to well under the ceiling — from permanently occupying a 16 MiB slot
    /// for the whole request. Consumes and returns the permit; the body is left to the
    /// caller so nothing is moved unnecessarily.
    ///
    /// If the split cannot be expressed (`actual > reserved`, a caller bug), the
    /// original permit is returned unchanged rather than failing the request: over-
    /// reserving is the safe direction.
    fn shrink_permit(
        mut permit: OwnedSemaphorePermit,
        reserved: usize,
        actual: usize,
    ) -> OwnedSemaphorePermit {
        let units = |bytes: usize| bytes.div_ceil(4096);
        let reserved_units = units(reserved);
        let actual_units = units(actual).min(reserved_units);
        if actual_units == reserved_units {
            return permit;
        }
        match permit.split(reserved_units.saturating_sub(actual_units)) {
            // `split` moved the surplus tokens into the returned permit, which is
            // dropped here, releasing them back to the semaphore.
            Some(surplus) => {
                drop(surplus);
                permit
            }
            None => permit,
        }
    }
}

/// Acquire the preflight reservation for a compressed body, then decompress it.
///
/// The reservation is denominated in the **decompressed** ceiling, not the wire size:
/// a 1 KB gzip body may legitimately inflate to `MAX_DECOMPRESSED_BYTES`, so reserving
/// the wire size would let `N` concurrent requests allocate `N × 16 MiB` while the
/// budget still read free (`SEC-8`, `PERF-3`, `ISSUE-0147`).
///
/// The reservation is reconciled down to the actual decompressed size before return,
/// and released by the permit's `Drop` on every error path.
async fn reserve_and_decompress_compressed(
    semaphore: &PreflightBudgetSemaphore,
    encoding: &str,
    wire_body: Bytes,
) -> Result<ReservedBody, ReserveDecompressError> {
    // Worst case first: the ceiling the decoder is allowed to reach.
    let ceiling = crate::forward::MAX_DECOMPRESSED_BYTES;
    let reservation = wire_body.len().max(ceiling);
    let permit = semaphore
        .acquire(reservation, Duration::from_millis(2000))
        .await
        .map_err(ReserveDecompressError::Busy)?;

    match crate::forward::decompress_chunk(encoding, &wire_body) {
        Ok(decompressed) => {
            let body = Bytes::from(decompressed);
            let actual = body.len();
            let permit = ReservedBody::shrink_permit(permit, reservation, actual);
            Ok(ReservedBody { permit, body })
        }
        // `permit` is dropped here, releasing the worst-case reservation.
        Err(error) => Err(ReserveDecompressError::InvalidEncoding(error)),
    }
}

/// Acquire the preflight reservation for an uncompressed body and return it unchanged.
///
/// Reserves only the wire length: charging the 16 MiB decompression ceiling to a body
/// that is not compressed would cut throughput on the common path by the ratio of the
/// ceiling to the typical body size.
async fn reserve_and_decompress_uncompressed(
    semaphore: &PreflightBudgetSemaphore,
    body: Bytes,
) -> Result<ReservedBody, ReserveDecompressError> {
    let reserved = body.len();
    let permit = semaphore
        .acquire(reserved, Duration::from_millis(2000))
        .await
        .map_err(ReserveDecompressError::Busy)?;
    Ok(ReservedBody { permit, body })
}

/// Pre-candidate gate sequence shared by inference and the auxiliary endpoints
/// (`API-1`, `SEC-8`, `F040`, `ISSUE-0145`).
///
/// Everything here runs **before** candidate selection, so `admit_inbound_request`
/// and `post_messages_count_tokens` can share one copy rather than each maintaining
/// its own list. The defect this replaces was two hand-maintained gate lists, one of
/// which drifted: `count_tokens` had no preflight reservation, no decompression, and
/// no client-key TPM accounting.
///
/// Deliberately **not** included: `is_ip_rate_limited`, the RPM limiter, and
/// diagnostic-header extraction. Those live in `client_key_auth_middleware`, which
/// both endpoints already pass through; re-implementing them here would restart the
/// same drift in a new place.
///
/// Also not included: candidate selection, pins, budget-window reservations, usage
/// events, and tracing. `CountTokens` is auxiliary, so C8 budget windows are skipped
/// for it (SRDD 10.1) — it needs the gates, not the pipeline.
pub(crate) struct GatedInbound {
    /// Body after decompression.
    pub(crate) body: Bytes,
    /// Preflight memory-admission permit for the buffered body. Held by the caller
    /// for as long as the buffered body is live; `count_tokens` drops it immediately
    /// because its continuation is a cheap auxiliary call.
    pub(crate) permit: Option<OwnedSemaphorePermit>,
    pub(crate) scan: llmroller_proto::body_edit::TopLevelScan,
    pub(crate) model: SnapshotModel,
    pub(crate) scope: RoutingScope,
    pub(crate) now: i64,
    /// Estimated input tokens, from the same `estimate_input_tokens` the TPM gate used.
    pub(crate) input_tokens_est: i64,
    /// Echoed back so the continuation does not have to re-derive it.
    pub(crate) is_streaming: bool,
    pub(crate) store_flag: bool,
}

/// Run the shared pre-candidate gates.
///
/// The per-request facts the gates need that are not derived from the body itself.
pub(crate) struct GateRequest<'a> {
    pub(crate) endpoint: EndpointKind,
    pub(crate) endpoint_str: &'static str,
    pub(crate) is_messages: bool,
    pub(crate) headers: &'a HeaderMap,
    pub(crate) request_id_str: String,
    pub(crate) body: Bytes,
}

/// On success returns everything the continuation needs. On any rejection returns a
/// fully-formed protocol error response, so both callers propagate it verbatim.
///
/// The preflight permit is returned to the caller inside [`GatedInbound`] rather than
/// dropped here, because the buffered body it accounts for is returned too. Each
/// caller decides how long to hold it: inference keeps it for the request, while
/// `count_tokens` drops it immediately after its own candidate loop has copied the
/// body it needs.
#[allow(clippy::result_large_err)]
pub(crate) async fn gate_inbound_request(
    state: &ProxyAuthState,
    caller: &ClientKeyRecord,
    request: GateRequest<'_>,
) -> Result<GatedInbound, Response> {
    let GateRequest {
        endpoint,
        endpoint_str,
        is_messages,
        headers,
        request_id_str,
        body,
    } = request;
    let encoding = headers
        .get(axum::http::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();
    // 1. Reserve preflight memory, then decompress inside that reservation
    //    (`SEC-8`, `PERF-3`, `ISSUE-0147`).
    //
    //    The reservation for a compressed body is denominated in the *decompressed*
    //    ceiling, not the wire size: a 1 KB gzip body may legitimately inflate to
    //    `MAX_DECOMPRESSED_BYTES`, so reserving the wire size would let N concurrent
    //    requests each allocate up to the ceiling while the budget still read free.
    //    Uncompressed bodies reserve only their own length. The permit is held for the
    //    request's lifetime and released by its `Drop` on every error path.
    let is_compressed = !encoding.is_empty() && encoding != "identity";
    let reserved = if is_compressed {
        match reserve_and_decompress_compressed(&state.budget_semaphore, encoding, body).await {
            Ok(reserved) => reserved,
            Err(ReserveDecompressError::InvalidEncoding(error)) => {
                crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                return Err(protocol_error(
                    is_messages,
                    StatusCode::BAD_REQUEST,
                    "invalid_encoding",
                    format!("failed to decompress request body: {error}"),
                    Some(&request_id_str),
                    None,
                ));
            }
            Err(ReserveDecompressError::Busy(error)) => {
                crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
                return Err(protocol_error(
                    is_messages,
                    error.status_code(),
                    "gateway_busy",
                    error.to_string(),
                    Some(&request_id_str),
                    Some(error.retry_after_secs()),
                ));
            }
        }
    } else {
        match reserve_and_decompress_uncompressed(&state.budget_semaphore, body).await {
            Ok(reserved) => reserved,
            Err(ReserveDecompressError::Busy(error)) => {
                crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
                return Err(protocol_error(
                    is_messages,
                    error.status_code(),
                    "gateway_busy",
                    error.to_string(),
                    Some(&request_id_str),
                    Some(error.retry_after_secs()),
                ));
            }
            // The uncompressed path never produces an encoding error.
            Err(ReserveDecompressError::InvalidEncoding(error)) => {
                crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                return Err(protocol_error(
                    is_messages,
                    StatusCode::BAD_REQUEST,
                    "invalid_encoding",
                    format!("failed to decompress request body: {error}"),
                    Some(&request_id_str),
                    None,
                ));
            }
        }
    };
    let permit = Some(reserved.permit);
    let body = reserved.body;

    // 2. Scan and validate body envelope and duplicate keys
    let scan = match scan_top_level(&body) {
        Ok(scan) => scan,
        Err(error) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return Err(protocol_error(
                is_messages,
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                error.to_string(),
                Some(&request_id_str),
                None,
            ));
        }
    };

    let Some((ref model_str, _)) = scan.model else {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return Err(protocol_error(
            is_messages,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model is required in request body",
            Some(&request_id_str),
            None,
        ));
    };

    // F004 & API-1: POST /v1/responses/compact is strictly non-streaming; reject stream: true with HTTP 400.
    if matches!(
        endpoint,
        EndpointKind::ResponsesCompact | EndpointKind::Embeddings
    ) && scan.stream.as_ref().is_some_and(|(stream, _)| *stream)
    {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return Err(protocol_error(
            is_messages,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("stream is not supported on /v1/{endpoint_str}"),
            Some(&request_id_str),
            None,
        ));
    }
    let is_streaming = scan.stream.as_ref().is_some_and(|(stream, _)| *stream);
    let store_flag = response_store_flag(&scan, &body);
    let now = state.clock.now_ms();

    let snapshot = state.snapshot.load();
    // `snapshot` is a guard borrowed from `state`; everything below reads through it,
    // and `GatedInbound` carries only owned values.
    // 3. Canonical model resolution is a pure in-memory lookup (PERF-1).
    let model = match snapshot.resolve_model(model_str) {
        Ok(model) => model.clone(),
        Err(llmroller_core::snapshot::SnapshotResolveError::NotFound) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return Err(protocol_error(
                is_messages,
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("model {model_str} not found in catalog"),
                Some(&request_id_str),
                None,
            ));
        }
        Err(llmroller_core::snapshot::SnapshotResolveError::Ambiguous) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return Err(protocol_error(
                is_messages,
                StatusCode::CONFLICT,
                "ambiguous_model_id",
                format!("model {model_str} matches more than one catalog entry"),
                Some(&request_id_str),
                None,
            ));
        }
        Err(llmroller_core::snapshot::SnapshotResolveError::KillSwitched) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return Err(protocol_error(
                is_messages,
                StatusCode::FORBIDDEN,
                "model_disabled",
                format!("model {model_str} is disabled"),
                Some(&request_id_str),
                None,
            ));
        }
        Err(llmroller_core::snapshot::SnapshotResolveError::RedirectLoop) => {
            crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
            return Err(protocol_error(
                is_messages,
                StatusCode::INTERNAL_SERVER_ERROR,
                "model_redirect_loop",
                "catalog redirect chain contains a cycle",
                Some(&request_id_str),
                None,
            ));
        }
    };

    // 4. Gate G6: client key model allow-list (supporting wildcards and canonical IDs; F043).
    if let Some(ref allowed) = caller.allowed_models
        && !llmroller_core::client_key::is_model_allowed_by_patterns(
            allowed,
            model_str,
            &model.canonical_id,
        )
    {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return Err(protocol_error(
            is_messages,
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("model {} not permitted on this key", model.canonical_id),
            Some(&request_id_str),
            None,
        ));
    }

    // F040: Client key TPM sliding-window rate limit enforcement at proxy ingress
    let input_tokens_est = estimate_input_tokens(&body);
    if let Err(retry_secs) = state.runtime_state.try_consume_client_key_tpm(
        caller.id,
        input_tokens_est,
        caller.rate_limit_tpm,
        now,
    ) {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return Err(protocol_error(
            is_messages,
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "client key token rate limit exceeded",
            Some(&request_id_str),
            Some(retry_secs),
        ));
    }

    let scope = resolve_caller_scope(caller, &snapshot);
    Ok(GatedInbound {
        body,
        permit,
        scan,
        model,
        scope,
        now,
        input_tokens_est,
        is_streaming,
        store_flag,
    })
}

#[allow(clippy::result_large_err)]
pub(crate) async fn admit_inbound_request(
    state: ProxyAuthState,
    endpoint: EndpointKind,
    headers: HeaderMap,
    body: Bytes,
    client_key: Option<ClientKeyRecord>,
) -> Result<AdmittedRequest, Response> {
    let endpoint_str = match endpoint {
        EndpointKind::Chat => "chat",
        EndpointKind::Responses => "responses",
        EndpointKind::ResponsesCompact => "responses_compact",
        EndpointKind::Messages => "messages",
        EndpointKind::Embeddings => "embeddings",
        _ => "chat",
    };
    let protocol = match endpoint {
        // The compact endpoint speaks the Responses wire protocol but has its own upstream path.
        EndpointKind::ResponsesCompact => "responses",
        EndpointKind::Embeddings => "embeddings",
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

    // 1-4. Shared pre-candidate gates: preflight memory admission, decompression,
    // envelope scan, model resolution, G6 allow-list, client-key TPM and scope
    // resolution. One copy, shared with the auxiliary endpoints
    // (`API-1`, `SEC-8`, `F040`, `ISSUE-0145`).
    let Some(caller) = client_key.clone() else {
        return Err(protocol_error(
            is_messages,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "authentication required",
            Some(&request_id_str),
            None,
        ));
    };
    let gated = gate_inbound_request(
        &state,
        &caller,
        GateRequest {
            endpoint,
            endpoint_str,
            is_messages,
            headers: &headers,
            request_id_str: request_id_str.clone(),
            body,
        },
    )
    .await?;
    let GatedInbound {
        body,
        permit,
        scan,
        model,
        scope,
        now,
        input_tokens_est,
        is_streaming,
        store_flag,
    } = gated;
    // `gate_inbound_request` always returns a permit; the struct makes it optional only
    // so `count_tokens` can drop its own without a second type.
    let permit = permit
        .unwrap_or_else(|| unreachable!("gate_inbound_request always reserves a preflight permit"));
    let snapshot = state.snapshot.load();
    // Response header policy R4 depends on the caller role, not the request path (SEC-16).
    let is_admin = snapshot
        .users
        .get(&caller.owner_user_id)
        .is_some_and(|user| user.role == Role::Admin);
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
    let trace_request = if trace_enabled {
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
                    return Err(response_not_found(previous_id));
                }
                Ok(Some(link)) if now <= link.expires_at => Some(link),
                Ok(_) => None,
                Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
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
                    return Err(permission_error(
                        "the upstream key that holds previous_response_id is no longer available to this client key",
                    ));
                };
                let tier = candidate.pool_tier;
                (tier, vec![candidate])
            }
            HardRouteDecision::RateLimit429 { retry_after_secs } => {
                crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                return Err(rate_limit_error(retry_after_secs));
            }
            HardRouteDecision::Forbidden403KeyUnavailable => {
                state.pin_store.invalidate_key(&link.upstream_key_id);
                return Err(permission_error(
                    "the upstream key that holds previous_response_id is no longer available to this client key",
                ));
            }
            HardRouteDecision::Forbidden403ModelOrProtocolBlocked => {
                return Err(permission_error(
                    "the model or protocol that holds previous_response_id is no longer available",
                ));
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
                return Err(aux_endpoint_unsupported_error(is_messages));
            }
            return Err(protocol_error(
                is_messages,
                StatusCode::SERVICE_UNAVAILABLE,
                "no_eligible_route",
                "no eligible upstream route available for this caller",
                Some(&request_id_str),
                Some(1),
            ));
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
                    return Err(protocol_error(
                        is_messages,
                        StatusCode::TOO_MANY_REQUESTS,
                        "client_key_budget_exhausted",
                        "monthly budget capacity exhausted",
                        Some(&request_id_str),
                        Some(60),
                    ));
                }
                Err(PoolSelectionError::NoEligibleRoute) => {
                    // F096 & F099: If no native same-protocol route exists, check whether any
                    // candidate route supports a transcodable upstream protocol (chat/responses/messages)
                    // without having an explicit protocol block on the inbound protocol.
                    let transcoded_selection = if aux_label.is_none()
                        && matches!(protocol, "chat" | "responses" | "messages")
                    {
                        let mut transcodable = eligibility.clone();
                        for r in &mut transcodable {
                            if !r.protocols.iter().any(|p| p == protocol)
                                && !r.protocol_blocked
                                && r.protocols.iter().any(|p| {
                                    matches!(p.as_str(), "chat" | "responses" | "messages")
                                        && !state.runtime_state.is_route_protocol_blocked(
                                            r.upstream_key_id,
                                            r.model_pk,
                                            p,
                                            now,
                                        )
                                })
                            {
                                r.protocols.push(protocol.to_string());
                            }
                        }
                        select_pool(&transcodable, &context, shares, scope).ok()
                    } else {
                        None
                    };
                    if let Some(selection) = transcoded_selection {
                        selection
                    } else {
                        crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
                        return Err(protocol_error(
                            is_messages,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "no_eligible_route",
                            "no eligible upstream route available for this caller",
                            Some(&request_id_str),
                            Some(1),
                        ));
                    }
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
        return Err(protocol_error(
            is_messages,
            StatusCode::SERVICE_UNAVAILABLE,
            "no_eligible_route",
            "no eligible upstream route available for this caller",
            Some(&request_id_str),
            Some(1),
        ));
    }

    let eligible_keys: HashSet<UpstreamKeyId> = tier_candidates
        .iter()
        .map(|candidate| candidate.upstream_key_id)
        .collect();

    // Stage 1: Gate G7 monthly caps are reserved before any upstream connection.
    let requested_output = requested_max_output_tokens(&body);
    let first_cache_ratio = state
        .runtime_state
        .route_cache_ratio(tier_candidates[0].route_id());
    let estimate = estimate_cost_micro_usd(
        &snapshot,
        &state.runtime_state,
        &tier_candidates[0],
        input_tokens_est,
        requested_output,
    )
    .max(estimate_reservation_cost_micro_usd(
        input_tokens_est,
        first_cache_ratio,
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
            // Gate G7 rejected before any upstream call, so the event carries no key or
            // provider attribution and costs nothing (`USG-1`, `ISSUE-0151`). Without
            // this the refusal is invisible to `/usage`: the caller is told their budget
            // is exhausted while the ledger holds no record that they tried.
            enqueue_terminal_usage(
                &state,
                TerminalUsageFacts {
                    request_id,
                    caller: &caller,
                    canonical_model: &model.canonical_id,
                    upstream_key_id: None,
                    provider_id: None,
                    provider_scope: None,
                    share_id: None,
                    upstream_model_id: model.canonical_id.clone(),
                    endpoint_str,
                    session_hash: None,
                    pin_outcome: None,
                    failovers: 0,
                    error_code: Some(
                        match error {
                            llmroller_core::accumulators::GateG7Error::ClientKeyBudgetExhausted { .. } => {
                                "client_key_budget_exhausted"
                            }
                            llmroller_core::accumulators::GateG7Error::UserGlobalBudgetExhausted { .. } => {
                                "user_budget_exhausted"
                            }
                            llmroller_core::accumulators::GateG7Error::ProviderShareCapExhausted { .. } => {
                                "provider_share_budget_exhausted"
                            }
                        }
                        .to_string(),
                    ),
                    billing_coverage_mode: "pay_as_you_go".to_string(),
                    plan_multiplier_bps: 10_000,
                    request_headers_json: request_headers_json.clone(),
                    status: StatusCode::TOO_MANY_REQUESTS,
                    is_streaming: scan.stream.as_ref().is_some_and(|(s, _)| *s),
                    termination: Termination::Completed.as_str(),
                    cost_micro_usd: 0,
                    cost_source: CostSource::EstimatedIncomplete,
                    now,
                    latency_ms: Some(0),
                },
                Vec::new(),
            );
            return Err(gate_error_response(&error, is_messages));
        }
    };
    let client_guard = client_guard;

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
            writer: state.writer.as_ref(),
            audit_hmac_key: state.audit_hmac_key.as_ref().map(|k| k.expose_secret()),
            pool: Some(&state.pool),
            now_ms: now,
        },
    );
    let mut pin_key = resolved_session
        .as_ref()
        .map(|session| session.pin_key.clone());
    let pin_ttl_ms = snapshot.settings.pin_ttl_secs.saturating_mul(1000);
    let clock = state.clock.as_ref();
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
                clock,
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
            clock,
            pin_ttl_ms,
            snapshot.settings.repin_on_personal_recovery,
            snapshot.settings.recovery_repin_min_idle_secs,
            scope,
            Some(selected_tier),
            |key_id| eligible_keys.contains(&key_id),
            |tried| {
                select_next_candidate(
                    &state.runtime_state,
                    &snapshot,
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
            if ticket.outcome == PinOutcome::UnpinnedCapacity {
                crate::metrics::record_pin_capacity_overflow();
            }
            if let Some(candidate) = tier_candidates
                .iter()
                .find(|candidate| candidate.upstream_key_id == ticket.upstream_key_id)
            {
                ordered.push(candidate.clone());
            }
            pin_ticket = Some(ticket);
        }
    }

    let snapshot_arc = Arc::clone(&snapshot);
    Ok(AdmittedRequest {
        state,
        endpoint,
        endpoint_str,
        protocol,
        aux_label,
        is_messages,
        request_id,
        request_id_str,
        request_headers_json,
        headers,
        body,
        permit,
        scan,
        model,
        caller,
        is_admin,
        scope,
        snapshot: snapshot_arc,
        selected_tier,
        tier_candidates,
        eligible_keys,
        resolved_session,
        hard_link,
        pin_ticket,
        ordered,
        strategy,
        client_guard,
        now,
        is_streaming,
        requested_output,
        pin_key,
        trace_thread_id,
        trace_request,
        trace_retention_days,
        store_flag,
        pin_ttl_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write as _;

    /// Compress `payload` with gzip so tests can build a small body that inflates
    /// to a known size.
    fn gzip(payload: &[u8]) -> Bytes {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(payload).expect("compress");
        Bytes::from(encoder.finish().expect("finish"))
    }

    fn semaphore(budget_mb: usize) -> PreflightBudgetSemaphore {
        PreflightBudgetSemaphore::new(budget_mb, 64)
    }

    /// Remaining permits in 4096-byte units, converted back to bytes.
    fn available_bytes(sem: &PreflightBudgetSemaphore) -> usize {
        sem.semaphore.available_permits() * 4096
    }

    /// A compressed body must reserve its decompressed ceiling *before* inflating.
    ///
    /// Sequential form, because "while decompression is in progress" has no
    /// observable hook: a `decompress_chunk` call is synchronous.
    ///
    /// Request A is an uncompressed body that occupies all but one unit of the
    /// budget. Request B's **wire** size fits in the remaining unit, but its
    /// decompressed size does not. If the reservation were denominated in wire
    /// bytes, B would be admitted and then allocate far past the budget; because the
    /// reservation is denominated in the decompressed ceiling, B is refused.
    #[tokio::test]
    #[doc = "Spec: SEC-8, PERF-3 — ISSUE-0147 compressed bodies reserve their decompressed ceiling"]
    async fn compressed_body_reserves_its_decompressed_ceiling_before_inflating() {
        // 1 MiB budget = 256 units of 4096 bytes.
        let sem = semaphore(1);
        let budget_bytes = 1024 * 1024;

        // Hold everything except one unit, so only 4096 bytes remain.
        let hog = sem
            .acquire(budget_bytes - 4096, Duration::from_millis(2000))
            .await
            .expect("the hog must fit the budget");
        assert_eq!(available_bytes(&sem), 4096);

        // B's wire size is tiny and would fit in the remaining 4096 bytes.
        let body = gzip(&vec![0_u8; 200_000]);
        assert!(
            body.len() <= 4096,
            "the fixture must be smaller than the remaining budget to be a fair test; \
             got {} bytes",
            body.len()
        );

        let outcome = reserve_and_decompress_compressed(&sem, "gzip", body).await;
        assert!(
            matches!(outcome, Err(ReserveDecompressError::Busy(_))),
            "a compressed body whose decompressed size exceeds the remaining budget \
             must be refused before inflating, not admitted on its wire size"
        );

        drop(hog);
        assert_eq!(
            available_bytes(&sem),
            budget_bytes,
            "a refused reservation must not leak permits"
        );
    }

    /// Concurrent compressed bodies must not collectively exceed the budget.
    ///
    /// N is derived from the budget: with a budget of `n × decompressed_size / 4`,
    /// at most a small number of requests can hold the worst-case reservation at
    /// once, so the number admitted is bounded by the budget rather than by N.
    #[tokio::test]
    #[doc = "Spec: SEC-8, PERF-3 — ISSUE-0147 concurrent compressed bodies cannot exceed the budget"]
    async fn concurrent_compressed_bodies_cannot_exceed_the_budget() {
        const DECOMPRESSED: usize = 4 * 1024 * 1024;
        // Budget fits exactly one worst-case reservation, with headroom for the
        // wire bytes of the fixtures.
        let budget_mb = 17;
        let sem = Arc::new(semaphore(budget_mb));

        let payload = vec![0_u8; DECOMPRESSED];
        let body = gzip(&payload);
        let concurrency = 8;

        let attempts = async {
            let mut set = tokio::task::JoinSet::new();
            for _ in 0..concurrency {
                let sem = Arc::clone(&sem);
                let body = body.clone();
                set.spawn(
                    async move { reserve_and_decompress_compressed(&sem, "gzip", body).await },
                );
            }
            // Hold every successful reservation: in production the permit lives for
            // the whole request, and releasing it here would let the next wave in.
            let mut held = Vec::new();
            let mut refused = 0;
            while let Some(joined) = set.join_next().await {
                match joined.expect("task must not panic") {
                    Ok(reserved) => held.push(reserved),
                    Err(ReserveDecompressError::Busy(_)) => refused += 1,
                    Err(ReserveDecompressError::InvalidEncoding(_)) => {
                        panic!("the fixture is valid gzip")
                    }
                }
            }
            (held.len(), refused)
        };

        let (admitted, refused) = tokio::time::timeout(Duration::from_secs(60), attempts)
            .await
            .expect(
                "the concurrent admission test must terminate; hitting this bound is a \
                 finding, not a flake",
            );

        assert_eq!(admitted + refused, concurrency);
        assert!(
            admitted <= 1,
            "{admitted} concurrent compressed bodies were admitted against a budget that \
             fits exactly one {DECOMPRESSED}-byte decompression; the reservation is not \
             bounding concurrent decompression"
        );
        assert!(
            refused >= concurrency - 1,
            "the budget must refuse the rest; only {refused} of {} were refused",
            concurrency - 1
        );
    }

    /// A failed decompression must release its reservation.
    #[tokio::test]
    #[doc = "Spec: SEC-8 — ISSUE-0147 reservation is released when decompression fails"]
    async fn reservation_is_released_when_decompression_fails() {
        // The worst-case reservation is the 16 MiB decompression ceiling, so the budget
        // must be able to hold one; this test is about leak detection, not tightness.
        let sem = semaphore(32);
        let budget_bytes = 32 * 1024 * 1024;
        assert_eq!(available_bytes(&sem), budget_bytes);

        // Not valid gzip.
        let body = Bytes::from_static(b"this is definitely not gzip");
        let outcome = reserve_and_decompress_compressed(&sem, "gzip", body).await;
        assert!(
            matches!(outcome, Err(ReserveDecompressError::InvalidEncoding(_))),
            "invalid gzip must surface as an encoding error, got {outcome:?}"
        );

        assert_eq!(
            available_bytes(&sem),
            budget_bytes,
            "the full budget must be available again after a failed decompression; a \
             leaked permit permanently shrinks the budget"
        );
    }

    /// An uncompressed body must reserve only its wire length.
    #[tokio::test]
    #[doc = "Spec: SEC-8, PERF-3 — ISSUE-0147 uncompressed bodies reserve only their wire length"]
    async fn uncompressed_body_reserves_only_its_wire_length() {
        // Budget large enough for many small bodies but far smaller than the
        // 16 MiB ceiling an uncompressed body must not reserve.
        let sem = semaphore(1);
        let budget_bytes = 1024 * 1024;
        let body = Bytes::from(vec![b'x'; 8 * 1024]);

        let admitted = reserve_and_decompress_uncompressed(&sem, body.clone())
            .await
            .expect("an 8 KiB body must fit a 1 MiB budget");
        assert_eq!(admitted.body.as_ref(), body.as_ref());

        let remaining = available_bytes(&sem);
        assert_eq!(
            remaining,
            budget_bytes - 8192,
            "an uncompressed body must reserve exactly its wire length; reserving the \
             16 MiB decompression ceiling would have exhausted this budget"
        );
    }
}

#[cfg(test)]
mod prefix_measurement {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write as _;

    fn gzip(payload: &[u8]) -> Bytes {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(payload).expect("compress");
        Bytes::from(encoder.finish().expect("finish"))
    }

    /// Measures the *old* ordering: decompress first, then acquire on the
    /// decompressed length.
    ///
    /// This exists purely to produce the pre-fix number for the cycle report. It is
    /// the exact sequence the production path used before the fix, so the count it
    /// prints is what an attacker could have achieved. Left as a test rather than a
    /// throwaway so the measurement is reproducible.
    #[tokio::test]
    #[doc = "Spec: PERF-3 — ISSUE-0147 pre-fix concurrent decompression measurement"]
    async fn prefix_ordering_admits_every_concurrent_body() {
        let sem = Arc::new(PreflightBudgetSemaphore::new(17, 64));
        let payload = vec![0_u8; 4 * 1024 * 1024];
        let body = gzip(&payload);
        let concurrency = 16;

        // Pre-fix sequence: decompress each body unconditionally, then try to acquire.
        let attempts = async {
            let mut set = tokio::task::JoinSet::new();
            for _ in 0..concurrency {
                let sem = Arc::clone(&sem);
                let body = body.clone();
                set.spawn(async move {
                    // Decompression happens with no reservation at all.
                    let decompressed =
                        crate::forward::decompress_chunk("gzip", &body).expect("valid gzip");
                    let inflated = decompressed.len();
                    // Only now does the budget get consulted, on the inflated length.
                    sem.acquire(inflated, Duration::from_millis(2000))
                        .await
                        .is_ok()
                });
            }
            let mut admitted = 0;
            while let Some(joined) = set.join_next().await {
                if joined.expect("task must not panic") {
                    admitted += 1;
                }
            }
            admitted
        };

        let admitted = tokio::time::timeout(Duration::from_secs(60), attempts)
            .await
            .expect("the measurement must terminate");

        println!(
            "PRE-FIX MEASUREMENT: {admitted} of {concurrency} concurrent compressed bodies \
             reached the budget check, each having already allocated {} bytes with no \
             reservation",
            4 * 1024 * 1024
        );
        assert_eq!(
            admitted, concurrency,
            "the pre-fix ordering inflates every body before any admission decision"
        );
    }
}
