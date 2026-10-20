//! Fuzz target for inline SseUsageTap stream parsing.
//!
//! Spec: Section 12.4.1, Requirements: API-5, USG-1, USG-2.

use llmroller_proto::adapters::EndpointKind;
use llmroller_proto::sse_tap::SseUsageTap;

/// Spec: API-5, USG-1, USG-2
/// Fuzz target testing SseUsageTap frame extraction over arbitrary byte streams.
#[doc = "Spec: API-5, USG-1, USG-2"]
pub fn fuzz_sse_tap(data: &[u8]) {
    for endpoint in [EndpointKind::Chat, EndpointKind::Responses, EndpointKind::Messages] {
        let mut tap = SseUsageTap::new(endpoint);
        tap.feed_chunk(data);
        let _ = tap.finalize_usage();
    }
}

fn main() {
    fuzz_sse_tap(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10}}\n\n");
}
