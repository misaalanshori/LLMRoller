//! Zero-overhead streaming SSE usage tap, delta text accumulator, and aborted-stream token estimator.
//!
//! Spec: Section 2.5, Section 6.8, Section 6.9, Section 12.4.1, Section 12.5,
//! Requirements: API-5, USG-1, USG-2, USG-3, USG-9.

use crate::adapters::EndpointKind;
use crate::json_usage::{
    ExtractedUsage, extract_chat_json_usage, extract_json_stop_reason, extract_messages_json_usage,
    extract_responses_json_usage,
};
use crate::sse_frame::find_frame_delimiter;

/// Streaming SSE usage tap that inspects SSE frames without delaying client forwarding.
#[derive(Debug, Clone)]
pub struct SseUsageTap {
    pub endpoint: EndpointKind,
    pub observed_usage: Option<ExtractedUsage>,
    pub accumulated_output_text_bytes: usize,
    pub saw_injected_usage_chunk: bool,
    pub saw_done: bool,
    pub saw_terminal_event: bool,
    pub has_terminal_output_usage: bool,
    pub stop_reason: Option<String>,
    pub response_id: Option<String>,
    pending: Vec<u8>,
}

impl SseUsageTap {
    pub fn new(endpoint: EndpointKind) -> Self {
        Self {
            endpoint,
            observed_usage: None,
            accumulated_output_text_bytes: 0,
            saw_injected_usage_chunk: false,
            saw_done: false,
            saw_terminal_event: false,
            has_terminal_output_usage: false,
            stop_reason: None,
            response_id: None,
            pending: Vec::with_capacity(4096),
        }
    }

    /// Feeds raw SSE bytes from upstream into the tap.
    pub fn feed_chunk(&mut self, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
        let mut offset = 0;
        while let Some(rel_end) = find_frame_delimiter(&self.pending[offset..]) {
            let frame_end = offset + rel_end;
            let frame = self.pending[offset..frame_end].to_vec();
            offset = frame_end;
            self.process_frame(&frame);
        }
        if offset > 0 {
            self.pending.drain(..offset);
        }
        // Cap pending buffer at 1 MiB to guard against malformed streams without delimiters
        if self.pending.len() > 1024 * 1024 {
            self.pending.clear();
        }
    }

    /// Flushes any trailing unterminated SSE frame at stream EOF (`F009`).
    pub fn finish(&mut self) {
        if self.pending.iter().any(|b| !b.is_ascii_whitespace()) {
            let tail = std::mem::take(&mut self.pending);
            self.process_frame(&tail);
        } else {
            self.pending.clear();
        }
    }

    /// Returns whether the stream reached a clean protocol-level EOF (`F009`).
    #[must_use]
    pub fn is_complete_at_eof(&self) -> bool {
        if self.pending.iter().any(|b| !b.is_ascii_whitespace()) {
            let mut copy = self.clone();
            copy.finish();
            return copy.saw_done || copy.saw_terminal_event || copy.has_terminal_output_usage;
        }
        self.saw_done || self.saw_terminal_event || self.has_terminal_output_usage
    }

    fn process_frame(&mut self, frame: &[u8]) {
        let Ok(s) = std::str::from_utf8(frame) else {
            return;
        };

        let mut data_payload = String::new();
        for line in s.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with(':') {
                continue;
            }
            if let Some(ev) = trimmed.strip_prefix("event:") {
                let ev_name = ev.trim();
                if matches!(
                    ev_name,
                    "message_stop"
                        | "response.completed"
                        | "response.incomplete"
                        | "response.failed"
                ) {
                    self.saw_terminal_event = true;
                }
            } else if let Some(d) = trimmed.strip_prefix("data:") {
                let d_trim = d.trim();
                if d_trim == "[DONE]" {
                    self.saw_done = true;
                    self.saw_terminal_event = true;
                    return;
                }
                if !data_payload.is_empty() {
                    data_payload.push('\n');
                }
                data_payload.push_str(d_trim);
            }
        }

        if data_payload.is_empty() {
            return;
        }

        let Ok(json) = serde_json::from_str::<serde_json::Value>(&data_payload) else {
            return;
        };

        if let Some(reason) = extract_json_stop_reason(self.endpoint, &json) {
            self.stop_reason = Some(reason);
        }

        match self.endpoint {
            EndpointKind::Chat => {
                // Check if this is an injected usage-only chunk: choices == [] and usage is a JSON object
                if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                    if choices.is_empty() && json.get("usage").is_some_and(|u| u.is_object()) {
                        self.saw_injected_usage_chunk = true;
                    }
                    // Accumulate delta text bytes across content, reasoning, and tool call arguments
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
                            if let Some(reasoning) = delta.get("reasoning").and_then(|c| c.as_str())
                            {
                                self.accumulated_output_text_bytes += reasoning.len();
                            }
                            if let Some(tool_calls) =
                                delta.get("tool_calls").and_then(|tc| tc.as_array())
                            {
                                for tc in tool_calls {
                                    if let Some(args) = tc
                                        .get("function")
                                        .and_then(|f| f.get("arguments"))
                                        .and_then(|a| a.as_str())
                                    {
                                        self.accumulated_output_text_bytes += args.len();
                                    }
                                }
                            }
                        }
                    }
                }
                if json.get("usage").is_some_and(|u| !u.is_null())
                    && let Some(u) = extract_chat_json_usage(&json)
                {
                    self.observed_usage = Some(u);
                    self.has_terminal_output_usage = true;
                }
            }
            EndpointKind::Responses | EndpointKind::ResponsesCompact => {
                if self.response_id.is_none()
                    && let Some(rid) = json
                        .pointer("/response/id")
                        .or_else(|| json.get("response_id"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                {
                    self.response_id = Some(rid.to_string());
                }
                let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if matches!(
                    event_type,
                    "response.completed"
                        | "response.incomplete"
                        | "response.failed"
                        | "response.done"
                ) {
                    self.saw_terminal_event = true;
                }
                if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                    self.accumulated_output_text_bytes += delta.len();
                }
                if let Some(u) = extract_responses_json_usage(&json) {
                    self.observed_usage = Some(merge_responses_usage(self.observed_usage, u));
                    if matches!(
                        event_type,
                        "response.completed"
                            | "response.incomplete"
                            | "response.failed"
                            | "response.done"
                    ) || u.output_tokens > 0
                    {
                        self.has_terminal_output_usage = true;
                    }
                }
            }
            EndpointKind::Messages => {
                let msg_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if msg_type == "message_stop" {
                    self.saw_terminal_event = true;
                    if let Some(usage_obj) = json
                        .get("usage")
                        .or_else(|| json.get("delta").and_then(|d| d.get("usage")))
                    {
                        let extracted = extract_messages_json_usage(&json).unwrap_or_default();
                        let prev = self.observed_usage;
                        self.observed_usage =
                            Some(merge_messages_usage(prev, extracted, usage_obj));
                        self.has_terminal_output_usage = true;
                    }
                } else if msg_type == "message_start" {
                    if let Some(u) = extract_messages_json_usage(&json) {
                        self.observed_usage = Some(u);
                    }
                } else if msg_type == "content_block_delta" {
                    if let Some(delta) = json.get("delta") {
                        if let Some(txt) = delta.get("text").and_then(|t| t.as_str()) {
                            self.accumulated_output_text_bytes += txt.len();
                        }
                        if let Some(thinking) = delta.get("thinking").and_then(|t| t.as_str()) {
                            self.accumulated_output_text_bytes += thinking.len();
                        }
                        if let Some(partial_json) =
                            delta.get("partial_json").and_then(|t| t.as_str())
                        {
                            self.accumulated_output_text_bytes += partial_json.len();
                        }
                    }
                } else if msg_type == "message_delta"
                    && let Some(usage_obj) = json
                        .get("usage")
                        .or_else(|| json.get("delta").and_then(|d| d.get("usage")))
                {
                    let extracted = extract_messages_json_usage(&json).unwrap_or_default();
                    let prev = self.observed_usage;
                    self.observed_usage = Some(merge_messages_usage(prev, extracted, usage_obj));
                    self.has_terminal_output_usage = true;
                }
            }
            _ => {}
        }
    }

    /// Finalizes token usage on stream completion or abort.
    /// If upstream usage was observed with terminal output tokens, returns it;
    /// if the stream aborted after an initial `message_start` or `response.created`
    /// before terminal output usage arrived, preserves the observed input/cache tokens
    /// while estimating `output_tokens` as `ceil(accumulated_output_text_bytes / 4)` (`F-003`, `USG-3`).
    pub fn finalize_usage(&self) -> ExtractedUsage {
        if self.pending.iter().any(|b| !b.is_ascii_whitespace()) {
            let mut flushed = self.clone();
            flushed.finish();
            return flushed.finalize_usage();
        }
        let estimated_out = if self.accumulated_output_text_bytes == 0 {
            0
        } else {
            self.accumulated_output_text_bytes.div_ceil(4) as i64
        };
        if let Some(mut u) = self.observed_usage {
            if !self.has_terminal_output_usage && self.accumulated_output_text_bytes > 0 {
                u.output_tokens = u.output_tokens.max(estimated_out);
            }
            u
        } else {
            ExtractedUsage {
                input_uncached_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                output_tokens: estimated_out,
                reasoning_tokens: 0,
                provider_cost_micro_usd: None,
            }
        }
    }
}


fn merge_responses_usage(
    previous: Option<ExtractedUsage>,
    current: ExtractedUsage,
) -> ExtractedUsage {
    let Some(prev) = previous else {
        return current;
    };
    ExtractedUsage {
        input_uncached_tokens: if current.input_uncached_tokens > 0 {
            current.input_uncached_tokens
        } else {
            prev.input_uncached_tokens
        },
        cache_read_tokens: if current.cache_read_tokens > 0 {
            current.cache_read_tokens
        } else {
            prev.cache_read_tokens
        },
        cache_write_tokens: if current.cache_write_tokens > 0 {
            current.cache_write_tokens
        } else {
            prev.cache_write_tokens
        },
        output_tokens: if current.output_tokens > 0 {
            current.output_tokens
        } else {
            prev.output_tokens
        },
        reasoning_tokens: if current.reasoning_tokens > 0 {
            current.reasoning_tokens
        } else {
            prev.reasoning_tokens
        },
        provider_cost_micro_usd: current
            .provider_cost_micro_usd
            .or(prev.provider_cost_micro_usd),
    }
}

fn merge_messages_usage(
    previous: Option<ExtractedUsage>,
    current: ExtractedUsage,
    usage_obj: &serde_json::Value,
) -> ExtractedUsage {
    let Some(prev) = previous else {
        return current;
    };
    ExtractedUsage {
        input_uncached_tokens: if usage_obj
            .get("input_tokens")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|v| v > 0)
        {
            current.input_uncached_tokens
        } else {
            prev.input_uncached_tokens
        },
        cache_read_tokens: if usage_obj
            .get("cache_read_input_tokens")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|v| v > 0)
        {
            current.cache_read_tokens
        } else {
            prev.cache_read_tokens
        },
        cache_write_tokens: if (usage_obj
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|v| v > 0))
            || (usage_obj.get("cache_creation").is_some() && current.cache_write_tokens > 0)
        {
            current.cache_write_tokens
        } else {
            prev.cache_write_tokens
        },
        output_tokens: if usage_obj
            .get("output_tokens")
            .and_then(serde_json::Value::as_i64)
            .is_some()
        {
            current.output_tokens
        } else {
            prev.output_tokens
        },
        reasoning_tokens: if current.reasoning_tokens > 0 {
            current.reasoning_tokens
        } else {
            prev.reasoning_tokens
        },
        provider_cost_micro_usd: current
            .provider_cost_micro_usd
            .or(prev.provider_cost_micro_usd),
    }
}

#[cfg(test)]
mod golden_tests;
