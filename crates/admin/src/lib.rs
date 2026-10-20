#![forbid(unsafe_code)]
//! Control-plane HTTP service, setup, and administration APIs.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2
//! Spec: ./docs/srdd/14. SRDD - API and Web UI Specification.md#142-control-plane-json-api-apiuser-and-adminapi

pub mod api;
pub mod auth;
pub mod setup;

pub use api::ApiAppState;
pub use auth::AuthAppState;
use axum::Router;
use axum::middleware::from_fn;
pub use setup::SetupAppState;

/// Combine setup and authentication routers into the admin control plane router.
pub fn control_plane_router(setup_state: SetupAppState, auth_state: AuthAppState) -> Router {
    Router::new()
        .merge(setup::router(setup_state))
        .merge(auth::router(auth_state))
        .layer(from_fn(auth::security_headers_middleware))
}

/// Combine setup, authentication, and JSON API routers into the complete control plane router.
pub fn control_plane_router_full(
    setup_state: SetupAppState,
    auth_state: AuthAppState,
    api_state: ApiAppState,
) -> Router {
    Router::new()
        .merge(setup::router(setup_state))
        .merge(auth::router(auth_state))
        .merge(api::router(api_state))
        .layer(from_fn(auth::security_headers_middleware))
}
