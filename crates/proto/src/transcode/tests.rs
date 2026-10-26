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

// Spec: ./docs/srdd/24. SRDD - Cross-Protocol Transcoding and OpenAI Client Normalization.md#241-bidirectional-protocol-transcoding-bridge-cratesprotosrctranscode | Phase: ./docs/bigbang/big-bang-phases-2.md#24-sub-phase-24-zero-copy-surgical-json-span-editor-duplicate-key-guard-and-body-edit-fuzzer
#[test]
#[doc = "Spec: API-1, TRN-2, F099 - 7-protocol canonical request, response, multimodal, tool-call, and incremental SSE stream transcoding"]
fn test_all_seven_protocols_canonical_request_response_and_incremental_stream() {
    use super::{
        CanonicalContentPart, SseStreamTranscoder, encode_canonical_request,
        encode_canonical_response, parse_canonical_request, parse_canonical_response,
    };

    // 1. Parse GoogleGemini request with systemInstruction, inlineData image, functionCall, functionResponse, and functionDeclarations
    let gemini_req = br#"{
        "systemInstruction": {"parts": [{"text": "You are a multimodal assistant."}]},
        "contents": [
            {
                "role": "user",
                "parts": [
                    {"text": "Inspect this image"},
                    {"inlineData": {"mimeType": "image/jpeg", "data": "aGVsbG8="}}
                ]
            },
            {
                "role": "model",
                "parts": [
                    {"thought": true, "text": "Analyzing image...", "thoughtSignature": "sig_123"},
                    {"functionCall": {"name": "get_weather", "args": {"city": "Singapore"}}}
                ]
            },
            {
                "role": "user",
                "parts": [
                    {"functionResponse": {"name": "get_weather", "response": {"content": "31C sunny"}}}
                ]
            }
        ],
        "tools": [{"functionDeclarations": [{"name": "get_weather", "description": "Get weather", "parameters": {"type": "object"}}]}],
        "generationConfig": {"maxOutputTokens": 1024, "temperature": 0.5}
    }"#;

    let canonical = parse_canonical_request(WireProtocol::GoogleGemini, gemini_req)
        .expect("parse gemini request");
    assert_eq!(
        canonical.system.as_deref(),
        Some("You are a multimodal assistant.")
    );
    assert_eq!(canonical.messages.len(), 3);
    assert!(matches!(
        &canonical.messages[0].parts[1],
        CanonicalContentPart::Image { media_type, data_or_url, is_url: false }
        if media_type == "image/jpeg" && data_or_url == "aGVsbG8="
    ));
    assert_eq!(
        canonical.messages[1].reasoning_content.as_deref(),
        Some("Analyzing image...")
    );
    assert_eq!(
        canonical.messages[1].reasoning_signature.as_deref(),
        Some("sig_123")
    );
    assert_eq!(canonical.messages[1].tool_calls[0].name, "get_weather");
    assert_eq!(canonical.messages[2].role, "tool");
    assert_eq!(canonical.messages[2].content, "31C sunny");
    assert_eq!(canonical.tools.len(), 1);

    // 2. Encode into CloudCodeAssist, BedrockConverseStream, and CommandCodeGenerate
    let mut custom_req = canonical.clone();
    custom_req.project = Some("my-gcp-project".to_string());
    custom_req.thread_id = Some("thr_custom_99".to_string());

    let cca_bytes =
        encode_canonical_request(WireProtocol::CloudCodeAssist, &custom_req, "gemini-2.5-pro")
            .expect("encode cloud code assist");
    let cca_val: Value = serde_json::from_slice(&cca_bytes).expect("cca json");
    assert_eq!(cca_val["project"], "my-gcp-project");
    assert_eq!(
        cca_val["request"]["contents"][1]["parts"][1]["functionCall"]["name"],
        "get_weather"
    );

    let bedrock_bytes = encode_canonical_request(
        WireProtocol::BedrockConverseStream,
        &custom_req,
        "anthropic.claude-sonnet-4-6-v1:0",
    )
    .expect("encode bedrock");
    let bedrock_val: Value = serde_json::from_slice(&bedrock_bytes).expect("bedrock json");
    assert_eq!(
        bedrock_val["toolConfig"]["tools"][0]["toolSpec"]["name"],
        "get_weather"
    );
    assert_eq!(
        bedrock_val["messages"][1]["content"][1]["toolUse"]["name"],
        "get_weather"
    );
    assert_eq!(
        bedrock_val["messages"][2]["content"][0]["toolResult"]["content"][0]["text"],
        "31C sunny"
    );

    let cc_bytes = encode_canonical_request(
        WireProtocol::CommandCodeGenerate,
        &custom_req,
        "claude-sonnet-4.6",
    )
    .expect("encode command code");
    let cc_val: Value = serde_json::from_slice(&cc_bytes).expect("cc json");
    assert_eq!(cc_val["threadId"], "thr_custom_99");
    assert_eq!(cc_val["params"]["tools"][0]["name"], "get_weather");

    // 3. Round-trip Bedrock and Gemini non-streaming responses
    let bedrock_resp_parsed = parse_canonical_response(
        WireProtocol::BedrockConverseStream,
        br#"{"output":{"message":{"role":"assistant","content":[{"text":"Done"},{"toolUse":{"toolUseId":"call_9","name":"get_weather","input":{"city":"SG"}}}]}},"stopReason":"tool_use","usage":{"inputTokens":25,"outputTokens":14}}"#,
        "claude-sonnet-4.6",
    )
    .expect("parse bedrock response");
    assert_eq!(bedrock_resp_parsed.text, "Done");
    assert_eq!(bedrock_resp_parsed.tool_calls[0].name, "get_weather");
    assert_eq!(bedrock_resp_parsed.input_tokens, 25);
    assert_eq!(bedrock_resp_parsed.output_tokens, 14);

    let gemini_encoded =
        encode_canonical_response(WireProtocol::GoogleGemini, &bedrock_resp_parsed)
            .expect("encode gemini response");
    let gemini_val: Value = serde_json::from_slice(&gemini_encoded).expect("gemini resp json");
    assert_eq!(gemini_val["usageMetadata"]["promptTokenCount"], 25);
    assert_eq!(gemini_val["usageMetadata"]["candidatesTokenCount"], 14);
    assert_eq!(
        gemini_val["candidates"][0]["content"]["parts"][1]["functionCall"]["name"],
        "get_weather"
    );

    // 4. Incremental SseStreamTranscoder preserves tool calls and reasoning chunks in O(1) frame memory
    let mut stream_tc = SseStreamTranscoder::new(
        WireProtocol::Messages,
        WireProtocol::ChatCompletions,
        "claude-sonnet-4.6",
    );
    let f1 = stream_tc.push_chunk(
        b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":18,\"output_tokens\":0}}}\n\n",
    );
    assert!(f1.is_empty());
    let f2 = stream_tc.push_chunk(
        b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_abc\",\"name\":\"run_bash\",\"input\":{}}}\n\n",
    );
    assert_eq!(f2.len(), 1);
    let f2_str = String::from_utf8_lossy(&f2[0]);
    assert!(f2_str.contains("\"name\":\"run_bash\""));
    let f3 = stream_tc.push_chunk(
        b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n\n",
    );
    assert_eq!(f3.len(), 1);
    let f3_str = String::from_utf8_lossy(&f3[0]);
    assert!(f3_str.contains("arguments"));
    let f4 = stream_tc.push_chunk(
        b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    assert!(!f4.is_empty());
    let f4_str = String::from_utf8_lossy(&f4[0]);
    assert!(f4_str.contains("\"finish_reason\":\"tool_calls\""));
    assert!(f4_str.contains("data: [DONE]"));
}
