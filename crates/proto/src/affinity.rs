//! Outbound provider session hint profiles, salted affinity tokens, and carrier stripping.
//!
//! Spec: Section 8.2, Section 11.2, Requirements: SEC-16, RTE-1.

use hmac::{Hmac, KeyInit, Mac};
use http::header::{HeaderMap, HeaderName, HeaderValue};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::PoolTier;
use sha2::Sha256;

pub use llmroller_core::provider::SessionProfile;

type HmacSha256 = Hmac<Sha256>;

/// Base32 lowercase unpadded encoding per RFC 4648.
pub fn encode_base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer: u64 = 0;
    let mut bits: u8 = 0;
    for &b in bytes {
        buffer = (buffer << 8) | (b as u64);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buffer >> bits) & 0x1F) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1F) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

/// Computes the tenant-salted affinity token `llr_<base32(HMAC)[0..26]>` per Section 11.2.
pub fn compute_salted_affinity_token(
    session_hmac_secret: &[u8],
    owner_user_id: &UserId,
    raw_session_id: &str,
) -> String {
    let mut mac =
        HmacSha256::new_from_slice(session_hmac_secret).expect("HMAC accepts any key length");
    mac.update(b"affinity.v1\0");
    mac.update(owner_user_id.to_string().as_bytes());
    mac.update(b"\0");
    mac.update(raw_session_id.as_bytes());
    let result = mac.finalize().into_bytes();
    let b32 = encode_base32_lower(&result);
    let token_26 = if b32.len() >= 26 { &b32[..26] } else { &b32 };
    format!("llr_{token_26}")
}

const SESSION_HEADERS_TO_STRIP: [&str; 11] = [
    "x-session-affinity",
    "x-session-id",
    "session-id",
    "session_id",
    "conversation-id",
    "x-bf-session-id",
    "x-opencode-session",
    "x-parent-session-id",
    "prompt-cache-key",
    "promptcachekey",
    "x-claude-code-session-id",
];

/// Applies outbound session headers based on key tier and provider profile (SEC-16).
pub fn apply_outbound_session_affinity(
    headers: &mut HeaderMap,
    tier: PoolTier,
    profile: &SessionProfile,
    salted_token: &str,
    raw_session_id: &str,
) {
    if tier == PoolTier::Own {
        // Own tier: forward client session headers verbatim; write affinity headers only if absent
        let token = if raw_session_id.is_empty() {
            salted_token
        } else {
            raw_session_id
        };
        for h_name in &profile.headers {
            if let Ok(name) = HeaderName::from_bytes(h_name.as_bytes())
                && !headers.contains_key(&name)
                && let Ok(val) = HeaderValue::from_str(token)
            {
                headers.insert(name, val);
            }
        }
    } else {
        // Shared or Global tier: strip recognized session headers
        // (x-claude-code-agent-id and x-claude-code-parent-agent-id are preserved per Section 11.2)
        for h_name in SESSION_HEADERS_TO_STRIP {
            if let Ok(name) = HeaderName::from_bytes(h_name.as_bytes()) {
                headers.remove(name);
            }
        }
        // Always write salted affinity token
        for h_name in &profile.headers {
            if let Ok(name) = HeaderName::from_bytes(h_name.as_bytes())
                && let Ok(val) = HeaderValue::from_str(salted_token)
            {
                headers.insert(name, val);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[doc = "Spec: RTE-1, SEC-16"]
    #[test]
    fn test_salted_affinity_token_format_and_tier_stripping() {
        let secret = [0x42u8; 32];
        let user_id = UserId::generate();
        let raw_sess = "client-session-12345";

        let token = compute_salted_affinity_token(&secret, &user_id, raw_sess);
        assert!(token.starts_with("llr_"));
        assert_eq!(token.len(), 30); // "llr_" (4) + 26 chars = 30 chars
        assert!(
            token[4..]
                .chars()
                .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c))
        );

        let profile = SessionProfile {
            headers: vec!["x-opencode-session".to_string()],
            body_field: None,
        };

        // 1. Own tier: client session header preserved, profile written only if absent
        let mut own_headers = HeaderMap::new();
        own_headers.insert(
            HeaderName::from_static("x-claude-code-session-id"),
            HeaderValue::from_static("claude-native-id"),
        );
        apply_outbound_session_affinity(
            &mut own_headers,
            PoolTier::Own,
            &profile,
            &token,
            raw_sess,
        );
        assert_eq!(
            own_headers
                .get("x-claude-code-session-id")
                .expect("claude header"),
            "claude-native-id"
        );
        assert_eq!(
            own_headers
                .get("x-opencode-session")
                .expect("opencode header"),
            raw_sess
        );

        // 2. Shared/Global tier: client session header stripped, salted token injected
        let mut shared_headers = HeaderMap::new();
        shared_headers.insert(
            HeaderName::from_static("x-claude-code-session-id"),
            HeaderValue::from_static("claude-native-id"),
        );
        shared_headers.insert(
            HeaderName::from_static("x-claude-code-agent-id"),
            HeaderValue::from_static("agent-sub-1"),
        );
        apply_outbound_session_affinity(
            &mut shared_headers,
            PoolTier::Shared,
            &profile,
            &token,
            raw_sess,
        );
        assert!(!shared_headers.contains_key("x-claude-code-session-id"));
        // Subagent header is preserved
        assert_eq!(
            shared_headers
                .get("x-claude-code-agent-id")
                .expect("agent sub header"),
            "agent-sub-1"
        );
        // Salted affinity header written
        assert_eq!(
            shared_headers
                .get("x-opencode-session")
                .expect("salted header"),
            token.as_str()
        );
    }
}
