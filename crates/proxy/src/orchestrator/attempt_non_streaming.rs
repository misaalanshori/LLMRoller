use super::*;

pub(crate) async fn handle_non_streaming_attempt(
    ctx: AttemptExecutionContext<'_>,
    response: reqwest::Response,
) -> Option<Response> {
    let AttemptExecutionContext {
        state,
        snapshot,
        candidate,
        endpoint,
        endpoint_str,
        protocol,
        is_messages,
        is_admin,
        request_id,
        request_id_str,
        now,
        attempt_no,
        attempt_started,
        candidate_share_id,
        body,
        model,
        caller,
        mut reservation,
        client_guard,
        mut cap_guards,
        pin_ticket,
        resolved_session: _,
        permit,
        input_only_estimate,
        pin_key,
        trace_thread_id,
        trace_request,
        trace_retention_days,
        store_flag,
        pin_ttl_ms,
        request_headers_json,
        origin,
        secret,
        input_tokens_est,
        cand_cov_mode,
        cand_mult_bps,
        in_flight: _,
        attempt_endpoint,
        transcode_pair,
        strip_injected_usage: _,
        pinned_order,
        live_pin_ticket,
        attempts,
        try_outcomes,
        last_retry_after_ms: _,
        failovers,
        pin_context,
        tier_candidates,
    } = ctx;
    let mut trace_request = trace_request.cloned();
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
    let nonstream_timeout = Duration::from_millis(candidate.config.nonstream_response_timeout_ms());
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
            let _ = state.runtime_state.record_upstream_failure(
                origin,
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
            // F003: Once HTTP 200 headers were received on a non-streaming request, a body read
            // timeout MUST NOT fail over by default because the upstream already accepted and
            // billed the prompt; only fail over if `timeout_failover` is explicitly enabled.
            if is_timeout && status_value.is_success() && !candidate.config.timeout_failover() {
                if let Some(guard) = client_guard.take() {
                    guard.commit(charged_micro_usd);
                }
                cap_guards.commit(charged_micro_usd);
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
                        provider_scope: Some(provider_scope_label(candidate).to_string()),
                        share_id: candidate.share_id.map(|share| share.to_string()),
                        upstream_model_id: candidate.upstream_model_id.clone(),
                        endpoint_str: endpoint_str.to_string(),
                        session_hash: pin_key.as_ref().map(|key| key.session_hash.clone()),
                        pin_outcome: pin_ticket
                            .as_ref()
                            .map(|ticket| ticket.outcome.as_str().to_string()),
                        failovers: *failovers,
                        error_code: Some("timeout_error".to_string()),
                        billing_coverage_mode,
                        plan_multiplier_bps,
                        service_account_id: caller
                            .service_account_id
                            .clone()
                            .map(|sa| sa.to_string()),
                        request_headers_json: request_headers_json.clone(),
                    };
                    let est_usage = llmroller_proto::json_usage::ExtractedUsage {
                        input_uncached_tokens: input_tokens_est,
                        ..Default::default()
                    };
                    let event = usage.event(
                        now,
                        StatusCode::GATEWAY_TIMEOUT.as_u16(),
                        false,
                        true,
                        Termination::IdleTimeout.as_str(),
                        &est_usage,
                        charged_micro_usd,
                        CostSource::EstimatedIncomplete,
                        None,
                        None,
                        Some(state.clock.now_ms() - attempt_started),
                    );
                    writer
                        .queues
                        .usage_writer
                        .enqueue(event, std::mem::take(attempts));
                }
                crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
                return Some(protocol_error(
                    is_messages,
                    StatusCode::GATEWAY_TIMEOUT,
                    "timeout_error",
                    "upstream timed out while reading non-streaming response body",
                    Some(request_id_str),
                    None,
                ));
            }
            *failovers += 1;
            crate::metrics::record_failover();
            try_outcomes.push(try_outcome);
            return None;
        }
    };
    let body_bytes = match decode_upstream_body(content_encoding.as_deref(), body_bytes) {
        Ok(bytes) => bytes,
        Err(_) => {
            let _ = state.runtime_state.record_upstream_failure(
                origin,
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
            *failovers += 1;
            crate::metrics::record_failover();
            try_outcomes.push(TryOutcome::HostFailure);
            return None;
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
        return Some(
            builder
                .body(Body::from(body_bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        );
    }

    let json_value: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or_else(|_| serde_json::json!({}));
    // RTE-8 & F002: Upstream errors embedded inside HTTP 200 JSON bodies MUST NEVER promote
    // provisional session pins or return 200 OK; classify and fail over (or reject on 400).
    let is_embedded_error = json_value
        .get("error")
        .or_else(|| json_value.pointer("/response/error"))
        .is_some_and(|e| e.is_object() || e.as_str().is_some_and(|s| !s.trim().is_empty()))
        || json_value.get("type").and_then(|t| t.as_str()) == Some("error")
        || json_value.get("status").and_then(|s| s.as_str()) == Some("failed");
    if is_embedded_error {
        let classify_prefix = &body_bytes[..body_bytes.len().min(ERROR_CLASSIFY_BYTES)];
        let class = classify_error_with_classifiers(
            200,
            classify_prefix,
            Some(&candidate.config.error_classifiers),
        );
        match class {
            ErrorClass::RequestRejected => {
                drop(reservation);
                let att = attempt_record(
                    request_id,
                    attempt_no as i64,
                    now,
                    candidate.upstream_key_id,
                    candidate_share_id.clone(),
                    llmroller_core::usage::AttemptOutcome::from_http_status(400),
                    Some(400),
                    body.len(),
                    0,
                    "none",
                );
                attempts.push(att);
                crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
                return Some(upstream_error_response(
                    StatusCode::BAD_REQUEST,
                    &response_headers,
                    body_bytes,
                    candidate,
                    &request_id.to_string(),
                    is_admin,
                ));
            }
            ErrorClass::Other(404) => {
                let until = now.saturating_add(600_000);
                state.runtime_state.block_route_protocol(
                    candidate.upstream_key_id,
                    candidate.model_pk,
                    protocol,
                    until,
                );
                enqueue_route_protocol_block(
                    state,
                    candidate.upstream_key_id,
                    candidate.model_pk,
                    protocol,
                    "model_not_found",
                    until,
                );
                enqueue_route_health_update(
                    state,
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
            }
            ErrorClass::QuotaExhausted => {
                state
                    .runtime_state
                    .record_quota_exhausted(candidate.upstream_key_id, None, now);
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
            ErrorClass::PlanRestricted => {
                let until = now.saturating_add(PROTOCOL_BLOCK_MS);
                state.runtime_state.block_route_protocol(
                    candidate.upstream_key_id,
                    candidate.model_pk,
                    protocol,
                    until,
                );
                enqueue_route_protocol_block(
                    state,
                    candidate.upstream_key_id,
                    candidate.model_pk,
                    protocol,
                    "plan_restricted",
                    until,
                );
                try_outcomes.push(TryOutcome::PlanRestricted);
            }
            ErrorClass::CredentialInvalid => {
                let strike = state.runtime_state.record_upstream_failure(
                    origin,
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
                        state,
                        &candidate.base_url,
                        candidate.allow_private_network,
                        secret,
                        candidate.upstream_key_id,
                    );
                }
                try_outcomes.push(TryOutcome::HostFailure);
            }
            ErrorClass::Other(429 | 529) => {
                let _ = state.runtime_state.record_upstream_failure(
                    origin,
                    candidate.upstream_key_id,
                    caller.id,
                    llmroller_core::usage::AttemptOutcome::Http429,
                    None,
                    now,
                );
                enqueue_key_health(
                    state.writer.as_ref(),
                    &state.runtime_state,
                    candidate.upstream_key_id,
                    now,
                );
                try_outcomes.push(TryOutcome::RateLimited {
                    retry_after_secs: Some(1),
                });
            }
            _ => {
                let _ = state.runtime_state.record_upstream_failure(
                    origin,
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
        if let Some(res) = reservation.take() {
            res.commit(input_only_estimate, now);
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
        if let Some(replacement) = note_pre_first_byte_failure(
            state,
            pin_key.as_ref(),
            pin_ticket.as_ref(),
            candidate.upstream_key_id,
            pin_context,
        ) && let Some(next) = tier_candidates
            .iter()
            .find(|entry| entry.upstream_key_id == replacement.upstream_key_id)
        {
            pinned_order.push(next.clone());
            *live_pin_ticket = Some(replacement);
        }
        *failovers += 1;
        crate::metrics::record_failover();
        return None;
    }
    if matches!(endpoint, EndpointKind::Responses)
        && let Some(response_id) = json_response_id(&json_value)
    {
        enqueue_response_link(
            state,
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
    let extracted = extract_json_usage(attempt_endpoint, &json_value).unwrap_or_default();
    let body_bytes = if let Some((src_wire, dst_wire)) = transcode_pair {
        llmroller_proto::transcode::transcode_response(
            dst_wire,
            src_wire,
            &body_bytes,
            &model.canonical_id,
        )
        .map(Bytes::from)
        .unwrap_or(body_bytes)
    } else {
        body_bytes
    };
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
    let is_free_tier = candidate.upstream_model_id.ends_with(":free")
        || model.canonical_id.ends_with(":free")
        || cand_cov_mode == "free_tier";
    let effective_cost = llmroller_core::pricing::apply_effective_upstream_cost(
        cost.cost_micro_usd,
        cand_mult_bps,
        cand_cov_mode,
        is_free_tier,
    );

    if let Some(reservation) = reservation {
        reservation.commit(effective_cost, now);
    }
    if let Some(guard) = client_guard.take() {
        guard.commit(effective_cost);
    }
    cap_guards.commit(effective_cost);

    state.runtime_state.record_route_cache_ratio(
        candidate.route_id(),
        extracted.input_uncached_tokens,
        extracted.cache_read_tokens,
        extracted.cache_write_tokens,
    );
    let attempt_latency = (now - attempt_started).max(0) as u64;
    state.runtime_state.record_upstream_success(
        origin,
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
    if let (Some(pin_key), Some(ticket)) = (&pin_key, &pin_ticket) {
        if let Some(entry) = state
            .pin_store
            .first_valid_frame(pin_key, ticket, now, pin_ttl_ms)
        {
            enqueue_pin_upsert(state, pin_key, candidate.model_pk, &entry);
        } else {
            crate::metrics::record_pin_stale_confirm();
        }
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
            provider_scope: Some(provider_scope_label(candidate).to_string()),
            share_id: candidate.share_id.map(|share| share.to_string()),
            upstream_model_id: candidate.upstream_model_id.clone(),
            endpoint_str: endpoint_str.to_string(),
            session_hash: pin_key.as_ref().map(|key| key.session_hash.clone()),
            pin_outcome: pin_ticket
                .as_ref()
                .map(|ticket| ticket.outcome.as_str().to_string()),
            failovers: *failovers,
            error_code: None,
            billing_coverage_mode,
            plan_multiplier_bps,
            service_account_id: caller.service_account_id.clone().map(|sa| sa.to_string()),
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
            Some(state.clock.now_ms() - attempt_started),
        );
        let pending_ns_trace = trace_output.map(|(request_body, tap)| {
            let raw_size = serde_json::to_vec(&json_value).map_or(usize::MAX, |raw| raw.len());
            let response_raw = (raw_size <= llmroller_proto::trace_tap::MAX_RESPONSE_OUTPUT_BYTES)
                .then(|| json_value.clone());
            let prepared = prepare_trace_request(&request_body, trace_retention_days, now);
            (tap, response_raw, prepared)
        });
        if let Some((ref tap, _, ref prepared)) = pending_ns_trace {
            let can_submit = prepared.is_some()
                && state.sealer_pool.is_some()
                && state.master_key_id.is_some()
                && !tap.trace_dropped.load(std::sync::atomic::Ordering::Acquire);
            if !can_submit {
                event.trace_dropped = true;
            }
        }
        let event_id = event.id.clone();
        writer.queues.durable_state.lock().insert(
            llmroller_store::writer::DurableKey::ClientKeyTouch(caller.id.to_string()),
            llmroller_store::writer::DurableOp::ClientKeyTouchLastUsed {
                client_key_id: caller.id.to_string(),
                last_used_at: now,
            },
        );
        // F016: Enqueue UsageEventRecord BEFORE submitting SealJob so usage_events row
        // is always queued ahead of sealed trace persistence.
        writer
            .queues
            .usage_writer
            .enqueue(event, std::mem::take(attempts));
        if let Some((tap, response_raw, prepared_opt)) = pending_ns_trace {
            let accepted = prepared_opt.is_some_and(|prepared| {
                submit_trace_job(
                    state,
                    TraceJobMeta {
                        usage_event_id: &event_id,
                        request_id,
                        ts: now,
                        owner_user_id: caller.owner_user_id,
                        client_key_id: caller.id,
                        session_hash: pin_key.as_ref().map_or("", |key| key.session_hash.as_str()),
                        thread_id: trace_thread_id,
                        canonical_id: &model.canonical_id,
                        protocol: endpoint_str,
                        response_raw,
                    },
                    prepared,
                    &tap.output,
                    tap.trace_dropped.load(std::sync::atomic::Ordering::Acquire),
                )
            });
            if !accepted {
                crate::metrics::record_trace_dropped();
            }
        }
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
    let latency_duration = (state.clock.now_ms() - attempt_started).max(0);
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
    Some(
        builder
            .body(Body::from(body_bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    )
}
