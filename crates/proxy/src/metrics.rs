#![forbid(unsafe_code)]
//! Prometheus metrics exporter with closed label allow-list per OBS-1.
//!
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#74-portability-maintainability-observability-and-privacy-por-mnt-obs-priv
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Phase: ./docs/bigbang/big-bang-phases-2.md#22-sub-phase-22-client-key-llr_-auth-rotation-grace-model-request-gates-g1g6-and-listeners

use std::sync::OnceLock;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
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

static RECORDER_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Retrieve or install the global Prometheus recorder handle.
pub fn get_or_init_prometheus_recorder() -> &'static PrometheusHandle {
    RECORDER_HANDLE.get_or_init(|| {
        PrometheusBuilder::new()
            .install_recorder()
            .expect("install Prometheus recorder")
    })
}

/// Record a completed or failed request metric with validated closed-allowlist labels.
pub fn record_request_metric(
    endpoint: &'static str,
    status_class: &'static str,
    outcome: &'static str,
) {
    let _ = get_or_init_prometheus_recorder();
    let labels = [
        ("endpoint", endpoint),
        ("status_class", status_class),
        ("outcome", outcome),
    ];
    let _ = validate_metric_labels(&labels);
    metrics::counter!("llmroller_requests_total", "endpoint" => endpoint, "status_class" => status_class, "outcome" => outcome).increment(1);
}

/// Record an upstream 403 that did not match any classified error pattern (API-8).
pub fn record_unclassified_403(provider_id: &str) {
    let _ = get_or_init_prometheus_recorder();
    metrics::counter!(
        "llmroller_upstream_unclassified_403_total",
        "provider_id" => provider_id.to_owned()
    )
    .increment(1);
}

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
    let recorder = get_or_init_prometheus_recorder();
    let output = recorder.render();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        output,
    )
        .into_response()
}
