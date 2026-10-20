//! Embedded static assets serving HTMX, uPlot, and application styling.
//!
//! Spec: Section 8.2, Section 14.3, Requirements: UI-3, SEC-5.

use axum::Router;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

pub static APP_JS: &str = include_str!("../static/app.js");
pub static APP_CSS: &str = include_str!("../static/app.css");
pub static HTMX_JS: &str = include_str!("../static/htmx.min.js");
pub static UPLOT_JS: &str = include_str!("../static/uPlot.iife.min.js");
pub static UPLOT_CSS: &str = include_str!("../static/uPlot.min.css");

pub fn router() -> Router {
    Router::new().route("/static/{*path}", get(serve_static_asset))
}

pub async fn serve_static_asset(Path(path): Path<String>) -> Response {
    match path.as_str() {
        "app.js" => (
            StatusCode::OK,
            [(CONTENT_TYPE, "application/javascript")],
            APP_JS,
        )
            .into_response(),
        "app.css" => (StatusCode::OK, [(CONTENT_TYPE, "text/css")], APP_CSS).into_response(),
        "htmx.min.js" => (
            StatusCode::OK,
            [(CONTENT_TYPE, "application/javascript")],
            HTMX_JS,
        )
            .into_response(),
        "uPlot.iife.min.js" => (
            StatusCode::OK,
            [(CONTENT_TYPE, "application/javascript")],
            UPLOT_JS,
        )
            .into_response(),
        "uPlot.min.css" => {
            (StatusCode::OK, [(CONTENT_TYPE, "text/css")], UPLOT_CSS).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
