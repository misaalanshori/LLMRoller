#![forbid(unsafe_code)]
//! Surgical JSON body validation, duplicate-key rejection, and span editing.
//!
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#73-transparency-and-compatibility-trn--com
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#123-surgical-json-body-validation-and-edits
//! Phase: ./docs/bigbang/big-bang-phases-2.md#24-sub-phase-24-zero-copy-surgical-json-span-editor-duplicate-key-guard-and-body-edit-fuzzer

use std::collections::BTreeSet;
use std::ops::Range;

use bytes::Bytes;
use llmroller_core::rbac::PoolTier;
use thiserror::Error;

/// Body editing and JSON envelope errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BodyEditError {
    #[error("body is not valid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("UTF-8 BOM is not allowed (HTTP 400 invalid_request_error)")]
    BomNotAllowed,
    #[error("top-level JSON value must be an object (HTTP 400 invalid_request_error)")]
    NotAnObject,
    #[error("unexpected trailing non-whitespace bytes after JSON object")]
    TrailingBytes,
    #[error("{0}")]
    DuplicateKey(String),
    #[error("invalid type for field: {0}")]
    InvalidType(String),
    #[error("JSON serialization error: {0}")]
    Json(String),
}

/// Chat stream usage reporting mode per Section 12.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatStreamMode {
    #[default]
    Inject,
    Native,
    None,
}

/// Representation of top-level `stream_options` found in request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamOptionsState {
    Null(Range<usize>),
    Object {
        span: Range<usize>,
        include_usage: Option<(bool, Range<usize>)>,
    },
}

/// Top-level field record capturing exact key and value spans with comma boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopLevelField {
    pub key: String,
    pub key_start: usize,
    pub val_start: usize,
    pub val_end: usize,
    pub leading_comma: Option<usize>,
    pub trailing_comma: Option<usize>,
}

/// Scan results from parsing the top-level JSON object.
#[derive(Debug, Clone)]
pub struct TopLevelScan {
    pub opening_brace: usize,
    pub closing_brace: usize,
    pub is_empty_object: bool,
    pub model: Option<(String, Range<usize>)>,
    pub stream: Option<(bool, Range<usize>)>,
    pub previous_response_id: Option<(String, Range<usize>)>,
    pub stream_options: Option<StreamOptionsState>,
    pub session_spans: Vec<(String, Range<usize>)>,
    pub fields: Vec<TopLevelField>,
    pub metadata_span: Option<Range<usize>>,
}

/// Scan top-level object members in a single pass enforcing all validation constraints.
pub fn scan_top_level(bytes: &[u8]) -> Result<TopLevelScan, BodyEditError> {
    if bytes.starts_with(b"\xef\xbb\xbf") {
        return Err(BodyEditError::BomNotAllowed);
    }

    let text = std::str::from_utf8(bytes)?;

    let mut start_idx = 0;
    while start_idx < bytes.len() && bytes[start_idx].is_ascii_whitespace() {
        start_idx += 1;
    }
    if start_idx >= bytes.len() || bytes[start_idx] != b'{' {
        return Err(BodyEditError::NotAnObject);
    }
    let opening_brace = start_idx;

    let mut match_cursor = start_idx;
    let matched_end = scan_delimited(bytes, &mut match_cursor, b'{', b'}')?;
    let closing_brace = matched_end - 1;

    // Verify no non-whitespace trailing bytes exist
    for &b in &bytes[matched_end..] {
        if !b.is_ascii_whitespace() {
            return Err(BodyEditError::TrailingBytes);
        }
    }

    let mut cursor = opening_brace + 1;
    let mut seen_keys = BTreeSet::new();

    let mut model = None;
    let mut stream = None;
    let mut previous_response_id = None;
    let mut stream_options = None;
    let mut session_spans = Vec::new();
    let mut fields: Vec<TopLevelField> = Vec::new();
    let mut metadata_span: Option<Range<usize>> = None;
    let mut last_comma: Option<usize> = None;
    let mut is_empty_object = true;

    while cursor < closing_brace {
        skip_ws(bytes, &mut cursor);
        if cursor >= closing_brace || bytes[cursor] == b'}' {
            break;
        }

        is_empty_object = false;

        // Parse key string
        if bytes[cursor] != b'"' {
            return Err(BodyEditError::NotAnObject);
        }
        let key_start = cursor;
        let key_end = scan_string(bytes, &mut cursor)?;
        let raw_key_slice = &text[key_start..key_end];
        let unescaped_key: String =
            serde_json::from_str(raw_key_slice).map_err(|e| BodyEditError::Json(e.to_string()))?;

        // Duplicate-key check
        if !seen_keys.insert(unescaped_key.clone()) {
            let cleaned: String = unescaped_key
                .chars()
                .filter(|c| !c.is_control())
                .take(64)
                .collect();
            return Err(BodyEditError::DuplicateKey(format!(
                "duplicate top-level JSON key: {cleaned}"
            )));
        }

        // Colon separator
        skip_ws(bytes, &mut cursor);
        if cursor >= closing_brace || bytes[cursor] != b':' {
            return Err(BodyEditError::NotAnObject);
        }
        cursor += 1; // skip ':'
        skip_ws(bytes, &mut cursor);

        // Parse value
        let val_start = cursor;
        if cursor >= closing_brace {
            return Err(BodyEditError::NotAnObject);
        }

        let val_first = bytes[cursor];
        match val_first {
            b'"' => {
                let val_end = scan_string(bytes, &mut cursor)?;
                let raw_val = &text[val_start..val_end];
                let unescaped_val: String = serde_json::from_str(raw_val)
                    .map_err(|e| BodyEditError::Json(e.to_string()))?;

                if unescaped_key == "model" {
                    model = Some((unescaped_val, val_start..val_end));
                } else if unescaped_key == "previous_response_id" {
                    previous_response_id = Some((unescaped_val, val_start..val_end));
                } else if unescaped_key == "stream" {
                    return Err(BodyEditError::InvalidType(
                        "stream must be a boolean".into(),
                    ));
                } else if is_session_key(&unescaped_key) {
                    session_spans.push((unescaped_key.clone(), key_start..val_end));
                }
            }
            b't' | b'f' => {
                let val_end = scan_ident(bytes, &mut cursor);
                let ident = &text[val_start..val_end];
                if ident == "true" {
                    if unescaped_key == "stream" {
                        stream = Some((true, val_start..val_end));
                    }
                } else if ident == "false" {
                    if unescaped_key == "stream" {
                        stream = Some((false, val_start..val_end));
                    }
                } else {
                    return Err(BodyEditError::NotAnObject);
                }

                if unescaped_key == "model" {
                    return Err(BodyEditError::InvalidType("model must be a string".into()));
                } else if unescaped_key == "previous_response_id" {
                    return Err(BodyEditError::InvalidType(
                        "previous_response_id must be a string".into(),
                    ));
                }
            }
            b'n' => {
                let val_end = scan_ident(bytes, &mut cursor);
                if &text[val_start..val_end] != "null" {
                    return Err(BodyEditError::NotAnObject);
                }
                if unescaped_key == "stream_options" {
                    stream_options = Some(StreamOptionsState::Null(val_start..val_end));
                } else if unescaped_key == "model" {
                    return Err(BodyEditError::InvalidType("model must be a string".into()));
                } else if unescaped_key == "stream" {
                    return Err(BodyEditError::InvalidType(
                        "stream must be a boolean".into(),
                    ));
                }
            }
            b'{' => {
                let val_end = scan_delimited(bytes, &mut cursor, b'{', b'}')?;
                if unescaped_key == "model" {
                    return Err(BodyEditError::InvalidType("model must be a string".into()));
                } else if unescaped_key == "stream" {
                    return Err(BodyEditError::InvalidType(
                        "stream must be a boolean".into(),
                    ));
                } else if unescaped_key == "previous_response_id" {
                    return Err(BodyEditError::InvalidType(
                        "previous_response_id must be a string".into(),
                    ));
                } else if unescaped_key == "stream_options" {
                    let obj_slice = &bytes[val_start..val_end];
                    let usage_opt = scan_stream_options_obj(obj_slice, val_start)?;
                    stream_options = Some(StreamOptionsState::Object {
                        span: val_start..val_end,
                        include_usage: usage_opt,
                    });
                } else if unescaped_key == "metadata" {
                    metadata_span = Some(val_start..val_end);
                } else if is_session_key(&unescaped_key) {
                    session_spans.push((unescaped_key.clone(), key_start..val_end));
                }
            }
            b'[' => {
                let val_end = scan_delimited(bytes, &mut cursor, b'[', b']')?;
                if unescaped_key == "model" {
                    return Err(BodyEditError::InvalidType("model must be a string".into()));
                } else if unescaped_key == "stream" {
                    return Err(BodyEditError::InvalidType(
                        "stream must be a boolean".into(),
                    ));
                } else if unescaped_key == "previous_response_id" {
                    return Err(BodyEditError::InvalidType(
                        "previous_response_id must be a string".into(),
                    ));
                } else if is_session_key(&unescaped_key) {
                    session_spans.push((unescaped_key.clone(), key_start..val_end));
                }
            }
            b'0'..=b'9' | b'-' => {
                let val_end = scan_number(bytes, &mut cursor);
                if unescaped_key == "model" {
                    return Err(BodyEditError::InvalidType("model must be a string".into()));
                } else if unescaped_key == "stream" {
                    return Err(BodyEditError::InvalidType(
                        "stream must be a boolean".into(),
                    ));
                } else if unescaped_key == "previous_response_id" {
                    return Err(BodyEditError::InvalidType(
                        "previous_response_id must be a string".into(),
                    ));
                }
                cursor = val_end;
            }
            _ => return Err(BodyEditError::NotAnObject),
        }

        let val_end = cursor;

        skip_ws(bytes, &mut cursor);
        let mut next_comma = None;
        if cursor < closing_brace {
            if bytes[cursor] == b',' {
                next_comma = Some(cursor);
                cursor += 1;
            } else if bytes[cursor] == b'}' {
                // finished object
            } else {
                return Err(BodyEditError::NotAnObject);
            }
        }

        fields.push(TopLevelField {
            key: unescaped_key,
            key_start,
            val_start,
            val_end,
            leading_comma: last_comma,
            trailing_comma: next_comma,
        });
        last_comma = next_comma;
    }

    Ok(TopLevelScan {
        opening_brace,
        closing_brace,
        is_empty_object,
        model,
        stream,
        previous_response_id,
        stream_options,
        session_spans,
        fields,
        metadata_span,
    })
}

/// Surgically edit the JSON body per Section 12.3.
pub fn edit_body(
    original: Bytes,
    tier: PoolTier,
    target_model: Option<&str>,
    chat_stream_mode: ChatStreamMode,
    session_field: Option<(&str, &str)>,
) -> Result<(Bytes, bool), BodyEditError> {
    let scan = scan_top_level(&original)?;

    // Check if any edits are needed
    let model_needs_rewrite = if let Some(target) = target_model {
        scan.model.as_ref().is_none_or(|(m, _)| m != target)
    } else {
        false
    };

    let is_streaming = scan.stream.as_ref().is_some_and(|(s, _)| *s);
    let stream_inject_needed = is_streaming && chat_stream_mode == ChatStreamMode::Inject;

    let stream_options_action = if stream_inject_needed {
        match &scan.stream_options {
            None => StreamOptionsAction::InsertField,
            Some(StreamOptionsState::Null(span)) => StreamOptionsAction::ReplaceNull(span.clone()),
            Some(StreamOptionsState::Object {
                span,
                include_usage: None,
            }) => StreamOptionsAction::InsertIntoObject(span.start),
            Some(StreamOptionsState::Object {
                include_usage: Some((false, val_span)),
                ..
            }) => StreamOptionsAction::ReplaceFalseWithTrue(val_span.clone()),
            Some(StreamOptionsState::Object {
                include_usage: Some((true, _)),
                ..
            }) => StreamOptionsAction::None,
        }
    } else {
        StreamOptionsAction::None
    };

    let strip_injected_usage = !matches!(stream_options_action, StreamOptionsAction::None);

    let session_edits_needed = match tier {
        PoolTier::Own => {
            if let Some((field_name, _)) = session_field {
                !scan.fields.iter().any(|f| f.key == field_name)
            } else {
                false
            }
        }
        _ => {
            let has_top_session = scan.fields.iter().any(|f| is_session_key(&f.key));
            let has_meta_session = scan.metadata_span.as_ref().is_some_and(|range| {
                serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(
                    &original[range.clone()],
                )
                .is_ok_and(|m| m.contains_key("session_id") || m.contains_key("user_id"))
            });
            has_top_session || has_meta_session || session_field.is_some()
        }
    };

    // TRN-1 check: If no edits apply, return the original buffer with identical pointer identity
    if !model_needs_rewrite
        && stream_options_action == StreamOptionsAction::None
        && !session_edits_needed
    {
        return Ok((original, false));
    }

    // Apply edits
    let mut buf = Vec::with_capacity(original.len() + 128);
    let orig_bytes = &original[..];

    let session_fields_to_remove_count = if tier != PoolTier::Own {
        scan.fields
            .iter()
            .filter(|f| is_session_key(&f.key))
            .count()
    } else {
        0
    };
    let remaining_fields_count = scan
        .fields
        .len()
        .saturating_sub(session_fields_to_remove_count);

    // 1. Copy up to opening brace + 1
    buf.extend_from_slice(&orig_bytes[..scan.opening_brace + 1]);

    let mut inserted_something = false;

    // 2. Insert new top-level members right after {
    // (a) Insert stream_options if needed
    if stream_options_action == StreamOptionsAction::InsertField {
        buf.extend_from_slice(b"\"stream_options\":{\"include_usage\":true}");
        inserted_something = true;
    }

    // (b) Insert session field if needed
    if let Some((field_name, token)) = session_field {
        let should_insert = match tier {
            PoolTier::Own => !scan.fields.iter().any(|f| f.key == field_name),
            _ => true,
        };
        if should_insert {
            if inserted_something {
                buf.push(b',');
            }
            let truncated_token = truncate_utf8(token, 256);
            let val_json = serde_json::to_string(truncated_token)
                .map_err(|e| BodyEditError::Json(e.to_string()))?;
            buf.extend_from_slice(format!("\"{field_name}\":{val_json}").as_bytes());
            inserted_something = true;
        }
    }

    if inserted_something && remaining_fields_count > 0 {
        buf.push(b',');
    }

    // 3. Process existing spans between opening { and closing }
    let mut replacements: Vec<(Range<usize>, Vec<u8>)> = Vec::new();

    // Model replacement
    if let (Some(target), Some((_, span))) = (target_model, &scan.model) {
        let escaped =
            serde_json::to_string(target).map_err(|e| BodyEditError::Json(e.to_string()))?;
        replacements.push((span.clone(), escaped.into_bytes()));
    }

    // Stream options replacement
    match stream_options_action {
        StreamOptionsAction::ReplaceNull(span) => {
            replacements.push((span, b"{\"include_usage\":true}".to_vec()));
        }
        StreamOptionsAction::ReplaceFalseWithTrue(span) => {
            replacements.push((span, b"true".to_vec()));
        }
        StreamOptionsAction::InsertIntoObject(obj_start) => {
            let brace_idx = orig_bytes[obj_start..]
                .iter()
                .position(|&b| b == b'{')
                .map(|p| obj_start + p + 1)
                .unwrap_or(obj_start + 1);
            let is_empty_st_obj = orig_bytes[brace_idx..]
                .iter()
                .find(|&&b| !b.is_ascii_whitespace())
                .is_some_and(|&b| b == b'}');
            let insertion = if is_empty_st_obj {
                b"\"include_usage\":true".to_vec()
            } else {
                b"\"include_usage\":true,".to_vec()
            };
            replacements.push((brace_idx..brace_idx, insertion));
        }
        _ => {}
    }

    // Metadata sanitization on Shared/Global tier
    if tier != PoolTier::Own
        && let Some(meta_range) = &scan.metadata_span
        && let Ok(mut map) = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(
            &orig_bytes[meta_range.clone()],
        )
    {
        let removed_session = map.remove("session_id").is_some();
        let removed_user = map.remove("user_id").is_some();
        if removed_session || removed_user {
            let new_meta_bytes = serde_json::to_vec(&map).unwrap_or_else(|_| b"{}".to_vec());
            replacements.push((meta_range.clone(), new_meta_bytes));
        }
    }

    // Remove top-level session spans on Shared/Global tier
    if tier != PoolTier::Own {
        let mut i = 0;
        while i < scan.fields.len() {
            if is_session_key(&scan.fields[i].key) {
                let start_idx = i;
                while i < scan.fields.len() && is_session_key(&scan.fields[i].key) {
                    i += 1;
                }
                let end_idx = i - 1;
                let first = &scan.fields[start_idx];
                let last = &scan.fields[end_idx];

                let range_to_remove = if let Some(comma_pos) = first.leading_comma {
                    comma_pos..last.val_end
                } else if let Some(comma_pos) = last.trailing_comma {
                    let mut end = comma_pos + 1;
                    while end < scan.closing_brace && orig_bytes[end].is_ascii_whitespace() {
                        end += 1;
                    }
                    first.key_start..end
                } else {
                    first.key_start..last.val_end
                };
                replacements.push((range_to_remove, Vec::new()));
            } else {
                i += 1;
            }
        }
    }

    // Sort replacements by start index
    replacements.sort_by_key(|r| r.0.start);

    let mut cursor = scan.opening_brace + 1;
    for (range, repl) in replacements {
        if range.start >= cursor {
            buf.extend_from_slice(&orig_bytes[cursor..range.start]);
            buf.extend_from_slice(&repl);
            cursor = range.end;
        }
    }

    buf.extend_from_slice(&orig_bytes[cursor..]);

    Ok((Bytes::from(buf), strip_injected_usage))
}

#[derive(Debug, PartialEq, Eq)]
enum StreamOptionsAction {
    None,
    InsertField,
    ReplaceNull(Range<usize>),
    InsertIntoObject(usize),
    ReplaceFalseWithTrue(Range<usize>),
}

fn is_session_key(key: &str) -> bool {
    matches!(key, "session_id" | "prompt_cache_key" | "promptCacheKey")
}

fn skip_ws(bytes: &[u8], cursor: &mut usize) {
    while *cursor < bytes.len() && bytes[*cursor].is_ascii_whitespace() {
        *cursor += 1;
    }
}

fn scan_string(bytes: &[u8], cursor: &mut usize) -> Result<usize, BodyEditError> {
    *cursor += 1; // skip opening quote
    let mut in_escape = false;

    while *cursor < bytes.len() {
        let b = bytes[*cursor];
        if in_escape {
            in_escape = false;
        } else if b == b'\\' {
            in_escape = true;
        } else if b == b'"' {
            *cursor += 1; // skip closing quote
            return Ok(*cursor);
        }
        *cursor += 1;
    }
    Err(BodyEditError::NotAnObject)
}

fn scan_ident(bytes: &[u8], cursor: &mut usize) -> usize {
    while *cursor < bytes.len() && (bytes[*cursor].is_ascii_alphabetic() || bytes[*cursor] == b'_')
    {
        *cursor += 1;
    }
    *cursor
}

fn scan_number(bytes: &[u8], cursor: &mut usize) -> usize {
    while *cursor < bytes.len()
        && (bytes[*cursor].is_ascii_digit()
            || matches!(bytes[*cursor], b'-' | b'+' | b'.' | b'e' | b'E'))
    {
        *cursor += 1;
    }
    *cursor
}

fn scan_delimited(
    bytes: &[u8],
    cursor: &mut usize,
    open: u8,
    close: u8,
) -> Result<usize, BodyEditError> {
    let mut depth = 0;
    while *cursor < bytes.len() {
        let b = bytes[*cursor];
        if b == b'"' {
            scan_string(bytes, cursor)?;
            continue;
        }
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                *cursor += 1;
                return Ok(*cursor);
            }
        }
        *cursor += 1;
    }
    Err(BodyEditError::NotAnObject)
}

fn scan_stream_options_obj(
    obj_slice: &[u8],
    offset: usize,
) -> Result<Option<(bool, Range<usize>)>, BodyEditError> {
    let text = std::str::from_utf8(obj_slice).map_err(BodyEditError::Utf8)?;
    let mut cursor = 1; // skip {
    let len = obj_slice.len();

    while cursor < len {
        skip_ws(obj_slice, &mut cursor);
        if cursor >= len || obj_slice[cursor] == b'}' {
            break;
        }
        if obj_slice[cursor] != b'"' {
            break;
        }
        let key_start = cursor;
        let key_end = scan_string(obj_slice, &mut cursor)?;
        let key_unescaped: String = serde_json::from_str(&text[key_start..key_end])
            .map_err(|e| BodyEditError::Json(e.to_string()))?;

        skip_ws(obj_slice, &mut cursor);
        if cursor >= len || obj_slice[cursor] != b':' {
            break;
        }
        cursor += 1;
        skip_ws(obj_slice, &mut cursor);

        let val_start = cursor;
        if key_unescaped == "include_usage" {
            let val_end = scan_ident(obj_slice, &mut cursor);
            let val_str = &text[val_start..val_end];
            if val_str == "true" {
                return Ok(Some((true, offset + val_start..offset + val_end)));
            } else if val_str == "false" {
                return Ok(Some((false, offset + val_start..offset + val_end)));
            }
        } else {
            // skip other values
            match obj_slice[cursor] {
                b'"' => {
                    let _ = scan_string(obj_slice, &mut cursor)?;
                }
                b'{' => {
                    let _ = scan_delimited(obj_slice, &mut cursor, b'{', b'}')?;
                }
                b'[' => {
                    let _ = scan_delimited(obj_slice, &mut cursor, b'[', b']')?;
                }
                _ => {
                    let _ = scan_ident(obj_slice, &mut cursor);
                }
            }
        }

        skip_ws(obj_slice, &mut cursor);
        if cursor < len && obj_slice[cursor] == b',' {
            cursor += 1;
        }
    }
    Ok(None)
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut boundary = 0;
    for (idx, _) in s.char_indices() {
        if idx > max_bytes {
            break;
        }
        boundary = idx;
    }
    &s[..boundary]
}

#[cfg(test)]
mod proptests;
