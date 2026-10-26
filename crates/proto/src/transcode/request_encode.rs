#![forbid(unsafe_code)]
use serde_json::{Value, json};

use super::request::parse_canonical_request;
use super::types::{CanonicalContentPart, CanonicalRequest, WireProtocol};

/// Encode a [`CanonicalRequest`] into the target upstream wire format (`F099`, `FND-011`).
pub fn encode_canonical_request(
    target: WireProtocol,
    req: &CanonicalRequest,
    target_model: &str,
) -> Result<Vec<u8>, String> {
    let model = if target_model.is_empty() {
        req.model.as_str()
    } else {
        target_model
    };

    let doc = match target {
        WireProtocol::ChatCompletions => {
            let mut msgs = Vec::new();
            if let Some(ref sys) = req.system {
                msgs.push(json!({"role": "system", "content": sys}));
            }
            for m in &req.messages {
                let mut m_obj = serde_json::Map::new();
                m_obj.insert("role".to_string(), Value::String(m.role.clone()));
                let has_multimodal = m
                    .parts
                    .iter()
                    .any(|p| !matches!(p, CanonicalContentPart::Text { .. }));
                if has_multimodal {
                    let arr: Vec<Value> = m
                        .parts
                        .iter()
                        .map(|p| match p {
                            CanonicalContentPart::Text { text } => {
                                json!({"type": "text", "text": text})
                            }
                            CanonicalContentPart::Image {
                                media_type,
                                data_or_url,
                                is_url,
                            } => {
                                let url = if *is_url {
                                    data_or_url.clone()
                                } else {
                                    format!("data:{media_type};base64,{data_or_url}")
                                };
                                json!({"type": "image_url", "image_url": {"url": url}})
                            }
                            CanonicalContentPart::Audio { format, data } => {
                                json!({"type": "input_audio", "input_audio": {"format": format, "data": data}})
                            }
                            CanonicalContentPart::Document {
                                data_or_url, name, ..
                            } => {
                                json!({"type": "file", "file": {"file_data": data_or_url, "filename": name}})
                            }
                        })
                        .collect();
                    m_obj.insert("content".to_string(), Value::Array(arr));
                } else {
                    m_obj.insert("content".to_string(), Value::String(m.content.clone()));
                }
                if let Some(ref rc) = m.reasoning_content {
                    m_obj.insert("reasoning_content".to_string(), Value::String(rc.clone()));
                }
                if let Some(ref sig) = m.reasoning_signature {
                    m_obj.insert("reasoning_opaque".to_string(), Value::String(sig.clone()));
                }
                if let Some(ref tc_id) = m.tool_call_id {
                    m_obj.insert("tool_call_id".to_string(), Value::String(tc_id.clone()));
                }
                if !m.tool_calls.is_empty() {
                    let tcs: Vec<Value> = m
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
                    m_obj.insert("tool_calls".to_string(), Value::Array(tcs));
                }
                msgs.push(Value::Object(m_obj));
            }
            let mut out = json!({
                "model": model,
                "messages": msgs,
                "stream": req.stream,
            });
            if let Some(max_tok) = req.max_tokens {
                out["max_tokens"] = json!(max_tok);
            }
            if let Some(temp) = req.temperature {
                out["temperature"] = json!(temp);
            }
            if !req.tools.is_empty() {
                let tools: Vec<Value> = req
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect();
                out["tools"] = Value::Array(tools);
            }
            out
        }
        WireProtocol::Responses => {
            let mut input = Vec::new();
            for m in &req.messages {
                if m.role == "tool" {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": m.tool_call_id.as_deref().unwrap_or("call_1"),
                        "output": m.content,
                    }));
                } else if !m.tool_calls.is_empty() {
                    if !m.content.is_empty() {
                        input.push(json!({
                            "role": "assistant",
                            "content": m.content,
                        }));
                    }
                    for tc in &m.tool_calls {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.name,
                            "arguments": tc.arguments_json,
                        }));
                    }
                } else {
                    input.push(json!({
                        "role": m.role,
                        "content": m.content,
                    }));
                }
            }
            let mut out = json!({
                "model": model,
                "input": input,
                "stream": req.stream,
                "store": false,
            });
            if let Some(ref sys) = req.system {
                out["instructions"] = Value::String(sys.clone());
            }
            if let Some(max_tok) = req.max_tokens {
                out["max_output_tokens"] = json!(max_tok);
            }
            if let Some(temp) = req.temperature {
                out["temperature"] = json!(temp);
            }
            if !req.tools.is_empty() {
                let tools: Vec<Value> = req
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        })
                    })
                    .collect();
                out["tools"] = Value::Array(tools);
            }
            out
        }
        WireProtocol::Messages => {
            let mut msgs = Vec::new();
            for m in &req.messages {
                if m.role == "tool" {
                    msgs.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": m.tool_call_id.as_deref().unwrap_or("toolu_1"),
                            "content": m.content,
                        }]
                    }));
                } else if !m.tool_calls.is_empty() || m.reasoning_content.is_some() {
                    let mut blocks = Vec::new();
                    if let Some(ref rc) = m.reasoning_content {
                        let mut tb = json!({"type": "thinking", "thinking": rc});
                        if let Some(ref sig) = m.reasoning_signature {
                            tb["signature"] = Value::String(sig.clone());
                        }
                        blocks.push(tb);
                    }
                    if !m.content.is_empty() {
                        blocks.push(json!({"type": "text", "text": m.content}));
                    }
                    for tc in &m.tool_calls {
                        let parsed_input = serde_json::from_str::<Value>(&tc.arguments_json)
                            .unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": parsed_input,
                        }));
                    }
                    msgs.push(json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                } else if m
                    .parts
                    .iter()
                    .any(|p| !matches!(p, CanonicalContentPart::Text { .. }))
                {
                    let blocks: Vec<Value> = m
                        .parts
                        .iter()
                        .map(|p| match p {
                            CanonicalContentPart::Text { text } => {
                                json!({"type": "text", "text": text})
                            }
                            CanonicalContentPart::Image {
                                media_type,
                                data_or_url,
                                is_url,
                            } => {
                                if *is_url {
                                    json!({
                                        "type": "image",
                                        "source": {"type": "url", "url": data_or_url}
                                    })
                                } else {
                                    json!({
                                        "type": "image",
                                        "source": {"type": "base64", "media_type": media_type, "data": data_or_url}
                                    })
                                }
                            }
                            CanonicalContentPart::Document {
                                media_type,
                                data_or_url,
                                name,
                            } => {
                                json!({
                                    "type": "document",
                                    "title": name,
                                    "source": {"type": "base64", "media_type": media_type, "data": data_or_url}
                                })
                            }
                            CanonicalContentPart::Audio { format, data } => {
                                json!({"type": "text", "text": format!("[audio/{format}:{data}]")})
                            }
                        })
                        .collect();
                    msgs.push(json!({
                        "role": if m.role == "assistant" { "assistant" } else { "user" },
                        "content": blocks,
                    }));
                } else {
                    msgs.push(json!({
                        "role": if m.role == "assistant" { "assistant" } else { "user" },
                        "content": m.content,
                    }));
                }
            }
            let mut out = json!({
                "model": model,
                "max_tokens": req.max_tokens.unwrap_or(4096),
                "messages": msgs,
                "stream": req.stream,
            });
            if let Some(ref sys) = req.system {
                out["system"] = Value::String(sys.clone());
            }
            if let Some(temp) = req.temperature {
                out["temperature"] = json!(temp);
            }
            if !req.tools.is_empty() {
                let tools: Vec<Value> = req
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": t.parameters,
                        })
                    })
                    .collect();
                out["tools"] = Value::Array(tools);
            }
            out
        }
        WireProtocol::GoogleGemini | WireProtocol::CloudCodeAssist => {
            let mut contents = Vec::new();
            for m in &req.messages {
                if m.role == "tool" {
                    let tool_name = m
                        .tool_call_id
                        .as_deref()
                        .and_then(|id| id.strip_prefix("call_"))
                        .unwrap_or_else(|| m.tool_call_id.as_deref().unwrap_or("tool"));
                    let resp_val = serde_json::from_str::<Value>(&m.content)
                        .ok()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({"content": m.content}));
                    contents.push(json!({
                        "role": "user",
                        "parts": [{
                            "functionResponse": {
                                "name": tool_name,
                                "response": resp_val,
                            }
                        }]
                    }));
                    continue;
                }
                let gemini_role = if m.role == "assistant" {
                    "model"
                } else {
                    "user"
                };
                let mut parts = Vec::new();
                if let Some(ref rc) = m.reasoning_content {
                    let mut tp = json!({"thought": true, "text": rc});
                    if let Some(ref sig) = m.reasoning_signature {
                        tp["thoughtSignature"] = Value::String(sig.clone());
                    }
                    parts.push(tp);
                }
                if !m.parts.is_empty() {
                    for p in &m.parts {
                        match p {
                            CanonicalContentPart::Text { text } => {
                                if !text.is_empty() {
                                    parts.push(json!({"text": text}));
                                }
                            }
                            CanonicalContentPart::Image {
                                media_type,
                                data_or_url,
                                is_url,
                            } => {
                                if *is_url {
                                    parts.push(json!({"fileData": {"mimeType": media_type, "fileUri": data_or_url}}));
                                } else {
                                    parts.push(json!({"inlineData": {"mimeType": media_type, "data": data_or_url}}));
                                }
                            }
                            CanonicalContentPart::Audio { format, data } => {
                                parts.push(json!({"inlineData": {"mimeType": format!("audio/{format}"), "data": data}}));
                            }
                            CanonicalContentPart::Document {
                                media_type,
                                data_or_url,
                                ..
                            } => {
                                parts.push(json!({"inlineData": {"mimeType": media_type, "data": data_or_url}}));
                            }
                        }
                    }
                } else if !m.content.is_empty() || m.tool_calls.is_empty() {
                    parts.push(json!({"text": m.content}));
                }
                for tc in &m.tool_calls {
                    let args_val = serde_json::from_str::<Value>(&tc.arguments_json)
                        .unwrap_or_else(|_| json!({}));
                    let mut fc_part = json!({
                        "functionCall": {
                            "name": tc.name,
                            "args": args_val,
                        }
                    });
                    if let Some(ref sig) = m.reasoning_signature {
                        fc_part["thoughtSignature"] = Value::String(sig.clone());
                    }
                    parts.push(fc_part);
                }
                contents.push(json!({
                    "role": gemini_role,
                    "parts": parts,
                }));
            }
            let mut inner = json!({
                "contents": contents,
            });
            if let Some(ref sys) = req.system {
                inner["systemInstruction"] = json!({
                    "parts": [{"text": sys}]
                });
            }
            if req.max_tokens.is_some() || req.temperature.is_some() {
                let mut gen_cfg = serde_json::Map::new();
                if let Some(max_tok) = req.max_tokens {
                    gen_cfg.insert("maxOutputTokens".to_string(), json!(max_tok));
                }
                if let Some(temp) = req.temperature {
                    gen_cfg.insert("temperature".to_string(), json!(temp));
                }
                inner["generationConfig"] = Value::Object(gen_cfg);
            }
            if !req.tools.is_empty() {
                let decls: Vec<Value> = req
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "parameters": sanitize_gemini_schema(&t.parameters),
                        })
                    })
                    .collect();
                inner["tools"] = json!([{"functionDeclarations": decls}]);
            }
            if target == WireProtocol::CloudCodeAssist {
                let project_id = req
                    .project
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("cloudaicompanion-default");
                json!({
                    "project": project_id,
                    "model": model,
                    "request": inner,
                    "requestType": "agent",
                    "userAgent": "antigravity",
                })
            } else {
                inner
            }
        }
        WireProtocol::BedrockConverseStream => {
            let mut messages = Vec::new();
            for m in &req.messages {
                if m.role == "tool" {
                    messages.push(json!({
                        "role": "user",
                        "content": [{
                            "toolResult": {
                                "toolUseId": m.tool_call_id.as_deref().unwrap_or("toolu_1"),
                                "content": [{"text": m.content}],
                            }
                        }]
                    }));
                    continue;
                }
                let role = if m.role == "assistant" {
                    "assistant"
                } else {
                    "user"
                };
                let mut blocks = Vec::new();
                if let Some(ref rc) = m.reasoning_content {
                    let mut rt = json!({"text": rc});
                    if let Some(ref sig) = m.reasoning_signature {
                        rt["signature"] = Value::String(sig.clone());
                    }
                    blocks.push(json!({"reasoningContent": {"reasoningText": rt}}));
                }
                if !m.content.is_empty() || (m.tool_calls.is_empty() && blocks.is_empty()) {
                    blocks.push(json!({"text": m.content}));
                }
                for tc in &m.tool_calls {
                    let args_val = serde_json::from_str::<Value>(&tc.arguments_json)
                        .unwrap_or_else(|_| json!({}));
                    blocks.push(json!({
                        "toolUse": {
                            "toolUseId": tc.id,
                            "name": tc.name,
                            "input": args_val,
                        }
                    }));
                }
                messages.push(json!({
                    "role": role,
                    "content": blocks,
                }));
            }
            let mut inf_cfg = json!({
                "maxTokens": req.max_tokens.unwrap_or(4096),
            });
            if let Some(temp) = req.temperature {
                inf_cfg["temperature"] = json!(temp);
            }
            let mut out = json!({
                "modelId": model,
                "messages": messages,
                "inferenceConfig": inf_cfg,
            });
            if let Some(ref sys) = req.system {
                out["system"] = json!([{"text": sys}]);
            }
            if !req.tools.is_empty() {
                let bedrock_tools: Vec<Value> = req
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "toolSpec": {
                                "name": t.name,
                                "description": t.description.clone().unwrap_or_default(),
                                "inputSchema": {
                                    "json": t.parameters,
                                }
                            }
                        })
                    })
                    .collect();
                out["toolConfig"] = json!({"tools": bedrock_tools});
            }
            out
        }
        WireProtocol::CommandCodeGenerate => {
            let msgs: Vec<Value> = req
                .messages
                .iter()
                .map(|m| {
                    let mut m_val = json!({"role": m.role, "content": m.content});
                    if let Some(ref tc_id) = m.tool_call_id {
                        m_val["tool_call_id"] = Value::String(tc_id.clone());
                    }
                    if !m.tool_calls.is_empty() {
                        let tcs: Vec<Value> = m
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
                        m_val["tool_calls"] = Value::Array(tcs);
                    }
                    m_val
                })
                .collect();
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect();
            let eff_thread = req
                .thread_id
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("thr_llmroller");
            let mut params = json!({
                "messages": msgs,
                "system": req.system.clone().unwrap_or_default(),
                "tools": tools,
            });
            if let Some(max_tok) = req.max_tokens {
                params["max_tokens"] = json!(max_tok);
            }
            if let Some(temp) = req.temperature {
                params["temperature"] = json!(temp);
            }
            json!({
                "threadId": eff_thread,
                "memory": "",
                "config": {
                    "model": model,
                },
                "params": params,
            })
        }
    };

    serde_json::to_vec(&doc).map_err(|e| e.to_string())
}

/// Sanitize a JSON Schema for Google Gemini / Cloud Code Assist (`F176`, SRDD Section 23.3).
#[must_use]
pub fn sanitize_gemini_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if matches!(
                    k.as_str(),
                    "$schema"
                        | "$ref"
                        | "$defs"
                        | "definitions"
                        | "additionalProperties"
                        | "minimum"
                        | "maximum"
                        | "exclusiveMinimum"
                        | "exclusiveMaximum"
                ) {
                    continue;
                }
                if k == "const" {
                    out.insert("enum".to_string(), json!([v]));
                    continue;
                }
                out.insert(k.clone(), sanitize_gemini_schema(v));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sanitize_gemini_schema).collect()),
        other => other.clone(),
    }
}

/// Transcode a JSON request payload from `source` protocol to `target` protocol (`F099`).
pub fn transcode_request(
    source: WireProtocol,
    target: WireProtocol,
    body: &[u8],
    target_model: &str,
) -> Result<Vec<u8>, String> {
    let req = parse_canonical_request(source, body)?;
    encode_canonical_request(target, &req, target_model)
}
