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
            Self::Io(_) | Self::Sqlx(_) | Self::User(_) => {
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

#[doc = "Spec: AUTH-1, Spec: AUTH-3"]
async fn get_setup_page(State(state): State<SetupAppState>) -> Result<Response, SetupError> {
    let count = count_users(&state.pool).await?;
    if count > 0 {
        return Err(SetupError::AlreadyInitialized);
    }
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>LLMRoller - First-Admin Setup</title>
    <style>
        :root { --bg: #0f172a; --card: #1e293b; --border: #334155; --text: #f8fafc; --muted: #94a3b8; --primary: #3b82f6; --primary-hover: #2563eb; --error: #ef4444; }
        * { box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; }
        body { background: var(--bg); color: var(--text); min-height: 100vh; display: flex; align-items: center; justify-content: center; padding: 1rem; }
        .card { background: var(--card); border: 1px solid var(--border); border-radius: 12px; width: 100%; max-width: 440px; padding: 2rem; box-shadow: 0 10px 25px rgba(0,0,0,0.5); }
        h1 { font-size: 1.5rem; font-weight: 700; margin-bottom: 0.5rem; text-align: center; }
        p.subtitle { color: var(--muted); font-size: 0.875rem; margin-bottom: 1.5rem; text-align: center; }
        .form-group { margin-bottom: 1.25rem; }
        label { display: block; font-size: 0.8125rem; font-weight: 600; margin-bottom: 0.375rem; color: #cbd5e1; }
        input { width: 100%; padding: 0.625rem 0.875rem; background: #0f172a; border: 1px solid var(--border); border-radius: 6px; color: #fff; font-size: 0.9375rem; outline: none; transition: border-color 0.2s; }
        input:focus { border-color: var(--primary); }
        .help { font-size: 0.75rem; color: var(--muted); margin-top: 0.25rem; }
        button { width: 100%; padding: 0.75rem; background: var(--primary); color: #fff; border: none; border-radius: 6px; font-weight: 600; font-size: 0.9375rem; cursor: pointer; transition: background 0.2s; margin-top: 0.5rem; }
        button:hover { background: var(--primary-hover); }
        .alert { display: none; background: rgba(239, 68, 68, 0.15); border: 1px solid var(--error); color: #fca5a5; padding: 0.75rem; border-radius: 6px; font-size: 0.875rem; margin-bottom: 1.25rem; }
    </style>
</head>
<body>
    <div class="card">
        <h1>LLMRoller Setup</h1>
        <p class="subtitle">Create the initial administrator account</p>
        <div id="error-box" class="alert"></div>
        <form id="setup-form" method="POST" action="/admin/api/setup">
            <div class="form-group">
                <label for="username">Administrator Username</label>
                <input id="username" name="username" type="text" required autofocus placeholder="admin">
            </div>
            <div class="form-group">
                <label for="password">Strong Password</label>
                <input id="password" name="password" type="password" required placeholder="••••••••••••" minlength="12">
                <div class="help">Must be at least 12 characters.</div>
            </div>
            <div class="form-group">
                <label for="setup_token">Setup Token</label>
                <input id="setup_token" name="setup_token" type="text" required placeholder="Paste token from setup-token file">
                <div class="help">Found in data directory setup-token file.</div>
            </div>
            <button type="submit" id="submit-btn">Initialize Administrator</button>
        </form>
    </div>
    <script>
        const form = document.getElementById('setup-form');
        const errBox = document.getElementById('error-box');
        const btn = document.getElementById('submit-btn');

        form.addEventListener('submit', async (e) => {
            e.preventDefault();
            errBox.style.display = 'none';
            btn.disabled = true;
            btn.textContent = 'Initializing...';

            const payload = {
                username: form.username.value.trim(),
                password: form.password.value,
                setup_token: form.setup_token.value.trim()
            };

            try {
                const res = await fetch('/admin/api/setup', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
                    body: JSON.stringify(payload)
                });
                const data = await res.json().catch(() => ({}));
                if (res.ok) {
                    window.location.href = '/login';
                } else {
                    errBox.textContent = data.error || ('Setup failed with status ' + res.status);
                    errBox.style.display = 'block';
                    btn.disabled = false;
                    btn.textContent = 'Initialize Administrator';
                }
            } catch (err) {
                // If fetch fails, fallback to native form submission
                form.submit();
            }
        });
    </script>
</body>
</html>"#;
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
