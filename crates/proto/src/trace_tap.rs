//! Bounded TraceTap collector, binary block replacement, and head-tail truncation.
//!
//! Spec: Section 6.10, Section 7.1, Section 9.2, Section 12.6, Section 21.1,
//! Requirements: TRC-2, TRC-3, PERF-2, PERF-5.

use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::mpsc;

use crate::sse_frame::find_frame_delimiter;

pub const MAX_REQUEST_BLOCKS_BYTES: usize = 4 * 1024 * 1024; // 4 MiB per block
pub const MAX_TURN_NEW_BLOCKS_BYTES: usize = 4 * 1024 * 1024; // 4 MiB per turn, newest kept
pub const MAX_RESPONSE_OUTPUT_BYTES: usize = 1024 * 1024; // 1 MiB
pub const HALF_RESPONSE_OUTPUT_BYTES: usize = 512 * 1024; // 512 KiB
pub const GLOBAL_TRACE_BUDGET_DEFAULT_BYTES: usize = 256 * 1024 * 1024; // 256 MiB
/// Global accumulator budget, sized at startup as `min(256 MiB, 10% of memory_limit)` (PERF-5).
pub static GLOBAL_TRACE_BUDGET_BYTES: AtomicUsize =
    AtomicUsize::new(GLOBAL_TRACE_BUDGET_DEFAULT_BYTES);
pub const MAX_INFLIGHT_TRACE_BYTES: usize = 32 * 1024 * 1024; // 32 MiB per turn
/// Retained for callers that used the old per-turn name; the authoritative
/// per-turn cap is [`MAX_TURN_NEW_BLOCKS_BYTES`] (TRC-2, Section 12.6).
pub const MAX_REQUEST_TRACE_BYTES: usize = MAX_TURN_NEW_BLOCKS_BYTES;
pub const MAX_SSE_PENDING_BYTES: usize = 2 * 1024 * 1024; // incomplete frame guard
/// Maximum tool calls assembled per turn to prevent malicious/corrupt index OOM (`F015`).
pub const MAX_TOOL_CALLS_PER_TURN: usize = 256;

/// Global accumulator tracking trace memory usage across all active streams (PERF-5).
pub static GLOBAL_TRACE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Size the global trace accumulator from the deployment memory limit (PERF-5).
///
/// Values above the 256 MiB ceiling are clamped; a zero budget is ignored so a
/// misconfigured ceiling can never disable traceability entirely.
pub fn set_global_trace_budget_bytes(bytes: usize) {
    let bounded = bytes.min(GLOBAL_TRACE_BUDGET_DEFAULT_BYTES);
    if bounded > 0 {
        GLOBAL_TRACE_BUDGET_BYTES.store(bounded, Ordering::Relaxed);
    }
}

/// Current global trace memory in flight, and the active budget.
///
/// Exposed so the observability layer and tests can assert the accounting invariant
/// without touching the statics directly (`PERF-5`). Note that `used > budget` is a
/// legal state: lowering the budget at runtime does not retroactively clamp money
/// already reserved.
#[must_use]
pub fn global_trace_memory() -> (usize, usize) {
    (
        GLOBAL_TRACE_BYTES.load(Ordering::Relaxed),
        GLOBAL_TRACE_BUDGET_BYTES.load(Ordering::Relaxed),
    )
}

/// Try to claim `bytes` from the global trace budget in one atomic decision.
///
/// Both admission sites ([`TraceTap::reserve_global_bytes`] and
/// [`TraceTap::try_feed_chunk`]) go through this, so the arithmetic cannot drift
/// into two shapes again — which is exactly how they diverged before (`PERF-5`,
/// `ISSUE-0007`).
///
/// The compare-and-set is the whole point: a load, compare, then add lets concurrent
/// callers each observe room that only one of them can have, so the sum of grants can
/// exceed the cap. `fetch_update` retries until the update is consistent with an
/// observed value, making the budget a hard ceiling.
///
/// Saturating arithmetic throughout, so a pathological `bytes` value can neither
/// panic in debug nor wrap in release.
fn claim_global_trace_bytes(bytes: usize) -> bool {
    let budget = GLOBAL_TRACE_BUDGET_BYTES.load(Ordering::Relaxed);
    GLOBAL_TRACE_BYTES
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            let next = current.saturating_add(bytes);
            if next > budget { None } else { Some(next) }
        })
        .is_ok()
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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

/// Request body split into an audit skeleton plus ordered content-addressed blocks.
#[derive(Debug, Clone)]
pub struct PreparedRequestBlocks {
    /// Request metadata with prompt-bearing fields replaced in place by
    /// `{"$blk": "<hash>"}` references (or `{"$dropped": true, ...}` markers).
    pub skeleton: serde_json::Value,
    /// Deduplicated `(sha256_hex, canonical_block_bytes)` pairs.
    pub blocks: Vec<(String, Vec<u8>)>,
    /// Ordered block hashes corresponding to every prompt position. Repeated
    /// blocks appear once per position so LCP reconstruction stays aligned.
    pub block_refs: Vec<String>,
    /// Number of oversized or budget-exceeding block positions replaced with markers.
    pub dropped_blocks: u32,
}

#[derive(Debug)]
struct BlockCandidate {
    hash: String,
    bytes: Vec<u8>,
    drop_reason: Option<&'static str>,
}

/// Recursively removes per-turn annotations that must not affect block hashes.
fn strip_volatile_trace_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("cache_control");
            map.remove("citations");
            for child in map.values_mut() {
                strip_volatile_trace_fields(child);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                strip_volatile_trace_fields(child);
            }
        }
        _ => {}
    }
}

/// Normalizes a prompt-bearing field value into the block shape used by the
/// preview API without changing the wire meaning of the stored content.
fn normalize_trace_field_value(field: &str, value: serde_json::Value) -> serde_json::Value {
    match field {
        "system" | "instructions" => serde_json::json!({
            "role": "system",
            "content": value,
        }),
        "messages" | "input" => match value {
            serde_json::Value::String(text) => serde_json::json!({
                "role": "user",
                "content": text,
            }),
            other => other,
        },
        _ => value,
    }
}

/// Split a request into a metadata skeleton and content-addressed message blocks.
///
/// The preview API reconstructs messages by replaying `block_refs` in order.
/// Prompt-bearing fields (`system`, `instructions`, `tools`, `messages`, and
/// `input`) are replaced in place by `{"$blk": "<hash>"}` references while all
/// other top-level request keys (`model`, `temperature`, `max_tokens`, ...) stay
/// intact (TRC-2, TRC-4, Section 12.6).
///
/// `cache_control` and `citations` are stripped recursively before hashing so a
/// moving cache marker cannot defeat deduplication. Binary/base64 assets above
/// 1 KiB are replaced with SHA-256 placeholders. Duplicate blocks are stored once
/// per turn but retain one reference per prompt position. When the turn's unique
/// blocks exceed 4 MiB, selection runs from the newest block backwards so the
/// newest prompt is retained and the oldest prefix is dropped.
#[must_use]
pub fn prepare_request_blocks(request: &serde_json::Value) -> PreparedRequestBlocks {
    let mut skeleton = request.clone();
    let mut field_candidates: Vec<(&'static str, Vec<usize>)> = Vec::new();
    let mut candidates: Vec<BlockCandidate> = Vec::new();

    if let Some(object) = skeleton.as_object_mut() {
        for (field, single) in [
            ("system", true),
            ("instructions", true),
            ("tools", false),
            ("messages", false),
            ("input", false),
        ] {
            let Some(value) = object.remove(field) else {
                continue;
            };
            if value.is_null() {
                object.insert(field.to_string(), serde_json::Value::Null);
                continue;
            }

            let values = if single {
                vec![normalize_trace_field_value(field, value)]
            } else {
                match value {
                    serde_json::Value::Array(items) => items
                        .into_iter()
                        .map(|item| normalize_trace_field_value(field, item))
                        .collect(),
                    other => vec![normalize_trace_field_value(field, other)],
                }
            };

            let mut indices = Vec::with_capacity(values.len());
            for mut value in values {
                strip_volatile_trace_fields(&mut value);
                replace_binary_blocks(&mut value);
                let Ok(bytes) = serde_json::to_vec(&value) else {
                    let index = candidates.len();
                    candidates.push(BlockCandidate {
                        hash: String::new(),
                        bytes: Vec::new(),
                        drop_reason: Some("serialization_failed"),
                    });
                    indices.push(index);
                    continue;
                };
                let hash = hex_sha256(&bytes);
                let index = candidates.len();
                candidates.push(BlockCandidate {
                    hash,
                    bytes,
                    drop_reason: None,
                });
                indices.push(index);
            }
            field_candidates.push((field, indices));
        }
    }

    // Tail-preserving budget selection: walk newest to oldest so the latest turn
    // survives. Duplicate hashes encountered from the tail are already stored and
    // therefore consume no additional budget.
    let mut kept_hashes: HashSet<String> = HashSet::new();
    let mut kept = vec![false; candidates.len()];
    let mut total_bytes = 0_usize;
    for index in (0..candidates.len()).rev() {
        let candidate = &mut candidates[index];
        if candidate.drop_reason.is_some() {
            continue;
        }
        if kept_hashes.contains(&candidate.hash) {
            kept[index] = true;
            continue;
        }
        if candidate.bytes.len() > MAX_REQUEST_BLOCKS_BYTES {
            candidate.drop_reason = Some("oversized_block");
            continue;
        }
        if total_bytes.saturating_add(candidate.bytes.len()) > MAX_TURN_NEW_BLOCKS_BYTES {
            candidate.drop_reason = Some("turn_budget_exceeded");
            continue;
        }
        total_bytes = total_bytes.saturating_add(candidate.bytes.len());
        kept_hashes.insert(candidate.hash.clone());
        kept[index] = true;
    }

    let dropped_blocks = candidates
        .iter()
        .enumerate()
        .filter(|(index, candidate)| !kept[*index] && candidate.drop_reason.is_some())
        .count()
        .try_into()
        .unwrap_or(u32::MAX);

    let block_refs: Vec<String> = candidates
        .iter()
        .enumerate()
        .filter(|(index, _)| kept[*index])
        .map(|(_, candidate)| candidate.hash.clone())
        .collect();

    if let Some(object) = skeleton.as_object_mut() {
        for (field, indices) in &field_candidates {
            let references = indices
                .iter()
                .map(|&index| {
                    let candidate = &candidates[index];
                    if kept[index] {
                        serde_json::json!({ "$blk": candidate.hash })
                    } else {
                        serde_json::json!({
                            "$dropped": true,
                            "reason": candidate.drop_reason.unwrap_or("serialization_failed"),
                            "bytes": candidate.bytes.len(),
                        })
                    }
                })
                .collect();
            object.insert(field.to_string(), serde_json::Value::Array(references));
        }
        object.insert(
            "dropped_block_count".to_string(),
            serde_json::json!(dropped_blocks),
        );
    }

    let mut blocks = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    for (index, candidate) in candidates.into_iter().enumerate() {
        if kept[index] && emitted.insert(candidate.hash.clone()) {
            blocks.push((candidate.hash, candidate.bytes));
        }
    }

    PreparedRequestBlocks {
        skeleton,
        blocks,
        block_refs,
        dropped_blocks,
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
    /// Partial SSE frame carried across upstream chunks.
    sse_pending: Vec<u8>,
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
                sse_pending: Vec::new(),
                output: NormalizedTurnOutput::default(),
            },
            rx,
        )
    }

    /// Feed one parsed SSE `data:` JSON object and reassemble assistant deltas.
    ///
    /// Supports OpenAI Chat Completions, OpenAI Responses, and Anthropic Messages.
    /// Reserve `bytes` against the 256 MiB global trace memory budget (`F014`, `PERF-5`).
    pub fn reserve_global_bytes(&self, bytes: usize) -> bool {
        if bytes == 0 {
            return true;
        }
        if self.trace_dropped.load(Ordering::Acquire) {
            return false;
        }
        if !claim_global_trace_bytes(bytes) {
            self.trace_dropped.store(true, Ordering::Release);
            return false;
        }
        self.allocated_bytes.fetch_add(bytes, Ordering::Relaxed);
        true
    }

    pub fn feed_sse_data_json(&mut self, value: &serde_json::Value) {
        if self.output.truncated {
            return;
        }
        let raw = value.to_string();
        if !self.reserve_global_bytes(raw.len()) {
            self.output.truncated = true;
            return;
        }
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
            Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    self.append_output_text(delta, true);
                }
            }
            Some("response.output_item.added") => {
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
            Some("response.function_call_arguments.delta") => {
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    if self.output.assistant_tool_calls.is_empty() {
                        self.output.assistant_tool_calls.push(AssembledToolCall {
                            id: value
                                .get("item_id")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            name: String::new(),
                            arguments_json: String::new(),
                        });
                    }
                    if let Some(last) = self.output.assistant_tool_calls.last_mut() {
                        last.arguments_json.push_str(delta);
                    }
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
                    if let Some(existing) = self
                        .output
                        .assistant_tool_calls
                        .iter_mut()
                        .rev()
                        .find(|tc| tc.id == id || tc.name == name || tc.id.is_empty())
                    {
                        if !id.is_empty() {
                            existing.id = id;
                        }
                        if !name.is_empty() {
                            existing.name = name;
                        }
                        if !arguments.is_empty() {
                            existing.arguments_json = arguments;
                        }
                    } else {
                        self.output.assistant_tool_calls.push(AssembledToolCall {
                            id,
                            name,
                            arguments_json: arguments,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// Feed raw upstream SSE bytes, reassembling frames split across chunk boundaries.
    ///
    /// Non-JSON frames, comments, and the `[DONE]` sentinel are ignored. An
    /// incomplete frame larger than [`MAX_SSE_PENDING_BYTES`] marks the trace
    /// truncated instead of retaining unbounded memory.
    pub fn feed_sse_chunk(&mut self, chunk: &[u8]) {
        if self.output.truncated {
            return;
        }
        self.sse_pending.extend_from_slice(chunk);
        let mut cursor = 0;
        while let Some(rel_end) = find_frame_delimiter(&self.sse_pending[cursor..]) {
            let end = cursor + rel_end;
            let frame = self.sse_pending[cursor..end].to_vec();
            self.feed_sse_frame(&frame);
            cursor = end;
        }
        if cursor > 0 {
            self.sse_pending.drain(..cursor);
        }
        if self.sse_pending.len() > MAX_SSE_PENDING_BYTES {
            self.sse_pending.clear();
            self.output.truncated = true;
        }
    }

    /// Flush any trailing unterminated SSE frame at stream EOF (`F009`).
    pub fn finish_sse(&mut self) {
        if !self.output.truncated && self.sse_pending.iter().any(|b| !b.is_ascii_whitespace()) {
            let tail = std::mem::take(&mut self.sse_pending);
            self.feed_sse_frame(&tail);
        } else {
            self.sse_pending.clear();
        }
    }

    fn feed_sse_frame(&mut self, frame: &[u8]) {
        let Ok(text) = std::str::from_utf8(frame) else {
            return;
        };
        for line in text.lines() {
            let Some(payload) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
                self.feed_sse_data_json(&value);
            }
        }
    }

    /// Feed a complete (non-streaming) upstream response body.
    ///
    /// Normalizes OpenAI Chat Completions, OpenAI Responses, and Anthropic
    /// Messages into the same [`NormalizedTurnOutput`] shape used for SSE.
    pub fn feed_json_response(&mut self, value: &serde_json::Value) {
        if self.output.truncated {
            return;
        }
        if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
            self.output.response_id = Some(id.to_string());
        }

        if let Some(choice) = value.pointer("/choices/0") {
            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(serde_json::Value::as_str)
            {
                self.output.finish_reason = Some(reason.to_string());
            }
            if let Some(message) = choice.get("message") {
                self.absorb_message(message);
            }
        }

        if let Some(output) = value.get("output") {
            match output {
                serde_json::Value::Array(items) => {
                    for item in items {
                        self.absorb_responses_item(item);
                    }
                }
                serde_json::Value::String(text) => self.append_output_text(text, false),
                _ => {}
            }
            if let Some(status) = value.get("status").and_then(serde_json::Value::as_str) {
                self.output.finish_reason = Some(status.to_string());
            }
        }

        if let Some(blocks) = value.get("content").and_then(serde_json::Value::as_array) {
            for block in blocks {
                self.absorb_content_block(block);
            }
        }
        if let Some(stop_reason) = value.get("stop_reason").and_then(serde_json::Value::as_str) {
            self.output.finish_reason = Some(stop_reason.to_string());
        }
    }

    fn absorb_message(&mut self, message: &serde_json::Value) {
        if let Some(content) = message.get("content") {
            match content {
                serde_json::Value::String(text) => self.append_output_text(text, false),
                serde_json::Value::Array(blocks) => {
                    for block in blocks {
                        self.absorb_content_block(block);
                    }
                }
                _ => {}
            }
        }
        if let Some(reasoning) = message
            .get("reasoning_content")
            .or_else(|| message.get("reasoning"))
            .and_then(serde_json::Value::as_str)
        {
            self.append_output_text(reasoning, true);
        }
        if let Some(calls) = message
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
        {
            for call in calls {
                let function = call.get("function");
                self.output.assistant_tool_calls.push(AssembledToolCall {
                    id: call
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: function
                        .and_then(|f| f.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments_json: function
                        .and_then(|f| f.get("arguments"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            }
        }
    }

    fn absorb_responses_item(&mut self, item: &serde_json::Value) {
        match item.get("type").and_then(serde_json::Value::as_str) {
            Some("message") => {
                if let Some(blocks) = item.get("content").and_then(serde_json::Value::as_array) {
                    for block in blocks {
                        self.absorb_content_block(block);
                    }
                }
            }
            Some("function_call") => {
                self.output.assistant_tool_calls.push(AssembledToolCall {
                    id: item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: item
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments_json: item
                        .get("arguments")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            }
            _ => {}
        }
    }

    fn absorb_content_block(&mut self, block: &serde_json::Value) {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("text" | "output_text" | "input_text") => {
                if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                    self.append_output_text(text, false);
                }
            }
            Some("thinking") => {
                if let Some(text) = block
                    .get("thinking")
                    .or_else(|| block.get("text"))
                    .and_then(serde_json::Value::as_str)
                {
                    self.append_output_text(text, true);
                }
            }
            Some("tool_use") => {
                let input = block
                    .get("input")
                    .map_or_else(String::new, serde_json::Value::to_string);
                self.output.assistant_tool_calls.push(AssembledToolCall {
                    id: block
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments_json: input,
                });
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
        if index >= MAX_TOOL_CALLS_PER_TURN {
            self.output.truncated = true;
            return;
        }
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
        if !claim_global_trace_bytes(chunk_len) {
            self.trace_dropped.store(true, Ordering::Release);
            return false;
        }

        match self.tx.try_send(chunk) {
            Ok(()) => {
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
        // `fetch_sub` on an `AtomicUsize` wraps, and a counter driven to a value near
        // `usize::MAX` makes every subsequent reservation fail — content traceability
        // would switch itself off silently and permanently (`ISSUE-0007`).
        //
        // The floor is a defensive guard, not a correctness proof: under-release
        // leaks the counter upward and degrades traceability toward off, which is the
        // safe direction; over-release would wrap and kill it instantly, which is not.
        //
        // Note this reads `&mut self`, so no concurrent reader of `allocated_bytes`
        // for *this* tap can exist. Replacing the load with a `swap(0)` would only
        // obscure the arithmetic without buying anything.
        let _ = GLOBAL_TRACE_BYTES.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_sub(allocated))
        });
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
