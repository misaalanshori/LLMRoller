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

            // Injected usage chunk detection (choices:[] and usage:{...}) (API-5)
            if trimmed.contains(&b"\"choices\":[]"[0])
                && (trimmed.windows(12).any(|w| w == b"\"choices\":[]")
                    || trimmed.windows(13).any(|w| w == b"\"choices\": []"))
                && (trimmed.windows(7).any(|w| w == b"\"usage\"")
                    || trimmed.windows(8).any(|w| w == b"\"usage\":"))
            {
                self.saw_injected_usage_chunk = true;
            }

            // Attempt JSON parse when line can carry usage or delta content
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(trimmed) {
                self.extract_from_json(&v);
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
                // Accumulate delta text for aborted stream estimation
                if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                    for choice in choices {
                        if let Some(delta) = choice.get("delta") {
                            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                self.accumulated_output_text_bytes += content.len();
                            }
                            if let Some(reasoning) =
                                delta.get("reasoning_content").and_then(|c| c.as_str())
                            {
                                self.accumulated_output_text_bytes += reasoning.len();
                            }
                        }
                    }
                }
            }
            EndpointKind::Responses => {
                let ev = self.current_event.as_deref().unwrap_or("");
                let is_resp_event = ev == "response.completed"
                    || ev == "response.created"
                    || ev == "response.incomplete"
                    || ev == "response.failed"
                    || json.get("usage").is_some();
                if is_resp_event && let Some(u) = extract_responses_json_usage(json) {
                    self.observed_usage = Some(u);
                }
                // Accumulate delta output text
                if let Some(output) = json.get("output_text").and_then(|o| o.as_str()) {
                    self.accumulated_output_text_bytes += output.len();
                }
            }
            EndpointKind::Messages => {
                let ev = self.current_event.as_deref().unwrap_or("");
                if ev == "message_start" {
                    if let Some(message) = json.get("message")
                        && let Some(u) = extract_messages_json_usage(message)
                    {
                        self.observed_usage = Some(u);
                    }
                } else if ev == "message_delta" {
                    if let Some(delta_usage) = json.get("usage") {
                        let out_toks = delta_usage
                            .get("output_tokens")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        if let Some(ref mut u) = self.observed_usage {
                            u.output_tokens = out_toks;
                        }
                    }
                } else if ev == "content_block_delta"
                    && let Some(delta) = json.get("delta")
                    && let Some(text) = delta.get("text").and_then(|t| t.as_str())
                {
                    self.accumulated_output_text_bytes += text.len();
                }
            }
            _ => {}
        }
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
