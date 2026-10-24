//! Tests for block-hash LCP timeline reconstruction, cursor pagination, and XSS sanitization.
//!
//! Spec: Section 12.6, Requirements: TRC-5, SEC-19.

use super::*;

#[doc = "Spec: TRC-5"]
#[test]
fn test_block_hash_lcp_computation() {
    let refs1 = vec!["b1".to_string(), "b2".to_string()];
    let refs2 = vec!["b1".to_string(), "b2".to_string(), "b3".to_string()];
    let refs3 = vec!["b1".to_string(), "b4".to_string()];

    assert_eq!(compute_lcp(&refs1, &refs2), 2);
    assert_eq!(compute_lcp(&refs2, &refs3), 1);
    assert_eq!(compute_lcp(&[], &refs1), 0);
}

#[doc = "Spec: TRC-5"]
#[test]
fn test_cursor_pagination_parsing_and_formatting() {
    let cursor = format_cursor(1_700_000_123_000, "req-uuid-456");
    assert_eq!(cursor, "1700000123000:req-uuid-456");

    let parsed = parse_cursor(&cursor).expect("cursor must parse");
    assert_eq!(parsed.0, 1_700_000_123_000);
    assert_eq!(parsed.1, "req-uuid-456");

    assert!(parse_cursor("invalid_cursor_format").is_none());
}

#[doc = "Spec: TRC-5"]
#[test]
fn test_timeline_reconstruction_with_lcp_and_decryption_cap() {
    let b1 = "hash_b1".to_string();
    let b2 = "hash_b2".to_string();
    let b3 = "hash_b3".to_string();

    let turn1_refs = [b1.clone(), b2.clone()];
    let turn1 = PreviewTurn {
        request_id: "req-1".to_string(),
        ts: 1000,
        canonical_model: "gpt-4o".to_string(),
        protocol: "chat".to_string(),
        messages: vec![
            PreviewMessage {
                role: "system".to_string(),
                content: "sys prompt".to_string(),
                block_hash: b1.clone(),
                is_new: false,
            },
            PreviewMessage {
                role: "user".to_string(),
                content: "user question 1".to_string(),
                block_hash: b2.clone(),
                is_new: false,
            },
        ],
        assistant_text: "assistant reply 1".to_string(),
        assistant_reasoning: None,
        assistant_tool_calls: vec![],
        has_load_more_placeholder: false,
        context_compacted_or_branched: false,
    };

    let turn2_refs = [b1.clone(), b2.clone(), b3.clone()];
    let turn2 = PreviewTurn {
        request_id: "req-2".to_string(),
        ts: 2000,
        canonical_model: "gpt-4o".to_string(),
        protocol: "chat".to_string(),
        messages: vec![
            PreviewMessage {
                role: "system".to_string(),
                content: "sys prompt".to_string(),
                block_hash: b1,
                is_new: false,
            },
            PreviewMessage {
                role: "user".to_string(),
                content: "user question 1".to_string(),
                block_hash: b2,
                is_new: false,
            },
            PreviewMessage {
                role: "user".to_string(),
                content: "user follow up 2".to_string(),
                block_hash: b3,
                is_new: false,
            },
        ],
        assistant_text: "assistant reply 2".to_string(),
        assistant_reasoning: None,
        assistant_tool_calls: vec![],
        has_load_more_placeholder: false,
        context_compacted_or_branched: false,
    };

    let input = [
        (&turn1_refs[..], turn1.clone()),
        (&turn2_refs[..], turn2.clone()),
    ];

    let timeline = reconstruct_timeline(&input, 10_000);
    assert_eq!(timeline.len(), 2);

    // Turn 1: both messages are new, not compacted or branched
    assert!(timeline[0].messages[0].is_new);
    assert!(timeline[0].messages[1].is_new);
    assert!(!timeline[0].context_compacted_or_branched);

    // Turn 2: LCP is 2! Messages 0 and 1 are repeated prefix, message 2 is new!
    assert!(!timeline[1].messages[0].is_new);
    assert!(!timeline[1].messages[1].is_new);
    assert!(timeline[1].messages[2].is_new);
    assert!(!timeline[1].context_compacted_or_branched);

    // Turn 3: Compacted/branched context (e.g. prefix shrinks or branches)
    let b_comp = "hash_b_comp".to_string();
    let turn3_refs = [b_comp.clone()];
    let turn3 = PreviewTurn {
        request_id: "req-3".to_string(),
        ts: 3000,
        canonical_model: "gpt-4o".to_string(),
        protocol: "chat".to_string(),
        messages: vec![PreviewMessage {
            role: "user".to_string(),
            content: "compacted question".to_string(),
            block_hash: b_comp,
            is_new: false,
        }],
        assistant_text: "assistant reply 3".to_string(),
        assistant_reasoning: None,
        assistant_tool_calls: vec![],
        has_load_more_placeholder: false,
        context_compacted_or_branched: false,
    };
    let input3 = [
        (&turn1_refs[..], turn1.clone()),
        (&turn2_refs[..], turn2.clone()),
        (&turn3_refs[..], turn3),
    ];
    let timeline3 = reconstruct_timeline(&input3, 10_000);
    assert_eq!(timeline3.len(), 3);
    assert!(timeline3[2].context_compacted_or_branched);

    // Decryption cap: if max_decrypt_bytes is small, sets has_load_more_placeholder
    let capped_timeline = reconstruct_timeline(&input, 10);
    assert!(capped_timeline[1].has_load_more_placeholder);
}

#[doc = "Spec: TRC-5, Spec: TRC-6, Spec: SEC-19"]
#[test]
fn test_xss_escaping_and_sanitization() {
    let malicious = "<script>alert(1)</script><img src=x onerror=alert(1)>";
    let escaped = escape_html(malicious);
    assert!(!escaped.contains('<'));
    assert!(!escaped.contains('>'));
    assert!(escaped.contains("&lt;script&gt;"));

    let sanitized = sanitize_assistant_text(malicious);
    assert!(!sanitized.contains("<script"));
    assert!(!sanitized.contains("<img"));

    // XSS bypass attempts that bypass naive substring checks
    let svg_xss = "<svg/onmouseover=alert(1)>";
    let img_xss = "<img src=x onerror =alert(1)>";
    let js_link = "[click](javascript:alert(1))";
    let js_a_tag = "<a href=\"javascript:alert(1)\">click</a>";

    let sanitized_svg = sanitize_assistant_text(svg_xss);
    assert!(
        !sanitized_svg.contains("<svg"),
        "svg tag should be stripped or escaped"
    );

    let sanitized_img = sanitize_assistant_text(img_xss);
    assert!(
        !sanitized_img.contains("<img"),
        "img tag should be stripped"
    );
    // Raw HTML is neutralized as escaped text, never emitted as a live tag or
    // event-handler attribute (`SEC-19`).
    assert!(
        sanitized_img.contains("&lt;img"),
        "raw img tag must be escaped as inert text: {sanitized_img}"
    );
    assert!(
        !sanitized_img.contains("<img src=x onerror"),
        "onerror must not survive as a live attribute"
    );

    let sanitized_link = sanitize_assistant_text(js_link);
    assert!(
        !sanitized_link.contains("javascript:"),
        "javascript: link scheme should be stripped"
    );

    let sanitized_a = sanitize_assistant_text(js_a_tag);
    assert!(
        !sanitized_a.contains("<a href=\"javascript:"),
        "javascript: scheme in a live anchor must be stripped"
    );
    assert!(
        sanitized_a.contains("&lt;a"),
        "raw anchor tag must be escaped as inert text: {sanitized_a}"
    );

    // Safe Markdown is preserved and converted to safe HTML tags
    let safe_md = "**bold text** and `code span` and [safe link](https://example.com)";
    let sanitized_safe = sanitize_assistant_text(safe_md);
    assert!(sanitized_safe.contains("<strong>bold text</strong>"));
    assert!(sanitized_safe.contains("<code>code span</code>"));
    assert!(sanitized_safe.contains("<a href=\"https://example.com\""));
    assert!(sanitized_safe.contains("rel=\"noopener noreferrer nofollow\""));
}

#[doc = "Spec: SEC-19, TRC-5"]
#[test]
fn test_raw_html_disabled_mailto_rejected_and_lcp_compaction_detected() {
    let out = sanitize_assistant_markdown("[mail](mailto:test@example.com) <b>raw_tag</b>");
    assert!(
        !out.contains("mailto:"),
        "mailto: scheme must be stripped: {out}"
    );
    assert!(
        !out.contains("<b>raw_tag</b>"),
        "raw HTML tags in markdown must be escaped as text: {out}"
    );
    assert!(out.contains("&lt;b&gt;raw_tag&lt;/b&gt;"));

    // F-062: compaction/branch detection vs identical retry
    let prev = vec!["b1".to_string(), "b2".to_string(), "b3".to_string()];
    let retry = vec!["b1".to_string(), "b2".to_string(), "b3".to_string()];
    let compacted = vec!["b1".to_string(), "b_summary".to_string()];
    assert!(!is_compacted_or_branched(&[], &prev));
    assert!(!is_compacted_or_branched(&prev, &retry));
    assert!(is_compacted_or_branched(&prev, &compacted));
}
