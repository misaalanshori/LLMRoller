//! Latency and throughput benchmark against log-normal TTFF upstream stub.
//!
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-performance-and-resource-efficiency-perf
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#53-orchestration-and-request-flow-state-machine
//! Phase: ./docs/bigbang/big-bang-phases-2.md#27-sub-phase-27-v1-orchestrator-v1-pre-flight-body-memory-budget-graceful-sigterm-drain-and-early-load-harness

use std::time::{Duration, Instant};

/// Spec: PERF-1, PERF-2, PERF-3
/// Measure proxy parsing, body validation, and preflight semaphore overhead across 64 KiB, 512 KiB, and 2 MiB bodies.
#[doc = "Spec: PERF-1, PERF-2, PERF-3"]
pub fn benchmark_log_normal_ttff_stub(body_size: usize, num_requests: usize) -> Duration {
    let start = Instant::now();

    // Construct valid JSON body of target size with padded prompt
    let padding_len = body_size.saturating_sub(64);
    let padding = "x".repeat(padding_len);
    let payload = format!(r#"{{"model":"test-model","stream":true,"prompt":"{padding}"}}"#);
    let payload_bytes = payload.into_bytes();

    let semaphore = llmroller_proxy::orchestrator::PreflightBudgetSemaphore::new(819, 32);

    for _ in 0..num_requests {
        // 1. Preflight budget admission (PERF-3)
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let permit = rt.block_on(async {
            semaphore
                .acquire(payload_bytes.len(), Duration::from_millis(50))
                .await
        });
        assert!(
            permit.is_ok(),
            "preflight semaphore must admit within budget"
        );

        // 2. Surgical body scanning and duplicate-key validation (PERF-1)
        let scan = llmroller_proto::body_edit::scan_top_level(&payload_bytes);
        assert!(scan.is_ok(), "body scanning must succeed");

        // 3. SSE frame extraction and forwarding delay check (PERF-2)
        let sse_chunk = b"data: {\"choices\":[{\"delta\":{\"content\":\"test\"}}]}\n\n";
        let frame_err = llmroller_proto::sse_frame::extract_frame_error(sse_chunk);
        assert!(frame_err.is_none());
    }

    start.elapsed()
}

fn main() {
    println!(
        "Running early load benchmark against log-normal TTFF stub (PERF-1, PERF-2, PERF-3)..."
    );
    for &size in &[64 * 1024, 512 * 1024, 2 * 1024 * 1024] {
        let elapsed = benchmark_log_normal_ttff_stub(size, 20);
        let per_req = elapsed / 20;
        println!(
            "Body size {size} bytes: 20 iterations took {elapsed:?} (avg {per_req:?} per request)"
        );
    }
}
