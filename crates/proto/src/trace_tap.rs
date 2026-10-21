//! Bounded TraceTap collector, binary block replacement, and head-tail truncation.
//!
//! Spec: Section 6.10, Section 7.1, Section 9.2, Section 12.6, Section 21.1,
//! Requirements: TRC-2, TRC-3, PERF-2, PERF-5.

use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::mpsc;

pub const MAX_REQUEST_BLOCKS_BYTES: usize = 4 * 1024 * 1024; // 4 MiB
pub const MAX_RESPONSE_OUTPUT_BYTES: usize = 1024 * 1024; // 1 MiB
pub const HALF_RESPONSE_OUTPUT_BYTES: usize = 512 * 1024; // 512 KiB
pub const GLOBAL_TRACE_BUDGET_BYTES: usize = 256 * 1024 * 1024; // 256 MiB
pub const MAX_INFLIGHT_TRACE_BYTES: usize = 32 * 1024 * 1024; // 32 MiB per turn

/// Global accumulator tracking trace memory usage across all active streams (PERF-5).
pub static GLOBAL_TRACE_BYTES: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone)]
pub struct AssembledToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

#[derive(Debug, Clone, Default)]
pub struct AssembledResponse {
    pub assistant_text: String,
    pub assistant_reasoning: String,
    pub assistant_tool_calls: Vec<AssembledToolCall>,
    pub truncated_content: bool,
    pub omitted_bytes: usize,
}

/// Protocol-agnostic reassembly of streamed deltas into one assistant turn.
#[derive(Debug, Clone, Default)]
pub struct NormalizedTurnOutput {
    pub response_id: Option<String>,
    pub assistant_text: String,
    pub assistant_reasoning: String,
    pub assistant_tool_calls: Vec<AssembledToolCall>,
    pub finish_reason: Option<String>,
    pub truncated: bool,
    pub dropped_block_count: u32,
}

/// Helper calculating SHA-256 in lowercase hex.
pub fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    llmroller_core::session::hex_lower(&digest)
}

/// Recursively replaces binary payloads > 1 KiB with lightweight omission descriptors (TRC-2).
pub fn replace_binary_blocks(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if matches!(k.as_str(), "data" | "file_data" | "b64_json") {
                    if let Some(s) = v.as_str()
                        && s.len() > 1024
                    {
                        let sha256_hex = hex_sha256(s.as_bytes());
                        *v = serde_json::json!({
                            "type": k,
                            "llmroller_omitted": "binary",
                            "sha256": sha256_hex,
                            "bytes": s.len()
                        });
                    }
                } else if k == "url" {
                    if let Some(s) = v.as_str()
                        && s.starts_with("data:")
                        && s.len() > 1024
                    {
                        let sha256_hex = hex_sha256(s.as_bytes());
                        *v = serde_json::json!({
                            "type": "url",
                            "llmroller_omitted": "binary",
                            "sha256": sha256_hex,
                            "bytes": s.len()
                        });
                    }
                } else {
                    replace_binary_blocks(v);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                replace_binary_blocks(v);
            }
        }
        _ => {}
    }
}

/// Enforces 1 MiB response output cap using head/tail truncation (TRC-3).
pub fn apply_head_tail_truncation(text: &mut String) -> (bool, usize) {
    if text.len() <= MAX_RESPONSE_OUTPUT_BYTES {
        return (false, 0);
    }
    let total_len = text.len();
    let omitted = total_len - MAX_RESPONSE_OUTPUT_BYTES;

    let mut head_end = HALF_RESPONSE_OUTPUT_BYTES;
    while !text.is_char_boundary(head_end) && head_end > 0 {
        head_end -= 1;
    }

    let mut tail_start = total_len - HALF_RESPONSE_OUTPUT_BYTES;
    while !text.is_char_boundary(tail_start) && tail_start < total_len {
        tail_start += 1;
    }

    let head = &text[..head_end];
    let tail = &text[tail_start..];
    *text = format!("{head}\n[llmroller: {omitted} bytes omitted]\n{tail}");
    (true, omitted)
}

/// Bounded trace chunk sender protecting the client forwarder loop (PERF-2, PERF-5).
#[derive(Debug)]
pub struct TraceTap {
    tx: mpsc::Sender<Bytes>,
    pub trace_dropped: Arc<AtomicBool>,
    allocated_bytes: AtomicUsize,
    inflight_bytes: usize,
    /// Deltas reassembled from the response stream.
    pub output: NormalizedTurnOutput,
}

impl TraceTap {
    pub fn new() -> (Self, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(256);
        (
            Self {
                tx,
                trace_dropped: Arc::new(AtomicBool::new(false)),
                allocated_bytes: AtomicUsize::new(0),
                inflight_bytes: 0,
                output: NormalizedTurnOutput::default(),
            },
            rx,
        )
    }

    /// Feed one parsed SSE `data:` JSON object and reassemble assistant deltas.
    ///
    /// Supports OpenAI Chat Completions, OpenAI Responses, and Anthropic Messages.
    pub fn feed_sse_data_json(&mut self, value: &serde_json::Value) {
        if self.output.truncated {
            return;
        }
        let raw = value.to_string();
        self.inflight_bytes = self.inflight_bytes.saturating_add(raw.len());
        if self.inflight_bytes > MAX_INFLIGHT_TRACE_BYTES {
            self.output.truncated = true;
            return;
        }
        if let Some(id) = value.get("id").and_then(serde_json::Value::as_str)
            && self.output.response_id.is_none()
        {
            self.output.response_id = Some(id.to_string());
        }

        if let Some(choice) = value.pointer("/choices/0") {
            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(serde_json::Value::as_str)
            {
                self.output.finish_reason = Some(reason.to_string());
            }
            if let Some(delta) = choice.get("delta") {
                if let Some(text) = delta.get("content").and_then(serde_json::Value::as_str) {
                    self.append_output_text(text, false);
                }
                if let Some(reasoning) = delta
                    .get("reasoning_content")
                    .or_else(|| delta.get("reasoning"))
                    .and_then(serde_json::Value::as_str)
                {
                    self.append_output_text(reasoning, true);
                }
                if let Some(calls) = delta
                    .get("tool_calls")
                    .and_then(serde_json::Value::as_array)
                {
                    for call in calls {
                        self.append_tool_call(call);
                    }
                }
            }
        }

        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("content_block_delta") => {
                if let Some(delta) = value.get("delta") {
                    match delta.get("type").and_then(serde_json::Value::as_str) {
                        Some("text_delta") => {
                            if let Some(text) =
                                delta.get("text").and_then(serde_json::Value::as_str)
                            {
                                self.append_output_text(text, false);
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(text) =
                                delta.get("thinking").and_then(serde_json::Value::as_str)
                            {
                                self.append_output_text(text, true);
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(partial) = delta
                                .get("partial_json")
                                .and_then(serde_json::Value::as_str)
                            {
                                self.append_anthropic_tool_json(partial);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("message_delta") => {
                if let Some(reason) = value
                    .pointer("/delta/stop_reason")
                    .and_then(serde_json::Value::as_str)
                {
                    self.output.finish_reason = Some(reason.to_string());
                }
            }
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    self.append_output_text(delta, false);
                }
            }
            Some("response.reasoning_summary_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    self.append_output_text(delta, true);
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = value.get("item")
                    && item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
                {
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arguments = item
                        .get("arguments")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    self.output.assistant_tool_calls.push(AssembledToolCall {
                        id,
                        name,
                        arguments_json: arguments,
                    });
                }
            }
            _ => {}
        }
    }

    /// Drop request content blocks larger than the 4 MiB cap before persisting.
    ///
    /// Returns `true` when at least one oversized block was replaced.
    pub fn sanitize_request_blocks(&mut self, value: &mut serde_json::Value) -> bool {
        let mut dropped = 0_u32;
        sanitize_blocks(value, &mut dropped);
        self.output.dropped_block_count += dropped;
        dropped > 0
    }

    fn append_output_text(&mut self, text: &str, reasoning: bool) {
        let target = if reasoning {
            &mut self.output.assistant_reasoning
        } else {
            &mut self.output.assistant_text
        };
        target.push_str(text);
        let (truncated, _) = apply_head_tail_truncation(target);
        if truncated {
            self.output.truncated = true;
        }
    }

    fn append_tool_call(&mut self, call: &serde_json::Value) {
        let index = call
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |index| index as usize);
        while self.output.assistant_tool_calls.len() <= index {
            self.output.assistant_tool_calls.push(AssembledToolCall {
                id: String::new(),
                name: String::new(),
                arguments_json: String::new(),
            });
        }
        let entry = &mut self.output.assistant_tool_calls[index];
        if let Some(id) = call.get("id").and_then(serde_json::Value::as_str) {
            entry.id = id.to_string();
        }
        if let Some(function) = call.get("function") {
            if let Some(name) = function.get("name").and_then(serde_json::Value::as_str) {
                entry.name.push_str(name);
            }
            if let Some(arguments) = function
                .get("arguments")
                .and_then(serde_json::Value::as_str)
            {
                entry.arguments_json.push_str(arguments);
            }
        }
    }

    fn append_anthropic_tool_json(&mut self, partial: &str) {
        if self.output.assistant_tool_calls.is_empty() {
            self.output.assistant_tool_calls.push(AssembledToolCall {
                id: String::new(),
                name: String::new(),
                arguments_json: String::new(),
            });
        }
        if let Some(last) = self.output.assistant_tool_calls.last_mut() {
            last.arguments_json.push_str(partial);
        }
    }

    /// Feeds chunk using non-blocking try_send without ever blocking client streaming (PERF-2).
    pub fn try_feed_chunk(&self, chunk: Bytes) -> bool {
        if self.trace_dropped.load(Ordering::Acquire) {
            return false;
        }

        let chunk_len = chunk.len();
        let curr_global = GLOBAL_TRACE_BYTES.load(Ordering::Relaxed);
        if curr_global + chunk_len > GLOBAL_TRACE_BUDGET_BYTES {
            self.trace_dropped.store(true, Ordering::Release);
            return false;
        }

        match self.tx.try_send(chunk) {
            Ok(()) => {
                GLOBAL_TRACE_BYTES.fetch_add(chunk_len, Ordering::Relaxed);
                self.allocated_bytes.fetch_add(chunk_len, Ordering::Relaxed);
                true
            }
            Err(_) => {
                self.trace_dropped.store(true, Ordering::Release);
                false
            }
        }
    }
}

impl Drop for TraceTap {
    fn drop(&mut self) {
        let allocated = self.allocated_bytes.load(Ordering::Relaxed);
        GLOBAL_TRACE_BYTES.fetch_sub(allocated, Ordering::Relaxed);
    }
}

fn looks_like_block(value: &serde_json::Value) -> bool {
    value.get("type").is_some()
        && [
            "data",
            "url",
            "file_data",
            "b64_json",
            "source",
            "image_url",
        ]
        .iter()
        .any(|key| value.get(*key).is_some())
}

fn sanitize_blocks(value: &mut serde_json::Value, dropped: &mut u32) {
    match value {
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                let is_oversized_block = looks_like_block(child)
                    && serde_json::to_vec(child)
                        .is_ok_and(|bytes| bytes.len() > MAX_REQUEST_BLOCKS_BYTES);
                if is_oversized_block {
                    let size = serde_json::to_vec(child).map_or(0, |bytes| bytes.len());
                    *child = serde_json::json!({
                        "llmroller_omitted": "oversized_block",
                        "bytes": size
                    });
                    *dropped += 1;
                } else {
                    sanitize_blocks(child, dropped);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                let is_oversized_block = looks_like_block(child)
                    && serde_json::to_vec(child)
                        .is_ok_and(|bytes| bytes.len() > MAX_REQUEST_BLOCKS_BYTES);
                if is_oversized_block {
                    let size = serde_json::to_vec(child).map_or(0, |bytes| bytes.len());
                    *child = serde_json::json!({
                        "llmroller_omitted": "oversized_block",
                        "bytes": size
                    });
                    *dropped += 1;
                } else {
                    sanitize_blocks(child, dropped);
                }
            }
        }
        serde_json::Value::String(text) if text.len() > MAX_REQUEST_BLOCKS_BYTES => {
            let size = text.len();
            *value = serde_json::json!({
                "llmroller_omitted": "oversized_block",
                "bytes": size
            });
            *dropped += 1;
        }
        _ => {}
    }
}

#[cfg(test)]
pub mod tests;
