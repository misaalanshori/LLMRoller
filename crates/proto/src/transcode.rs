#![forbid(unsafe_code)]
//! Bidirectional cross-protocol request, response, and SSE stream transcoder (`F099`, SRDD Section 24.1).
//!
//! Spec: ./docs/srdd/24. SRDD - Cross-Protocol Transcoding and OpenAI Client Normalization.md#241-bidirectional-protocol-transcoding-bridge-cratesprotosrctranscode
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#123-surgical-json-body-validation-and-edits

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Supported wire protocols for downstream ingress and upstream egress (`F099`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    ChatCompletions,
    Responses,
    Messages,
    GoogleGemini,
    CloudCodeAssist,
    BedrockConverseStream,
    CommandCodeGenerate,
}

impl WireProtocol {
    /// Parse a catalog protocol identifier (`"chat"`, `"responses"`, `"messages"`, etc.).
    #[must_use]
    pub fn from_catalog_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "chat" | "chat_completions" | "openai_chat" => Some(Self::ChatCompletions),
            "responses" | "openai_responses" => Some(Self::Responses),
            "messages" | "anthropic" | "anthropic_messages" => Some(Self::Messages),
            "gemini" | "google_gemini" => Some(Self::GoogleGemini),
            "cloudcode" | "cloud_code_assist" | "antigravity" => Some(Self::CloudCodeAssist),
            "bedrock" | "bedrock_converse" => Some(Self::BedrockConverseStream),
            "command_code_generate" | "alpha_generate" => Some(Self::CommandCodeGenerate),
            _ => None,
        }
    }

    /// Canonical route protocol string (`"chat"`, `"responses"`, `"messages"`).
    #[must_use]
    pub const fn as_catalog_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
            Self::GoogleGemini => "gemini",
            Self::CloudCodeAssist => "cloudcode",
            Self::BedrockConverseStream => "bedrock",
            Self::CommandCodeGenerate => "command_code_generate",
        }
    }
}

/// Canonical normalized tool call representation (`F099`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

/// Canonical normalized message representation (`F099`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<CanonicalToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Canonical normalized tool definition (`F099`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
}

/// Canonical normalized request across all wire formats (`F099`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<CanonicalMessage>,
    pub tools: Vec<CanonicalTool>,
    pub stream: bool,
    pub max_tokens: Option<i64>,
    pub temperature: Option<f64>,
}

/// Canonical normalized response across all wire formats (`F099`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalResponse {
    pub id: String,
    pub model: String,
    pub text: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<CanonicalToolCall>,
    pub finish_reason: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

fn extract_text_from_value(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(s) = item.as_str() {
                    parts.push(s.to_string());
                } else if let Some(obj) = item.as_object() {
                    if let Some(t) = obj.get("text").and_then(Value::as_str) {
                        parts.push(t.to_string());
                    } else if let Some(c) = obj.get("content").and_then(Value::as_str) {
                        parts.push(c.to_string());
                    }
                }
            }
            parts.join("")
        }
        Value::Object(obj) => obj
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

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
                        finish_reason = "tool_calls".to_string();
                    }
                }
            }
            if let Some(u) = val.get("usage") {
                input_tokens = u.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
                output_tokens = u.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);
            }
        }
        WireProtocol::Messages => {
            if let Some(sr) = val.get("stop_reason").and_then(Value::as_str) {
                finish_reason = if sr == "tool_use" {
                    "tool_calls".to_string()
                } else {
                    "stop".to_string()
                };
            }
            if let Some(blocks) = val.get("content").and_then(Value::as_array) {
                for b in blocks {
                    match b.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            if let Some(t) = b.get("text").and_then(Value::as_str) {
                                text.push_str(t);
                            }
                        }
                        "thinking" => {
                            if let Some(t) = b.get("thinking").and_then(Value::as_str) {
                                reasoning = Some(t.to_string());
                            }
                        }
                        "tool_use" => {
                            tool_calls.push(CanonicalToolCall {
                                id: b
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("call_1")
                                    .to_string(),
                                name: b
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("tool")
                                    .to_string(),
                                arguments_json: b
                                    .get("input")
                                    .map(Value::to_string)
                                    .unwrap_or_else(|| "{}".to_string()),
                            });
                        }
                        _ => {}
                    }
                }
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
                && let Some(parts) = cand.pointer("/content/parts").and_then(Value::as_array)
            {
                for p in parts {
                    if p.get("thought").and_then(Value::as_bool) == Some(true) {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            reasoning = Some(t.to_string());
                        }
                    } else if let Some(t) = p.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    } else if let Some(fc) = p.get("functionCall") {
                        tool_calls.push(CanonicalToolCall {
                            id: fc
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("call_gemini_1")
                                .to_string(),
                            name: fc
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string(),
                            arguments_json: fc
                                .get("args")
                                .map(Value::to_string)
                                .unwrap_or_else(|| "{}".to_string()),
                        });
                    }
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
        {
            if !d.is_empty() {
                deltas.push(d.to_string());
            }
        } else if let Some(d) = val
            .pointer("/candidates/0/content/parts/0/text")
            .or_else(|| val.pointer("/response/candidates/0/content/parts/0/text"))
            .and_then(Value::as_str)
        {
            if !d.is_empty() {
                deltas.push(d.to_string());
            }
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
            out.push_str(&format!(
                "event: response.completed\ndata: {completed}\n\n"
            ));
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
