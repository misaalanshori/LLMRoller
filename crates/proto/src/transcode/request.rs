use serde_json::{Value, json};

use super::types::{
    CanonicalContentPart, CanonicalMessage, CanonicalRequest, CanonicalTool, CanonicalToolCall,
    WireProtocol, extract_text_from_value,
};

fn parse_multimodal_parts_openai(content: &Value) -> Vec<CanonicalContentPart> {
    let mut parts = Vec::new();
    match content {
        Value::String(s) if !s.is_empty() => {
            parts.push(CanonicalContentPart::Text { text: s.clone() });
        }
        Value::Array(arr) => {
            for item in arr {
                let Some(obj) = item.as_object() else {
                    continue;
                };
                match obj.get("type").and_then(Value::as_str).unwrap_or("text") {
                    "text" | "input_text" | "output_text" => {
                        if let Some(t) = obj.get("text").and_then(Value::as_str) {
                            parts.push(CanonicalContentPart::Text {
                                text: t.to_string(),
                            });
                        }
                    }
                    "image_url" | "input_image" => {
                        let url_str = obj
                            .get("image_url")
                            .and_then(|v| v.get("url").or(Some(v)))
                            .and_then(Value::as_str)
                            .or_else(|| obj.get("url").and_then(Value::as_str))
                            .unwrap_or("");
                        if !url_str.is_empty() {
                            if let Some(rest) = url_str.strip_prefix("data:")
                                && let Some((mime, b64)) = rest.split_once(";base64,")
                            {
                                parts.push(CanonicalContentPart::Image {
                                    media_type: mime.to_string(),
                                    data_or_url: b64.to_string(),
                                    is_url: false,
                                });
                            } else {
                                parts.push(CanonicalContentPart::Image {
                                    media_type: "image/png".to_string(),
                                    data_or_url: url_str.to_string(),
                                    is_url: true,
                                });
                            }
                        }
                    }
                    "input_audio" | "audio" => {
                        if let Some(aud) = obj.get("input_audio").or_else(|| obj.get("audio")) {
                            let data = aud
                                .get("data")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let format = aud
                                .get("format")
                                .and_then(Value::as_str)
                                .unwrap_or("wav")
                                .to_string();
                            if !data.is_empty() {
                                parts.push(CanonicalContentPart::Audio { format, data });
                            }
                        }
                    }
                    "file" | "input_file" | "document" => {
                        let data = obj
                            .get("file")
                            .and_then(|f| f.get("file_data"))
                            .or_else(|| obj.get("file_data"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let name = obj
                            .get("file")
                            .and_then(|f| f.get("filename"))
                            .or_else(|| obj.get("filename"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        if !data.is_empty() {
                            parts.push(CanonicalContentPart::Document {
                                media_type: "application/pdf".to_string(),
                                data_or_url: data,
                                name,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    parts
}

/// Parse an inbound request body from `source` protocol into [`CanonicalRequest`] (`F099`, `FND-010`).
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
        .or_else(|| obj.get("modelId"))
        .and_then(Value::as_str)
        .or_else(|| val.pointer("/config/model").and_then(Value::as_str))
        .or_else(|| val.pointer("/request/model").and_then(Value::as_str))
        .unwrap_or("default")
        .to_string();
    let stream = obj
        .get("stream")
        .and_then(Value::as_bool)
        .or_else(|| val.pointer("/request/stream").and_then(Value::as_bool))
        .or_else(|| val.pointer("/params/stream").and_then(Value::as_bool))
        .unwrap_or(false);
    let max_tokens = obj
        .get("max_completion_tokens")
        .or_else(|| obj.get("max_tokens"))
        .or_else(|| obj.get("max_output_tokens"))
        .or_else(|| val.pointer("/generationConfig/maxOutputTokens"))
        .or_else(|| val.pointer("/request/generationConfig/maxOutputTokens"))
        .or_else(|| val.pointer("/inferenceConfig/maxTokens"))
        .or_else(|| val.pointer("/params/max_tokens"))
        .and_then(Value::as_i64);
    let temperature = obj
        .get("temperature")
        .or_else(|| val.pointer("/generationConfig/temperature"))
        .or_else(|| val.pointer("/request/generationConfig/temperature"))
        .or_else(|| val.pointer("/inferenceConfig/temperature"))
        .or_else(|| val.pointer("/params/temperature"))
        .and_then(Value::as_f64);
    let project = obj
        .get("project")
        .and_then(Value::as_str)
        .map(str::to_string);
    let thread_id = obj
        .get("threadId")
        .or_else(|| obj.get("thread_id"))
        .or_else(|| val.pointer("/metadata/session_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let tool_choice = obj
        .get("tool_choice")
        .or_else(|| val.pointer("/toolConfig/toolChoice"))
        .or_else(|| val.pointer("/toolConfig/functionCallingConfig"))
        .or_else(|| val.pointer("/request/toolConfig/functionCallingConfig"))
        .cloned();

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
                    let parts = m_obj
                        .get("content")
                        .map(parse_multimodal_parts_openai)
                        .unwrap_or_default();
                    let reasoning_content = m_obj
                        .get("reasoning_content")
                        .or_else(|| m_obj.get("reasoning"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let reasoning_signature = m_obj
                        .get("reasoning_opaque")
                        .or_else(|| m_obj.get("thought_signature"))
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
                        parts,
                        reasoning_content,
                        reasoning_signature,
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
                        parts: vec![CanonicalContentPart::Text { text: s.clone() }],
                        reasoning_content: None,
                        reasoning_signature: None,
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
                                parts: Vec::new(),
                                reasoning_content: None,
                                reasoning_signature: None,
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
                            let content_str = i_obj
                                .get("output")
                                .map(extract_text_from_value)
                                .unwrap_or_default();
                            messages.push(CanonicalMessage {
                                role: "tool".to_string(),
                                content: content_str.clone(),
                                parts: if content_str.is_empty() {
                                    Vec::new()
                                } else {
                                    vec![CanonicalContentPart::Text { text: content_str }]
                                },
                                reasoning_content: None,
                                reasoning_signature: None,
                                tool_calls: Vec::new(),
                                tool_call_id: i_obj
                                    .get("call_id")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            });
                            continue;
                        }
                        if item_type == "reasoning" {
                            let summary = i_obj
                                .get("summary")
                                .map(extract_text_from_value)
                                .unwrap_or_default();
                            let encrypted = i_obj
                                .get("encrypted_content")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            messages.push(CanonicalMessage {
                                role: "assistant".to_string(),
                                content: String::new(),
                                parts: Vec::new(),
                                reasoning_content: if summary.is_empty() {
                                    None
                                } else {
                                    Some(summary)
                                },
                                reasoning_signature: encrypted,
                                tool_calls: Vec::new(),
                                tool_call_id: None,
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
                        let parts = i_obj
                            .get("content")
                            .map(parse_multimodal_parts_openai)
                            .unwrap_or_default();
                        if matches!(role.as_str(), "system" | "developer") {
                            if !content.is_empty() {
                                system_parts.push(content);
                            }
                        } else {
                            messages.push(CanonicalMessage {
                                role,
                                content,
                                parts,
                                reasoning_content: None,
                                reasoning_signature: None,
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
        WireProtocol::Messages => {
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
                    let mut parts = Vec::new();
                    let mut reasoning_parts = Vec::new();
                    let mut reasoning_signature = None;
                    let mut tool_calls = Vec::new();
                    let mut tool_call_id = None;

                    match m_obj.get("content") {
                        Some(Value::String(s)) => {
                            text_parts.push(s.clone());
                            parts.push(CanonicalContentPart::Text { text: s.clone() });
                        }
                        Some(Value::Array(blocks)) => {
                            for b in blocks {
                                let Some(b_obj) = b.as_object() else {
                                    continue;
                                };
                                match b_obj.get("type").and_then(Value::as_str).unwrap_or("text") {
                                    "text" => {
                                        if let Some(t) = b_obj.get("text").and_then(Value::as_str) {
                                            text_parts.push(t.to_string());
                                            parts.push(CanonicalContentPart::Text {
                                                text: t.to_string(),
                                            });
                                        }
                                    }
                                    "image" => {
                                        if let Some(src) = b_obj.get("source") {
                                            let stype = src
                                                .get("type")
                                                .and_then(Value::as_str)
                                                .unwrap_or("base64");
                                            let media_type = src
                                                .get("media_type")
                                                .and_then(Value::as_str)
                                                .unwrap_or("image/png")
                                                .to_string();
                                            let data = src
                                                .get("data")
                                                .or_else(|| src.get("url"))
                                                .and_then(Value::as_str)
                                                .unwrap_or("")
                                                .to_string();
                                            if !data.is_empty() {
                                                parts.push(CanonicalContentPart::Image {
                                                    media_type,
                                                    data_or_url: data,
                                                    is_url: stype == "url",
                                                });
                                            }
                                        }
                                    }
                                    "document" => {
                                        if let Some(src) = b_obj.get("source") {
                                            let media_type = src
                                                .get("media_type")
                                                .and_then(Value::as_str)
                                                .unwrap_or("application/pdf")
                                                .to_string();
                                            let data = src
                                                .get("data")
                                                .or_else(|| src.get("url"))
                                                .and_then(Value::as_str)
                                                .unwrap_or("")
                                                .to_string();
                                            let name = b_obj
                                                .get("title")
                                                .and_then(Value::as_str)
                                                .map(str::to_string);
                                            if !data.is_empty() {
                                                parts.push(CanonicalContentPart::Document {
                                                    media_type,
                                                    data_or_url: data,
                                                    name,
                                                });
                                            }
                                        }
                                    }
                                    "thinking" => {
                                        if let Some(t) =
                                            b_obj.get("thinking").and_then(Value::as_str)
                                        {
                                            reasoning_parts.push(t.to_string());
                                        }
                                        if let Some(sig) =
                                            b_obj.get("signature").and_then(Value::as_str)
                                        {
                                            reasoning_signature = Some(sig.to_string());
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
                                            let t = extract_text_from_value(c);
                                            text_parts.push(t.clone());
                                            parts.push(CanonicalContentPart::Text { text: t });
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
                        parts,
                        reasoning_content: if reasoning_parts.is_empty() {
                            None
                        } else {
                            Some(reasoning_parts.join(""))
                        },
                        reasoning_signature,
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
        WireProtocol::GoogleGemini | WireProtocol::CloudCodeAssist => {
            let root = val.get("request").and_then(Value::as_object).unwrap_or(obj);
            if let Some(sys_inst) = root
                .get("systemInstruction")
                .or_else(|| root.get("system_instruction"))
            {
                if let Some(parts_arr) = sys_inst.get("parts").and_then(Value::as_array) {
                    for p in parts_arr {
                        if let Some(t) = p.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            system_parts.push(t.to_string());
                        }
                    }
                } else {
                    let s = extract_text_from_value(sys_inst);
                    if !s.is_empty() {
                        system_parts.push(s);
                    }
                }
            }
            if let Some(contents) = root.get("contents").and_then(Value::as_array) {
                for c_item in contents {
                    let Some(c_obj) = c_item.as_object() else {
                        continue;
                    };
                    let raw_role = c_obj.get("role").and_then(Value::as_str).unwrap_or("user");
                    let mut role = if raw_role == "model" {
                        "assistant".to_string()
                    } else {
                        "user".to_string()
                    };
                    let mut text_parts = Vec::new();
                    let mut parts = Vec::new();
                    let mut reasoning_parts = Vec::new();
                    let mut reasoning_signature = None;
                    let mut tool_calls = Vec::new();
                    let mut tool_call_id = None;

                    if let Some(p_arr) = c_obj.get("parts").and_then(Value::as_array) {
                        for p in p_arr {
                            let Some(p_obj) = p.as_object() else {
                                continue;
                            };
                            let is_thought = p_obj
                                .get("thought")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            if let Some(sig) = p_obj
                                .get("thoughtSignature")
                                .or_else(|| p_obj.get("thought_signature"))
                                .and_then(Value::as_str)
                            {
                                reasoning_signature = Some(sig.to_string());
                            }
                            if let Some(t) = p_obj.get("text").and_then(Value::as_str) {
                                if is_thought {
                                    reasoning_parts.push(t.to_string());
                                } else {
                                    text_parts.push(t.to_string());
                                    parts.push(CanonicalContentPart::Text {
                                        text: t.to_string(),
                                    });
                                }
                            } else if let Some(inline) =
                                p_obj.get("inlineData").or_else(|| p_obj.get("inline_data"))
                            {
                                let mime = inline
                                    .get("mimeType")
                                    .or_else(|| inline.get("mime_type"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("image/png");
                                let data = inline.get("data").and_then(Value::as_str).unwrap_or("");
                                if !data.is_empty() {
                                    if mime.starts_with("audio/") {
                                        parts.push(CanonicalContentPart::Audio {
                                            format: mime.trim_start_matches("audio/").to_string(),
                                            data: data.to_string(),
                                        });
                                    } else if mime == "application/pdf" {
                                        parts.push(CanonicalContentPart::Document {
                                            media_type: mime.to_string(),
                                            data_or_url: data.to_string(),
                                            name: None,
                                        });
                                    } else {
                                        parts.push(CanonicalContentPart::Image {
                                            media_type: mime.to_string(),
                                            data_or_url: data.to_string(),
                                            is_url: false,
                                        });
                                    }
                                }
                            } else if let Some(file_data) =
                                p_obj.get("fileData").or_else(|| p_obj.get("file_data"))
                            {
                                let mime = file_data
                                    .get("mimeType")
                                    .or_else(|| file_data.get("mime_type"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("application/pdf");
                                let uri = file_data
                                    .get("fileUri")
                                    .or_else(|| file_data.get("file_uri"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                if !uri.is_empty() {
                                    parts.push(CanonicalContentPart::Document {
                                        media_type: mime.to_string(),
                                        data_or_url: uri.to_string(),
                                        name: None,
                                    });
                                }
                            } else if let Some(fc) = p_obj.get("functionCall") {
                                let name = fc
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("tool")
                                    .to_string();
                                let id = fc
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                    .unwrap_or_else(|| format!("call_{name}"));
                                let args = fc
                                    .get("args")
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "{}".to_string());
                                tool_calls.push(CanonicalToolCall {
                                    id,
                                    name,
                                    arguments_json: args,
                                });
                            } else if let Some(fr) = p_obj.get("functionResponse") {
                                role = "tool".to_string();
                                let name = fr.get("name").and_then(Value::as_str).unwrap_or("tool");
                                tool_call_id = Some(
                                    fr.get("id")
                                        .and_then(Value::as_str)
                                        .map(str::to_string)
                                        .unwrap_or_else(|| format!("call_{name}")),
                                );
                                if let Some(resp_obj) = fr.get("response") {
                                    let t = if let Some(c) = resp_obj.get("content") {
                                        extract_text_from_value(c)
                                    } else if let Some(r) = resp_obj.get("result") {
                                        extract_text_from_value(r)
                                    } else {
                                        resp_obj.to_string()
                                    };
                                    text_parts.push(t.clone());
                                    parts.push(CanonicalContentPart::Text { text: t });
                                }
                            }
                        }
                    }
                    messages.push(CanonicalMessage {
                        role,
                        content: text_parts.join(""),
                        parts,
                        reasoning_content: if reasoning_parts.is_empty() {
                            None
                        } else {
                            Some(reasoning_parts.join(""))
                        },
                        reasoning_signature,
                        tool_calls,
                        tool_call_id,
                    });
                }
            }
            if let Some(t_arr) = root.get("tools").and_then(Value::as_array) {
                for t in t_arr {
                    if let Some(decls) = t.get("functionDeclarations").and_then(Value::as_array) {
                        for d in decls {
                            if let Some(name) = d.get("name").and_then(Value::as_str) {
                                tools.push(CanonicalTool {
                                    name: name.to_string(),
                                    description: d
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .map(str::to_string),
                                    parameters: d.get("parameters").cloned().unwrap_or_else(
                                        || json!({"type": "object", "properties": {}}),
                                    ),
                                });
                            }
                        }
                    } else if let Some(name) = t.get("name").and_then(Value::as_str) {
                        tools.push(CanonicalTool {
                            name: name.to_string(),
                            description: t
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            parameters: t
                                .get("parameters")
                                .or_else(|| t.get("input_schema"))
                                .cloned()
                                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                        });
                    }
                }
            }
        }
        WireProtocol::BedrockConverseStream => {
            if let Some(sys_arr) = obj.get("system").and_then(Value::as_array) {
                for item in sys_arr {
                    if let Some(t) = item.get("text").and_then(Value::as_str)
                        && !t.is_empty()
                    {
                        system_parts.push(t.to_string());
                    }
                }
            } else if let Some(sys) = obj.get("system") {
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
                    let mut role = m_obj
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("user")
                        .to_string();
                    let mut text_parts = Vec::new();
                    let mut parts = Vec::new();
                    let mut reasoning_parts = Vec::new();
                    let mut reasoning_signature = None;
                    let mut tool_calls = Vec::new();
                    let mut tool_call_id = None;

                    if let Some(blocks) = m_obj.get("content").and_then(Value::as_array) {
                        for b in blocks {
                            let Some(b_obj) = b.as_object() else {
                                continue;
                            };
                            if let Some(t) = b_obj.get("text").and_then(Value::as_str) {
                                text_parts.push(t.to_string());
                                parts.push(CanonicalContentPart::Text {
                                    text: t.to_string(),
                                });
                            } else if let Some(rc) = b_obj
                                .get("reasoningContent")
                                .and_then(|r| r.get("reasoningText"))
                            {
                                if let Some(t) = rc.get("text").and_then(Value::as_str) {
                                    reasoning_parts.push(t.to_string());
                                }
                                if let Some(sig) = rc.get("signature").and_then(Value::as_str) {
                                    reasoning_signature = Some(sig.to_string());
                                }
                            } else if let Some(tu) = b_obj.get("toolUse") {
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
                            } else if let Some(tr) = b_obj.get("toolResult") {
                                role = "tool".to_string();
                                tool_call_id = tr
                                    .get("toolUseId")
                                    .and_then(Value::as_str)
                                    .map(str::to_string);
                                if let Some(c_arr) = tr.get("content").and_then(Value::as_array) {
                                    for c_item in c_arr {
                                        if let Some(t) = c_item.get("text").and_then(Value::as_str)
                                        {
                                            text_parts.push(t.to_string());
                                            parts.push(CanonicalContentPart::Text {
                                                text: t.to_string(),
                                            });
                                        }
                                    }
                                }
                            } else if let Some(img) = b_obj.get("image") {
                                let fmt =
                                    img.get("format").and_then(Value::as_str).unwrap_or("png");
                                let bytes_str = img
                                    .pointer("/source/bytes")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                if !bytes_str.is_empty() {
                                    parts.push(CanonicalContentPart::Image {
                                        media_type: format!("image/{fmt}"),
                                        data_or_url: bytes_str.to_string(),
                                        is_url: false,
                                    });
                                }
                            }
                        }
                    }
                    messages.push(CanonicalMessage {
                        role,
                        content: text_parts.join(""),
                        parts,
                        reasoning_content: if reasoning_parts.is_empty() {
                            None
                        } else {
                            Some(reasoning_parts.join(""))
                        },
                        reasoning_signature,
                        tool_calls,
                        tool_call_id,
                    });
                }
            }
            if let Some(t_arr) = val
                .pointer("/toolConfig/tools")
                .or_else(|| obj.get("tools"))
                .and_then(Value::as_array)
            {
                for t in t_arr {
                    let spec = t.get("toolSpec").unwrap_or(t);
                    if let Some(name) = spec.get("name").and_then(Value::as_str) {
                        tools.push(CanonicalTool {
                            name: name.to_string(),
                            description: spec
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            parameters: spec
                                .pointer("/inputSchema/json")
                                .or_else(|| spec.get("parameters"))
                                .cloned()
                                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                        });
                    }
                }
            }
        }
        WireProtocol::CommandCodeGenerate => {
            let params = val.get("params").and_then(Value::as_object).unwrap_or(obj);
            if let Some(sys) = params.get("system") {
                let s = extract_text_from_value(sys);
                if !s.is_empty() {
                    system_parts.push(s);
                }
            }
            if let Some(arr) = params.get("messages").and_then(Value::as_array) {
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
                    let mut tool_calls = Vec::new();
                    if let Some(tcs) = m_obj.get("tool_calls").and_then(Value::as_array) {
                        for tc in tcs {
                            let id = tc
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("call_1")
                                .to_string();
                            let name = tc
                                .pointer("/function/name")
                                .or_else(|| tc.get("name"))
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let arguments_json = tc
                                .pointer("/function/arguments")
                                .or_else(|| tc.get("arguments"))
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
                        content: content.clone(),
                        parts: if content.is_empty() {
                            Vec::new()
                        } else {
                            vec![CanonicalContentPart::Text { text: content }]
                        },
                        reasoning_content: m_obj
                            .get("reasoning_content")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        reasoning_signature: None,
                        tool_calls,
                        tool_call_id: m_obj
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                }
            }
            if let Some(t_arr) = params.get("tools").and_then(Value::as_array) {
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
                                .or_else(|| func.get("input_schema"))
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
        project,
        thread_id,
        tool_choice,
    })
}
