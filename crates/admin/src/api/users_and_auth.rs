use super::*;
use sqlx::Row as _;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct UpdatePreferencesRequest {
    pub ui_theme: Option<String>,
    pub ui_nav_layout: Option<String>,
    pub ui_locale: Option<String>,
    pub default_routing_scope: Option<String>,
}

/// `GET /api/user/preferences` (`UI-5`, `UI-6`).
pub(crate) async fn get_user_preferences_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let row = sqlx::query(
        "SELECT ui_theme, ui_nav_layout, ui_locale, default_routing_scope FROM users WHERE id = ?",
    )
    .bind(user_id.to_string())
    .fetch_optional(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "user not found"))?;

    Ok(Json(serde_json::json!({
        "ui_theme": row.try_get::<String, _>("ui_theme").unwrap_or_else(|_| "dark".to_string()),
        "ui_nav_layout": row.try_get::<String, _>("ui_nav_layout").unwrap_or_else(|_| "top_tabs".to_string()),
        "ui_locale": row.try_get::<String, _>("ui_locale").unwrap_or_else(|_| "en".to_string()),
        "default_routing_scope": row.try_get::<String, _>("default_routing_scope").unwrap_or_else(|_| "personal_first".to_string()),
    })))
}

/// `PATCH /api/user/preferences` (`UI-5`, `UI-6`).
pub(crate) async fn patch_user_preferences_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdatePreferencesRequest>,
) -> Result<Json<serde_json::Value>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    if let Some(theme) = &payload.ui_theme
        && !matches!(theme.as_str(), "dark" | "light" | "system")
    {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid ui_theme: {theme:?}"),
        ));
    }
    if let Some(layout) = &payload.ui_nav_layout
        && !matches!(layout.as_str(), "top_tabs" | "sidebar")
    {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid ui_nav_layout: {layout:?}"),
        ));
    }
    if let Some(scope) = &payload.default_routing_scope
        && !matches!(
            scope.as_str(),
            "personal_first" | "blended" | "personal_only" | "own_only" | "global_only"
        )
    {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid default_routing_scope: {scope:?}"),
        ));
    }
    if let Some(locale) = &payload.ui_locale
        && !matches!(locale.as_str(), "en" | "id" | "zh-CN")
    {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("unsupported ui_locale: {locale:?}"),
        ));
    }

    // `default_routing_scope` is not presentation: the data plane's `resolve_caller_scope`
    // reads `snapshot.users[..].default_routing_scope`, so a change must reach the snapshot
    // or the caller keeps the old scope until something unrelated bumps it (`CLK-3`).
    // The three UI fields need no publish at all (`ISSUE-0182`).
    //
    // The split has to be at the *writer* boundary, not the domain list.
    // `execute_single_control_task` bumps `config_version` unconditionally in the same
    // transaction, so routing a UI-only update through `execute_atomic_mutation` (even with
    // an empty domain list) still rebuilds. A UI-only update therefore writes directly and
    // marks the mutation handled so the control-mutation barrier does not publish for it
    // either — the barrier is what was doing it, because `/api/user/preferences` matches no
    // `ConfigDomain` in `domains_for_path` and the empty result means "rebuild everything".
    let ui_only = payload.default_routing_scope.is_none();
    let theme = payload.ui_theme.clone();
    let layout = payload.ui_nav_layout.clone();
    let locale = payload.ui_locale.clone();
    let scope_change = payload.default_routing_scope.clone();
    let user_id_str = user_id.to_string();

    if ui_only {
        write_user_preferences(&state, &user_id_str, theme, layout, locale, scope_change).await?;
        // Suppress the barrier's fallback publish: nothing in this payload changes a
        // snapshot slice.
        let _ = MUTATION_HANDLED.try_with(|flag| {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    } else {
        execute_atomic_mutation(&state, vec![ConfigDomain::Users], move |tx| {
            Box::pin(async move {
                write_user_preferences_tx(tx, &user_id_str, theme, layout, locale, scope_change)
                    .await
            })
        })
        .await?;
    }
    Ok(Json(serde_json::json!({
        "status": "updated",
        "ui_theme": payload.ui_theme,
        "ui_nav_layout": payload.ui_nav_layout,
        "ui_locale": payload.ui_locale,
        "default_routing_scope": payload.default_routing_scope,
    })))
}

async fn write_user_preferences_tx(
    tx: &mut Transaction<'_, Sqlite>,
    user_id: &str,
    theme: Option<String>,
    layout: Option<String>,
    locale: Option<String>,
    scope: Option<String>,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    sqlx::query(UPDATE_PREFERENCES_SQL)
        .bind(theme.as_deref())
        .bind(layout.as_deref())
        .bind(locale.as_deref())
        .bind(scope.as_deref())
        .bind(user_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(())
}

/// Apply a preferences update outside the writer, for the presentation-only case
/// (`ISSUE-0182`). Kept adjacent to [`write_user_preferences_tx`] so the two statements
/// cannot drift.
async fn write_user_preferences(
    state: &ApiAppState,
    user_id: &str,
    theme: Option<String>,
    layout: Option<String>,
    locale: Option<String>,
    scope: Option<String>,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    sqlx::query(UPDATE_PREFERENCES_SQL)
        .bind(theme.as_deref())
        .bind(layout.as_deref())
        .bind(locale.as_deref())
        .bind(scope.as_deref())
        .bind(user_id)
        .execute(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(())
}

const UPDATE_PREFERENCES_SQL: &str = "UPDATE users SET
        ui_theme = COALESCE(?, ui_theme),
        ui_nav_layout = COALESCE(?, ui_nav_layout),
        ui_locale = COALESCE(?, ui_locale),
        default_routing_scope = COALESCE(?, default_routing_scope)
     WHERE id = ?";

/// `GET /api/user/web-sessions` (`AUTH-7`, `F011`).
pub(crate) async fn get_user_web_sessions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();

    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let current_token =
        crate::auth::session_token_from_cookie_header(cookie_header).unwrap_or_default();
    let current_hash = llmroller_store::crypto::sha256_hex(current_token.as_bytes());

    let rows = sqlx::query(
        "SELECT id_hash, ip_addr, user_agent, created_at, last_seen_at, expires_at
         FROM web_sessions WHERE user_id = ? AND expires_at > ?
         ORDER BY last_seen_at DESC LIMIT 100",
    )
    .bind(user_id.to_string())
    .bind(now_ms)
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(
        rows.iter()
            .map(|row| {
                let id_hash: String = row.try_get("id_hash").unwrap_or_default();
                serde_json::json!({
                    "id_prefix": id_hash.chars().take(12).collect::<String>(),
                    "ip_addr": row.try_get::<Option<String>, _>("ip_addr").unwrap_or_default(),
                    "user_agent": row.try_get::<Option<String>, _>("user_agent").unwrap_or_default(),
                    "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
                    "last_seen_at": row.try_get::<i64, _>("last_seen_at").unwrap_or_default(),
                    "expires_at": row.try_get::<i64, _>("expires_at").unwrap_or_default(),
                    "is_current": id_hash == current_hash,
                })
            })
            .collect(),
    ))
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct AuditLogQuery {
    pub action: Option<String>,
    pub target_type: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub cursor: Option<i64>,
    pub limit: Option<i64>,
}

pub(crate) async fn get_admin_users_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let users = llmroller_store::users::list_users(&state.auth.pool, &actor)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let json_list: Vec<serde_json::Value> = users
        .into_iter()
        .map(|u| {
            serde_json::json!({
                "id": u.id.to_string(),
                "username": u.username,
                "role": u.role,
                "can_use_global_providers": u.can_use_global_providers,
                "global_budget_micro_usd_month": u.global_budget_micro_usd_month,
                "disabled": u.disabled,
                "created_at": u.created_at,
            })
        })
        .collect();

    Ok(Json(json_list))
}

pub(crate) async fn post_admin_user_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let role = match payload.role.as_deref().unwrap_or("user") {
        "admin" => llmroller_core::rbac::Role::Admin,
        _ => llmroller_core::rbac::Role::User,
    };

    let _permit = state
        .auth
        .argon2_semaphore
        .acquire()
        .await
        .map_err(|_| map_price_error(StatusCode::SERVICE_UNAVAILABLE, "server busy"))?;
    let user = llmroller_store::users::create_user(
        &state.auth.pool,
        &actor,
        llmroller_store::users::NewUser {
            username: &payload.username,
            password: &payload.password,
            role,
            must_change_password: payload.must_change_password.unwrap_or(false),
        },
        state.auth.entropy.as_ref(),
        now_ms,
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": user.id.to_string(),
            "username": user.username,
            "role": user.role,
        })),
    ))
}

pub(crate) async fn patch_admin_user_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UserId>,
    Json(payload): Json<PatchUserRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();

    let role = payload.role.as_deref().map(|role_str| match role_str {
        "admin" => llmroller_core::rbac::Role::Admin,
        _ => llmroller_core::rbac::Role::User,
    });
    let patch = llmroller_store::users::UserPatch {
        role,
        disabled: payload.disabled,
        can_use_global_providers: payload.can_use_global_providers,
        global_budget_micro_usd_month: payload.global_budget_micro_usd_month,
    };

    let user_res = execute_atomic_mutation(&state, vec![ConfigDomain::Users], move |tx| {
        Box::pin(async move {
            llmroller_store::users::patch_user_tx(tx, &actor, id, patch, now_ms)
                .await
                .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;
            Ok(())
        })
    })
    .await;

    user_res.map(|_| StatusCode::OK)
}

pub(crate) async fn delete_admin_user_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UserId>,
    axum::extract::Query(params): axum::extract::Query<DeleteUserParams>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let confirm = params.confirm.unwrap_or_default();

    llmroller_store::users::delete_user(&state.auth.pool, &actor, id, &confirm, now_ms)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    if headers.contains_key("hx-request") {
        Ok(StatusCode::OK)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

/// `POST /admin/api/users/{id}/totp/reset` (`AUTH-9`, `SEC-4`).
pub(crate) async fn post_admin_reset_totp_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UserId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;

    execute_atomic_mutation(&state, vec![ConfigDomain::Users], move |tx| {
        Box::pin(async move {
            llmroller_store::users::clear_totp_tx(tx, &actor, id)
                .await
                .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;
            sqlx::query("DELETE FROM web_sessions WHERE user_id = ?")
                .bind(id.to_string())
                .execute(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok(())
        })
    })
    .await?;

    Ok(Json(
        serde_json::json!({ "reset": true, "user_id": id.to_string() }),
    ))
}

#[derive(Debug, Deserialize)]
pub(crate) struct AdminResetPasswordRequest {
    pub temporary_password: Option<String>,
}

/// `POST /admin/api/users/{id}/reset-password` (`AUTH-4`, `SEC-5`).
pub(crate) async fn post_admin_reset_password_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UserId>,
    Json(payload): Json<AdminResetPasswordRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;

    let temp_password = payload.temporary_password.unwrap_or_else(|| {
        let mut bytes = [0u8; 16];
        state.auth.entropy.fill_bytes(&mut bytes);
        let hex_suffix = llmroller_store::crypto::sha256_hex(&bytes);
        format!("Temp-{}", &hex_suffix[..16])
    });

    let _permit = state
        .auth
        .argon2_semaphore
        .acquire()
        .await
        .map_err(|_| map_price_error(StatusCode::SERVICE_UNAVAILABLE, "server busy"))?;
    let entropy = state.auth.entropy.clone();
    let pw_clone = temp_password.clone();
    let password_hash = tokio::task::spawn_blocking(move || {
        llmroller_store::users::hash_password(&pw_clone, entropy.as_ref())
    })
    .await
    .map_err(|_| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, "spawn_blocking failed"))?
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;

    execute_atomic_mutation(&state, vec![ConfigDomain::Users], move |tx| {
        Box::pin(async move {
            let res = sqlx::query(
                "UPDATE users SET password_hash = ?, must_change_password = 1 WHERE id = ?",
            )
            .bind(password_hash)
            .bind(id.to_string())
            .execute(&mut **tx)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

            if res.rows_affected() == 0 {
                return Err(map_price_error(StatusCode::NOT_FOUND, "user not found"));
            }

            sqlx::query("DELETE FROM web_sessions WHERE user_id = ?")
                .bind(id.to_string())
                .execute(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok(())
        })
    })
    .await?;

    Ok(Json(serde_json::json!({
        "status": "reset",
        "user_id": id.to_string(),
        "temporary_password": temp_password,
    })))
}

// ---------------------------------------------------------------------------
// Model Catalog & Aliases (/admin/api/models)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub(crate) struct AddAliasRequest {
    pub alias: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PatchModelRequest {
    pub exposed: Option<i64>,
}

pub(crate) async fn post_user_password_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<ChangePasswordRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor_internal(&state, &headers, true, true).await?;

    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session_token =
        crate::auth::session_token_from_cookie_header(cookie_header).unwrap_or_default();
    let id_hash = llmroller_store::crypto::sha256_hex(session_token.as_bytes());

    let _permit = state
        .auth
        .argon2_semaphore
        .acquire()
        .await
        .map_err(|_| map_price_error(StatusCode::SERVICE_UNAVAILABLE, "server busy"))?;
    llmroller_store::users::change_password(
        &state.auth.pool,
        &actor,
        &payload.current_password,
        &payload.new_password,
        state.auth.entropy.as_ref(),
        Some(&id_hash),
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok(StatusCode::OK)
}

/// Build the sealer for this instance, or error if `instance_meta.instance_id` is absent/corrupt.
///
/// `instance_id` is the HKDF **salt** (`store/src/crypto.rs:303`), so substituting `[0u8; 16]` would
/// derive a different key and make every previously sealed row unreadable (`ISSUE-0229`). Public so
/// integration tests can assert the failure rather than only the handlers' status codes.
pub async fn load_sealer(
    state: &ApiAppState,
) -> Result<llmroller_store::crypto::XChaCha20Poly1305Sealer, (StatusCode, Json<serde_json::Value>)>
{
    // `fetch_one`, not `fetch_optional`: a missing `instance_meta` row is a configuration fault and
    // must surface, never become a `[0u8; 16]` HKDF salt (`ISSUE-0229`).
    let instance_id_vec: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_one(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    // The schema guarantees `length(instance_id) = 16` (`0001_initial.sql:11`), so a mismatch means
    // the database was written by something that bypassed the constraint. Error, do not substitute.
    let instance_id: [u8; 16] = instance_id_vec.try_into().map_err(|_| {
        map_price_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "corrupt instance_id: expected 16 bytes",
        )
    })?;
    Ok(llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
        state.master_key.clone(),
        instance_id,
        state.auth.entropy.clone(),
    ))
}

pub(crate) async fn post_user_totp_enroll_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let sealer = load_sealer(&state).await?;

    let enrollment = llmroller_store::users::enroll_totp(
        &state.auth.pool,
        &actor,
        &sealer,
        &state.master_key_id,
        state.auth.entropy.as_ref(),
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    use llmroller_core::seams::ExposeSecret as _;
    let recovery_codes: Vec<&str> = enrollment
        .recovery_codes
        .iter()
        .map(|c| c.expose_secret())
        .collect();

    Ok(Json(serde_json::json!({
        "otpauth_url": enrollment.otpauth_url,
        "recovery_codes": recovery_codes,
    })))
}

pub(crate) async fn post_user_totp_verify_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<VerifyTotpRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let sealer = load_sealer(&state).await?;
    let now_ms = state.auth.clock.now_ms();

    if let Some(uid) = actor.user_id()
        && !state
            .auth
            .check_and_record_totp_replay(uid, &payload.code, now_ms)
    {
        return Err(map_price_error(
            StatusCode::UNAUTHORIZED,
            "TOTP code already used in this time window",
        ));
    }
    llmroller_store::users::confirm_totp(
        &state.auth.pool,
        &actor,
        &payload.code,
        &sealer,
        &state.master_key_id,
        now_ms,
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok(StatusCode::OK)
}

pub(crate) async fn post_user_totp_disable_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<DisableTotpRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    // Re-authentication is mandatory before removing a second factor (`AUTH-9`,
    // `SEC-4`). Argon2id verification is bounded by the SEC-4 semaphore and runs
    // off the Tokio worker threads (F098).
    let password = payload
        .password
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            map_price_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "current password is required to disable TOTP",
            )
        })?;
    let password_hash: Option<String> = sqlx::query("SELECT password_hash FROM users WHERE id = ?")
        .bind(user_id.to_string())
        .fetch_optional(&state.auth.reader_pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .map(|row| row.try_get("password_hash").unwrap_or_default());
    let password_hash =
        password_hash.ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "user not found"))?;

    let _permit = state
        .auth
        .argon2_semaphore
        .acquire()
        .await
        .map_err(|_| map_price_error(StatusCode::SERVICE_UNAVAILABLE, "server busy"))?;
    let verified = tokio::task::spawn_blocking(move || {
        llmroller_store::users::verify_password(&password, &password_hash)
    })
    .await
    .map_err(|_| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, "verification failed"))?;
    if !verified {
        return Err(map_price_error(
            StatusCode::UNAUTHORIZED,
            "invalid current password",
        ));
    }

    execute_atomic_mutation(&state, vec![ConfigDomain::Users], move |tx| {
        Box::pin(async move {
            llmroller_store::users::clear_totp_tx(tx, &actor, user_id)
                .await
                .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))
        })
    })
    .await?;

    Ok(StatusCode::OK)
}

pub(crate) async fn delete_user_session_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id_hash): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;

    llmroller_store::users::delete_owned_web_session(&state.auth.pool, &actor, &id_hash)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Upstream Key Reset Health (PRV-7, F-107)
// ---------------------------------------------------------------------------
