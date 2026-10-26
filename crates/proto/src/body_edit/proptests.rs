//! Property-based tests for surgical JSON span editor, envelope validation, and duplicate-key rejection.
//!
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#73-transparency-and-compatibility-trn--com
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#123-surgical-json-body-validation-and-edits
//! Phase: ./docs/bigbang/big-bang-phases-2.md#24-sub-phase-24-zero-copy-surgical-json-span-editor-duplicate-key-guard-and-body-edit-fuzzer

use bytes::Bytes;
use llmroller_core::rbac::PoolTier;
use proptest::prelude::*;

use crate::body_edit::{BodyEditError, ChatStreamMode, edit_body, scan_top_level};

/// Spec: API-4, TRN-1, TRN-2
/// Envelope validation: Non-UTF8, BOM, non-object, and trailing bytes are rejected with 400.
#[doc = "Spec: API-4, TRN-1, TRN-2"]
#[test]
fn test_envelope_validation() {
    // 1. UTF-8 BOM
    let bom_body = b"\xEF\xBB\xBF{\"model\":\"gpt-4o\"}";
    let err = scan_top_level(bom_body).expect_err("BOM must be rejected");
    assert!(matches!(err, BodyEditError::BomNotAllowed));

    // 2. Non-object top-level (array, string, number)
    let array_body = b"[\"model\", \"gpt-4o\"]";
    let err = scan_top_level(array_body).expect_err("array top-level must be rejected");
    assert!(matches!(err, BodyEditError::NotAnObject));

    let string_body = b"\"just a string\"";
    let err = scan_top_level(string_body).expect_err("string top-level must be rejected");
    assert!(matches!(err, BodyEditError::NotAnObject));

    // 3. Trailing non-whitespace bytes
    let trailing_body = b"{\"model\":\"gpt-4o\"} trailing";
    let err = scan_top_level(trailing_body).expect_err("trailing bytes must be rejected");
    assert!(matches!(err, BodyEditError::TrailingBytes));

    // 4. Whitespace before and after object is allowed
    let valid_ws = b"   {\"model\":\"gpt-4o\"}   \n  ";
    assert!(scan_top_level(valid_ws).is_ok());
}

/// Spec: API-4, TRN-1, TRN-2
/// Duplicate top-level keys must be rejected with HTTP 400 invalid_request_error.
#[doc = "Spec: API-4, TRN-1, TRN-2"]
#[test]
fn test_duplicate_key_rejection() {
    // Duplicate "model"
    let dup_model = b"{\"model\":\"cheap\",\"stream\":false,\"model\":\"expensive\"}";
    let err = scan_top_level(dup_model).expect_err("duplicate model key must be rejected");
    assert!(matches!(err, BodyEditError::DuplicateKey(_)));

    // Duplicate "stream"
    let dup_stream = b"{\"model\":\"claude\",\"stream\":false,\"stream\":true}";
    let err = scan_top_level(dup_stream).expect_err("duplicate stream key must be rejected");
    assert!(matches!(err, BodyEditError::DuplicateKey(_)));

    // Duplicate key with JSON escape representation (\u006d\u006f\u0064\u0065\u006c == model)
    let dup_escaped =
        b"{\"model\":\"cheap\",\"\\u006d\\u006f\\u0064\\u0065\\u006c\":\"expensive\"}";
    let err = scan_top_level(dup_escaped).expect_err("escaped duplicate key must be rejected");
    assert!(matches!(err, BodyEditError::DuplicateKey(_)));
}

/// Spec: API-4, TRN-1, TRN-2
/// Type validation of top-level routing fields:
/// - model must be string
/// - stream must be boolean
/// - previous_response_id must be string
#[doc = "Spec: API-4, TRN-1, TRN-2"]
#[test]
fn test_top_level_type_validation() {
    let num_model = b"{\"model\":12345}";
    let err = scan_top_level(num_model).expect_err("numeric model must be rejected");
    assert!(matches!(err, BodyEditError::InvalidType(_)));

    let str_stream = b"{\"model\":\"gpt-4o\",\"stream\":\"true\"}";
    let err = scan_top_level(str_stream).expect_err("string stream must be rejected");
    assert!(matches!(err, BodyEditError::InvalidType(_)));

    let num_prev_id = b"{\"model\":\"gpt-4o\",\"previous_response_id\":42}";
    let err = scan_top_level(num_prev_id).expect_err("numeric prev_id must be rejected");
    assert!(matches!(err, BodyEditError::InvalidType(_)));
}

/// Spec: API-4, TRN-1, TRN-2
/// TRN-1: When routing on Own tier with no edits needed, the output Bytes pointer is identical to the input Bytes.
#[doc = "Spec: API-4, TRN-1, TRN-2"]
#[test]
fn test_trn1_pointer_identity_when_unmodified() {
    let raw = Bytes::from_static(
        b"{\"model\":\"claude-sonnet-4.6\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}",
    );

    let (out, strip) = edit_body(
        raw.clone(),
        PoolTier::Own,
        None, // no model rewrite
        ChatStreamMode::Native,
        None, // no session field injection
    )
    .expect("edit body");

    assert!(!strip);
    assert_eq!(out.as_ptr(), raw.as_ptr());
}

/// Spec: API-4, TRN-1, TRN-2
/// TRN-2: Model rewrite preserves exact byte sequence and key order of surrounding fields,
/// properly escaping arbitrary model strings.
#[doc = "Spec: API-4, TRN-1, TRN-2"]
#[test]
fn test_trn2_model_rewrite_byte_preservation() {
    let raw = Bytes::from_static(b"{\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}],\"model\":\"original-model\",\"tools\":[]}");

    let target_model = "provider/custom-model:special\"with\\escapes";
    let (out, _) = edit_body(
        raw,
        PoolTier::Own,
        Some(target_model),
        ChatStreamMode::Native,
        None,
    )
    .expect("edit body");

    let out_str = std::str::from_utf8(&out).expect("utf8");
    // Verify surrounding messages and tools are byte-identical
    assert!(out_str.contains("\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]"));
    assert!(out_str.contains("\"tools\":[]"));
    // Verify target model is JSON-escaped
    assert!(out_str.contains("\"provider/custom-model:special\\\"with\\\\escapes\""));

    // Verify resulting JSON parses cleanly
    let parsed: serde_json::Value = serde_json::from_str(out_str).expect("valid json");
    assert_eq!(parsed["model"], target_model);
}

/// Spec: API-4, TRN-1, TRN-2
/// Stream options handling across all six cases in Section 12.3 table.
#[doc = "Spec: API-4, TRN-1, TRN-2"]
#[test]
fn test_stream_options_chat_stream_injection_cases() {
    // Case 1: stream absent -> no edit, strip = false
    let c1 = Bytes::from_static(b"{\"model\":\"gpt-4o\"}");
    let (out1, strip1) = edit_body(
        c1.clone(),
        PoolTier::Own,
        None,
        ChatStreamMode::Inject,
        None,
    )
    .expect("c1 edit");
    assert!(!strip1);
    assert_eq!(out1.as_ptr(), c1.as_ptr());

    // Case 2: stream: true, no stream_options present -> insert "stream_options":{"include_usage":true} after {, strip = true
    let c2 = Bytes::from_static(b"{\"stream\":true,\"model\":\"gpt-4o\"}");
    let (out2, strip2) =
        edit_body(c2, PoolTier::Own, None, ChatStreamMode::Inject, None).expect("c2 edit");
    assert!(strip2);
    let v2: serde_json::Value = serde_json::from_slice(&out2).expect("c2 parse");
    assert_eq!(v2["stream_options"]["include_usage"], true);

    // Case 3: stream_options is null -> replace null with {"include_usage":true}, strip = true
    let c3 = Bytes::from_static(b"{\"model\":\"gpt-4o\",\"stream\":true,\"stream_options\":null}");
    let (out3, strip3) =
        edit_body(c3, PoolTier::Own, None, ChatStreamMode::Inject, None).expect("c3 edit");
    assert!(strip3);
    let v3: serde_json::Value = serde_json::from_slice(&out3).expect("c3 parse");
    assert_eq!(v3["stream_options"]["include_usage"], true);

    // Case 4: stream_options is object without include_usage -> insert include_usage: true, strip = true
    let c4 = Bytes::from_static(
        b"{\"model\":\"gpt-4o\",\"stream\":true,\"stream_options\":{\"extra\":1}}",
    );
    let (out4, strip4) =
        edit_body(c4, PoolTier::Own, None, ChatStreamMode::Inject, None).expect("c4 edit");
    assert!(strip4);
    let v4: serde_json::Value = serde_json::from_slice(&out4).expect("c4 parse");
    assert_eq!(v4["stream_options"]["include_usage"], true);
    assert_eq!(v4["stream_options"]["extra"], 1);

    // Case 5: stream_options.include_usage is false -> replace false with true, strip = true
    let c5 = Bytes::from_static(
        b"{\"model\":\"gpt-4o\",\"stream\":true,\"stream_options\":{\"include_usage\":false}}",
    );
    let (out5, strip5) =
        edit_body(c5, PoolTier::Own, None, ChatStreamMode::Inject, None).expect("c5 edit");
    assert!(strip5);
    let v5: serde_json::Value = serde_json::from_slice(&out5).expect("c5 parse");
    assert_eq!(v5["stream_options"]["include_usage"], true);

    // Case 6: stream_options.include_usage is already true -> no edit, strip = false
    let c6 = Bytes::from_static(
        b"{\"model\":\"gpt-4o\",\"stream\":true,\"stream_options\":{\"include_usage\":true}}",
    );
    let (out6, strip6) = edit_body(
        c6.clone(),
        PoolTier::Own,
        None,
        ChatStreamMode::Inject,
        None,
    )
    .expect("c6 edit");
    assert!(!strip6);
    assert_eq!(out6.as_ptr(), c6.as_ptr());
}

#[doc = "Spec: API-4, TRN-2, SEC-16"]
#[test]
fn test_edit_body_strips_session_keys_without_comma_corruption() {
    // 1. Session id at start
    let raw1 = Bytes::from_static(br#"{"session_id":"s1","model":"gpt-4o"}"#);
    let (out1, _) = edit_body(
        raw1,
        PoolTier::Global,
        Some("gpt-4o"),
        ChatStreamMode::Native,
        None,
    )
    .expect("edit");
    let parsed1: serde_json::Value =
        serde_json::from_slice(&out1).expect("valid json when session_id at start");
    assert_eq!(parsed1["model"], "gpt-4o");
    assert!(parsed1.get("session_id").is_none());

    // 2. Session id in middle
    let raw2 = Bytes::from_static(br#"{"a":1,"session_id":"s1","model":"gpt-4o"}"#);
    let (out2, _) = edit_body(
        raw2,
        PoolTier::Global,
        Some("gpt-4o"),
        ChatStreamMode::Native,
        None,
    )
    .expect("edit");
    let parsed2: serde_json::Value =
        serde_json::from_slice(&out2).expect("valid json when session_id in middle");
    assert_eq!(parsed2["a"], 1);
    assert_eq!(parsed2["model"], "gpt-4o");
    assert!(parsed2.get("session_id").is_none());

    // 3. Session id at end
    let raw3 = Bytes::from_static(br#"{"model":"gpt-4o","session_id":"s1"}"#);
    let (out3, _) = edit_body(
        raw3,
        PoolTier::Global,
        Some("gpt-4o"),
        ChatStreamMode::Native,
        None,
    )
    .expect("edit");
    let parsed3: serde_json::Value =
        serde_json::from_slice(&out3).expect("valid json when session_id at end");
    assert_eq!(parsed3["model"], "gpt-4o");
    assert!(parsed3.get("session_id").is_none());

    // 4. Session id as sole key
    let raw4 = Bytes::from_static(br#"{"session_id":"s1"}"#);
    let (out4, _) =
        edit_body(raw4, PoolTier::Global, None, ChatStreamMode::Native, None).expect("edit");
    let parsed4: serde_json::Value =
        serde_json::from_slice(&out4).expect("valid json when session_id sole key");
    assert!(parsed4.get("session_id").is_none());

    // 5. Metadata containing session_id and custom field
    let raw5 = Bytes::from_static(
        br#"{"metadata":{"session_id":"s1","user_id":"u1","custom":"keep_me"},"model":"gpt-4o"}"#,
    );
    let (out5, _) = edit_body(
        raw5,
        PoolTier::Shared,
        Some("gpt-4o"),
        ChatStreamMode::Native,
        None,
    )
    .expect("edit");
    let parsed5: serde_json::Value =
        serde_json::from_slice(&out5).expect("valid json when metadata stripped");
    assert_eq!(parsed5["model"], "gpt-4o");
    assert_eq!(parsed5["metadata"]["custom"], "keep_me");
    assert!(parsed5["metadata"].get("session_id").is_none());
    assert!(parsed5["metadata"].get("user_id").is_none());
}

proptest! {
    /// Spec: API-4, TRN-1, TRN-2
    /// Property: Random models containing quotes and backslashes always parse back identically.
    #[test]
    fn prop_random_model_escaping(model_name in "[-a-zA-Z0-9_./\":\\\\]{1,100}") {
        let raw = Bytes::from_static(b"{\"messages\":[],\"model\":\"old\"}");
        let (out, _) = edit_body(
            raw,
            PoolTier::Own,
            Some(&model_name),
            ChatStreamMode::Native,
            None,
        ).expect("edit body");

        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("must parse");
        prop_assert_eq!(parsed["model"].as_str(), Some(model_name.as_str()));
    }
}

#[doc = "Spec: API-4, API-5, TRN-2, SEC-16"]
#[test]
fn test_stream_options_sibling_fields_and_session_override_edge_cases() {
    // F-007: numeric/boolean sibling fields before include_usage inside stream_options
    let raw_sibling = Bytes::from_static(
        br#"{"model":"gpt-4o","stream":true,"stream_options":{"extra":123,"include_obfuscation":true,"include_usage":false}}"#,
    );
    let (out_sib, strip_sib) = edit_body(
        raw_sibling,
        PoolTier::Own,
        None,
        ChatStreamMode::Inject,
        None,
    )
    .expect("sibling stream_options must parse and replace false with true");
    assert!(strip_sib);
    let parsed_sib: serde_json::Value = serde_json::from_slice(&out_sib).expect("valid json");
    assert_eq!(parsed_sib["stream_options"]["extra"], 123);
    assert_eq!(parsed_sib["stream_options"]["include_obfuscation"], true);
    assert_eq!(parsed_sib["stream_options"]["include_usage"], true);

    // F-007: non-boolean include_usage and duplicate keys inside stream_options are rejected
    let bad_usage_type =
        br#"{"model":"gpt-4o","stream":true,"stream_options":{"include_usage":"yes"}}"#;
    assert!(matches!(
        scan_top_level(bad_usage_type).expect_err("non-bool include_usage must fail"),
        BodyEditError::InvalidType(_)
    ));
    let dup_stream_opt =
        br#"{"model":"gpt-4o","stream":true,"stream_options":{"include_usage":false,"include_usage":true}}"#;
    assert!(matches!(
        scan_top_level(dup_stream_opt).expect_err("duplicate stream_options key must fail"),
        BodyEditError::DuplicateKey(_)
    ));

    // F-008: custom session_field on Shared/Global strips any matching top-level key before inserting
    let raw_custom = Bytes::from_static(br#"{"custom_session":"client-1","model":"gpt-4o"}"#);
    let (out_custom, _) = edit_body(
        raw_custom,
        PoolTier::Shared,
        None,
        ChatStreamMode::Native,
        Some(("custom_session", "llr_salted123")),
    )
    .expect("custom session field edit");
    let parsed_custom: serde_json::Value = serde_json::from_slice(&out_custom).expect("valid json");
    assert_eq!(parsed_custom["custom_session"], "llr_salted123");
    assert!(scan_top_level(&out_custom).is_ok());

    // F-016: on Own tier, a higher-precedence header session ID overrides a conflicting body session_id
    let raw_own = Bytes::from_static(br#"{"session_id":"stale-body","model":"gpt-4o"}"#);
    let (out_own, _) = edit_body(
        raw_own,
        PoolTier::Own,
        None,
        ChatStreamMode::Native,
        Some(("session_id", "header-override")),
    )
    .expect("own tier header override");
    let parsed_own: serde_json::Value = serde_json::from_slice(&out_own).expect("valid json");
    assert_eq!(parsed_own["session_id"], "header-override");
    assert!(scan_top_level(&out_own).is_ok());
}

#[doc = "Spec: F005, F007, F024 - responses failover sanitization, OpenAI completions compat, and wildcard/null-promoting body edits"]
#[test]
fn test_responses_failover_sanitization_openai_compat_and_wildcard_body_edits() {
    use crate::body_edit::{
        apply_body_edit_rules, apply_openai_completions_compat, sanitize_responses_failover_body,
    };
    use llmroller_core::provider::{BodyEditOp, BodyEditRule, MaxTokensField, OpenAiCompatConfig};

    // F005: sanitize_responses_failover_body strips previous_response_id, encrypted_content, and item_reference
    let raw_resp = Bytes::from_static(br#"{
        "model": "gpt-5.4",
        "previous_response_id": "resp_acc_a_123",
        "input": [
            {"type": "item_reference", "id": "rs_secret_1"},
            {"type": "reasoning", "encrypted_content": "gAAAAAB...", "summary": []},
            {"type": "reasoning", "encrypted_content": "gAAAAAB...", "summary": [{"type": "summary_text", "text": "thought"}]},
            {"role": "user", "content": "hello"}
        ]
    }"#);
    let sanitized = sanitize_responses_failover_body(raw_resp, true);
    let val: serde_json::Value = serde_json::from_slice(&sanitized).expect("valid json");
    assert!(val.get("previous_response_id").is_none());
    let items = val["input"].as_array().expect("input array");
    assert_eq!(items.len(), 2);
    assert!(items[0].get("encrypted_content").is_none());
    assert_eq!(items[1]["role"], "user");

    // F007: OpenAiCompatConfig rewrites developer->system, max_completion_tokens->max_tokens, and 9-char Mistral tool IDs
    let mut chat_val = serde_json::json!({
        "model": "mistral-large-latest",
        "max_completion_tokens": 512,
        "store": true,
        "reasoning_effort": "high",
        "messages": [
            {"role": "developer", "content": "Be concise"},
            {"role": "assistant", "tool_calls": [{"id": "call_very_long_openai_id_12345", "type": "function", "function": {"name": "fn1", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "call_very_long_openai_id_12345", "content": "ok"}
        ]
    });
    let compat = OpenAiCompatConfig {
        rewrite_developer_role_to_system: true,
        max_tokens_field: Some(MaxTokensField::MaxTokens),
        strip_store: true,
        strip_reasoning_effort: true,
        normalize_mistral_tool_ids: true,
    };
    assert!(apply_openai_completions_compat(&mut chat_val, &compat));
    assert_eq!(chat_val["max_tokens"], 512);
    assert!(chat_val.get("max_completion_tokens").is_none());
    assert!(chat_val.get("store").is_none());
    assert!(chat_val.get("reasoning_effort").is_none());
    assert_eq!(chat_val["messages"][0]["role"], "system");
    let norm_id = chat_val["messages"][1]["tool_calls"][0]["id"]
        .as_str()
        .expect("id");
    assert_eq!(norm_id.len(), 9);
    assert_eq!(
        chat_val["messages"][2]["tool_call_id"].as_str(),
        Some(norm_id)
    );

    // F024: apply_body_edit_rules promotes intermediate Null to Object and supports * array wildcards
    let mut body_val = serde_json::json!({
        "reasoning": null,
        "messages": [
            {"role": "user", "content": "a"},
            {"role": "user", "content": "b"}
        ]
    });
    let rules = vec![
        BodyEditRule {
            op: BodyEditOp::Set,
            path: "reasoning.effort".to_string(),
            value: Some(serde_json::json!("high")),
            from: None,
        },
        BodyEditRule {
            op: BodyEditOp::Default,
            path: "messages.*.cache_control".to_string(),
            value: Some(serde_json::json!({"type": "ephemeral"})),
            from: None,
        },
    ];
    assert!(apply_body_edit_rules(&mut body_val, &rules));
    assert_eq!(body_val["reasoning"]["effort"], "high");
    assert_eq!(
        body_val["messages"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        body_val["messages"][1]["cache_control"]["type"],
        "ephemeral"
    );
}

// Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#123-surgical-json-body-validation-and-edits | Phase: ./docs/bigbang/big-bang-phases-2.md#24-sub-phase-24-zero-copy-surgical-json-span-editor-duplicate-key-guard-and-body-edit-fuzzer
#[test]
#[doc = "Spec: API-1, TRN-1 - extract model and stream flag from Gemini URL path, Bedrock modelId, and Cloud Code Assist request envelope"]
fn test_scan_top_level_with_fallback_extracts_gemini_path_and_nested_models() {
    let gemini_body = br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
    let scan = scan_top_level_with_fallback(
        gemini_body,
        Some("/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"),
    )
    .expect("scan gemini");
    assert_eq!(
        scan.model.as_ref().map(|(m, _)| m.as_str()),
        Some("gemini-2.5-pro")
    );
    assert_eq!(scan.stream.map(|(s, _)| s), Some(true));

    let cca_body = br#"{"project":"p1","request":{"model":"gemini-2.5-flash","stream":true,"contents":[]}}"#;
    let scan_cca = scan_top_level_with_fallback(cca_body, None).expect("scan cca");
    assert_eq!(
        scan_cca.model.as_ref().map(|(m, _)| m.as_str()),
        Some("gemini-2.5-flash")
    );
    assert_eq!(scan_cca.stream.map(|(s, _)| s), Some(true));

    let bedrock_body = br#"{"modelId":"anthropic.claude-sonnet-4-6-v1:0","messages":[]}"#;
    let scan_bed =
        scan_top_level_with_fallback(bedrock_body, Some("/model/anthropic.claude-sonnet-4-6-v1:0/converse-stream"))
            .expect("scan bedrock");
    assert_eq!(
        scan_bed.model.as_ref().map(|(m, _)| m.as_str()),
        Some("anthropic.claude-sonnet-4-6-v1:0")
    );
    assert_eq!(scan_bed.stream.map(|(s, _)| s), Some(true));
}
