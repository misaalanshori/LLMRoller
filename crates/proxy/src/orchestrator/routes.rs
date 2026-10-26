use super::*;

/// Compute the Unix wall-clock time in milliseconds.
pub(crate) fn now_ms() -> i64 {
    SystemClock::new().now_ms()
}

/// Auxiliary endpoints are probed and cached independently from primary inference routes (API-8).
pub(crate) const fn aux_endpoint_label(endpoint: EndpointKind) -> Option<&'static str> {
    match endpoint {
        EndpointKind::ResponsesCompact => Some("responses_compact"),
        EndpointKind::CountTokens => Some("count_tokens"),
        _ => None,
    }
}

pub(crate) fn aux_endpoint_unsupported_error(is_messages: bool) -> Response {
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
pub(crate) fn credential_probe_verdict(status: StatusCode) -> Option<bool> {
    if status.is_success() {
        Some(true)
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        Some(false)
    } else {
        None
    }
}

/// Fire the single-flight credential verification probe for a struck key (API-8).
pub(crate) fn spawn_credential_probe(
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

/// Resolve the effective routing scope for a client key, honoring `inherit`
/// against `users.default_routing_scope` (Section 11.2, CLK-3, F-048).
pub(crate) fn resolve_caller_scope(
    caller: &ClientKeyRecord,
    snapshot: &ConfigSnapshot,
) -> RoutingScope {
    if caller.routing_scope == "inherit" {
        snapshot
            .users
            .get(&caller.owner_user_id)
            .map(|u| u.default_routing_scope)
            .unwrap_or_default()
    } else {
        caller.routing_scope.parse().unwrap_or_default()
    }
}

/// Whether a discovered route is currently routable by health state.
pub(crate) fn route_healthy(route: &SnapshotModelRoute, now: i64) -> bool {
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

pub(crate) fn collect_routes(
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
            strict_tier: key.strict_tier,
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

pub(crate) const fn candidate_tier_as_pool(tier: CandidatePoolTier) -> PoolTier {
    match tier {
        CandidatePoolTier::Own => PoolTier::Own,
        CandidatePoolTier::Shared => PoolTier::Shared,
        CandidatePoolTier::Global => PoolTier::Global,
        CandidatePoolTier::Blended => PoolTier::Blended,
    }
}

/// Persisted `usage_events.provider_scope` label for a candidate.
pub(crate) const fn provider_scope_label(candidate: &RouteCandidate) -> &'static str {
    match (candidate.ownership_scope, candidate.share_id.is_some()) {
        (llmroller_core::rbac::OwnershipScope::Global, _) => "global",
        (llmroller_core::rbac::OwnershipScope::Personal, true) => "shared",
        (llmroller_core::rbac::OwnershipScope::Personal, false) => "personal",
    }
}

/// Map a wire endpoint to its independently configurable provider profile section.
pub(crate) const fn provider_endpoint(endpoint: EndpointKind) -> Option<ProviderEndpoint> {
    match endpoint {
        EndpointKind::Chat => Some(ProviderEndpoint::Chat),
        EndpointKind::Responses => Some(ProviderEndpoint::Responses),
        EndpointKind::ResponsesCompact => Some(ProviderEndpoint::ResponsesCompact),
        EndpointKind::Messages => Some(ProviderEndpoint::Messages),
        EndpointKind::CountTokens => Some(ProviderEndpoint::CountTokens),
        EndpointKind::Models => Some(ProviderEndpoint::Models),
        EndpointKind::Usage => Some(ProviderEndpoint::Usage),
        EndpointKind::Embeddings => Some(ProviderEndpoint::Embeddings),
    }
}

/// Build an upstream URL from the provider's configured path, falling back to the adapter preset.
pub(crate) fn endpoint_url(candidate: &RouteCandidate, endpoint: EndpointKind) -> String {
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
    if let Some(ref preset_id) = candidate.config.preset_id
        && let Some(spec) = llmroller_proto::providers::lookup_provider_preset(preset_id)
    {
        return (spec.build_endpoint_url)(&candidate.base_url, endpoint);
    }
    get_adapter(candidate.provider_kind).build_endpoint_url(&candidate.base_url, endpoint)
}

/// Resolve the configured credential header for an endpoint.
pub(crate) fn auth_scheme_for(
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
pub(crate) fn build_upstream_headers(
    inbound: &HeaderMap,
    candidate: &RouteCandidate,
    secret: &str,
    endpoint: EndpointKind,
    session: Option<&ResolvedSession>,
    now_ms: i64,
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

    let hw_profile = llmroller_proto::fingerprint::DeviceProfile::from_key_id(
        &candidate.upstream_key_id.to_string(),
    );
    let sess_token_for_tpl = session
        .map(|s| s.outbound_token(candidate.pool_tier()))
        .unwrap_or("");
    let caller_client_name = llmroller_proto::headers::extract_caller_client_name(inbound);
    let now_unix_secs = u64::try_from((now_ms / 1000).max(0)).unwrap_or(0);
    for (name, value) in &candidate.config.add_headers {
        let lower = name.to_ascii_lowercase();
        if lower == "host"
            || lower == "content-length"
            || lower == "authorization"
            || lower == auth_header
            || lower.starts_with("x-llmroller-")
        {
            continue;
        }
        let Ok(h_name) = HeaderName::from_bytes(name.as_bytes()) else {
            tracing::warn!(header = %name, "ignoring invalid provider add_headers name");
            continue;
        };
        let expanded = llmroller_proto::fingerprint::expand_header_template_with_caller(
            value,
            &hw_profile,
            &hw_profile.device_id,
            sess_token_for_tpl,
            now_unix_secs,
            caller_client_name.as_deref(),
            incoming_ua,
        );
        if expanded.trim().is_empty() {
            continue;
        }
        let Ok(h_val) = HeaderValue::from_str(&expanded) else {
            tracing::warn!(header = %name, "ignoring invalid provider add_headers value");
            continue;
        };
        // Transparent proxy invariant (SRDD 12.1): preserve caller-supplied headers (such as
        // x-client-name, x-title, http-referer, anthropic-beta) unless add_headers explicitly
        // overrides User-Agent for a CLI harness or the caller did not supply the header.
        if lower == "user-agent" || !inbound.contains_key(&h_name) {
            outbound.insert(h_name, h_val);
        } else {
            outbound.entry(h_name).or_insert(h_val);
        }
    }

    // F095, F096, F097, F098, F101-F180: Synthesize built-in provider compatibility headers when absent
    if let Some(ref preset_id) = candidate.config.preset_id
        && let Some(spec) = llmroller_proto::providers::lookup_provider_preset(preset_id)
    {
        for (hk, hv) in
            (spec.synthesize_headers)(&candidate.upstream_key_id.to_string(), sess_token_for_tpl)
        {
            if hk.to_ascii_lowercase().starts_with("x-llmroller-") {
                continue;
            }
            let expanded = llmroller_proto::fingerprint::expand_header_template_with_caller(
                &hv,
                &hw_profile,
                &hw_profile.device_id,
                sess_token_for_tpl,
                now_unix_secs,
                caller_client_name.as_deref(),
                incoming_ua,
            );
            if expanded.trim().is_empty() {
                continue;
            }
            if let (Ok(h_name), Ok(h_val)) = (
                HeaderName::from_bytes(hk.as_bytes()),
                HeaderValue::from_str(&expanded),
            ) {
                if hk.eq_ignore_ascii_case("user-agent") && incoming_ua.is_none() {
                    outbound.insert(h_name, h_val);
                } else {
                    outbound.entry(h_name).or_insert(h_val);
                }
            }
        }
    }
    match candidate.provider_kind {
        ProviderKind::CommandCode => {
            for (hk, hv) in llmroller_proto::providers::command_code::command_code_cli_headers(
                sess_token_for_tpl,
            ) {
                if let (Ok(h_name), Ok(h_val)) = (
                    HeaderName::from_bytes(hk.as_bytes()),
                    HeaderValue::from_str(&hv),
                ) {
                    outbound.entry(h_name).or_insert(h_val);
                }
            }
        }
        ProviderKind::OpencodeGo => {
            outbound
                .entry(HeaderName::from_static("x-opencode-client"))
                .or_insert_with(|| {
                    caller_client_name
                        .as_deref()
                        .and_then(|c| HeaderValue::from_str(c).ok())
                        .unwrap_or_else(|| HeaderValue::from_static("app"))
                });
            let default_oc_sess = format!("ses_{}", &hw_profile.machine_id_sha256[..32]);
            if !outbound.contains_key("x-opencode-session")
                && let Ok(hv) = HeaderValue::from_str(&default_oc_sess)
            {
                outbound.insert(HeaderName::from_static("x-opencode-session"), hv);
            }
        }
        ProviderKind::OpenDesignAmr => {
            outbound
                .entry(HeaderName::from_static("anthropic-version"))
                .or_insert_with(|| HeaderValue::from_static("2023-06-01"));
        }
        ProviderKind::Openrouter => {
            // Transparent proxy: never identify as LLMRoller. If the caller sent x-client-name,
            // x-title, or a User-Agent, translate the caller's client name to x-title when absent.
            if !outbound.contains_key("x-title")
                && let Some(ref client_name) = caller_client_name
                && let Ok(hv) = HeaderValue::from_str(client_name)
            {
                outbound.insert(HeaderName::from_static("x-title"), hv);
            }
        }
        ProviderKind::Custom => {}
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
pub(crate) const fn failure_charge(
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
pub(crate) fn candidate_cap_scope(
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
pub(crate) struct CandidateCapGuards {
    pub(crate) user: Option<MonthReservationGuard>,
    pub(crate) share: Option<MonthReservationGuard>,
}

impl CandidateCapGuards {
    pub(crate) fn commit(&mut self, actual_micro_usd: i64) {
        if let Some(guard) = self.user.take() {
            guard.commit(actual_micro_usd);
        }
        if let Some(guard) = self.share.take() {
            guard.commit(actual_micro_usd);
        }
    }
}

/// Reserves the monthly caps that the candidate's tier actually charges.
pub(crate) fn reserve_candidate_caps(
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
pub(crate) fn eligibility_route(
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
pub(crate) fn select_next_candidate(
    runtime: &RuntimeState,
    snapshot: &ConfigSnapshot,
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
    let is_strict = snapshot
        .models
        .get(&model_pk)
        .is_some_and(|m| m.distribution.eq_ignore_ascii_case("strict"));
    let min_strict_tier = if is_strict {
        eligible.iter().map(|candidate| candidate.strict_tier).min()
    } else {
        None
    };

    let candidates: Vec<(RouteId, i64)> = eligible
        .iter()
        .filter(|candidate| !tried.contains(&candidate.upstream_key_id))
        .filter(|candidate| min_strict_tier.is_none_or(|min_t| candidate.strict_tier == min_t))
        .map(|candidate| {
            let weight = if snapshot.settings.headroom_scaling_enabled {
                let bps = runtime.headroom_bps_for_route(
                    snapshot,
                    candidate.upstream_key_id,
                    Some(model_pk),
                    now,
                );
                llmroller_core::strategy::scale_weight_by_headroom_bps(
                    candidate.priority,
                    bps,
                    true,
                )
            } else {
                candidate.priority
            };
            (candidate.route_id(), weight)
        })
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
pub(crate) struct PinFailureContext<'a> {
    pub(crate) runtime: &'a RuntimeState,
    pub(crate) snapshot: &'a ConfigSnapshot,
    pub(crate) strategy: RoutingStrategy,
    pub(crate) caller: UserId,
    pub(crate) model_pk: ModelPk,
    pub(crate) canonical_id: &'a str,
    pub(crate) protocol: &'a str,
    pub(crate) selected_tier: CandidatePoolTier,
    pub(crate) tier_candidates: &'a [RouteCandidate],
    pub(crate) eligible_keys: &'a HashSet<UpstreamKeyId>,
    pub(crate) now: i64,
    pub(crate) entropy: &'a dyn EntropySource,
}

impl PinFailureContext<'_> {
    pub(crate) fn select(
        &self,
        tried: &[UpstreamKeyId],
    ) -> Option<(UpstreamKeyId, CandidatePoolTier)> {
        select_next_candidate(
            self.runtime,
            self.snapshot,
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
pub(crate) fn note_pre_first_byte_failure(
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

/// Compute the monthly and per-window reservation estimate for an attempt (`F032`).
pub(crate) fn estimate_cost_micro_usd(
    snapshot: &ConfigSnapshot,
    runtime_state: &RuntimeState,
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
    let cache_ratio = runtime_state.route_cache_ratio(candidate.route_id());
    let raw_est = estimate_reservation_cost_micro_usd(
        input_tokens,
        cache_ratio,
        &rates,
        max_output_tokens,
        None,
    );
    let (mode, bps) = snapshot
        .providers
        .get(&candidate.provider_id)
        .map(|p| (p.billing_coverage_mode.as_str(), p.plan_multiplier_bps))
        .unwrap_or(("pay_as_you_go", 10_000));
    let is_free_tier = candidate.upstream_model_id.ends_with(":free") || mode == "free_tier";
    llmroller_core::pricing::apply_effective_upstream_cost(raw_est, bps, mode, is_free_tier)
}

/// Extract the caller's requested output-token ceiling for the pre-flight
/// reservation (`LIM-1`): `max_output_tokens` (Responses), `max_completion_tokens`
/// (o-series Chat), or `max_tokens` (Chat/Messages), in that precedence order.
pub(crate) fn requested_max_output_tokens(body: &[u8]) -> Option<i64> {
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
pub(crate) fn unseal_upstream_key(
    state: &ProxyAuthState,
    snapshot: &ConfigSnapshot,
    key: &UpstreamKeyRecord,
) -> Result<String, ()> {
    let now_ms = state.clock.now_ms();
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
