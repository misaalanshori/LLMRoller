//! Pass-through usage endpoints (/v1/usage, /v1/key, /v1/usage/raw).
//!
//! Spec: Section 13.4, Requirements: API-1, USG-7, USG-8, SEC-9, SEC-18.

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use llmroller_core::accumulators::BudgetScopeId;
use llmroller_core::ids::UpstreamKeyId;
use llmroller_core::rbac::{OwnershipScope, Role};
use llmroller_core::seams::{Clock as _, ExposeSecret as _, OsEntropy, SystemClock};
use llmroller_core::snapshot::{ConfigSnapshot, SnapshotBudgetWindow, UpstreamKeyRecord};
use llmroller_proto::usage_poller::{SanitizedUsageSnapshot, parse_raw_usage_to_sanitized};
use llmroller_store::crypto::{SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_store::usage_snapshots::{
    NewUsageSnapshot, insert_usage_snapshot_tx, seal_usage_snapshot,
};
use llmroller_store::writer::{DurableKey, DurableOp};
use serde::Deserialize;
use sqlx::Row as _;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::auth::ProxyAuthState;
use llmroller_core::client_key::ClientKeyRecord;
use llmroller_core::windows::BudgetCounter as _;

/// Minimum spacing between on-demand upstream `/usage` polls (SRDD 13.4).
const FRESH_POLL_MIN_INTERVAL_MS: i64 = 10_000;
/// Hard wait budget for `?fresh=1` (SRDD 13.4).
const FRESH_POLL_TIMEOUT: Duration = Duration::from_secs(3);
/// A cached upstream snapshot older than this is reported as stale. Three
/// background poll intervals tolerate one missed tick without flapping.
const STALE_AFTER_MS: i64 = 60_000;

#[derive(Debug, Deserialize)]
pub struct UsageQueryParams {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub upstream_key_id: Option<String>,
    pub fresh: Option<String>,
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": {
                "code": "not_found_error",
                "message": message
            }
        })),
    )
        .into_response()
}

fn internal_error(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": {
                "code": "internal_error",
                "message": message
            }
        })),
    )
        .into_response()
}

fn invalid_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": {
                "code": "invalid_request_error",
                "message": message
            }
        })),
    )
        .into_response()
}

fn is_admin(snapshot: &ConfigSnapshot, caller: &ClientKeyRecord) -> bool {
    snapshot
        .users
        .get(&caller.owner_user_id)
        .is_some_and(|user| user.role == Role::Admin)
}

/// Whether `key_id` is visible to the caller as Own, Shared, or Global (SEC-12, SEC-17).
fn accessible_key<'a>(
    snapshot: &'a ConfigSnapshot,
    key_id: &UpstreamKeyId,
    caller: &ClientKeyRecord,
) -> Option<&'a UpstreamKeyRecord> {
    let key = snapshot.upstream_keys.get(key_id)?;
    let provider = snapshot.providers.get(&key.provider_id)?;
    match provider.ownership_scope {
        OwnershipScope::Global => Some(key),
        OwnershipScope::Personal if provider.owner_user_id == Some(caller.owner_user_id) => {
            Some(key)
        }
        OwnershipScope::Personal => {
            let grantor_disabled = provider
                .owner_user_id
                .is_none_or(|owner| snapshot.users.get(&owner).is_none_or(|user| user.disabled));
            snapshot
                .effective_share(
                    caller.owner_user_id,
                    provider.id,
                    key.id,
                    provider.kind,
                    &provider.share_config_fingerprint,
                    grantor_disabled,
                )
                .map(|_| key)
        }
    }
}

/// Resolve the upstream key whose windows a usage response should describe.
#[derive(Debug)]
struct UsageKeyNotFound(String);

fn resolve_usage_key(
    snapshot: &ConfigSnapshot,
    caller: &ClientKeyRecord,
    params: &UsageQueryParams,
    session_hash: Option<&str>,
    pin_store: Option<&llmroller_core::pins::PinStore>,
    now: i64,
) -> Result<Option<UpstreamKeyId>, UsageKeyNotFound> {
    if let Some(raw) = &params.upstream_key_id {
        let Ok(key_id) = UpstreamKeyId::parse(raw) else {
            return Err(UsageKeyNotFound(format!("upstream key '{raw}' not found")));
        };
        return match accessible_key(snapshot, &key_id, caller) {
            // `USG-7`, `USR-4`, `SEC-16`: global keys belong to the shared pool.
            // Returning their usage to a standard user would let them probe
            // arbitrary global key ULIDs (existence and spend), so explicit
            // global-key lookups are admin-only.
            Some(key)
                if snapshot
                    .providers
                    .get(&key.provider_id)
                    .is_some_and(|provider| provider.ownership_scope == OwnershipScope::Global)
                    && !is_admin(snapshot, caller) =>
            {
                Err(UsageKeyNotFound(format!("upstream key '{raw}' not found")))
            }
            Some(key) => Ok(Some(key.id)),
            None => Err(UsageKeyNotFound(format!("upstream key '{raw}' not found"))),
        };
    }

    // Step 2: Active session pin (Section 13.4, USG-7)
    if let (Some(sess_hash), Some(store)) = (session_hash, pin_store)
        && let Some(pinned_key) =
            store.find_active_pin(&caller.id, sess_hash, params.model.as_deref(), now)
        && let Some(key) = accessible_key(snapshot, &pinned_key, caller)
    {
        return Ok(Some(key.id));
    }

    let scope = crate::orchestrator::parse_scope(&caller.routing_scope);
    if let Some(raw_model) = &params.model {
        let model = snapshot
            .resolve_model(raw_model)
            .map_err(|_| UsageKeyNotFound(format!("model '{raw_model}' not found")))?;
        if let Some(allowed) = &caller.allowed_models
            && !allowed.iter().any(|entry| entry == &model.canonical_id)
        {
            return Err(UsageKeyNotFound(format!("model '{raw_model}' not found")));
        }
        let mut candidates =
            crate::orchestrator::collect_candidates(snapshot, model, caller, scope, now);
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.priority()));
        return Ok(candidates
            .first()
            .map(|candidate| candidate.upstream_key_id()));
    }

    // Default: the caller's highest-priority route across every visible model.
    let mut best: Option<(i64, UpstreamKeyId)> = None;
    for model in snapshot.models.values() {
        if model.exposed == Some(false) {
            continue;
        }
        if let Some(allowed) = &caller.allowed_models
            && !allowed.iter().any(|entry| entry == &model.canonical_id)
        {
            continue;
        }
        for candidate in
            crate::orchestrator::collect_candidates(snapshot, model, caller, scope, now)
        {
            if best.is_none_or(|(priority, _)| candidate.priority() > priority) {
                best = Some((candidate.priority(), candidate.upstream_key_id()));
            }
        }
    }
    Ok(best.map(|(_, key_id)| key_id))
}

/// Build live budget-window gauges for one upstream key.
fn usage_windows(
    state: &ProxyAuthState,
    snapshot: &ConfigSnapshot,
    key_id: Option<UpstreamKeyId>,
    now: i64,
) -> Vec<serde_json::Value> {
    let Some(key_id) = key_id else {
        return Vec::new();
    };
    let windows: &[SnapshotBudgetWindow] = snapshot
        .budget_windows
        .get(&key_id)
        .map_or(&[], Vec::as_slice);
    windows
        .iter()
        .filter_map(|window| {
            let counter = state.runtime_state.window_counter(&window.id)?;
            let used = counter.effective_used(now);
            let limit = counter.limit_micro_usd();
            let used_percent = if limit > 0 {
                (used as f64) * 100.0 / (limit as f64)
            } else {
                0.0
            };
            let status = if limit > 0 && used >= limit {
                "exhausted"
            } else if limit > 0 && used.saturating_mul(100) >= limit.saturating_mul(90) {
                "warning"
            } else {
                "ok"
            };
            Some(serde_json::json!({
                "id": window.id,
                "kind": window.window_kind,
                "status": status,
                "used_percent": used_percent,
                "used_micro_usd": used,
                "limit_micro_usd": limit
            }))
        })
        .collect()
}

fn usage_endpoint(config_json: &str) -> Option<String> {
    let config: serde_json::Value = serde_json::from_str(config_json).ok()?;
    if let Some(ep) = config
        .get("usage_endpoint")
        .or_else(|| config.get("usage_path"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        return Some(ep.to_string());
    }
    if let Some(ep) = config
        .get("usage_profile")
        .and_then(|up| up.get("endpoints"))
        .and_then(|arr| arr.as_array())
        .and_then(|arr| arr.first())
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        return Some(ep.to_string());
    }
    config
        .get("paths")
        .and_then(|p| p.get("usage"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// Resolves the full upstream `/usage` URL for a provider without duplicating `/api/v1` prefixes (`USG-6`, `USG-7`, `PRV-1`).
#[must_use]
pub fn resolve_provider_usage_url(
    base_url: &str,
    kind: llmroller_core::provider::ProviderKind,
    config_json: &str,
) -> Option<String> {
    let base = base_url.trim_end_matches('/');
    let raw_ep = usage_endpoint(config_json).or_else(|| match kind {
        llmroller_core::provider::ProviderKind::Openrouter => Some("/key".to_string()),
        llmroller_core::provider::ProviderKind::OpencodeGo
        | llmroller_core::provider::ProviderKind::OpenDesignAmr => Some("/usage".to_string()),
        llmroller_core::provider::ProviderKind::CommandCode => {
            Some("/alpha/billing/credits".to_string())
        }
        llmroller_core::provider::ProviderKind::Custom => None,
    })?;
    if raw_ep.starts_with("http://") || raw_ep.starts_with("https://") {
        return Some(raw_ep);
    }
    let rel = raw_ep.trim_start_matches('/');
    if base.ends_with("/api/v1") && rel.starts_with("api/v1/") {
        return Some(format!("{}/{}", base.trim_end_matches("/api/v1"), rel));
    }
    Some(format!("{base}/{rel}"))
}

async fn load_instance_id(state: &ProxyAuthState) -> Result<[u8; 16], String> {
    if let Some(instance_id) = state.instance_id {
        return Ok(instance_id);
    }
    let bytes: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_one(&state.reader_pool)
            .await
            .map_err(|error| format!("cannot read instance id: {error}"))?;
    bytes
        .try_into()
        .map_err(|_| "stored instance id is not 16 bytes".to_string())
}

/// One stored upstream usage snapshot joined with its provider ownership.
struct UsageSnapshotRow {
    key_id: UpstreamKeyId,
    snap_id: String,
    master_key_id: String,
    raw_json_enc: Vec<u8>,
    sanitized_json: String,
    fetched_at: i64,
    http_status: i64,
    ownership_scope: String,
    owner_user_id: Option<String>,
}

fn parse_usage_row(row: &sqlx::sqlite::SqliteRow) -> Result<UsageSnapshotRow, String> {
    let key_raw: String = row
        .try_get("upstream_key_id")
        .map_err(|error| format!("snapshot key is invalid: {error}"))?;
    let key_id = UpstreamKeyId::parse(&key_raw)
        .map_err(|_| "stored snapshot references an invalid upstream key".to_string())?;
    Ok(UsageSnapshotRow {
        key_id,
        snap_id: row
            .try_get("snap_id")
            .map_err(|error| format!("snapshot id is invalid: {error}"))?,
        master_key_id: row
            .try_get("master_key_id")
            .map_err(|error| format!("snapshot master key id is invalid: {error}"))?,
        raw_json_enc: row
            .try_get("raw_json_enc")
            .map_err(|error| format!("snapshot ciphertext is invalid: {error}"))?,
        sanitized_json: row
            .try_get("sanitized_json")
            .map_err(|error| format!("snapshot sanitized JSON is invalid: {error}"))?,
        fetched_at: row
            .try_get("fetched_at")
            .map_err(|error| format!("snapshot timestamp is invalid: {error}"))?,
        http_status: row
            .try_get("http_status")
            .map_err(|error| format!("snapshot HTTP status is invalid: {error}"))?,
        ownership_scope: row
            .try_get("ownership_scope")
            .map_err(|error| format!("provider ownership is invalid: {error}"))?,
        owner_user_id: row
            .try_get("owner_user_id")
            .map_err(|error| format!("provider owner is invalid: {error}"))?,
    })
}

const SNAPSHOT_FOR_KEY: &str = r#"SELECT s.id AS snap_id, s.upstream_key_id, s.master_key_id,
              s.raw_json_enc, s.sanitized_json, s.fetched_at, s.http_status,
              p.ownership_scope, p.owner_user_id
       FROM upstream_usage_snapshots s
       JOIN upstream_keys k ON s.upstream_key_id = k.id
       JOIN providers p ON k.provider_id = p.id
       WHERE s.upstream_key_id = ?
       ORDER BY s.fetched_at DESC, s.id DESC LIMIT 1"#;

const SNAPSHOT_RECENT: &str = r#"SELECT s.id AS snap_id, s.upstream_key_id, s.master_key_id,
              s.raw_json_enc, s.sanitized_json, s.fetched_at, s.http_status,
              p.ownership_scope, p.owner_user_id
       FROM upstream_usage_snapshots s
       JOIN upstream_keys k ON s.upstream_key_id = k.id
       JOIN providers p ON k.provider_id = p.id
       ORDER BY s.fetched_at DESC, s.id DESC LIMIT 200"#;

/// Load the newest snapshot visible to the caller, optionally restricted to one key.
async fn load_usage_snapshot(
    state: &ProxyAuthState,
    snapshot: &ConfigSnapshot,
    caller: &ClientKeyRecord,
    key_id: Option<UpstreamKeyId>,
) -> Result<Option<UsageSnapshotRow>, String> {
    let rows = if let Some(key_id) = key_id {
        sqlx::query(SNAPSHOT_FOR_KEY)
            .bind(key_id.to_string())
            .fetch_all(&state.reader_pool)
            .await
            .map_err(|error| format!("cannot query usage snapshots: {error}"))?
    } else {
        sqlx::query(SNAPSHOT_RECENT)
            .fetch_all(&state.reader_pool)
            .await
            .map_err(|error| format!("cannot query usage snapshots: {error}"))?
    };

    for row in rows {
        let parsed = parse_usage_row(&row)?;
        if accessible_key(snapshot, &parsed.key_id, caller).is_some() {
            return Ok(Some(parsed));
        }
    }
    Ok(None)
}

fn raw_usage_allowed(
    snapshot: &ConfigSnapshot,
    caller: &ClientKeyRecord,
    row: &UsageSnapshotRow,
) -> bool {
    is_admin(snapshot, caller)
        || (row.ownership_scope == "personal"
            && row
                .owner_user_id
                .as_ref()
                .is_some_and(|owner| owner == &caller.owner_user_id.to_string()))
}

/// Role-scoped source payload: decrypted raw for Own/admin, allow-list sanitized otherwise.
fn render_source_usage(
    state: &ProxyAuthState,
    snapshot: &ConfigSnapshot,
    caller: &ClientKeyRecord,
    row: &UsageSnapshotRow,
    instance_id: [u8; 16],
) -> Result<serde_json::Value, String> {
    if raw_usage_allowed(snapshot, caller, row) {
        let master_key = state
            .master_key
            .clone()
            .ok_or_else(|| "master key is unavailable for raw usage".to_string())?;
        let sealer = XChaCha20Poly1305Sealer::new(master_key, instance_id, Arc::new(OsEntropy));
        let decrypted = sealer
            .open(
                "usage_raw",
                &[row.master_key_id.as_bytes(), row.snap_id.as_bytes()],
                &row.raw_json_enc,
            )
            .map_err(|_| "cannot decrypt stored upstream usage snapshot".to_string())?;
        return serde_json::from_slice(decrypted.expose_secret())
            .map_err(|error| format!("stored raw usage JSON is invalid: {error}"));
    }

    serde_json::from_str(&row.sanitized_json)
        .map_err(|error| format!("stored sanitized usage JSON is invalid: {error}"))
}

fn fresh_poll_allowed(
    snapshot: &ConfigSnapshot,
    caller: &ClientKeyRecord,
    key: &UpstreamKeyRecord,
) -> bool {
    if is_admin(snapshot, caller) {
        return true;
    }
    snapshot
        .providers
        .get(&key.provider_id)
        .is_some_and(|provider| {
            provider.ownership_scope == OwnershipScope::Personal
                && provider.owner_user_id == Some(caller.owner_user_id)
        })
}

/// Fetch one upstream `/usage` response, persist it, reconcile counters, and publish it
/// for the usage endpoint. Bounded to the 3 s `?fresh=1` wait budget (USG-7).
async fn fresh_poll_usage(
    state: &ProxyAuthState,
    snapshot: &ConfigSnapshot,
    key: &UpstreamKeyRecord,
    now_ms: i64,
) -> Result<SanitizedUsageSnapshot, String> {
    let provider = snapshot
        .providers
        .get(&key.provider_id)
        .ok_or_else(|| "upstream provider is missing".to_string())?;
    let target =
        resolve_provider_usage_url(&provider.base_url, provider.kind, &provider.config_json)
            .ok_or_else(|| "provider has no usage endpoint".to_string())?;
    let master_key = state
        .master_key
        .clone()
        .ok_or_else(|| "master key is unavailable for usage polling".to_string())?;
    let instance_id = load_instance_id(state).await?;
    let sealer = XChaCha20Poly1305Sealer::new(master_key, instance_id, Arc::new(OsEntropy));

    let secret = sealer
        .open(
            "upstream_key",
            &[
                key.master_key_id.as_bytes(),
                key.id.to_string().as_bytes(),
                key.provider_id.to_string().as_bytes(),
            ],
            &key.secret_enc,
        )
        .map_err(|_| "cannot unseal upstream key for usage polling".to_string())?;
    let secret = String::from_utf8_lossy(secret.expose_secret()).to_string();

    crate::ssrf::validate_url(&target, provider.allow_private_network)
        .map_err(|_| "usage endpoint is not allowed".to_string())?;

    let response = state
        .select_http_client(provider.allow_private_network)
        .get(&target)
        .header("authorization", format!("Bearer {secret}"))
        .timeout(FRESH_POLL_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("usage request failed: {error}"))?;
    let http_status = i64::from(response.status().as_u16());
    let raw_bytes = response
        .bytes()
        .await
        .map_err(|error| format!("usage response body failed: {error}"))?
        .to_vec();
    let mut raw_json: serde_json::Value = serde_json::from_slice(&raw_bytes)
        .map_err(|error| format!("usage response is not JSON: {error}"))?;
    if raw_json.get("now_ms").is_none()
        && let Some(object) = raw_json.as_object_mut()
    {
        object.insert("now_ms".to_string(), serde_json::json!(now_ms));
    }

    let parsed = parse_raw_usage_to_sanitized(provider.kind, &raw_json)
        .ok_or_else(|| "usage response is not a supported provider schema".to_string())?;
    let sanitized_json = serde_json::to_string(&parsed)
        .map_err(|error| format!("cannot serialize sanitized usage: {error}"))?;
    let windows_json = serde_json::to_string(&parsed.windows)
        .map_err(|error| format!("cannot serialize usage windows: {error}"))?;

    let new_snapshot = NewUsageSnapshot {
        key_id: &key.id,
        now_ms,
        http_status,
        plan_name: parsed.plan_name.as_deref(),
        windows_json: &windows_json,
        balance_micro_usd: parsed.balance_micro_usd,
        master_key_id: &key.master_key_id,
        sanitized_json: &sanitized_json,
        raw_json_bytes: &raw_bytes,
    };
    if let Some(writer) = &state.writer {
        let sealed = seal_usage_snapshot(&sealer, &new_snapshot);
        writer.queues.durable_state.lock().insert(
            DurableKey::UsageSnapshot(sealed.id.clone()),
            DurableOp::UsageSnapshotInsert(sealed),
        );
    } else {
        let mut tx = state
            .pool
            .begin()
            .await
            .map_err(|error| format!("cannot begin usage snapshot transaction: {error}"))?;
        insert_usage_snapshot_tx(&mut tx, &sealer, new_snapshot)
            .await
            .map_err(|error| format!("cannot store usage snapshot: {error}"))?;
        tx.commit()
            .await
            .map_err(|error| format!("cannot commit usage snapshot: {error}"))?;
    }

    let observations: Vec<llmroller_core::runtime_state::UpstreamWindowObservation> = parsed
        .windows
        .iter()
        .map(
            |window| llmroller_core::runtime_state::UpstreamWindowObservation {
                kind: window.kind.clone(),
                used_percent: window.used_percent,
                resets_at: window.resets_at,
            },
        )
        .collect();
    let empty: &[SnapshotBudgetWindow] = &[];
    let budget_windows = snapshot
        .budget_windows
        .get(&key.id)
        .map_or(empty, Vec::as_slice);
    state
        .runtime_state
        .reconcile_upstream_usage(key.id, budget_windows, &observations, now_ms);
    state.runtime_state.publish_upstream_snapshot(
        key.id,
        serde_json::json!({
            "fetched_at": now_ms,
            "sanitized": parsed,
        }),
    );

    Ok(parsed)
}

/// Handler for GET /v1/usage (USG-7).
pub async fn get_v1_usage_handler(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    Query(params): Query<UsageQueryParams>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    // Reject ?session_id= query parameter with HTTP 400 per SEC-9.
    if params.session_id.is_some() {
        return invalid_request(
            "session_id must be provided via request headers, not query parameters",
        );
    }

    let header_vec: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let (sess_id, _step) =
        llmroller_core::session::extract_session_precedence(&header_vec, &HashMap::new());
    let fallback = [0u8; 32];
    use llmroller_core::seams::ExposeSecret as _;
    let secret = state
        .session_hmac_secret
        .as_ref()
        .map(|s| s.expose_secret())
        .unwrap_or(&fallback);
    let session_hash = if !sess_id.is_empty() {
        Some(llmroller_core::session::compute_session_hash(
            secret,
            &client_key.owner_user_id,
            &sess_id,
        ))
    } else {
        None
    };

    let now = SystemClock::new().now_ms();
    let snapshot = state.snapshot.load();
    let target_key = match resolve_usage_key(
        &snapshot,
        &client_key,
        &params,
        session_hash.as_deref(),
        Some(&state.pin_store),
        now,
    ) {
        Ok(key) => key,
        Err(UsageKeyNotFound(message)) => return not_found(&message),
    };

    let mut stored = match load_usage_snapshot(&state, &snapshot, &client_key, target_key).await {
        Ok(row) => row,
        Err(error) => return internal_error(&error),
    };

    // `fresh=1` is honored only for an Own personal key or an admin, and only when
    // the stored snapshot is at least 10 s old (`FRESH_POLL_MIN_INTERVAL_MS`). For Shared
    // and Global keys the request silently uses the cached snapshot (Sections 13.4, 4).
    let want_fresh = matches!(params.fresh.as_deref(), Some("1" | "true"));
    let stored_is_recent = stored
        .as_ref()
        .is_some_and(|row| now.saturating_sub(row.fetched_at) < FRESH_POLL_MIN_INTERVAL_MS);
    if want_fresh
        && !stored_is_recent
        && let Some(key_id) = target_key
        && let Some(key) = snapshot.upstream_keys.get(&key_id)
        && fresh_poll_allowed(&snapshot, &client_key, key)
        && state
            .runtime_state
            .try_begin_fresh_poll(key_id, now, FRESH_POLL_MIN_INTERVAL_MS)
    {
        match fresh_poll_usage(&state, &snapshot, key, now).await {
            Ok(_) => {
                if let Ok(reloaded) =
                    load_usage_snapshot(&state, &snapshot, &client_key, target_key).await
                {
                    stored = reloaded;
                }
            }
            Err(error) => {
                tracing::warn!(upstream_key = %key_id, "on-demand usage poll failed: {error}");
            }
        }
    }
    let response_windows = usage_windows(&state, &snapshot, target_key, now);

    let (source, fetched_at, upstream_value) = if let Some(row) = &stored {
        ("upstream", row.fetched_at, serde_json::Value::Null)
    } else if let Some(key_id) = target_key
        && let Some(cached) = state.runtime_state.upstream_snapshot(key_id)
    {
        let fetched_at = cached
            .get("fetched_at")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(now);
        ("upstream", fetched_at, (*cached).clone())
    } else if target_key.is_some() {
        ("local", now, serde_json::Value::Null)
    } else {
        ("none", now, serde_json::Value::Null)
    };

    let source_provider_usage = if let Some(row) = &stored {
        let instance_id = match load_instance_id(&state).await {
            Ok(instance_id) => instance_id,
            Err(error) => return internal_error(&error),
        };
        match render_source_usage(&state, &snapshot, &client_key, row, instance_id) {
            Ok(value) => value,
            Err(error) => return internal_error(&error),
        }
    } else if !upstream_value.is_null() {
        upstream_value
            .get("sanitized")
            .cloned()
            .unwrap_or(upstream_value)
    } else {
        serde_json::Value::Null
    };

    let stale_ms = if source == "upstream" {
        now.saturating_sub(fetched_at).max(0)
    } else {
        0
    };
    let stale = source == "upstream" && stale_ms > STALE_AFTER_MS;

    let used_micro = state
        .runtime_state
        .accumulators
        .usage_for(&BudgetScopeId::ClientKey(client_key.id), now);

    let is_global_non_admin = target_key
        .and_then(|kid| snapshot.upstream_keys.get(&kid))
        .and_then(|uk| snapshot.providers.get(&uk.provider_id))
        .is_some_and(|prov| prov.ownership_scope == OwnershipScope::Global)
        && !is_admin(&snapshot, &client_key);
    let projected_upstream_key_id = if is_global_non_admin {
        None
    } else {
        target_key.map(|key| key.to_string())
    };
    let projected_pinned_route = target_key.map(|key| {
        if is_global_non_admin {
            serde_json::json!({
                "upstream_key_id": serde_json::Value::Null,
                "upstream_key_label": "global pool",
                "source": source
            })
        } else {
            serde_json::json!({
                "upstream_key_id": key.to_string(),
                "source": source
            })
        }
    });

    let mut response = (
        StatusCode::OK,
        Json(serde_json::json!({
            "object": "usage_snapshot",
            "source": source,
            "fetched_at": fetched_at,
            "stale": stale,
            "client_key_prefix": client_key.prefix,
            "session_id": sess_id,
            "upstream_key_id": projected_upstream_key_id,
            "pinned_route": projected_pinned_route,
            "windows": response_windows,
            "usage": {
                "windows": usage_windows(&state, &snapshot, target_key, now),
                "client_key": {
                    "monthly_cap_micro_usd": client_key.budget_micro_usd_month,
                    "used_this_month_micro_usd": used_micro
                }
            },
            "source_provider_usage": source_provider_usage,
            "credits": serde_json::Value::Null,
            "client_key_allowance": {
                "monthly_cap_micro_usd": client_key.budget_micro_usd_month,
                "used_this_month_micro_usd": used_micro
            }
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&stale_ms.to_string()) {
        response
            .headers_mut()
            .insert("x-llmroller-usage-stale-ms", value);
    }
    response
}

/// Handler for GET /v1/key and GET /v1/auth/key (USG-7).
pub async fn get_v1_key_handler(
    State(state): State<ProxyAuthState>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    let now = SystemClock::new().now_ms();
    let used_micro = state
        .runtime_state
        .accumulators
        .usage_for(&BudgetScopeId::ClientKey(client_key.id), now);
    let usage_usd = (used_micro as f64) / 1_000_000.0;
    let limit_usd = client_key
        .budget_micro_usd_month
        .map(|cap| (cap as f64) / 1_000_000.0);
    let limit_remaining_usd = limit_usd.map(|limit| (limit - usage_usd).max(0.0));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "object": "client_key_info",
            "id": client_key.id.to_string(),
            "client_key_id": client_key.id.to_string(),
            "label": client_key.label,
            "prefix": client_key.prefix,
            "routing_scope": client_key.routing_scope,
            "rate_limit_rpm": client_key.rate_limit_rpm,
            "budget_micro_usd_month": client_key.budget_micro_usd_month,
            "monthly_cap_micro_usd": client_key.budget_micro_usd_month,
            "used_this_month_micro_usd": used_micro,
            "data": {
                "label": client_key.prefix,
                "usage": usage_usd,
                "usage_daily": usage_usd,
                "usage_weekly": usage_usd,
                "usage_monthly": usage_usd,
                "limit": limit_usd,
                "limit_remaining": limit_remaining_usd,
                "limit_reset": if client_key.budget_micro_usd_month.is_some() {
                    serde_json::json!("monthly")
                } else {
                    serde_json::Value::Null
                },
                "is_free_tier": false,
                "rate_limit": {
                    "requests": client_key.rate_limit_rpm.unwrap_or(0),
                    "interval": "1m"
                },
                "llmroller": {
                    "used_this_month_micro_usd": used_micro,
                    "monthly_cap_micro_usd": client_key.budget_micro_usd_month,
                    "routing_scope": client_key.routing_scope
                }
            }
        })),
    )
        .into_response()
}

/// Handler for GET /v1/usage/raw (USG-8).
///
/// Always serves the newest stored snapshot, never a live fetch. Raw JSON is
/// returned only for Own personal keys and admins; Shared and Global keys get
/// the allow-list sanitized document.
pub async fn get_v1_usage_raw_handler(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    Query(params): Query<UsageQueryParams>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    if params.session_id.is_some() {
        return invalid_request(
            "session_id must be provided via request headers, not query parameters",
        );
    }

    let header_vec: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let (sess_id, _step) =
        llmroller_core::session::extract_session_precedence(&header_vec, &HashMap::new());
    let fallback = [0u8; 32];
    let secret = state
        .session_hmac_secret
        .as_ref()
        .map(|s| s.expose_secret())
        .unwrap_or(&fallback);
    let session_hash = if !sess_id.is_empty() {
        Some(llmroller_core::session::compute_session_hash(
            secret,
            &client_key.owner_user_id,
            &sess_id,
        ))
    } else {
        None
    };

    let now = SystemClock::new().now_ms();
    let snapshot = state.snapshot.load();
    let has_explicit_target =
        params.upstream_key_id.is_some() || params.model.is_some() || session_hash.is_some();
    let target_key = if has_explicit_target {
        match resolve_usage_key(
            &snapshot,
            &client_key,
            &params,
            session_hash.as_deref(),
            Some(&state.pin_store),
            now,
        ) {
            Ok(key) => key,
            Err(UsageKeyNotFound(message)) => return not_found(&message),
        }
    } else {
        None
    };

    let row = match load_usage_snapshot(&state, &snapshot, &client_key, target_key).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return not_found("no stored upstream usage snapshot is available for this client key");
        }
        Err(error) => return internal_error(&error),
    };

    let instance_id = match load_instance_id(&state).await {
        Ok(instance_id) => instance_id,
        Err(error) => return internal_error(&error),
    };
    let value = match render_source_usage(&state, &snapshot, &client_key, &row, instance_id) {
        Ok(value) => value,
        Err(error) => return internal_error(&error),
    };

    let mut response = (StatusCode::OK, Json(value)).into_response();
    if let Ok(status) = HeaderValue::from_str(&row.http_status.to_string()) {
        response
            .headers_mut()
            .insert("x-llmroller-upstream-http-status", status);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmroller_core::provider::ProviderKind;

    #[test]
    #[doc = "Spec: USG-6, USG-7, PRV-1, USR-4, SEC-7, SEC-9, API-1"]
    fn openrouter_usage_url_does_not_duplicate_api_v1_prefix() {
        let url = resolve_provider_usage_url(
            "https://openrouter.ai/api/v1",
            ProviderKind::Openrouter,
            r#"{"paths":{"usage":"/api/v1/key"}}"#,
        )
        .expect("url");
        assert_eq!(url, "https://openrouter.ai/api/v1/key");

        let default_or = resolve_provider_usage_url(
            "https://openrouter.ai/api/v1",
            ProviderKind::Openrouter,
            "{}",
        )
        .expect("default openrouter url");
        assert_eq!(default_or, "https://openrouter.ai/api/v1/key");
    }

    fn caller(user_id: llmroller_core::ids::UserId) -> ClientKeyRecord {
        ClientKeyRecord {
            id: llmroller_core::ids::ClientKeyId::generate(),
            owner_user_id: user_id,
            label: "test".to_string(),
            prefix: "llr_test".to_string(),
            key_hash: [0; 32],
            prev_key_hash: None,
            prev_key_expires_at: None,
            routing_scope: "inherit".to_string(),
            allowed_models: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: "off".to_string(),
            service_account_id: None,
            disabled: false,
            paused_at: None,
            revoked_at: None,
            expires_at: None,
        }
    }

    #[test]
    #[doc = "Spec: USG-7, USR-4, SEC-16"]
    fn explicit_global_upstream_key_lookup_is_admin_only() {
        use llmroller_core::ids::{ProviderId, UserId};
        use llmroller_core::provider::ProviderKind;
        use llmroller_core::snapshot::{ProviderRecord, UserRecord};

        let mut snapshot = ConfigSnapshot::default();
        let standard_id = UserId::generate();
        let admin_id = UserId::generate();
        let provider_id = ProviderId::generate();
        let key_id = UpstreamKeyId::generate();

        let mut users = std::collections::HashMap::new();
        users.insert(
            standard_id,
            UserRecord {
                id: standard_id,
                username: "standard".to_string(),
                role: Role::User,
                disabled: false,
                can_use_global_providers: true,
                global_budget_micro_usd_month: None,
            },
        );
        users.insert(
            admin_id,
            UserRecord {
                id: admin_id,
                username: "admin".to_string(),
                role: Role::Admin,
                disabled: false,
                can_use_global_providers: true,
                global_budget_micro_usd_month: None,
            },
        );
        snapshot.users = Arc::new(users);

        let mut providers = std::collections::HashMap::new();
        providers.insert(
            provider_id,
            ProviderRecord {
                id: provider_id,
                ownership_scope: OwnershipScope::Global,
                owner_user_id: None,
                kind: ProviderKind::Openrouter,
                name: "global".to_string(),
                base_url: "https://openrouter.ai/api/v1".to_string(),
                config_json: "{}".to_string(),
                config: llmroller_core::provider::ProviderConfig::default(),
                share_config_fingerprint: String::new(),
                usage_url_cross_origin_ack: false,
                allow_private_network: false,
                enabled: true,
                billing_coverage_mode: "pay_as_you_go".to_string(),
                plan_multiplier_bps: 10_000,
            },
        );
        snapshot.providers = Arc::new(providers);

        let mut keys = std::collections::HashMap::new();
        keys.insert(
            key_id,
            UpstreamKeyRecord {
                id: key_id,
                provider_id,
                owner_user_id: None,
                label: "global key".to_string(),
                secret_last4: "1234".to_string(),
                secret_enc: Vec::new(),
                master_key_id: "mk".to_string(),
                enabled: true,
                priority: 0,
                strict_tier: 0,
                health: "healthy".to_string(),
                health_version: 1,
            },
        );
        snapshot.upstream_keys = Arc::new(keys);

        let standard = caller(standard_id);
        let admin = caller(admin_id);
        let params = UsageQueryParams {
            session_id: None,
            model: None,
            upstream_key_id: Some(key_id.to_string()),
            fresh: None,
        };
        assert!(
            resolve_usage_key(&snapshot, &standard, &params, None, None, 0).is_err(),
            "standard users must not probe arbitrary global upstream keys"
        );
        assert_eq!(
            resolve_usage_key(&snapshot, &admin, &params, None, None, 0).expect("admin lookup"),
            Some(key_id)
        );
    }
}
