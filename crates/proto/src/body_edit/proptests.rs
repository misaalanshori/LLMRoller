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
