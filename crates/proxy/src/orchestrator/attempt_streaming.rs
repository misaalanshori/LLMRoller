use super::*;

pub(crate) async fn handle_streaming_attempt(
    ctx: AttemptExecutionContext<'_>,
    response: reqwest::Response,
    status: StatusCode,
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
        in_flight,
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
        cand_cov_mode: _,
        cand_mult_bps: _,
        attempt_endpoint: _,
        transcode_pair: _,
        strip_injected_usage,
        pinned_order,
        live_pin_ticket,
        attempts,
        try_outcomes,
        last_retry_after_ms: _,
        failovers,
        pin_context,
        tier_candidates,
    } = ctx;
            if status.is_success() {
                let upstream_headers = response.headers().clone();
                let content_encoding = upstream_content_encoding(&upstream_headers);
                let mut buffer = PreFirstFrameBuffer::new(status, upstream_headers.clone());
                let mut upstream = decode_upstream_stream(response, content_encoding);
                let mut tap = SseUsageTap::new(endpoint);
                let mut first_chunk: Option<Bytes> = None;
                let mut failure = false;
                let mut is_stream_timeout = false;
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
                        Ok(Some(Err(_))) => {
                            failure = true;
                            break;
                        }
                        Err(_) => {
                            failure = true;
                            is_stream_timeout = true;
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
                                return Some(protocol_error(
                                    is_messages,
                                    rej_status,
                                    "invalid_request_error",
                                    err_msg.clone(),
                                    Some(request_id_str),
                                    None,
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
                                    let _ =
                                        tx.send(llmroller_proto::discovery::DiscoveryTarget::Key(
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
                                    llmroller_core::usage::AttemptOutcome::from_http_status(404),
                                    Some(404),
                                    body.len(),
                                    0,
                                    "none",
                                );
                                attempts.push(att);
                                if let Some(replacement) = note_pre_first_byte_failure(
                                    state,
                                    pin_key.as_ref(),
                                    pin_ticket.as_ref(),
                                    candidate.upstream_key_id,
                                    pin_context,
                                ) && let Some(next) = tier_candidates.iter().find(|entry| {
                                    entry.upstream_key_id == replacement.upstream_key_id
                                }) {
                                    pinned_order.push(next.clone());
                                    *live_pin_ticket = Some(replacement);
                                }
                                *failovers += 1;
                                crate::metrics::record_failover();
                                return None;
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
                    } else {
                        let stream_outcome = if is_stream_timeout {
                            llmroller_core::usage::AttemptOutcome::WaitTimeout
                        } else {
                            llmroller_core::usage::AttemptOutcome::ErrorFrame
                        };
                        let _ = state.runtime_state.record_upstream_failure(
                            origin,
                            candidate.upstream_key_id,
                            caller.id,
                            stream_outcome,
                            None,
                            now,
                        );
                        enqueue_key_health(
                            state.writer.as_ref(),
                            &state.runtime_state,
                            candidate.upstream_key_id,
                            now,
                        );
                        try_outcomes.push(if is_stream_timeout {
                            TryOutcome::WaitTimeout
                        } else {
                            TryOutcome::HostFailure
                        });
                    }

                    let stream_outcome = if is_stream_timeout {
                        llmroller_core::usage::AttemptOutcome::WaitTimeout
                    } else {
                        llmroller_core::usage::AttemptOutcome::ErrorFrame
                    };
                    let (charged_micro_usd, charge_basis) =
                        failure_charge(stream_outcome, input_only_estimate);
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
                        stream_outcome,
                        Some(if is_stream_timeout {
                            504
                        } else {
                            status.as_u16()
                        }),
                        body.len(),
                        charged_micro_usd,
                        charge_basis,
                    );
                    attempts.push(att);
                    // F003: If streaming first frame timed out, check timeout_failover to prevent duplicate billing
                    if is_stream_timeout && !candidate.config.timeout_failover() {
                        if let Some(guard) = client_guard.take() {
                            guard.commit(charged_micro_usd);
                        }
                        cap_guards.commit(charged_micro_usd);
                        crate::metrics::record_request_metric(endpoint_str, "5xx", "timeout");
                        return Some(protocol_error(
                            is_messages,
                            StatusCode::GATEWAY_TIMEOUT,
                            "timeout_error",
                            "upstream timed out waiting for first valid SSE frame",
                            Some(request_id_str),
                            None,
                        ));
                    }
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

                let ttfb_ms = (now_ms() - attempt_started).max(0);
                if ttfb_ms >= 10_000 {
                    crate::metrics::record_upstream_slow(&candidate.provider_id.to_string());
                }
                crate::metrics::record_upstream_ttfb(
                    endpoint_str,
                    &candidate.provider_kind.to_string(),
                    ttfb_ms as f64 / 1000.0,
                );
                state.runtime_state.record_upstream_success(
                    origin,
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

                let response_link = if matches!(endpoint, EndpointKind::Responses) {
                    first_chunk
                        .as_ref()
                        .and_then(|bytes| sse_response_id(bytes))
                        .map(|response_id| {
                            let session_hash = pin_key
                                .as_ref()
                                .map_or_else(String::new, |key| key.session_hash.clone());
                            enqueue_response_link(
                                state,
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
                let trace = trace_request.map(|request_body| {
                    TraceCapture::new(request_body.clone(), trace_retention_days, first_chunk.as_ref())
                });
                let finalizer = StreamFinalizer {
                    state: state.clone(),
                    usage,
                    attempt_key_id: candidate.upstream_key_id,
                    model_pk: model.id,
                    store_flag,
                    http_status: status.as_u16(),
                    tap,
                    thread_id: trace_thread_id.to_string(),
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
                    attempts: std::mem::take(attempts),
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
                return Some(builder
                    .body(Body::from_stream(metered))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()));
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
            Some(builder
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))

}
