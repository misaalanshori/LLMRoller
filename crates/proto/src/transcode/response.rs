use serde_json::{Value, json};

use super::types::{
    CanonicalResponse, CanonicalToolCall, WireProtocol, extract_text_from_value,
};

/// Parse an upstream non-streaming JSON response from `upstream_proto` into [`CanonicalResponse`] (`F099`).
pub fn parse_canonical_response(
    upstream_proto: WireProtocol,
    body: &[u8],
    fallback_model: &str,
) -> Result<CanonicalResponse, String> {
    let val: Value = serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let id = val
        .get("id")
        .or_else(|| val.get("responseId"))
        .and_then(Value::as_str)
        .unwrap_or("resp_transcoded")
        .to_string();
    let model = val
        .get("model")
        .or_else(|| val.get("modelVersion"))
        .and_then(Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();

    let mut text = String::new();
    let mut reasoning = None;
    let mut tool_calls = Vec::new();
    let mut finish_reason = "stop".to_string();
    let mut input_tokens = 0;
    let mut output_tokens = 0;

    match upstream_proto {
        WireProtocol::ChatCompletions => {
            if let Some(choice) = val
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
            {
                if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
                    finish_reason = fr.to_string();
                }
                if let Some(msg) = choice.get("message") {
                    if let Some(c) = msg.get("content") {
                        text = extract_text_from_value(c);
                    }
                    reasoning = msg
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if let Some(tcs) = msg.get("tool_calls").and_then(Value::as_array) {
                        for tc in tcs {
                            tool_calls.push(CanonicalToolCall {
                                id: tc
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("call_1")
                                    .to_string(),
                                name: tc
                                    .pointer("/function/name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("tool")
                                    .to_string(),
                                arguments_json: tc
                                    .pointer("/function/arguments")
                                    .and_then(Value::as_str)
                                    .unwrap_or("{}")
                                    .to_string(),
                            });
                        }
                    }
                }
            }
            if let Some(u) = val.get("usage") {
                input_tokens = u.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0);
                output_tokens = u
                    .get("completion_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
            }
        }
        WireProtocol::Responses => {
            if let Some(ot) = val.get("output_text").and_then(Value::as_str) {
                text = ot.to_string();
            }
            if let Some(out_arr) = val.get("output").and_then(Value::as_array) {
                for item in out_arr {
                    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                    if item_type == "message" && text.is_empty() {
                        if let Some(c) = item.get("content") {
                            text.push_str(&extract_text_from_value(c));
                        }
                    } else if item_type == "function_call" {
                        tool_calls.push(CanonicalToolCall {
                            id: item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(Value::as_str)
                                .unwrap_or("call_1")
                                .to_string(),
                            name: item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string(),
                            arguments_json: item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}")
                                .to_string(),
                        });
                    }
                }
            }
            if let Some(fr) = val.get("status").and_then(Value::as_str) {
                finish_reason = if fr == "completed" {
                    "stop".to_string()
                } else {
                    fr.to_string()
                };
            }
            if let Some(u) = val.get("usage") {
                input_tokens = u.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
                output_tokens = u.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);
            }
        }
        WireProtocol::Messages => {
            if let Some(c_arr) = val.get("content").and_then(Value::as_array) {
                for block in c_arr {
                    let b_type = block.get("type").and_then(Value::as_str).unwrap_or("");
                    match b_type {
                        "text" => {
                            if let Some(t) = block.get("text").and_then(Value::as_str) {
                                text.push_str(t);
                            }
                        }
                        "thinking" => {
                            if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                                reasoning = Some(t.to_string());
                            }
                        }
                        "tool_use" => {
                            let id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("toolu_1")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let args = block
                                .get("input")
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "{}".to_string());
                            tool_calls.push(CanonicalToolCall {
                                id,
                                name,
                                arguments_json: args,
                            });
                        }
                        _ => {}
                    }
                }
            }
            if let Some(sr) = val.get("stop_reason").and_then(Value::as_str) {
                finish_reason = match sr {
                    "end_turn" => "stop".to_string(),
                    "max_tokens" => "length".to_string(),
                    "tool_use" => "tool_calls".to_string(),
                    other => other.to_string(),
                };
            }
            if let Some(u) = val.get("usage") {
                input_tokens = u.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
                output_tokens = u.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);
            }
        }
        WireProtocol::GoogleGemini | WireProtocol::CloudCodeAssist => {
            let root = val.get("response").unwrap_or(&val);
            if let Some(cand) = root
                .get("candidates")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
            {
                if let Some(parts) = cand.pointer("/content/parts").and_then(Value::as_array) {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            text.push_str(t);
                        } else if let Some(fc) = p.get("functionCall") {
                            let name = fc
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let args = fc
                                .get("args")
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "{}".to_string());
                            tool_calls.push(CanonicalToolCall {
                                id: format!("call_{name}"),
                                name,
                                arguments_json: args,
                            });
                        }
                    }
                }
                if let Some(fr) = cand.get("finishReason").and_then(Value::as_str) {
                    finish_reason = match fr {
                        "STOP" => "stop".to_string(),
                        "MAX_TOKENS" => "length".to_string(),
                        other => other.to_ascii_lowercase(),
                    };
                }
            }
            if let Some(um) = root.get("usageMetadata") {
                input_tokens = um
                    .get("promptTokenCount")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                output_tokens = um
                    .get("candidatesTokenCount")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
            }
        }
        WireProtocol::BedrockConverseStream | WireProtocol::CommandCodeGenerate => {
            if let Some(t) = val.get("text").and_then(Value::as_str) {
                text = t.to_string();
            } else if let Some(p) = val
                .pointer("/output/message/content/0/text")
                .and_then(Value::as_str)
            {
                text = p.to_string();
            }
        }
    }

    Ok(CanonicalResponse {
        id,
        model,
        text,
        reasoning,
        tool_calls,
        finish_reason,
        input_tokens,
        output_tokens,
    })
}

/// Encode a [`CanonicalResponse`] into the downstream client's expected `downstream_proto` JSON format (`F099`).
pub fn encode_canonical_response(
    downstream_proto: WireProtocol,
    resp: &CanonicalResponse,
) -> Result<Vec<u8>, String> {
    let doc = match downstream_proto {
        WireProtocol::ChatCompletions => {
            let mut msg = json!({
                "role": "assistant",
                "content": resp.text,
            });
            if let Some(ref rc) = resp.reasoning {
                msg["reasoning_content"] = Value::String(rc.clone());
            }
            if !resp.tool_calls.is_empty() {
                let tcs: Vec<Value> = resp
                    .tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments_json,
                            }
                        })
                    })
                    .collect();
                msg["tool_calls"] = Value::Array(tcs);
            }
            json!({
                "id": resp.id,
                "object": "chat.completion",
                "model": resp.model,
                "choices": [{
                    "index": 0,
                    "message": msg,
                    "finish_reason": if !resp.tool_calls.is_empty() { "tool_calls" } else { resp.finish_reason.as_str() },
                }],
                "usage": {
                    "prompt_tokens": resp.input_tokens,
                    "completion_tokens": resp.output_tokens,
                    "total_tokens": resp.input_tokens + resp.output_tokens,
                }
            })
        }
        WireProtocol::Responses => {
            let mut output = vec![json!({
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": resp.text,
                }]
            })];
            for tc in &resp.tool_calls {
                output.push(json!({
                    "type": "function_call",
                    "call_id": tc.id,
                    "name": tc.name,
                    "arguments": tc.arguments_json,
                }));
            }
            json!({
                "id": resp.id,
                "object": "response",
                "model": resp.model,
                "status": "completed",
                "output_text": resp.text,
                "output": output,
                "usage": {
                    "input_tokens": resp.input_tokens,
                    "output_tokens": resp.output_tokens,
                    "total_tokens": resp.input_tokens + resp.output_tokens,
                }
            })
        }
        WireProtocol::Messages
        | WireProtocol::GoogleGemini
        | WireProtocol::CloudCodeAssist
        | WireProtocol::BedrockConverseStream
        | WireProtocol::CommandCodeGenerate => {
            let mut content = Vec::new();
            if let Some(ref rc) = resp.reasoning {
                content.push(json!({
                    "type": "thinking",
                    "thinking": rc,
                }));
            }
            if !resp.text.is_empty() || resp.tool_calls.is_empty() {
                content.push(json!({
                    "type": "text",
                    "text": resp.text,
                }));
            }
            for tc in &resp.tool_calls {
                let parsed_in =
                    serde_json::from_str::<Value>(&tc.arguments_json).unwrap_or_else(|_| json!({}));
                content.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": parsed_in,
                }));
            }
            json!({
                "id": resp.id,
                "type": "message",
                "role": "assistant",
                "model": resp.model,
                "content": content,
                "stop_reason": if !resp.tool_calls.is_empty() { "tool_use" } else { "end_turn" },
                "usage": {
                    "input_tokens": resp.input_tokens,
                    "output_tokens": resp.output_tokens,
                }
            })
        }
    };
    serde_json::to_vec(&doc).map_err(|e| e.to_string())
}

/// Transcode a non-streaming response body from `upstream_proto` into `downstream_proto` (`F099`).
pub fn transcode_response(
    upstream_proto: WireProtocol,
    downstream_proto: WireProtocol,
    body: &[u8],
    model: &str,
) -> Result<Vec<u8>, String> {
    if upstream_proto == downstream_proto {
        return Ok(body.to_vec());
    }
    let canonical = parse_canonical_response(upstream_proto, body, model)?;
    encode_canonical_response(downstream_proto, &canonical)
}

/// Transcode a streaming SSE (or NDJSON) payload from `upstream_proto` into `downstream_proto` SSE frames (`F099`).
#[must_use]
pub fn transcode_sse_payload(
    upstream_proto: WireProtocol,
    downstream_proto: WireProtocol,
    raw_stream: &[u8],
    model: &str,
) -> Vec<u8> {
    if upstream_proto == downstream_proto {
        return raw_stream.to_vec();
    }
    let text = String::from_utf8_lossy(raw_stream);
    let mut deltas: Vec<String> = Vec::new();
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;

    for line in text.lines() {
        let trimmed = line.trim();
        let json_slice = if let Some(rest) = trimmed.strip_prefix("data:") {
            rest.trim()
        } else if trimmed.starts_with('{') {
            trimmed
        } else {
            continue;
        };
        if json_slice.is_empty() || json_slice == "[DONE]" {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(json_slice) else {
            continue;
        };

        // Extract text delta from any upstream streaming event format
        if let Some(d) = val
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            if !d.is_empty() {
                deltas.push(d.to_string());
            }
        } else if let Some(d) = val.pointer("/delta/text").and_then(Value::as_str) {
            if !d.is_empty() {
                deltas.push(d.to_string());
            }
        } else if val.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
            && let Some(d) = val.get("delta").and_then(Value::as_str)
        {
            if !d.is_empty() {
                deltas.push(d.to_string());
            }
        } else if val.get("type").and_then(Value::as_str) == Some("text-delta")
            && let Some(d) = val
                .get("textDelta")
                .or_else(|| val.get("delta"))
                .and_then(Value::as_str)
            && !d.is_empty()
        {
            deltas.push(d.to_string());
        } else if let Some(d) = val
            .pointer("/candidates/0/content/parts/0/text")
            .or_else(|| val.pointer("/response/candidates/0/content/parts/0/text"))
            .and_then(Value::as_str)
            && !d.is_empty()
        {
            deltas.push(d.to_string());
        }

        // Extract token usage from any upstream streaming event format
        let usage_obj = val
            .get("usage")
            .or_else(|| val.pointer("/response/usage"))
            .or_else(|| val.pointer("/message/usage"));
        if let Some(u) = usage_obj {
            if let Some(inp) = u
                .get("prompt_tokens")
                .or_else(|| u.get("input_tokens"))
                .or_else(|| u.get("promptTokens"))
                .and_then(Value::as_i64)
            {
                input_tokens = input_tokens.max(inp);
            }
            if let Some(outp) = u
                .get("completion_tokens")
                .or_else(|| u.get("output_tokens"))
                .or_else(|| u.get("completionTokens"))
                .and_then(Value::as_i64)
            {
                output_tokens = output_tokens.max(outp);
            }
        }
        if let Some(um) = val
            .get("usageMetadata")
            .or_else(|| val.pointer("/response/usageMetadata"))
        {
            if let Some(inp) = um.get("promptTokenCount").and_then(Value::as_i64) {
                input_tokens = input_tokens.max(inp);
            }
            if let Some(outp) = um.get("candidatesTokenCount").and_then(Value::as_i64) {
                output_tokens = output_tokens.max(outp);
            }
        }
    }

    let mut out = String::new();
    match downstream_proto {
        WireProtocol::ChatCompletions => {
            for delta in &deltas {
                let chunk = json!({
                    "id": "chatcmpl-transcoded",
                    "object": "chat.completion.chunk",
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": {"content": delta},
                        "finish_reason": Value::Null,
                    }]
                });
                out.push_str(&format!("data: {chunk}\n\n"));
            }
            let stop_chunk = json!({
                "id": "chatcmpl-transcoded",
                "object": "chat.completion.chunk",
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop",
                }],
                "usage": {
                    "prompt_tokens": input_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": input_tokens + output_tokens,
                }
            });
            out.push_str(&format!("data: {stop_chunk}\n\ndata: [DONE]\n\n"));
        }
        WireProtocol::Responses => {
            for delta in &deltas {
                let ev = json!({
                    "type": "response.output_text.delta",
                    "delta": delta,
                });
                out.push_str(&format!(
                    "event: response.output_text.delta\ndata: {ev}\n\n"
                ));
            }
            let full_text = deltas.join("");
            let completed = json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_transcoded",
                    "object": "response",
                    "model": model,
                    "status": "completed",
                    "output_text": full_text,
                    "usage": {
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "total_tokens": input_tokens + output_tokens,
                    }
                }
            });
            out.push_str(&format!("event: response.completed\ndata: {completed}\n\n"));
        }
        WireProtocol::Messages
        | WireProtocol::GoogleGemini
        | WireProtocol::CloudCodeAssist
        | WireProtocol::BedrockConverseStream
        | WireProtocol::CommandCodeGenerate => {
            let msg_start = json!({
                "type": "message_start",
                "message": {
                    "id": "msg_transcoded",
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "usage": {
                        "input_tokens": input_tokens,
                        "output_tokens": 0,
                    }
                }
            });
            out.push_str(&format!("event: message_start\ndata: {msg_start}\n\n"));
            for delta in &deltas {
                let block_delta = json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "text_delta",
                        "text": delta,
                    }
                });
                out.push_str(&format!(
                    "event: content_block_delta\ndata: {block_delta}\n\n"
                ));
            }
            let msg_delta = json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn"},
                "usage": {"output_tokens": output_tokens},
            });
            out.push_str(&format!(
                "event: message_delta\ndata: {msg_delta}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
            ));
        }
    }

    out.into_bytes()
}
