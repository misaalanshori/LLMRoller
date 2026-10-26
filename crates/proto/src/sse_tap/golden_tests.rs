//! Golden fixture and token extraction tests for SseUsageTap and JsonUsageExtractor.
//!
//! Spec: Section 2.5, Section 6.8, Section 6.9, Section 12.4.1, Section 12.5,
//! Requirements: API-5, USG-1, USG-2.

use super::*;
use crate::adapters::EndpointKind;
use crate::json_usage::extract_json_usage;

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn test_openai_chat_cache_subtraction_and_miss_tokens() {
    // 1. Standard OpenAI chat with prompt_tokens_details
    let chat_json = serde_json::json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_tokens_details": {
                "cached_tokens": 30,
                "cache_write_tokens": 20
            },
            "completion_tokens_details": {
                "reasoning_tokens": 15
            }
        }
    });

    let u = extract_json_usage(EndpointKind::Chat, &chat_json).expect("extract usage");
    // input_uncached = 100 - 30 - 20 = 50
    assert_eq!(u.input_uncached_tokens, 50);
    assert_eq!(u.cache_read_tokens, 30);
    assert_eq!(u.cache_write_tokens, 20);
    assert_eq!(u.output_tokens, 50);
    assert_eq!(u.reasoning_tokens, 15);

    // 2. OpenAI chat with explicit prompt_cache_miss_tokens
    let miss_json = serde_json::json!({
        "id": "chatcmpl-456",
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 40,
            "prompt_cache_hit_tokens": 30,
            "prompt_cache_miss_tokens": 70
        }
    });

    let u_miss = extract_json_usage(EndpointKind::Chat, &miss_json).expect("extract usage");
    assert_eq!(u_miss.input_uncached_tokens, 70);
    assert_eq!(u_miss.cache_read_tokens, 30);
    assert_eq!(u_miss.output_tokens, 40);
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn test_openai_responses_cache_subtraction() {
    let resp_json = serde_json::json!({
        "id": "resp_abc",
        "object": "response",
        "usage": {
            "input_tokens": 200,
            "output_tokens": 60,
            "input_tokens_details": {
                "cached_tokens": 50,
                "cache_write_tokens": 50
            },
            "output_tokens_details": {
                "reasoning_tokens": 25
            }
        }
    });

    let u = extract_json_usage(EndpointKind::Responses, &resp_json).expect("extract usage");
    // input_uncached = 200 - 50 - 50 = 100
    assert_eq!(u.input_uncached_tokens, 100);
    assert_eq!(u.cache_read_tokens, 50);
    assert_eq!(u.cache_write_tokens, 50);
    assert_eq!(u.output_tokens, 60);
    assert_eq!(u.reasoning_tokens, 25);
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn test_anthropic_messages_streaming_usage_lifecycle() {
    let mut tap = SseUsageTap::new(EndpointKind::Messages);

    // message_start event
    let chunk1 = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":80,\"output_tokens\":1,\"cache_read_input_tokens\":20,\"cache_creation_input_tokens\":10}}}\n\n";
    tap.feed_chunk(chunk1);

    assert_eq!(
        tap.observed_usage,
        Some(ExtractedUsage {
            input_uncached_tokens: 80,
            cache_read_tokens: 20,
            cache_write_tokens: 10,
            output_tokens: 1,
            reasoning_tokens: 0,
            provider_cost_micro_usd: None,
        })
    );

    // content_block_delta events
    let chunk2 = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\n";
    let chunk3 = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world!\"}}\n\n";
    tap.feed_chunk(chunk2);
    tap.feed_chunk(chunk3);

    assert_eq!(tap.accumulated_output_text_bytes, 12); // "Hello world!" = 12 bytes

    // message_delta event with final output_tokens
    let chunk4 = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":45}}\n\n";
    tap.feed_chunk(chunk4);

    let final_u = tap.finalize_usage();
    assert_eq!(final_u.input_uncached_tokens, 80);
    assert_eq!(final_u.cache_read_tokens, 20);
    assert_eq!(final_u.cache_write_tokens, 10);
    assert_eq!(final_u.output_tokens, 45);
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn test_injected_usage_chunk_detection() {
    let mut tap = SseUsageTap::new(EndpointKind::Chat);

    // Normal content frame
    let frame1 = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n";
    tap.feed_chunk(frame1);
    assert!(!tap.saw_injected_usage_chunk);

    // Injected usage chunk (choices:[] and usage:{...}) (API-5)
    let injected_frame = b"data: {\"choices\": [], \"usage\": {\"prompt_tokens\": 50, \"completion_tokens\": 10, \"total_tokens\": 60}}\n\n";
    tap.feed_chunk(injected_frame);
    assert!(
        tap.saw_injected_usage_chunk,
        "injected usage chunk must be detected"
    );

    let u = tap.finalize_usage();
    assert_eq!(u.input_uncached_tokens, 50);
    assert_eq!(u.output_tokens, 10);
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn test_aborted_stream_text_estimation() {
    let mut tap = SseUsageTap::new(EndpointKind::Chat);

    // Feed 10 KiB of delta text (10,240 bytes)
    let chunk_10k = "a".repeat(10_240);
    let sse_chunk =
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{chunk_10k}\"}}}}]}}\n\n");
    tap.feed_chunk(sse_chunk.as_bytes());

    // Stream aborts without upstream usage frame
    let estimated = tap.finalize_usage();
    // ceil(10,240 / 4) = 2,560 tokens
    assert_eq!(estimated.output_tokens, 2560);
    assert_eq!(estimated.input_uncached_tokens, 0);

    // Exact 10,000 bytes -> ceil(10,000 / 4) = 2,500 tokens
    let mut tap2 = SseUsageTap::new(EndpointKind::Chat);
    let chunk_10000 = "b".repeat(10_000);
    let sse_chunk2 =
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{chunk_10000}\"}}}}]}}\n\n");
    tap2.feed_chunk(sse_chunk2.as_bytes());
    let est2 = tap2.finalize_usage();
    assert_eq!(est2.output_tokens, 2500);
}

#[doc = "Spec: API-5, USG-1, USG-2, USG-3"]
#[test]
fn test_responses_sse_extracts_nested_response_completed_usage_and_delta_bytes() {
    let mut tap = SseUsageTap::new(EndpointKind::Responses);

    // Some compatible providers omit the `event:` line and only carry `type`.
    tap.feed_chunk(
        b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello world\"}\n\n",
    );
    tap.feed_chunk(
        b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":100,\"output_tokens\":25,\"input_tokens_details\":{\"cached_tokens\":10},\"output_tokens_details\":{\"reasoning_tokens\":5}}}}\n\n",
    );

    assert_eq!(tap.accumulated_output_text_bytes, 11);
    let usage = tap.finalize_usage();
    assert_eq!(usage.input_uncached_tokens, 90);
    assert_eq!(usage.cache_read_tokens, 10);
    assert_eq!(usage.output_tokens, 25);
    assert_eq!(usage.reasoning_tokens, 5);
}

#[doc = "Spec: API-5, USG-1, USG-2, USG-3"]
#[test]
fn test_responses_sse_null_usage_does_not_clobber_aborted_estimation() {
    let mut tap = SseUsageTap::new(EndpointKind::Responses);
    tap.feed_chunk(b"data: {\"type\":\"response.created\",\"response\":{\"usage\":null}}\n\n");
    tap.feed_chunk(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n");

    assert!(
        tap.observed_usage.is_none(),
        "a null usage object must not be treated as observed usage"
    );
    let usage = tap.finalize_usage();
    assert_eq!(usage.output_tokens, 2);
}

#[doc = "Spec: API-5, USG-1, USG-2, USG-3"]
#[test]
fn test_messages_sse_accumulates_thinking_and_input_json_and_merges_message_delta_usage() {
    let mut tap = SseUsageTap::new(EndpointKind::Messages);
    tap.feed_chunk(
        b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":80,\"output_tokens\":1,\"cache_read_input_tokens\":20,\"cache_creation_input_tokens\":10,\"reasoning_tokens\":3}}}\n\n",
    );
    tap.feed_chunk(
        b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"deep\"}}\n\n",
    );
    tap.feed_chunk(
        b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n\n",
    );
    assert_eq!(tap.accumulated_output_text_bytes, 4 + 12);

    // message_delta carries cumulative output plus refreshed input/cache counts.
    tap.feed_chunk(
        b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"input_tokens\":90,\"output_tokens\":45,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":11}}\n\n",
    );

    let usage = tap.finalize_usage();
    assert_eq!(usage.input_uncached_tokens, 90);
    assert_eq!(usage.cache_read_tokens, 30);
    assert_eq!(usage.cache_write_tokens, 11);
    assert_eq!(usage.output_tokens, 45);
    assert_eq!(usage.reasoning_tokens, 3);

    // Absent fields in a later message_delta preserve the last observed values.
    tap.feed_chunk(
        b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":46}}\n\n",
    );
    let usage = tap.finalize_usage();
    assert_eq!(usage.input_uncached_tokens, 90);
    assert_eq!(usage.cache_read_tokens, 30);
    assert_eq!(usage.cache_write_tokens, 11);
    assert_eq!(usage.output_tokens, 46);
}

#[doc = "Spec: API-5, USG-1, USG-2, USG-3"]
#[test]
fn test_chat_sse_accumulates_tool_arguments_and_openrouter_reasoning() {
    let mut tap = SseUsageTap::new(EndpointKind::Chat);
    tap.feed_chunk(
        b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\",\"reasoning\":\"think\",\"reasoning_content\":\"plan\"}}]}\n\n",
    );
    tap.feed_chunk(
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"run\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}]}}]}\n\n",
    );

    // No upstream usage frame: estimate from every decoded output delta.
    assert_eq!(tap.accumulated_output_text_bytes, 2 + 5 + 4 + 12);
    let usage = tap.finalize_usage();
    assert_eq!(usage.output_tokens, 6);
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn test_injected_usage_chunk_detection_tolerates_json_whitespace() {
    let mut tap = SseUsageTap::new(EndpointKind::Chat);
    tap.feed_chunk(b"data: {\"choices\" : [ ] , \"usage\" : {\"prompt_tokens\": 7, \"completion_tokens\": 2}}\n\n");
    assert!(
        tap.saw_injected_usage_chunk,
        "structurally empty choices with an object usage must be detected"
    );
    let usage = tap.finalize_usage();
    assert_eq!(usage.input_uncached_tokens, 7);
    assert_eq!(usage.output_tokens, 2);
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn test_openrouter_cost_parsing() {
    let chat_openrouter = serde_json::json!({
        "id": "gen-123",
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "cost": 0.001234
        }
    });

    let u = extract_json_usage(EndpointKind::Chat, &chat_openrouter).expect("extract usage");
    assert_eq!(u.provider_cost_micro_usd, Some(1234));
}

#[doc = "Spec: API-5, USG-1, USG-2, USG-3, USG-9"]
#[test]
fn test_aborted_messages_and_responses_streams_preserve_input_and_estimate_output() {
    // F-002 & F-003: message_start followed by delta text and abort before message_delta
    let mut msg_tap = SseUsageTap::new(EndpointKind::Messages);
    msg_tap.feed_chunk(
        b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":120,\"output_tokens\":1,\"cache_read_input_tokens\":40,\"cache_creation\":{\"ephemeral_5m_input_tokens\":15,\"ephemeral_1h_input_tokens\":5},\"output_tokens_details\":{\"thinking_tokens\":9}}}}\n\n",
    );
    let delta_400 = "x".repeat(400);
    let delta_frame = format!(
        "event: content_block_delta\ndata: {}\n\n",
        serde_json::json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": delta_400}
        })
    );
    msg_tap.feed_chunk(delta_frame.as_bytes());
    let u = msg_tap.finalize_usage();
    assert_eq!(u.input_uncached_tokens, 120);
    assert_eq!(u.cache_read_tokens, 40);
    assert_eq!(u.cache_write_tokens, 20);
    assert_eq!(u.reasoning_tokens, 9);
    assert_eq!(u.output_tokens, 100); // ceil(400 / 4) = 100

    // F-002: message_delta with input_tokens: 0 or null does not clobber message_start input_tokens
    msg_tap.feed_chunk(
        b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":0,\"output_tokens\":150}}\n\n",
    );
    let u_done = msg_tap.finalize_usage();
    assert_eq!(u_done.input_uncached_tokens, 120);
    assert_eq!(u_done.output_tokens, 150);
    assert_eq!(msg_tap.stop_reason.as_deref(), Some("end_turn"));

    // F-003 & F-006: response.created with output_tokens: 0 followed by abort estimates output
    let mut resp_tap = SseUsageTap::new(EndpointKind::Responses);
    resp_tap.feed_chunk(
        b"data: {\"type\":\"response.created\",\"response\":{\"usage\":{\"input_tokens\":80,\"output_tokens\":0}}}\n\n",
    );
    resp_tap.feed_chunk(
        b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"1234567890123456\"}\n\n",
    );
    let u_resp = resp_tap.finalize_usage();
    assert_eq!(u_resp.input_uncached_tokens, 80);
    assert_eq!(u_resp.output_tokens, 4);

    // F-006: incomplete_details.reason == max_output_tokens normalizes to "length"
    resp_tap.feed_chunk(
        b"data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"usage\":{\"input_tokens\":80,\"output_tokens\":32}}}\n\n",
    );
    assert_eq!(resp_tap.stop_reason.as_deref(), Some("length"));
    assert_eq!(resp_tap.finalize_usage().output_tokens, 32);
}

#[doc = "Spec: USG-1, USG-2, F009, F021, F022 - unterminated EOF SSE frame flush and nested responses/messages usage"]
#[test]
fn test_unterminated_eof_sse_frame_and_nested_responses_messages_usage() {
    // F009: Final usage chunk at EOF without trailing \n\n is flushed by finalize_usage() / finish()
    let mut chat_tap = SseUsageTap::new(EndpointKind::Chat);
    chat_tap.feed_chunk(
        b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7}}",
    );
    assert!(chat_tap.is_complete_at_eof());
    let u_chat = chat_tap.finalize_usage();
    assert_eq!(u_chat.input_uncached_tokens, 42);
    assert_eq!(u_chat.output_tokens, 7);

    // F021: response.completed with nested input_tokens_details.cached_tokens and output_tokens_details.reasoning_tokens
    let mut resp_tap = SseUsageTap::new(EndpointKind::Responses);
    resp_tap.feed_chunk(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":200,\"input_tokens_details\":{\"cached_tokens\":150},\"output_tokens\":90,\"output_tokens_details\":{\"reasoning_tokens\":30}}}}\n\n",
    );
    let u_resp = resp_tap.finalize_usage();
    assert_eq!(u_resp.input_uncached_tokens, 50);
    assert_eq!(u_resp.cache_read_tokens, 150);
    assert_eq!(u_resp.output_tokens, 90);
    assert_eq!(u_resp.reasoning_tokens, 30);
}

// Spec: ./docs/srdd/13. SRDD - Usage Tracking, Quotas, and Cost Accounting.md#131-token-and-cost-accounting | Phase: ./docs/bigbang/big-bang-phases-2.md#25-sub-phase-25-unbuffered-sse-forwarder-64-kib-pre-first-frame-holding-buffer-single-frame-filter-and-requestlifecycle-guard
#[doc = "Spec: USG-1, USG-2, SEC-8 - Gemini usageMetadata, Bedrock usage, Cohere billed_units, and >1MiB multimodal SSE frame support"]
#[test]
fn test_extended_provider_usage_extraction_and_large_multimodal_sse_frame() {
    use crate::json_usage::extract_json_usage;
    use crate::sse_frame::DEFAULT_MAX_FRAME_BYTES;

    assert_eq!(DEFAULT_MAX_FRAME_BYTES, 16 * 1024 * 1024);

    // 1. Gemini usageMetadata
    let gemini_json = serde_json::json!({
        "usageMetadata": {
            "promptTokenCount": 500,
            "cachedContentTokenCount": 200,
            "candidatesTokenCount": 120,
            "thoughtsTokenCount": 45
        }
    });
    let u_gem = extract_json_usage(EndpointKind::Chat, &gemini_json).expect("gemini usage");
    assert_eq!(u_gem.input_uncached_tokens, 300);
    assert_eq!(u_gem.cache_read_tokens, 200);
    assert_eq!(u_gem.output_tokens, 120);
    assert_eq!(u_gem.reasoning_tokens, 45);

    // 2. AWS Bedrock Converse usage
    let bedrock_json = serde_json::json!({
        "usage": {
            "inputTokens": 180,
            "outputTokens": 65,
            "cacheReadInputTokens": 40,
            "cacheWriteInputTokens": 10
        }
    });
    let u_bed = extract_json_usage(EndpointKind::Chat, &bedrock_json).expect("bedrock usage");
    assert_eq!(u_bed.input_uncached_tokens, 180);
    assert_eq!(u_bed.output_tokens, 65);
    assert_eq!(u_bed.cache_read_tokens, 40);
    assert_eq!(u_bed.cache_write_tokens, 10);

    // 3. Cohere meta.billed_units
    let cohere_json = serde_json::json!({
        "meta": {
            "billed_units": {
                "input_tokens": 95,
                "output_tokens": 33
            }
        }
    });
    let u_coh = extract_json_usage(EndpointKind::Chat, &cohere_json).expect("cohere usage");
    assert_eq!(u_coh.input_uncached_tokens, 95);
    assert_eq!(u_coh.output_tokens, 33);

    // 4. SseUsageTap handles > 1 MiB multimodal frame without dropping pending buffer
    let mut tap = SseUsageTap::new(EndpointKind::Chat);
    let large_b64 = "A".repeat(1_200_000);
    let frame = format!(
        "data: {{"choices":[{{"delta":{{"content":"{large_b64}"}}}}],"usage":{{"prompt_tokens":10,"completion_tokens":20}}}}\n\n"
    );
    tap.feed_chunk(frame.as_bytes());
    let u_stream = tap.finalize_usage();
    assert_eq!(u_stream.input_uncached_tokens, 10);
    assert_eq!(u_stream.output_tokens, 20);
}
