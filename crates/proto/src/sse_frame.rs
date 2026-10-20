#![forbid(unsafe_code)]
//! Server-Sent Events (SSE) frame parsing, comment detection, and single-frame usage filtering.
//!
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#1241-frame-filter-inline-sseusagetap-and-bounded-tracetap-execution-model
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#73-transparency-and-compatibility-trn--com
//! Phase: ./docs/bigbang/big-bang-phases-2.md#25-sub-phase-25-unbuffered-sse-forwarder-64-kib-pre-first-frame-holding-buffer-single-frame-filter-and-requestlifecycle-guard

use serde_json::Value;

/// Locate the end position of the first complete SSE frame delimiter (`\n\n` or `\r\n\r\n`).
#[must_use]
pub fn find_frame_delimiter(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 2 {
        return None;
    }

    for i in 0..bytes.len() - 1 {
        if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
            return Some(i + 2);
        }
        if i + 3 < bytes.len()
            && bytes[i] == b'\r'
            && bytes[i + 1] == b'\n'
            && bytes[i + 2] == b'\r'
            && bytes[i + 3] == b'\n'
        {
            return Some(i + 4);
        }
    }

    None
}

/// Determine whether a complete SSE frame consists solely of comments (`:` lines) and whitespace.
#[must_use]
pub fn is_comment_frame(frame: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(frame) else {
        return false;
    };

    let mut had_content = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        had_content = true;
        if !trimmed.starts_with(':') {
            return false;
        }
    }

    had_content
}

/// Determine whether a complete SSE frame is an injected usage frame that must be suppressed per Section 12.4.1.
///
/// Returns `true` iff the single `data:` JSON object has `"choices": []` and an object `"usage"`.
#[must_use]
pub fn is_injected_usage_frame(frame: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(frame) else {
        return false;
    };

    let mut data_payload = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(payload) = trimmed.strip_prefix("data:") {
            let p = payload.trim();
            if p == "[DONE]" {
                return false;
            }
            data_payload = Some(p);
        }
    }

    let Some(raw_json) = data_payload else {
        return false;
    };

    let Ok(val) = serde_json::from_str::<Value>(raw_json) else {
        return false;
    };

    let has_empty_choices = val
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|c| c.is_empty());

    let has_usage_obj = val.get("usage").and_then(Value::as_object).is_some();

    has_empty_choices && has_usage_obj
}

/// Extract an upstream error object from an SSE frame or initial JSON response body if present.
#[must_use]
pub fn extract_frame_error(bytes: &[u8]) -> Option<String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return None;
    };

    // 1. Direct JSON body error: {"error": ...}
    if let Ok(val) = serde_json::from_str::<Value>(text.trim())
        && let Some(err) = val.get("error")
    {
        return Some(err.to_string());
    }

    // 2. SSE data: {"error": ...}
    for line in text.lines() {
        if let Some(payload) = line.trim().strip_prefix("data:")
            && let Ok(val) = serde_json::from_str::<Value>(payload.trim())
            && let Some(err) = val.get("error")
        {
            return Some(err.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comment_frame_detection() {
        assert!(is_comment_frame(b": ping\n\n"));
        assert!(is_comment_frame(b": keep-alive\r\n: second\r\n\r\n"));
        assert!(!is_comment_frame(b"data: hello\n\n"));
        assert!(!is_comment_frame(b": ping\ndata: hello\n\n"));
    }

    #[test]
    fn test_injected_usage_frame_detection() {
        // Injected usage frame: choices: [] and usage: {}
        let injected =
            b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":20}}\n\n";
        assert!(is_injected_usage_frame(injected));

        // Normal stream chunk with choices: NOT injected usage chunk
        let normal = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        assert!(!is_injected_usage_frame(normal));

        // Stream chunk with choices AND usage: NOT suppressed
        let with_both = b"data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":5}}\n\n";
        assert!(!is_injected_usage_frame(with_both));

        // [DONE] frame is not usage frame
        assert!(!is_injected_usage_frame(b"data: [DONE]\n\n"));
    }

    #[test]
    fn test_extract_frame_error() {
        let err_frame = b"data: {\"error\":{\"message\":\"upstream overloaded\",\"type\":\"server_error\"}}\n\n";
        assert!(extract_frame_error(err_frame).is_some());

        let json_err =
            b"{\"error\":{\"message\":\"rate limit exceeded\",\"type\":\"rate_limit_error\"}}";
        assert!(extract_frame_error(json_err).is_some());

        let valid = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        assert!(extract_frame_error(valid).is_none());
    }
}
