//! Block-hash LCP timeline reconstructor, predecessor-inclusive cursor pagination, and chat preview formatting.
//!
//! Spec: Section 4.2, Section 4.6, Section 6.10, Section 8.2, Section 12.6, Section 12.6.1,
//! Requirements: TRC-5, TRC-8, TRC-9, SEC-17, SEC-19, Partial: TRC-6.

use serde::{Deserialize, Serialize};

/// Maximum decrypted content bytes per page view (8 MiB) (Section 12.6).
pub const MAX_PAGE_DECRYPT_BYTES: usize = 8 * 1024 * 1024;

/// Computes the Longest Common Prefix (LCP) between two block reference hash arrays without decryption (TRC-5).
pub fn compute_lcp(prev_refs: &[String], curr_refs: &[String]) -> usize {
    let mut lcp = 0;
    while lcp < prev_refs.len() && lcp < curr_refs.len() && prev_refs[lcp] == curr_refs[lcp] {
        lcp += 1;
    }
    lcp
}

/// Parses cursor string in format `<ts>:<request_id>`.
pub fn parse_cursor(cursor: &str) -> Option<(i64, String)> {
    let (ts_str, req_id) = cursor.split_once(':')?;
    let ts = ts_str.parse::<i64>().ok()?;
    Some((ts, req_id.to_string()))
}

/// Formats cursor string from timestamp and request identifier.
pub fn format_cursor(ts: i64, request_id: &str) -> String {
    format!("{ts}:{request_id}")
}

/// Neutralizes XSS by escaping HTML special characters in code, tool arguments, and raw text (SEC-19).
pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Neutralizes malicious HTML/JS tags from assistant markdown while preserving clean markup (SEC-19).
pub fn sanitize_assistant_text(text: &str) -> String {
    // 1. Render Markdown with pulldown-cmark with raw HTML tags disabled
    let parser = pulldown_cmark::Parser::new(text);
    let mut html_output = String::new();
    pulldown_cmark::html::push_html(&mut html_output, parser);

    // 2. Strict sanitization with ammonia allowlist per SRDD Section 8.2 (SEC-19)
    let tags = [
        "p",
        "pre",
        "code",
        "em",
        "strong",
        "ul",
        "ol",
        "li",
        "blockquote",
        "a",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "br",
    ];
    let tag_set: std::collections::HashSet<&str> = tags.into_iter().collect();

    let mut builder = ammonia::Builder::new();
    builder
        .tags(tag_set)
        .link_rel(Some("noopener noreferrer nofollow"))
        .url_schemes(["https", "http", "mailto"].into_iter().collect());

    builder.clean(&html_output).to_string()
}

/// Single reconstructed message in the session preview timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewMessage {
    pub role: String,
    pub content: String,
    pub block_hash: String,
    pub is_new: bool,
}

/// Single turn in the session chat preview timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewTurn {
    pub request_id: String,
    pub ts: i64,
    pub canonical_model: String,
    pub protocol: String,
    pub messages: Vec<PreviewMessage>,
    pub assistant_text: String,
    pub assistant_reasoning: Option<String>,
    pub assistant_tool_calls: Vec<serde_json::Value>,
    pub has_load_more_placeholder: bool,
}

/// Reconstructs a sequence of turns into a deduplicated preview timeline (TRC-5).
pub fn reconstruct_timeline(
    turns: &[(&[String], PreviewTurn)],
    max_decrypt_bytes: usize,
) -> Vec<PreviewTurn> {
    let mut timeline = Vec::new();
    let mut decrypted_bytes_accum = 0;

    let mut prev_refs: &[String] = &[];
    for (block_refs, turn) in turns {
        let lcp = compute_lcp(prev_refs, block_refs);
        let mut new_turn = turn.clone();

        // Mark only new blocks beyond LCP as is_new
        for (i, msg) in new_turn.messages.iter_mut().enumerate() {
            msg.is_new = i >= lcp;
            if msg.is_new {
                decrypted_bytes_accum += msg.content.len();
            }
        }

        if decrypted_bytes_accum > max_decrypt_bytes {
            new_turn.has_load_more_placeholder = true;
        }

        timeline.push(new_turn);
        prev_refs = block_refs;
    }

    timeline
}

#[cfg(test)]
pub mod tests;
