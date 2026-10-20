#![forbid(unsafe_code)]
//! Prometheus metrics exporter with closed label allow-list per OBS-1.
//!
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#74-portability-maintainability-observability-and-privacy-por-mnt-obs-priv
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Phase: ./docs/bigbang/big-bang-phases-2.md#22-sub-phase-22-client-key-llr_-auth-rotation-grace-model-request-gates-g1g6-and-listeners

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

/// Closed allowlist of all 10 permissible Prometheus metric labels per OBS-1.
pub const CLOSED_LABEL_ALLOWLIST: [&str; 10] = [
    "provider_kind",
    "provider_id",
    "protocol",
    "endpoint",
    "status_class",
    "outcome",
    "strategy",
    "job",
    "reason",
    "model_bucket",
];

/// Errors returned when metric labels violate the closed allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MetricLabelError {
    #[error("metric label {0:?} is not in the closed label allowlist (OBS-1)")]
    DisallowedLabel(String),
}

/// Validate that every label key belongs to the closed allowlist.
pub fn validate_metric_labels(labels: &[(&str, &str)]) -> Result<(), MetricLabelError> {
    for &(key, _) in labels {
        if !CLOSED_LABEL_ALLOWLIST.contains(&key) {
            return Err(MetricLabelError::DisallowedLabel(key.to_owned()));
        }
    }
    Ok(())
}

/// Handler for the `/metrics` endpoint on the loopback metrics listener (:9090).
pub async fn metrics_handler() -> Response {
    let mut output = String::new();
    output.push_str("# HELP llmroller_requests_total Total requests processed\n");
    output.push_str("# TYPE llmroller_requests_total counter\n");
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        output,
    )
        .into_response()
}
