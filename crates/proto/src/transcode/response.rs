use bytes::Bytes;
use serde_json::{Value, json};

use super::types::{CanonicalResponse, CanonicalToolCall, WireProtocol, extract_text_from_value};

/// Parse an upstream non-streaming JSON response from `upstream_proto` into [`CanonicalResponse`] (`F099`, `FND-012`).
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
                        .or_else(|| msg.get("reasoning"))
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
                    } else if item_type == "reasoning" {
                        if let Some(s) = item.get("summary") {
                            let r = extract_text_from_value(s);
                            if !r.is_empty() {
                                reasoning = Some(r);
                            }
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
                        let is_thought = p.get("thought").and_then(Value::as_bool).unwrap_or(false);
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            if is_thought {
                                reasoning = Some(t.to_string());
                            } else {
                                text.push_str(t);
                            }
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
        WireProtocol::BedrockConverseStream => {
            if let Some(blocks) = val
                .pointer("/output/message/content")
                .or_else(|| val.get("content"))
                .and_then(Value::as_array)
            {
                for b in blocks {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    } else if let Some(rt) = b
                        .pointer("/reasoningContent/reasoningText/text")
                        .and_then(Value::as_str)
                    {
                        reasoning = Some(rt.to_string());
                    } else if let Some(tu) = b.get("toolUse") {
                        let id = tu
                            .get("toolUseId")
                            .and_then(Value::as_str)
                            .unwrap_or("toolu_1")
                            .to_string();
                        let name = tu
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string();
                        let args = tu
                            .get("input")
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "{}".to_string());
                        tool_calls.push(CanonicalToolCall {
                            id,
                            name,
                            arguments_json: args,
                        });
                    }
                }
            }
            if let Some(sr) = val.get("stopReason").and_then(Value::as_str) {
                finish_reason = match sr {
                    "end_turn" => "stop".to_string(),
                    "max_tokens" => "length".to_string(),
                    "tool_use" => "tool_calls".to_string(),
                    other => other.to_string(),
                };
            }
            if let Some(u) = val.get("usage") {
                input_tokens = u.get("inputTokens").and_then(Value::as_i64).unwrap_or(0);
                output_tokens = u.get("outputTokens").and_then(Value::as_i64).unwrap_or(0);
            }
        }
        WireProtocol::CommandCodeGenerate => {
            if let Some(t) = val.get("text").and_then(Value::as_str) {
                text = t.to_string();
            } else if let Some(c) = val.pointer("/message/content") {
                text = extract_text_from_value(c);
            }
            if let Some(rc) = val.get("reasoning").and_then(Value::as_str) {
                reasoning = Some(rc.to_string());
            }
            if let Some(tcs) = val
                .get("toolCalls")
                .or_else(|| val.get("tool_calls"))
                .and_then(Value::as_array)
            {
                for tc in tcs {
                    let id = tc
                        .get("toolCallId")
                        .or_else(|| tc.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("call_1")
                        .to_string();
                    let name = tc
                        .get("toolName")
                        .or_else(|| tc.pointer("/function/name"))
                        .or_else(|| tc.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    let args = tc
                        .get("args")
                        .map(|v| {
                            if let Some(s) = v.as_str() {
                                s.to_string()
                            } else {
                                v.to_string()
                            }
                        })
                        .or_else(|| {
                            tc.pointer("/function/arguments")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_else(|| "{}".to_string());
                    tool_calls.push(CanonicalToolCall {
                        id,
                        name,
                        arguments_json: args,
                    });
                }
            }
            if let Some(u) = val
                .get("usage")
                .or_else(|| val.pointer("/meta/billed_units"))
            {
                input_tokens = u
                    .get("promptTokens")
                    .or_else(|| u.get("input_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                output_tokens = u
                    .get("completionTokens")
                    .or_else(|| u.get("output_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
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

/// Encode a [`CanonicalResponse`] into the downstream client's expected `downstream_proto` JSON format (`F099`, `FND-012`).
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
            let mut output = Vec::new();
            if let Some(ref rc) = resp.reasoning {
                output.push(json!({
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": rc}],
                }));
            }
            output.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": resp.text,
                }]
            }));
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
        WireProtocol::Messages => {
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
        WireProtocol::GoogleGemini | WireProtocol::CloudCodeAssist => {
            let mut parts = Vec::new();
            if let Some(ref rc) = resp.reasoning {
                parts.push(json!({
                    "thought": true,
                    "text": rc,
                }));
            }
            if !resp.text.is_empty() || resp.tool_calls.is_empty() {
                parts.push(json!({
                    "text": resp.text,
                }));
            }
            for tc in &resp.tool_calls {
                let args_val =
                    serde_json::from_str::<Value>(&tc.arguments_json).unwrap_or_else(|_| json!({}));
                parts.push(json!({
                    "functionCall": {
                        "name": tc.name,
                        "args": args_val,
                    }
                }));
            }
            let gemini_resp = json!({
                "responseId": resp.id,
                "modelVersion": resp.model,
                "candidates": [{
                    "index": 0,
                    "content": {
                        "role": "model",
                        "parts": parts,
                    },
                    "finishReason": if resp.finish_reason == "length" { "MAX_TOKENS" } else { "STOP" },
                }],
                "usageMetadata": {
                    "promptTokenCount": resp.input_tokens,
                    "candidatesTokenCount": resp.output_tokens,
                    "totalTokenCount": resp.input_tokens + resp.output_tokens,
                }
            });
            if downstream_proto == WireProtocol::CloudCodeAssist {
                json!({ "response": gemini_resp })
            } else {
                gemini_resp
            }
        }
        WireProtocol::BedrockConverseStream => {
            let mut content = Vec::new();
            if let Some(ref rc) = resp.reasoning {
                content.push(json!({
                    "reasoningContent": {
                        "reasoningText": {
                            "text": rc,
                        }
                    }
                }));
            }
            if !resp.text.is_empty() || resp.tool_calls.is_empty() {
                content.push(json!({
                    "text": resp.text,
                }));
            }
            for tc in &resp.tool_calls {
                let args_val =
                    serde_json::from_str::<Value>(&tc.arguments_json).unwrap_or_else(|_| json!({}));
                content.push(json!({
                    "toolUse": {
                        "toolUseId": tc.id,
                        "name": tc.name,
                        "input": args_val,
                    }
                }));
            }
            json!({
                "output": {
                    "message": {
                        "role": "assistant",
                        "content": content,
                    }
                },
                "stopReason": if !resp.tool_calls.is_empty() { "tool_use" } else if resp.finish_reason == "length" { "max_tokens" } else { "end_turn" },
                "usage": {
                    "inputTokens": resp.input_tokens,
                    "outputTokens": resp.output_tokens,
                    "totalTokens": resp.input_tokens + resp.output_tokens,
                }
            })
        }
        WireProtocol::CommandCodeGenerate => {
            let tcs: Vec<Value> = resp
                .tool_calls
                .iter()
                .map(|tc| {
                    let args_val = serde_json::from_str::<Value>(&tc.arguments_json)
                        .unwrap_or_else(|_| Value::String(tc.arguments_json.clone()));
                    json!({
                        "toolCallId": tc.id,
                        "toolName": tc.name,
                        "args": args_val,
                    })
                })
                .collect();
            json!({
                "id": resp.id,
                "model": resp.model,
                "text": resp.text,
                "reasoning": resp.reasoning,
                "toolCalls": tcs,
                "finishReason": if !resp.tool_calls.is_empty() { "tool-calls" } else { resp.finish_reason.as_str() },
                "usage": {
                    "promptTokens": resp.input_tokens,
                    "completionTokens": resp.output_tokens,
                    "totalTokens": resp.input_tokens + resp.output_tokens,
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

/// Incremental, stateful chunk-by-chunk SSE and NDJSON stream transcoder (`FND-012`, `FND-031`, `F099`).
#[derive(Debug, Clone)]
pub struct SseStreamTranscoder {
    pub upstream_proto: WireProtocol,
    pub downstream_proto: WireProtocol,
    pub model: String,
    pub emitted_start: bool,
    pub emitted_done: bool,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub accumulated_text: String,
    pub next_tool_index: usize,
    pending: Vec<u8>,
}

impl SseStreamTranscoder {
    #[must_use]
    pub fn new(upstream_proto: WireProtocol, downstream_proto: WireProtocol, model: &str) -> Self {
        Self {
            upstream_proto,
            downstream_proto,
            model: model.to_string(),
            emitted_start: false,
            emitted_done: false,
            input_tokens: 0,
            output_tokens: 0,
            accumulated_text: String::new(),
            next_tool_index: 0,
            pending: Vec::with_capacity(4096),
        }
    }

    /// Feed a raw byte chunk from upstream (either SSE or Command Code NDJSON) and emit transcoded downstream SSE frames.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        if self.upstream_proto == self.downstream_proto
            && self.upstream_proto != WireProtocol::CommandCodeGenerate
        {
            return vec![Bytes::copy_from_slice(chunk)];
        }
        self.pending.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.pending.drain(..=pos).collect();
            if let Ok(line_str) = std::str::from_utf8(&line_bytes) {
                for frame in self.process_line(line_str) {
                    out.push(Bytes::from(frame));
                }
            }
        }
        out
    }

    /// Flush any remaining partial line and emit terminal protocol framing if not yet emitted.
    pub fn finish(&mut self) -> Vec<Bytes> {
        if self.upstream_proto == self.downstream_proto
            && self.upstream_proto != WireProtocol::CommandCodeGenerate
        {
            if self.pending.is_empty() {
                return Vec::new();
            }
            let rem = std::mem::take(&mut self.pending);
            return vec![Bytes::from(rem)];
        }
        let mut out = Vec::new();
        if !self.pending.is_empty() {
            let rem = std::mem::take(&mut self.pending);
            if let Ok(line_str) = std::str::from_utf8(&rem) {
                for frame in self.process_line(line_str) {
                    out.push(Bytes::from(frame));
                }
            }
        }
        if !self.emitted_done {
            for frame in self.emit_terminal(false) {
                out.push(Bytes::from(frame));
            }
        }
        out
    }

    fn ensure_started(&mut self, out: &mut Vec<String>) {
        if self.emitted_start {
            return;
        }
        self.emitted_start = true;
        if self.downstream_proto == WireProtocol::Messages {
            let msg_start = json!({
                "type": "message_start",
                "message": {
                    "id": "msg_transcoded",
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "output_tokens": 0,
                    }
                }
            });
            out.push(format!("event: message_start\ndata: {msg_start}\n\n"));
        }
    }

    fn process_line(&mut self, line: &str) -> Vec<String> {
        let trimmed = line.trim();
        let json_slice = if let Some(rest) = trimmed.strip_prefix("data:") {
            rest.trim()
        } else if trimmed.starts_with('{') {
            trimmed
        } else {
            return Vec::new();
        };
        if json_slice.is_empty() {
            return Vec::new();
        }
        if json_slice == "[DONE]" {
            return self.emit_terminal(false);
        }
        let Ok(val) = serde_json::from_str::<Value>(json_slice) else {
            return Vec::new();
        };

        // Update observed usage
        let usage_obj = val
            .get("usage")
            .or_else(|| val.pointer("/response/usage"))
            .or_else(|| val.pointer("/message/usage"))
            .or_else(|| val.pointer("/meta/billed_units"));
        if let Some(u) = usage_obj {
            if let Some(inp) = u
                .get("prompt_tokens")
                .or_else(|| u.get("input_tokens"))
                .or_else(|| u.get("promptTokens"))
                .or_else(|| u.get("inputTokens"))
                .and_then(Value::as_i64)
            {
                self.input_tokens = self.input_tokens.max(inp);
            }
            if let Some(outp) = u
                .get("completion_tokens")
                .or_else(|| u.get("output_tokens"))
                .or_else(|| u.get("completionTokens"))
                .or_else(|| u.get("outputTokens"))
                .and_then(Value::as_i64)
            {
                self.output_tokens = self.output_tokens.max(outp);
            }
        }
        if let Some(um) = val
            .get("usageMetadata")
            .or_else(|| val.pointer("/response/usageMetadata"))
        {
            if let Some(inp) = um.get("promptTokenCount").and_then(Value::as_i64) {
                self.input_tokens = self.input_tokens.max(inp);
            }
            if let Some(outp) = um.get("candidatesTokenCount").and_then(Value::as_i64) {
                self.output_tokens = self.output_tokens.max(outp);
            }
        }

        let mut out = Vec::new();

        // Extract text delta
        let mut text_delta: Option<String> = None;
        if let Some(d) = val
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            if !d.is_empty() {
                text_delta = Some(d.to_string());
            }
        } else if let Some(d) = val.pointer("/delta/text").and_then(Value::as_str) {
            if !d.is_empty() {
                text_delta = Some(d.to_string());
            }
        } else if val.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
            && let Some(d) = val.get("delta").and_then(Value::as_str)
            && !d.is_empty()
        {
            text_delta = Some(d.to_string());
        } else if matches!(
            val.get("type")
                .or_else(|| val.get("event_type"))
                .and_then(Value::as_str),
            Some("text-delta" | "text-generation")
        ) && let Some(d) = val
            .get("textDelta")
            .or_else(|| val.get("delta"))
            .or_else(|| val.get("text"))
            .and_then(Value::as_str)
            && !d.is_empty()
        {
            text_delta = Some(d.to_string());
        } else if let Some(d) = val
            .pointer("/candidates/0/content/parts/0/text")
            .or_else(|| val.pointer("/response/candidates/0/content/parts/0/text"))
            .or_else(|| val.pointer("/contentBlockDelta/delta/text"))
            .and_then(Value::as_str)
            && !d.is_empty()
        {
            text_delta = Some(d.to_string());
        }

        // Extract reasoning/thinking delta
        let mut reasoning_delta: Option<String> = None;
        if let Some(r) = val
            .pointer("/choices/0/delta/reasoning_content")
            .or_else(|| val.pointer("/choices/0/delta/reasoning"))
            .or_else(|| val.pointer("/delta/thinking"))
            .or_else(|| val.pointer("/contentBlockDelta/delta/reasoningContent/text"))
            .and_then(Value::as_str)
            && !r.is_empty()
        {
            reasoning_delta = Some(r.to_string());
        } else if val.get("type").and_then(Value::as_str) == Some("reasoning-delta")
            && let Some(r) = val
                .get("textDelta")
                .or_else(|| val.get("delta"))
                .and_then(Value::as_str)
            && !r.is_empty()
        {
            reasoning_delta = Some(r.to_string());
        }

        // Extract tool call deltas or complete tool calls
        let mut tool_calls_observed: Vec<CanonicalToolCall> = Vec::new();
        if let Some(tcs) = val
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for tc in tcs {
                let id = tc
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("call_1")
                    .to_string();
                let name = tc
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let args = tc
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                tool_calls_observed.push(CanonicalToolCall {
                    id,
                    name,
                    arguments_json: args,
                });
            }
        } else if val.get("type").and_then(Value::as_str) == Some("content_block_start")
            && val.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use")
        {
            let id = val
                .pointer("/content_block/id")
                .and_then(Value::as_str)
                .unwrap_or("toolu_1")
                .to_string();
            let name = val
                .pointer("/content_block/name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            tool_calls_observed.push(CanonicalToolCall {
                id,
                name,
                arguments_json: String::new(),
            });
        } else if val.get("type").and_then(Value::as_str) == Some("content_block_delta")
            && val.pointer("/delta/type").and_then(Value::as_str) == Some("input_json_delta")
            && let Some(pj) = val.pointer("/delta/partial_json").and_then(Value::as_str)
        {
            tool_calls_observed.push(CanonicalToolCall {
                id: String::new(),
                name: String::new(),
                arguments_json: pj.to_string(),
            });
        } else if val.get("type").and_then(Value::as_str) == Some("tool-call") {
            let id = val
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("call_1")
                .to_string();
            let name = val
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            let args = val
                .get("args")
                .map(|v| {
                    if let Some(s) = v.as_str() {
                        s.to_string()
                    } else {
                        v.to_string()
                    }
                })
                .unwrap_or_else(|| "{}".to_string());
            tool_calls_observed.push(CanonicalToolCall {
                id,
                name,
                arguments_json: args,
            });
        } else if let Some(parts) = val
            .pointer("/candidates/0/content/parts")
            .or_else(|| val.pointer("/response/candidates/0/content/parts"))
            .and_then(Value::as_array)
        {
            for p in parts {
                if let Some(fc) = p.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    let args = fc
                        .get("args")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "{}".to_string());
                    tool_calls_observed.push(CanonicalToolCall {
                        id: format!("call_{name}"),
                        name,
                        arguments_json: args,
                    });
                }
            }
        }

        if let Some(ref d) = text_delta {
            self.ensure_started(&mut out);
            self.accumulated_text.push_str(d);
            out.push(self.encode_text_delta(d));
        }
        if let Some(ref r) = reasoning_delta {
            self.ensure_started(&mut out);
            out.push(self.encode_reasoning_delta(r));
        }
        for tc in &tool_calls_observed {
            self.ensure_started(&mut out);
            out.push(self.encode_tool_call_delta(tc));
        }

        let is_terminal = matches!(
            val.get("type")
                .or_else(|| val.get("event_type"))
                .and_then(Value::as_str),
            Some("finish" | "stream-end" | "message_stop" | "response.completed")
        );
        if is_terminal {
            out.extend(self.emit_terminal(!tool_calls_observed.is_empty()));
        }

        out
    }

    fn encode_text_delta(&self, delta: &str) -> String {
        match self.downstream_proto {
            WireProtocol::ChatCompletions | WireProtocol::CommandCodeGenerate => {
                let chunk = json!({
                    "id": "chatcmpl-transcoded",
                    "object": "chat.completion.chunk",
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {"content": delta},
                        "finish_reason": Value::Null,
                    }]
                });
                format!("data: {chunk}\n\n")
            }
            WireProtocol::Responses => {
                let ev = json!({
                    "type": "response.output_text.delta",
                    "delta": delta,
                });
                format!("event: response.output_text.delta\ndata: {ev}\n\n")
            }
            WireProtocol::Messages => {
                let block_delta = json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "text_delta",
                        "text": delta,
                    }
                });
                format!("event: content_block_delta\ndata: {block_delta}\n\n")
            }
            WireProtocol::GoogleGemini | WireProtocol::CloudCodeAssist => {
                let g = json!({
                    "candidates": [{
                        "index": 0,
                        "content": {
                            "role": "model",
                            "parts": [{"text": delta}],
                        }
                    }]
                });
                let wrapped = if self.downstream_proto == WireProtocol::CloudCodeAssist {
                    json!({"response": g})
                } else {
                    g
                };
                format!("data: {wrapped}\n\n")
            }
            WireProtocol::BedrockConverseStream => {
                let ev = json!({
                    "contentBlockDelta": {
                        "contentBlockIndex": 0,
                        "delta": {"text": delta}
                    }
                });
                format!("data: {ev}\n\n")
            }
        }
    }

    fn encode_reasoning_delta(&self, delta: &str) -> String {
        match self.downstream_proto {
            WireProtocol::ChatCompletions | WireProtocol::CommandCodeGenerate => {
                let chunk = json!({
                    "id": "chatcmpl-transcoded",
                    "object": "chat.completion.chunk",
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {"reasoning_content": delta},
                        "finish_reason": Value::Null,
                    }]
                });
                format!("data: {chunk}\n\n")
            }
            WireProtocol::Responses => {
                let ev = json!({
                    "type": "response.reasoning_summary_text.delta",
                    "delta": delta,
                });
                format!("event: response.reasoning_summary_text.delta\ndata: {ev}\n\n")
            }
            WireProtocol::Messages => {
                let block_delta = json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": delta,
                    }
                });
                format!("event: content_block_delta\ndata: {block_delta}\n\n")
            }
            WireProtocol::GoogleGemini | WireProtocol::CloudCodeAssist => {
                let g = json!({
                    "candidates": [{
                        "index": 0,
                        "content": {
                            "role": "model",
                            "parts": [{"thought": true, "text": delta}],
                        }
                    }]
                });
                let wrapped = if self.downstream_proto == WireProtocol::CloudCodeAssist {
                    json!({"response": g})
                } else {
                    g
                };
                format!("data: {wrapped}\n\n")
            }
            WireProtocol::BedrockConverseStream => {
                let ev = json!({
                    "contentBlockDelta": {
                        "contentBlockIndex": 0,
                        "delta": {"reasoningContent": {"text": delta}}
                    }
                });
                format!("data: {ev}\n\n")
            }
        }
    }

    fn encode_tool_call_delta(&mut self, tc: &CanonicalToolCall) -> String {
        let idx = self.next_tool_index;
        if !tc.name.is_empty() {
            self.next_tool_index += 1;
        }
        match self.downstream_proto {
            WireProtocol::ChatCompletions | WireProtocol::CommandCodeGenerate => {
                let mut fn_obj = serde_json::Map::new();
                if !tc.name.is_empty() {
                    fn_obj.insert("name".to_string(), Value::String(tc.name.clone()));
                }
                if !tc.arguments_json.is_empty() {
                    fn_obj.insert(
                        "arguments".to_string(),
                        Value::String(tc.arguments_json.clone()),
                    );
                }
                let mut tc_obj = serde_json::Map::new();
                tc_obj.insert("index".to_string(), json!(idx));
                if !tc.id.is_empty() {
                    tc_obj.insert("id".to_string(), Value::String(tc.id.clone()));
                    tc_obj.insert("type".to_string(), Value::String("function".to_string()));
                }
                tc_obj.insert("function".to_string(), Value::Object(fn_obj));
                let chunk = json!({
                    "id": "chatcmpl-transcoded",
                    "object": "chat.completion.chunk",
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {"tool_calls": [Value::Object(tc_obj)]},
                        "finish_reason": Value::Null,
                    }]
                });
                format!("data: {chunk}\n\n")
            }
            WireProtocol::Responses => {
                let ev = json!({
                    "type": "response.function_call_arguments.delta",
                    "call_id": if tc.id.is_empty() { "call_1" } else { tc.id.as_str() },
                    "name": tc.name,
                    "delta": tc.arguments_json,
                });
                format!("event: response.function_call_arguments.delta\ndata: {ev}\n\n")
            }
            WireProtocol::Messages => {
                let mut out = String::new();
                if !tc.name.is_empty() {
                    let start_ev = json!({
                        "type": "content_block_start",
                        "index": idx + 1,
                        "content_block": {
                            "type": "tool_use",
                            "id": if tc.id.is_empty() { "toolu_1" } else { tc.id.as_str() },
                            "name": tc.name,
                            "input": {},
                        }
                    });
                    out.push_str(&format!("event: content_block_start\ndata: {start_ev}\n\n"));
                }
                if !tc.arguments_json.is_empty() {
                    let delta_ev = json!({
                        "type": "content_block_delta",
                        "index": idx.max(1),
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": tc.arguments_json,
                        }
                    });
                    out.push_str(&format!("event: content_block_delta\ndata: {delta_ev}\n\n"));
                }
                out
            }
            WireProtocol::GoogleGemini | WireProtocol::CloudCodeAssist => {
                let args_val =
                    serde_json::from_str::<Value>(&tc.arguments_json).unwrap_or_else(|_| json!({}));
                let g = json!({
                    "candidates": [{
                        "index": 0,
                        "content": {
                            "role": "model",
                            "parts": [{"functionCall": {"name": tc.name, "args": args_val}}],
                        }
                    }]
                });
                let wrapped = if self.downstream_proto == WireProtocol::CloudCodeAssist {
                    json!({"response": g})
                } else {
                    g
                };
                format!("data: {wrapped}\n\n")
            }
            WireProtocol::BedrockConverseStream => {
                let ev = json!({
                    "contentBlockDelta": {
                        "contentBlockIndex": idx,
                        "delta": {
                            "toolUse": {
                                "input": tc.arguments_json,
                            }
                        }
                    }
                });
                format!("data: {ev}\n\n")
            }
        }
    }

    fn emit_terminal(&mut self, has_tool_calls: bool) -> Vec<String> {
        if self.emitted_done {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.ensure_started(&mut out);
        self.emitted_done = true;

        match self.downstream_proto {
            WireProtocol::ChatCompletions | WireProtocol::CommandCodeGenerate => {
                let finish_reason = if has_tool_calls || self.next_tool_index > 0 {
                    "tool_calls"
                } else {
                    "stop"
                };
                let stop_chunk = json!({
                    "id": "chatcmpl-transcoded",
                    "object": "chat.completion.chunk",
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": finish_reason,
                    }],
                    "usage": {
                        "prompt_tokens": self.input_tokens,
                        "completion_tokens": self.output_tokens,
                        "total_tokens": self.input_tokens + self.output_tokens,
                    }
                });
                out.push(format!("data: {stop_chunk}\n\ndata: [DONE]\n\n"));
            }
            WireProtocol::Responses => {
                let completed = json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_transcoded",
                        "object": "response",
                        "model": self.model,
                        "status": "completed",
                        "output_text": self.accumulated_text,
                        "usage": {
                            "input_tokens": self.input_tokens,
                            "output_tokens": self.output_tokens,
                            "total_tokens": self.input_tokens + self.output_tokens,
                        }
                    }
                });
                out.push(format!("event: response.completed\ndata: {completed}\n\n"));
            }
            WireProtocol::Messages => {
                let stop_reason = if has_tool_calls || self.next_tool_index > 0 {
                    "tool_use"
                } else {
                    "end_turn"
                };
                let msg_delta = json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": stop_reason},
                    "usage": {"output_tokens": self.output_tokens},
                });
                out.push(format!(
                    "event: message_delta\ndata: {msg_delta}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
                ));
            }
            WireProtocol::GoogleGemini | WireProtocol::CloudCodeAssist => {
                let g = json!({
                    "candidates": [{
                        "index": 0,
                        "finishReason": "STOP",
                    }],
                    "usageMetadata": {
                        "promptTokenCount": self.input_tokens,
                        "candidatesTokenCount": self.output_tokens,
                        "totalTokenCount": self.input_tokens + self.output_tokens,
                    }
                });
                let wrapped = if self.downstream_proto == WireProtocol::CloudCodeAssist {
                    json!({"response": g})
                } else {
                    g
                };
                out.push(format!("data: {wrapped}\n\n"));
            }
            WireProtocol::BedrockConverseStream => {
                let meta = json!({
                    "messageStop": {"stopReason": "end_turn"},
                    "metadata": {
                        "usage": {
                            "inputTokens": self.input_tokens,
                            "outputTokens": self.output_tokens,
                            "totalTokens": self.input_tokens + self.output_tokens,
                        }
                    }
                });
                out.push(format!("data: {meta}\n\n"));
            }
        }
        out
    }
}

/// Transcode a streaming SSE (or NDJSON) payload from `upstream_proto` into `downstream_proto` SSE frames (`F099`, `FND-012`).
#[must_use]
pub fn transcode_sse_payload(
    upstream_proto: WireProtocol,
    downstream_proto: WireProtocol,
    raw_stream: &[u8],
    model: &str,
) -> Vec<u8> {
    if upstream_proto == downstream_proto && upstream_proto != WireProtocol::CommandCodeGenerate {
        return raw_stream.to_vec();
    }
    let mut transcoder = SseStreamTranscoder::new(upstream_proto, downstream_proto, model);
    let mut out = Vec::new();
    for b in transcoder.push_chunk(raw_stream) {
        out.extend_from_slice(&b);
    }
    for b in transcoder.finish() {
        out.extend_from_slice(&b);
    }
    out
}
