use super::*;

#[derive(Debug, Clone)]
pub(crate) struct MutationAuditContext {
    pub(crate) ts: i64,
    pub(crate) actor_user_id: Option<String>,
    pub(crate) ip_addr: Option<String>,
    pub(crate) action: String,
    pub(crate) target_type: String,
    pub(crate) target_id: Option<String>,
    pub(crate) detail_json: String,
}

/// Run one control-plane mutation, its `audit_log` row, and the
/// `config_version` bump in a single `BEGIN IMMEDIATE` transaction on the
/// writer actor (F09, AUD-1, REL-4).
///
/// Entity SQL is deliberately supplied as a transaction closure so network
/// operations and validation can stay outside the write lock.
pub(crate) async fn execute_atomic_mutation<T, F>(
    state: &ApiAppState,
    domains: Vec<ConfigDomain>,
    run: F,
) -> Result<T, MutationResponseError>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&'a mut Transaction<'_, Sqlite>) -> MutationFuture<'a, T> + Send + 'static,
{
    execute_atomic_mutation_with_invalidations(state, domains, Vec::new(), run).await
}

pub(crate) async fn execute_atomic_mutation_with_invalidations<T, F>(
    state: &ApiAppState,
    domains: Vec<ConfigDomain>,
    invalidations: Vec<llmroller_core::snapshot::CacheInvalidationEvent>,
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
            .execute_control(domains, invalidations, AdminControlOp { run: Some(op) })
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
pub(crate) async fn record_control_mutation(
    state: &ApiAppState,
    domains: Vec<ConfigDomain>,
) -> Result<(), MutationResponseError> {
    execute_atomic_mutation(state, domains, |_tx| Box::pin(async { Ok(()) })).await
}

pub(crate) fn writer_busy_error() -> MutationResponseError {
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

pub(crate) struct AdminControlOp {
    pub(crate) run: Option<MutationRunner>,
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

pub(crate) async fn scan_mutation_audit(
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

pub(crate) async fn await_published_version(state: &ApiAppState, version: i64) {
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
        self.publish_control_change_with_invalidations(domains, Vec::new())
            .await;
    }

    pub async fn publish_control_change_with_invalidations(
        &self,
        domains: Vec<ConfigDomain>,
        invalidations: Vec<llmroller_core::snapshot::CacheInvalidationEvent>,
    ) {
        let Some(writer) = &self.writer else {
            return;
        };
        let result = writer
            .execute_control(domains, invalidations, VersionBumpOp)
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
pub(crate) struct VersionBumpOp;

impl llmroller_store::writer::ControlOp for VersionBumpOp {
    fn call<'a>(
        self: Box<Self>,
        _tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), llmroller_store::writer::WriterError>> + Send + 'a>>
    {
        Box::pin(async { Ok(()) })
    }
}

pub(crate) fn domains_for_path(path: &str) -> Vec<ConfigDomain> {
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
    } else if path.contains("/windows")
        || path.contains("/budget-windows")
        || path.contains("/budgets")
    {
        vec![ConfigDomain::BudgetWindows]
    } else if path.contains("/service-accounts") {
        vec![ConfigDomain::ServiceAccounts, ConfigDomain::ClientKeys]
    } else if path.contains("/traces") || path.contains("/sessions") {
        vec![ConfigDomain::Traceability]
    } else {
        // Unknown mutation domain: force a full snapshot rebuild to stay correct.
        Vec::new()
    }
}

pub(crate) fn invalidations_for_path(
    path: &str,
) -> Vec<llmroller_core::snapshot::CacheInvalidationEvent> {
    use llmroller_core::snapshot::CacheInvalidationEvent;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    for (idx, seg) in segments.iter().enumerate() {
        let next = segments.get(idx + 1).copied();
        match (*seg, next) {
            ("providers", Some(id)) => {
                if let Ok(pid) = ProviderId::parse(id) {
                    out.push(CacheInvalidationEvent::ProviderChanged(pid));
                }
            }
            ("upstream-keys" | "keys", Some(id)) => {
                if let Ok(kid) = UpstreamKeyId::parse(id) {
                    out.push(CacheInvalidationEvent::UpstreamKeyChanged(kid));
                }
            }
            ("client-keys", Some(id)) => {
                if let Ok(ckid) = ClientKeyId::parse(id) {
                    out.push(CacheInvalidationEvent::ClientKeyChanged(ckid));
                }
            }
            ("budget-windows", Some(id)) => {
                out.push(CacheInvalidationEvent::BudgetWindowChanged(id.to_string()));
            }
            ("users", Some(id)) => {
                if let Ok(uid) = UserId::parse(id) {
                    out.push(CacheInvalidationEvent::UserChanged(uid));
                }
            }
            ("settings", _) => {
                out.push(CacheInvalidationEvent::SettingsChanged);
            }
            _ => {}
        }
    }
    out
}

pub(crate) struct MutationAudit {
    pub(crate) action: String,
    pub(crate) target_type: &'static str,
    pub(crate) target_id: Option<String>,
}

pub(crate) fn mutation_resource(segment: &str) -> Option<&'static str> {
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

pub(crate) fn method_verb(method: &Method) -> &'static str {
    match *method {
        Method::POST => "created",
        Method::PUT => "replaced",
        Method::PATCH => "updated",
        Method::DELETE => "deleted",
        _ => "mutated",
    }
}

pub(crate) fn mutation_target_id(
    segments: &[&str],
    resource_index: usize,
    resource: &str,
) -> Option<String> {
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

pub(crate) fn classify_mutation(method: &Method, path: &str) -> MutationAudit {
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
pub(crate) async fn control_mutation_barrier(
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
        let invalidations = invalidations_for_path(&path);
        state
            .publish_control_change_with_invalidations(domains, invalidations)
            .await;
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
pub(crate) async fn authenticate_actor_internal(
    state: &ApiAppState,
    headers: &HeaderMap,
    is_mutation: bool,
    allow_must_change_password: bool,
) -> Result<ActorScope, (StatusCode, Json<serde_json::Value>)> {
    let session = authenticate_session(&state.auth, headers)
        .await
        .map_err(map_auth_error)?;

    if !allow_must_change_password && session.user.must_change_password {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": {
                    "code": "must_change_password",
                    "message": "password change required before accessing other resources"
                }
            })),
        ));
    }

    if is_mutation {
        crate::auth::validate_session_csrf(&session, headers)
            .await
            .map_err(map_auth_error)?;
    }

    Ok(ActorScope::user(session.user.id, session.user.role))
}

pub(crate) async fn authenticate_actor(
    state: &ApiAppState,
    headers: &HeaderMap,
    is_mutation: bool,
) -> Result<ActorScope, (StatusCode, Json<serde_json::Value>)> {
    authenticate_actor_internal(state, headers, is_mutation, false).await
}

pub(crate) async fn authenticate_admin(
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

pub(crate) fn map_provider_error(err: ProviderError) -> (StatusCode, Json<serde_json::Value>) {
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

pub(crate) fn map_client_key_error(err: ClientKeyError) -> (StatusCode, Json<serde_json::Value>) {
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

pub(crate) fn map_settings_error(err: SettingsError) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY),
        Json(serde_json::json!({ "error": { "message": err.to_string() } })),
    )
}

// ---------------------------------------------------------------------------
// Request/Response DTOs
// ---------------------------------------------------------------------------

/// Provider kind or preset slug submitted in `CreateProviderRequest.kind`.
///
/// Accepts the 5 core `ProviderKind` values or any of the 80 compiled-in preset slugs
/// (`F101`–`F180`), resolving a preset slug to `kind = ProviderKind::Custom` plus `preset_id`.
#[derive(Debug, Clone)]
pub(crate) struct ProviderKindOrPreset {
    pub kind: ProviderKind,
    pub preset_id: Option<String>,
}

impl<'de> Deserialize<'de> for ProviderKindOrPreset {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        if let Ok(kind) = raw.parse::<ProviderKind>() {
            return Ok(Self {
                kind,
                preset_id: None,
            });
        }
        if let Some(spec) = llmroller_proto::providers::lookup_provider_preset(&raw) {
            return Ok(Self {
                kind: ProviderKind::Custom,
                preset_id: Some(spec.preset_id.to_string()),
            });
        }
        Err(serde::de::Error::custom(format!(
            "invalid provider kind or preset {raw:?}"
        )))
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateProviderRequest {
    pub name: String,
    pub kind: ProviderKindOrPreset,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub preset_id: Option<String>,
    #[serde(default = "default_config_json")]
    pub config_json: String,
    #[serde(default)]
    pub usage_url_cross_origin_ack: bool,
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

pub(crate) fn default_config_json() -> String {
    "{}".to_owned()
}

/// Reject provider configuration documents that cannot be deserialized.
///
/// Snapshot loading stays fail-open for legacy or hand-edited rows, but the
/// control plane must not persist a document that would silently wipe the
/// provider's routing configuration (`PRV-3`, `API-2`).
pub(crate) fn validate_provider_config_json(
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

pub(crate) const fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(crate) struct PatchProviderRequest {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub config_json: Option<String>,
    pub usage_url_cross_origin_ack: Option<bool>,
    pub allow_private_network: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateKeyRequest {
    pub label: String,
    pub secret: String,
    pub priority: Option<i64>,
    pub strict_tier: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PatchKeyRequest {
    pub label: Option<String>,
    pub priority: Option<i64>,
    pub strict_tier: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateClientKeyRequest {
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
pub(crate) struct ClientKeyCreationResponse {
    pub client_key: ClientKey,
    pub token: String,
    pub raw_token: String,
}

// ---------------------------------------------------------------------------
// Personal Provider Endpoints (/api/user/providers)
// ---------------------------------------------------------------------------
