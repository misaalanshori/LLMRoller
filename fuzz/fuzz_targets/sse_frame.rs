//! Fuzz target for SSE frame parsing, delimiter finding, and usage chunk detection.
//!
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#1241-frame-filter-inline-sseusagetap-and-bounded-tracetap-execution-model
//! Phase: ./docs/bigbang/big-bang-phases-2.md#25-sub-phase-25-unbuffered-sse-forwarder-64-kib-pre-first-frame-holding-buffer-single-frame-filter-and-requestlifecycle-guard

/// Spec: API-7
/// Fuzz target testing sse_frame functions over arbitrary byte streams.
#[doc = "Spec: API-7"]
pub fn fuzz_sse_frame(data: &[u8]) {
    let _ = llmroller_proto::sse_frame::find_frame_delimiter(data);
    let _ = llmroller_proto::sse_frame::is_comment_frame(data);
    let _ = llmroller_proto::sse_frame::is_injected_usage_frame(data);
    let _ = llmroller_proto::sse_frame::extract_frame_error(data);
}

fn main() {
    fuzz_sse_frame(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10}}\n\n");
}
