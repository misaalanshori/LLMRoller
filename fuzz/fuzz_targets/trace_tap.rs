//! Fuzz target for TraceTap binary block replacement and head/tail truncation.
//!
//! Spec: Section 12.6, Requirements: TRC-2, TRC-3, PERF-2, PERF-5.

use bytes::Bytes;
use llmroller_proto::trace_tap::{apply_head_tail_truncation, replace_binary_blocks, TraceTap};

/// Spec: TRC-2, TRC-3, PERF-2, PERF-5
/// Fuzz target testing binary block replacement and head/tail truncation over arbitrary byte streams.
#[doc = "Spec: TRC-2, TRC-3, PERF-2, PERF-5"]
pub fn fuzz_trace_tap(data: &[u8]) {
    if let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(data) {
        replace_binary_blocks(&mut json);
    }
    if let Ok(mut text) = std::str::from_utf8(data).map(str::to_string) {
        let _ = apply_head_tail_truncation(&mut text);
    }
    let (tap, _rx) = TraceTap::new();
    let _ = tap.try_feed_chunk(Bytes::copy_from_slice(data));
}

fn main() {
    fuzz_trace_tap(b"{\"data\":\"large_base64_payload\"}");
}
