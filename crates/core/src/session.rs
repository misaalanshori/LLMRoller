//! Inbound session ID resolution, Step 8 fallback fingerprint, and 21-slot constant session detector.
//!
//! Spec: Section 6.7, Section 8.2, Section 11.1, Section 11.1.1, Section 11.2,
//! Requirements: RTE-1, SEC-16.

use hmac::{Hmac, KeyInit, Mac};
use parking_lot::RwLock;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use crate::ids::{ClientKeyId, UserId};

type HmacSha256 = Hmac<Sha256>;

/// Inbound session precedence level per SRDD Section 11.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPrecedenceStep {
    Step1XllmrollerSessionId,
    Step2XClaudeCodeSessionId,
    Step3XParentSessionId,
    Step4XOpencodeSession,
    Step5XSessionAffinity,
    Step6GenericHeaders,
    Step7JsonBody,
    Step8Fingerprint,
}

impl SessionPrecedenceStep {
    pub fn is_generic(&self) -> bool {
        matches!(
            self,
            Self::Step4XOpencodeSession
                | Self::Step5XSessionAffinity
                | Self::Step6GenericHeaders
                | Self::Step7JsonBody
        )
    }
}

/// Validates that a raw session ID is non-empty, <= 256 bytes, and printable ASCII.
pub fn is_valid_session_id(s: &str) -> bool {
    if s.is_empty() || s.len() > 256 {
        return false;
    }
    s.bytes().all(|b| (0x21..=0x7E).contains(&b) || b == b' ')
}

/// Resolves the raw session ID from headers and body fields across Steps 1 through 7.
pub fn extract_session_precedence(
    headers: &[(String, String)],
    body_fields: &HashMap<String, String>,
) -> (String, SessionPrecedenceStep) {
    let get_h = |name: &str| -> Option<&str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .filter(|v| is_valid_session_id(v))
    };

    if let Some(v) = get_h("x-llmroller-session-id") {
        return (
            v.to_string(),
            SessionPrecedenceStep::Step1XllmrollerSessionId,
        );
    }
    if let Some(v) = get_h("x-claude-code-session-id") {
        return (
            v.to_string(),
            SessionPrecedenceStep::Step2XClaudeCodeSessionId,
        );
    }
    if let Some(v) = get_h("x-parent-session-id") {
        return (v.to_string(), SessionPrecedenceStep::Step3XParentSessionId);
    }
    if let Some(v) = get_h("x-opencode-session") {
        return (v.to_string(), SessionPrecedenceStep::Step4XOpencodeSession);
    }
    if let Some(v) = get_h("x-session-affinity") {
        return (v.to_string(), SessionPrecedenceStep::Step5XSessionAffinity);
    }
    for name in &[
        "x-session-id",
        "session-id",
        "session_id",
        "conversation-id",
        "x-bf-session-id",
    ] {
        if let Some(v) = get_h(name) {
            return (v.to_string(), SessionPrecedenceStep::Step6GenericHeaders);
        }
    }
    for field in &[
        "session_id",
        "prompt_cache_key",
        "promptCacheKey",
        "metadata.session_id",
    ] {
        if let Some(v) = body_fields.get(*field).filter(|v| is_valid_session_id(v)) {
            return (v.to_string(), SessionPrecedenceStep::Step7JsonBody);
        }
    }

    ("".to_string(), SessionPrecedenceStep::Step8Fingerprint)
}

/// Helper to render byte buffer as lowercase hexadecimal.
pub fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Computes tenant-scoped HMAC-SHA256 session hash (128 bits, 32 hex chars).
pub fn compute_session_hash(
    session_hmac_secret: &[u8],
    owner_user_id: &UserId,
    raw_session_id: &str,
) -> String {
    let mut mac =
        HmacSha256::new_from_slice(session_hmac_secret).expect("HMAC accepts any key length");
    mac.update(owner_user_id.to_string().as_bytes());
    mac.update(b"\0");
    mac.update(raw_session_id.as_bytes());
    let result = mac.finalize().into_bytes();
    hex_lower(&result[..16])
}

/// Maps an inbound protocol name to the canonical Step 8 protocol code.
#[must_use]
pub fn protocol_code(protocol: &str) -> u8 {
    match protocol {
        "responses" => 2,
        "messages" => 3,
        _ => 1,
    }
}

/// Computes Step 8 derived fallback fingerprint over byte-sliced SYS and USER1 (Section 11.1.3).
pub fn compute_step8_fingerprint(
    client_key_id: &ClientKeyId,
    canonical_id: &str,
    protocol_code: u8,
    canon_sys: &[u8],
    canon_usr: &[u8],
) -> String {
    let sys_len = canon_sys.len().min(4096);
    let usr_len = canon_usr.len().min(4096);
    let sys = &canon_sys[..sys_len];
    let usr = &canon_usr[..usr_len];

    let mut hasher = Sha256::new();
    hasher.update(b"llmroller.fp.v1\0");

    let k_bytes = client_key_id.to_string();
    hasher.update((k_bytes.len() as u32).to_be_bytes());
    hasher.update(k_bytes.as_bytes());

    hasher.update((canonical_id.len() as u32).to_be_bytes());
    hasher.update(canonical_id.as_bytes());

    hasher.update([protocol_code]);

    hasher.update((sys.len() as u32).to_be_bytes());
    hasher.update(sys);

    hasher.update((usr.len() as u32).to_be_bytes());
    hasher.update(usr);

    let digest = hasher.finalize();
    let hex_full = hex_lower(&digest);
    format!("fp:{}", &hex_full[..32])
}

fn canonical_block(block: &serde_json::Value) -> Vec<u8> {
    let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("text");
    if block_type == "text" || block_type == "input_text" {
        let text_bytes = block
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.as_bytes())
            .unwrap_or(b"");
        let mut out = Vec::with_capacity(1 + text_bytes.len());
        out.push(b'T');
        out.extend_from_slice(text_bytes);
        out
    } else {
        let mut out = Vec::new();
        out.push(b'B');
        out.extend_from_slice(block_type.as_bytes());
        out.push(0x1F);
        let mut cleaned_block = block.clone();
        if let serde_json::Value::Object(ref mut map) = cleaned_block {
            map.remove("cache_control");
            map.remove("citations");
        }
        let cleaned_bytes = serde_json::to_vec(&cleaned_block).unwrap_or_default();
        let hash = Sha256::digest(&cleaned_bytes);
        out.extend_from_slice(&hash);
        out
    }
}

/// Canonical content extractor canon(content) per Section 11.1.3.
/// Strips cache_control and citations across Chat, Responses, and Messages content blocks.
pub fn canonical_content(content_val: &serde_json::Value) -> Vec<u8> {
    match content_val {
        serde_json::Value::String(s) => s.as_bytes().to_vec(),
        serde_json::Value::Array(blocks) => {
            let mut out = Vec::new();
            for (i, block) in blocks.iter().enumerate() {
                if i > 0 {
                    out.push(0x1E);
                }
                out.extend_from_slice(&canonical_block(block));
            }
            out
        }
        serde_json::Value::Object(_) => canonical_block(content_val),
        other => serde_json::to_vec(other).unwrap_or_default(),
    }
}

/// Extracts canonical byte slices for SYS and USER1 per Section 11.1.3.
pub fn extract_canonical_sys_and_user1(
    protocol: &str,
    body: &serde_json::Value,
) -> (Vec<u8>, Vec<u8>) {
    let mut sys = Vec::new();
    let mut usr = Vec::new();

    match protocol {
        "messages" => {
            if let Some(sys_val) = body.get("system") {
                sys = canonical_content(sys_val);
            }
            if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
                for msg in messages {
                    if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                        if let Some(content) = msg.get("content") {
                            usr = canonical_content(content);
                        }
                        break;
                    }
                }
            }
        }
        "responses" => {
            if let Some(instructions) = body.get("instructions") {
                sys.extend_from_slice(&canonical_content(instructions));
            }
            if let Some(input) = body.get("input") {
                if let Some(s) = input.as_str() {
                    usr = s.as_bytes().to_vec();
                } else if let Some(items) = input.as_array() {
                    let mut found_user = false;
                    for item in items {
                        let role = item.get("role").and_then(|r| r.as_str());
                        if !found_user && (role == Some("system") || role == Some("developer")) {
                            if !sys.is_empty() {
                                sys.push(0x1E);
                            }
                            if let Some(c) = item.get("content") {
                                sys.extend_from_slice(&canonical_content(c));
                            }
                        } else if role == Some("user") && !found_user {
                            found_user = true;
                            if let Some(c) = item.get("content") {
                                usr = canonical_content(c);
                            }
                        }
                    }
                }
            }
        }
        _ => {
            if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
                let mut found_user = false;
                for msg in messages {
                    let role = msg.get("role").and_then(|r| r.as_str());
                    if !found_user && (role == Some("system") || role == Some("developer")) {
                        if !sys.is_empty() {
                            sys.push(0x1E);
                        }
                        if let Some(c) = msg.get("content") {
                            sys.extend_from_slice(&canonical_content(c));
                        }
                    } else if role == Some("user") && !found_user {
                        found_user = true;
                        if let Some(c) = msg.get("content") {
                            usr = canonical_content(c);
                        }
                    }
                }
            }
        }
    }

    (sys, usr)
}

/// Single slot in the 21-slot constant session detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpeningSlot {
    pub fingerprint: u64,
    pub seen_minute: u32,
}

/// Detector entry for one (ClientKeyId, SessionHash) pair.
#[derive(Debug, Clone)]
pub struct DetectorEntry {
    pub slots: Vec<OpeningSlot>,
    pub flagged_at_ms: Option<i64>,
}

/// Opening prompt context evaluated by the 21-slot detector.
#[derive(Debug, Clone, Copy)]
pub struct PromptContext<'a> {
    pub sys: &'a [u8],
    pub usr: &'a [u8],
    pub has_claude_code_agent_id: bool,
    pub step: SessionPrecedenceStep,
}

/// 21-slot exact constant session detector (Section 11.1.1).
#[derive(Debug, Default)]
pub struct ConstantSessionDetector {
    entries: RwLock<HashMap<(ClientKeyId, String), DetectorEntry>>,
}

impl ConstantSessionDetector {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Evaluates an opening prompt. Returns true if constant session is flagged for the first time.
    pub fn record_and_evaluate(
        &self,
        client_key_id: ClientKeyId,
        session_hash: String,
        ctx: &PromptContext<'_>,
        now_ms: i64,
    ) -> bool {
        // Evaluate only generic session sources (Steps 4 through 7) when x-claude-code-agent-id is absent
        if ctx.has_claude_code_agent_id || !ctx.step.is_generic() {
            return false;
        }

        let now_minute = (now_ms / 60_000) as u32;

        let mut h = Sha256::new();
        h.update(ctx.sys);
        h.update([0x00]);
        h.update(ctx.usr);
        let digest = h.finalize();
        let fp_u64 = u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"));

        let mut lock = self.entries.write();
        let entry = lock
            .entry((client_key_id, session_hash))
            .or_insert_with(|| DetectorEntry {
                slots: Vec::with_capacity(21),
                flagged_at_ms: None,
            });

        // 1. Drop slots with now_minute - seen_minute >= 60
        entry
            .slots
            .retain(|s| now_minute.saturating_sub(s.seen_minute) < 60);

        // 2. If fingerprint is already present, update seen_minute and return
        if let Some(existing) = entry.slots.iter_mut().find(|s| s.fingerprint == fp_u64) {
            existing.seen_minute = now_minute;
            return false;
        }

        // 3. Insert new slot
        entry.slots.push(OpeningSlot {
            fingerprint: fp_u64,
            seen_minute: now_minute,
        });

        // When len reaches 21 within 60 minutes and flagged_at_ms is None -> flag!
        if entry.slots.len() >= 21 && entry.flagged_at_ms.is_none() {
            entry.flagged_at_ms = Some(now_ms);
            return true;
        }

        false
    }

    pub fn is_flagged(&self, client_key_id: ClientKeyId, session_hash: &str) -> bool {
        let lock = self.entries.read();
        lock.get(&(client_key_id, session_hash.to_string()))
            .and_then(|e| e.flagged_at_ms)
            .is_some()
    }
}

#[cfg(test)]
pub mod tests;
