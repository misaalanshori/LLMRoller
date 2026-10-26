use super::*;
use sqlx::Row as _;

pub(crate) async fn get_user_providers(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Provider>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let providers = list_providers(&state.auth.pool, &actor)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(providers))
}

/// Resolves optional `preset_id` defaults (`default_base_url` and `default_provider_config`) from
/// `llmroller_proto::providers::ALL_PROVIDER_PRESET_SPECS` (`F101`–`F180`).
pub(crate) fn apply_preset_defaults(
    base_url: String,
    config_json: String,
    preset_id: Option<&str>,
) -> (String, String) {
    let parsed_cfg = ProviderConfig::from_json(&config_json);
    let effective_preset = preset_id.or(parsed_cfg.preset_id.as_deref());
    let Some(slug) = effective_preset else {
        return (base_url, config_json);
    };
    let Some(spec) = llmroller_proto::providers::lookup_provider_preset(slug) else {
        return (base_url, config_json);
    };
    let resolved_url = if base_url.trim().is_empty() {
        spec.default_base_url.to_string()
    } else {
        base_url
    };
    let resolved_cfg = if config_json.trim().is_empty() || config_json.trim() == "{}" {
        serde_json::to_string(&(spec.default_provider_config)()).unwrap_or(config_json)
    } else {
        config_json
    };
    (resolved_url, resolved_cfg)
}

/// Returns all 80 compiled-in provider presets (`F101`–`F180`).
pub(crate) async fn get_provider_presets_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_actor(&state, &headers, false).await?;
    let presets: Vec<serde_json::Value> = llmroller_proto::providers::ALL_PROVIDER_PRESET_SPECS
        .iter()
        .map(|spec| {
            serde_json::json!({
                "finding_id": spec.finding_id,
                "preset_id": spec.preset_id,
                "display_name": spec.display_name,
                "default_base_url": spec.default_base_url,
                "supported_protocols": spec.supported_protocols,
                "default_models": spec.default_models,
            })
        })
        .collect();
    Ok(Json(presets))
}

pub(crate) async fn post_user_provider(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<Provider>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    validate_provider_config_json(&payload.config_json)?;
    let owner_user_id = actor.user_id();
    let effective_preset_id = payload
        .preset_id
        .as_deref()
        .or(payload.kind.preset_id.as_deref());
    let (base_url, config_json) =
        apply_preset_defaults(payload.base_url, payload.config_json, effective_preset_id);

    let new_provider = NewProvider {
        name: payload.name,
        kind: payload.kind.kind,
        base_url,
        config_json,
        ownership_scope: OwnershipScope::Personal,
        owner_user_id,
        usage_url_cross_origin_ack: payload.usage_url_cross_origin_ack,
        allow_private_network: payload.allow_private_network,
        enabled: payload.enabled,
    };

    let now_ms = state.auth.clock.now_ms();
    let provider = create_provider(&state.auth.pool, &actor, new_provider, now_ms)
        .await
        .map_err(map_provider_error)?;
    Ok((StatusCode::CREATED, Json(provider)))
}

pub(crate) async fn get_user_provider(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ProviderId>,
) -> Result<Json<Provider>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let provider = get_provider(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(provider))
}

pub(crate) async fn patch_user_provider_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ProviderId>,
    Json(payload): Json<PatchProviderRequest>,
) -> Result<Json<Provider>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    if let Some(config_json) = &payload.config_json {
        validate_provider_config_json(config_json)?;
    }
    let patch = ProviderPatch {
        name: payload.name,
        base_url: payload.base_url,
        config_json: payload.config_json,
        usage_url_cross_origin_ack: payload.usage_url_cross_origin_ack,
        allow_private_network: payload.allow_private_network,
        enabled: payload.enabled,
    };
    let now_ms = state.auth.clock.now_ms();
    let provider = patch_provider(&state.auth.pool, &actor, id, patch, now_ms)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(provider))
}

pub(crate) async fn delete_user_provider_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ProviderId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    delete_provider(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    if headers.contains_key("hx-request") {
        Ok(StatusCode::OK)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

// ---------------------------------------------------------------------------
// Personal Upstream Key Endpoints (/api/user/providers/{id}/keys, /api/user/upstream-keys/{id})
// ---------------------------------------------------------------------------

pub(crate) async fn get_user_provider_keys(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(provider_id): Path<ProviderId>,
) -> Result<Json<Vec<UpstreamKey>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let keys = list_upstream_keys(&state.auth.pool, &actor, provider_id)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(keys))
}

pub(crate) async fn post_user_provider_key(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(provider_id): Path<ProviderId>,
    Json(payload): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<UpstreamKey>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let new_key = NewUpstreamKey {
        provider_id,
        label: payload.label,
        secret: secret_string(&payload.secret),
        priority: payload.priority,
        strict_tier: payload.strict_tier,
        enabled: payload.enabled,
    };
    let now_ms = state.auth.clock.now_ms();
    let key = create_upstream_key(
        &state.auth.pool,
        &actor,
        &state.master_key,
        &state.master_key_id,
        state.auth.entropy.clone(),
        new_key,
        now_ms,
    )
    .await
    .map_err(map_provider_error)?;
    state.trigger_discovery(llmroller_proto::discovery::DiscoveryTarget::Key(key.id));
    Ok((StatusCode::CREATED, Json(key)))
}

pub(crate) async fn get_user_upstream_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
) -> Result<Json<UpstreamKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let key = get_upstream_key(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(key))
}

pub(crate) async fn patch_user_upstream_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
    Json(payload): Json<PatchKeyRequest>,
) -> Result<Json<UpstreamKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let patch = UpstreamKeyPatch {
        label: payload.label,
        priority: payload.priority,
        strict_tier: payload.strict_tier,
        enabled: payload.enabled,
    };
    let key = patch_upstream_key(&state.auth.pool, &actor, id, patch)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(key))
}

pub(crate) async fn delete_user_upstream_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    delete_upstream_key(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    if headers.contains_key("hx-request") {
        Ok(StatusCode::OK)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

// ---------------------------------------------------------------------------
// Client Key Endpoints (/api/user/client-keys)
// ---------------------------------------------------------------------------

pub(crate) async fn get_admin_providers(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Provider>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let providers = list_providers(&state.auth.pool, &actor)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(providers))
}

pub(crate) async fn post_admin_provider(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<Provider>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    validate_provider_config_json(&payload.config_json)?;
    let effective_preset_id = payload
        .preset_id
        .as_deref()
        .or(payload.kind.preset_id.as_deref());
    let (base_url, config_json) =
        apply_preset_defaults(payload.base_url, payload.config_json, effective_preset_id);
    let new_provider = NewProvider {
        name: payload.name,
        kind: payload.kind.kind,
        base_url,
        config_json,
        ownership_scope: OwnershipScope::Global,
        owner_user_id: None,
        usage_url_cross_origin_ack: payload.usage_url_cross_origin_ack,
        allow_private_network: payload.allow_private_network,
        enabled: payload.enabled,
    };
    let now_ms = state.auth.clock.now_ms();
    let provider = create_provider(&state.auth.pool, &actor, new_provider, now_ms)
        .await
        .map_err(map_provider_error)?;
    Ok((StatusCode::CREATED, Json(provider)))
}

pub(crate) async fn post_admin_provider_key(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(provider_id): Path<ProviderId>,
    Json(payload): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<UpstreamKey>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let new_key = NewUpstreamKey {
        provider_id,
        label: payload.label,
        secret: secret_string(&payload.secret),
        priority: payload.priority,
        strict_tier: payload.strict_tier,
        enabled: payload.enabled,
    };
    let now_ms = state.auth.clock.now_ms();
    let key = create_upstream_key(
        &state.auth.pool,
        &actor,
        &state.master_key,
        &state.master_key_id,
        state.auth.entropy.clone(),
        new_key,
        now_ms,
    )
    .await
    .map_err(map_provider_error)?;
    state.trigger_discovery(llmroller_proto::discovery::DiscoveryTarget::Key(key.id));
    Ok((StatusCode::CREATED, Json(key)))
}

pub(crate) async fn post_reset_upstream_key_health_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let key = get_upstream_key(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    if !actor.is_admin() && key.owner_user_id != actor.user_id() {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "upstream key not found",
        ));
    }
    let key_id_str = id.to_string();
    execute_atomic_mutation_with_invalidations(
        &state,
        vec![ConfigDomain::UpstreamKeys],
        vec![llmroller_core::snapshot::CacheInvalidationEvent::UpstreamKeyChanged(id)],
        move |tx| {
            Box::pin(async move {
                sqlx::query(
                    "UPDATE upstream_keys
                     SET health = 'healthy', health_until = NULL, health_version = health_version + 1
                     WHERE id = ?",
                )
                .bind(&key_id_str)
                .execute(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                sqlx::query("DELETE FROM route_protocol_blocks WHERE upstream_key_id = ?")
                    .bind(&key_id_str)
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                Ok(())
            })
        },
    )
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "upstream_key_id": id.to_string(),
        "health": "healthy",
    })))
}

// ---------------------------------------------------------------------------
// Upstream Key Test (PRV-6)
// ---------------------------------------------------------------------------
pub(crate) async fn post_test_upstream_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;

    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::SecretSealer as _;
    let key = sqlx::query(
        "SELECT k.id, k.owner_user_id, k.secret_enc, k.master_key_id, p.id AS provider_id,
                p.ownership_scope, p.base_url, p.kind, p.config_json, p.allow_private_network
         FROM upstream_keys k
         JOIN providers p ON k.provider_id = p.id
         WHERE k.id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "upstream key not found"))?;

    let scope: String = key.try_get("ownership_scope").unwrap_or_default();
    let owner: Option<String> = key.try_get("owner_user_id").unwrap_or_default();
    if scope == "personal" && !actor.is_admin() {
        let uid = actor.user_id().map(|u| u.to_string());
        if owner != uid {
            return Err(map_price_error(
                StatusCode::NOT_FOUND,
                "upstream key not found",
            ));
        }
    }

    let secret_enc: Vec<u8> = key.try_get("secret_enc").unwrap_or_default();
    let key_master_key_id: String = key.try_get("master_key_id").unwrap_or_default();
    let provider_id: String = key.try_get("provider_id").unwrap_or_default();
    let base_url: String = key.try_get("base_url").unwrap_or_default();
    let kind_text: String = key.try_get("kind").unwrap_or_default();
    let config_json: String = key.try_get("config_json").unwrap_or_default();
    let allow_private: bool = key.try_get::<i64, _>("allow_private_network").unwrap_or(0) != 0;

    let instance_id_vec: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_optional(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
            .unwrap_or_else(|| vec![0_u8; 16]);
    let instance_id: [u8; 16] = instance_id_vec.try_into().unwrap_or([0_u8; 16]);
    let sealer = llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
        state.master_key.clone(),
        instance_id,
        std::sync::Arc::new(llmroller_core::seams::OsEntropy),
    );
    let key_id_text = id.to_string();
    let secret = sealer
        .open(
            "upstream_key",
            &[
                key_master_key_id.as_bytes(),
                key_id_text.as_bytes(),
                provider_id.as_bytes(),
            ],
            &secret_enc,
        )
        .map_err(|_| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot open key"))?;
    let secret = String::from_utf8_lossy(secret.expose_secret()).to_string();

    // Probe the provider's configured discovery endpoint with its configured auth
    // scheme instead of assuming `Bearer` against `/models` (`PRV-6`, F044).
    let provider_kind = kind_text
        .parse::<llmroller_core::provider::ProviderKind>()
        .unwrap_or(llmroller_core::provider::ProviderKind::Custom);
    let provider_config = llmroller_core::provider::ProviderConfig::from_json(&config_json);
    let target =
        llmroller_proto::discovery::discovery_endpoint(provider_kind, &base_url, &provider_config);
    if llmroller_core::ssrf::validate_url(&target, allow_private).is_err() {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "provider URL is not allowed",
        ));
    }
    let client = llmroller_proxy::ssrf::create_guarded_client(allow_private)
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let started = std::time::Instant::now();
    let mut probe = client
        .get(&target)
        .timeout(std::time::Duration::from_secs(10));
    for (name, value) in llmroller_proto::discovery::discovery_headers(&provider_config, &secret) {
        if let Some(name) = name {
            probe = probe.header(name, value);
        }
    }
    let response = probe.send().await;
    let latency_ms = started.elapsed().as_millis() as i64;

    let (status, http_status, message) = match response {
        Ok(resp) if resp.status().is_success() => (
            "ok",
            Some(i64::from(resp.status().as_u16())),
            "Key validated successfully",
        ),
        Ok(resp) => {
            let code = i64::from(resp.status().as_u16());
            let message = match resp.status().as_u16() {
                401 | 403 => "Upstream rejected the credential",
                429 => "Upstream rate limited the probe",
                _ => "Upstream probe failed",
            };
            ("error", Some(code), message)
        }
        Err(_) => ("error", None, "Upstream unreachable"),
    };

    let health = if status == "ok" { "healthy" } else { "invalid" };
    let _ = sqlx::query("UPDATE upstream_keys SET health = ? WHERE id = ?")
        .bind(health)
        .bind(&key_id_text)
        .execute(&state.auth.pool)
        .await;

    Ok(Json(serde_json::json!({
        "status": status,
        "http_status": http_status,
        "latency_ms": latency_ms,
        "message": message,
        "key_id": key_id_text,
    })))
}
