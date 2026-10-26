use super::*;

/// Load one durable response link for a routed Responses operation.
pub(crate) async fn load_response_link(
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
pub(crate) fn hard_route_checks(
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
    let scope = resolve_caller_scope(caller, snapshot);

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
pub(crate) fn candidate_tier(
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
pub(crate) fn is_conflict_minority(model: &SnapshotModel, route: &SnapshotModelRoute) -> bool {
    model.merge_state == llmroller_core::catalog::MergeState::PendingReview
        && route.vendor_prefix.as_ref() != model.majority_vendor_prefix.as_ref()
}

/// Whether every enabled global provider with a usable key serves the model (MDL-3).
pub(crate) fn present_on_all_global_providers(
    snapshot: &ConfigSnapshot,
    model: &SnapshotModel,
) -> bool {
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
pub(crate) fn linked_candidate(
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
        strict_tier: key.strict_tier,
        provider_kind: provider.kind,
        share_id,
    })
}

/// Whether the Responses body opted into upstream storage (`store` defaults to true).
pub(crate) fn response_store_flag(
    scan: &llmroller_proto::body_edit::TopLevelScan,
    body: &[u8],
) -> bool {
    scan.fields
        .iter()
        .find(|field| field.key == "store")
        .and_then(|field| body.get(field.val_start..field.val_end))
        .and_then(|raw| std::str::from_utf8(raw).ok())
        .is_none_or(|raw| raw.trim() != "false")
}

/// Durable `response_links` TTL: 30 days when stored, 1 hour otherwise.
pub(crate) fn response_link_ttl_ms(store_flag: bool) -> i64 {
    if store_flag {
        30 * 24 * 60 * 60 * 1_000
    } else {
        60 * 60 * 1_000
    }
}

/// Coalesced durable response-link write (RTE-12).
#[allow(clippy::too_many_arguments)]
pub(crate) fn enqueue_response_link(
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
pub(crate) fn enqueue_pin_upsert(
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
pub(crate) fn enqueue_key_health(
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
pub(crate) fn enqueue_route_protocol_block(
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
pub(crate) fn enqueue_route_health_update(
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
    let next_ver = i64::try_from(state.runtime_state.health.bump_version()).unwrap_or(i64::MAX);
    writer.queues.durable_state.lock().insert(
        DurableKey::RouteHealth(key_id.to_string(), model_pk.to_string()),
        DurableOp::RouteHealthUpdate {
            upstream_key_id: key_id.to_string(),
            model_pk: model_pk.to_string(),
            route_health: route_health.to_string(),
            route_health_until,
            health_version: next_ver,
            last_error,
        },
    );
}

/// Extract the upstream response id from a non-streaming `responses` payload.
pub(crate) fn json_response_id(value: &serde_json::Value) -> Option<&str> {
    value.get("id").and_then(|id| id.as_str())
}

/// Extract `response.created` (or any response object) id from the first SSE frame.
pub(crate) fn sse_response_id(bytes: &[u8]) -> Option<String> {
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
pub(crate) async fn forward_state_bound_response_op(
    state: &ProxyAuthState,
    caller: &ClientKeyRecord,
    response_id: &str,
    method: Method,
    subpath: &str,
    query_str: Option<&str>,
) -> Response {
    use crate::failover::{HardRouteDecision, evaluate_hard_route_decision};

    let now = state.clock.now_ms();
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

    let raw_secret = match unseal_upstream_key(state, &snapshot, &key) {
        Ok(secret) => secret,
        Err(()) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let secret = match state
        .token_manager
        .resolve_and_persist_secret(
            key.id,
            key.provider_id,
            &raw_secret,
            state.select_http_client(provider.allow_private_network),
            provider.allow_private_network,
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
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
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
    let mut req_builder = client
        .request(method.clone(), &url)
        .header(auth_header, format!("{auth_prefix}{secret}"))
        .header("x-request-id", &request_id)
        .header(header::ACCEPT_ENCODING, "identity");
    for (h_name, h_val) in &provider.config.add_headers {
        let lower = h_name.to_ascii_lowercase();
        if lower != "host" && lower != "content-length" && lower != auth_header {
            req_builder = req_builder.header(h_name.as_str(), h_val.as_str());
        }
    }
    let send_timeout = Duration::from_millis(provider.config.first_activity_timeout_ms());
    let upstream = match tokio::time::timeout(send_timeout, req_builder.send()).await {
        Ok(Ok(response)) => response,
        Err(_) => {
            return json_error(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout_error",
                "the linked upstream key timed out",
            );
        }
        Ok(Err(_)) => {
            return json_error(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "the linked upstream key could not be reached",
            );
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_encoding = upstream_content_encoding(upstream.headers());
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
    let body_timeout = Duration::from_millis(provider.config.nonstream_response_timeout_ms());
    let raw_body = match tokio::time::timeout(body_timeout, upstream.bytes()).await {
        Ok(Ok(body)) => body,
        Err(_) => {
            return json_error(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout_error",
                "the linked upstream key timed out while reading response body",
            );
        }
        Ok(Err(_)) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let body = decode_upstream_body(content_encoding.as_deref(), raw_body).unwrap_or_default();

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
                        updated_at: state.clock.now_ms(),
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
            PoolTier::Own => "personal",
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
                latency_ms: Some((state.clock.now_ms() - now).max(0)),
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
    let now = state.clock.now_ms();
    let request_id_str = RequestId::generate().to_string();
    let scan = match scan_top_level(&body) {
        Ok(scan) => scan,
        Err(error) => {
            return protocol_error(
                true,
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                error.to_string(),
                Some(&request_id_str),
                None,
            );
        }
    };
    let Some((ref model_str, _)) = scan.model else {
        return protocol_error(
            true,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model is required in request body",
            Some(&request_id_str),
            None,
        );
    };
    let snapshot = state.snapshot.load();
    let model = match snapshot.resolve_model(model_str) {
        Ok(model) => model.clone(),
        Err(llmroller_core::snapshot::SnapshotResolveError::KillSwitched) => {
            return protocol_error(
                true,
                StatusCode::FORBIDDEN,
                "model_disabled",
                format!("model {model_str} is disabled"),
                Some(&request_id_str),
                None,
            );
        }
        Err(_) => {
            return protocol_error(
                true,
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("model {model_str} not found in catalog"),
                Some(&request_id_str),
                None,
            );
        }
    };
    // Gate G6: client key model allow-list (CLK-2, FND-007)
    if let Some(ref allowed) = caller.allowed_models
        && !llmroller_core::client_key::is_model_allowed_by_patterns(
            allowed,
            model_str,
            &model.canonical_id,
        )
    {
        return protocol_error(
            true,
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("model {} not permitted on this key", model.canonical_id),
            Some(&request_id_str),
            None,
        );
    }
    // Gate G7: client key monthly budget preflight check (CLK-5, FND-007)
    if state.runtime_state.accumulators.is_exhausted(
        &llmroller_core::accumulators::BudgetScopeId::ClientKey(caller.id),
        caller.budget_micro_usd_month,
        now,
    ) {
        let month = llmroller_core::accumulators::UtcYearMonth::from_epoch_millis(now);
        let retry_after = month.seconds_until_next_month(now);
        return protocol_error(
            true,
            StatusCode::TOO_MANY_REQUESTS,
            "client_key_budget_exhausted",
            "client key monthly budget exhausted",
            Some(&request_id_str),
            Some(retry_after),
        );
    }

    let scope = resolve_caller_scope(&caller, &snapshot);
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
        let raw_secret = match unseal_upstream_key(&state, &snapshot, &key) {
            Ok(secret) => secret,
            Err(()) => continue,
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
            Err(_) => continue,
        };
        let url = endpoint_url(&candidate, EndpointKind::CountTokens);
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
            now,
        ) {
            Ok(headers) => headers,
            Err(response) => return *response,
        };

        let send_timeout = Duration::from_millis(candidate.config.first_activity_timeout_ms());
        let response = match tokio::time::timeout(
            send_timeout,
            client.post(&url).body(edited).headers(outbound).send(),
        )
        .await
        {
            Ok(Ok(response)) => response,
            _ => {
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
        let is_admin = snapshot
            .users
            .get(&caller.owner_user_id)
            .is_some_and(|user| user.role == Role::Admin);
        let req_id_str = RequestId::generate().to_string();
        let filtered_headers = llmroller_proto::headers::apply_response_header_pipeline(
            response.headers(),
            candidate.pool_tier(),
            is_admin,
            &req_id_str,
            &candidate.config.response_header_allow,
        );
        let content_encoding = upstream_content_encoding(response.headers());
        let body_timeout = Duration::from_millis(candidate.config.nonstream_response_timeout_ms());
        let raw_bytes = match tokio::time::timeout(body_timeout, response.bytes()).await {
            Ok(Ok(bytes)) => bytes,
            _ => return StatusCode::BAD_GATEWAY.into_response(),
        };
        let bytes =
            decode_upstream_body(content_encoding.as_deref(), raw_bytes).unwrap_or_default();
        let mut builder = Response::builder().status(status);
        for (name, value) in filtered_headers {
            if let Some(name) = name {
                builder = builder.header(name, value);
            }
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
    pub(crate) upstream_key_id: UpstreamKeyId,
    pub(crate) provider_id: ProviderId,
    pub(crate) model_pk: ModelPk,
    pub(crate) upstream_model_id: String,
    pub(crate) base_url: String,
    pub(crate) allow_private_network: bool,
    /// Parsed provider configuration for endpoint paths, auth, and header profiles.
    pub(crate) config: llmroller_core::provider::ProviderConfig,
    pub(crate) pool_tier: CandidatePoolTier,
    pub(crate) ownership_scope: llmroller_core::rbac::OwnershipScope,
    pub(crate) priority: i64,
    pub(crate) strict_tier: i64,
    pub(crate) provider_kind: ProviderKind,
    pub(crate) share_id: Option<ShareId>,
}

impl RouteCandidate {
    pub(crate) fn route_id(&self) -> RouteId {
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

    pub(crate) fn pool_tier(&self) -> PoolTier {
        match self.pool_tier {
            CandidatePoolTier::Own => PoolTier::Own,
            CandidatePoolTier::Shared => PoolTier::Shared,
            CandidatePoolTier::Global => PoolTier::Global,
            CandidatePoolTier::Blended => PoolTier::Blended,
        }
    }

    /// Configured session hint profile, falling back to the provider preset default.
    pub(crate) fn session_profile(&self) -> llmroller_core::provider::SessionProfile {
        self.config
            .session_profile
            .clone()
            .unwrap_or_else(|| get_adapter(self.provider_kind).default_session_profile())
    }
}
