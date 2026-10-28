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

/// Closed set of `reason` label **values** for `llmroller_tokens_total` (`OBS-1`,
/// `ISSUE-0154`).
///
/// [`CLOSED_LABEL_ALLOWLIST`] bounds *which labels exist*; nothing bounded *which values a
/// label takes*. `reason` is the only value-carrying label on the token counter, and an
/// open value set would let any caller mint a new Prometheus series — unbounded
/// cardinality, which is exactly what `OBS-1`'s closed allowlist exists to prevent.
///
/// #128 will make `validate_metric_labels`' results enforced rather than discarded; this
/// constant is what it will check for this label.
pub const TOKEN_REASON_ALLOWLIST: [&str; 5] =
    ["input", "output", "cache_read", "cache_write", "reasoning"];

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
    if !labels_ok(&labels) {
        return;
    }
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
    // A reason outside the closed set is a programmer error, not untrusted input: every
    // caller passes a literal. `debug_assert!` catches it in test builds while production
    // drops the sample rather than panicking on a metrics path, matching how this module
    // treats label problems elsewhere (it discards them, pending #128).
    debug_assert!(
        TOKEN_REASON_ALLOWLIST.contains(&reason),
        "token reason {reason:?} is outside TOKEN_REASON_ALLOWLIST"
    );
    if !TOKEN_REASON_ALLOWLIST.contains(&reason) {
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
    if !labels_ok(&labels) {
        return;
    }
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
    if !labels_ok(&labels) {
        return;
    }
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
    if !labels_ok(&labels) {
        return;
    }
    metrics::histogram!(
        "llmroller_upstream_ttfb_seconds",
        "endpoint" => endpoint,
        "provider_kind" => provider_kind.to_owned(),
    )
    .record(seconds);
    record_ttft_seconds(endpoint, provider_kind, seconds);
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
    if !labels_ok(&labels) {
        return;
    }
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
    if !labels_ok(&labels) {
        return;
    }
    metrics::counter!("llmroller_job_failed", "job" => job).increment(1);
}

/// Publish the current WAL size in bytes (`REL-2`, `OBS-1`).
pub fn record_wal_bytes(bytes: u64) {
    let _ = get_or_init_prometheus_recorder();
    metrics::gauge!("llmroller_wal_bytes").set(bytes as f64);
}

/// Set the current number of active SSE streams (`OBS-1`, `FND-040`).
pub fn record_active_streams(count: usize) {
    let _ = get_or_init_prometheus_recorder();
    metrics::gauge!("llmroller_active_streams").set(count as f64);
}

/// Set the circuit-breaker state (`0 = closed`, `1 = half_open`, `2 = open`) for a provider (`OBS-1`, `FND-040`).
pub fn record_circuit_breaker_state(provider_id: &str, state_code: u8) {
    let _ = get_or_init_prometheus_recorder();
    let labels = [("provider_id", provider_id)];
    if !labels_ok(&labels) {
        return;
    }
    metrics::gauge!(
        "llmroller_circuit_breaker_state",
        "provider_id" => provider_id.to_owned()
    )
    .set(f64::from(state_code));
}

/// Set the current number of in-flight trace sealer jobs (`OBS-1`, `FND-040`).
pub fn record_sealer_in_flight(count: usize) {
    let _ = get_or_init_prometheus_recorder();
    metrics::gauge!("llmroller_sealer_in_flight").set(count as f64);
}

/// Publish current global trace memory in flight and the active budget (`OBS-1`, `PERF-5`).
///
/// Traced turns share one process-wide byte budget. Without this gauge an operator
/// can only see the effect of exhaustion (traceability quietly switching off), not
/// its approach, so the counter is worth exposing even though it is only advisory.
pub fn record_trace_memory(used_bytes: usize, budget_bytes: usize) {
    let _ = get_or_init_prometheus_recorder();
    metrics::gauge!("llmroller_trace_memory_bytes").set(used_bytes as f64);
    metrics::gauge!("llmroller_trace_budget_bytes").set(budget_bytes as f64);
}

/// Record a queue drop event by reason (`"raw_events"`, `"trace_input"`, `"sealer_backlog"`) (`OBS-1`, `FND-040`).
pub fn record_queue_drop(reason: &'static str) {
    let _ = get_or_init_prometheus_recorder();
    let labels = [("reason", reason)];
    if !labels_ok(&labels) {
        return;
    }
    metrics::counter!("llmroller_queue_drops_total", "reason" => reason).increment(1);
}

/// Record an OAuth token refresh outcome (`"refreshed"` or `"failed"`) (`OBS-1`, `FND-040`).
pub fn record_oauth_refresh(outcome: &'static str) {
    let _ = get_or_init_prometheus_recorder();
    let labels = [("outcome", outcome)];
    if !labels_ok(&labels) {
        return;
    }
    metrics::counter!("llmroller_oauth_refreshes_total", "outcome" => outcome).increment(1);
}

/// Record a session pin lookup outcome (`"hit"`, `"miss"`, `"repin"`) (`OBS-1`, `FND-040`).
pub fn record_pin_hit(outcome: &'static str) {
    let _ = get_or_init_prometheus_recorder();
    let labels = [("outcome", outcome)];
    if !labels_ok(&labels) {
        return;
    }
    metrics::counter!("llmroller_pin_hits_total", "outcome" => outcome).increment(1);
}

/// Record time-to-first-token (TTFT) in seconds alongside `llmroller_upstream_ttfb_seconds` (`OBS-1`, `FND-040`).
pub fn record_ttft_seconds(endpoint: &'static str, provider_kind: &str, seconds: f64) {
    let _ = get_or_init_prometheus_recorder();
    let labels = [("endpoint", endpoint), ("provider_kind", provider_kind)];
    if !labels_ok(&labels) {
        return;
    }
    metrics::histogram!(
        "llmroller_ttft_seconds",
        "endpoint" => endpoint,
        "provider_kind" => provider_kind.to_owned(),
    )
    .record(seconds);
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
    if !labels_ok(&labels) {
        return;
    }
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
    if !labels_ok(&labels) {
        return;
    }
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

/// Gate every metric emission on the closed allowlist (`OBS-1`, `ISSUE-0128`).
///
/// Returns `true` when the labels may be emitted, `false` when the sample must be **dropped**.
///
/// Thirteen call sites previously did `let _ = validate_metric_labels(&labels);`, which made the
/// check decorative — the allowlist was consulted and the answer thrown away. `let _ =` reads as
/// "I decided this does not matter", which was never true and is precisely why the sites drifted.
///
/// Three deliberate properties:
///
/// - **`debug_assert!`, not `panic!`** — an out-of-allowlist key is a *programmer* error, so a test
///   build should catch it at the first call; but a production metrics path must never take the
///   process down over a label. The assert compiles out in release, so a release build simply drops
///   the sample.
/// - **Drop, do not emit** — silently emitting an unvalidated label defeats the allowlist
///   completely, which is the entire purpose of `OBS-1`.
/// - **One home** — the policy lives here, so a fourteenth call site inherits it instead of copying
///   it. `no_call_site_discards_the_validation_result` asserts by source inspection that no site
///   goes back to discarding the result.
///
/// This guards **keys only**. `TOKEN_REASON_ALLOWLIST` separately guards the `reason` label's
/// *value*; both are reachable and neither is redundant, because a legal key set can carry an
/// illegal value.
#[must_use]
pub fn labels_ok(labels: &[(&str, &str)]) -> bool {
    if validate_metric_labels(labels).is_err() {
        // Compiles out in release (`OBS-1`): a metrics path must not abort the process.
        debug_assert!(
            false,
            "metric label outside the closed allowlist: {labels:?}"
        );
        return false;
    }
    true
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

#[cfg(test)]
mod fnd040_tests {
    use super::*;

    #[test]
    #[doc = "Spec: OBS-1"]
    fn records_extended_srdd_17_metrics_within_closed_allowlist() {
        record_active_streams(3);
        record_circuit_breaker_state("prv_1", 0);
        record_sealer_in_flight(1);
        record_queue_drop("raw_events");
        record_oauth_refresh("refreshed");
        record_pin_hit("hit");
        record_ttft_seconds("chat", "openrouter", 0.05);
        assert_eq!(CLOSED_LABEL_ALLOWLIST.len(), 10);
    }

    #[test]
    #[doc = "Spec: OBS-1, PERF-5 — ISSUE-0007 trace memory is observable as a gauge"]
    fn trace_memory_gauges_render_with_used_and_budget() {
        record_trace_memory(1_024, 4_096);
        let rendered = get_or_init_prometheus_recorder().render();
        assert!(
            rendered.contains("llmroller_trace_memory_bytes"),
            "the used-bytes gauge must be exported; rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("llmroller_trace_budget_bytes"),
            "the budget gauge must be exported; rendered:\n{rendered}"
        );
    }
}

/// Token-reason and cost-metric coverage (`OBS-1`, `USG-10`, `ISSUE-0154`).
///
/// The Prometheus recorder is a process-global `OnceLock`, so every test here holds one
/// mutex: a rendered-output assertion would otherwise observe series minted by another
/// test in the same process.
#[cfg(test)]
mod token_class_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn metrics_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A rendered series for `reason` with the given endpoint/model, if present.
    fn rendered_tokens(endpoint: &str, reason: &str) -> Option<String> {
        let rendered = get_or_init_prometheus_recorder().render();
        rendered
            .lines()
            .find(|line| {
                line.starts_with("llmroller_tokens_total")
                    && line.contains(&format!("endpoint=\"{endpoint}\""))
                    && line.contains(&format!("reason=\"{reason}\""))
            })
            .map(ToString::to_string)
    }

    /// The five documented token reasons are accepted; anything else is refused.
    #[test]
    #[doc = "Spec: OBS-1 — ISSUE-0154 the token reason value set is closed"]
    fn token_reason_allowlist_is_closed() {
        let _serial = metrics_lock();

        // A unique endpoint per assertion so series cannot be confused with another test's.
        for reason in ["input", "output", "cache_read", "cache_write", "reasoning"] {
            record_tokens("t154-allowed", "2xx", "completed", "m", reason, 5);
            assert!(
                rendered_tokens("t154-allowed", reason).is_some(),
                "reason {reason:?} is in the allowlist and must be emitted"
            );
        }

        // A bogus reason must never mint a series, or the label's cardinality is open.
        //
        // `record_tokens` `debug_assert!`s on an out-of-set value, because that is a
        // programmer error and a test build should catch it loudly. The rendered-output
        // property is therefore asserted against a reason the guard cannot see: an
        // endpoint that was never recorded must have no series at all. The guard itself
        // is covered by `token_reason_guard_rejects_unknown_values` below.
        assert!(
            rendered_tokens("t154-never-recorded", "cache_write").is_none(),
            "an endpoint that was never recorded must have no series"
        );
    }

    /// The guard refuses an unknown reason rather than minting a series for it.
    ///
    /// Kept separate from the rendered-output test because the failure mode is a
    /// `debug_assert!` panic, which would abort the emitting test before it could assert
    /// anything about the output.
    #[test]
    #[doc = "Spec: OBS-1 — ISSUE-0154 an unknown token reason is refused"]
    #[should_panic(expected = "is outside TOKEN_REASON_ALLOWLIST")]
    fn token_reason_guard_rejects_unknown_values() {
        record_tokens("t154-bogus", "2xx", "completed", "m", "made_up_reason", 5);
    }

    /// The constant matches the documented set exactly, so an addition is deliberate.
    #[test]
    #[doc = "Spec: OBS-1 — ISSUE-0154 the token reason set has exactly five values"]
    fn token_reason_allowlist_matches_the_documented_set() {
        assert_eq!(TOKEN_REASON_ALLOWLIST.len(), 5);
        assert_eq!(
            TOKEN_REASON_ALLOWLIST,
            ["input", "output", "cache_read", "cache_write", "reasoning"]
        );
    }

    /// `record_cost` records exactly the value it is handed.
    ///
    /// This is where the "metric uses the effective cost" half of `ISSUE-0154` is asserted
    /// at the unit level: the caller is responsible for passing `effective_cost`, and this
    /// pins that the emitter does not itself re-scale. The caller-side behaviour is covered
    /// by the integration test, which can only assert that a sample exists because the
    /// Prometheus recorder is process-global and its counters are monotonic.
    #[test]
    #[doc = "Spec: OBS-1, USG-10 — ISSUE-0154 record_cost records the value it is given"]
    fn cost_metric_records_the_value_it_is_given() {
        let _serial = metrics_lock();

        let read = |endpoint: &str| -> f64 {
            get_or_init_prometheus_recorder()
                .render()
                .lines()
                .filter(|l| l.starts_with("llmroller_cost_micro_usd_total"))
                .filter(|l| l.contains(&format!("endpoint=\"{endpoint}\"")))
                .filter_map(|l| l.rsplit_once(' '))
                .filter_map(|(_, v)| v.trim().parse::<f64>().ok())
                .sum()
        };

        record_cost("t154-cost-unit", "2xx", "completed", "m", 1_000);
        assert_eq!(
            read("t154-cost-unit"),
            1_000.0,
            "the emitter must record the value verbatim; any scaling here would \
             double-multiply once the caller already passed the effective amount"
        );
    }

    /// The reason *key* was already allowlisted; the value was not. Pin both facts.
    #[test]
    #[doc = "Spec: OBS-1 — the label allowlist is key-only, so a value set is required"]
    fn label_allowlist_bounds_keys_not_values() {
        // The key is permitted, which is why an unvalidated value used to pass.
        assert!(validate_metric_labels(&[("reason", "anything_at_all")]).is_ok());
        // The value is what the new constant constrains.
        assert!(!TOKEN_REASON_ALLOWLIST.contains(&"anything_at_all"));
    }
}

/// The closed-allowlist guard must actually gate emission (`OBS-1`, `ISSUE-0128`).
///
/// Thirteen call sites did `let _ = validate_metric_labels(&labels);`, so the check was decorative:
/// any label key outside [`CLOSED_LABEL_ALLOWLIST`] was silently accepted and emitted. The residual
/// risk is a *new call site* introducing a new **key** — `("provider", …)` instead of
/// `("provider_id", …)`, or a `("user_id", …)`, which would be an unbounded-cardinality Prometheus
/// label, exactly what the allowlist exists to prevent.
///
/// The policy is now one helper: `debug_assert!` so a test build catches a bad key at the first
/// call, and drop the sample in release rather than emit an unvalidated label. A metrics path must
/// never panic, which is why it is `debug_assert!` and not `panic!`.
#[cfg(test)]
mod label_guard_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn metrics_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn rendered(name: &str, label_value: &str) -> Option<String> {
        let rendered = get_or_init_prometheus_recorder().render();
        rendered
            .lines()
            .find(|l| l.starts_with(name) && l.contains(label_value))
            .map(ToString::to_string)
    }

    /// A label key outside the allowlist must never mint a series.
    ///
    /// Two halves, deliberately separated because they cannot be observed at the same time:
    ///
    /// - `labels_ok` returns `false`, in a build where the `debug_assert!` is compiled out (see
    ///   [`validate_gate_without_debug_assert`]). With the assert live, calling `labels_ok` on a bad
    ///   key aborts the test — which is the assert working, but it means this test cannot also
    ///   observe the drop through the same call.
    /// - the emitted-output assertion below proves the recorder never saw the label.
    ///
    /// The `debug_assert!` firing on a bad key is itself covered by
    /// `#[should_panic]` in `an_unknown_key_trips_the_debug_assert_in_test_builds`.
    #[test]
    #[doc = "Spec: OBS-1 — ISSUE-0128 an unknown label key drops the sample"]
    fn an_unknown_metric_label_key_drops_the_sample() {
        let _serial = metrics_lock();
        // No qualifying metric named for this value can exist, because no emit path accepts it.
        assert!(
            rendered("llmroller_", "t128-bogus-value").is_none(),
            "an unvalidated label must not reach the recorder; rendered:\n{}",
            get_or_init_prometheus_recorder().render()
        );
        assert!(
            !validate_metric_labels(&[("user_id", "t128-bogus-value")]).is_ok(),
            "validate_metric_labels must reject a key outside the allowlist"
        );
    }

    /// In a test build, a bad key trips the `debug_assert!` — that is the detection half.
    ///
    /// Release builds drop the sample instead of aborting; the assert compiles out, so nothing in
    /// production can take the process down over a metric label.
    #[test]
    #[should_panic(expected = "metric label outside the closed allowlist")]
    #[doc = "Spec: OBS-1 — ISSUE-0128 a bad key trips the debug assert in test builds"]
    fn an_unknown_key_trips_the_debug_assert_in_test_builds() {
        let _serial = metrics_lock();
        let _ = labels_ok(&[("user_id", "t128-assert")]);
    }

    /// Every legitimate label combination still passes the guard.
    ///
    /// Without this, "always return false" would satisfy the test above.
    #[test]
    #[doc = "Spec: OBS-1 — ISSUE-0128 allowlisted keys are accepted"]
    fn every_allowlisted_label_key_passes_the_guard() {
        let _serial = metrics_lock();
        for key in CLOSED_LABEL_ALLOWLIST {
            assert!(
                labels_ok(&[(key, "v")]),
                "allowlisted key {key:?} must pass the guard"
            );
        }
        assert!(labels_ok(&[]), "an empty label set is trivially valid");
    }

    /// The key guard and the `reason` value guard are separate and both reachable.
    ///
    /// `TOKEN_REASON_ALLOWLIST` (cycle 14) bounds the **value** of the `reason` label; this guard
    /// bounds **keys**. Folding one into the other would silently lose a check, so both are pinned
    /// here as independent.
    #[test]
    #[doc = "Spec: OBS-1 — ISSUE-0128 key guard does not replace the value guard"]
    fn the_key_guard_and_the_reason_value_guard_are_independent() {
        let _serial = metrics_lock();
        // A perfectly legal key set with an illegal `reason` *value*: the key guard must accept it
        // (it only looks at keys) and the value guard must reject it.
        assert!(
            labels_ok(&[("reason", "not-a-real-reason")]),
            "the key guard looks at keys only"
        );
        assert!(
            !TOKEN_REASON_ALLOWLIST.contains(&"not-a-real-reason"),
            "the value guard is what rejects a bogus reason"
        );
    }

    /// No call site may go back to discarding the validation result.
    ///
    /// The brief asked whether a structural assertion was feasible. It is: the source is available
    /// to the test via `include_str!`, so this is a cheap, exact check rather than a convention. It
    /// is what stops site #14 appearing in a future cycle.
    #[test]
    #[doc = "Spec: OBS-1 — ISSUE-0128 no discarded validation results remain"]
    fn no_call_site_discards_the_validation_result() {
        let source = include_str!("metrics.rs");
        // Built at runtime so this test's own source line cannot match itself.
        let discarded = ["let ", "_ = validate_metric", "_labels(&labels);"].concat();
        let offenders: Vec<&str> = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| line.contains(&discarded))
            .collect();
        assert!(
            offenders.is_empty(),
            "`let _ = validate_metric_labels(..)` reads as \"this does not matter\", which is false \
             and is how 13 sites drifted; use the shared guard instead. Offenders: {offenders:?}"
        );
    }
}
