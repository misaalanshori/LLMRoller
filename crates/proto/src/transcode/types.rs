use serde::{Deserialize, Serialize};
use serde_json::Value;

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
            "command_code_generate" | "alpha_generate" | "command_code" => Some(Self::CommandCodeGenerate),
            _ => None,
        }
    }

    /// Canonical route protocol string (`"chat"`, `"responses"`, `"messages"`, etc.).
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

/// Multimodal content part preserved across cross-protocol transcoding (`FND-010`, `F099`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanonicalContentPart {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        data_or_url: String,
        is_url: bool,
    },
    Audio {
        format: String,
        data: String,
    },
    Document {
        media_type: String,
        data_or_url: String,
        name: Option<String>,
    },
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<CanonicalContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_signature: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
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

pub(super) fn extract_text_from_value(val: &Value) -> String {
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
