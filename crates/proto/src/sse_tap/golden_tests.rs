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
