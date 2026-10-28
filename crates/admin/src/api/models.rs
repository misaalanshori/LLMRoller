use super::*;
use sqlx::Row as _;

pub(crate) async fn get_admin_models_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    let model_rows = sqlx::query(
        "SELECT m.model_pk, m.canonical_id, m.origin, m.exposed, m.merge_state,
                (SELECT COUNT(*) FROM model_routes r WHERE r.model_pk = m.model_pk) as route_count
         FROM models m ORDER BY m.canonical_id ASC",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut models = Vec::new();
    for r in model_rows {
        models.push(serde_json::json!({
            "model_pk": r.try_get::<String, _>("model_pk").unwrap_or_default(),
            "canonical_id": r.try_get::<String, _>("canonical_id").unwrap_or_default(),
            "origin": r.try_get::<String, _>("origin").unwrap_or_default(),
            "exposed": r.try_get::<Option<i64>, _>("exposed").unwrap_or_default(),
            "merge_state": r.try_get::<String, _>("merge_state").unwrap_or_default(),
            "route_count": r.try_get::<i64, _>("route_count").unwrap_or(0),
        }));
    }

    let alias_rows =
        sqlx::query("SELECT alias, model_pk, confirmed FROM model_aliases ORDER BY alias ASC")
            .fetch_all(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut aliases = Vec::new();
    for r in alias_rows {
        aliases.push(serde_json::json!({
            "alias": r.try_get::<String, _>("alias").unwrap_or_default(),
            "model_pk": r.try_get::<String, _>("model_pk").unwrap_or_default(),
            "confirmed": r.try_get::<i64, _>("confirmed").unwrap_or(0) == 1,
        }));
    }

    Ok(Json(serde_json::json!({
        "models": models,
        "aliases": aliases,
    })))
}

pub(crate) async fn post_admin_models_refresh_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    record_control_mutation(&state, vec![ConfigDomain::Models]).await?;
    state.trigger_discovery(llmroller_proto::discovery::DiscoveryTarget::AllEnabled);
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "status": "queued" })),
    ))
}

pub(crate) async fn post_admin_model_alias_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ModelPk>,
    Json(payload): Json<AddAliasRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    llmroller_store::models::add_model_alias_tx(&mut tx, &payload.alias, &id)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;
    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(StatusCode::CREATED)
}

/// Accepted `models.distribution` values, from the schema's CHECK constraint.
const MODEL_DISTRIBUTIONS: [&str; 2] = ["weighted", "strict"];

/// `PATCH /admin/api/models/{model_pk}` — partial update of one canonical model.
///
/// Replaces an eleven-line handler that accepted one field, discarded the SQL result, and
/// wrote outside a transaction (`ISSUE-0170`):
///
/// - **Validation, not a CHECK violation.** `distribution` and `routing_strategy` are
///   CHECK-constrained in `0001_initial.sql`, so an unchecked value surfaced as a `500` from
///   SQLite. Both are validated here first and rejected with `422`, mirroring the
///   `default_routing_scope` / `ui_locale` checks in `users_and_auth` — the house pattern.
///
/// - **`COALESCE`, not three `SET`s.** Every field is optional, so the statement keeps the
///   stored value when the request omitted the field. Writing `SET exposed = ?, distribution =
///   ?, routing_strategy = ?` from possibly-`None` values would null `routing_strategy` on
///   every `{"exposed":0}` PATCH, and 500 on `distribution` (it is `NOT NULL`).
///
/// - **`execute_atomic_mutation`, not a bare `UPDATE`.** The handler runs inside one
///   `BEGIN IMMEDIATE` on the writer actor, so the row, the audit entry, the
///   `ConfigDomain::Models` publish and the version bump are one unit. A bare `UPDATE` on the
///   reader-facing pool changed the database and left the data plane on the old snapshot.
///
/// - **`404` on zero rows.** A PATCH naming a `model_pk` that does not exist used to return
///   `200 OK` having changed nothing; the same shape cycle 18 fixed for the price delete.
///
/// The `routing_strategy` sentinel is documented on [`PatchModelRequest`]: `null` means
/// "unchanged", and `""` means "clear to inherit".
pub(crate) async fn patch_admin_model_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ModelPk>,
    Json(payload): Json<PatchModelRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;

    // --- Validate before touching SQL, so a bad value is a 422 and not a 500 ---------------
    if let Some(distribution) = &payload.distribution
        && !MODEL_DISTRIBUTIONS.contains(&distribution.as_str())
    {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "invalid distribution: {distribution:?}; expected one of {}",
                MODEL_DISTRIBUTIONS.join(", ")
            ),
        ));
    }
    // `Some("")` is the explicit "clear to inherit" request; anything else must parse as a
    // real strategy. The parse error already lists the six valid names, so its message is
    // passed through rather than duplicated here.
    let routing_strategy: Option<Option<String>> = match payload.routing_strategy.as_deref() {
        None => None,
        Some("") => Some(None),
        Some(value) => match value.parse::<llmroller_core::strategy::RoutingStrategy>() {
            Ok(parsed) => Some(Some(parsed.as_str().to_string())),
            Err(error) => {
                return Err(map_price_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("invalid routing_strategy: {value:?}; {error}"),
                ));
            }
        },
    };

    let id_str = id.to_string();
    execute_atomic_mutation(&state, vec![ConfigDomain::Models], move |tx| {
        Box::pin(async move {
            // `COALESCE` needs a flag per field: `None` means "keep the stored value", and for
            // `routing_strategy` an explicit clear is `Some(None)`, which must write `NULL`
            // rather than fall through to the stored value. Two bound booleans express that
            // without a second statement.
            let set_routing = routing_strategy.is_some();
            let routing_value = routing_strategy.flatten();

            let res = sqlx::query(
                "UPDATE models SET
                     exposed = COALESCE(?, exposed),
                     distribution = COALESCE(?, distribution),
                     routing_strategy = CASE WHEN ? THEN ? ELSE routing_strategy END
                 WHERE model_pk = ?",
            )
            .bind(payload.exposed)
            .bind(payload.distribution.as_deref())
            .bind(set_routing)
            .bind(routing_value.as_deref())
            .bind(&id_str)
            .execute(&mut **tx)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

            if res.rows_affected() == 0 {
                return Err(map_price_error(StatusCode::NOT_FOUND, "model not found"));
            }
            Ok(())
        })
    })
    .await?;
    Ok(StatusCode::OK)
}

pub(crate) async fn post_admin_model_confirm_merge_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ModelPk>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let id_str = id.to_string();
    execute_atomic_mutation(&state, vec![ConfigDomain::Models], move |tx| {
        Box::pin(async move {
            let res = sqlx::query("UPDATE models SET merge_state = 'confirmed' WHERE model_pk = ?")
                .bind(&id_str)
                .execute(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            if res.rows_affected() == 0 {
                return Err(map_price_error(StatusCode::NOT_FOUND, "model not found"));
            }
            sqlx::query("UPDATE model_aliases SET confirmed = 1 WHERE model_pk = ?")
                .bind(&id_str)
                .execute(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok(())
        })
    })
    .await?;
    Ok(StatusCode::OK)
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeleteAliasParams {
    pub alias: Option<String>,
}

/// `DELETE /admin/api/model-aliases?alias=…` — remove one confirmed alias.
///
/// Was a bare `DELETE` on the reader-facing pool (`ISSUE-0232`), which meant:
///
/// - **`204` even when zero rows matched**, so a caller deleting a typo'd alias was told it
///   worked;
/// - no `ConfigDomain::Models` publish, so the data plane kept resolving the deleted alias —
///   `SnapshotModel` aliases are projected into `ConfigSnapshot::aliases` and read by
///   `resolve_model`, so the stale entry is live, not cosmetic (`reloader.rs:167,197`);
/// - the write was not atomic with its audit decision.
///
/// Now the same shape as `patch_admin_model_handler` above: one transaction, `404` on zero
/// rows. (The audit row and publish come from `control_mutation_barrier` either way — verified,
/// see the cycle-33 report; what the atomic path adds is that the `rows_affected` decision and
/// the delete cannot disagree.)
pub(crate) async fn delete_admin_model_alias_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<DeleteAliasParams>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let Some(alias) = params.alias else {
        return Err(map_price_error(
            StatusCode::BAD_REQUEST,
            "missing alias parameter",
        ));
    };
    execute_atomic_mutation(&state, vec![ConfigDomain::Models], move |tx| {
        Box::pin(async move {
            let res = sqlx::query("DELETE FROM model_aliases WHERE alias = ?")
                .bind(&alias)
                .execute(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            if res.rows_affected() == 0 {
                return Err(map_price_error(StatusCode::NOT_FOUND, "alias not found"));
            }
            Ok(())
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The custom-provider admin-approval predicate, in one place (`ISSUE-0171`, `SHR-2`).
///
/// `crates/core/src/shares.rs::is_share_eligible` refuses a `custom` share whose
/// `admin_approved_at` is `NULL`, so the data plane cannot use one. Every display and write
/// path that offers a share to a grantee must apply the same rule, or the routing matrix
/// claims a route the data plane will refuse and the grantee cannot diagnose why.
///
/// A macro rather than a `const &str` because `sqlx::query_scalar` deliberately rejects
/// runtime-built SQL: pasting it with `concat!` keeps the string a compile-time literal, so
/// the injection lint stays satisfied and the predicate still exists once. Three sites need
/// it — two join `providers p`, one does not — so each caller supplies its own join.
macro_rules! share_approval_guard {
    () => {
        "AND (p.kind != 'custom' OR s.admin_approved_at IS NOT NULL)"
    };
}

pub(crate) async fn get_user_routes_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let model_rows = sqlx::query(
        "SELECT model_pk, canonical_id, origin, exposed, merge_state, distribution, routing_strategy
         FROM models WHERE (exposed IS NULL OR exposed != 0) ORDER BY canonical_id ASC",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut models = Vec::new();
    for m in model_rows {
        let model_pk: String = m.try_get::<String, _>("model_pk").unwrap_or_default();
        let canonical_id: String = m.try_get::<String, _>("canonical_id").unwrap_or_default();
        let distribution: String = m.try_get::<String, _>("distribution").unwrap_or_default();
        let routing_strategy: String = m
            .try_get::<Option<String>, _>("routing_strategy")
            .unwrap_or_default()
            .unwrap_or_else(|| "smooth_weighted_rr".to_string());

        let route_rows = sqlx::query(
            "SELECT r.upstream_key_id, r.priority_override as route_prio_override,
                    k.priority, k.strict_tier,
                    k.label as key_label, k.owner_user_id, p.ownership_scope,
                    p.id as provider_id,
                    p.name as provider_name, p.kind as provider_kind
             FROM model_routes r
             JOIN upstream_keys k ON r.upstream_key_id = k.id
             JOIN providers p ON k.provider_id = p.id
             WHERE r.model_pk = ? AND k.enabled = 1 AND p.enabled = 1 AND r.enabled = 1",
        )
        .bind(&model_pk)
        .fetch_all(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

        let pref_rows = sqlx::query(
            "SELECT upstream_key_id, priority_override, enabled
             FROM user_route_preferences
             WHERE user_id = ? AND model_pk = ?",
        )
        .bind(user_id.to_string())
        .bind(&model_pk)
        .fetch_all(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

        let mut prefs_map = std::collections::HashMap::new();
        for p in pref_rows {
            let kid: String = p
                .try_get::<String, _>("upstream_key_id")
                .unwrap_or_default();
            let prio: i64 = p.try_get::<i64, _>("priority_override").unwrap_or(100);
            let en: i64 = p.try_get::<i64, _>("enabled").unwrap_or(1);
            prefs_map.insert(kid, (prio, en == 1));
        }

        let mut own_keys = Vec::new();
        let mut shared_keys = Vec::new();
        let mut global_count = 0;

        for r in route_rows {
            let key_id: String = r
                .try_get::<String, _>("upstream_key_id")
                .unwrap_or_default();
            let scope: String = r
                .try_get::<String, _>("ownership_scope")
                .unwrap_or_default();
            let owner: Option<String> = r
                .try_get::<Option<String>, _>("owner_user_id")
                .unwrap_or_default();
            let label: String = r.try_get::<String, _>("key_label").unwrap_or_default();
            let provider_id: String = r.try_get::<String, _>("provider_id").unwrap_or_default();
            let priority: i64 = r.try_get::<i64, _>("priority").unwrap_or(100);
            let strict_tier: i64 = r.try_get::<i64, _>("strict_tier").unwrap_or(1);
            let weight: i64 = priority;

            let (user_prio, user_en) = prefs_map.get(&key_id).copied().unwrap_or((priority, true));

            if scope == "personal" && owner.as_deref() == Some(&user_id.to_string()) {
                own_keys.push(serde_json::json!({
                    "upstream_key_id": key_id,
                    "label": label,
                    "priority": priority,
                    "strict_tier": strict_tier,
                    "weight": weight,
                    "priority_override": user_prio,
                    "enabled": user_en,
                }));
            } else if scope == "global" {
                global_count += 1;
            } else {
                // A provider-level grant covers only keys of THAT provider, must be
                // accepted against the current fingerprint, and must still have an
                // active grantor (`SEC-17`, F042).
                let is_shared = sqlx::query_scalar::<_, i64>(concat!(
                    "SELECT 1 FROM provider_shares s
                     JOIN providers p ON p.id = s.provider_id
                     JOIN users u ON u.id = s.owner_user_id
                     WHERE s.grantee_user_id = ? AND s.provider_id = ?
                       AND s.enabled = 1 AND s.revoked_at IS NULL
                       AND s.accepted_at IS NOT NULL
                       AND s.accepted_config_fingerprint IS NOT NULL
                       AND s.accepted_config_fingerprint = p.share_config_fingerprint
                       AND u.disabled = 0
                       AND (s.upstream_key_id IS NULL OR s.upstream_key_id = ?)
                       AND (s.allowed_models IS NULL OR EXISTS (
                            SELECT 1 FROM json_each(s.allowed_models) WHERE value = ?))
                       ",
                    share_approval_guard!(),
                    "
                     LIMIT 1",
                ))
                .bind(user_id.to_string())
                .bind(&provider_id)
                .bind(&key_id)
                .bind(&canonical_id)
                .fetch_optional(&state.auth.reader_pool)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
                .is_some();

                if is_shared {
                    shared_keys.push(serde_json::json!({
                        "upstream_key_id": key_id,
                        "label": label,
                        "priority": priority,
                        "strict_tier": strict_tier,
                        "weight": weight,
                        "priority_override": user_prio,
                        "enabled": user_en,
                    }));
                }
            }
        }

        models.push(serde_json::json!({
            "model_pk": model_pk,
            "canonical_id": canonical_id,
            "distribution": distribution,
            "routing_strategy": routing_strategy,
            "own_keys": own_keys,
            "shared_keys": shared_keys,
            "global_pool": {
                "available": global_count > 0,
                "count": global_count,
                "label": "Global pool"
            }
        }));
    }

    Ok(Json(serde_json::json!({ "models": models })))
}

pub(crate) async fn put_user_route_preference_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path((key_id, canonical_id)): Path<(UpstreamKeyId, String)>,
    Json(payload): Json<PutRoutePreferenceRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let key_info = sqlx::query(
        "SELECT k.owner_user_id, p.id as provider_id, p.ownership_scope,
                p.share_config_fingerprint
         FROM upstream_keys k
         JOIN providers p ON k.provider_id = p.id
         WHERE k.id = ?",
    )
    .bind(key_id.to_string())
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "upstream key not found"))?;

    let scope: String = key_info.try_get("ownership_scope").unwrap_or_default();
    let owner_user_id: Option<String> = key_info.try_get("owner_user_id").unwrap_or_default();
    let provider_id: String = key_info.try_get("provider_id").unwrap_or_default();
    if scope == "global" && !actor.is_admin() {
        return Err(map_price_error(
            StatusCode::FORBIDDEN,
            "cannot configure route preference on global key",
        ));
    }
    let user_id_str = user_id.to_string();
    let is_owner = owner_user_id.as_deref() == Some(user_id_str.as_str());
    if scope == "personal" && !actor.is_admin() && !actor.is_system() && !is_owner {
        // The caller may only mutate a route preference on a personal key they
        // own or that was explicitly shared with them (`SEC-17`, `USR-3`, F043).
        // Unknown and unauthorized keys both return 404 to avoid an existence oracle.
        let share_fingerprint: String = key_info
            .try_get("share_config_fingerprint")
            .unwrap_or_default();
        let is_shared = sqlx::query_scalar::<_, i64>(concat!(
            "SELECT 1 FROM provider_shares s
             JOIN users u ON u.id = s.owner_user_id
             JOIN providers p ON p.id = s.provider_id
             WHERE s.grantee_user_id = ? AND s.provider_id = ?
               AND s.enabled = 1 AND s.revoked_at IS NULL
               AND s.accepted_at IS NOT NULL
               AND s.accepted_config_fingerprint IS NOT NULL
               AND s.accepted_config_fingerprint = ?
               AND u.disabled = 0
               AND (s.upstream_key_id IS NULL OR s.upstream_key_id = ?)
               ",
            share_approval_guard!(),
            "
             LIMIT 1",
        ))
        .bind(&user_id_str)
        .bind(&provider_id)
        .bind(&share_fingerprint)
        .bind(key_id.to_string())
        .fetch_optional(&state.auth.reader_pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .is_some();
        if !is_shared {
            return Err(map_price_error(
                StatusCode::NOT_FOUND,
                "upstream key not found",
            ));
        }
    }

    let model_pk: String = if let Some(m) =
        sqlx::query_scalar("SELECT model_pk FROM models WHERE canonical_id = ?")
            .bind(&canonical_id)
            .fetch_optional(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    {
        m
    } else if let Some(a) = sqlx::query_scalar("SELECT model_pk FROM model_aliases WHERE alias = ?")
        .bind(&canonical_id)
        .fetch_optional(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    {
        a
    } else {
        return Err(map_price_error(StatusCode::NOT_FOUND, "model not found"));
    };

    if scope == "personal" && !actor.is_admin() && !actor.is_system() && !is_owner {
        let canonical: String =
            sqlx::query_scalar("SELECT canonical_id FROM models WHERE model_pk = ?")
                .bind(&model_pk)
                .fetch_one(&state.auth.reader_pool)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let allowed = sqlx::query_scalar::<_, i64>(concat!(
            "SELECT 1 FROM provider_shares s
             JOIN providers p ON p.id = s.provider_id
             WHERE s.grantee_user_id = ? AND s.provider_id = ?
               AND s.enabled = 1 AND s.revoked_at IS NULL
               AND s.accepted_at IS NOT NULL
               AND (s.upstream_key_id IS NULL OR s.upstream_key_id = ?)
               AND (s.allowed_models IS NULL OR EXISTS (
                    SELECT 1 FROM json_each(s.allowed_models) WHERE value = ?))
               ",
            share_approval_guard!(),
            "
             LIMIT 1",
        ))
        .bind(&user_id_str)
        .bind(&provider_id)
        .bind(key_id.to_string())
        .bind(&canonical)
        .fetch_optional(&state.auth.reader_pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .is_some();
        if !allowed {
            return Err(map_price_error(
                StatusCode::NOT_FOUND,
                "model is not shared with this user",
            ));
        }
    }

    let priority_override = payload.priority_override.unwrap_or(100).clamp(1, 1000) as i64;
    let enabled = if payload.enabled.unwrap_or(true) {
        1i64
    } else {
        0i64
    };

    sqlx::query(
        "INSERT INTO user_route_preferences (user_id, upstream_key_id, model_pk, priority_override, enabled)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (user_id, upstream_key_id, model_pk) DO UPDATE SET
            priority_override = excluded.priority_override,
            enabled = excluded.enabled",
    )
    .bind(user_id.to_string())
    .bind(key_id.to_string())
    .bind(model_pk)
    .bind(priority_override)
    .bind(enabled)
    .execute(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// Self-Service Security Endpoints
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub(crate) struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VerifyTotpRequest {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DisableTotpRequest {
    pub password: Option<String>,
}
