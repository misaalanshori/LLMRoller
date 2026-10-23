#![forbid(unsafe_code)]
//! Initial first-admin setup bootstrap and serialization.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use llmroller_core::ids::UserId;
use llmroller_core::rbac::Role;
use llmroller_core::seams::{Clock, EntropySource, OsEntropy, SystemClock};
use llmroller_store::users::{UserError, count_users, hash_password, validate_username};
use serde::{Deserialize, Serialize};
use sqlx::{Connection as _, SqlitePool};
use subtle::ConstantTimeEq;
use thiserror::Error;

/// Environment variable containing an explicit bootstrap setup token.
pub const SETUP_TOKEN_ENV: &str = "LLMROLLER_SETUP_TOKEN";

/// Environment variable controlling whether a setup token is required.
pub const SETUP_TOKEN_REQUIRED_ENV: &str = "LLMROLLER_SETUP_TOKEN_REQUIRED";

/// Default file name for the auto-generated setup token inside the data directory.
pub const SETUP_TOKEN_FILE_NAME: &str = "setup-token";

/// Setup subsystem errors.
#[derive(Debug, Error)]
pub enum SetupError {
    /// File I/O failure.
    #[error("cannot read or write setup token: {0}")]
    Io(#[from] std::io::Error),
    /// Database failure.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// User store failure.
    #[error(transparent)]
    User(#[from] UserError),
    /// Setup is no longer accessible because users already exist.
    #[error("instance already initialized")]
    AlreadyInitialized,
    /// Setup token was invalid or missing.
    #[error("invalid setup token")]
    InvalidToken,
    /// Setup race conflict.
    #[error("setup conflict")]
    Conflict,
    /// Invalid request payload.
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
    /// Template render failure.
    #[error("template error: {0}")]
    Template(String),
}

impl IntoResponse for SetupError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::AlreadyInitialized => (StatusCode::NOT_FOUND, "instance already initialized"),
            Self::InvalidToken => (StatusCode::UNAUTHORIZED, "invalid setup token"),
            Self::Conflict => (
                StatusCode::CONFLICT,
                "setup conflict; instance already initialized",
            ),
            Self::InvalidPayload(_) => (StatusCode::BAD_REQUEST, "invalid payload"),
            Self::User(UserError::WeakPassword) => (StatusCode::BAD_REQUEST, "password too weak"),
            Self::User(UserError::InvalidUsername(_)) => {
                (StatusCode::BAD_REQUEST, "invalid username")
            }
            Self::User(UserError::DuplicateUsername) => {
                (StatusCode::CONFLICT, "username already exists")
            }
            Self::Io(_) | Self::Sqlx(_) | Self::User(_) | Self::Template(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

/// Read or auto-generate the one-time setup token per `AUTH-2`.
///
/// If `LLMROLLER_SETUP_TOKEN_REQUIRED=false`, returns `Ok(None)`.
/// If `LLMROLLER_SETUP_TOKEN` is set, returns `Ok(Some(token))`.
/// Otherwise, reads or creates `${data_dir}/setup-token` with mode `0600` (`O_EXCL`),
/// printing only the file path to `stderr`.
pub fn resolve_or_create_setup_token(
    data_dir: &Path,
    env_token: Option<&str>,
    token_required: bool,
    entropy: &dyn EntropySource,
) -> Result<Option<String>, SetupError> {
    if !token_required {
        return Ok(None);
    }
    if let Some(token) = env_token
        && !token.trim().is_empty()
    {
        return Ok(Some(token.trim().to_owned()));
    }

    let token_path = data_dir.join(SETUP_TOKEN_FILE_NAME);
    if token_path.is_file() {
        let content = fs::read_to_string(&token_path)?;
        return Ok(Some(content.trim().to_owned()));
    }

    fs::create_dir_all(data_dir)?;
    let mut raw_bytes = [0_u8; 32];
    entropy.fill_bytes(&mut raw_bytes);
    let token = URL_SAFE_NO_PAD.encode(raw_bytes);

    let mut open_options = OpenOptions::new();
    open_options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_options.mode(0o600);
    }

    use std::io::Write as _;
    let mut file = open_options.open(&token_path)?;
    writeln!(file, "{token}")?;
    drop(file);

    eprintln!("setup token written to {}", token_path.display());
    Ok(Some(token))
}

/// Delete the setup token file upon successful initial setup completion per `AUTH-2`.
pub fn delete_setup_token_file(data_dir: &Path) {
    let token_path = data_dir.join(SETUP_TOKEN_FILE_NAME);
    if token_path.is_file() {
        let _ = fs::remove_file(token_path);
    }
}

/// Shared application state for setup handlers.
#[derive(Clone)]
pub struct SetupAppState {
    pub pool: SqlitePool,
    pub expected_token: Option<String>,
    pub data_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    pub entropy: Arc<dyn EntropySource>,
    pub audit_hmac_key: Option<llmroller_core::seams::SecretBytes>,
}

impl std::fmt::Debug for SetupAppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupAppState")
            .field("data_dir", &self.data_dir)
            .field(
                "expected_token",
                &self.expected_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl SetupAppState {
    pub fn new(pool: SqlitePool, expected_token: Option<String>, data_dir: PathBuf) -> Self {
        Self {
            pool,
            expected_token,
            data_dir,
            clock: Arc::new(SystemClock::new()),
            entropy: Arc::new(OsEntropy::new()),
            audit_hmac_key: None,
        }
    }
}

/// Payload for `POST /admin/api/setup`.
#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
    pub setup_token: Option<String>,
}

/// Response for `POST /admin/api/setup`.
#[derive(Debug, Serialize)]
pub struct SetupResponse {
    pub user_id: UserId,
    pub username: String,
    pub role: Role,
    pub status: &'static str,
}

/// Status response for `GET /setup` or `GET /admin/api/setup/status`.
#[derive(Debug, Serialize)]
pub struct SetupStatusResponse {
    pub setup_needed: bool,
    pub token_required: bool,
}

/// Build the setup router.
pub fn router(state: SetupAppState) -> Router {
    Router::new()
        .route("/setup", get(get_setup_page))
        .route("/admin/api/setup/status", get(get_setup_status))
        .route("/admin/api/setup", post(post_setup))
        .with_state(state)
}

use askama::Template as _;

#[derive(Debug, askama::Template)]
#[template(path = "setup.html")]
pub struct SetupTemplate {}

#[doc = "Spec: AUTH-1, Spec: AUTH-3"]
async fn get_setup_page(State(state): State<SetupAppState>) -> Result<Response, SetupError> {
    let count = count_users(&state.pool).await?;
    if count > 0 {
        return Err(SetupError::AlreadyInitialized);
    }
    let template = SetupTemplate {};
    let html = template
        .render()
        .map_err(|e| SetupError::Template(e.to_string()))?;
    Ok((
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
        .into_response())
}

#[doc = "Spec: AUTH-1, Spec: AUTH-3"]
async fn get_setup_status(
    State(state): State<SetupAppState>,
) -> Result<Json<SetupStatusResponse>, SetupError> {
    let count = count_users(&state.pool).await?;
    if count > 0 {
        return Err(SetupError::AlreadyInitialized);
    }
    Ok(Json(SetupStatusResponse {
        setup_needed: true,
        token_required: state.expected_token.is_some(),
    }))
}

#[doc = "Spec: AUTH-1, Spec: AUTH-2, Spec: AUTH-3"]
async fn post_setup(
    State(state): State<SetupAppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, SetupError> {
    // 1. Permanent lockout check before anything else
    let count = count_users(&state.pool).await?;
    if count > 0 {
        return Err(SetupError::AlreadyInitialized);
    }

    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let payload: SetupRequest = if content_type.contains("application/json") {
        serde_json::from_slice(&body)
            .map_err(|e| SetupError::InvalidPayload(format!("invalid JSON payload: {e}")))?
    } else {
        let params: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        SetupRequest {
            username: params.get("username").cloned().unwrap_or_default(),
            password: params.get("password").cloned().unwrap_or_default(),
            setup_token: params
                .get("setup_token")
                .cloned()
                .filter(|s| !s.trim().is_empty()),
        }
    };

    // 2. Token validation per AUTH-2
    if let Some(ref expected) = state.expected_token {
        let provided = payload.setup_token.as_deref().unwrap_or("").trim();
        let expected_bytes = expected.as_bytes();
        let provided_bytes = provided.as_bytes();
        let valid = expected_bytes.ct_eq(provided_bytes);
        if !bool::from(valid) || provided.is_empty() {
            return Err(SetupError::InvalidToken);
        }
    }

    // 3. Serialized writer transaction per AUTH-1: BEGIN IMMEDIATE
    let mut connection = state.pool.acquire().await?;
    let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;

    // In-transaction double check
    let count_in_tx: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&mut *transaction)
        .await?;
    if count_in_tx > 0 {
        return Err(SetupError::Conflict);
    }

    let username = validate_username(&payload.username)?;
    let password_hash = hash_password(&payload.password, state.entropy.as_ref())?;
    let id = UserId::generate();
    let now_ms = state.clock.now_ms();

    sqlx::query(
        "INSERT INTO users (
            id, username, password_hash, role, must_change_password,
            totp_enabled, can_use_global_providers, disabled, created_at
         ) VALUES (?, ?, ?, 'admin', 0, 0, 1, 0, ?)",
    )
    .bind(id.to_string())
    .bind(username)
    .bind(password_hash)
    .bind(now_ms)
    .execute(&mut *transaction)
    .await
    .map_err(|err| SetupError::User(UserError::from(err)))?;

    // Bump config_version so Reloader picks up the newly created administrator (AUTH-1, REL-4)
    let _ =
        sqlx::query("UPDATE instance_meta SET config_version = config_version + 1, updated_at = ?")
            .bind(now_ms)
            .execute(&mut *transaction)
            .await;

    // Append setup.completed audit entry inside the same transaction (AUTH-1, AUD-1, F047)
    if let Some(ref audit_key) = state.audit_hmac_key {
        use llmroller_core::seams::ExposeSecret as _;
        let audit_id = llmroller_core::ids::RequestId::generate().to_string();
        let uid_str = id.to_string();
        let _ = llmroller_store::audit::append(
            &mut transaction,
            audit_key.expose_secret(),
            llmroller_store::audit::AuditEntry {
                id: &audit_id,
                ts: now_ms,
                actor_user_id: Some(&uid_str),
                ip_addr: None,
                action: "setup.completed",
                target_type: "instance_meta",
                target_id: Some("singleton"),
                detail_json: Some("{\"first_admin_created\":true}"),
            },
        )
        .await;
    }

    transaction.commit().await?;
    drop(connection);

    // 4. Delete setup token file on completion per AUTH-2
    delete_setup_token_file(&state.data_dir);

    let is_json = content_type.contains("application/json");
    if is_json {
        Ok((
            StatusCode::CREATED,
            Json(SetupResponse {
                user_id: id,
                username: username.to_owned(),
                role: Role::Admin,
                status: "created",
            }),
        )
            .into_response())
    } else {
        Ok((StatusCode::SEE_OTHER, [("location", "/login")]).into_response())
    }
}
