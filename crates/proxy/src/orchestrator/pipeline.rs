use super::*;

pub async fn handle_inference_request(
    state: ProxyAuthState,
    endpoint: EndpointKind,
    headers: HeaderMap,
    body: Bytes,
    client_key: Option<ClientKeyRecord>,
) -> Response {
    let admitted = match admit_inbound_request(state, endpoint, headers, body, client_key).await {
        Ok(admitted) => admitted,
        Err(err_response) => return err_response,
    };
    run_inference_attempt_loop(admitted).await
}

pub(crate) async fn run_inference_attempt_loop(admitted: AdmittedRequest) -> Response {
    let AdmittedRequest {
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
        scan: _,
        model,
        caller,
        is_admin,
        scope: _,
        snapshot,
        selected_tier,
        tier_candidates,
        eligible_keys,
        resolved_session,
        hard_link,
        mut pin_ticket,
        ordered,
        strategy,
        mut client_guard,
        now,
        is_streaming,
        requested_output,
        pin_key,
        trace_thread_id,
        trace_request,
        trace_retention_days,
        store_flag,
        pin_ttl_ms,
    } = admitted;
    let mut permit = Some(permit);
    // Stage 7: one selector call per attempt. Precomputing the whole ordering
    // would advance cyclic strategies once per candidate and starve later keys.
    let pin_context = PinFailureContext {
        runtime: state.runtime_state.as_ref(),
        snapshot: &snapshot,
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
    let mut attempt_no: usize = 0;

    loop {
        if attempt_no >= MAX_UPSTREAM_ATTEMPTS
            || (attempt_no >= 1 && state.clock.now_ms().saturating_sub(now) >= 60_000)
        {
            break;
        }
        let candidate = if let Some(candidate) = pinned_order.pop() {
            candidate
        } else {
            match select_next_candidate(
                &state.runtime_state,
                &snapshot,
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
        let raw_secret = match unseal_upstream_key(&state, &snapshot, &key) {
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
        let secret = match state
            .token_manager
            .resolve_and_persist_secret(
                key.id,
                key.provider_id,
                &raw_secret,
                state.select_http_client(candidate.allow_private_network),
                candidate.allow_private_network,
                now,
                state.writer.as_ref(),
                state.master_key.as_ref(),
                state.master_key_id.as_deref(),
                state.instance_id.or(Some(snapshot.instance_id)),
                &state.entropy,
            )
            .await
        {
            Ok(secret) => secret,
            Err(_) => {
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
        let cand_cache_ratio = state.runtime_state.route_cache_ratio(candidate.route_id());
        let is_free_tier = candidate.upstream_model_id.ends_with(":free")
            || model.canonical_id.ends_with(":free")
            || cand_cov_mode == "free_tier";
        let attempt_estimate = llmroller_core::pricing::apply_effective_upstream_cost(
            estimate_reservation_cost_micro_usd(
                input_tokens_est,
                cand_cache_ratio,
                &candidate_rates,
                requested_output,
                None,
            ),
            cand_mult_bps,
            cand_cov_mode,
            is_free_tier,
        );
        let input_only_estimate = llmroller_core::pricing::apply_effective_upstream_cost(
            (i128::from(input_tokens_est.max(0))
                * i128::from(candidate_rates.input_per_mtok_micro_usd.max(0))
                + 999_999)
                .div_euclid(1_000_000) as i64,
            cand_mult_bps,
            cand_cov_mode,
            is_free_tier,
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

        // F096 & F099: Determine whether this candidate natively supports `protocol` or requires cross-protocol transcoding.
        let route_protos: Option<Vec<String>> =
            snapshot.routes_by_model.get(&model.id).and_then(|routes| {
                routes
                    .iter()
                    .find(|r| r.upstream_key_id == candidate.upstream_key_id)
                    .map(|r| r.protocols.clone())
            });
        let (attempt_endpoint, transcode_pair) = if aux_label.is_none()
            && matches!(protocol, "chat" | "responses" | "messages")
            && let Some(ref protos) = route_protos
            && !protos.iter().any(|p| p == protocol)
            && let Some(target_p) = protos
                .iter()
                .find(|p| matches!(p.as_str(), "messages" | "responses" | "chat"))
        {
            let ep = match target_p.as_str() {
                "messages" => EndpointKind::Messages,
                "responses" => EndpointKind::Responses,
                _ => EndpointKind::Chat,
            };
            let src_wire = llmroller_proto::transcode::WireProtocol::from_catalog_str(protocol);
            let dst_wire = llmroller_proto::transcode::WireProtocol::from_catalog_str(target_p);
            (ep, src_wire.zip(dst_wire))
        } else {
            (endpoint, None)
        };
        let target_url = endpoint_url(&candidate, attempt_endpoint);
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

        // Body and header preparation for this attempt: session/stream field editing,
        // failover sanitisation, transcoding, preset normalization, compat rewrites and
        // header synthesis — extracted verbatim to `attempt_body.rs`.
        let PreparedAttemptBody {
            edited_body,
            outbound_headers,
            strip_injected_usage,
        } = match prepare_attempt_body(PreservedAttemptFacts {
            body: &body,
            candidate: &candidate,
            endpoint,
            attempt_endpoint,
            is_streaming,
            attempt_no,
            resolved_session: resolved_session.as_ref(),
            headers: &headers,
            secret: &secret,
            now,
            protocol,
            transcode_pair,
            hard_link: hard_link.clone(),
            target_url: &target_url,
        }) {
            Ok(prepared) => prepared,
            Err(response) => return *response,
        };

        let mut request_builder = client.post(&target_url).body(edited_body.clone());
        for (name, value) in &outbound_headers {
            request_builder = request_builder.header(name, value);
        }

        let attempt_started = state.clock.now_ms();
        // F003: Streaming requests use first_activity_timeout_ms (60s); non-streaming requests use nonstream_response_timeout_ms (600s).
        let send_timeout = if is_streaming {
            Duration::from_millis(candidate.config.first_activity_timeout_ms())
        } else {
            Duration::from_millis(candidate.config.nonstream_response_timeout_ms())
        };
        let send_res = tokio::time::timeout(send_timeout, request_builder.send()).await;
        let mut response = match send_res {
            Ok(Ok(response)) => response,
            Ok(Err(_)) | Err(_) => {
                let is_timeout = send_res.is_err();
                if is_timeout {
                    crate::metrics::record_upstream_slow(&candidate.provider_id.to_string());
                }
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
                    Some(if is_timeout { 504 } else { 502 }),
                    body.len(),
                    charged_micro_usd,
                    charge_basis,
                );
                attempts.push(att);
                // F003: If pre-response send() timed out after body dispatch, check timeout_failover
                // to prevent duplicate billing when upstream accepted request.
                if is_timeout && !candidate.config.timeout_failover() {
                    if let Some(guard) = client_guard.take() {
                        guard.commit(charged_micro_usd);
                    }
                    cap_guards.commit(charged_micro_usd);
                    crate::metrics::record_request_metric(endpoint_str, "5xx", "timeout");
                    // The charge above must reach the ledger. Without this the money is
                    // committed to the client-key and cap guards while `usage_events`
                    // holds no row, so `/usage` under-reports and the per-key spend view
                    // diverges permanently from what was charged (`USG-1`, `USG-3`,
                    // `ISSUE-0149`).
                    //
                    // Attribution differs from the terminal all-failed path: this
                    // attempt *did* reach a real upstream key, so the charge is
                    // attributed to that key and provider rather than to `"none"`.
                    let (billing_coverage_mode, plan_multiplier_bps) = snapshot
                        .providers
                        .get(&candidate.provider_id)
                        .map(|p| (p.billing_coverage_mode.clone(), p.plan_multiplier_bps))
                        .unwrap_or_else(|| ("pay_as_you_go".to_string(), 10_000));
                    enqueue_terminal_usage(
                        &state,
                        TerminalUsageFacts {
                            request_id,
                            caller: &caller,
                            canonical_model: &model.canonical_id,
                            upstream_key_id: Some(candidate.upstream_key_id.to_string()),
                            provider_id: Some(candidate.provider_id.to_string()),
                            provider_scope: Some(provider_scope_label(&candidate).to_string()),
                            share_id: candidate.share_id.map(|share| share.to_string()),
                            upstream_model_id: candidate.upstream_model_id.clone(),
                            endpoint_str,
                            session_hash: pin_key.as_ref().map(|key| key.session_hash.clone()),
                            pin_outcome: pin_ticket
                                .as_ref()
                                .map(|ticket| ticket.outcome.as_str().to_string()),
                            failovers,
                            error_code: Some("timeout_error".to_string()),
                            billing_coverage_mode,
                            plan_multiplier_bps,
                            request_headers_json: request_headers_json.clone(),
                            status: StatusCode::GATEWAY_TIMEOUT,
                            is_streaming,
                            termination: Termination::IdleTimeout.as_str(),
                            cost_micro_usd: charged_micro_usd,
                            cost_source: CostSource::EstimatedIncomplete,
                            now,
                            latency_ms: Some(state.clock.now_ms() - attempt_started),
                        },
                        std::mem::take(&mut attempts),
                    );
                    return protocol_error(
                        is_messages,
                        StatusCode::GATEWAY_TIMEOUT,
                        "timeout_error",
                        "upstream timed out waiting for response",
                        Some(&request_id_str),
                        None,
                    );
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
                attempt_no += 1;
                tokio::time::sleep(Duration::from_millis(delay_ms as u64)).await;

                let mut retry_builder = client.post(&target_url).body(edited_body.clone());
                for (name, value) in &outbound_headers {
                    retry_builder = retry_builder.header(name, value);
                }
                if let Ok(Ok(retry_resp)) =
                    tokio::time::timeout(send_timeout, retry_builder.send()).await
                {
                    response = retry_resp;
                    status = response.status();
                }
            }
        }

        if !status.is_success() {
            let upstream_headers = response.headers().clone();
            let err_encoding = upstream_content_encoding(&upstream_headers);
            let raw_error_body = read_bounded_error_body(response).await;
            // F023: Decompress compressed upstream error payloads before classification and passthrough.
            let error_body = decode_upstream_body(err_encoding.as_deref(), raw_error_body.clone())
                .unwrap_or(raw_error_body);
            // F029: Parse rate-limit backoff across Retry-After, retry-after-ms, Go durations, and JSON body.
            last_retry_after_ms = parse_rate_limit_backoff_ms(&upstream_headers, &error_body, now);
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
                    // Host responded cleanly with a client/moderation 4xx: close any HalfOpen
                    // origin breaker probe while keeping key health untouched (REL-5, FND-001).
                    if let Some(breaker) = state.runtime_state.origin_breakers.get(&origin) {
                        breaker.lock().record_success(now);
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
                            Some(state.clock.now_ms() - attempt_started),
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
                ErrorClass::Other(_)
                    if status == StatusCode::TOO_MANY_REQUESTS
                        || llmroller_proto::adapters::is_overloaded_error(
                            status.as_u16(),
                            classify_prefix,
                        ) =>
                {
                    // F018: HTTP 429 and HTTP 529 / overloaded_error apply key rate-limit backoff without tripping OriginBreaker.
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
                ErrorClass::Other(_)
                    if llmroller_proto::adapters::is_origin_timeout_status(status.as_u16()) =>
                {
                    // F018: HTTP 504 / Cloudflare 524 origin timeout maps to WaitTimeout.
                    let outcome = llmroller_core::usage::AttemptOutcome::WaitTimeout;
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
                    try_outcomes.push(TryOutcome::WaitTimeout);
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
                            Some(state.clock.now_ms() - attempt_started),
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

        let exec_ctx = AttemptExecutionContext {
            state: &state,
            snapshot: &snapshot,
            candidate: &candidate,
            endpoint,
            endpoint_str,
            protocol,
            is_messages,
            is_admin,
            request_id,
            request_id_str: &request_id_str,
            now,
            attempt_no,
            attempt_started,
            candidate_share_id: candidate_share_id.clone(),
            body: &body,
            model: &model,
            caller: &caller,
            reservation,
            client_guard: &mut client_guard,
            cap_guards,
            pin_ticket,
            resolved_session: resolved_session.as_ref(),
            permit: &mut permit,
            input_only_estimate,
            pin_key: pin_key.clone(),
            trace_thread_id: &trace_thread_id,
            trace_request: trace_request.as_ref(),
            trace_retention_days,
            store_flag,
            pin_ttl_ms,
            request_headers_json: request_headers_json.clone(),
            origin: &origin,
            secret: &secret,
            input_tokens_est,
            cand_cov_mode,
            cand_mult_bps,
            in_flight: &mut in_flight,
            attempt_endpoint,
            transcode_pair,
            strip_injected_usage,
            pinned_order: &mut pinned_order,
            live_pin_ticket: &mut pin_ticket,
            attempts: &mut attempts,
            try_outcomes: &mut try_outcomes,
            last_retry_after_ms: &mut last_retry_after_ms,
            failovers: &mut failovers,
            pin_context: &pin_context,
            tier_candidates: &tier_candidates,
        };

        if is_streaming {
            if let Some(resp) = handle_streaming_attempt(exec_ctx, response, status).await {
                return resp;
            }
            continue;
        } else {
            if let Some(resp) = handle_non_streaming_attempt(exec_ctx, response).await {
                return resp;
            }
            continue;
        }
    }

    if let Some(error) = last_cap_error.take() {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        // Gate G7 rejected before any upstream call, so this event carries **no** key or
        // provider attribution and costs nothing — the one site in this family where
        // `None`/`None`/`None` is correct (`USG-1`, `ISSUE-0151`). Any attempt rows from
        // earlier candidates are real and are passed through rather than dropped.
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
                failovers,
                error_code: Some(
                    match error {
                        llmroller_core::accumulators::GateG7Error::ClientKeyBudgetExhausted {
                            ..
                        } => "client_key_budget_exhausted",
                        llmroller_core::accumulators::GateG7Error::UserGlobalBudgetExhausted {
                            ..
                        } => "user_budget_exhausted",
                        llmroller_core::accumulators::GateG7Error::ProviderShareCapExhausted {
                            ..
                        } => "provider_share_budget_exhausted",
                    }
                    .to_string(),
                ),
                billing_coverage_mode: "pay_as_you_go".to_string(),
                plan_multiplier_bps: 10_000,
                request_headers_json: request_headers_json.clone(),
                // Every `GateG7Error` renders as 429 (`gate_error_response`).
                status: StatusCode::TOO_MANY_REQUESTS,
                is_streaming,
                termination: Termination::Completed.as_str(),
                cost_micro_usd: 0,
                cost_source: CostSource::EstimatedIncomplete,
                now,
                latency_ms: Some(state.clock.now_ms() - now),
            },
            std::mem::take(&mut attempts),
        );
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
    // Nothing reached a usable upstream key on this path, so the event carries no key
    // or provider attribution and no cost. The charged-timeout abort uses the same
    // emitter with the opposite attribution (see the `timeout_failover == false`
    // branch) — one builder, two call sites.
    enqueue_terminal_usage(
        &state,
        TerminalUsageFacts {
            request_id,
            caller: &caller,
            canonical_model: &model.canonical_id,
            upstream_key_id: None,
            provider_id: None,
            provider_scope: Some("none".to_string()),
            share_id: None,
            upstream_model_id: model.canonical_id.clone(),
            endpoint_str,
            session_hash: None,
            pin_outcome: None,
            failovers,
            error_code: Some(terminal_code.to_string()),
            billing_coverage_mode: "pay_as_you_go".to_string(),
            plan_multiplier_bps: 10_000,
            request_headers_json: request_headers_json.clone(),
            status: terminal_status,
            is_streaming,
            termination: Termination::UpstreamTruncated.as_str(),
            cost_micro_usd: 0,
            cost_source: CostSource::EstimatedIncomplete,
            now,
            latency_ms: Some(state.clock.now_ms() - now),
        },
        std::mem::take(&mut attempts),
    );

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
