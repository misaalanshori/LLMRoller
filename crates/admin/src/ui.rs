//! Web UI template handlers rendering `/app` and `/admin` HTML dashboards.
//!
//! Spec: ./docs/srdd/14. SRDD - API and Web UI Specification.md#143-web-ui-specification-app-and-admin
//! Requirements: UI-1, UI-2, UI-3, UI-4.

use askama::Template;
use axum::Json;
use axum::extract::State;
use axum::http::header::COOKIE;
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
    pub ui_theme: String,
    pub ui_nav_layout: String,
    pub ui_locale: String,
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
    let session = match authenticate_session(&state.auth, &headers).await {
        Ok(s) => s,
        Err(e) => {
            let accepts_html = headers
                .get(axum::http::header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|a| a.contains("text/html"));
            if accepts_html {
                return Ok((StatusCode::SEE_OTHER, [("location", "/login")]).into_response());
            }
            return Err(map_auth_error(e));
        }
    };

    let cookie_header = headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session_token =
        crate::auth::parse_cookie_value(cookie_header, state.auth.config.session_cookie_name())
            .unwrap_or("");
    let csrf_token = crate::auth::derive_session_csrf(session_token);

    let now_ms = state.auth.clock.now_ms();
    let traceability_enabled =
        match llmroller_store::settings::load_settings(&state.auth.pool).await {
            Ok(s) => {
                s.traceability_config.enabled
                    && s.traceability_config
                        .auto_disable_at
                        .is_none_or(|t| now_ms < t)
            }
            Err(_) => false,
        };

    let prefs: Option<(String, String, String)> =
        sqlx::query_as("SELECT ui_theme, ui_nav_layout, ui_locale FROM users WHERE id = ?")
            .bind(session.user.id.to_string())
            .fetch_optional(&state.auth.reader_pool)
            .await
            .unwrap_or(None);

    let (ui_theme, ui_nav_layout, ui_locale) =
        prefs.unwrap_or_else(|| ("dark".to_string(), "top_tabs".to_string(), "en".to_string()));

    let app_content = AppTemplate {}.render().unwrap_or_default();
    let base = BaseTemplate {
        title: "Personal Workspace".to_string(),
        csrf_token,
        traceability_enabled,
        is_admin: session.user.role == Role::Admin,
        ui_theme,
        ui_nav_layout,
        ui_locale,
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
    let session = match authenticate_session(&state.auth, &headers).await {
        Ok(s) => s,
        Err(e) => {
            let accepts_html = headers
                .get(axum::http::header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|a| a.contains("text/html"));
            if accepts_html {
                return Ok((StatusCode::SEE_OTHER, [("location", "/login")]).into_response());
            }
            return Err(map_auth_error(e));
        }
    };

    if session.user.role != Role::Admin {
        let accepts_html = headers
            .get(axum::http::header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("text/html"));
        if accepts_html {
            return Ok((StatusCode::SEE_OTHER, [("location", "/app")]).into_response());
        }
        return Err(map_auth_error(crate::auth::AuthError::Forbidden));
    }

    let cookie_header = headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session_token =
        crate::auth::parse_cookie_value(cookie_header, state.auth.config.session_cookie_name())
            .unwrap_or("");
    let csrf_token = crate::auth::derive_session_csrf(session_token);

    let now_ms = state.auth.clock.now_ms();
    let traceability_enabled =
        match llmroller_store::settings::load_settings(&state.auth.pool).await {
            Ok(s) => {
                s.traceability_config.enabled
                    && s.traceability_config
                        .auto_disable_at
                        .is_none_or(|t| now_ms < t)
            }
            Err(_) => false,
        };

    let prefs: Option<(String, String, String)> =
        sqlx::query_as("SELECT ui_theme, ui_nav_layout, ui_locale FROM users WHERE id = ?")
            .bind(session.user.id.to_string())
            .fetch_optional(&state.auth.reader_pool)
            .await
            .unwrap_or(None);

    let (ui_theme, ui_nav_layout, ui_locale) =
        prefs.unwrap_or_else(|| ("dark".to_string(), "top_tabs".to_string(), "en".to_string()));

    let admin_content = AdminTemplate {}.render().unwrap_or_default();
    let base = BaseTemplate {
        title: "Admin Console".to_string(),
        csrf_token,
        traceability_enabled,
        is_admin: true,
        ui_theme,
        ui_nav_layout,
        ui_locale,
        content: admin_content,
    };

    let rendered = base
        .render()
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Html(rendered).into_response())
}
