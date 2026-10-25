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
                return Err(protocol_error(
                    is_messages,
                    StatusCode::BAD_REQUEST,
                    "invalid_encoding",
                    format!("failed to decompress request body: {e}"),
                    Some(&request_id_str),
                    None,
                ));
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
            return Err(protocol_error(
                is_messages,
                error.status_code(),
                "gateway_busy",
                error.to_string(),
                Some(&request_id_str),
                Some(error.retry_after_secs()),
            ));
        }
    };

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
    let now = now_ms();

    let snapshot = state.snapshot.load();
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
    // Response header policy R4 depends on the caller role, not the request path (SEC-16).
    let is_admin = snapshot
        .users
        .get(&caller.owner_user_id)
        .is_some_and(|user| user.role == Role::Admin);

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

    let scope = resolve_caller_scope(&caller, &snapshot);
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
