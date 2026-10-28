//! Embedded static assets serving HTMX, uPlot, and application styling.
//!
//! Spec: Section 8.2, Section 14.3, Requirements: UI-3, SEC-5.

use axum::Router;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, HeaderMap, IF_NONE_MATCH};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// The application JavaScript, split by domain (`ISSUE-0013`).
///
/// **The files share one global scope.** That is a deliberate choice over per-file IIFEs
/// because 53 of 54 functions are invoked *by name* from the delegated listeners in
/// `app-core.js`; wrapping each file in an IIFE would mean rewriting all of them, a rewrite
/// rather than the extraction this was. `function` declarations are global across classic
/// scripts, so a handler defined in `app-keys.js` is visible to the dispatcher in
/// `app-core.js`.
///
/// **Order is declared here and mirrored in the templates**, and a test asserts the two
/// lists match exactly (`app_module_order_matches_between_assets_and_templates`). As written
/// today the order is *not* load-bearing — every top-level `let` is read only inside a
/// callback body, so loading the files in any order still works (verified by running the
/// modules reversed). It is kept explicit and fixed because that is a property of the current
/// code, not of the design: the moment one file reads another's `let` at top level, a wrong
/// order becomes a temporal-dead-zone error, and the test comparing the two lists is what
/// makes that a compile-of-the-test failure rather than a silent page break.
///
/// `usageChart` sits in `core` because `core`'s `resize` listener reads it — harmless today
/// only because the listener fires after load, but keeping a variable with the file that owns
/// its listeners is the cheaper invariant to maintain.
pub static APP_CORE_JS: &str = include_str!("../static/app-core.js");
pub static APP_PROVIDERS_JS: &str = include_str!("../static/app-providers.js");
pub static APP_KEYS_JS: &str = include_str!("../static/app-keys.js");
pub static APP_CATALOG_JS: &str = include_str!("../static/app-catalog.js");
pub static APP_SETTINGS_JS: &str = include_str!("../static/app-settings.js");
pub static APP_CHARTS_JS: &str = include_str!("../static/app-charts.js");
pub static APP_PRICES_JS: &str = include_str!("../static/app-prices.js");
pub static APP_SESSIONS_JS: &str = include_str!("../static/app-sessions.js");
pub static APP_ACCOUNTS_JS: &str = include_str!("../static/app-accounts.js");
pub static APP_SHARES_JS: &str = include_str!("../static/app-shares.js");
pub static APP_USERS_JS: &str = include_str!("../static/app-users.js");
pub static APP_AUTH_JS: &str = include_str!("../static/app-auth.js");

/// The ordered module list: name served under `/static/`, and body.
///
/// One table so the router and any consumer agree, and so a new module is added in one place.
pub static APP_JS_MODULES: [(&str, &str); 12] = [
    ("app-core.js", APP_CORE_JS),
    ("app-providers.js", APP_PROVIDERS_JS),
    ("app-keys.js", APP_KEYS_JS),
    ("app-catalog.js", APP_CATALOG_JS),
    ("app-settings.js", APP_SETTINGS_JS),
    ("app-charts.js", APP_CHARTS_JS),
    ("app-prices.js", APP_PRICES_JS),
    ("app-sessions.js", APP_SESSIONS_JS),
    ("app-accounts.js", APP_ACCOUNTS_JS),
    ("app-shares.js", APP_SHARES_JS),
    ("app-users.js", APP_USERS_JS),
    ("app-auth.js", APP_AUTH_JS),
];

/// All modules concatenated in load order.
///
/// Kept so `/static/app.js` still resolves to the complete application. Two reasons: it is a
/// single-request fallback, and existing tests fetch that path and assert on its contents, so
/// a page that only ever loaded this file would still behave identically. Built at compile
/// time via `concat!` over the module bodies.
pub static APP_JS: &str = concat!(
    include_str!("../static/app-core.js"),
    "\n",
    include_str!("../static/app-providers.js"),
    "\n",
    include_str!("../static/app-keys.js"),
    "\n",
    include_str!("../static/app-catalog.js"),
    "\n",
    include_str!("../static/app-settings.js"),
    "\n",
    include_str!("../static/app-charts.js"),
    "\n",
    include_str!("../static/app-prices.js"),
    "\n",
    include_str!("../static/app-sessions.js"),
    "\n",
    include_str!("../static/app-accounts.js"),
    "\n",
    include_str!("../static/app-shares.js"),
    "\n",
    include_str!("../static/app-users.js"),
    "\n",
    include_str!("../static/app-auth.js"),
);
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
    // Domain modules first, so adding one is a single edit to `APP_JS_MODULES`.
    if let Some((_, body)) = APP_JS_MODULES
        .iter()
        .find(|(name, _)| *name == path.as_str())
    {
        return asset_response(&headers, "application/javascript", body);
    }
    match path.as_str() {
        "app.js" => asset_response(&headers, "application/javascript", APP_JS),
        "app.css" => asset_response(&headers, "text/css", APP_CSS),
        "htmx.min.js" => asset_response(&headers, "application/javascript", HTMX_JS),
        "uPlot.iife.min.js" => asset_response(&headers, "application/javascript", UPLOT_JS),
        "uPlot.min.css" => asset_response(&headers, "text/css", UPLOT_CSS),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
