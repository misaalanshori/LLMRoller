#![no_main]
//! Fuzz target for HTTP header validation and smuggling checks.
//!
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-request-and-response-header-policies
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = llmroller_proto::headers::validate_header_value_bytes("x-fuzz", data);
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = llmroller_proto::headers::scrub_canary_tokens(s, &["canary"]);
        let _ = llmroller_proto::headers::sanitize_csv_cell(s);
    }
});
