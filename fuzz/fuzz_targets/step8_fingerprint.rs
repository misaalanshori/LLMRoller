#![no_main]
//! Fuzz target for Step 8 canonical fallback fingerprint extraction.
//!
//! Spec: Section 11.1.3, Requirements: SEC-16, RTE-1.

use libfuzzer_sys::fuzz_target;
use llmroller_core::ids::ClientKeyId;
use llmroller_core::session::compute_step8_fingerprint;

fuzz_target!(|data: &[u8]| {
    let client_key_id = ClientKeyId::generate();
    let _ = compute_step8_fingerprint(&client_key_id, "test-canonical-model", 1, data, data);
});
