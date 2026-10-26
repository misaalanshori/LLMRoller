//! Tests for eight-step session resolution, Step 8 fallback fingerprint, and 21-slot detector.
//!
//! Spec: Section 6.7, Section 8.2, Section 11.1, Section 11.1.1, Section 11.2,
//! Requirements: RTE-1, SEC-16.

use super::*;
use std::time::Instant;

#[doc = "Spec: RTE-1, SEC-16"]
#[test]
fn test_eight_step_session_precedence_and_validation() {
    let mut headers = Vec::new();
    let mut body = HashMap::new();

    // Step 8 when no headers/body
    let (id, step) = extract_session_precedence(&headers, &body);
    assert_eq!(id, "");
    assert_eq!(step, SessionPrecedenceStep::Step8Fingerprint);

    // Step 7: body field
    body.insert("session_id".to_string(), "body-sess-1".to_string());
    let (id, step) = extract_session_precedence(&headers, &body);
    assert_eq!(id, "body-sess-1");
    assert_eq!(step, SessionPrecedenceStep::Step7JsonBody);

    // Step 6: generic header overrides body
    headers.push(("x-session-id".to_string(), "generic-sess-2".to_string()));
    let (id, step) = extract_session_precedence(&headers, &body);
    assert_eq!(id, "generic-sess-2");
    assert_eq!(step, SessionPrecedenceStep::Step6GenericHeaders);

    // Step 5: x-session-affinity overrides generic
    headers.push(("x-session-affinity".to_string(), "affinity-3".to_string()));
    let (id, step) = extract_session_precedence(&headers, &body);
    assert_eq!(id, "affinity-3");
    assert_eq!(step, SessionPrecedenceStep::Step5XSessionAffinity);

    // Step 4: x-opencode-session overrides affinity
    headers.push(("x-opencode-session".to_string(), "opencode-4".to_string()));
    let (id, step) = extract_session_precedence(&headers, &body);
    assert_eq!(id, "opencode-4");
    assert_eq!(step, SessionPrecedenceStep::Step4XOpencodeSession);

    // Step 3: x-parent-session-id overrides opencode
    headers.push(("x-parent-session-id".to_string(), "parent-5".to_string()));
    let (id, step) = extract_session_precedence(&headers, &body);
    assert_eq!(id, "parent-5");
    assert_eq!(step, SessionPrecedenceStep::Step3XParentSessionId);

    // Step 2: x-claude-code-session-id overrides parent
    headers.push((
        "x-claude-code-session-id".to_string(),
        "claude-6".to_string(),
    ));
    let (id, step) = extract_session_precedence(&headers, &body);
    assert_eq!(id, "claude-6");
    assert_eq!(step, SessionPrecedenceStep::Step2XClaudeCodeSessionId);

    // Step 1: x-llmroller-session-id overrides claude
    headers.push((
        "x-llmroller-session-id".to_string(),
        "override-7".to_string(),
    ));
    let (id, step) = extract_session_precedence(&headers, &body);
    assert_eq!(id, "override-7");
    assert_eq!(step, SessionPrecedenceStep::Step1XllmrollerSessionId);

    // Validation: invalid characters or > 256 bytes are ignored and fall through
    let invalid_headers = vec![
        (
            "x-llmroller-session-id".to_string(),
            "invalid with \n newline".to_string(),
        ),
        ("x-claude-code-session-id".to_string(), "a".repeat(257)),
        (
            "x-parent-session-id".to_string(),
            "valid-parent".to_string(),
        ),
    ];

    let (id, step) = extract_session_precedence(&invalid_headers, &body);
    assert_eq!(id, "valid-parent");
    assert_eq!(step, SessionPrecedenceStep::Step3XParentSessionId);
}

#[doc = "Spec: RTE-1, SEC-16"]
#[test]
fn test_step8_byte_safe_slicing_and_large_payload_speed() {
    let client_key_id = ClientKeyId::generate();
    let canonical_id = "claude-sonnet-4.6";
    let protocol = 1; // chat

    // Create 5000 bytes with a 3-byte UTF-8 character (e.g. '€' = [0xE2, 0x82, 0xAC])
    // straddling bytes 4095..4098 so &str slicing would panic
    let mut sys_bytes = vec![b'a'; 4095];
    sys_bytes.extend_from_slice(&[0xE2, 0x82, 0xAC]); // 3-byte UTF-8
    sys_bytes.extend_from_slice(&[b'b'; 1000]);

    let usr_bytes = vec![b'c'; 5000];

    // Byte-safe slicing must never panic
    let fp = compute_step8_fingerprint(
        &client_key_id,
        canonical_id,
        protocol,
        &sys_bytes,
        &usr_bytes,
    );
    assert!(fp.starts_with("fp:"));
    assert_eq!(fp.len(), 35); // "fp:" + 32 hex chars

    // 10 MiB base64 payload hashing speed test: must complete in < 50 ms
    let large_image_bytes = vec![b'A'; 10 * 1024 * 1024];
    let start = Instant::now();
    let fp_large = compute_step8_fingerprint(
        &client_key_id,
        canonical_id,
        protocol,
        &sys_bytes,
        &large_image_bytes,
    );
    let duration = start.elapsed();
    assert!(
        duration.as_millis() < 50,
        "Step 8 fingerprint of 10 MiB payload took {} ms (must be < 50 ms)",
        duration.as_millis()
    );
    assert!(fp_large.starts_with("fp:"));
}

#[doc = "Spec: RTE-1, SEC-16"]
#[test]
fn test_nul_separator_and_canonical_content_extractor() {
    let secret = b"01234567890123456789012345678901";
    let user_id = UserId::generate();
    let hash = compute_session_hash(secret, &user_id, "sess-123");
    assert_eq!(hash.len(), 32);

    // Test canonical_content with cache_control and citations stripped
    let block1 = serde_json::json!({
        "type": "text",
        "text": "hello system",
        "cache_control": {"type": "ephemeral"}
    });
    let block2 = serde_json::json!({
        "type": "text",
        "text": "hello system"
    });
    // Two blocks with different cache_control yield identical text emission
    assert_eq!(canonical_content(&block1), canonical_content(&block2));

    let body = serde_json::json!({
        "messages": [
            {"role": "system", "content": "You are a helpful assistant"},
            {"role": "user", "content": [{"type": "text", "text": "What is 2+2?"}]}
        ]
    });
    let (sys, usr) = extract_canonical_sys_and_user1("chat", &body);
    assert_eq!(sys, b"You are a helpful assistant");
    assert_eq!(usr, b"TWhat is 2+2?");
}

#[doc = "Spec: RTE-1, SEC-16"]
#[test]
fn test_21_slot_exact_constant_session_detector() {
    let detector = ConstantSessionDetector::new();
    let key_id = ClientKeyId::generate();
    let session_hash = "0123456789abcdef0123456789abcdef".to_string();

    let t0 = 1_000_000_000_000;

    // 1. Claude Code subagent requests with has_claude_code_agent_id = true never trip detector
    for i in 0..30 {
        let sys = format!("sys prompt {i}");
        let usr = format!("usr prompt {i}");
        let ctx = PromptContext {
            sys: sys.as_bytes(),
            usr: usr.as_bytes(),
            has_claude_code_agent_id: true,
            step: SessionPrecedenceStep::Step6GenericHeaders,
        };
        let flagged =
            detector.record_and_evaluate(key_id, session_hash.clone(), &ctx, t0 + i * 1000);
        assert!(!flagged, "Claude Code subagents must never trip detector");
    }

    // Claude Code session header (Step 2) never trips detector
    for i in 0..30 {
        let sys = format!("sys {i}");
        let usr = format!("usr {i}");
        let ctx = PromptContext {
            sys: sys.as_bytes(),
            usr: usr.as_bytes(),
            has_claude_code_agent_id: false,
            step: SessionPrecedenceStep::Step2XClaudeCodeSessionId,
        };
        let flagged =
            detector.record_and_evaluate(key_id, session_hash.clone(), &ctx, t0 + i * 1000);
        assert!(!flagged, "Step 2 headers must never trip detector");
    }

    // 2. 21 distinct openings on generic header (Step 6) within 59 minutes:
    let generic_key = ClientKeyId::generate();
    let generic_hash = "abcdef0123456789abcdef0123456789".to_string();

    for i in 0..20 {
        let sys = format!("distinct sys {i}");
        let usr = format!("distinct usr {i}");
        let ctx = PromptContext {
            sys: sys.as_bytes(),
            usr: usr.as_bytes(),
            has_claude_code_agent_id: false,
            step: SessionPrecedenceStep::Step6GenericHeaders,
        };
        let flagged = detector.record_and_evaluate(
            generic_key,
            generic_hash.clone(),
            &ctx,
            t0 + i * 60_000, // 1 min apart (0 to 19 min)
        );
        assert!(!flagged, "not flagged before slot 21");
    }

    // 21st distinct opening at 58 minutes (< 60 minutes) flags!
    let ctx_21 = PromptContext {
        sys: b"distinct sys 20",
        usr: b"distinct usr 20",
        has_claude_code_agent_id: false,
        step: SessionPrecedenceStep::Step6GenericHeaders,
    };
    let flagged_21 =
        detector.record_and_evaluate(generic_key, generic_hash.clone(), &ctx_21, t0 + 58 * 60_000);
    assert!(flagged_21, "slot 21 within 59 minutes must trigger flag");
    assert!(detector.is_flagged(generic_key, &generic_hash));

    // 3. 21 openings spread over 61 minutes do NOT flag (slots older than 60m are dropped)
    let slow_key = ClientKeyId::generate();
    let slow_hash = "11112222333344445555666677778888".to_string();

    for i in 0..21 {
        let sys = format!("slow sys {i}");
        let usr = format!("slow usr {i}");
        let ctx = PromptContext {
            sys: sys.as_bytes(),
            usr: usr.as_bytes(),
            has_claude_code_agent_id: false,
            step: SessionPrecedenceStep::Step6GenericHeaders,
        };
        let flagged = detector.record_and_evaluate(
            slow_key,
            slow_hash.clone(),
            &ctx,
            t0 + i * 3 * 60_000, // 3 mins apart (spread over 60+ minutes)
        );
        assert!(!flagged, "openings spread over 61 minutes must not flag");
    }
    assert!(!detector.is_flagged(slow_key, &slow_hash));
}

#[doc = "Spec: RTE-1, SEC-16"]
#[test]
fn test_session_id_256_char_boundary_and_recursive_canonical_block_stripping() {
    // F-009: 200-char and 256-char session IDs are valid; 257-char or space-containing IDs are rejected
    let id_200 = "s".repeat(200);
    let id_256 = "a".repeat(256);
    let id_257 = "a".repeat(257);
    assert!(is_valid_session_id(&id_200));
    assert!(is_valid_session_id(&id_256));
    assert!(!is_valid_session_id(&id_257));
    assert!(!is_valid_session_id("has space"));

    // F-020: nested cache_control, prompt_cache_breakpoint, and citations inside tool_result content blocks
    let tool_res_with_nested_cache = serde_json::json!({
        "type": "tool_result",
        "tool_use_id": "toolu_01",
        "content": [
            {
                "type": "text",
                "text": "file contents",
                "cache_control": {"type": "ephemeral"},
                "prompt_cache_breakpoint": true,
                "citations": [{"type": "char_location"}]
            }
        ]
    });
    let tool_res_clean = serde_json::json!({
        "type": "tool_result",
        "tool_use_id": "toolu_01",
        "content": [
            {
                "type": "text",
                "text": "file contents"
            }
        ]
    });
    assert_eq!(
        canonical_content(&tool_res_with_nested_cache),
        canonical_content(&tool_res_clean)
    );
}

// Spec: ./docs/srdd/11. SRDD - Session Identification and Hints.md#1111-inbound-session-precedence | Phase: ./docs/bigbang/big-bang-phases-3.md#33-sub-phase-33-inbound-session-precedence-step-8-fingerprint-outbound-affinity-and-21-slot-detector
#[doc = "Spec: RTE-1, RTE-2, SEC-16"]
#[test]
fn test_observe_and_check_persists_flagged_state_across_subsequent_calls() {
    let detector = ConstantSessionDetector::new();
    let key_id = ClientKeyId::generate();
    let session_hash = "sess_generic_const".to_string();
    let now_ms = 1_776_000_000_000_i64;

    for i in 0..20 {
        let usr = format!("distinct prompt {i}");
        let ctx = PromptContext {
            sys: b"system",
            usr: usr.as_bytes(),
            has_claude_code_agent_id: false,
            step: SessionPrecedenceStep::Step6GenericHeaders,
        };
        let (is_flagged, newly_flagged) =
            detector.observe_and_check(key_id, session_hash.clone(), &ctx, now_ms);
        assert!(!is_flagged);
        assert!(!newly_flagged);
    }

    let ctx_21 = PromptContext {
        sys: b"system",
        usr: b"distinct prompt 20",
        has_claude_code_agent_id: false,
        step: SessionPrecedenceStep::Step6GenericHeaders,
    };
    let (is_flagged_21, newly_flagged_21) =
        detector.observe_and_check(key_id, session_hash.clone(), &ctx_21, now_ms);
    assert!(is_flagged_21);
    assert!(newly_flagged_21);

    // 22nd call remains flagged while newly_flagged is false
    let ctx_22 = PromptContext {
        sys: b"system",
        usr: b"distinct prompt 21",
        has_claude_code_agent_id: false,
        step: SessionPrecedenceStep::Step6GenericHeaders,
    };
    let (is_flagged_22, newly_flagged_22) =
        detector.observe_and_check(key_id, session_hash, &ctx_22, now_ms + 1_000);
    assert!(is_flagged_22);
    assert!(!newly_flagged_22);
}
