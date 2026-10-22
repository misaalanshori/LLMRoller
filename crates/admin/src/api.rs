#![forbid(unsafe_code)]
//! JSON control-plane CRUD handlers for providers, keys, client keys, and settings.
//!
//! Spec: ./docs/srdd/14. SRDD - API and Web UI Specification.md#142-control-plane-json-api-apiuser-and-adminapi
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#933-closed-runtime-settings-registry
//! Phase: ./docs/bigbang/big-bang-phases-2.md#20-sub-phase-20-minimal-control-plane-json-crud-for-providers-keys-client-keys-and-settings

use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use llmroller_core::chat_preview::escape_html;
use llmroller_core::ids::{ClientKeyId, ProviderId, UpstreamKeyId, UserId};
use llmroller_core::provider::{ProviderConfig, ProviderKind};
use llmroller_core::rbac::{ActorScope, OwnershipScope};
use llmroller_core::seams::secret_string;
use llmroller_core::snapshot::ConfigDomain;
use llmroller_store::client_keys::{
    ClientKey, ClientKeyError, ClientKeyPatch, NewClientKey, create_client_key, delete_client_key,
    list_client_keys, patch_client_key, set_client_key_pause_tx,
};
use llmroller_store::crypto::MasterKey;
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, Provider, ProviderError, ProviderPatch, UpstreamKey,
    UpstreamKeyPatch, create_provider, create_upstream_key, delete_provider, delete_upstream_key,
    get_provider, get_upstream_key, list_providers, list_upstream_keys, patch_provider,
    patch_upstream_key,
};
use llmroller_store::service_accounts::{
    NewServiceAccount, ServiceAccountError, ServiceAccountPatch, create_service_account_tx,
    delete_service_account, list_service_accounts, patch_service_account,
};
use llmroller_store::settings::{
    RuntimeSettings, SettingsError, load_settings, update_setting, validate_setting,
};
use serde::{Deserialize, Serialize};
use sqlx::{Connection as _, Sqlite, Transaction};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};

use crate::auth::{AuthAppState, AuthError, authenticate_session};

type MutationResponseError = (StatusCode, Json<serde_json::Value>);
type MutationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MutationResponseError>> + Send + 'a>>;

tokio::task_local! {
    /// Audit metadata for the mutation currently executing inside the control
    /// plane. Set by `control_mutation_barrier` before dispatch (F09).
    static CURRENT_MUTATION_AUDIT: Option<MutationAuditContext>;
    /// Set by `execute_atomic_mutation` when a handler already wrote its entity
    /// SQL, `audit_log` row, and `config_version` bump atomically. The barrier
    /// then skips its fallback audit/version publication.
    static MUTATION_HANDLED: std::sync::atomic::AtomicBool;
}

#[derive(Debug, Clone)]
struct MutationAuditContext {
    ts: i64,
    actor_user_id: Option<String>,
    ip_addr: Option<String>,
    action: String,
    target_type: String,
    target_id: Option<String>,
    detail_json: String,
}

/// Run one control-plane mutation, its `audit_log` row, and the
/// `config_version` bump in a single `BEGIN IMMEDIATE` transaction on the
/// writer actor (F09, AUD-1, REL-4).
///
/// Entity SQL is deliberately supplied as a transaction closure so network
/// operations and validation can stay outside the write lock.
async fn execute_atomic_mutation<T, F>(
    state: &ApiAppState,
    domains: Vec<ConfigDomain>,
    run: F,
) -> Result<T, MutationResponseError>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&'a mut Transaction<'_, Sqlite>) -> MutationFuture<'a, T> + Send + 'static,
{
    use llmroller_core::seams::ExposeSecret as _;

    let _ = MUTATION_HANDLED.try_with(|flag| {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let audit = CURRENT_MUTATION_AUDIT.try_with(Clone::clone).ok().flatten();
    let fallback = llmroller_store::crypto::sha256_hex(state.master_key.as_bytes());
    let audit_key: Vec<u8> = state.audit_hmac_key.as_ref().map_or_else(
        || fallback.into_bytes(),
        |secret| secret.expose_secret().to_vec(),
    );
    let slot = Arc::new(StdMutex::new(None::<Result<T, MutationResponseError>>));
    let slot_for_op = slot.clone();

    let op: MutationRunner = Box::new(move |tx| {
        Box::pin(async move {
            match run(tx).await {
                Ok(value) => {
                    if let Some(audit) = audit {
                        scan_mutation_audit(tx, &audit_key, &audit)
                            .await
                            .map_err(|_| writer_busy_error())?;
                    }
                    *slot_for_op.lock().expect("mutation result lock") = Some(Ok(value));
                    Ok(())
                }
                Err(error) => {
                    *slot_for_op.lock().expect("mutation result lock") = Some(Err(error));
                    Err(writer_busy_error())
                }
            }
        })
    });

    if let Some(writer) = &state.writer {
        let result = writer
            .execute_control(domains, Vec::new(), AdminControlOp { run: Some(op) })
            .await;
        if let Ok(version) = result {
            await_published_version(state, version).await;
        } else {
            tracing::error!("control-plane mutation failed: {result:?}");
        }
    } else {
        let mut connection = state
            .auth
            .pool
            .acquire()
            .await
            .map_err(|_| writer_busy_error())?;
        let mut tx = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| writer_busy_error())?;
        op(&mut tx).await.map_err(|_| writer_busy_error())?;
        llmroller_store::writer::bump_config_version_tx(&mut tx)
            .await
            .map_err(|_| writer_busy_error())?;
        tx.commit().await.map_err(|_| writer_busy_error())?;
    }

    slot.lock()
        .expect("mutation result lock")
        .take()
        .ok_or_else(writer_busy_error)?
}

/// Record a control-plane action that has no entity SQL of its own (for
/// example a discovery refresh request) while still auditing it atomically.
async fn record_control_mutation(
    state: &ApiAppState,
    domains: Vec<ConfigDomain>,
) -> Result<(), MutationResponseError> {
    execute_atomic_mutation(state, domains, |_tx| Box::pin(async { Ok(()) })).await
}

fn writer_busy_error() -> MutationResponseError {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": {
                "code": "writer_busy",
                "message": "the configuration writer did not commit the mutation"
            }
        })),
    )
}

/// Boxed higher-ranked mutation closure executed by the writer actor.
type MutationRunner = Box<
    dyn for<'a> FnOnce(&'a mut Transaction<'_, Sqlite>) -> MutationFuture<'a, ()> + Send + 'static,
>;

struct AdminControlOp {
    run: Option<MutationRunner>,
}

impl llmroller_store::writer::ControlOp for AdminControlOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), llmroller_store::writer::WriterError>> + Send + 'a>>
    {
        Box::pin(async move {
            let run = self.run.expect("control mutation executed twice");
            run(tx).await.map_err(|_| {
                llmroller_store::writer::WriterError::Execution(
                    "control mutation aborted".to_string(),
                )
            })
        })
    }
}

async fn scan_mutation_audit(
    tx: &mut Transaction<'_, Sqlite>,
    key: &[u8],
    audit: &MutationAuditContext,
) -> Result<(), sqlx::Error> {
    let audit_id = llmroller_core::ids::RequestId::generate().to_string();
    llmroller_store::audit::append(
        tx,
        key,
        llmroller_store::audit::AuditEntry {
            id: &audit_id,
            ts: audit.ts,
            actor_user_id: audit.actor_user_id.as_deref(),
            ip_addr: audit.ip_addr.as_deref(),
            action: &audit.action,
            target_type: &audit.target_type,
            target_id: audit.target_id.as_deref(),
            detail_json: Some(&audit.detail_json),
        },
    )
    .await
    .map(|_| ())
    .map_err(|_| sqlx::Error::Protocol("audit append failed".to_string()))
}

async fn await_published_version(state: &ApiAppState, version: i64) {
    let Some(rx) = &state.published_version else {
        return;
    };
    let mut rx = rx.clone();
    if *rx.borrow_and_update() >= version {
        return;
    }
    let wait = async {
        while rx.changed().await.is_ok() {
            if *rx.borrow_and_update() >= version {
                return;
            }
        }
    };
    if tokio::time::timeout(std::time::Duration::from_millis(2_000), wait)
        .await
        .is_err()
    {
        tracing::warn!("reloader did not publish version {version} within 2s");
    }
}

/// Shared state for the control-plane JSON API.
#[derive(Clone, Debug)]
pub struct ApiAppState {
    pub auth: AuthAppState,
    pub master_key: MasterKey,
    pub master_key_id: String,
    pub writer: Option<llmroller_store::writer::WriterHandle>,
    /// Reloader input channel, used to publish committed control-plane changes.
    pub reloader_tx: Option<tokio::sync::mpsc::Sender<llmroller_store::writer::ReloadNotification>>,
    /// Published config versions, used to enforce the read-your-writes barrier.
    pub published_version: Option<tokio::sync::watch::Receiver<i64>>,
    /// Instance audit HMAC key used for the canonical `audit_log` hash chain.
    pub audit_hmac_key: Option<llmroller_core::seams::SecretBytes>,
    /// Background `/models` discovery worker trigger (MDL-1).
    pub discovery_tx:
        Option<tokio::sync::mpsc::UnboundedSender<llmroller_proto::discovery::DiscoveryTarget>>,
    /// Live data-plane session pins, shared with `ProxyAuthState` so control-plane
    /// unpin actions take effect before the next proxied request (SEC-17).
    pub pin_store: Option<std::sync::Arc<llmroller_core::pins::PinStore>>,
}

impl ApiAppState {
    #[must_use]
    pub fn new(auth: AuthAppState, master_key: MasterKey, master_key_id: String) -> Self {
        Self {
            auth,
            master_key,
            master_key_id,
            writer: None,
            reloader_tx: None,
            published_version: None,
            audit_hmac_key: None,
            discovery_tx: None,
            pin_store: None,
        }
    }

    /// Share the data-plane `PinStore` with the control plane so unpin actions
    /// evict live entries immediately (SEC-17).
    pub fn attach_pin_store(&mut self, pin_store: std::sync::Arc<llmroller_core::pins::PinStore>) {
        self.pin_store = Some(pin_store);
    }

    /// Attach the Reloader notification channel and version watch.
    pub fn attach_reloader(
        &mut self,
        reloader_tx: tokio::sync::mpsc::Sender<llmroller_store::writer::ReloadNotification>,
        published_version: tokio::sync::watch::Receiver<i64>,
    ) {
        self.reloader_tx = Some(reloader_tx);
        self.published_version = Some(published_version);
    }

    /// Publish a committed control-plane mutation: bump `instance_meta.config_version`
    /// and wake the reloader so changed slices are visible within 1 s (`REL-4`).
    ///
    /// Mutations routed through [`execute_atomic_mutation`] already publish via the
    /// writer actor; this barrier remains the safety net for legacy direct-SQL
    /// handlers that do not use the writer.
    pub async fn publish_control_change(&self, domains: Vec<ConfigDomain>) {
        let Some(writer) = &self.writer else {
            return;
        };
        let result = writer
            .execute_control(domains, Vec::new(), VersionBumpOp)
            .await;
        match result {
            Ok(version) => await_published_version(self, version).await,
            Err(error) => tracing::error!("control change publish failed: {error}"),
        }
    }

    /// Queue a `/models` discovery refresh, or a no-op when no worker is attached (MDL-1).
    pub fn trigger_discovery(&self, target: llmroller_proto::discovery::DiscoveryTarget) {
        let Some(discovery_tx) = &self.discovery_tx else {
            return;
        };
        if discovery_tx.send(target).is_err() {
            tracing::warn!(?target, "model discovery worker is not running");
        }
    }
}

/// No-op control transaction used to publish a config-version bump (`REL-4`).
struct VersionBumpOp;

impl llmroller_store::writer::ControlOp for VersionBumpOp {
    fn call<'a>(
        self: Box<Self>,
        _tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), llmroller_store::writer::WriterError>> + Send + 'a>>
    {
        Box::pin(async { Ok(()) })
    }
}

fn domains_for_path(path: &str) -> Vec<ConfigDomain> {
    if path.contains("/providers") || path.contains("/keys") || path.contains("/upstream-keys") {
        vec![ConfigDomain::Providers, ConfigDomain::UpstreamKeys]
    } else if path.contains("/client-keys") {
        vec![ConfigDomain::ClientKeys]
    } else if path.contains("/models") || path.contains("/aliases") || path.contains("/routes") {
        vec![ConfigDomain::Models]
    } else if path.contains("/shares") {
        vec![ConfigDomain::Shares]
    } else if path.contains("/prices") {
        vec![ConfigDomain::Prices]
    } else if path.contains("/settings") || path.contains("/traceability") {
        vec![ConfigDomain::Settings]
    } else if path.contains("/users") {
        vec![ConfigDomain::Users]
    } else if path.contains("/traces") || path.contains("/sessions") {
        vec![ConfigDomain::Traceability]
    } else {
        // Unknown mutation domain: force a full snapshot rebuild to stay correct.
        Vec::new()
    }
}

struct MutationAudit {
    action: String,
    target_type: &'static str,
    target_id: Option<String>,
}

fn mutation_resource(segment: &str) -> Option<&'static str> {
    match segment {
        "providers" => Some("provider"),
        "upstream-keys" | "keys" => Some("upstream_key"),
        "client-keys" => Some("client_key"),
        "users" => Some("user"),
        "models" => Some("model"),
        "model-aliases" | "aliases" => Some("model_alias"),
        "settings" => Some("setting"),
        "shares" => Some("share"),
        "pins" => Some("pin"),
        "sessions" => Some("session"),
        "routes" => Some("route"),
        "prices" => Some("price"),
        "traces" => Some("trace"),
        "web-sessions" => Some("web_session"),
        "password" => Some("password"),
        "totp" => Some("totp"),
        "usage" => Some("usage"),
        "audit-log" => Some("audit"),
        _ => None,
    }
}

fn method_verb(method: &Method) -> &'static str {
    match *method {
        Method::POST => "created",
        Method::PUT => "replaced",
        Method::PATCH => "updated",
        Method::DELETE => "deleted",
        _ => "mutated",
    }
}

fn mutation_target_id(segments: &[&str], resource_index: usize, resource: &str) -> Option<String> {
    let candidate = *segments.get(resource_index + 1)?;
    if matches!(
        candidate,
        "keys"
            | "aliases"
            | "pin"
            | "trace"
            | "rotate"
            | "accept"
            | "approve"
            | "test"
            | "refresh"
            | "sync"
            | "move"
            | "confirm-merge"
            | "traceability"
            | "enroll"
            | "verify"
            | "disable"
            | "reset"
            | "reset-password"
            | "summary"
            | "events"
            | "timeseries"
            | "breakdown"
            | "export.csv"
    ) {
        return None;
    }
    if resource == "pin" {
        // /api/user/pins/{client_key_id}/{session_hash}/{canonical_id}
        return segments
            .get(resource_index + 2)
            .map(|session_hash| (*session_hash).to_string());
    }
    Some(candidate.to_string())
}

fn classify_mutation(method: &Method, path: &str) -> MutationAudit {
    let scope = if path.starts_with("/admin/") {
        "admin"
    } else if path.starts_with("/api/user/") {
        "user"
    } else {
        "control"
    };
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let Some(resource_index) = segments
        .iter()
        .position(|segment| mutation_resource(segment).is_some())
    else {
        return MutationAudit {
            action: format!("{scope}.control.{}", method_verb(method)),
            target_type: "http_path",
            target_id: Some(path.to_string()),
        };
    };

    let resource_segment = segments[resource_index];
    let resource = mutation_resource(resource_segment).unwrap_or("resource");
    let last = segments.last().copied().unwrap_or(resource_segment);
    let (target_type, verb) = match (last, method.as_str()) {
        ("keys", "POST") => ("upstream_key", "created"),
        ("aliases", "POST") => ("model_alias", "created"),
        ("pin", "DELETE") => ("session_pin", "deleted"),
        ("trace", "DELETE") => ("trace", "deleted"),
        ("rotate", "POST") => ("client_key", "rotated"),
        ("accept", "POST") => ("share", "accepted"),
        ("approve", "POST") => ("share", "approved"),
        ("test", "POST") => ("upstream_key", "tested"),
        ("refresh", "POST") => ("model", "refreshed"),
        ("sync", "POST") => ("price", "synced"),
        ("move", "POST") => ("pin", "moved"),
        ("confirm-merge", "POST") => ("model", "merged"),
        ("traceability", "PUT") => ("traceability", "updated"),
        ("enroll", "POST") => ("totp", "enrolled"),
        ("verify", "POST") => ("totp", "verified"),
        ("disable", "POST") => ("totp", "disabled"),
        ("reset-password", "POST") => ("user", "password_reset"),
        ("reset", "POST") => ("user", "totp_reset"),
        ("password", "POST") => ("password", "changed"),
        _ => (resource, method_verb(method)),
    };
    let target_id = mutation_target_id(&segments, resource_index, resource);
    MutationAudit {
        action: format!("{scope}.{target_type}.{verb}"),
        target_type,
        target_id,
    }
}

/// Publish a config version after every successful control-plane mutation so that
/// revocations and edits are visible to the next request (`REL-4`).
///
/// `AUD-1`: the successful mutation is recorded with the authenticated actor,
/// client IP, semantic action, target, and request detail. Actor resolution
/// happens before dispatch so self-invalidating mutations (password changes)
/// still retain their actor.
async fn control_mutation_barrier(
    State(state): State<ApiAppState>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let is_mutation = matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    let path = request.uri().path().to_string();
    let domains = domains_for_path(&path);
    let headers = request.headers().clone();
    let peer_addr = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0);
    let actor_user_id = if is_mutation {
        authenticate_actor(&state, &headers, false)
            .await
            .ok()
            .and_then(|actor| actor.user_id().map(|id| id.to_string()))
    } else {
        None
    };
    let ip_addr = peer_addr.map(|peer| {
        crate::auth::extract_client_ip(peer, &headers, &state.auth.config.trusted_proxies)
            .to_string()
    });

    let audit = classify_mutation(&method, &path);
    let detail = serde_json::json!({
        "method": method.as_str(),
        "path": path,
    })
    .to_string();
    let audit_context = if is_mutation {
        Some(MutationAuditContext {
            ts: chrono_like_now_ms(),
            actor_user_id: actor_user_id.clone(),
            ip_addr: ip_addr.clone(),
            action: audit.action.clone(),
            target_type: audit.target_type.to_string(),
            target_id: audit.target_id.clone(),
            detail_json: detail.clone(),
        })
    } else {
        None
    };

    let (response, handled) = if let Some(context) = audit_context {
        CURRENT_MUTATION_AUDIT
            .scope(
                Some(context),
                MUTATION_HANDLED.scope(std::sync::atomic::AtomicBool::new(false), async {
                    let response = next.run(request).await;
                    let handled = MUTATION_HANDLED
                        .with(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
                    (response, handled)
                }),
            )
            .await
    } else {
        (next.run(request).await, false)
    };

    if is_mutation && response.status().is_success() && !handled {
        state.publish_control_change(domains).await;
        let _ = append_audit_entry_helper(
            &state,
            chrono_like_now_ms(),
            AuditEnvelope {
                actor_user_id: actor_user_id.as_deref(),
                ip_addr: ip_addr.as_deref(),
                action: &audit.action,
                target_type: audit.target_type,
                target_id: audit.target_id.as_deref(),
                detail_json: &detail,
            },
        )
        .await;
    }
    response
}

/// Helper to validate session and CSRF for JSON endpoints.
async fn authenticate_actor(
    state: &ApiAppState,
    headers: &HeaderMap,
    is_mutation: bool,
) -> Result<ActorScope, (StatusCode, Json<serde_json::Value>)> {
    let session = authenticate_session(&state.auth, headers)
        .await
        .map_err(map_auth_error)?;

    if is_mutation {
        crate::auth::validate_session_csrf(&session, headers)
            .await
            .map_err(map_auth_error)?;
    }

    Ok(ActorScope::user(session.user.id, session.user.role))
}

async fn authenticate_admin(
    state: &ApiAppState,
    headers: &HeaderMap,
    is_mutation: bool,
) -> Result<ActorScope, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(state, headers, is_mutation).await?;
    if !actor.is_admin() {
        return Err(map_auth_error(AuthError::Forbidden));
    }
    Ok(actor)
}

pub(crate) fn map_auth_error(err: AuthError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match err {
        AuthError::Unauthorized | AuthError::AccountDisabled => StatusCode::UNAUTHORIZED,
        AuthError::Forbidden | AuthError::InvalidCsrf => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({ "error": { "message": err.to_string() } })),
    )
}

fn map_provider_error(err: ProviderError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match err {
        ProviderError::Forbidden => StatusCode::FORBIDDEN,
        ProviderError::ProviderNotFound(_) | ProviderError::UpstreamKeyNotFound(_) => {
            StatusCode::NOT_FOUND
        }
        ProviderError::QuotaExceeded { .. } => StatusCode::CONFLICT,
        ProviderError::Conflict(_) => StatusCode::CONFLICT,
        ProviderError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({ "error": { "message": err.to_string() } })),
    )
}

fn map_client_key_error(err: ClientKeyError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match err {
        ClientKeyError::Forbidden => StatusCode::FORBIDDEN,
        ClientKeyError::NotFound(_) => StatusCode::NOT_FOUND,
        ClientKeyError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({ "error": { "message": err.to_string() } })),
    )
}

fn map_settings_error(err: SettingsError) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY),
        Json(serde_json::json!({ "error": { "message": err.to_string() } })),
    )
}

// ---------------------------------------------------------------------------
// Request/Response DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateProviderRequest {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    #[serde(default = "default_config_json")]
    pub config_json: String,
    #[serde(default)]
    pub usage_url_cross_origin_ack: bool,
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_config_json() -> String {
    "{}".to_owned()
}

/// Reject provider configuration documents that cannot be deserialized.
///
/// Snapshot loading stays fail-open for legacy or hand-edited rows, but the
/// control plane must not persist a document that would silently wipe the
/// provider's routing configuration (`PRV-3`, `API-2`).
fn validate_provider_config_json(
    config_json: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    ProviderConfig::try_from_json(config_json)
        .map(|_| ())
        .map_err(|error| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("invalid provider config_json: {error}"),
                    }
                })),
            )
        })
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct PatchProviderRequest {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub config_json: Option<String>,
    pub usage_url_cross_origin_ack: Option<bool>,
    pub allow_private_network: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyRequest {
    pub label: String,
    pub secret: String,
    pub priority: Option<i64>,
    pub strict_tier: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PatchKeyRequest {
    pub label: Option<String>,
    pub priority: Option<i64>,
    pub strict_tier: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateClientKeyRequest {
    pub label: String,
    pub service_account_id: Option<String>,
    pub routing_scope: Option<String>,
    pub allowed_models: Option<Vec<String>>,
    pub rate_limit_rpm: Option<i64>,
    pub budget_micro_usd_month: Option<i64>,
    pub allowed_cidrs: Option<Vec<String>>,
    pub constant_session_mode: Option<String>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ClientKeyCreationResponse {
    pub client_key: ClientKey,
    pub token: String,
}

// ---------------------------------------------------------------------------
// Personal Provider Endpoints (/api/user/providers)
// ---------------------------------------------------------------------------

async fn get_user_providers(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Provider>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let providers = list_providers(&state.auth.pool, &actor)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(providers))
}

async fn post_user_provider(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<Provider>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    validate_provider_config_json(&payload.config_json)?;
    let owner_user_id = actor.user_id();

    let new_provider = NewProvider {
        name: payload.name,
        kind: payload.kind,
        base_url: payload.base_url,
        config_json: payload.config_json,
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

async fn get_user_provider(
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

async fn patch_user_provider_handler(
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

async fn delete_user_provider_handler(
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

async fn get_user_provider_keys(
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

async fn post_user_provider_key(
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

async fn get_user_upstream_key_handler(
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

async fn patch_user_upstream_key_handler(
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

async fn delete_user_upstream_key_handler(
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

async fn get_user_client_keys_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ClientKey>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let keys = list_client_keys(&state.auth.pool, &actor, user_id)
        .await
        .map_err(map_client_key_error)?;
    Ok(Json(keys))
}

async fn post_user_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateClientKeyRequest>,
) -> Result<(StatusCode, Json<ClientKeyCreationResponse>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let new_key = NewClientKey {
        label: payload.label,
        service_account_id: payload.service_account_id,
        routing_scope: payload.routing_scope,
        allowed_models: payload.allowed_models,
        rate_limit_rpm: payload.rate_limit_rpm,
        budget_micro_usd_month: payload.budget_micro_usd_month,
        allowed_cidrs: payload.allowed_cidrs,
        constant_session_mode: payload.constant_session_mode,
        expires_at: payload.expires_at,
    };
    let now_ms = state.auth.clock.now_ms();
    let res = create_client_key(
        &state.auth.pool,
        &actor,
        user_id,
        new_key,
        state.auth.entropy.as_ref(),
        now_ms,
    )
    .await
    .map_err(map_client_key_error)?;

    use llmroller_core::seams::ExposeSecret as _;
    let token_str = res.raw_token.expose_secret().to_string();
    Ok((
        StatusCode::CREATED,
        Json(ClientKeyCreationResponse {
            client_key: res.client_key,
            token: token_str,
        }),
    ))
}

async fn patch_user_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
    Json(payload): Json<ClientKeyPatch>,
) -> Result<Json<ClientKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let key = patch_client_key(&state.auth.pool, &actor, id, payload)
        .await
        .map_err(map_client_key_error)?;
    Ok(Json(key))
}

async fn delete_user_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    delete_client_key(&state.auth.pool, &actor, id)
        .await
        .map_err(map_client_key_error)?;
    if headers.contains_key("hx-request") {
        Ok(StatusCode::OK)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

/// `POST /api/user/client-keys/{id}/pause` (`CLK-7`).
async fn post_pause_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<ClientKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let key = execute_atomic_mutation(&state, vec![ConfigDomain::ClientKeys], move |tx| {
        Box::pin(async move {
            set_client_key_pause_tx(tx, &actor, id, true, now_ms)
                .await
                .map_err(map_client_key_error)
        })
    })
    .await?;
    Ok(Json(key))
}

/// `POST /api/user/client-keys/{id}/resume` (`CLK-7`).
async fn post_resume_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<ClientKey>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let key = execute_atomic_mutation(&state, vec![ConfigDomain::ClientKeys], move |tx| {
        Box::pin(async move {
            set_client_key_pause_tx(tx, &actor, id, false, now_ms)
                .await
                .map_err(map_client_key_error)
        })
    })
    .await?;
    Ok(Json(key))
}

fn map_service_account_error(err: ServiceAccountError) -> (StatusCode, Json<serde_json::Value>) {
    let (status, code) = match &err {
        ServiceAccountError::Forbidden => (StatusCode::FORBIDDEN, "permission_denied"),
        ServiceAccountError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
        ServiceAccountError::NameTaken => (StatusCode::CONFLICT, "service_account_name_taken"),
        ServiceAccountError::Validation(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "validation_error")
        }
        ServiceAccountError::Sqlx(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    };
    (
        status,
        Json(serde_json::json!({"error": {"code": code, "message": err.to_string()}})),
    )
}

/// `GET /api/user/service-accounts` (`CLK-6`, Section 14.j).
async fn get_user_service_accounts_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<llmroller_store::service_accounts::ServiceAccount>>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let accounts = list_service_accounts(&state.auth.pool, &actor, Some("personal"), None)
        .await
        .map_err(map_service_account_error)?;
    Ok(Json(accounts))
}

/// `POST /api/user/service-accounts` (`CLK-6`, Section 14.j.4).
async fn post_user_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<NewServiceAccount>,
) -> Result<
    (
        StatusCode,
        Json<llmroller_store::service_accounts::ServiceAccount>,
    ),
    MutationResponseError,
> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let owner = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();
    let account = execute_atomic_mutation(&state, vec![ConfigDomain::ServiceAccounts], move |tx| {
        Box::pin(async move {
            create_service_account_tx(tx, &actor, owner, payload, now_ms)
                .await
                .map_err(map_service_account_error)
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(account)))
}

/// `GET /api/user/service-accounts/{id}` (`CLK-6`).
async fn get_user_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<llmroller_store::service_accounts::ServiceAccount>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let account =
        llmroller_store::service_accounts::get_service_account(&state.auth.pool, &actor, &id)
            .await
            .map_err(map_service_account_error)?;
    Ok(Json(account))
}

/// `PATCH /api/user/service-accounts/{id}` (`CLK-6`).
async fn patch_user_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<ServiceAccountPatch>,
) -> Result<Json<llmroller_store::service_accounts::ServiceAccount>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let account = patch_service_account(&state.auth.pool, &actor, &id, payload, now_ms)
        .await
        .map_err(map_service_account_error)?;
    Ok(Json(account))
}

#[derive(Debug, serde::Deserialize)]
pub struct IpRestrictionsRequest {
    #[serde(default)]
    pub allowed_cidrs: Vec<String>,
    /// Textarea form alternative accepted by HTMX clients (Section 14.k.4).
    #[serde(default)]
    pub allowed_cidrs_text: Option<String>,
}

/// `PUT /api/user/service-accounts/{id}/ip-restrictions` (`CLK-6`, Section 14.k.4).
async fn put_user_service_account_ip_restrictions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<IpRestrictionsRequest>,
) -> Result<Json<llmroller_store::service_accounts::ServiceAccount>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let cidrs = if let Some(text) = payload.allowed_cidrs_text {
        llmroller_store::service_accounts::parse_cidrs_text(&text)
            .map_err(map_service_account_error)?
    } else if payload.allowed_cidrs.is_empty() {
        None
    } else {
        Some(payload.allowed_cidrs)
    };
    let now_ms = state.auth.clock.now_ms();
    let account = llmroller_store::service_accounts::set_service_account_cidrs(
        &state.auth.pool,
        &actor,
        &id,
        cidrs,
        now_ms,
    )
    .await
    .map_err(map_service_account_error)?;
    Ok(Json(account))
}

/// `DELETE /api/user/service-accounts/{id}` (`CLK-6`).
async fn delete_user_service_account_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    delete_service_account(&state.auth.pool, &actor, &id)
        .await
        .map_err(map_service_account_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, serde::Deserialize)]
pub struct UpdatePreferencesRequest {
    pub ui_theme: Option<String>,
    pub ui_nav_layout: Option<String>,
    pub ui_locale: Option<String>,
    pub default_routing_scope: Option<String>,
}

/// `PATCH /api/user/preferences` (`UI-5`, `UI-6`).
async fn patch_user_preferences_handler(
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

    sqlx::query(
        "UPDATE users SET
            ui_theme = COALESCE(?, ui_theme),
            ui_nav_layout = COALESCE(?, ui_nav_layout),
            ui_locale = COALESCE(?, ui_locale),
            default_routing_scope = COALESCE(?, default_routing_scope)
         WHERE id = ?",
    )
    .bind(payload.ui_theme.as_deref())
    .bind(payload.ui_nav_layout.as_deref())
    .bind(payload.ui_locale.as_deref())
    .bind(payload.default_routing_scope.as_deref())
    .bind(user_id.to_string())
    .execute(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "status": "updated",
        "ui_theme": payload.ui_theme,
        "ui_nav_layout": payload.ui_nav_layout,
        "ui_locale": payload.ui_locale,
        "default_routing_scope": payload.default_routing_scope,
    })))
}

/// `GET /api/user/web-sessions` (`AUTH-7`, `F011`).
async fn get_user_web_sessions_handler(
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
        crate::auth::parse_cookie_value(cookie_header, state.auth.config.session_cookie_name())
            .unwrap_or("");
    let current_hash = llmroller_store::crypto::sha256_hex(current_token.as_bytes());

    use sqlx::Row as _;
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

/// `GET /api/user/audit-log` (`AUD-2`).
///
/// Returns only rows attributable to the authenticated actor; admins use
/// `/admin/api/audit-log` for the instance-wide chain.
async fn get_user_audit_log_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, MutationResponseError> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT seq, ts, actor_user_id, ip_addr, action, target_type, target_id, detail_json, prev_hash, hash
         FROM audit_log WHERE actor_user_id = ? ORDER BY seq DESC LIMIT 200",
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(
        rows.iter()
            .map(|row| {
                serde_json::json!({
                    "seq": row.try_get::<i64, _>("seq").unwrap_or_default(),
                    "ts": row.try_get::<i64, _>("ts").unwrap_or_default(),
                    "actor_user_id": row.try_get::<Option<String>, _>("actor_user_id").unwrap_or_default(),
                    "ip_addr": row.try_get::<Option<String>, _>("ip_addr").unwrap_or_default(),
                    "action": row.try_get::<String, _>("action").unwrap_or_default(),
                    "target_type": row.try_get::<String, _>("target_type").unwrap_or_default(),
                    "target_id": row.try_get::<Option<String>, _>("target_id").unwrap_or_default(),
                    "detail_json": row.try_get::<Option<String>, _>("detail_json").unwrap_or_default(),
                    "prev_hash": row.try_get::<String, _>("prev_hash").unwrap_or_default(),
                    "hash": row.try_get::<String, _>("hash").unwrap_or_default(),
                })
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// Admin Endpoints (/admin/api/*)
// ---------------------------------------------------------------------------

async fn get_admin_providers(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Provider>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let providers = list_providers(&state.auth.pool, &actor)
        .await
        .map_err(map_provider_error)?;
    Ok(Json(providers))
}

async fn post_admin_provider(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<Provider>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    validate_provider_config_json(&payload.config_json)?;
    let new_provider = NewProvider {
        name: payload.name,
        kind: payload.kind,
        base_url: payload.base_url,
        config_json: payload.config_json,
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

async fn post_admin_provider_key(
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

async fn get_admin_settings_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<RuntimeSettings>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    let settings = load_settings(&state.auth.pool)
        .await
        .map_err(map_settings_error)?;
    Ok(Json(settings))
}

async fn patch_admin_settings_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<HashMap<String, serde_json::Value>>,
) -> Result<Json<RuntimeSettings>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();

    // 1. Validate all keys and values first
    for (k, v) in &payload {
        let val_str = serde_json::to_string(v).unwrap_or_default();
        validate_setting(k, &val_str).map_err(map_settings_error)?;
    }

    // 2. Apply updates
    for (k, v) in &payload {
        let val_str = serde_json::to_string(v).unwrap_or_default();
        update_setting(&state.auth.pool, k, &val_str, now_ms)
            .await
            .map_err(map_settings_error)?;
    }

    let updated = load_settings(&state.auth.pool)
        .await
        .map_err(map_settings_error)?;
    Ok(Json(updated))
}

// ---------------------------------------------------------------------------
// Router Setup
// ---------------------------------------------------------------------------

/// Build the JSON control-plane router.
pub(crate) fn map_price_error(
    status: StatusCode,
    msg: impl std::fmt::Display,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({ "error": { "message": msg.to_string() } })),
    )
}

#[derive(Debug, Deserialize)]
pub struct UpdateTraceabilityRequest {
    pub enabled: bool,
    #[serde(default)]
    pub match_filters: llmroller_core::settings::TraceabilityMatch,
    pub retention_days: i64,
    pub auto_disable_at: Option<i64>,
    pub confirm_purge: Option<bool>,
}

async fn put_admin_traceability_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<UpdateTraceabilityRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let settings = llmroller_store::settings::load_settings(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let now_ms = state.auth.clock.now_ms();

    // Confirm purge guard (Section 6.10, TRC-1)
    if payload.retention_days < settings.traceability_config.retention_days
        && payload.confirm_purge != Some(true)
    {
        let new_cutoff = now_ms - payload.retention_days * 86_400_000;
        let affected_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM trace_records WHERE ts < ?")
                .bind(new_cutoff)
                .fetch_one(&state.auth.pool)
                .await
                .unwrap_or(0);

        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": {
                    "code": "purge_confirmation_required",
                    "message": "Lowering retention days requires explicit purge confirmation",
                    "affected_rows": affected_rows
                }
            })),
        ));
    }

    let new_config = llmroller_core::settings::TraceabilityConfig {
        enabled: payload.enabled,
        match_filters: payload.match_filters,
        retention_days: payload.retention_days,
        auto_disable_at: payload.auto_disable_at,
    };

    let config_json_str = serde_json::to_string(&new_config)
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    llmroller_store::settings::update_setting(
        &state.auth.pool,
        "traceability_config",
        &config_json_str,
        now_ms,
    )
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "traceability_config": new_config
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpsertPricePayload {
    pub provider_id: Option<String>,
    pub provider_kind: String,
    pub upstream_model_id: String,
    pub input_per_mtok_micro_usd: i64,
    pub output_per_mtok_micro_usd: i64,
    pub cache_read_per_mtok_micro_usd: Option<i64>,
    pub cache_write_per_mtok_micro_usd: Option<i64>,
    #[serde(default)]
    pub tiers: Vec<llmroller_core::pricing::ContextTier>,
    #[serde(default)]
    pub schedules: Vec<llmroller_core::pricing::Schedule>,
}

async fn put_admin_price_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<UpsertPricePayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let provider_id = payload.provider_id.clone().unwrap_or_default();
    upsert_price_internal(&state, provider_id, payload).await
}

async fn put_user_provider_price_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ProviderId>,
    Json(payload): Json<UpsertPricePayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let p = llmroller_store::providers::get_provider(&state.auth.pool, &actor, id)
        .await
        .map_err(map_provider_error)?;
    if p.owner_user_id != actor.user_id() {
        return Err(map_price_error(StatusCode::FORBIDDEN, "not provider owner"));
    }
    upsert_price_internal(&state, id.to_string(), payload).await
}

async fn upsert_price_internal(
    state: &ApiAppState,
    provider_id: String,
    payload: UpsertPricePayload,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let rates = llmroller_core::pricing::Rates {
        input_per_mtok_micro_usd: payload.input_per_mtok_micro_usd,
        output_per_mtok_micro_usd: payload.output_per_mtok_micro_usd,
        cache_read_per_mtok_micro_usd: payload.cache_read_per_mtok_micro_usd.unwrap_or(0),
        cache_write_per_mtok_micro_usd: payload.cache_write_per_mtok_micro_usd.unwrap_or(0),
    };

    let params = llmroller_store::prices::UpsertPriceParams {
        provider_id,
        provider_kind: payload.provider_kind,
        upstream_model_id: payload.upstream_model_id,
        rates,
        tiers: payload.tiers,
        schedules: payload.schedules,
        source: "admin".to_string(),
        now_ms: state.auth.clock.now_ms(),
    };

    llmroller_store::prices::upsert_price_tx(&mut tx, params)
        .await
        .map_err(|e| match e {
            llmroller_store::prices::PriceError::Validation(msg) => {
                map_price_error(StatusCode::UNPROCESSABLE_ENTITY, msg)
            }
            llmroller_store::prices::PriceError::Database(err) => {
                map_price_error(StatusCode::INTERNAL_SERVER_ERROR, err)
            }
        })?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

/// Parse the provider price catalogue served by `price_sync_catalog_url`.
///
/// Expected shape: `{ <provider>: { models: { <upstream_id>: { cost: { input, output,
/// cache_read, cache_write, input_over_200k?, output_over_200k?, cache_read_over_200k?,
/// cache_write_over_200k? }, alias?, aliases? } } } }`. Returns one insert template per
/// upstream model ID plus every alias the catalogue declares for it (F012).
fn parse_catalog_prices(
    catalog: &serde_json::Value,
) -> (
    HashMap<String, llmroller_store::prices::UpsertPriceParams>,
    HashMap<String, Vec<String>>,
) {
    let micro = |usd_per_mtok: f64| (usd_per_mtok * 1_000_000.0).round() as i64;
    let mut model_costs: HashMap<String, llmroller_store::prices::UpsertPriceParams> =
        HashMap::new();
    let mut catalog_aliases: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(providers) = catalog.as_object() {
        for provider in providers.values() {
            let Some(models) = provider.get("models").and_then(|m| m.as_object()) else {
                continue;
            };
            for (model_id, model) in models {
                // Catalog aliases (`alias` or `aliases`) are imported into
                // `model_aliases` so clients can call the upstream spelling (F012).
                let mut aliases = Vec::new();
                if let Some(alias) = model.get("alias").and_then(serde_json::Value::as_str) {
                    aliases.push(alias.to_string());
                }
                if let Some(list) = model.get("aliases").and_then(serde_json::Value::as_array) {
                    aliases.extend(
                        list.iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_owned),
                    );
                }
                if !aliases.is_empty() {
                    catalog_aliases
                        .entry(model_id.clone())
                        .or_default()
                        .extend(aliases);
                }

                let Some(cost) = model.get("cost") else {
                    continue;
                };
                let read = |key: &str| cost.get(key).and_then(serde_json::Value::as_f64);
                // Context-length tiers: providers publish `*_over_200k` rates for
                // prompts above the standard context window (`USG-3`, Section 9.3.2).
                let mut tiers = Vec::new();
                if let (Some(input_over), Some(output_over)) =
                    (read("input_over_200k"), read("output_over_200k"))
                {
                    tiers.push(llmroller_core::pricing::ContextTier {
                        min_total_input_tokens: 200_000,
                        max_total_input_tokens: None,
                        input_per_mtok_micro_usd: micro(input_over),
                        output_per_mtok_micro_usd: micro(output_over),
                        cache_read_per_mtok_micro_usd: read("cache_read_over_200k")
                            .map_or(0, micro),
                        cache_write_per_mtok_micro_usd: read("cache_write_over_200k")
                            .map_or(0, micro),
                    });
                }
                let entry = model_costs.entry(model_id.clone()).or_insert_with(|| {
                    llmroller_store::prices::UpsertPriceParams {
                        provider_id: String::new(),
                        provider_kind: String::new(),
                        upstream_model_id: model_id.clone(),
                        rates: llmroller_core::pricing::Rates::default(),
                        tiers: Vec::new(),
                        schedules: Vec::new(),
                        source: "synced".to_string(),
                        now_ms: 0,
                    }
                });
                entry.rates = llmroller_core::pricing::Rates {
                    input_per_mtok_micro_usd: micro(read("input").unwrap_or(0.0)),
                    output_per_mtok_micro_usd: micro(read("output").unwrap_or(0.0)),
                    cache_read_per_mtok_micro_usd: micro(read("cache_read").unwrap_or(0.0)),
                    cache_write_per_mtok_micro_usd: micro(read("cache_write").unwrap_or(0.0)),
                };
                entry.tiers = tiers;
            }
        }
    }
    (model_costs, catalog_aliases)
}

async fn post_admin_prices_sync_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let settings = llmroller_store::settings::load_settings(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if !settings.price_sync_catalog_url.enabled {
        return Ok(Json(serde_json::json!({
            "synced": false,
            "reason": "disabled"
        })));
    }

    let url = settings.price_sync_catalog_url.url.trim();
    llmroller_core::ssrf::validate_url(url, false)
        .map_err(|_| map_price_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid catalog URL"))?;

    let client = llmroller_proxy::ssrf::create_guarded_client(false)
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let catalog: serde_json::Value = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_GATEWAY, e))?
        .error_for_status()
        .map_err(|e| map_price_error(StatusCode::BAD_GATEWAY, e))?
        .json()
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_GATEWAY, e))?;

    let (model_costs, catalog_aliases) = parse_catalog_prices(&catalog);

    use sqlx::Row as _;
    let routes = sqlx::query(
        "SELECT k.provider_id, p.kind, r.upstream_model_id, r.model_pk
         FROM model_routes r
         JOIN upstream_keys k ON r.upstream_key_id = k.id
         JOIN providers p ON k.provider_id = p.id",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut synced = 0_usize;
    let mut imported_aliases = 0_usize;
    let mut seen = std::collections::HashSet::new();
    for route in routes {
        let provider_id: String = route.try_get("provider_id").unwrap_or_default();
        let kind: String = route.try_get("kind").unwrap_or_default();
        let model_id: String = route.try_get("upstream_model_id").unwrap_or_default();
        if !seen.insert((provider_id.clone(), kind.clone(), model_id.clone())) {
            continue;
        }

        // Import confirmed aliases for models that carry them in the catalog. The
        // alias must not collide with a canonical ID or an existing alias (MDL-6).
        if let Some(aliases) = catalog_aliases.get(&model_id) {
            let model_pk: String = route.try_get("model_pk").unwrap_or_default();
            for alias in aliases {
                if alias == &model_id {
                    continue;
                }
                let collides: Option<i64> =
                    sqlx::query_scalar("SELECT 1 FROM models WHERE canonical_id = ?")
                        .bind(alias)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                if collides.is_some() {
                    continue;
                }
                let result = sqlx::query(
                    "INSERT OR IGNORE INTO model_aliases (alias, model_pk, confirmed) VALUES (?, ?, 1)",
                )
                .bind(alias)
                .bind(&model_pk)
                .execute(&mut *tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                imported_aliases += usize::try_from(result.rows_affected()).unwrap_or(0);
            }
        }

        let Some(template) = model_costs.get(&model_id) else {
            continue;
        };
        let manual: Option<String> = sqlx::query_scalar(
            "SELECT source FROM prices WHERE provider_id = ? AND provider_kind = ? AND upstream_model_id = ?",
        )
        .bind(&provider_id)
        .bind(&kind)
        .bind(&model_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        if matches!(manual.as_deref(), Some("manual" | "admin" | "user")) {
            continue;
        }
        llmroller_store::prices::upsert_price_tx(
            &mut tx,
            llmroller_store::prices::UpsertPriceParams {
                provider_id,
                provider_kind: kind,
                upstream_model_id: model_id,
                rates: template.rates,
                tiers: template.tiers.clone(),
                schedules: template.schedules.clone(),
                source: "synced".to_string(),
                now_ms: chrono_like_now_ms(),
            },
        )
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        synced += 1;
    }
    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "synced": true,
        "prices_synced": synced,
        "imported_aliases": imported_aliases
    })))
}

fn chrono_like_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

async fn get_user_client_key_sessions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let key = llmroller_store::client_keys::get_client_key(&state.auth.pool, &actor, id)
        .await
        .map_err(|_| map_price_error(StatusCode::NOT_FOUND, "client key not found"))?;
    if Some(key.owner_user_id) != actor.user_id() {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "client key not found",
        ));
    }

    list_client_key_sessions_internal(&state, &id).await
}

async fn get_admin_client_key_sessions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    list_client_key_sessions_internal(&state, &id).await
}

async fn list_client_key_sessions_internal(
    state: &ApiAppState,
    client_key_id: &ClientKeyId,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use sqlx::Row as _;
    let rows = sqlx::query(
        r#"SELECT session_hash, MAX(ts) as last_active_at, COUNT(*) as traced_turn_count
           FROM trace_records
           WHERE client_key_id = ?
           GROUP BY session_hash
           ORDER BY last_active_at DESC
           LIMIT 50"#,
    )
    .bind(client_key_id.to_string())
    .fetch_all(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Live pins are read from the durable `pins` table (not the hot-path store, which
    // is keyed for routing) so the drawer can show exactly which upstream key and
    // canonical model each traced session is pinned to (`USG-9`, Section 14.t).
    let now_ms = state.auth.clock.now_ms();
    let pin_rows = sqlx::query(
        r#"SELECT p.session_hash, p.upstream_key_id, m.canonical_id, p.pool_tier,
                  p.last_used_at, k.label AS key_label
           FROM pins p
           JOIN models m ON m.model_pk = p.model_pk
           JOIN upstream_keys k ON k.id = p.upstream_key_id
           WHERE p.client_key_id = ? AND p.expires_at > ?
           ORDER BY p.last_used_at DESC"#,
    )
    .bind(client_key_id.to_string())
    .bind(now_ms)
    .fetch_all(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut pins_by_session: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    let mut pin_last_used: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for pin in pin_rows {
        let session_hash: String = pin.try_get("session_hash").unwrap_or_default();
        let upstream_key_id: String = pin.try_get("upstream_key_id").unwrap_or_default();
        let canonical_id: String = pin.try_get("canonical_id").unwrap_or_default();
        let key_label: String = pin.try_get("key_label").unwrap_or_default();
        let pool_tier: i64 = pin.try_get("pool_tier").unwrap_or(0);
        let last_used_at: i64 = pin.try_get("last_used_at").unwrap_or(0);
        pin_last_used.insert(session_hash.clone(), last_used_at);
        pins_by_session
            .entry(session_hash)
            .or_default()
            .push(serde_json::json!({
                "upstream_key_id": upstream_key_id,
                "canonical_id": canonical_id,
                "pool_tier": pool_tier,
                "key_label": key_label,
            }));
    }

    // Union both sources: a session that already holds a live pin but has not been
    // traced yet (traceability disabled, or the first turn still in flight) must still
    // appear in the drawer with its pin, and a traced session must keep its history.
    let mut sessions: std::collections::BTreeMap<String, (i64, i64)> =
        std::collections::BTreeMap::new();
    for r in rows {
        let session_hash: String = r.try_get("session_hash").unwrap_or_default();
        let last_active_at: i64 = r.try_get("last_active_at").unwrap_or(0);
        let traced_turn_count: i64 = r.try_get("traced_turn_count").unwrap_or(0);
        sessions.insert(session_hash, (last_active_at, traced_turn_count));
    }
    for session_hash in pins_by_session.keys() {
        let last_used_at = pin_last_used.get(session_hash).copied().unwrap_or(0);
        sessions
            .entry(session_hash.clone())
            .or_insert((last_used_at, 0));
    }
    let mut sessions = sessions.into_iter().collect::<Vec<_>>();
    sessions.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
    sessions.truncate(50);

    let data = sessions
        .into_iter()
        .map(|(session_hash, (last_active_at, traced_turn_count))| {
            let current_pins = pins_by_session.remove(&session_hash).unwrap_or_default();
            serde_json::json!({
                "session_hash": session_hash,
                "last_active_at": last_active_at,
                "traced_turn_count": traced_turn_count,
                "current_pins": current_pins
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({ "data": data })))
}

async fn get_user_session_preview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let owner_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let exists = sqlx::query(
        "SELECT 1 FROM trace_records WHERE owner_user_id = ? AND session_hash = ? LIMIT 1",
    )
    .bind(owner_id.to_string())
    .bind(&session_hash)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if exists.is_none() {
        return Err(map_price_error(StatusCode::NOT_FOUND, "session not found"));
    }

    get_session_preview_internal(&state, &session_hash, Some(owner_id.to_string())).await
}

async fn get_admin_session_preview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let admin_id = actor.user_id().map(|u| u.to_string());
    let now_ms = state.auth.clock.now_ms();

    let recent_audit: bool = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM audit_log WHERE actor_user_id = ? AND action = 'trace.viewed' AND target_id = ? AND ts >= ? LIMIT 1",
    )
    .bind(&admin_id)
    .bind(&session_hash)
    .bind(now_ms - 30 * 60 * 1000)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .is_some();

    if !recent_audit {
        let _ = append_audit_entry_helper(
            &state,
            now_ms,
            AuditEnvelope {
                actor_user_id: admin_id.as_deref(),
                ip_addr: None,
                action: "trace.viewed",
                target_type: "session",
                target_id: Some(&session_hash),
                detail_json: "{}",
            },
        )
        .await;
    }

    get_session_preview_internal(&state, &session_hash, None).await
}

/// Escape every string leaf so preview JSON never carries executable markup (SEC-19).
fn escape_json_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = escape_html(text),
        serde_json::Value::Array(items) => items.iter_mut().for_each(escape_json_strings),
        serde_json::Value::Object(fields) => fields.values_mut().for_each(escape_json_strings),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

async fn get_session_preview_internal(
    state: &ApiAppState,
    session_hash: &str,
    owner_user_id: Option<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_core::chat_preview::{compute_lcp, sanitize_assistant_text};
    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::SecretSealer as _;
    use llmroller_store::traces::StoredTurnTraceV2;
    use sqlx::Row as _;

    let query_str = if owner_user_id.is_some() {
        "SELECT usage_event_id, request_id, ts, owner_user_id, client_key_id, session_hash,
                thread_id, canonical_id, protocol, truncated_content, payload_enc, master_key_id
         FROM trace_records
         WHERE session_hash = ? AND owner_user_id = ?
         ORDER BY ts ASC, request_id ASC"
    } else {
        "SELECT usage_event_id, request_id, ts, owner_user_id, client_key_id, session_hash,
                thread_id, canonical_id, protocol, truncated_content, payload_enc, master_key_id
         FROM trace_records
         WHERE session_hash = ?
         ORDER BY ts ASC, request_id ASC"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(query_str)
            .bind(session_hash)
            .bind(uid)
            .fetch_all(&state.auth.pool)
            .await
    } else {
        sqlx::query(query_str)
            .bind(session_hash)
            .fetch_all(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "session trace not found",
        ));
    }

    let sealer = load_sealer(state).await?;

    let mut timeline = Vec::new();
    let mut prev_block_refs: Vec<String> = Vec::new();
    let mut prev_thread_id = String::new();
    let mut first_owner_user_id = String::new();
    let mut first_client_key_id = String::new();

    for (idx, r) in rows.iter().enumerate() {
        let req_id: String = r.try_get("request_id").unwrap_or_default();
        let ts: i64 = r.try_get("ts").unwrap_or(0);
        let row_owner: String = r.try_get("owner_user_id").unwrap_or_default();
        let client_key_id: String = r.try_get("client_key_id").unwrap_or_default();
        let thread_id: String = r
            .try_get("thread_id")
            .unwrap_or_else(|_| "main".to_string());
        let canonical_model: String = r.try_get("canonical_id").unwrap_or_default();
        let protocol: String = r.try_get("protocol").unwrap_or_default();
        let truncated_content: bool = r.try_get::<i64, _>("truncated_content").unwrap_or(0) == 1;
        let payload_enc: Vec<u8> = r.try_get("payload_enc").unwrap_or_default();
        let master_key_id: String = r.try_get("master_key_id").unwrap_or_default();

        if idx == 0 {
            first_owner_user_id = row_owner.clone();
            first_client_key_id = client_key_id.clone();
        }

        let decrypted_trace = sealer
            .open(
                "trace_record",
                &[
                    master_key_id.as_bytes(),
                    req_id.as_bytes(),
                    row_owner.as_bytes(),
                    session_hash.as_bytes(),
                ],
                &payload_enc,
            )
            .map_err(|e| {
                map_price_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("open trace: {e:?}"),
                )
            })?;

        let decompressed_trace =
            zstd::bulk::decompress(decrypted_trace.expose_secret(), 10 * 1024 * 1024).map_err(
                |e| {
                    map_price_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("decompress trace: {e}"),
                    )
                },
            )?;

        let turn_trace: StoredTurnTraceV2 =
            serde_json::from_slice(&decompressed_trace).map_err(|e| {
                map_price_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("parse turn: {e}"),
                )
            })?;

        let lcp = if thread_id == prev_thread_id {
            compute_lcp(&prev_block_refs, &turn_trace.block_refs)
        } else {
            0
        };
        prev_block_refs = turn_trace.block_refs.clone();
        prev_thread_id = thread_id.clone();

        // Tool schemas participate in trace deduplication but are not chat
        // messages; expose them through the per-thread tools view instead of
        // rendering them as input turns (TRC-4, TRC-6).
        let tool_refs: HashSet<String> = turn_trace
            .request_skeleton
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|reference| reference.get("$blk")?.as_str().map(str::to_string))
            .collect();

        let mut new_input_messages = Vec::new();
        for hash in &turn_trace.block_refs[lcp..] {
            if tool_refs.contains(hash) {
                continue;
            }
            let block_row = sqlx::query(
                "SELECT payload_enc, master_key_id FROM trace_blocks WHERE owner_user_id = ? AND block_hash = ?",
            )
            .bind(&row_owner)
            .bind(hash)
            .fetch_optional(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

            if let Some(br) = block_row {
                let blk_enc: Vec<u8> = br.try_get("payload_enc").unwrap_or_default();
                let blk_mk: String = br.try_get("master_key_id").unwrap_or_default();
                let dec_blk = sealer
                    .open(
                        "trace_block",
                        &[blk_mk.as_bytes(), row_owner.as_bytes(), hash.as_bytes()],
                        &blk_enc,
                    )
                    .map_err(|e| {
                        map_price_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("open block: {e:?}"),
                        )
                    })?;

                let decomp_blk = zstd::bulk::decompress(dec_blk.expose_secret(), 10 * 1024 * 1024)
                    .map_err(|e| {
                        map_price_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("decompress block: {e}"),
                        )
                    })?;

                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&decomp_blk) {
                    new_input_messages.push(val);
                } else {
                    let text = String::from_utf8_lossy(&decomp_blk).to_string();
                    new_input_messages.push(serde_json::json!({ "role": "user", "content": text }));
                }
            } else {
                new_input_messages.push(serde_json::json!({
                    "role": "system",
                    "content": "content expired",
                    "expired": true
                }));
            }
        }

        let raw_assistant = turn_trace
            .response
            .get("assistant_text")
            .and_then(|v| v.as_str())
            .or_else(|| turn_trace.response.get("text").and_then(|v| v.as_str()))
            .unwrap_or("");
        let sanitized_assistant = sanitize_assistant_text(raw_assistant);

        let assistant_reasoning = turn_trace
            .response
            .get("assistant_reasoning")
            .and_then(|v| v.as_str())
            .map(escape_html);

        let mut assistant_tool_calls = turn_trace
            .response
            .get("assistant_tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for call in &mut assistant_tool_calls {
            escape_json_strings(call);
        }

        for message in &mut new_input_messages {
            escape_json_strings(message);
        }

        timeline.push(serde_json::json!({
            "ordinal": idx + 1,
            "request_id": escape_html(&req_id),
            "ts": ts,
            "thread_id": escape_html(&thread_id),
            "canonical_model": escape_html(&canonical_model),
            "protocol": escape_html(&protocol),
            "truncated_content": truncated_content,
            "new_input_messages": new_input_messages,
            "assistant_text": sanitized_assistant,
            "assistant_reasoning": assistant_reasoning,
            "assistant_tool_calls": assistant_tool_calls,
        }));
    }

    let pin_rows = sqlx::query(
        "SELECT p.upstream_key_id, m.canonical_id, p.pool_tier, k.label as key_label
         FROM pins p
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         WHERE p.session_hash = ?",
    )
    .bind(session_hash)
    .fetch_all(&state.auth.pool)
    .await
    .unwrap_or_default();

    let mut current_pins = Vec::new();
    for pr in pin_rows {
        current_pins.push(serde_json::json!({
            "upstream_key_id": pr.try_get::<Option<String>, _>("upstream_key_id").unwrap_or_default().map(|value| escape_html(&value)),
            "canonical_id": pr.try_get::<Option<String>, _>("canonical_id").unwrap_or_default().map(|value| escape_html(&value)),
            "pool_tier": pr.try_get::<i64, _>("pool_tier").unwrap_or(1),
            "key_label": pr.try_get::<Option<String>, _>("key_label").unwrap_or_default().map(|value| escape_html(&value)),
        }));
    }

    Ok(Json(serde_json::json!({
        "session_hash": escape_html(session_hash),
        "owner_user_id": escape_html(&first_owner_user_id),
        "client_key_id": escape_html(&first_client_key_id),
        "total_turns": timeline.len(),
        "current_pins": current_pins,
        "timeline": timeline,
    })))
}

async fn get_user_trace_record_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    get_trace_record_internal(&state, &request_id, Some(user_id.to_string())).await
}

async fn get_admin_trace_record_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_trace_record_internal(&state, &request_id, None).await
}

async fn get_trace_record_internal(
    state: &ApiAppState,
    request_id: &str,
    owner_user_id: Option<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::SecretSealer as _;
    use llmroller_store::traces::StoredTurnTraceV2;
    use sqlx::Row as _;

    let row = sqlx::query(
        "SELECT request_id, owner_user_id, session_hash, payload_enc, master_key_id
         FROM trace_records
         WHERE request_id = ?",
    )
    .bind(request_id)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "trace record not found"))?;

    let row_owner: String = row.try_get("owner_user_id").unwrap_or_default();
    if let Some(ref uid) = owner_user_id
        && &row_owner != uid
    {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "trace record not found",
        ));
    }

    let session_hash: String = row.try_get("session_hash").unwrap_or_default();
    let payload_enc: Vec<u8> = row.try_get("payload_enc").unwrap_or_default();
    let master_key_id: String = row.try_get("master_key_id").unwrap_or_default();

    let sealer = load_sealer(state).await?;
    let decrypted = sealer
        .open(
            "trace_record",
            &[
                master_key_id.as_bytes(),
                request_id.as_bytes(),
                row_owner.as_bytes(),
                session_hash.as_bytes(),
            ],
            &payload_enc,
        )
        .map_err(|e| {
            map_price_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("open trace: {e:?}"),
            )
        })?;

    let decompressed = zstd::bulk::decompress(decrypted.expose_secret(), 10 * 1024 * 1024)
        .map_err(|e| {
            map_price_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("decompress trace: {e}"),
            )
        })?;

    let turn_trace: StoredTurnTraceV2 = serde_json::from_slice(&decompressed).map_err(|e| {
        map_price_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("parse turn: {e}"),
        )
    })?;

    Ok(Json(serde_json::to_value(turn_trace).unwrap_or_default()))
}

async fn delete_admin_session_trace_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;

    use sqlx::Row as _;
    let row = sqlx::query("SELECT owner_user_id FROM trace_records WHERE session_hash = ? LIMIT 1")
        .bind(&session_hash)
        .fetch_optional(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let Some(r) = row else {
        return Ok(StatusCode::NO_CONTENT);
    };

    let owner_str: String = r.try_get("owner_user_id").unwrap_or_default();
    let owner_user_id = llmroller_core::ids::UserId::parse(&owner_str)
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")))?;

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    llmroller_store::traces::delete_session_trace_tx(&mut tx, &owner_user_id, &session_hash)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(StatusCode::NO_CONTENT)
}

async fn delete_admin_traces_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();
    let _ = sqlx::query("DELETE FROM trace_records WHERE expires_at <= ?")
        .bind(now_ms)
        .execute(&state.auth.pool)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

struct AuditEnvelope<'a> {
    actor_user_id: Option<&'a str>,
    ip_addr: Option<&'a str>,
    action: &'a str,
    target_type: &'a str,
    target_id: Option<&'a str>,
    detail_json: &'a str,
}

async fn append_audit_entry_helper(
    state: &ApiAppState,
    now_ms: i64,
    envelope: AuditEnvelope<'_>,
) -> Result<(), sqlx::Error> {
    use llmroller_core::seams::ExposeSecret as _;

    let audit_id = llmroller_core::ids::RequestId::generate().to_string();
    // Production always provides the instance audit key; tests without one derive a
    // deterministic fallback so the hash chain remains well-formed.
    let fallback = llmroller_store::crypto::sha256_hex(state.master_key.as_bytes());
    let key: &[u8] = state
        .audit_hmac_key
        .as_ref()
        .map_or(fallback.as_bytes(), |secret| secret.expose_secret());

    let mut tx = state.auth.pool.begin().await?;
    llmroller_store::audit::append(
        &mut tx,
        key,
        llmroller_store::audit::AuditEntry {
            id: &audit_id,
            ts: now_ms,
            actor_user_id: envelope.actor_user_id,
            ip_addr: envelope.ip_addr,
            action: envelope.action,
            target_type: envelope.target_type,
            target_id: envelope.target_id,
            detail_json: Some(envelope.detail_json),
        },
    )
    .await
    .map_err(|_| sqlx::Error::Protocol("audit append failed".to_string()))?;
    tx.commit().await
}

// ---------------------------------------------------------------------------
// Client Key In-Place Rotation (/api/user/client-keys/{id}/rotate)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct RotateKeyRequest {
    pub grace_period_secs: Option<u64>,
}

async fn post_rotate_client_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
    Json(payload): Json<RotateKeyRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let grace = payload.grace_period_secs.unwrap_or(86400) as i64;
    let now_ms = state.auth.clock.now_ms();
    let res = llmroller_store::client_keys::rotate_client_key(
        &state.auth.pool,
        &actor,
        id,
        grace,
        state.auth.entropy.as_ref(),
        now_ms,
    )
    .await
    .map_err(map_client_key_error)?;

    use llmroller_core::seams::ExposeSecret as _;
    Ok(Json(serde_json::json!({
        "client_key": res.client_key,
        "raw_token": res.raw_token.expose_secret()
    })))
}

// ---------------------------------------------------------------------------
// Sharing Endpoints (/api/user/shares)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct CreateShareRequest {
    pub provider_id: ProviderId,
    pub upstream_key_id: Option<UpstreamKeyId>,
    pub grantee_user_id: UserId,
    pub allowed_models: Option<Vec<String>>,
    pub max_monthly_micro_usd: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AcceptShareRequest {
    pub fingerprint: String,
}

async fn get_user_shares_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
    let outgoing_rows = sqlx::query(
        "SELECT id, provider_id, upstream_key_id, grantee_user_id, allowed_models, max_monthly_micro_usd, accepted_config_fingerprint, admin_approved_at, enabled, created_at, revoked_at FROM provider_shares WHERE owner_user_id = ? ORDER BY created_at DESC"
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut outgoing = Vec::new();
    for r in outgoing_rows {
        outgoing.push(serde_json::json!({
            "id": r.try_get::<String, _>("id").unwrap_or_default(),
            "provider_id": r.try_get::<String, _>("provider_id").unwrap_or_default(),
            "upstream_key_id": r.try_get::<Option<String>, _>("upstream_key_id").unwrap_or_default(),
            "grantee_user_id": r.try_get::<String, _>("grantee_user_id").unwrap_or_default(),
            "allowed_models": r.try_get::<Option<String>, _>("allowed_models").unwrap_or_default(),
            "max_monthly_micro_usd": r.try_get::<Option<i64>, _>("max_monthly_micro_usd").unwrap_or_default(),
            "accepted_config_fingerprint": r.try_get::<Option<String>, _>("accepted_config_fingerprint").unwrap_or_default(),
            "enabled": r.try_get::<i64, _>("enabled").unwrap_or_default() == 1,
            "created_at": r.try_get::<i64, _>("created_at").unwrap_or_default(),
        }));
    }

    let incoming_rows = sqlx::query(
        "SELECT s.id, s.provider_id, s.upstream_key_id, s.owner_user_id, s.allowed_models, s.max_monthly_micro_usd, s.accepted_config_fingerprint, s.admin_approved_at, s.enabled, p.name as provider_name, p.kind as provider_kind, p.share_config_fingerprint as current_fingerprint FROM provider_shares s JOIN providers p ON s.provider_id = p.id WHERE s.grantee_user_id = ? ORDER BY s.created_at DESC"
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut incoming = Vec::new();
    for r in incoming_rows {
        incoming.push(serde_json::json!({
            "id": r.try_get::<String, _>("id").unwrap_or_default(),
            "provider_id": r.try_get::<String, _>("provider_id").unwrap_or_default(),
            "provider_name": r.try_get::<String, _>("provider_name").unwrap_or_default(),
            "provider_kind": r.try_get::<String, _>("provider_kind").unwrap_or_default(),
            "upstream_key_id": r.try_get::<Option<String>, _>("upstream_key_id").unwrap_or_default(),
            "owner_user_id": r.try_get::<String, _>("owner_user_id").unwrap_or_default(),
            "allowed_models": r.try_get::<Option<String>, _>("allowed_models").unwrap_or_default(),
            "max_monthly_micro_usd": r.try_get::<Option<i64>, _>("max_monthly_micro_usd").unwrap_or_default(),
            "accepted_config_fingerprint": r.try_get::<Option<String>, _>("accepted_config_fingerprint").unwrap_or_default(),
            "current_fingerprint": r.try_get::<String, _>("current_fingerprint").unwrap_or_default(),
            "enabled": r.try_get::<i64, _>("enabled").unwrap_or_default() == 1,
        }));
    }

    Ok(Json(serde_json::json!({
        "outgoing": outgoing,
        "incoming": incoming,
    })))
}

async fn post_user_share_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateShareRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let share = llmroller_store::shares::create_provider_share_tx(
        &mut tx,
        &user_id,
        llmroller_store::shares::NewShare {
            provider_id: payload.provider_id,
            upstream_key_id: payload.upstream_key_id,
            grantee_user_id: payload.grantee_user_id,
            allowed_models: payload.allowed_models,
            max_monthly_micro_usd: payload.max_monthly_micro_usd,
        },
        now_ms,
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e:?}")))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "share_id": share.share_id.to_string() })),
    ))
}

async fn post_accept_share_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ShareId>,
    Json(payload): Json<AcceptShareRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    llmroller_store::shares::accept_provider_share_tx(
        &mut tx,
        &id,
        &user_id,
        &payload.fingerprint,
        now_ms,
    )
    .await
    .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e:?}")))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if headers.contains_key("hx-request") {
        use axum::response::IntoResponse as _;
        Ok(Html("<span class=\"badge badge-success\">Accepted</span>").into_response())
    } else {
        use axum::response::IntoResponse as _;
        Ok(StatusCode::OK.into_response())
    }
}

async fn delete_user_share_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ShareId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    llmroller_store::shares::revoke_provider_share_tx(&mut tx, &id, &user_id, now_ms)
        .await
        .map_err(|e| map_price_error(StatusCode::NOT_FOUND, format!("{e:?}")))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if headers.contains_key("hx-request") {
        Ok(StatusCode::OK)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

// ---------------------------------------------------------------------------
// User Management Endpoints (/admin/api/users)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub role: Option<String>,
    pub must_change_password: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PatchUserRequest {
    pub role: Option<String>,
    pub disabled: Option<bool>,
    pub can_use_global_providers: Option<bool>,
    pub global_budget_micro_usd_month: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteUserParams {
    pub confirm: Option<String>,
}

async fn get_admin_users_handler(
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

async fn post_admin_user_handler(
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

async fn patch_admin_user_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UserId>,
    Json(payload): Json<PatchUserRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let now_ms = state.auth.clock.now_ms();

    if let Some(ref role_str) = payload.role {
        let role = match role_str.as_str() {
            "admin" => llmroller_core::rbac::Role::Admin,
            _ => llmroller_core::rbac::Role::User,
        };
        llmroller_store::users::set_role(&state.auth.pool, &actor, id, role, now_ms)
            .await
            .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;
    }

    if let Some(disabled) = payload.disabled {
        llmroller_store::users::set_disabled(&state.auth.pool, &actor, id, disabled, now_ms)
            .await
            .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e}")))?;
    }

    if let Some(can_use) = payload.can_use_global_providers {
        let _ = sqlx::query("UPDATE users SET can_use_global_providers = ? WHERE id = ?")
            .bind(if can_use { 1i64 } else { 0i64 })
            .bind(id.to_string())
            .execute(&state.auth.pool)
            .await;
    }

    if let Some(budget_opt) = payload.global_budget_micro_usd_month {
        let _ = sqlx::query("UPDATE users SET global_budget_micro_usd_month = ? WHERE id = ?")
            .bind(budget_opt)
            .bind(id.to_string())
            .execute(&state.auth.pool)
            .await;
    }

    Ok(StatusCode::OK)
}

async fn delete_admin_user_handler(
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

// ---------------------------------------------------------------------------
// Model Catalog & Aliases (/admin/api/models)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct AddAliasRequest {
    pub alias: String,
}

#[derive(Debug, Deserialize)]
pub struct PatchModelRequest {
    pub exposed: Option<i64>,
}

async fn get_admin_models_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    use sqlx::Row as _;
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

async fn post_admin_models_refresh_handler(
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

async fn post_admin_model_alias_handler(
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

async fn patch_admin_model_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ModelPk>,
    Json(payload): Json<PatchModelRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let _ = sqlx::query("UPDATE models SET exposed = ? WHERE model_pk = ?")
        .bind(payload.exposed)
        .bind(id.to_string())
        .execute(&state.auth.pool)
        .await;
    Ok(StatusCode::OK)
}

async fn post_admin_model_confirm_merge_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ModelPk>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let res = sqlx::query("UPDATE models SET merge_state = 'confirmed' WHERE model_pk = ?")
        .bind(id.to_string())
        .execute(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if res.rows_affected() == 0 {
        return Err(map_price_error(StatusCode::NOT_FOUND, "model not found"));
    }
    Ok(StatusCode::OK)
}

#[derive(Debug, Deserialize)]
pub struct DeleteAliasParams {
    pub alias: Option<String>,
}

async fn delete_admin_model_alias_handler(
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
    sqlx::query("DELETE FROM model_aliases WHERE alias = ?")
        .bind(alias)
        .execute(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_admin_share_approve_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<llmroller_core::ids::ShareId>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, true).await?;
    let _admin_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let now_ms = state.auth.clock.now_ms();

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    llmroller_store::shares::approve_provider_share_tx(&mut tx, &id, now_ms)
        .await
        .map_err(|e| map_price_error(StatusCode::BAD_REQUEST, format!("{e:?}")))?;

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(StatusCode::OK)
}

async fn get_admin_overview_telemetry_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    let now_ms = state.auth.clock.now_ms();

    let settings = llmroller_store::settings::load_settings(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let retention_days = settings.traceability_config.retention_days;

    let daily_requests: i64 =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_events WHERE ts >= ?")
            .bind(now_ms - 86400 * 1000)
            .fetch_one(&state.auth.pool)
            .await
            .unwrap_or(0)
            .max(1000);

    let trace_bytes: i64 =
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(SUM(byte_len), 0) FROM trace_blocks")
            .fetch_one(&state.auth.pool)
            .await
            .unwrap_or(0);

    let projected_bytes =
        ((daily_requests as f64) * (retention_days as f64) * 600.0 * 1.3) + (trace_bytes as f64);
    let projected_disk_mb = (projected_bytes / (1024.0 * 1024.0) * 10.0).round() / 10.0;

    let unpriced_count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT r.model_pk) FROM model_routes r
         WHERE r.enabled = 1 AND NOT EXISTS (
            SELECT 1 FROM prices p WHERE p.upstream_model_id = r.upstream_model_id
         )",
    )
    .fetch_one(&state.auth.pool)
    .await
    .unwrap_or(0);

    let usage_events_count: i64 = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&state.auth.pool)
        .await
        .unwrap_or(0);

    Ok(Json(serde_json::json!({
        "projected_disk_mb": projected_disk_mb,
        "unpriced_models_count": unpriced_count,
        "usage_events_count": usage_events_count,
        "max_usage_events_rows": 50_000_000i64,
        "retention_days": retention_days,
        "daily_requests": daily_requests
    })))
}

// ---------------------------------------------------------------------------
// Audit Log Viewer (/admin/api/audit-log)
// ---------------------------------------------------------------------------
async fn get_admin_audit_log_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT seq, ts, actor_user_id, action, target_type, target_id, detail_json, hash AS hash_hex FROM audit_log ORDER BY seq DESC LIMIT 100"
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut list = Vec::new();
    for r in rows {
        list.push(serde_json::json!({
            "seq": r.try_get::<i64, _>("seq").unwrap_or_default(),
            "ts": r.try_get::<i64, _>("ts").unwrap_or_default(),
            "actor_user_id": r.try_get::<Option<String>, _>("actor_user_id").unwrap_or_default(),
            "action": r.try_get::<String, _>("action").unwrap_or_default(),
            "target_type": r.try_get::<String, _>("target_type").unwrap_or_default(),
            "target_id": r.try_get::<Option<String>, _>("target_id").unwrap_or_default(),
            "payload_json": r.try_get::<String, _>("detail_json").unwrap_or_default(),
            "detail_json": r.try_get::<String, _>("detail_json").unwrap_or_default(),
            "hash": r.try_get::<String, _>("hash_hex").unwrap_or_default(),
        }));
    }

    Ok(Json(list))
}

// ---------------------------------------------------------------------------
// Active Pins & Sessions (/api/user/pins, /admin/api/pins)
// ---------------------------------------------------------------------------
async fn get_active_pins_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;

    use sqlx::Row as _;
    let query_str = if actor.is_admin() {
        "SELECT p.session_hash, p.client_key_id, p.model_pk, p.upstream_key_id, p.pool_tier, p.created_at, p.last_used_at, m.canonical_id, k.label as key_label
         FROM pins p
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         ORDER BY p.last_used_at DESC LIMIT 50"
    } else {
        "SELECT p.session_hash, p.client_key_id, p.model_pk, p.upstream_key_id, p.pool_tier, p.created_at, p.last_used_at, m.canonical_id, k.label as key_label
         FROM pins p
         JOIN client_keys ck ON p.client_key_id = ck.id
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         WHERE ck.owner_user_id = ?
         ORDER BY p.last_used_at DESC LIMIT 50"
    };

    let rows = if actor.is_admin() {
        sqlx::query(query_str).fetch_all(&state.auth.pool).await
    } else {
        let uid = actor.user_id().map(|u| u.to_string()).unwrap_or_default();
        sqlx::query(query_str)
            .bind(uid)
            .fetch_all(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut list = Vec::new();
    for r in rows {
        list.push(serde_json::json!({
            "session_hash": r.try_get::<String, _>("session_hash").unwrap_or_default(),
            "client_key_id": r.try_get::<String, _>("client_key_id").unwrap_or_default(),
            "canonical_id": r.try_get::<Option<String>, _>("canonical_id").unwrap_or_default(),
            "key_label": r.try_get::<Option<String>, _>("key_label").unwrap_or_default(),
            "pool_tier": r.try_get::<i64, _>("pool_tier").unwrap_or(1),
            "created_at": r.try_get::<i64, _>("created_at").unwrap_or(0),
            "last_used_at": r.try_get::<i64, _>("last_used_at").unwrap_or(0),
        }));
    }

    Ok(Json(list))
}

fn pin_delete_status(headers: &HeaderMap) -> StatusCode {
    if headers.contains_key("hx-request") {
        StatusCode::OK
    } else {
        StatusCode::NO_CONTENT
    }
}

/// Admin-wide unpin: every tenant's pin for the session hash is removed.
///
/// `SEC-17`: only administrators may delete pins they do not own.
async fn delete_pin_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    sqlx::query("DELETE FROM pins WHERE session_hash = ?")
        .bind(&hash)
        .execute(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if let Some(store) = &state.pin_store {
        store.remove_by_session_hash(&hash);
    }
    Ok(pin_delete_status(&headers))
}

/// Owner-scoped unpin for the session detail UI.
///
/// `SEC-17` / `USR-3`: the SQL predicate derives ownership from the client key
/// join, and a cross-tenant attempt is an indistinguishable no-op. Only the
/// exact live pin is evicted from the shared `PinStore`.
async fn delete_user_pin_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path((client_key_id, session_hash, canonical_id)): Path<(ClientKeyId, String, String)>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let owner_user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Forbidden))?
        .to_string();

    // `USR-3`: a foreign client key is indistinguishable from a missing one.
    let owns_client_key: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM client_keys WHERE id = ? AND owner_user_id = ?")
            .bind(client_key_id.to_string())
            .bind(&owner_user_id)
            .fetch_optional(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if owns_client_key.is_none() {
        return Err(map_price_error(StatusCode::NOT_FOUND, "not_found"));
    }

    let result = sqlx::query(
        "DELETE FROM pins
         WHERE client_key_id = ?
           AND session_hash = ?
           AND model_pk = (SELECT model_pk FROM models WHERE canonical_id = ?)
           AND client_key_id IN (SELECT id FROM client_keys WHERE owner_user_id = ?)",
    )
    .bind(client_key_id.to_string())
    .bind(&session_hash)
    .bind(&canonical_id)
    .bind(owner_user_id)
    .execute(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if result.rows_affected() > 0
        && let Some(store) = &state.pin_store
    {
        store.remove(&llmroller_core::pins::PinKey {
            client_key_id,
            canonical_id,
            session_hash,
        });
    }
    Ok(pin_delete_status(&headers))
}

// ---------------------------------------------------------------------------
// User Routing Matrix (/api/user/routes)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct PutRoutePreferenceRequest {
    pub priority_override: Option<u32>,
    pub enabled: Option<bool>,
}

async fn get_user_routes_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
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
                let is_shared = sqlx::query_scalar::<_, i64>(
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
                     LIMIT 1",
                )
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

async fn put_user_route_preference_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path((key_id, canonical_id)): Path<(UpstreamKeyId, String)>,
    Json(payload): Json<PutRoutePreferenceRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
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
        let is_shared = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM provider_shares s
             JOIN users u ON u.id = s.owner_user_id
             WHERE s.grantee_user_id = ? AND s.provider_id = ?
               AND s.enabled = 1 AND s.revoked_at IS NULL
               AND s.accepted_at IS NOT NULL
               AND s.accepted_config_fingerprint IS NOT NULL
               AND s.accepted_config_fingerprint = ?
               AND u.disabled = 0
               AND (s.upstream_key_id IS NULL OR s.upstream_key_id = ?)
             LIMIT 1",
        )
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
        let allowed = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM provider_shares s
             WHERE s.grantee_user_id = ? AND s.provider_id = ?
               AND s.enabled = 1 AND s.revoked_at IS NULL
               AND s.accepted_at IS NOT NULL
               AND (s.upstream_key_id IS NULL OR s.upstream_key_id = ?)
               AND (s.allowed_models IS NULL OR EXISTS (
                    SELECT 1 FROM json_each(s.allowed_models) WHERE value = ?))
             LIMIT 1",
        )
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
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyTotpRequest {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct DisableTotpRequest {
    pub password: Option<String>,
}

async fn post_user_password_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<ChangePasswordRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;

    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session_token =
        crate::auth::parse_cookie_value(cookie_header, state.auth.config.session_cookie_name())
            .unwrap_or("");
    let id_hash = llmroller_store::crypto::sha256_hex(session_token.as_bytes());

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

async fn load_sealer(
    state: &ApiAppState,
) -> Result<llmroller_store::crypto::XChaCha20Poly1305Sealer, (StatusCode, Json<serde_json::Value>)>
{
    let instance_id_vec: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_one(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let instance_id: [u8; 16] = instance_id_vec
        .try_into()
        .map_err(|_| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, "corrupt instance_id"))?;
    Ok(llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
        state.master_key.clone(),
        instance_id,
        state.auth.entropy.clone(),
    ))
}

async fn post_user_totp_enroll_handler(
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

async fn post_user_totp_verify_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<VerifyTotpRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let sealer = load_sealer(&state).await?;
    let now_ms = state.auth.clock.now_ms();

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

async fn post_user_totp_disable_handler(
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
    use sqlx::Row as _;
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

async fn delete_user_session_handler(
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
// Upstream Key Test (PRV-6)
// ---------------------------------------------------------------------------
async fn post_test_upstream_key_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<UpstreamKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;

    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::SecretSealer as _;
    use sqlx::Row as _;
    let key = sqlx::query(
        "SELECT k.id, k.owner_user_id, k.secret_enc, k.master_key_id, p.id AS provider_id,
                p.ownership_scope, p.base_url, p.kind, p.allow_private_network
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

    let target = format!("{}/models", base_url.trim_end_matches('/'));
    if llmroller_core::ssrf::validate_url(&target, allow_private).is_err() {
        return Err(map_price_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "provider URL is not allowed",
        ));
    }
    let client = llmroller_proxy::ssrf::create_guarded_client(allow_private)
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let started = std::time::Instant::now();
    let response = client
        .get(&target)
        .header("authorization", format!("Bearer {secret}"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;
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

// ---------------------------------------------------------------------------
// Usage Explorer & CSV Export (USG-1..USG-3, SEC-6)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub struct UsageFilterParams {
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: Option<i64>,
    pub after: Option<String>,
}

async fn get_user_usage_summary_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    get_usage_summary_internal(&state, Some(user_id.to_string()), params).await
}

async fn get_admin_usage_summary_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_summary_internal(&state, None, params).await
}

async fn get_usage_summary_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    params: UsageFilterParams,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use sqlx::Row as _;
    let query_str = if owner_user_id.is_some() {
        "SELECT COUNT(*) as total_requests,
                COALESCE(SUM(input_uncached_tokens), 0) as input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) as cached_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(cost_micro_usd), 0) as total_cost_micro_usd
         FROM usage_events
         WHERE owner_user_id = ?
           AND (? IS NULL OR ts >= ?)
           AND (? IS NULL OR ts <= ?)"
    } else {
        "SELECT COUNT(*) as total_requests,
                COALESCE(SUM(input_uncached_tokens), 0) as input_tokens,
                COALESCE(SUM(cache_read_tokens), 0) as cached_tokens,
                COALESCE(SUM(output_tokens), 0) as output_tokens,
                COALESCE(SUM(cost_micro_usd), 0) as total_cost_micro_usd
         FROM usage_events
         WHERE (? IS NULL OR ts >= ?)
           AND (? IS NULL OR ts <= ?)"
    };

    let row = if let Some(ref uid) = owner_user_id {
        sqlx::query(query_str)
            .bind(uid)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_one(&state.auth.pool)
            .await
    } else {
        sqlx::query(query_str)
            .bind(params.since)
            .bind(params.since)
            .bind(params.until)
            .bind(params.until)
            .fetch_one(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let total_requests: i64 = row.try_get("total_requests").unwrap_or(0);
    let input_tokens: i64 = row.try_get("input_tokens").unwrap_or(0);
    let cached_tokens: i64 = row.try_get("cached_tokens").unwrap_or(0);
    let output_tokens: i64 = row.try_get("output_tokens").unwrap_or(0);
    let total_cost_micro_usd: i64 = row.try_get("total_cost_micro_usd").unwrap_or(0);

    let total_input = (input_tokens + cached_tokens).max(1) as f64;
    let cache_hit_rate = (cached_tokens as f64 / total_input * 1000.0).round() / 1000.0;

    Ok(Json(serde_json::json!({
        "total_requests": total_requests,
        "input_tokens": input_tokens,
        "cached_tokens": cached_tokens,
        "output_tokens": output_tokens,
        "total_cost_micro_usd": total_cost_micro_usd,
        "cache_hit_rate": cache_hit_rate,
    })))
}

async fn get_user_usage_events_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    get_usage_events_internal(&state, Some(user_id.to_string()), params).await
}

async fn get_admin_usage_events_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<UsageFilterParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    get_usage_events_internal(&state, None, params).await
}

async fn get_usage_events_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
    params: UsageFilterParams,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let limit = params.limit.unwrap_or(50).clamp(1, 5000);
    use sqlx::Row as _;

    let query_str = if owner_user_id.is_some() {
        "SELECT id, ts, request_id, client_key_id, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, cache_read_tokens, output_tokens, error_code
         FROM usage_events
         WHERE owner_user_id = ?
           AND (? IS NULL OR id < ?)
         ORDER BY id DESC
         LIMIT ?"
    } else {
        "SELECT id, ts, request_id, client_key_id, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, cache_read_tokens, output_tokens, error_code
         FROM usage_events
         WHERE (? IS NULL OR id < ?)
         ORDER BY id DESC
         LIMIT ?"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(query_str)
            .bind(uid)
            .bind(&params.after)
            .bind(&params.after)
            .bind(limit)
            .fetch_all(&state.auth.pool)
            .await
    } else {
        sqlx::query(query_str)
            .bind(&params.after)
            .bind(&params.after)
            .bind(limit)
            .fetch_all(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut events = Vec::new();
    let mut last_id = None;

    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        last_id = Some(id.clone());
        events.push(serde_json::json!({
            "id": id,
            "ts": r.try_get::<i64, _>("ts").unwrap_or(0),
            "request_id": r.try_get::<String, _>("request_id").unwrap_or_default(),
            "client_key_id": r.try_get::<String, _>("client_key_id").unwrap_or_default(),
            "canonical_id": r.try_get::<Option<String>, _>("canonical_id").unwrap_or_default(),
            "status": r.try_get::<i64, _>("status").unwrap_or(0),
            "cost_micro_usd": r.try_get::<i64, _>("cost_micro_usd").unwrap_or(0),
            "input_tokens": r.try_get::<Option<i64>, _>("input_uncached_tokens").unwrap_or_default(),
            "cached_tokens": r.try_get::<Option<i64>, _>("cache_read_tokens").unwrap_or_default(),
            "output_tokens": r.try_get::<Option<i64>, _>("output_tokens").unwrap_or_default(),
            "error_code": r.try_get::<Option<String>, _>("error_code").unwrap_or_default(),
        }));
    }

    Ok(Json(serde_json::json!({
        "events": events,
        "next_cursor": last_id,
    })))
}

async fn get_user_usage_export_csv_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    export_csv_internal(&state, Some(user_id.to_string())).await
}

async fn get_admin_usage_export_csv_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    export_csv_internal(&state, None).await
}

async fn export_csv_internal(
    state: &ApiAppState,
    owner_user_id: Option<String>,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_proto::headers::sanitize_csv_cell;
    use sqlx::Row as _;

    let query_str = if owner_user_id.is_some() {
        "SELECT id, ts, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, cache_read_tokens, output_tokens, error_code
         FROM usage_events
         WHERE owner_user_id = ?
         ORDER BY id DESC
         LIMIT 5000"
    } else {
        "SELECT id, ts, canonical_id, status, cost_micro_usd,
                input_uncached_tokens, cache_read_tokens, output_tokens, error_code
         FROM usage_events
         ORDER BY id DESC
         LIMIT 5000"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(query_str)
            .bind(uid)
            .fetch_all(&state.auth.pool)
            .await
    } else {
        sqlx::query(query_str).fetch_all(&state.auth.pool).await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut csv = String::from(
        "id,ts,canonical_id,status,cost_micro_usd,input_tokens,cached_tokens,output_tokens,error_code\r\n",
    );
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let ts: i64 = r.try_get("ts").unwrap_or(0);
        let canonical_id: String = r
            .try_get::<Option<String>, _>("canonical_id")
            .unwrap_or_default()
            .unwrap_or_default();
        let status: i64 = r.try_get("status").unwrap_or(0);
        let cost: i64 = r.try_get("cost_micro_usd").unwrap_or(0);
        let input: i64 = r
            .try_get::<Option<i64>, _>("input_uncached_tokens")
            .unwrap_or_default()
            .unwrap_or(0);
        let cached: i64 = r
            .try_get::<Option<i64>, _>("cache_read_tokens")
            .unwrap_or_default()
            .unwrap_or(0);
        let output: i64 = r
            .try_get::<Option<i64>, _>("output_tokens")
            .unwrap_or_default()
            .unwrap_or(0);
        let error_code: String = r
            .try_get::<Option<String>, _>("error_code")
            .unwrap_or_default()
            .unwrap_or_default();

        let line = format!(
            "{},{},{},{},{},{},{},{},{}\r\n",
            sanitize_csv_cell(&id),
            ts,
            sanitize_csv_cell(&canonical_id),
            status,
            cost,
            input,
            cached,
            output,
            sanitize_csv_cell(&error_code),
        );
        csv.push_str(&line);
    }

    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            "attachment; filename=\"usage_export.csv\"",
        )
        .body(axum::body::Body::from(csv))
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(response)
}

// ---------------------------------------------------------------------------
// HTMX Partial Endpoints (SRDD Section 14.3)
// ---------------------------------------------------------------------------

async fn get_partial_pins_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let is_admin = actor.is_admin();
    let uid = actor.user_id().map(|u| u.to_string()).unwrap_or_default();

    use sqlx::Row as _;
    let query_str = if is_admin {
        "SELECT p.session_hash, p.client_key_id, p.model_pk, p.upstream_key_id, p.pool_tier, p.created_at, p.last_used_at, m.canonical_id, k.label as key_label
         FROM pins p
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         ORDER BY p.last_used_at DESC LIMIT 50"
    } else {
        "SELECT p.session_hash, p.client_key_id, p.model_pk, p.upstream_key_id, p.pool_tier, p.created_at, p.last_used_at, m.canonical_id, k.label as key_label
         FROM pins p
         JOIN client_keys ck ON p.client_key_id = ck.id
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         WHERE ck.owner_user_id = ?
         ORDER BY p.last_used_at DESC LIMIT 50"
    };

    let rows = if is_admin {
        sqlx::query(query_str).fetch_all(&state.auth.pool).await
    } else {
        sqlx::query(query_str)
            .bind(&uid)
            .fetch_all(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"7\" class=\"text-center text-muted\">No active sessions pinned right now.</td></tr>".to_string()
        ));
    }

    let mut html = String::new();
    for r in rows {
        let hash: String = r.try_get("session_hash").unwrap_or_default();
        let short_hash = if hash.len() > 12 { &hash[..12] } else { &hash };
        let short_hash = escape_html(short_hash);
        let clk: String = r.try_get("client_key_id").unwrap_or_default();
        let short_clk = if clk.len() > 10 { &clk[..10] } else { &clk };
        let short_clk = escape_html(short_clk);
        let model: String = r
            .try_get::<Option<String>, _>("canonical_id")
            .unwrap_or_default()
            .unwrap_or_else(|| "unresolved".to_string());
        let model = escape_html(&model);
        let key_label: String = r
            .try_get::<Option<String>, _>("key_label")
            .unwrap_or_default()
            .unwrap_or_else(|| "key".to_string());
        let key_label = escape_html(&key_label);
        let pool_tier: i64 = r.try_get("pool_tier").unwrap_or(1);
        let last_used: i64 = r.try_get("last_used_at").unwrap_or(0);
        let time_str = format!(
            "{}s ago",
            (state.auth.clock.now_ms() - last_used).max(0) / 1000
        );
        let unpin_button = if is_admin {
            format!(
                "<button class=\"btn btn-xs btn-danger\" hx-delete=\"/admin/api/sessions/{hash}/pin\" hx-target=\"closest tr\" hx-swap=\"outerHTML\">Unpin</button>"
            )
        } else if model == "unresolved" {
            String::new()
        } else {
            format!(
                "<button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/pins/{clk}/{hash}/{model}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\">Unpin</button>"
            )
        };

        html.push_str(&format!(
            "<tr>\
                <td><code>{short_hash}...</code></td>\
                <td><code>{short_clk}...</code></td>\
                <td><strong>{model}</strong></td>\
                <td>{key_label}</td>\
                <td><span class=\"badge badge-info\">Tier {pool_tier}</span></td>\
                <td>{time_str}</td>\
                <td>\
                    <button class=\"btn btn-xs btn-primary\" data-action=\"preview-chat\" data-session-hash=\"{hash}\">Preview Chat</button>\
                    {unpin_button}\
                </td>\
            </tr>"
        ));
    }

    Ok(Html(html))
}

async fn get_partial_client_keys_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT id, prefix, label, routing_scope, rate_limit_rpm, budget_micro_usd_month, created_at
         FROM client_keys WHERE owner_user_id = ? ORDER BY created_at DESC",
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"7\" class=\"text-center text-muted\">No client keys issued yet. Click \"+ New Client Key\" above.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let prefix: String = r.try_get("prefix").unwrap_or_default();
        let label: String = r.try_get("label").unwrap_or_default();
        let scope: String = r.try_get("routing_scope").unwrap_or_default();
        let id = escape_html(&id);
        let prefix = escape_html(&prefix);
        let label = escape_html(&label);
        let scope = escape_html(&scope);
        let rpm: Option<i64> = r.try_get("rate_limit_rpm").unwrap_or_default();
        let rpm_str = rpm.map_or_else(|| "Unlimited".to_string(), |v| format!("{v} RPM"));
        let budget: Option<i64> = r.try_get("budget_micro_usd_month").unwrap_or_default();
        let budget_str = budget.map_or_else(
            || "No Cap".to_string(),
            |b| format!("${:.2}", b as f64 / 1_000_000.0),
        );

        html.push_str(&format!(
            "<tr>\
                <td><code>{prefix}...</code></td>\
                <td><strong>{label}</strong></td>\
                <td><span class=\"badge badge-info\">{scope}</span></td>\
                <td>{rpm_str}</td>\
                <td>{budget_str}</td>\
                <td>Active</td>\
                <td>\
                    <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/client-keys/{id}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Revoke this client key?\">Revoke</button>\
                </td>\
            </tr>"
        ));
    }

    Ok(Html(html))
}

async fn get_partial_providers_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
    let prov_rows = sqlx::query(
        "SELECT id, name, kind, base_url, enabled FROM providers WHERE owner_user_id = ? ORDER BY created_at ASC",
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if prov_rows.is_empty() {
        return Ok(Html(
            "<p class=\"text-muted\">No personal providers configured yet. Click \"+ Add Provider\" above.</p>".to_string(),
        ));
    }

    let mut html = String::new();
    for p in prov_rows {
        let pid: String = p.try_get("id").unwrap_or_default();
        let name: String = p.try_get("name").unwrap_or_default();
        let kind: String = p.try_get("kind").unwrap_or_default();
        let base_url: String = p.try_get("base_url").unwrap_or_default();
        let pid = escape_html(&pid);
        let name = escape_html(&name);
        let kind = escape_html(&kind);
        let base_url = escape_html(&base_url);

        let key_rows = sqlx::query(
            "SELECT id, label, secret_last4, priority, strict_tier, enabled FROM upstream_keys WHERE provider_id = ?",
        )
        .bind(&pid)
        .fetch_all(&state.auth.pool)
        .await
        .unwrap_or_default();

        let mut keys_html = String::new();
        if key_rows.is_empty() {
            keys_html = "<p class=\"text-muted\" style=\"font-size: 0.8125rem; padding: 0.5rem 0;\">No upstream keys under this provider. Add one to enable routing.</p>".to_string();
        } else {
            keys_html.push_str("<table class=\"data-table\" style=\"margin-top: 0.5rem; background: #080c18;\"><thead><tr><th>Label</th><th>Priority</th><th>Strict Tier</th><th>Health</th><th>Action</th></tr></thead><tbody>");
            for k in key_rows {
                let kid: String = k.try_get("id").unwrap_or_default();
                let klabel: String = k.try_get("label").unwrap_or_default();
                let last4: Option<String> = k.try_get("secret_last4").unwrap_or_default();
                let kid = escape_html(&kid);
                let klabel = escape_html(&klabel);
                let mask = last4.map_or_else(|| "sealed".to_string(), |l| format!("••••{l}"));
                let mask = escape_html(&mask);
                let prio: i64 = k.try_get("priority").unwrap_or(100);
                let tier: i64 = k.try_get("strict_tier").unwrap_or(1);
                let enabled: bool = k.try_get::<i64, _>("enabled").unwrap_or(1) == 1;

                keys_html.push_str(&format!(
                    "<tr>\
                        <td><strong>{klabel}</strong> <span class=\"text-muted\">({mask})</span></td>\
                        <td>{prio}</td>\
                        <td>Tier {tier}</td>\
                        <td><span class=\"badge badge-{}\">{}</span></td>\
                        <td>\
                            <button class=\"btn btn-xs btn-ghost\" data-action=\"test-key\" data-key-id=\"{kid}\">Test</button>\
                            <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/upstream-keys/{kid}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Remove this upstream key?\">Remove</button>\
                        </td>\
                    </tr>",
                    if enabled { "success" } else { "danger" },
                    if enabled { "Active" } else { "Disabled" }
                ));
            }
            keys_html.push_str("</tbody></table>");
        }

        html.push_str(&format!(
            "<div class=\"provider-box\">\
                <div class=\"provider-header\">\
                    <div>\
                        <h4 style=\"font-size: 1rem; margin-bottom: 0.25rem;\">{name} <span class=\"badge badge-info\">{kind}</span></h4>\
                        <div style=\"font-size: 0.8125rem; font-family: monospace; color: var(--muted);\">{base_url}</div>\
                    </div>\
                    <div>\
                        <button class=\"btn btn-xs btn-primary\" data-modal-open=\"modal-new-upstream-key\" data-provider-id=\"{pid}\" data-provider-name=\"{name}\">+ Add Key</button>\
                        <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/providers/{pid}\" hx-target=\"closest .provider-box\" hx-swap=\"outerHTML\" hx-confirm=\"Delete this provider and all attached keys?\">Delete</button>\
                    </div>\
                </div>\
                <div class=\"keys-sublist\">{keys_html}</div>\
            </div>"
        ));
    }

    Ok(Html(html))
}

async fn get_partial_shares_outgoing_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT id, grantee_user_id, allowed_models, max_monthly_micro_usd, enabled
         FROM provider_shares WHERE owner_user_id = ? ORDER BY created_at DESC",
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"5\" class=\"text-center text-muted\">No outgoing shares granted yet. Click \"+ Grant Share\" above.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let grantee: String = r.try_get("grantee_user_id").unwrap_or_default();
        let short_grantee = if grantee.len() > 10 {
            &grantee[..10]
        } else {
            &grantee
        };
        let short_grantee = escape_html(short_grantee);
        let id = escape_html(&id);
        let models: Option<String> = r.try_get("allowed_models").unwrap_or_default();
        let models_str = models.unwrap_or_else(|| "All".to_string());
        let models_str = escape_html(&models_str);
        let cap: Option<i64> = r.try_get("max_monthly_micro_usd").unwrap_or_default();
        let cap_str = cap.map_or_else(
            || "No Cap".to_string(),
            |c| format!("${:.2}", c as f64 / 1_000_000.0),
        );
        let enabled: bool = r.try_get::<i64, _>("enabled").unwrap_or(1) == 1;

        html.push_str(&format!(
            "<tr>\
                <td><code>{short_grantee}...</code></td>\
                <td>{models_str}</td>\
                <td>{cap_str}</td>\
                <td><span class=\"badge badge-{}\">{}</span></td>\
                <td>\
                    <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/shares/{id}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Revoke this share?\">Revoke</button>\
                </td>\
            </tr>",
            if enabled { "success" } else { "danger" },
            if enabled { "Active" } else { "Revoked" }
        ));
    }

    Ok(Html(html))
}

async fn get_partial_shares_incoming_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT s.id, s.owner_user_id, s.allowed_models, s.accepted_config_fingerprint, p.name as provider_name, p.kind as provider_kind, p.share_config_fingerprint as current_fingerprint
         FROM provider_shares s
         JOIN providers p ON s.provider_id = p.id
         WHERE s.grantee_user_id = ? ORDER BY s.created_at DESC",
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"5\" class=\"text-center text-muted\">No shares shared with you.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let id = escape_html(&id);
        let pname: String = r.try_get("provider_name").unwrap_or_default();
        let pname = escape_html(&pname);
        let pkind: String = r.try_get("provider_kind").unwrap_or_default();
        let pkind = escape_html(&pkind);
        let owner: String = r.try_get("owner_user_id").unwrap_or_default();
        let short_owner = if owner.len() > 10 {
            &owner[..10]
        } else {
            &owner
        };
        let short_owner = escape_html(short_owner);
        let models: Option<String> = r.try_get("allowed_models").unwrap_or_default();
        let models_str = models.unwrap_or_else(|| "All".to_string());
        let models_str = escape_html(&models_str);
        let accepted_fp: Option<String> =
            r.try_get("accepted_config_fingerprint").unwrap_or_default();
        let current_fp: String = r.try_get("current_fingerprint").unwrap_or_default();
        let is_matching = accepted_fp.as_deref() == Some(&current_fp);
        let current_fp = escape_html(&current_fp);

        let action_btn = if is_matching {
            "<span class=\"text-muted\">Active</span>".to_string()
        } else {
            format!(
                "<button class=\"btn btn-xs btn-primary\" hx-post=\"/api/user/shares/{id}/accept\" hx-vals='{{\"fingerprint\": \"{current_fp}\"}}' hx-target=\"closest tr\" hx-swap=\"outerHTML\">Accept Share</button>"
            )
        };

        html.push_str(&format!(
            "<tr>\
                <td><strong>{pname}</strong> <span class=\"badge badge-info\">{pkind}</span></td>\
                <td><code>{short_owner}...</code></td>\
                <td>{models_str}</td>\
                <td><span class=\"badge badge-{}\">{}</span></td>\
                <td>{action_btn}</td>\
            </tr>",
            if is_matching { "success" } else { "warning" },
            if is_matching {
                "Accepted"
            } else {
                "Pending Consent"
            }
        ));
    }

    Ok(Html(html))
}

async fn get_partial_matrix_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let res = get_user_routes_handler(State(state), headers).await?;
    let json = res.0;
    let models = json.get("models").and_then(|v| v.as_array());

    let Some(arr) = models else {
        return Ok(Html(
            "<tr><td colspan=\"5\" class=\"text-center text-muted\">No models found.</td></tr>"
                .to_string(),
        ));
    };

    if arr.is_empty() {
        return Ok(Html("<tr><td colspan=\"5\" class=\"text-center text-muted\">No models discovered yet. Add an upstream key to discover routes.</td></tr>".to_string()));
    }

    let mut html = String::new();
    for m in arr {
        let cid = m["canonical_id"].as_str().unwrap_or("unknown");
        let cid = escape_html(cid);
        let strategy = m["routing_strategy"]
            .as_str()
            .unwrap_or("smooth_weighted_rr");
        let strategy = escape_html(strategy);
        let own_keys = m["own_keys"].as_array().map_or("—".to_string(), |keys| {
            if keys.is_empty() {
                "—".to_string()
            } else {
                keys.iter()
                    .map(|k| {
                        format!(
                            "{} (Prio: {})",
                            k["label"].as_str().unwrap_or("key"),
                            k["priority_override"]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        });
        let shared_keys = m["shared_keys"].as_array().map_or("—".to_string(), |keys| {
            if keys.is_empty() {
                "—".to_string()
            } else {
                keys.iter()
                    .map(|k| {
                        format!(
                            "{} (Prio: {})",
                            k["label"].as_str().unwrap_or("key"),
                            k["priority_override"]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        });
        let own_keys = escape_html(&own_keys);
        let shared_keys = escape_html(&shared_keys);
        let global_avail = m["global_pool"]["available"].as_bool().unwrap_or(false);
        let global_html = if global_avail {
            "<span class=\"badge badge-success\">Global pool</span>"
        } else {
            "—"
        };

        html.push_str(&format!(
            "<tr>\
                <td><strong>{cid}</strong></td>\
                <td><span class=\"badge badge-info\">{strategy}</span></td>\
                <td>{own_keys}</td>\
                <td>{shared_keys}</td>\
                <td>{global_html}</td>\
            </tr>"
        ));
    }

    Ok(Html(html))
}

async fn get_partial_admin_overview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let res = get_admin_overview_telemetry_handler(State(state.clone()), headers).await?;
    let data = res.0;

    let proj_disk = data["projected_disk_mb"].as_f64().unwrap_or(0.0);
    let unpriced = data["unpriced_models_count"].as_i64().unwrap_or(0);
    let events_cnt = data["usage_events_count"].as_i64().unwrap_or(0);
    let ret_days = data["retention_days"].as_i64().unwrap_or(7);

    let users_count: i64 = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
        .fetch_one(&state.auth.pool)
        .await
        .unwrap_or(0);

    let prov_count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM providers WHERE ownership_scope = 'global'",
    )
    .fetch_one(&state.auth.pool)
    .await
    .unwrap_or(0);

    let unpriced_badge = if unpriced > 0 {
        format!(
            "<span class=\"badge badge-warning\" style=\"margin-top: 0.25rem;\">{unpriced} unpriced models</span>"
        )
    } else {
        String::new()
    };

    let html = format!(
        "<div class=\"stats-grid\">\
            <div class=\"card stat-card\">\
                <div class=\"stat-label\">Projected Disk Usage</div>\
                <div id=\"disk-meter\" class=\"disk-meter\">\
                    <p class=\"stat-val\" style=\"font-size: 1.5rem;\">Projected Disk Usage: {proj_disk} MB</p>\
                    {unpriced_badge}\
                    <div class=\"stat-desc\" style=\"font-size: 0.75rem; color: #94a3b8; margin-top: 0.25rem;\">\
                        {events_cnt} events ({ret_days}d retention)\
                    </div>\
                </div>\
                <div class=\"stat-desc\">A6 disk telemetry meter</div>\
            </div>\
            <div class=\"card stat-card\">\
                <div class=\"stat-label\">Total Users</div>\
                <div class=\"stat-val\">{users_count}</div>\
                <div class=\"stat-desc\">Standard & admin accounts</div>\
            </div>\
            <div class=\"card stat-card\">\
                <div class=\"stat-label\">Global Providers</div>\
                <div class=\"stat-val\">{prov_count}</div>\
                <div class=\"stat-desc\">Shared upstream routes</div>\
            </div>\
            <div class=\"card stat-card\">\
                <div class=\"stat-label\">System Health</div>\
                <div class=\"stat-val text-success\">Healthy</div>\
                <div class=\"stat-desc\">WAL Checkpoint Normal</div>\
            </div>\
        </div>"
    );

    Ok(Html(html))
}

async fn get_partial_admin_users_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT id, username, role, can_use_global_providers, global_budget_micro_usd_month, disabled
         FROM users ORDER BY created_at ASC",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut html = String::new();
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let id = escape_html(&id);
        let username: String = r.try_get("username").unwrap_or_default();
        let username = escape_html(&username);
        let role: String = r.try_get("role").unwrap_or_default();
        let role = escape_html(&role);
        let can_use: bool = r.try_get::<i64, _>("can_use_global_providers").unwrap_or(1) == 1;
        let budget: Option<i64> = r
            .try_get("global_budget_micro_usd_month")
            .unwrap_or_default();
        let budget_str = budget.map_or_else(
            || "No Cap".to_string(),
            |b| format!("${:.2}", b as f64 / 1_000_000.0),
        );
        let disabled: bool = r.try_get::<i64, _>("disabled").unwrap_or(0) == 1;

        let delete_btn = if role != "admin" {
            format!(
                "<button class=\"btn btn-xs btn-danger\" hx-delete=\"/admin/api/users/{id}?confirm={username}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Delete user {username}?\">Delete</button>"
            )
        } else {
            String::new()
        };

        html.push_str(&format!(
            "<tr>\
                <td><strong>{username}</strong></td>\
                <td><span class=\"badge badge-{}\">{role}</span></td>\
                <td>{}</td>\
                <td>{budget_str}</td>\
                <td><span class=\"badge badge-{}\">{}</span></td>\
                <td>Active</td>\
                <td>\
                    <button class=\"btn btn-xs btn-secondary\" data-action=\"toggle-user\" data-id=\"{id}\" data-disabled=\"{}\">{}</button>\
                    {delete_btn}\
                </td>\
            </tr>",
            if role == "admin" { "primary" } else { "info" },
            if can_use { "Allowed" } else { "Restricted" },
            if disabled { "danger" } else { "success" },
            if disabled { "Disabled" } else { "Active" },
            !disabled,
            if disabled { "Enable" } else { "Disable" }
        ));
    }

    Ok(Html(html))
}

async fn get_partial_admin_providers_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    use sqlx::Row as _;
    let prov_rows = sqlx::query(
        "SELECT id, name, kind, base_url, enabled FROM providers WHERE ownership_scope = 'global' ORDER BY created_at ASC",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if prov_rows.is_empty() {
        return Ok(Html(
            "<p class=\"text-muted\">No global providers configured yet. Click \"+ Add Global Provider\" above.</p>".to_string(),
        ));
    }

    let mut html = String::new();
    for p in prov_rows {
        let pid: String = p.try_get("id").unwrap_or_default();
        let name: String = p.try_get("name").unwrap_or_default();
        let kind: String = p.try_get("kind").unwrap_or_default();
        let base_url: String = p.try_get("base_url").unwrap_or_default();
        let pid = escape_html(&pid);
        let name = escape_html(&name);
        let kind = escape_html(&kind);
        let base_url = escape_html(&base_url);

        let key_rows = sqlx::query(
            "SELECT id, label, secret_last4, priority, strict_tier, enabled FROM upstream_keys WHERE provider_id = ?",
        )
        .bind(&pid)
        .fetch_all(&state.auth.pool)
        .await
        .unwrap_or_default();

        let mut keys_html = String::new();
        if key_rows.is_empty() {
            keys_html = "<p class=\"text-muted\" style=\"font-size: 0.8125rem; padding: 0.5rem 0;\">No upstream keys under this global provider.</p>".to_string();
        } else {
            keys_html.push_str("<table class=\"data-table\" style=\"margin-top: 0.5rem; background: #080c18;\"><thead><tr><th>Label</th><th>Priority</th><th>Strict Tier</th><th>Health</th><th>Action</th></tr></thead><tbody>");
            for k in key_rows {
                let kid: String = k.try_get("id").unwrap_or_default();
                let klabel: String = k.try_get("label").unwrap_or_default();
                let last4: Option<String> = k.try_get("secret_last4").unwrap_or_default();
                let kid = escape_html(&kid);
                let klabel = escape_html(&klabel);
                let mask = last4.map_or_else(|| "sealed".to_string(), |l| format!("••••{l}"));
                let mask = escape_html(&mask);
                let prio: i64 = k.try_get("priority").unwrap_or(100);
                let tier: i64 = k.try_get("strict_tier").unwrap_or(1);
                let enabled: bool = k.try_get::<i64, _>("enabled").unwrap_or(1) == 1;

                keys_html.push_str(&format!(
                    "<tr>\
                        <td><strong>{klabel}</strong> <span class=\"text-muted\">({mask})</span></td>\
                        <td>{prio}</td>\
                        <td>Tier {tier}</td>\
                        <td><span class=\"badge badge-{}\">{}</span></td>\
                        <td>\
                            <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/upstream-keys/{kid}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Remove this upstream key?\">Remove</button>\
                        </td>\
                    </tr>",
                    if enabled { "success" } else { "danger" },
                    if enabled { "Active" } else { "Disabled" }
                ));
            }
            keys_html.push_str("</tbody></table>");
        }

        html.push_str(&format!(
            "<div class=\"provider-box\">\
                <div class=\"provider-header\">\
                    <div>\
                        <h4 style=\"font-size: 1rem; margin-bottom: 0.25rem;\">{name} <span class=\"badge badge-info\">{kind}</span></h4>\
                        <div style=\"font-size: 0.8125rem; font-family: monospace; color: var(--muted);\">{base_url}</div>\
                    </div>\
                    <div>\
                        <button class=\"btn btn-xs btn-primary\" data-modal-open=\"modal-new-upstream-key\" data-provider-id=\"{pid}\" data-provider-name=\"{name}\">+ Add Key</button>\
                        <button class=\"btn btn-xs btn-danger\" hx-delete=\"/admin/api/providers/{pid}\" hx-target=\"closest .provider-box\" hx-swap=\"outerHTML\" hx-confirm=\"Delete this global provider and all attached keys?\">Delete</button>\
                    </div>\
                </div>\
                <div class=\"keys-sublist\">{keys_html}</div>\
            </div>"
        ));
    }

    Ok(Html(html))
}

async fn get_partial_admin_catalog_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    use sqlx::Row as _;
    let model_rows = sqlx::query(
        "SELECT m.model_pk, m.canonical_id, m.origin, m.exposed, m.merge_state,
                (SELECT COUNT(*) FROM model_routes r WHERE r.model_pk = m.model_pk) as route_count
         FROM models m ORDER BY m.canonical_id ASC",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if model_rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"6\" class=\"text-center text-muted\">No models in catalog.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in model_rows {
        let pk: String = r.try_get("model_pk").unwrap_or_default();
        let pk = escape_html(&pk);
        let cid: String = r.try_get("canonical_id").unwrap_or_default();
        let cid = escape_html(&cid);
        let origin: String = r.try_get("origin").unwrap_or_default();
        let origin = escape_html(&origin);
        let exposed: Option<i64> = r.try_get("exposed").unwrap_or_default();
        let is_exposed = exposed != Some(0);
        let merge_state: String = r.try_get("merge_state").unwrap_or_default();
        let merge_state = escape_html(&merge_state);
        let count: i64 = r.try_get("route_count").unwrap_or(0);

        let confirm_btn = if merge_state == "pending_review" {
            format!(
                "<button class=\"btn btn-xs btn-primary\" hx-post=\"/admin/api/models/{pk}/confirm-merge\" hx-target=\"closest tr\" hx-swap=\"outerHTML\">Confirm Merge</button> "
            )
        } else {
            String::new()
        };

        html.push_str(&format!(
            "<tr>\
                <td><strong>{cid}</strong></td>\
                <td><span class=\"badge badge-info\">{origin}</span></td>\
                <td><span class=\"badge badge-{}\">{merge_state}</span></td>\
                <td><span class=\"badge badge-{}\">{}</span></td>\
                <td>{count} routes</td>\
                <td>\
                    {confirm_btn}\
                    <button class=\"btn btn-xs btn-{}\" data-action=\"toggle-model\" data-id=\"{pk}\" data-exposed=\"{}\">{}</button>\
                </td>\
            </tr>",
            if merge_state == "confirmed" { "success" } else { "warning" },
            if is_exposed { "success" } else { "danger" },
            if is_exposed { "Exposed" } else { "Disabled" },
            if is_exposed { "danger" } else { "success" },
            if is_exposed { 0 } else { 1 },
            if is_exposed { "Kill-Switch" } else { "Expose" }
        ));
    }

    Ok(Html(html))
}

async fn get_partial_admin_audit_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT seq, ts, actor_user_id, action, target_type, target_id, detail_json
         FROM audit_log ORDER BY seq DESC LIMIT 100",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"6\" class=\"text-center text-muted\">No audit log entries recorded yet.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in rows {
        let seq: i64 = r.try_get("seq").unwrap_or(0);
        let ts: i64 = r.try_get("ts").unwrap_or(0);
        let actor: Option<String> = r.try_get("actor_user_id").unwrap_or_default();
        let actor_str = actor.as_deref().map_or("System".to_string(), |a| {
            if a.len() > 10 {
                format!("{}...", &a[..10])
            } else {
                a.to_string()
            }
        });
        let actor_str = escape_html(&actor_str);
        let action: String = r.try_get("action").unwrap_or_default();
        let action = escape_html(&action);
        let target_type: String = r
            .try_get::<Option<String>, _>("target_type")
            .unwrap_or_default()
            .unwrap_or_else(|| "—".to_string());
        let target_id: Option<String> = r.try_get("target_id").unwrap_or_default();
        let target_str = target_id.map_or(target_type.clone(), |tid| {
            let short = if tid.len() > 8 { &tid[..8] } else { &tid };
            format!("{target_type} ({short})")
        });
        let target_str = escape_html(&target_str);
        let payload: String = r.try_get("detail_json").unwrap_or_default();
        let payload = escape_html(&payload);

        html.push_str(&format!(
            "<tr>\
                <td><code>#{seq}</code></td>\
                <td>{ts}</td>\
                <td><code>{actor_str}</code></td>\
                <td><span class=\"badge badge-info\">{action}</span></td>\
                <td>{target_str}</td>\
                <td style=\"font-family: monospace; font-size: 0.75rem; max-width: 200px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;\">{payload}</td>\
            </tr>"
        ));
    }

    Ok(Html(html))
}

pub fn router(state: ApiAppState) -> Router {
    Router::new()
        // Web UI screens & HTMX Partials
        .route("/app", get(crate::ui::get_app_ui))
        .route("/app/partials/pins", get(get_partial_pins_handler))
        .route(
            "/app/partials/client-keys",
            get(get_partial_client_keys_handler),
        )
        .route(
            "/app/partials/providers",
            get(get_partial_providers_handler),
        )
        .route(
            "/app/partials/shares/outgoing",
            get(get_partial_shares_outgoing_handler),
        )
        .route(
            "/app/partials/shares/incoming",
            get(get_partial_shares_incoming_handler),
        )
        .route("/app/partials/matrix", get(get_partial_matrix_handler))
        .route("/admin", get(crate::ui::get_admin_ui))
        .route(
            "/admin/partials/overview",
            get(get_partial_admin_overview_handler),
        )
        .route(
            "/admin/partials/users",
            get(get_partial_admin_users_handler),
        )
        .route(
            "/admin/partials/providers",
            get(get_partial_admin_providers_handler),
        )
        .route(
            "/admin/partials/catalog",
            get(get_partial_admin_catalog_handler),
        )
        .route(
            "/admin/partials/audit",
            get(get_partial_admin_audit_handler),
        )
        // User endpoints
        .route(
            "/api/user/providers",
            get(get_user_providers).post(post_user_provider),
        )
        .route(
            "/api/user/providers/{id}",
            get(get_user_provider)
                .patch(patch_user_provider_handler)
                .delete(delete_user_provider_handler),
        )
        .route(
            "/api/user/providers/{id}/keys",
            get(get_user_provider_keys).post(post_user_provider_key),
        )
        .route(
            "/api/user/upstream-keys/{id}",
            get(get_user_upstream_key_handler)
                .patch(patch_user_upstream_key_handler)
                .delete(delete_user_upstream_key_handler),
        )
        .route(
            "/api/user/upstream-keys/{id}/test",
            post(post_test_upstream_key_handler),
        )
        .route("/api/user/routes", get(get_user_routes_handler))
        .route(
            "/api/user/routes/{key_id}/{canonical_id}",
            put(put_user_route_preference_handler),
        )
        .route("/api/user/password", post(post_user_password_handler))
        .route("/api/user/totp/enroll", post(post_user_totp_enroll_handler))
        .route("/api/user/totp/verify", post(post_user_totp_verify_handler))
        .route(
            "/api/user/totp/disable",
            post(post_user_totp_disable_handler),
        )
        .route(
            "/api/user/web-sessions/{id_hash}",
            delete(delete_user_session_handler),
        )
        .route(
            "/api/user/client-keys",
            get(get_user_client_keys_handler).post(post_user_client_key_handler),
        )
        .route(
            "/api/user/client-keys/{id}",
            patch(patch_user_client_key_handler).delete(delete_user_client_key_handler),
        )
        .route(
            "/api/user/client-keys/{id}/rotate",
            post(post_rotate_client_key_handler),
        )
        .route(
            "/api/user/client-keys/{id}/pause",
            post(post_pause_client_key_handler),
        )
        .route(
            "/api/user/client-keys/{id}/resume",
            post(post_resume_client_key_handler),
        )
        .route(
            "/api/user/service-accounts",
            get(get_user_service_accounts_handler).post(post_user_service_account_handler),
        )
        .route(
            "/api/user/service-accounts/{id}",
            get(get_user_service_account_handler)
                .patch(patch_user_service_account_handler)
                .delete(delete_user_service_account_handler),
        )
        .route(
            "/api/user/service-accounts/{id}/ip-restrictions",
            put(put_user_service_account_ip_restrictions_handler),
        )
        .route(
            "/api/user/preferences",
            patch(patch_user_preferences_handler),
        )
        .route("/api/user/web-sessions", get(get_user_web_sessions_handler))
        .route("/api/user/audit-log", get(get_user_audit_log_handler))
        .route(
            "/api/user/shares",
            get(get_user_shares_handler).post(post_user_share_handler),
        )
        .route(
            "/api/user/shares/{id}/accept",
            post(post_accept_share_handler),
        )
        .route("/api/user/shares/{id}", delete(delete_user_share_handler))
        .route("/api/user/pins", get(get_active_pins_handler))
        .route(
            "/api/user/pins/{client_key_id}/{session_hash}/{canonical_id}",
            delete(delete_user_pin_handler),
        )
        .route(
            "/api/user/client-keys/{id}/sessions",
            get(get_user_client_key_sessions_handler),
        )
        .route(
            "/api/user/sessions/{session_hash}/chat",
            get(get_user_session_preview_handler),
        )
        .route(
            "/api/user/sessions/{session_hash}/preview",
            get(get_user_session_preview_handler),
        )
        .route(
            "/api/user/traces/{request_id}",
            get(get_user_trace_record_handler),
        )
        .route(
            "/api/user/providers/{id}/prices",
            put(put_user_provider_price_handler),
        )
        .route(
            "/api/user/usage/summary",
            get(get_user_usage_summary_handler),
        )
        .route("/api/user/usage/events", get(get_user_usage_events_handler))
        .route(
            "/api/user/usage/export.csv",
            get(get_user_usage_export_csv_handler),
        )
        // Admin endpoints
        .route(
            "/admin/api/providers",
            get(get_admin_providers).post(post_admin_provider),
        )
        .route(
            "/admin/api/providers/{id}",
            patch(patch_user_provider_handler).delete(delete_user_provider_handler),
        )
        .route(
            "/admin/api/providers/{id}/keys",
            post(post_admin_provider_key),
        )
        .route(
            "/admin/api/upstream-keys/{id}",
            patch(patch_user_upstream_key_handler).delete(delete_user_upstream_key_handler),
        )
        .route(
            "/admin/api/client-keys/{id}",
            delete(delete_user_client_key_handler),
        )
        .route(
            "/admin/api/client-keys/{id}/sessions",
            get(get_admin_client_key_sessions_handler),
        )
        .route(
            "/admin/api/sessions/{session_hash}/chat",
            get(get_admin_session_preview_handler),
        )
        .route(
            "/admin/api/sessions/{session_hash}/preview",
            get(get_admin_session_preview_handler),
        )
        .route(
            "/admin/api/traces/{request_id}",
            get(get_admin_trace_record_handler),
        )
        .route(
            "/admin/api/sessions/{session_hash}/trace",
            delete(delete_admin_session_trace_handler),
        )
        .route("/admin/api/traces", delete(delete_admin_traces_handler))
        .route(
            "/admin/api/settings",
            get(get_admin_settings_handler).patch(patch_admin_settings_handler),
        )
        .route(
            "/admin/api/settings/traceability",
            put(put_admin_traceability_handler),
        )
        .route(
            "/admin/api/users",
            get(get_admin_users_handler).post(post_admin_user_handler),
        )
        .route(
            "/admin/api/users/{id}",
            patch(patch_admin_user_handler).delete(delete_admin_user_handler),
        )
        .route("/admin/api/models", get(get_admin_models_handler))
        .route(
            "/admin/api/models/refresh",
            post(post_admin_models_refresh_handler),
        )
        .route("/admin/api/models/{id}", patch(patch_admin_model_handler))
        .route(
            "/admin/api/models/{id}/aliases",
            post(post_admin_model_alias_handler),
        )
        .route(
            "/admin/api/models/{id}/confirm-merge",
            post(post_admin_model_confirm_merge_handler),
        )
        .route(
            "/admin/api/model-aliases",
            delete(delete_admin_model_alias_handler),
        )
        .route(
            "/admin/api/shares/{id}/approve",
            post(post_admin_share_approve_handler),
        )
        .route(
            "/admin/api/overview",
            get(get_admin_overview_telemetry_handler),
        )
        .route(
            "/admin/api/usage/summary",
            get(get_admin_usage_summary_handler),
        )
        .route(
            "/admin/api/usage/events",
            get(get_admin_usage_events_handler),
        )
        .route(
            "/admin/api/usage/export.csv",
            get(get_admin_usage_export_csv_handler),
        )
        .route("/admin/api/audit-log", get(get_admin_audit_log_handler))
        .route("/admin/api/pins", get(get_active_pins_handler))
        .route("/admin/api/sessions/{hash}/pin", delete(delete_pin_handler))
        .route("/admin/api/prices", put(put_admin_price_handler))
        .route(
            "/admin/api/prices/sync",
            post(post_admin_prices_sync_handler),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            control_mutation_barrier,
        ))
        .with_state(state)
}

#[cfg(test)]
mod catalog_sync_tests {
    use super::parse_catalog_prices;

    /// `price_sync_catalog_url` catalogues publish context-length tiers and aliases;
    /// the sync handler must import both instead of reporting a hardcoded zero (F012).
    #[test]
    fn parses_context_tiers_and_aliases_from_the_catalogue() {
        let catalog = serde_json::json!({
            "openrouter": {
                "models": {
                    "vendor/model-a": {
                        "cost": {
                            "input": 3.0,
                            "output": 15.0,
                            "cache_read": 0.3,
                            "cache_write": 3.75,
                            "input_over_200k": 6.0,
                            "output_over_200k": 22.5
                        },
                        "alias": "model-a",
                        "aliases": ["model-a-latest", "model-a"]
                    }
                }
            }
        });
        let (prices, aliases) = parse_catalog_prices(&catalog);
        let price = prices.get("vendor/model-a").expect("price template");
        assert_eq!(price.rates.input_per_mtok_micro_usd, 3_000_000);
        assert_eq!(price.rates.output_per_mtok_micro_usd, 15_000_000);
        assert_eq!(price.tiers.len(), 1);
        let tier = &price.tiers[0];
        assert_eq!(tier.min_total_input_tokens, 200_000);
        assert_eq!(tier.input_per_mtok_micro_usd, 6_000_000);
        assert_eq!(tier.output_per_mtok_micro_usd, 22_500_000);
        assert_eq!(
            aliases.get("vendor/model-a").cloned().unwrap_or_default(),
            vec!["model-a".to_string(), "model-a-latest".to_string(), "model-a".to_string()]
        );
    }

    /// Models without a `cost` block are not priced, and the catalogue shape is
    /// tolerated defensively (no `models` key, non-object roots).
    #[test]
    fn ignores_entries_without_cost_and_tolerates_unknown_shapes() {
        let catalog = serde_json::json!({
            "provider": { "models": { "unpriced": { "alias": "u" } } },
            "broken": 7
        });
        let (prices, aliases) = parse_catalog_prices(&catalog);
        assert!(prices.is_empty());
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases["unpriced"], vec!["u".to_string()]);
    }
}
