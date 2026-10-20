#![forbid(unsafe_code)]
//! Data plane and metrics HTTP listener routers.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#22-sub-phase-22-client-key-llr_-auth-rotation-grace-model-request-gates-g1g6-and-listeners

use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::auth::{ProxyAuthState, client_key_auth_middleware};
use crate::metrics::metrics_handler;

/// Build the data plane router serving `/v1/*`, `/healthz`, and `/readyz`.
pub fn data_plane_router(state: ProxyAuthState) -> Router {
    let authed_v1 = Router::new()
        .route("/v1/models", get(v1_models_stub))
        .layer(from_fn_with_state(
            state.clone(),
            client_key_auth_middleware,
        ));

    Router::new()
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler))
        .merge(authed_v1)
        .with_state(state)
}

/// Build the loopback metrics router serving `/metrics`.
pub fn metrics_router() -> Router {
    Router::new().route("/metrics", get(metrics_handler))
}

async fn healthz_handler() -> Response {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

async fn readyz_handler(State(state): State<ProxyAuthState>) -> Response {
    if state.is_ready.load(Ordering::SeqCst) {
        (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ready" })),
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "not_ready" })),
        )
            .into_response()
    }
}

async fn v1_models_stub() -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "object": "list",
            "data": []
        })),
    )
        .into_response()
}
