use super::*;

/// Request content prepared when the turn finalizes, off the TTFB path (PERF-5).
pub(crate) struct PreparedTraceRequest {
    pub(crate) skeleton: serde_json::Value,
    pub(crate) blocks: Vec<(String, Vec<u8>)>,
    pub(crate) block_refs: Vec<String>,
    pub(crate) dropped_blocks: u32,
    pub(crate) expires_at: i64,
}

/// Streaming collector paired with the original request bytes until the turn finalizes.
/// Preparation is deliberately deferred so trace mode does not add TTFB work.
pub(crate) struct TraceCapture {
    pub(crate) request_body: Bytes,
    pub(crate) retention_days: i64,
    pub(crate) tap: TraceTap,
}

impl TraceCapture {
    pub(crate) fn new(
        request_body: Bytes,
        retention_days: i64,
        first_chunk: Option<&Bytes>,
    ) -> Self {
        let (mut tap, _receiver) = TraceTap::new();
        // F013 & F014: Account captured request_body against the 256 MiB GLOBAL_TRACE_BYTES budget.
        let retained_body = if tap.reserve_global_bytes(request_body.len()) {
            request_body
        } else {
            Bytes::new()
        };
        if let Some(chunk) = first_chunk {
            tap.feed_sse_chunk(chunk);
        }
        Self {
            request_body: retained_body,
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
pub(crate) fn extract_trace_thread_id(headers: &HeaderMap) -> String {
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
pub(crate) fn prepare_trace_request(
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
pub(crate) struct TraceJobMeta<'a> {
    pub(crate) usage_event_id: &'a str,
    pub(crate) request_id: RequestId,
    pub(crate) ts: i64,
    pub(crate) owner_user_id: UserId,
    pub(crate) client_key_id: ClientKeyId,
    pub(crate) session_hash: &'a str,
    pub(crate) thread_id: &'a str,
    pub(crate) canonical_id: &'a str,
    pub(crate) protocol: &'a str,
    pub(crate) response_raw: Option<serde_json::Value>,
}

/// Submit a sealed turn to the sealer pool. Returns `false` when tracing is unavailable
/// or the bounded pool rejected the job, which is reflected in `usage_events.trace_dropped`.
pub(crate) fn submit_trace_job(
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
pub(crate) struct UsageContext {
    pub(crate) request_id: RequestId,
    pub(crate) client_key_id: ClientKeyId,
    pub(crate) owner_user_id: UserId,
    pub(crate) canonical_model: String,
    pub(crate) upstream_key_id: Option<String>,
    pub(crate) provider_id: Option<String>,
    pub(crate) provider_scope: Option<String>,
    pub(crate) share_id: Option<String>,
    pub(crate) upstream_model_id: String,
    pub(crate) endpoint_str: String,
    pub(crate) session_hash: Option<String>,
    pub(crate) pin_outcome: Option<String>,
    pub(crate) failovers: i64,
    pub(crate) error_code: Option<String>,
    pub(crate) billing_coverage_mode: String,
    pub(crate) plan_multiplier_bps: i64,
    pub(crate) service_account_id: Option<String>,
    pub(crate) request_headers_json: Option<String>,
}

impl UsageContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn event(
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
        let is_free_tier = self.upstream_model_id.ends_with(":free")
            || self.canonical_model.ends_with(":free")
            || self.billing_coverage_mode == "free_tier";
        let effective_cost = llmroller_core::pricing::apply_effective_upstream_cost(
            list_cost_micro_usd,
            self.plan_multiplier_bps,
            &self.billing_coverage_mode,
            is_free_tier,
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
pub(crate) fn attempt_record(
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
pub(crate) struct ResponseLinkFinalizer {
    pub(crate) response_id: String,
    pub(crate) owner_user_id: UserId,
    pub(crate) client_key_id: ClientKeyId,
    pub(crate) upstream_key_id: UpstreamKeyId,
    pub(crate) model_pk: ModelPk,
    pub(crate) session_hash: String,
    pub(crate) store_flag: bool,
    pub(crate) created_at: i64,
}

pub(crate) struct StreamFinalizer {
    pub(crate) state: ProxyAuthState,
    pub(crate) usage: UsageContext,
    pub(crate) attempt_key_id: UpstreamKeyId,
    pub(crate) model_pk: ModelPk,
    pub(crate) store_flag: bool,
    pub(crate) http_status: u16,
    pub(crate) tap: SseUsageTap,
    pub(crate) thread_id: String,
    pub(crate) reservation: Option<Reservation>,
    pub(crate) client_guard: Option<MonthReservationGuard>,
    pub(crate) cap_guards: CandidateCapGuards,
    pub(crate) price_entry: Option<PriceEntry>,
    pub(crate) fallback_rates: Rates,
    pub(crate) attempts: Vec<UsageAttemptRecord>,
    pub(crate) response_link: Option<ResponseLinkFinalizer>,
    pub(crate) trace: Option<TraceCapture>,
    pub(crate) estimated_input_tokens: i64,
    pub(crate) started_ms: i64,
    pub(crate) ttfb_ms: Option<i64>,
    pub(crate) bytes_sent: usize,
    pub(crate) finalized: bool,
}

impl StreamFinalizer {
    pub(crate) fn finalize(&mut self, termination: Termination) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        let finished_ms = self.state.clock.now_ms();
        // F009: Flush any trailing unterminated SSE frame before computing final usage.
        self.tap.finish();
        let mut extracted = self.tap.finalize_usage();
        let is_incomplete = termination.is_incomplete();
        let missing_upstream_usage = self.tap.observed_usage.is_none();
        if (is_incomplete || missing_upstream_usage)
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
        if (is_incomplete || missing_upstream_usage) && extracted.provider_cost_micro_usd.is_none()
        {
            cost.cost_source = CostSource::EstimatedIncomplete;
        }
        let is_free_tier = self.usage.upstream_model_id.ends_with(":free")
            || self.usage.canonical_model.ends_with(":free")
            || self.usage.billing_coverage_mode == "free_tier";
        let effective_cost = llmroller_core::pricing::apply_effective_upstream_cost(
            cost.cost_micro_usd,
            self.usage.plan_multiplier_bps,
            &self.usage.billing_coverage_mode,
            is_free_tier,
        );

        if let Some(reservation) = self.reservation.take() {
            reservation.commit(effective_cost, finished_ms);
        }
        if let Some(guard) = self.client_guard.take() {
            guard.commit(effective_cost);
        }
        self.cap_guards.commit(effective_cost);

        let final_attempt_outcome = match termination {
            Termination::Completed => llmroller_core::usage::AttemptOutcome::Served,
            Termination::IdleTimeout => llmroller_core::usage::AttemptOutcome::WaitTimeout,
            Termination::UpstreamTruncated => llmroller_core::usage::AttemptOutcome::ConnectError,
            Termination::ClientGone | Termination::ShutdownCut => {
                llmroller_core::usage::AttemptOutcome::WriteStalled
            }
        };
        let final_charge_basis = if termination.is_incomplete() && self.tap.observed_usage.is_none()
        {
            "estimated_incomplete"
        } else {
            "actual"
        };
        let served_attempt = attempt_record(
            self.usage.request_id,
            self.attempts.len() as i64 + 1,
            finished_ms,
            self.attempt_key_id,
            self.usage.share_id.clone(),
            final_attempt_outcome,
            Some(self.http_status),
            self.bytes_sent,
            effective_cost,
            final_charge_basis,
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
            let pending_trace = self.trace.take().map(|mut trace| {
                trace.tap.finish_sse();
                let prepared =
                    prepare_trace_request(&trace.request_body, trace.retention_days, finished_ms);
                (trace, prepared)
            });
            if let Some((ref trace, ref prepared)) = pending_trace {
                let can_submit = prepared.is_some()
                    && self.state.sealer_pool.is_some()
                    && self.state.master_key_id.is_some()
                    && !trace
                        .tap
                        .trace_dropped
                        .load(std::sync::atomic::Ordering::Acquire);
                if !can_submit {
                    event.trace_dropped = true;
                }
            }
            let event_id = event.id.clone();
            // F016: Enqueue UsageEventRecord BEFORE submitting SealJob so usage_events row
            // is always queued ahead of sealed trace persistence.
            writer
                .queues
                .usage_writer
                .enqueue(event, std::mem::take(&mut self.attempts));
            if let Some((trace, prepared_opt)) = pending_trace {
                let accepted = prepared_opt.is_some_and(|prepared| {
                    submit_trace_job(
                        &self.state,
                        TraceJobMeta {
                            usage_event_id: &event_id,
                            request_id: self.usage.request_id,
                            ts: finished_ms,
                            owner_user_id: self.usage.owner_user_id,
                            client_key_id: self.usage.client_key_id,
                            session_hash: self.usage.session_hash.as_deref().unwrap_or_default(),
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
                if !accepted {
                    crate::metrics::record_trace_dropped();
                }
            }
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
        if extracted.cache_write_tokens > 0 {
            crate::metrics::record_tokens(
                ep,
                "2xx",
                "stream",
                &self.usage.canonical_model,
                "cache_write",
                extracted.cache_write_tokens as u64,
            );
        }
        if extracted.reasoning_tokens > 0 {
            crate::metrics::record_tokens(
                ep,
                "2xx",
                "stream",
                &self.usage.canonical_model,
                "reasoning",
                extracted.reasoning_tokens as u64,
            );
        }
        // Same rule as the non-streaming path: the metric is an observed operator number
        // and reflects what was charged, so it takes the plan-multiplied
        // `effective_cost`. The ledger keeps list cost plus `plan_multiplier_bps`
        // separately (`USG-10`, SRDD 13).
        if effective_cost > 0 {
            crate::metrics::record_cost(
                ep,
                "2xx",
                "stream",
                &self.usage.canonical_model,
                effective_cost as u64,
            );
        }

        self.state.runtime_state.record_route_cache_ratio(
            RouteId {
                upstream_key_id: self.attempt_key_id,
                model_pk: self.model_pk,
            },
            extracted.input_uncached_tokens,
            extracted.cache_read_tokens,
            extracted.cache_write_tokens,
        );

        let status_label = if termination.is_incomplete() {
            "incomplete"
        } else {
            "completed"
        };
        if let Some(link) = &self.response_link {
            enqueue_response_link(
                &self.state,
                link.owner_user_id,
                link.client_key_id,
                link.model_pk,
                link.upstream_key_id,
                &link.session_hash,
                &link.response_id,
                status_label,
                link.store_flag,
                link.created_at,
            );
        } else if self.usage.endpoint_str == "responses"
            && let Some(ref response_id) = self.tap.response_id
        {
            // F049 & F050: Persist response_link when response.created arrived after first_chunk
            enqueue_response_link(
                &self.state,
                self.usage.owner_user_id,
                self.usage.client_key_id,
                self.model_pk,
                self.attempt_key_id,
                self.usage.session_hash.as_deref().unwrap_or(""),
                response_id,
                status_label,
                self.store_flag,
                self.started_ms,
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
        let next = counter.fetch_add(1, Ordering::SeqCst) + 1;
        crate::metrics::record_active_streams(next);
        Self(counter)
    }
}

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        let prev = self.0.fetch_sub(1, Ordering::SeqCst);
        crate::metrics::record_active_streams(prev.saturating_sub(1));
    }
}
