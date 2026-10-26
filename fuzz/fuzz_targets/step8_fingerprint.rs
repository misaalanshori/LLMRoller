#![no_main]
//! Fuzz target for Step 8 canonical fallback fingerprint extraction and session resolution.
//!
//! Spec: Section 11.1.3, Requirements: SEC-16, RTE-1, RTE-2.

use std::collections::HashMap;
use libfuzzer_sys::fuzz_target;
use llmroller_core::ids::ClientKeyId;
use llmroller_core::session::{
    ConstantSessionDetector, PromptContext, compute_step8_fingerprint,
    extract_canonical_sys_and_user1, extract_session_precedence,
};

fuzz_target!(|data: &[u8]| {
    let client_key_id = ClientKeyId::generate();
    let _ = compute_step8_fingerprint(&client_key_id, "test-canonical-model", 1, data, data);

    if let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(data) {
        for protocol in ["chat", "responses", "messages"] {
            let (sys, usr) = extract_canonical_sys_and_user1(protocol, &parsed);
            let _ = compute_step8_fingerprint(&client_key_id, "test-canonical-model", 1, &sys, &usr);

            let mut body_fields = HashMap::new();
            if let Some(sid) = parsed.get("session_id").and_then(|v| v.as_str()) {
                body_fields.insert("session_id".to_string(), sid.to_string());
            }
            let headers = vec![("x-session-id".to_string(), "fuzz-session".to_string())];
            let (raw_session, step) = extract_session_precedence(&headers, &body_fields);
            if !raw_session.is_empty() && step.is_generic() {
                let detector = ConstantSessionDetector::new();
                let _ = detector.observe_and_check(
                    client_key_id,
                    raw_session,
                    &PromptContext {
                        sys: &sys,
                        usr: &usr,
                        has_claude_code_agent_id: false,
                        step,
                    },
                    1_000,
                );
            }
        }
    }
});
