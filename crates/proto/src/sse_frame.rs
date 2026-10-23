//! Pre-first-frameSSE holding buffer and frame-level error classifier.
//!
//! Spec: Section 6.7, Section 6.8, Section 10.5, Section 12.4,
//! Requirements: RTE-8, API-5, API-6, USG-1.

use bytes::BytesMut;

/// Maximum holding buffer size before committing streaming response (64 KiB).
pub const PRE_COMMIT_BUFFER_LIMIT: usize = 64 * 1024;

/// Finds the end index (exclusive) of the first complete SSE frame (`\n\n` or `\r\n\r\n`).
pub fn find_frame_delimiter(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i + 2);
        }
        if i + 3 < buf.len()
            && buf[i] == b'\r'
            && buf[i + 1] == b'\n'
            && buf[i + 2] == b'\r'
            && buf[i + 3] == b'\n'
        {
            return Some(i + 4);
        }
        i += 1;
    }
    None
}

/// Checks if an SSE frame consists solely of comment lines (`: ...`) or `event: ping`.
pub fn is_comment_frame(frame: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(frame) else {
        return false;
    };
    let mut has_lines = false;
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        has_lines = true;
        if trimmed.starts_with(':') || trimmed == "event: ping" || trimmed == "event:ping" {
            continue;
        }
        if trimmed.starts_with("data:") && trimmed.contains("\"type\":\"ping\"") {
            continue;
        }
        return false;
    }
    has_lines
}

fn is_non_null_error_value(err: &serde_json::Value) -> bool {
    err.is_object() || err.as_str().is_some_and(|text| !text.trim().is_empty())
}

/// Checks if an SSE frame represents an upstream error event (`event: error`, `response.failed`,
/// `response.error`, or a non-null top-level/nested `"error"` object).
///
/// OpenAI and OpenRouter `/v1/responses` frames include `"error": null` on normal non-error
/// lifecycle events (`response.created`, `response.in_progress`, `response.completed`); those
/// must never be classified as errors (`F-001`, `API-6`, `RTE-8`).
pub fn extract_frame_error(frame: &[u8]) -> Option<String> {
    let Ok(s) = std::str::from_utf8(frame) else {
        return None;
    };
    let mut is_error_event = false;
    let mut data_payload = String::new();

    for line in s.lines() {
        let trimmed = line.trim();
        if let Some(ev) = trimmed.strip_prefix("event:") {
            let ev_name = ev.trim();
            if ev_name == "error" || ev_name == "response.failed" || ev_name == "response.error" {
                is_error_event = true;
            }
        } else if let Some(d) = trimmed.strip_prefix("data:") {
            if !data_payload.is_empty() {
                data_payload.push('\n');
            }
            data_payload.push_str(d.trim());
        }
    }

    if is_error_event {
        return Some(data_payload);
    }

    if !data_payload.is_empty()
        && data_payload != "[DONE]"
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&data_payload)
    {
        if val.get("type").and_then(|t| t.as_str()) == Some("error")
            || val.get("type").and_then(|t| t.as_str()) == Some("response.failed")
            || val.get("type").and_then(|t| t.as_str()) == Some("response.error")
        {
            return Some(data_payload);
        }
        if let Some(err) = val.get("error")
            && is_non_null_error_value(err)
        {
            return Some(data_payload);
        }
        if let Some(err) = val.pointer("/response/error")
            && is_non_null_error_value(err)
        {
            return Some(data_payload);
        }
    }

    None
}

/// Checks whether a complete SSE frame is an OpenAI injected usage-only frame
/// (`choices: []` and a JSON object `usage: {...}`) on a single `data:` line.
///
/// Used when `strip_injected_usage_chunk == true` so clients that did not ask for
/// `stream_options.include_usage` never observe the trailing usage-only frame
/// while `SseUsageTap` still records the authoritative token counts (`API-5`, `TRN-3`).
/// A multi-`data:` frame is never classified as an injected usage-only chunk (`F-019`).
pub fn is_injected_usage_frame(frame: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(frame) else {
        return false;
    };
    let mut data_lines: Vec<&str> = Vec::new();
    for line in s.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("data:") {
            data_lines.push(rest.trim());
        }
    }
    if data_lines.len() != 1 {
        return false;
    }
    let data = data_lines[0];
    if data.is_empty() || data == "[DONE]" {
        return false;
    }
    let Ok(val) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    let empty_choices = val
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty);
    let has_usage_object = val.get("usage").is_some_and(serde_json::Value::is_object);
    empty_choices && has_usage_object
}

/// Pre-commit decision from the 64 KiB holding buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreCommitDecision {
    /// Keep buffering more bytes from upstream.
    NeedMoreBytes,
    /// Commit response to client and flush buffered bytes.
    Commit,
    /// Upstream sent an initial SSE error frame before any valid content frame; trigger failover/classification.
    InitialFrameError(String),
}

/// 64 KiB holding buffer verifying the first valid non-comment SSE frame before committing headers (RTE-8, API-6).
#[derive(Debug, Default)]
pub struct PreFirstFrameBuffer {
    pub buf: BytesMut,
    pub committed: bool,
    scan_offset: usize,
}

impl PreFirstFrameBuffer {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(8192),
            committed: false,
            scan_offset: 0,
        }
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> PreCommitDecision {
        if self.committed {
            return PreCommitDecision::Commit;
        }
        self.buf.extend_from_slice(chunk);

        loop {
            let slice = &self.buf[self.scan_offset..];
            if let Some(rel_end) = find_frame_delimiter(slice) {
                let frame_end = self.scan_offset + rel_end;
                let frame = &self.buf[self.scan_offset..frame_end];
                self.scan_offset = frame_end;

                if is_comment_frame(frame) {
                    if self.buf.len() >= PRE_COMMIT_BUFFER_LIMIT {
                        self.committed = true;
                        return PreCommitDecision::Commit;
                    }
                    continue;
                }

                if let Some(err_payload) = extract_frame_error(frame) {
                    return PreCommitDecision::InitialFrameError(err_payload);
                }

                // Valid non-comment, non-error frame verified!
                self.committed = true;
                return PreCommitDecision::Commit;
            } else {
                break;
            }
        }

        if self.buf.len() >= PRE_COMMIT_BUFFER_LIMIT {
            self.committed = true;
            return PreCommitDecision::Commit;
        }

        PreCommitDecision::NeedMoreBytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[doc = "Spec: API-5"]
    #[test]
    fn test_is_injected_usage_frame_detects_empty_choices_with_usage_object() {
        let injected =
            b"data: {\"id\":\"chatcmpl-1\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n";
        assert!(is_injected_usage_frame(injected));

        let normal =
            b"data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        assert!(!is_injected_usage_frame(normal));

        let with_both =
            b"data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":10}}\n\n";
        assert!(!is_injected_usage_frame(with_both));

        assert!(!is_injected_usage_frame(b"data: [DONE]\n\n"));

        // Multi-line data frames must never be stripped as a single injected usage chunk (F-019)
        let multi_line = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n"
        );
        assert!(!is_injected_usage_frame(multi_line.as_bytes()));
    }

    #[doc = "Spec: RTE-8, API-6"]
    #[test]
    fn test_pre_first_frame_buffer_comment_error_and_64k_commit() {
        // 1. Keepalive comments do not commit; subsequent error frame triggers InitialFrameError
        let mut buf = PreFirstFrameBuffer::new();
        assert_eq!(
            buf.push_chunk(b": keepalive\n\n"),
            PreCommitDecision::NeedMoreBytes
        );
        assert_eq!(
            buf.push_chunk(b"event: ping\ndata: {\"type\":\"ping\"}\n\n"),
            PreCommitDecision::NeedMoreBytes
        );

        let err_frame = b"event: error\ndata: {\"error\":{\"message\":\"overloaded\"}}\n\n";
        match buf.push_chunk(err_frame) {
            PreCommitDecision::InitialFrameError(msg) => assert!(msg.contains("overloaded")),
            other => panic!("expected InitialFrameError, got {other:?}"),
        }

        // 2. Valid content frame commits immediately
        let mut buf2 = PreFirstFrameBuffer::new();
        assert_eq!(
            buf2.push_chunk(b": openrouter processing\n\n"),
            PreCommitDecision::NeedMoreBytes
        );
        assert_eq!(
            buf2.push_chunk(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n"),
            PreCommitDecision::Commit
        );

        // 3. 64 KiB boundary commits even without frame delimiter
        let mut buf3 = PreFirstFrameBuffer::new();
        let large_chunk = vec![b'a'; PRE_COMMIT_BUFFER_LIMIT];
        assert_eq!(buf3.push_chunk(&large_chunk), PreCommitDecision::Commit);
    }

    #[doc = "Spec: RTE-8, API-6"]
    #[test]
    fn test_responses_null_error_field_commits_while_non_null_error_fails_over() {
        // OpenAI and OpenRouter /v1/responses include `"error": null` on normal frames (F-001)
        let mut buf = PreFirstFrameBuffer::new();
        let created_frame = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\",\"error\":null},\"error\":null}\n\n";
        assert_eq!(buf.push_chunk(created_frame), PreCommitDecision::Commit);

        // Non-null nested response.error or event: response.error triggers InitialFrameError
        let mut buf_err = PreFirstFrameBuffer::new();
        let failed_frame = b"data: {\"type\":\"response.in_progress\",\"response\":{\"id\":\"resp_2\",\"error\":{\"code\":\"server_error\",\"message\":\"boom\"}}}\n\n";
        match buf_err.push_chunk(failed_frame) {
            PreCommitDecision::InitialFrameError(msg) => assert!(msg.contains("server_error")),
            other => panic!("expected InitialFrameError, got {other:?}"),
        }
    }
}
