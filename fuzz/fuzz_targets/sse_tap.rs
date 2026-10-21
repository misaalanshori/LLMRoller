#![no_main]
//! Fuzz target for inline SseUsageTap stream parsing.
//!
//! Spec: Section 12.4.1, Requirements: API-5, USG-1, USG-2.

use libfuzzer_sys::fuzz_target;
use llmroller_proto::adapters::EndpointKind;
use llmroller_proto::sse_tap::SseUsageTap;

fuzz_target!(|data: &[u8]| {
    for endpoint in [EndpointKind::Chat, EndpointKind::Responses, EndpointKind::Messages] {
        let mut tap = SseUsageTap::new(endpoint);
        tap.feed_chunk(data);
        let _ = tap.finalize_usage();
    }
});
