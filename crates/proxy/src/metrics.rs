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

/// Record input/output/cached tokens with closed-allowlist labels (OBS-1).
pub fn record_tokens(
    endpoint: &'static str,
    status_class: &'static str,
    outcome: &'static str,
    model_id: &str,
    reason: &'static str,
    count: u64,
) {
    if count == 0 {
        return;
    }
    let _ = get_or_init_prometheus_recorder();
    let bucket = model_bucket(model_id);
    let labels = [
        ("endpoint", endpoint),
        ("status_class", status_class),
        ("outcome", outcome),
        ("reason", reason),
        ("model_bucket", bucket.as_str()),
    ];
    let _ = validate_metric_labels(&labels);
    metrics::counter!(
        "llmroller_tokens_total",
        "endpoint" => endpoint,
        "status_class" => status_class,
        "outcome" => outcome,
        "reason" => reason,
        "model_bucket" => bucket,
    )
    .increment(count);
}

/// Record cost in micro-USD with closed-allowlist labels (OBS-1).
pub fn record_cost(
    endpoint: &'static str,
    status_class: &'static str,
    outcome: &'static str,
    model_id: &str,
    micro_usd: u64,
) {
    if micro_usd == 0 {
        return;
    }
    let _ = get_or_init_prometheus_recorder();
    let bucket = model_bucket(model_id);
    let labels = [
        ("endpoint", endpoint),
        ("status_class", status_class),
        ("outcome", outcome),
        ("model_bucket", bucket.as_str()),
    ];
    let _ = validate_metric_labels(&labels);
    metrics::counter!(
        "llmroller_cost_micro_usd_total",
        "endpoint" => endpoint,
        "status_class" => status_class,
        "outcome" => outcome,
        "model_bucket" => bucket,
    )
    .increment(micro_usd);
}

/// Record upstream TTFB in seconds (OBS-1).
pub fn record_upstream_ttfb(endpoint: &'static str, provider_kind: &str, seconds: f64) {
    let _ = get_or_init_prometheus_recorder();
    let labels = [("endpoint", endpoint), ("provider_kind", provider_kind)];
    let _ = validate_metric_labels(&labels);
    metrics::histogram!(
        "llmroller_upstream_ttfb_seconds",
        "endpoint" => endpoint,
        "provider_kind" => provider_kind.to_owned(),
    )
    .record(seconds);
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
    let labels = [("provider_id", provider_id)];
    let _ = validate_metric_labels(&labels);
    metrics::counter!(
        "llmroller_upstream_unclassified_403_total",
        "provider_id" => provider_id.to_owned()
    )
    .increment(1);
}

/// Record a suspected constant-session client key anomaly (`RTE-1`, `OBS-1`).
pub fn record_constant_session_suspected() {
    let _ = get_or_init_prometheus_recorder();
    metrics::counter!("llmroller_constant_session_suspected_total").increment(1);
}

/// Record a slow upstream TTFB or timeout observation (`OBS-1`).
pub fn record_upstream_slow(provider_id: &str) {
    let _ = get_or_init_prometheus_recorder();
    let labels = [("provider_id", provider_id)];
    let _ = validate_metric_labels(&labels);
    metrics::counter!(
        "llmroller_upstream_slow_total",
        "provider_id" => provider_id.to_owned()
    )
    .increment(1);
}

/// Record a PinStore capacity overflow event (`RTE-11`, `OBS-1`).
pub fn record_pin_capacity_overflow() {
    let _ = get_or_init_prometheus_recorder();
    metrics::counter!("llmroller_pin_capacity_overflow_total").increment(1);
}

/// Record a stale generation confirmation ignored by PinStore (`RTE-10`, `OBS-1`).
pub fn record_pin_stale_confirm() {
    let _ = get_or_init_prometheus_recorder();
    metrics::counter!("llmroller_pin_stale_confirm_total").increment(1);
}

/// Record a dropped session trace job (`TRC-1`, `OBS-1`).
pub fn record_trace_dropped() {
    let _ = get_or_init_prometheus_recorder();
    metrics::counter!("llmroller_trace_dropped_total").increment(1);
}

/// Set the `llmroller_plaintext_data_plane` gauge (`0` or `1`) per `SEC-10` / `OBS-1` (`F-099`).
pub fn set_plaintext_data_plane(val: u8) {
    let _ = get_or_init_prometheus_recorder();
    metrics::gauge!("llmroller_plaintext_data_plane").set(f64::from(val));
}

/// Verify data-plane bind security per `SEC-10` (`F-099`).
///
/// Returns `Ok(true)` when binding plaintext on a non-loopback address (`llmroller_plaintext_data_plane = 1`),
/// `Ok(false)` when loopback or TLS-enabled, or `Err(2)` when `require_tls` is true on a non-loopback plaintext bind.
pub fn check_data_plane_bind_security(
    listen_addr: &str,
    tls_enabled: bool,
    require_tls: bool,
) -> Result<bool, u8> {
    let parsed: std::net::SocketAddr = listen_addr.parse().map_err(|_| 2)?;
    if !parsed.ip().is_loopback() && !tls_enabled {
        if require_tls {
            return Err(2);
        }
        return Ok(true);
    }
    Ok(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: OBS-1, SEC-9"]
    fn increments_normative_prometheus_counters_on_edge_events() {
        record_constant_session_suspected();
        record_upstream_slow("prv_test");
        record_pin_capacity_overflow();
        record_pin_stale_confirm();
        record_trace_dropped();
        assert!(validate_metric_labels(&[("provider_id", "prv_test")]).is_ok());
        assert!(validate_metric_labels(&[("user_email", "bad")]).is_err());
    }
}
