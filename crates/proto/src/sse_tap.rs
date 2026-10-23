//! Inline SSE stream usage extractor with unified cache subtraction.
//!
//! Spec: Section 12.4.1, Section 12.5, Requirements: API-5, USG-1, USG-2, USG-3.

use crate::adapters::EndpointKind;
use crate::json_usage::{
    ExtractedUsage, extract_chat_json_usage, extract_messages_json_usage,
    extract_responses_json_usage,
};
use llmroller_core::pricing::div_ceil_i64;

/// Inline synchronous SSE token and text extractor.
#[derive(Debug, Default)]
pub struct SseUsageTap {
    endpoint: EndpointKind,
    pub observed_usage: Option<ExtractedUsage>,
    pub accumulated_output_text_bytes: usize,
    pub saw_injected_usage_chunk: bool,
    line_buf: Vec<u8>,
    current_event: Option<String>,
}

impl SseUsageTap {
    pub fn new(endpoint: EndpointKind) -> Self {
        Self {
            endpoint,
            observed_usage: None,
            accumulated_output_text_bytes: 0,
            saw_injected_usage_chunk: false,
            line_buf: Vec::with_capacity(4096),
            current_event: None,
        }
    }

    /// Process a streaming byte chunk inline without buffering client frames.
    pub fn feed_chunk(&mut self, chunk: &[u8]) {
        for &b in chunk {
            if b == b'\n' {
                let line = std::mem::take(&mut self.line_buf);
                self.process_line(&line);
            } else if b != b'\r' && self.line_buf.len() < 65_536 {
                self.line_buf.push(b);
            }
        }
    }

    fn process_line(&mut self, line: &[u8]) {
        if line.is_empty() {
            self.current_event = None;
            return;
        }

        if let Some(rest) = line.strip_prefix(b"event:") {
            if let Ok(ev) = std::str::from_utf8(rest) {
                self.current_event = Some(ev.trim().to_string());
            }
            return;
        }

        if let Some(rest) = line.strip_prefix(b"data:") {
            let trimmed = rest.trim_ascii();
            if trimmed.is_empty() || trimmed == b"[DONE]" {
                return;
            }

            // Attempt JSON parse when line can carry usage or delta content.
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(trimmed) {
                // Injected usage chunk detection (choices:[], usage:{...}) (API-5).
                // Parse structurally so whitespace variants are still suppressed.
                if value
                    .get("choices")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(Vec::is_empty)
                    && value.get("usage").is_some_and(serde_json::Value::is_object)
                {
                    self.saw_injected_usage_chunk = true;
                }
                self.extract_from_json(&value);
            }
        }
    }

    fn extract_from_json(&mut self, json: &serde_json::Value) {
        match self.endpoint {
            EndpointKind::Chat => {
                // Extract usage if present in chunk
                if let Some(u) = extract_chat_json_usage(json) {
                    self.observed_usage = Some(u);
                }
                // Accumulate every output-bearing delta for aborted stream estimation.
                if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                    for choice in choices {
                        let Some(delta) = choice.get("delta") else {
                            continue;
                        };
                        for field in ["content", "reasoning_content", "reasoning"] {
                            if let Some(text) = delta.get(field).and_then(|c| c.as_str()) {
                                self.accumulated_output_text_bytes += text.len();
                            }
                        }
                        if let Some(tool_calls) = delta.get("tool_calls").and_then(|c| c.as_array())
                        {
                            for tool_call in tool_calls {
                                if let Some(arguments) = tool_call
                                    .pointer("/function/arguments")
                                    .and_then(|a| a.as_str())
                                {
                                    self.accumulated_output_text_bytes += arguments.len();
                                }
                            }
                        }
                        // Legacy single-function-call shape used by some compatible APIs.
                        if let Some(arguments) = delta
                            .pointer("/function_call/arguments")
                            .and_then(|a| a.as_str())
                        {
                            self.accumulated_output_text_bytes += arguments.len();
                        }
                    }
                }
            }
            EndpointKind::Responses => {
                let event_type = self
                    .current_event
                    .as_deref()
                    .or_else(|| json.get("type").and_then(|t| t.as_str()))
                    .unwrap_or("");
                let is_usage_event = matches!(
                    event_type,
                    "response.created"
                        | "response.completed"
                        | "response.incomplete"
                        | "response.failed"
                ) || json.get("usage").is_some()
                    || json
                        .get("response")
                        .is_some_and(|response| response.get("usage").is_some());
                if is_usage_event && let Some(u) = extract_responses_json_usage(json) {
                    self.observed_usage = Some(u);
                }
                // Responses deltas (`output_text`, reasoning summary, function arguments)
                // carry the incremental string in `delta`.
                if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                    self.accumulated_output_text_bytes += delta.len();
                }
                if let Some(output) = json.get("output_text").and_then(|o| o.as_str()) {
                    self.accumulated_output_text_bytes += output.len();
                }
            }
            EndpointKind::Messages => {
                let event_type = self
                    .current_event
                    .as_deref()
                    .or_else(|| json.get("type").and_then(|t| t.as_str()))
                    .unwrap_or("");
                if event_type == "message_start" {
                    if let Some(message) = json.get("message")
                        && let Some(u) = extract_messages_json_usage(message)
                    {
                        self.observed_usage = Some(u);
                    }
                } else if event_type == "message_delta" {
                    if let Some(delta_usage) = json.get("usage") {
                        self.merge_messages_usage(delta_usage);
                    }
                } else if event_type == "content_block_delta"
                    && let Some(delta) = json.get("delta")
                {
                    for field in ["text", "thinking", "partial_json"] {
                        if let Some(text) = delta.get(field).and_then(|t| t.as_str()) {
                            self.accumulated_output_text_bytes += text.len();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Merges cumulative `message_delta` usage with the last observed message usage.
    fn merge_messages_usage(&mut self, usage: &serde_json::Value) {
        let mut merged = self.observed_usage.unwrap_or_default();
        if let Some(tokens) = usage.get("input_tokens").and_then(|v| v.as_i64())
            && (tokens > 0 || merged.input_uncached_tokens == 0)
        {
            merged.input_uncached_tokens = tokens;
        }
        if let Some(tokens) = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_i64())
        {
            merged.cache_read_tokens = tokens;
        }
        if let Some(tokens) = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_i64())
        {
            merged.cache_write_tokens = tokens;
        }
        if let Some(tokens) = usage.get("output_tokens").and_then(|v| v.as_i64()) {
            merged.output_tokens = tokens;
        }
        if let Some(tokens) = usage.get("reasoning_tokens").and_then(|v| v.as_i64()) {
            merged.reasoning_tokens = tokens;
        }
        if let Some(cost) = usage.get("cost").and_then(|v| v.as_f64()) {
            merged.provider_cost_micro_usd =
                llmroller_core::pricing::parse_provider_cost(Some(cost));
        }
        self.observed_usage = Some(merged);
    }

    /// Finalizes token usage: uses observed usage if received, else estimates aborted output (USG-3).
    pub fn finalize_usage(&self) -> ExtractedUsage {
        if let Some(u) = self.observed_usage {
            u
        } else {
            // Aborted stream without upstream usage: ceil(decoded_text_bytes / 4)
            let estimated_output = div_ceil_i64(self.accumulated_output_text_bytes as i64, 4);
            ExtractedUsage {
                input_uncached_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                output_tokens: estimated_output,
                reasoning_tokens: 0,
                provider_cost_micro_usd: None,
            }
        }
    }
}

#[cfg(test)]
pub mod golden_tests;
