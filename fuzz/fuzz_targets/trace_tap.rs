#![no_main]
//! Fuzz target for TraceTap binary block replacement and head/tail truncation.
//!
//! Spec: Section 12.6, Requirements: TRC-2, TRC-3, PERF-2, PERF-5.

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use llmroller_proto::trace_tap::{apply_head_tail_truncation, replace_binary_blocks, TraceTap};

fuzz_target!(|data: &[u8]| {
    if let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(data) {
        replace_binary_blocks(&mut json);
    }
    if let Ok(mut text) = std::str::from_utf8(data).map(str::to_string) {
        let _ = apply_head_tail_truncation(&mut text);
    }
    let (tap, _rx) = TraceTap::new();
    let _ = tap.try_feed_chunk(Bytes::copy_from_slice(data));
});
