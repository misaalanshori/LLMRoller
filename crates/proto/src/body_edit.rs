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

impl From<llmroller_core::provider::ChatStreamSetting> for ChatStreamMode {
    fn from(value: llmroller_core::provider::ChatStreamSetting) -> Self {
        match value {
            llmroller_core::provider::ChatStreamSetting::Inject => Self::Inject,
            llmroller_core::provider::ChatStreamSetting::Native => Self::Native,
            llmroller_core::provider::ChatStreamSetting::None => Self::None,
        }
    }
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


/// Scan top-level JSON request and fall back to nested `/request/model`, `/config/model`, `/modelId`,
/// or URL-path model hints (e.g. `/v1beta/models/{model}:generateContent` or `:streamGenerateContent`) (`FND-015`).
pub fn scan_top_level_with_fallback(
    bytes: &[u8],
    path_hint: Option<&str>,
) -> Result<TopLevelScan, BodyEditError> {
    let mut scan = scan_top_level(bytes)?;
    if scan.model.is_none() || scan.stream.is_none() {
        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(bytes) {
            if scan.model.is_none() {
                if let Some(m) = val
                    .get("modelId")
                    .or_else(|| val.pointer("/request/model"))
                    .or_else(|| val.pointer("/config/model"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    scan.model = Some((m.to_string(), 0..0));
                }
            }
            if scan.stream.is_none() {
                if let Some(st) = val
                    .pointer("/request/stream")
                    .or_else(|| val.pointer("/params/stream"))
                    .and_then(serde_json::Value::as_bool)
                {
                    scan.stream = Some((st, 0..0));
                }
            }
        }
    }
    if let Some(path) = path_hint {
        if scan.model.is_none() {
            if let Some(m) = extract_model_from_url_path(path) {
                scan.model = Some((m, 0..0));
            }
        }
        if scan.stream.is_none() && (path.contains(":streamGenerateContent") || path.contains("/converse-stream")) {
            scan.stream = Some((true, 0..0));
        }
    }
    Ok(scan)
}

/// Extract a model identifier from Gemini or Bedrock URL paths (`/models/{model}:generateContent` or `/model/{model}/converse`).
#[must_use]
pub fn extract_model_from_url_path(path: &str) -> Option<String> {
    let without_query = path.split('?').next().unwrap_or(path);
    for marker in ["/models/", "/model/"] {
        if let Some(idx) = without_query.find(marker) {
            let after = &without_query[idx + marker.len()..];
            let segment = after.split('/').next().unwrap_or(after);
            let model = segment.split(':').next().unwrap_or(segment).trim();
            if !model.is_empty() {
                return Some(model.to_string());
            }
        }
    }
    None
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
    edit_body_with_session_limit(
        original,
        tier,
        target_model,
        chat_stream_mode,
        session_field,
        256,
    )
}

/// Surgical JSON body edits with an explicit session affinity value limit.
///
/// `session_max_len` is the provider's `session_profile.max_len` (SRDD Section
/// 12.3): the inserted value is truncated to the largest UTF-8 boundary at or
/// below this byte count without re-serializing the rest of the payload.
pub fn edit_body_with_session_limit(
    original: Bytes,
    tier: PoolTier,
    target_model: Option<&str>,
    chat_stream_mode: ChatStreamMode,
    session_field: Option<(&str, &str)>,
    session_max_len: usize,
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

    let encoded_session_field = if let Some((field_name, token)) = session_field {
        let truncated_token = truncate_utf8(token, session_max_len);
        let val_json = serde_json::to_string(truncated_token)
            .map_err(|e| BodyEditError::Json(e.to_string()))?;
        Some((field_name, val_json))
    } else {
        None
    };

    let session_edits_needed = match tier {
        PoolTier::Own => {
            if let Some((field_name, ref val_json)) = encoded_session_field {
                match scan.fields.iter().find(|f| f.key == field_name) {
                    None => true,
                    Some(existing) => {
                        &original[existing.val_start..existing.val_end] != val_json.as_bytes()
                    }
                }
            } else {
                false
            }
        }
        _ => {
            let has_top_session = scan
                .fields
                .iter()
                .any(|f| is_removed_session_key(&f.key, session_field));
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
            .filter(|f| is_removed_session_key(&f.key, session_field))
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
    if let Some((field_name, ref val_json)) = encoded_session_field {
        let should_insert = match tier {
            PoolTier::Own => !scan.fields.iter().any(|f| f.key == field_name),
            _ => true,
        };
        if should_insert {
            if inserted_something {
                buf.push(b',');
            }
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

    // Own-tier session field value override when higher-precedence header resolved a different session ID (F-016)
    if tier == PoolTier::Own
        && let Some((field_name, ref val_json)) = encoded_session_field
        && let Some(existing) = scan.fields.iter().find(|f| f.key == field_name)
        && &orig_bytes[existing.val_start..existing.val_end] != val_json.as_bytes()
    {
        replacements.push((
            existing.val_start..existing.val_end,
            val_json.as_bytes().to_vec(),
        ));
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

    // Remove top-level session spans on Shared/Global tier (including custom session_field key, F-008)
    if tier != PoolTier::Own {
        let mut i = 0;
        while i < scan.fields.len() {
            if is_removed_session_key(&scan.fields[i].key, session_field) {
                let start_idx = i;
                while i < scan.fields.len()
                    && is_removed_session_key(&scan.fields[i].key, session_field)
                {
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

fn is_removed_session_key(key: &str, session_field: Option<(&str, &str)>) -> bool {
    is_session_key(key) || session_field.is_some_and(|(field_name, _)| key == field_name)
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
    let mut seen_keys = BTreeSet::new();
    let mut include_usage_found: Option<(bool, Range<usize>)> = None;

    while cursor < len {
        skip_ws(obj_slice, &mut cursor);
        if cursor >= len || obj_slice[cursor] == b'}' {
            break;
        }
        if obj_slice[cursor] != b'"' {
            return Err(BodyEditError::NotAnObject);
        }
        let key_start = cursor;
        let key_end = scan_string(obj_slice, &mut cursor)?;
        let key_unescaped: String = serde_json::from_str(&text[key_start..key_end])
            .map_err(|e| BodyEditError::Json(e.to_string()))?;

        if !seen_keys.insert(key_unescaped.clone()) {
            return Err(BodyEditError::DuplicateKey(format!(
                "duplicate stream_options JSON key: {key_unescaped}"
            )));
        }

        skip_ws(obj_slice, &mut cursor);
        if cursor >= len || obj_slice[cursor] != b':' {
            return Err(BodyEditError::NotAnObject);
        }
        cursor += 1;
        skip_ws(obj_slice, &mut cursor);
        if cursor >= len {
            return Err(BodyEditError::NotAnObject);
        }

        let val_start = cursor;
        if key_unescaped == "include_usage" {
            let val_end = scan_ident(obj_slice, &mut cursor);
            let val_str = &text[val_start..val_end];
            if val_str == "true" {
                include_usage_found = Some((true, offset + val_start..offset + val_end));
            } else if val_str == "false" {
                include_usage_found = Some((false, offset + val_start..offset + val_end));
            } else {
                return Err(BodyEditError::InvalidType(
                    "stream_options.include_usage must be a boolean".into(),
                ));
            }
        } else {
            // Skip any valid JSON value (including numbers, booleans, null, strings, objects, arrays; F-007)
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
                b'0'..=b'9' | b'-' => {
                    let _ = scan_number(obj_slice, &mut cursor);
                }
                b't' | b'f' | b'n' => {
                    let val_end = scan_ident(obj_slice, &mut cursor);
                    let ident = &text[val_start..val_end];
                    if !matches!(ident, "true" | "false" | "null") {
                        return Err(BodyEditError::NotAnObject);
                    }
                }
                _ => return Err(BodyEditError::NotAnObject),
            }
        }

        skip_ws(obj_slice, &mut cursor);
        if cursor < len && obj_slice[cursor] == b',' {
            cursor += 1;
        }
    }
    Ok(include_usage_found)
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

/// Sanitize a `/v1/responses` JSON payload when failing over across upstream keys or accounts (`F005`).
///
/// Strips `previous_response_id` when `strip_previous_response_id` is true, and removes
/// cross-account `encrypted_content` and orphan `item_reference` / empty `reasoning` items
/// from `input[]` so the failover upstream does not reject Account A's organization-encrypted
/// items with `400 invalid_encrypted_content`.
#[must_use]
pub fn sanitize_responses_failover_body(body: Bytes, strip_previous_response_id: bool) -> Bytes {
    let Ok(mut val) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(obj) = val.as_object_mut() else {
        return body;
    };
    let mut changed = false;
    if strip_previous_response_id && obj.remove("previous_response_id").is_some() {
        changed = true;
    }
    if let Some(serde_json::Value::Array(items)) = obj.get_mut("input") {
        let orig_len = items.len();
        items.retain_mut(|item| {
            let Some(item_obj) = item.as_object_mut() else {
                return true;
            };
            let is_item_reference =
                item_obj.get("type").and_then(serde_json::Value::as_str) == Some("item_reference");
            if is_item_reference {
                return false;
            }
            let is_reasoning =
                item_obj.get("type").and_then(serde_json::Value::as_str) == Some("reasoning");
            if item_obj.remove("encrypted_content").is_some() {
                changed = true;
            }
            if is_reasoning {
                let has_summary = item_obj
                    .get("summary")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|arr| !arr.is_empty());
                let has_content = item_obj
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|arr| !arr.is_empty());
                return has_summary || has_content;
            }
            true
        });
        if items.len() != orig_len {
            changed = true;
        }
    }
    if !changed {
        return body;
    }
    serde_json::to_vec(&val).map_or(body, Bytes::from)
}

/// Deterministically normalize a tool call ID into a 9-character ASCII alphanumeric string
/// (`^[a-zA-Z0-9]{9}$`) required by Mistral-family Chat Completions endpoints (`F007`).
#[must_use]
pub fn normalize_mistral_tool_call_id(raw_id: &str) -> String {
    if raw_id.len() == 9 && raw_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return raw_id.to_string();
    }
    let hex = crate::trace_tap::hex_sha256(raw_id.as_bytes());
    hex.chars().take(9).collect()
}

/// Apply provider-configured OpenAI Chat Completions compatibility normalizations (`F007`).
pub fn apply_openai_completions_compat(
    value: &mut serde_json::Value,
    compat: &llmroller_core::provider::OpenAiCompatConfig,
) -> bool {
    use llmroller_core::provider::MaxTokensField;
    let Some(obj) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;

    if compat.strip_store && obj.remove("store").is_some() {
        changed = true;
    }
    if compat.strip_reasoning_effort {
        if obj.remove("reasoning_effort").is_some() {
            changed = true;
        }
        if obj.remove("reasoning").is_some() {
            changed = true;
        }
    }
    match compat.max_tokens_field {
        Some(MaxTokensField::MaxTokens) => {
            if let Some(v) = obj.remove("max_completion_tokens") {
                obj.entry("max_tokens".to_string()).or_insert(v);
                changed = true;
            }
        }
        Some(MaxTokensField::MaxCompletionTokens) => {
            if let Some(v) = obj.remove("max_tokens") {
                obj.entry("max_completion_tokens".to_string()).or_insert(v);
                changed = true;
            }
        }
        None => {}
    }

    if (compat.rewrite_developer_role_to_system || compat.normalize_mistral_tool_ids)
        && let Some(serde_json::Value::Array(messages)) = obj.get_mut("messages")
    {
        for msg in messages {
            let Some(msg_obj) = msg.as_object_mut() else {
                continue;
            };
            if compat.rewrite_developer_role_to_system
                && msg_obj.get("role").and_then(serde_json::Value::as_str) == Some("developer")
            {
                msg_obj.insert(
                    "role".to_string(),
                    serde_json::Value::String("system".to_string()),
                );
                changed = true;
            }
            if compat.normalize_mistral_tool_ids {
                if let Some(tc_id) = msg_obj
                    .get("tool_call_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                {
                    let norm = normalize_mistral_tool_call_id(&tc_id);
                    if norm != tc_id {
                        msg_obj.insert("tool_call_id".to_string(), serde_json::Value::String(norm));
                        changed = true;
                    }
                }
                if let Some(serde_json::Value::Array(tool_calls)) = msg_obj.get_mut("tool_calls") {
                    for tc in tool_calls {
                        if let Some(tc_obj) = tc.as_object_mut()
                            && let Some(id_str) = tc_obj
                                .get("id")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        {
                            let norm = normalize_mistral_tool_call_id(&id_str);
                            if norm != id_str {
                                tc_obj.insert("id".to_string(), serde_json::Value::String(norm));
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
    }

    changed
}

/// Apply declarative JSON body edit rules supporting dot-separated paths, `Value::Null` intermediate
/// object promotion, and `*` array wildcards (`F024`).
pub fn apply_body_edit_rules(
    value: &mut serde_json::Value,
    rules: &[llmroller_core::provider::BodyEditRule],
) -> bool {
    use llmroller_core::provider::BodyEditOp;
    let mut changed = false;
    for rule in rules {
        let segments: Vec<&str> = rule
            .path
            .split('.')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if segments.is_empty() {
            continue;
        }
        match rule.op {
            BodyEditOp::Set => {
                if let Some(ref v) = rule.value
                    && apply_path_set_or_default(value, &segments, v, false)
                {
                    changed = true;
                }
            }
            BodyEditOp::Default => {
                if let Some(ref v) = rule.value
                    && apply_path_set_or_default(value, &segments, v, true)
                {
                    changed = true;
                }
            }
            BodyEditOp::Delete => {
                if apply_path_delete(value, &segments) {
                    changed = true;
                }
            }
            BodyEditOp::Move => {
                if let Some(ref from_path) = rule.from {
                    let from_segments: Vec<&str> = from_path
                        .split('.')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect();
                    if let Some(extracted) = take_path_value(value, &from_segments) {
                        let _ = apply_path_set_or_default(value, &segments, &extracted, false);
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

fn apply_path_set_or_default(
    current: &mut serde_json::Value,
    segments: &[&str],
    val: &serde_json::Value,
    only_if_absent_or_null: bool,
) -> bool {
    let Some((&first, rest)) = segments.split_first() else {
        return false;
    };
    if first == "*" {
        if let serde_json::Value::Array(items) = current {
            let mut any = false;
            for item in items.iter_mut() {
                if rest.is_empty() {
                    if !only_if_absent_or_null || item.is_null() {
                        *item = val.clone();
                        any = true;
                    }
                } else if apply_path_set_or_default(item, rest, val, only_if_absent_or_null) {
                    any = true;
                }
            }
            return any;
        }
        return false;
    }

    // Promote intermediate Null to Object (`F024`)
    if current.is_null() {
        *current = serde_json::Value::Object(serde_json::Map::new());
    }
    let Some(map) = current.as_object_mut() else {
        return false;
    };

    if rest.is_empty() {
        if only_if_absent_or_null {
            match map.get(first) {
                None | Some(serde_json::Value::Null) => {
                    map.insert(first.to_string(), val.clone());
                    true
                }
                _ => false,
            }
        } else {
            map.insert(first.to_string(), val.clone());
            true
        }
    } else {
        let entry = map
            .entry(first.to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if entry.is_null() {
            *entry = serde_json::Value::Object(serde_json::Map::new());
        }
        apply_path_set_or_default(entry, rest, val, only_if_absent_or_null)
    }
}

fn apply_path_delete(current: &mut serde_json::Value, segments: &[&str]) -> bool {
    let Some((&first, rest)) = segments.split_first() else {
        return false;
    };
    if first == "*" {
        if let serde_json::Value::Array(items) = current {
            let mut any = false;
            for item in items.iter_mut() {
                if apply_path_delete(item, rest) {
                    any = true;
                }
            }
            return any;
        }
        return false;
    }
    let Some(map) = current.as_object_mut() else {
        return false;
    };
    if rest.is_empty() {
        map.remove(first).is_some()
    } else if let Some(child) = map.get_mut(first) {
        apply_path_delete(child, rest)
    } else {
        false
    }
}

fn take_path_value(
    current: &mut serde_json::Value,
    segments: &[&str],
) -> Option<serde_json::Value> {
    let (&first, rest) = segments.split_first()?;
    let map = current.as_object_mut()?;
    if rest.is_empty() {
        map.remove(first)
    } else {
        take_path_value(map.get_mut(first)?, rest)
    }
}

/// Force `store: false` and strip prior `reasoning` items from `input[]` when dispatching
/// to OpenCode Go `/v1/responses` (`F096`).
#[must_use]
pub fn sanitize_opencode_go_responses_body(body: Bytes) -> Bytes {
    let Ok(mut val) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(obj) = val.as_object_mut() else {
        return body;
    };
    obj.insert("store".to_string(), serde_json::Value::Bool(false));
    if let Some(serde_json::Value::Array(items)) = obj.get_mut("input") {
        items.retain(|item| {
            item.get("type").and_then(serde_json::Value::as_str) != Some("reasoning")
        });
    }
    serde_json::to_vec(&val).map_or(body, Bytes::from)
}

/// Inject `cache_control: {"type": "ephemeral"}` onto the last multipart text block
/// when routing an `anthropic/*` or `claude-*` model on OpenRouter (`F098`, `F177`).
pub fn inject_openrouter_anthropic_cache_control(
    value: &mut serde_json::Value,
    upstream_model_id: &str,
) -> bool {
    let lower = upstream_model_id.trim().to_ascii_lowercase();
    if !lower.starts_with("anthropic/") && !lower.contains("claude-") {
        return false;
    }
    let Some(messages) = value
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    for msg in messages.iter_mut().rev() {
        let role = msg
            .get("role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if !matches!(role, "system" | "user") {
            continue;
        }
        if let Some(serde_json::Value::Array(parts)) = msg.get_mut("content") {
            if parts.iter().any(|p| p.get("cache_control").is_some()) {
                return false;
            }
            if let Some(last_part) = parts
                .iter_mut()
                .rev()
                .find(|p| p.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                && let Some(part_obj) = last_part.as_object_mut()
            {
                part_obj.insert(
                    "cache_control".to_string(),
                    serde_json::json!({"type": "ephemeral"}),
                );
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod proptests;
