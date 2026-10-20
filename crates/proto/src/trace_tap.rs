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
}

impl TraceTap {
    pub fn new() -> (Self, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(256);
        (
            Self {
                tx,
                trace_dropped: Arc::new(AtomicBool::new(false)),
                allocated_bytes: AtomicUsize::new(0),
            },
            rx,
        )
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

#[cfg(test)]
pub mod tests;
