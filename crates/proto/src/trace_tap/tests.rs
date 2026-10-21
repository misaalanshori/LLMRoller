//! Tests for bounded TraceTap, binary block replacement, head/tail truncation, and memory bounds.
//!
//! Spec: Section 6.10, Section 7.1, Section 9.2, Section 12.6, Section 21.1,
//! Requirements: TRC-2, TRC-3, PERF-2, PERF-5.

use super::*;

#[doc = "Spec: TRC-2, TRC-3, PERF-2, PERF-5"]
#[test]
fn test_binary_block_omission_replacement() {
    let large_base64 = "A".repeat(4096);
    let mut payload = serde_json::json!({
        "messages": [
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "Describe this image" },
                    { "type": "image", "data": large_base64 },
                    { "type": "image_url", "url": format!("data:image/png;base64,{large_base64}") }
                ]
            }
        ]
    });

    replace_binary_blocks(&mut payload);

    let content_arr = payload["messages"][0]["content"].as_array().expect("array");
    assert_eq!(content_arr[0]["type"], "text");
    assert_eq!(content_arr[0]["text"], "Describe this image");

    // data block replaced
    let data_block = &content_arr[1]["data"];
    assert_eq!(data_block["llmroller_omitted"], "binary");
    assert_eq!(data_block["bytes"], 4096);
    assert!(data_block["sha256"].as_str().is_some());

    // url block replaced
    let url_block = &content_arr[2]["url"];
    assert_eq!(url_block["llmroller_omitted"], "binary");
    assert_eq!(url_block["type"], "url");
}

#[doc = "Spec: TRC-2, TRC-3, PERF-2, PERF-5"]
#[test]
fn test_response_head_tail_truncation_at_one_mib() {
    // 2 MiB string (2 * 1024 * 1024 = 2_097_152 bytes)
    let mut text_2mib = "X".repeat(2 * 1024 * 1024);
    let (truncated, omitted) = apply_head_tail_truncation(&mut text_2mib);

    assert!(truncated, "2 MiB output must be truncated");
    assert_eq!(omitted, 1024 * 1024); // Exactly 1 MiB omitted

    // Text contains marker
    assert!(text_2mib.contains("[llmroller: 1048576 bytes omitted]"));
    // First 512 KiB preserved
    assert!(text_2mib.starts_with(&"X".repeat(100)));
    // Last 512 KiB preserved
    assert!(text_2mib.ends_with(&"X".repeat(100)));

    // Output <= 1 MiB is NOT truncated
    let mut small = "Hello, world!".to_string();
    let (t_small, o_small) = apply_head_tail_truncation(&mut small);
    assert!(!t_small);
    assert_eq!(o_small, 0);
    assert_eq!(small, "Hello, world!");
}

#[doc = "Spec: TRC-2, TRC-3, PERF-2, PERF-5"]
#[test]
fn test_trace_tap_non_blocking_try_send_and_memory_budget() {
    let (tap, _rx) = TraceTap::new();

    // Fill the 256-item channel buffer with 1-byte chunks
    for _ in 0..256 {
        assert!(tap.try_feed_chunk(Bytes::from_static(b"x")));
    }

    // 257th item overflows channel -> try_feed_chunk returns false and sets trace_dropped
    assert!(!tap.try_feed_chunk(Bytes::from_static(b"overflow")));
    assert!(tap.trace_dropped.load(Ordering::SeqCst));

    // Subsequent chunks immediately rejected without delay (< 1 ms, never blocking forwarder) (PERF-2)
    assert!(!tap.try_feed_chunk(Bytes::from_static(b"after drop")));
}

#[doc = "Spec: TRC-2, TRC-3, API-5"]
#[test]
fn reassembles_chat_responses_and_messages_deltas() {
    let (_tap, _rx) = TraceTap::new();
    let mut tap = TraceTap::new().0;
    tap.feed_sse_data_json(&serde_json::json!({
        "id": "chatcmpl",
        "choices": [{"delta": {"content": "Hel"}}]
    }));
    tap.feed_sse_data_json(&serde_json::json!({
        "choices": [{
            "delta": {
                "content": "lo",
                "tool_calls": [{"index": 0, "id": "call_1",
                    "function": {"name": "lookup", "arguments": "{\"q\":"}}]
            },
            "finish_reason": "tool_calls"
        }]
    }));
    assert_eq!(tap.output.response_id.as_deref(), Some("chatcmpl"));
    assert_eq!(tap.output.assistant_text, "Hello");
    assert_eq!(tap.output.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(tap.output.assistant_tool_calls[0].name, "lookup");
    assert_eq!(tap.output.assistant_tool_calls[0].arguments_json, "{\"q\":");

    tap.feed_sse_data_json(&serde_json::json!({
        "type": "response.output_text.delta", "delta": " world"
    }));
    tap.feed_sse_data_json(&serde_json::json!({
        "type": "response.reasoning_summary_text.delta", "delta": "thinking"
    }));
    assert_eq!(tap.output.assistant_text, "Hello world");
    assert_eq!(tap.output.assistant_reasoning, "thinking");

    tap.feed_sse_data_json(&serde_json::json!({
        "type": "content_block_delta",
        "delta": {"type": "text_delta", "text": "!"}
    }));
    assert_eq!(tap.output.assistant_text, "Hello world!");
}

#[doc = "Spec: TRC-2"]
#[test]
fn drops_oversized_request_blocks_above_four_mebibytes() {
    let mut tap = TraceTap::new().0;
    let huge = "x".repeat(MAX_REQUEST_BLOCKS_BYTES + 1);
    let mut request = serde_json::json!({
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "keep"},
                {"type": "image", "data": huge}
            ]
        }]
    });
    assert!(tap.sanitize_request_blocks(&mut request));
    assert_eq!(tap.output.dropped_block_count, 1);
    assert_eq!(request["messages"][0]["content"][0]["text"], "keep");
    assert_eq!(
        request["messages"][0]["content"][1]["llmroller_omitted"],
        "oversized_block"
    );
}

/// Spec: TRC-2; SSE frames may be split across arbitrary chunk boundaries.
#[test]
fn test_feed_sse_chunk_reassembles_frames_split_across_chunks() {
    let mut tap = TraceTap::new().0;
    tap.feed_sse_chunk(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel");
    assert_eq!(tap.output.assistant_text, "");
    tap.feed_sse_chunk(b"lo \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"world\"}}]}\n\ndata: [DONE]\n\n");
    assert_eq!(tap.output.assistant_text, "Hello world");
}

/// Spec: TRC-2; Responses text deltas and Anthropic events share the same collector.
#[test]
fn test_feed_sse_chunk_reassembles_responses_and_anthropic_events() {
    let mut tap = TraceTap::new().0;
    tap.feed_sse_chunk(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"resp \"}\n\n");
    tap.feed_sse_chunk(b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"claude\"}}\n\n");
    assert_eq!(tap.output.assistant_text, "resp claude");
}

/// Spec: TRC-2, TRC-4; non-streaming responses normalize into the same shape.
#[test]
fn test_feed_json_response_normalizes_chat_completion() {
    let mut tap = TraceTap::new().0;
    tap.feed_json_response(&serde_json::json!({
        "id": "chatcmpl-1",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "mock answer",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": "lookup", "arguments": "{\"q\":\"x\"}" }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }));
    assert_eq!(tap.output.response_id.as_deref(), Some("chatcmpl-1"));
    assert_eq!(tap.output.assistant_text, "mock answer");
    assert_eq!(tap.output.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(tap.output.assistant_tool_calls.len(), 1);
    assert_eq!(tap.output.assistant_tool_calls[0].name, "lookup");
}

/// Spec: TRC-4; request prompts become ordered, deduplicated content-addressed blocks.
#[test]
fn test_split_request_blocks_extracts_messages_and_deduplicates() {
    let request = serde_json::json!({
        "model": "gpt-4o",
        "stream": false,
        "system": "be brief",
        "messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": "hi"},
            {"role": "user", "content": "hi"}
        ]
    });
    let prepared = prepare_request_blocks(&request);
    assert!(prepared.skeleton.get("messages").is_none());
    assert!(prepared.skeleton.get("system").is_none());
    assert_eq!(
        prepared.blocks.len(),
        2,
        "system + one deduplicated user block"
    );
    assert_eq!(
        prepared.block_refs,
        prepared
            .blocks
            .iter()
            .map(|(hash, _)| hash.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(prepared.dropped_blocks, 0);
}
