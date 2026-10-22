//! Golden SSE fixture replay across arbitrary chunk boundaries.
//!
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#1652-golden-fixtures-layout
//! Requirements: API-5, USG-1, USG-2, USG-3

use llmroller_proto::adapters::EndpointKind;
use llmroller_proto::sse_tap::SseUsageTap;

/// Deliberately small and non-frame-aligned so every replay exercises line buffering.
const REPLAY_CHUNK_BYTES: usize = 7;

fn replay(fixture: &[u8], endpoint: EndpointKind) -> SseUsageTap {
    let mut tap = SseUsageTap::new(endpoint);
    for chunk in fixture.chunks(REPLAY_CHUNK_BYTES) {
        tap.feed_chunk(chunk);
    }
    tap
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn openai_chat_include_usage_fixture_extracts_usage_and_suppresses_injected_chunk() {
    let tap = replay(
        include_bytes!("../../../fixtures/sse/openai_chat_include_usage.sse"),
        EndpointKind::Chat,
    );

    assert!(tap.saw_injected_usage_chunk);
    assert_eq!(tap.accumulated_output_text_bytes, 5);
    let usage = tap.finalize_usage();
    assert_eq!(usage.input_uncached_tokens, 70);
    assert_eq!(usage.cache_read_tokens, 30);
    assert_eq!(usage.cache_write_tokens, 0);
    assert_eq!(usage.output_tokens, 50);
    assert_eq!(usage.reasoning_tokens, 15);
}

#[doc = "Spec: API-5, USG-1, USG-3"]
#[test]
fn openai_chat_without_usage_fixture_estimates_from_deltas() {
    let tap = replay(
        include_bytes!("../../../fixtures/sse/openai_chat_without_usage.sse"),
        EndpointKind::Chat,
    );

    assert!(tap.observed_usage.is_none());
    assert_eq!(tap.accumulated_output_text_bytes, 2);
    let usage = tap.finalize_usage();
    assert_eq!(usage.output_tokens, 1);
    assert_eq!(usage.input_uncached_tokens, 0);
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn openai_responses_fixture_extracts_nested_completed_usage() {
    let tap = replay(
        include_bytes!("../../../fixtures/sse/openai_responses.sse"),
        EndpointKind::Responses,
    );

    assert_eq!(tap.accumulated_output_text_bytes, 5);
    let usage = tap.finalize_usage();
    assert_eq!(usage.input_uncached_tokens, 100);
    assert_eq!(usage.cache_read_tokens, 50);
    assert_eq!(usage.cache_write_tokens, 50);
    assert_eq!(usage.output_tokens, 60);
    assert_eq!(usage.reasoning_tokens, 25);
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn anthropic_messages_fixture_merges_message_lifecycle_usage() {
    let tap = replay(
        include_bytes!("../../../fixtures/sse/anthropic_messages.sse"),
        EndpointKind::Messages,
    );

    assert_eq!(tap.accumulated_output_text_bytes, 12);
    let usage = tap.finalize_usage();
    assert_eq!(usage.input_uncached_tokens, 80);
    assert_eq!(usage.cache_read_tokens, 20);
    assert_eq!(usage.cache_write_tokens, 10);
    assert_eq!(usage.output_tokens, 45);
}

#[doc = "Spec: API-5, USG-1, USG-2"]
#[test]
fn openrouter_keepalive_fixture_ignores_comments_and_captures_cost() {
    let tap = replay(
        include_bytes!("../../../fixtures/sse/openrouter_keepalive_cost.sse"),
        EndpointKind::Chat,
    );

    assert!(tap.saw_injected_usage_chunk);
    assert_eq!(tap.accumulated_output_text_bytes, 2);
    let usage = tap.finalize_usage();
    assert_eq!(usage.input_uncached_tokens, 10);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(usage.provider_cost_micro_usd, Some(1234));
}

#[doc = "Spec: API-5, USG-3"]
#[test]
fn truncated_fixture_finalizes_without_upstream_usage() {
    let tap = replay(
        include_bytes!("../../../fixtures/sse/truncated.sse"),
        EndpointKind::Chat,
    );

    assert!(tap.observed_usage.is_none());
    assert_eq!(tap.accumulated_output_text_bytes, 13);
    let usage = tap.finalize_usage();
    assert_eq!(usage.output_tokens, 4);
    assert_eq!(usage.input_uncached_tokens, 0);
}
