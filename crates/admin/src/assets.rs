//! Embedded static assets serving HTMX, uPlot, and application styling.
//!
//! Spec: Section 8.2, Section 14.3, Requirements: UI-3, SEC-5.

use axum::Router;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, HeaderMap, IF_NONE_MATCH};
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

fn asset_response(headers: &HeaderMap, content_type: &'static str, body: &'static str) -> Response {
    let etag = format!(
        "\"{}\"",
        &llmroller_store::crypto::sha256_hex(body.as_bytes())[..16]
    );
    if headers
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|inm| inm == etag || inm == "*")
    {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (CACHE_CONTROL, "public, max-age=31536000, immutable"),
                (ETAG, etag.as_str()),
            ],
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, content_type),
            (CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (ETAG, etag.as_str()),
        ],
        body,
    )
        .into_response()
}

pub async fn serve_static_asset(headers: HeaderMap, Path(path): Path<String>) -> Response {
    match path.as_str() {
        "app.js" => asset_response(&headers, "application/javascript", APP_JS),
        "app.css" => asset_response(&headers, "text/css", APP_CSS),
        "htmx.min.js" => asset_response(&headers, "application/javascript", HTMX_JS),
        "uPlot.iife.min.js" => asset_response(&headers, "application/javascript", UPLOT_JS),
        "uPlot.min.css" => asset_response(&headers, "text/css", UPLOT_CSS),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
