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

/// Maximum number of distinct canonical model IDs tracked as their own
/// `model_bucket`; everything else is grouped under `"other"` (OBS-1).
const MAX_MODEL_BUCKETS: usize = 50;

/// Map a canonical model ID onto a bounded `model_bucket` label (OBS-1).
///
/// The first [`MAX_MODEL_BUCKETS`] distinct IDs seen become buckets so that a
/// long tail of ephemeral model IDs can never explode Prometheus cardinality.
fn model_bucket(model_id: &str) -> String {
    use std::collections::HashMap;
    use std::sync::Mutex;

    static BUCKETS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    let buckets = BUCKETS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = buckets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(count) = guard.get_mut(model_id) {
        *count += 1;
        return model_id.to_owned();
    }
    if guard.len() < MAX_MODEL_BUCKETS {
        guard.insert(model_id.to_owned(), 1);
        return model_id.to_owned();
    }
    "other".to_owned()
}

/// Record one route selection with the closed allow-list labels required by
/// SRDD Section 10.3.3 step 4 (`OBS-1`).
pub fn record_routing_selection(
    provider_kind: &str,
    provider_id: &str,
    protocol: &str,
    outcome: &str,
    strategy: &str,
    model_id: &str,
) {
    let _ = get_or_init_prometheus_recorder();
    let bucket = model_bucket(model_id);
    let labels = [
        ("provider_kind", provider_kind),
        ("provider_id", provider_id),
        ("protocol", protocol),
        ("outcome", outcome),
        ("strategy", strategy),
        ("model_bucket", bucket.as_str()),
    ];
    let _ = validate_metric_labels(&labels);
    metrics::counter!(
        "llmroller_routing_selections_total",
        "provider_kind" => provider_kind.to_owned(),
        "provider_id" => provider_id.to_owned(),
        "protocol" => protocol.to_owned(),
        "outcome" => outcome.to_owned(),
        "strategy" => strategy.to_owned(),
        "model_bucket" => bucket,
    )
    .increment(1);
}

/// Mark a supervised background job as permanently failed (Section 5.5.3).
///
/// `job` is one of the closed set of static job names ("discovery", "usage_poller",
/// "retention", "reloader", "maintenance"), never user input (OBS-1).
pub fn record_job_failed(job: &'static str) {
    let _ = get_or_init_prometheus_recorder();
    let labels = [("job", job)];
    let _ = validate_metric_labels(&labels);
    metrics::counter!("llmroller_job_failed", "job" => job).increment(1);
}

/// Publish the current WAL size in bytes (`REL-2`, `OBS-1`).
pub fn record_wal_bytes(bytes: u64) {
    let _ = get_or_init_prometheus_recorder();
    metrics::gauge!("llmroller_wal_bytes").set(bytes as f64);
}

/// Record a failed upstream attempt that triggered failover (RTE-8).
pub fn record_failover() {
    let _ = get_or_init_prometheus_recorder();
    metrics::counter!("llmroller_failovers_total").increment(1);
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
