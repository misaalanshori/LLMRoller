#![forbid(unsafe_code)]
//! Command Code native `/alpha/generate` envelope and AI SDK v5 NDJSON stream translator (`F095`).
//!
//! Spec: ./docs/srdd/2. SRDD - Research Findings.md#23-target-provider-endpoints-protocols-and-quotas
//! Spec: ./docs/srdd/24. SRDD - Cross-Protocol Transcoding and OpenAI Client Normalization.md#241-bidirectional-protocol-transcoding-bridge-cratesprotosrctranscode

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::transcode::{WireProtocol, parse_canonical_request, transcode_sse_payload};

/// Canonical Command Code CLI version sent in `x-command-code-version` (`F095`).
pub const COMMAND_CODE_CLI_VERSION: &str = "0.25.7";

/// Default model catalog supported by Command Code (`F095`).
pub const COMMAND_CODE_DEFAULT_MODELS: &[&str] = &[
    "claude-opus-4.6",
    "claude-sonnet-4.6",
    "claude-haiku-4.5",
    "gpt-5.4",
    "gpt-5.3-codex",
    "glm-5",
    "kimi-k2.5",
    "minimax-m2.5",
];

/// Resolve the origin-root Command Code `/alpha/billing/credits` URL from any Command Code base URL (`F095`).
#[must_use]
pub fn resolve_command_code_credits_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let origin = trimmed.trim_end_matches("/provider/v1");
    format!("{origin}/alpha/billing/credits")
}

/// Resolve the origin-root Command Code `/alpha/generate` URL from any Command Code base URL (`F095`).
#[must_use]
pub fn resolve_command_code_generate_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let origin = trimmed.trim_end_matches("/provider/v1");
    format!("{origin}/alpha/generate")
}

/// Build the required Command Code CLI headers (`F095`).
#[must_use]
pub fn command_code_cli_headers(session_id: &str) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert(
        "x-command-code-version".to_string(),
        COMMAND_CODE_CLI_VERSION.to_string(),
    );
    headers.insert("x-cli-environment".to_string(), "cli".to_string());
    headers.insert(
        "user-agent".to_string(),
        "ai-sdk/provider-utils/3.0.20 runtime/node.js".to_string(),
    );
    headers.insert("accept".to_string(), "text/event-stream".to_string());
    if !session_id.trim().is_empty() {
        headers.insert("x-session-id".to_string(), session_id.to_string());
    }
    headers
}

/// Translate an inbound OpenAI Chat, OpenAI Responses, or Anthropic Messages request
/// into the Command Code `/alpha/generate` JSON envelope (`F095`).
pub fn build_alpha_generate_envelope(
    source_proto: WireProtocol,
    body: &[u8],
    target_model: &str,
    thread_id: &str,
) -> Result<Vec<u8>, String> {
    let req = parse_canonical_request(source_proto, body)?;
    let model = if target_model.trim().is_empty() {
        req.model.as_str()
    } else {
        target_model
    };
    let eff_thread = if thread_id.trim().is_empty() {
        "thr_llmroller"
    } else {
        thread_id
    };
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            json!({
                "role": m.role,
                "content": m.content,
            })
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
    let envelope = json!({
        "threadId": eff_thread,
        "memory": "",
        "config": {
            "model": model,
        },
        "params": {
            "messages": messages,
            "system": req.system.unwrap_or_default(),
            "tools": tools,
        }
    });
    serde_json::to_vec(&envelope).map_err(|e| e.to_string())
}

/// Translate Command Code AI SDK v5 NDJSON stream lines (`text-delta`, `reasoning-delta`, `tool-call`, `finish`)
/// into the downstream client's expected SSE stream (`F095`).
#[must_use]
pub fn translate_ndjson_stream_to_sse(
    ndjson_bytes: &[u8],
    downstream_proto: WireProtocol,
    model: &str,
) -> Vec<u8> {
    transcode_sse_payload(
        WireProtocol::CommandCodeGenerate,
        downstream_proto,
        ndjson_bytes,
        model,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: PRV-2, USG-4, F095 - Command Code billing URL, CLI headers, and /alpha/generate translator"]
    fn regression_test_f095_command_code_preset_and_adapter_mismatch() {
        assert_eq!(
            resolve_command_code_credits_url("https://api.commandcode.ai/provider/v1"),
            "https://api.commandcode.ai/alpha/billing/credits"
        );
        assert_eq!(
            resolve_command_code_generate_url("https://api.commandcode.ai/provider/v1"),
            "https://api.commandcode.ai/alpha/generate"
        );

        let headers = command_code_cli_headers("sess-cc-1");
        assert_eq!(
            headers.get("x-command-code-version").map(String::as_str),
            Some("0.25.7")
        );
        assert_eq!(
            headers.get("x-cli-environment").map(String::as_str),
            Some("cli")
        );
        assert_eq!(
            headers.get("x-session-id").map(String::as_str),
            Some("sess-cc-1")
        );

        let chat_req = br#"{
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "system", "content": "You are a coding assistant."},
                {"role": "user", "content": "Write hello world"}
            ]
        }"#;
        let env_bytes = build_alpha_generate_envelope(
            WireProtocol::ChatCompletions,
            chat_req,
            "claude-sonnet-4.6",
            "thr_123",
        )
        .expect("envelope");
        let env: Value = serde_json::from_slice(&env_bytes).expect("json");
        assert_eq!(env["threadId"], "thr_123");
        assert_eq!(env["config"]["model"], "claude-sonnet-4.6");
        assert_eq!(env["params"]["system"], "You are a coding assistant.");
        assert_eq!(env["params"]["messages"][0]["content"], "Write hello world");

        let ndjson = b"{\"type\":\"text-delta\",\"textDelta\":\"Hello \"}\n{\"type\":\"text-delta\",\"textDelta\":\"Command Code!\"}\n{\"type\":\"finish\",\"usage\":{\"promptTokens\":11,\"completionTokens\":3}}\n";
        let sse = String::from_utf8(translate_ndjson_stream_to_sse(
            ndjson,
            WireProtocol::ChatCompletions,
            "claude-sonnet-4.6",
        ))
        .expect("utf8");
        assert!(sse.contains("\"content\":\"Hello \""));
        assert!(sse.contains("\"content\":\"Command Code!\""));
        assert!(sse.contains("\"prompt_tokens\":11"));
        assert!(sse.contains("\"completion_tokens\":3"));
    }
}
