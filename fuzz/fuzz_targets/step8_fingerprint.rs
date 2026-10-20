//! Fuzz target for Step 8 canonical fallback fingerprint extraction.
//!
//! Spec: Section 11.1.3, Requirements: SEC-16, RTE-1.

use llmroller_core::ids::ClientKeyId;
use llmroller_core::session::compute_step8_fingerprint;

/// Spec: RTE-1, SEC-16
/// Fuzz target testing Step 8 byte-sliced fallback fingerprint over arbitrary byte streams.
#[doc = "Spec: RTE-1, SEC-16"]
pub fn fuzz_step8_fingerprint(data: &[u8]) {
    let client_key_id = ClientKeyId::generate();
    let _ = compute_step8_fingerprint(&client_key_id, "test-canonical-model", 1, data, data);
}

fn main() {
    fuzz_step8_fingerprint(b"{\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}");
}
