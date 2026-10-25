use serde_json::Value;

use super::types::WireProtocol;
use super::{transcode_request, transcode_response, transcode_sse_payload};

#[test]
#[doc = "Spec: API-1, TRN-2, F099 - bidirectional request, response, and SSE stream transcoding"]
fn regression_test_f099_custom_preset_and_generic_adapter_lack_p() {
    // 1. OpenAI ChatCompletions -> Anthropic Messages request transcoding
    let chat_req = br#"{
        "model": "claude-sonnet-4-6",
        "messages": [
            {"role": "system", "content": "Be concise."},
            {"role": "user", "content": "Hello Anthropic!"}
        ],
        "max_tokens": 512,
        "stream": true
    }"#;
    let anthropic_bytes = transcode_request(
        WireProtocol::ChatCompletions,
        WireProtocol::Messages,
        chat_req,
        "claude-sonnet-4-6",
    )
    .expect("transcode chat -> messages");
    let anthropic_val: Value = serde_json::from_slice(&anthropic_bytes).expect("json");
    assert_eq!(anthropic_val["system"], "Be concise.");
    assert_eq!(anthropic_val["messages"][0]["role"], "user");
    assert_eq!(anthropic_val["messages"][0]["content"], "Hello Anthropic!");
    assert_eq!(anthropic_val["max_tokens"], 512);

    // 2. Anthropic Messages -> OpenAI Responses request transcoding
    let responses_bytes = transcode_request(
        WireProtocol::Messages,
        WireProtocol::Responses,
        &anthropic_bytes,
        "gpt-5.4",
    )
    .expect("transcode messages -> responses");
    let responses_val: Value = serde_json::from_slice(&responses_bytes).expect("json");
    assert_eq!(responses_val["instructions"], "Be concise.");
    assert_eq!(responses_val["input"][0]["content"], "Hello Anthropic!");
    assert_eq!(responses_val["store"], false);

    // 3. Anthropic Messages non-streaming response -> OpenAI ChatCompletions response
    let anthropic_resp = br#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-6",
        "content": [{"type": "text", "text": "Hi from Claude!"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 12, "output_tokens": 5}
    }"#;
    let chat_resp_bytes = transcode_response(
        WireProtocol::Messages,
        WireProtocol::ChatCompletions,
        anthropic_resp,
        "claude-sonnet-4-6",
    )
    .expect("transcode messages -> chat response");
    let chat_resp_val: Value = serde_json::from_slice(&chat_resp_bytes).expect("json");
    assert_eq!(
        chat_resp_val["choices"][0]["message"]["content"],
        "Hi from Claude!"
    );
    assert_eq!(chat_resp_val["usage"]["prompt_tokens"], 12);
    assert_eq!(chat_resp_val["usage"]["completion_tokens"], 5);

    // 4. Anthropic SSE stream -> OpenAI ChatCompletions SSE stream
    let anthropic_sse = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"world!\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4}}\n\n";
    let chat_sse = String::from_utf8(transcode_sse_payload(
        WireProtocol::Messages,
        WireProtocol::ChatCompletions,
        anthropic_sse,
        "claude-sonnet-4-6",
    ))
    .expect("utf8");
    assert!(chat_sse.contains("\"content\":\"Hello \""));
    assert!(chat_sse.contains("\"content\":\"world!\""));
    assert!(chat_sse.contains("\"prompt_tokens\":10"));
    assert!(chat_sse.contains("\"completion_tokens\":4"));
    assert!(chat_sse.contains("data: [DONE]"));
}
