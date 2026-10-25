use serde_json::{Value, json};

use super::types::{
    CanonicalMessage, CanonicalRequest, CanonicalTool, CanonicalToolCall, WireProtocol,
    extract_text_from_value,
};

/// Parse an inbound request body from `source` protocol into [`CanonicalRequest`] (`F099`).
pub fn parse_canonical_request(
    source: WireProtocol,
    body: &[u8],
) -> Result<CanonicalRequest, String> {
    let val: Value = serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let obj = val
        .as_object()
        .ok_or_else(|| "request body must be a JSON object".to_string())?;

    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    let stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let max_tokens = obj
        .get("max_completion_tokens")
        .or_else(|| obj.get("max_tokens"))
        .or_else(|| obj.get("max_output_tokens"))
        .and_then(Value::as_i64);
    let temperature = obj.get("temperature").and_then(Value::as_f64);

    let mut system_parts = Vec::new();
    let mut messages = Vec::new();
    let mut tools = Vec::new();

    match source {
        WireProtocol::ChatCompletions => {
            if let Some(arr) = obj.get("messages").and_then(Value::as_array) {
                for m in arr {
                    let Some(m_obj) = m.as_object() else {
                        continue;
                    };
                    let role = m_obj
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("user")
                        .to_string();
                    let content = m_obj
                        .get("content")
                        .map(extract_text_from_value)
                        .unwrap_or_default();
                    if matches!(role.as_str(), "system" | "developer") {
                        if !content.is_empty() {
                            system_parts.push(content);
                        }
                        continue;
                    }
                    let reasoning_content = m_obj
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let tool_call_id = m_obj
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let mut tool_calls = Vec::new();
                    if let Some(tcs) = m_obj.get("tool_calls").and_then(Value::as_array) {
                        for tc in tcs {
                            let id = tc
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("call_1")
                                .to_string();
                            let func = tc.get("function");
                            let name = func
                                .and_then(|f| f.get("name"))
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let arguments_json = func
                                .and_then(|f| f.get("arguments"))
                                .and_then(Value::as_str)
                                .unwrap_or("{}")
                                .to_string();
                            tool_calls.push(CanonicalToolCall {
                                id,
                                name,
                                arguments_json,
                            });
                        }
                    }
                    messages.push(CanonicalMessage {
                        role,
                        content,
                        reasoning_content,
                        tool_calls,
                        tool_call_id,
                    });
                }
            }
            if let Some(t_arr) = obj.get("tools").and_then(Value::as_array) {
                for t in t_arr {
                    let func = t.get("function").unwrap_or(t);
                    if let Some(name) = func.get("name").and_then(Value::as_str) {
                        tools.push(CanonicalTool {
                            name: name.to_string(),
                            description: func
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            parameters: func
                                .get("parameters")
                                .cloned()
                                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                        });
                    }
                }
            }
        }
        WireProtocol::Responses => {
            if let Some(inst) = obj.get("instructions").and_then(Value::as_str)
                && !inst.is_empty()
            {
                system_parts.push(inst.to_string());
            }
            match obj.get("input") {
                Some(Value::String(s)) => {
                    messages.push(CanonicalMessage {
                        role: "user".to_string(),
                        content: s.clone(),
                        reasoning_content: None,
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    });
                }
                Some(Value::Array(items)) => {
                    for item in items {
                        let Some(i_obj) = item.as_object() else {
                            continue;
                        };
                        let item_type = i_obj.get("type").and_then(Value::as_str).unwrap_or("");
                        if item_type == "function_call" {
                            messages.push(CanonicalMessage {
                                role: "assistant".to_string(),
                                content: String::new(),
                                reasoning_content: None,
                                tool_calls: vec![CanonicalToolCall {
                                    id: i_obj
                                        .get("call_id")
                                        .or_else(|| i_obj.get("id"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("call_1")
                                        .to_string(),
                                    name: i_obj
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("tool")
                                        .to_string(),
                                    arguments_json: i_obj
                                        .get("arguments")
                                        .and_then(Value::as_str)
                                        .unwrap_or("{}")
                                        .to_string(),
                                }],
                                tool_call_id: None,
                            });
                            continue;
                        }
                        if item_type == "function_call_output" {
                            messages.push(CanonicalMessage {
                                role: "tool".to_string(),
                                content: i_obj
                                    .get("output")
                                    .map(extract_text_from_value)
                                    .unwrap_or_default(),
                                reasoning_content: None,
                                tool_calls: Vec::new(),
                                tool_call_id: i_obj
                                    .get("call_id")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            });
                            continue;
                        }
                        let role = i_obj
                            .get("role")
                            .and_then(Value::as_str)
                            .unwrap_or("user")
                            .to_string();
                        let content = i_obj
                            .get("content")
                            .map(extract_text_from_value)
                            .unwrap_or_default();
                        if matches!(role.as_str(), "system" | "developer") {
                            if !content.is_empty() {
                                system_parts.push(content);
                            }
                        } else {
                            messages.push(CanonicalMessage {
                                role,
                                content,
                                reasoning_content: None,
                                tool_calls: Vec::new(),
                                tool_call_id: None,
                            });
                        }
                    }
                }
                _ => {}
            }
            if let Some(t_arr) = obj.get("tools").and_then(Value::as_array) {
                for t in t_arr {
                    let func = t.get("function").unwrap_or(t);
                    if let Some(name) = func.get("name").and_then(Value::as_str) {
                        tools.push(CanonicalTool {
                            name: name.to_string(),
                            description: func
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            parameters: func
                                .get("parameters")
                                .cloned()
                                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                        });
                    }
                }
            }
        }
        WireProtocol::Messages
        | WireProtocol::GoogleGemini
        | WireProtocol::CloudCodeAssist
        | WireProtocol::BedrockConverseStream
        | WireProtocol::CommandCodeGenerate => {
            if let Some(sys) = obj.get("system") {
                let s = extract_text_from_value(sys);
                if !s.is_empty() {
                    system_parts.push(s);
                }
            }
            if let Some(arr) = obj.get("messages").and_then(Value::as_array) {
                for m in arr {
                    let Some(m_obj) = m.as_object() else {
                        continue;
                    };
                    let role = m_obj
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("user")
                        .to_string();
                    let mut text_parts = Vec::new();
                    let mut reasoning_parts = Vec::new();
                    let mut tool_calls = Vec::new();
                    let mut tool_call_id = None;

                    match m_obj.get("content") {
                        Some(Value::String(s)) => text_parts.push(s.clone()),
                        Some(Value::Array(blocks)) => {
                            for b in blocks {
                                let Some(b_obj) = b.as_object() else {
                                    continue;
                                };
                                match b_obj.get("type").and_then(Value::as_str).unwrap_or("text") {
                                    "text" => {
                                        if let Some(t) = b_obj.get("text").and_then(Value::as_str) {
                                            text_parts.push(t.to_string());
                                        }
                                    }
                                    "thinking" => {
                                        if let Some(t) =
                                            b_obj.get("thinking").and_then(Value::as_str)
                                        {
                                            reasoning_parts.push(t.to_string());
                                        }
                                    }
                                    "tool_use" => {
                                        let id = b_obj
                                            .get("id")
                                            .and_then(Value::as_str)
                                            .unwrap_or("toolu_1")
                                            .to_string();
                                        let name = b_obj
                                            .get("name")
                                            .and_then(Value::as_str)
                                            .unwrap_or("tool")
                                            .to_string();
                                        let args = b_obj
                                            .get("input")
                                            .map(|v| v.to_string())
                                            .unwrap_or_else(|| "{}".to_string());
                                        tool_calls.push(CanonicalToolCall {
                                            id,
                                            name,
                                            arguments_json: args,
                                        });
                                    }
                                    "tool_result" => {
                                        tool_call_id = b_obj
                                            .get("tool_use_id")
                                            .and_then(Value::as_str)
                                            .map(str::to_string);
                                        if let Some(c) = b_obj.get("content") {
                                            text_parts.push(extract_text_from_value(c));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                    let actual_role = if tool_call_id.is_some() {
                        "tool".to_string()
                    } else {
                        role
                    };
                    messages.push(CanonicalMessage {
                        role: actual_role,
                        content: text_parts.join(""),
                        reasoning_content: if reasoning_parts.is_empty() {
                            None
                        } else {
                            Some(reasoning_parts.join(""))
                        },
                        tool_calls,
                        tool_call_id,
                    });
                }
            }
            if let Some(t_arr) = obj.get("tools").and_then(Value::as_array) {
                for t in t_arr {
                    if let Some(name) = t.get("name").and_then(Value::as_str) {
                        tools.push(CanonicalTool {
                            name: name.to_string(),
                            description: t
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            parameters: t
                                .get("input_schema")
                                .or_else(|| t.get("parameters"))
                                .cloned()
                                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                        });
                    }
                }
            }
        }
    }

    Ok(CanonicalRequest {
        model,
        system: if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        },
        messages,
        tools,
        stream,
        max_tokens,
        temperature,
    })
}

/// Encode a [`CanonicalRequest`] into the target upstream wire format (`F099`).
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
                m_obj.insert("content".to_string(), Value::String(m.content.clone()));
                if let Some(ref rc) = m.reasoning_content {
                    m_obj.insert("reasoning_content".to_string(), Value::String(rc.clone()));
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
                } else if !m.tool_calls.is_empty() {
                    let mut blocks = Vec::new();
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
                let gemini_role = if m.role == "assistant" {
                    "model"
                } else {
                    "user"
                };
                contents.push(json!({
                    "role": gemini_role,
                    "parts": [{"text": m.content}],
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
                json!({
                    "project": "cloudaicompanion-default",
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
                let role = if m.role == "assistant" {
                    "assistant"
                } else {
                    "user"
                };
                messages.push(json!({
                    "role": role,
                    "content": [{"text": m.content}],
                }));
            }
            let mut out = json!({
                "modelId": model,
                "messages": messages,
                "inferenceConfig": {
                    "maxTokens": req.max_tokens.unwrap_or(4096),
                }
            });
            if let Some(ref sys) = req.system {
                out["system"] = json!([{"text": sys}]);
            }
            out
        }
        WireProtocol::CommandCodeGenerate => {
            let msgs: Vec<Value> = req
                .messages
                .iter()
                .map(|m| json!({"role": m.role, "content": m.content}))
                .collect();
            json!({
                "threadId": "thr_llmroller",
                "memory": "",
                "config": {
                    "model": model,
                },
                "params": {
                    "messages": msgs,
                    "system": req.system.clone().unwrap_or_default(),
                }
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
