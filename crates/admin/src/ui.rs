//! Web UI template handlers rendering `/app` and `/admin` HTML dashboards.
//!
//! Spec: ./docs/srdd/14. SRDD - API and Web UI Specification.md#143-web-ui-specification-app-and-admin
//! Requirements: UI-1, UI-2, UI-3, UI-4.

use askama::Template;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};

use crate::api::{ApiAppState, map_auth_error, map_price_error};
use crate::auth::authenticate_session;
use llmroller_core::rbac::Role;

#[derive(Debug, Template)]
#[template(path = "base.html")]
pub struct BaseTemplate {
    pub title: String,
    pub csrf_token: String,
    pub traceability_enabled: bool,
    pub is_admin: bool,
    pub content: String,
}

#[derive(Debug, Template)]
#[template(path = "app.html")]
pub struct AppTemplate {}

#[derive(Debug, Template)]
#[template(path = "admin.html")]
pub struct AdminTemplate {}

/// Handler for `GET /app` rendering the Personal Workspace.
pub async fn get_app_ui(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let session = authenticate_session(&state.auth, &headers)
        .await
        .map_err(map_auth_error)?;

    let app_content = AppTemplate {}.render().unwrap_or_default();
    let base = BaseTemplate {
        title: "Personal Workspace".to_string(),
        csrf_token: session.session.csrf_hash,
        traceability_enabled: false,
        is_admin: session.user.role == Role::Admin,
        content: app_content,
    };

    let rendered = base
        .render()
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Html(rendered).into_response())
}

/// Handler for `GET /admin` rendering the Admin Console.
pub async fn get_admin_ui(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let session = authenticate_session(&state.auth, &headers)
        .await
        .map_err(map_auth_error)?;

    if session.user.role != Role::Admin {
        return Err(map_auth_error(crate::auth::AuthError::Forbidden));
    }

    let admin_content = AdminTemplate {}.render().unwrap_or_default();
    let base = BaseTemplate {
        title: "Admin Console".to_string(),
        csrf_token: session.session.csrf_hash,
        traceability_enabled: false,
        is_admin: true,
        content: admin_content,
    };

    let rendered = base
        .render()
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Html(rendered).into_response())
}
