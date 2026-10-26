//! Outbound provider session hint profiles, salted affinity tokens, and carrier stripping.
//!
//! Spec: Section 8.2, Section 11.2, Requirements: SEC-16, RTE-1.

use hmac::{Hmac, KeyInit, Mac};
use http::header::{HeaderMap, HeaderName, HeaderValue};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::PoolTier;
use sha2::Sha256;

pub use llmroller_core::provider::SessionProfile;
pub use llmroller_core::session::{
    MAX_SESSION_ID_LEN, canonical_content, canonicalize_block_value, is_valid_session_id,
};

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

/// Computes the tenant-salted 26-char lowercase RFC 4648 base32 affinity token per Section 11.2.
///
/// Never prefixes the token with `llr_` or any other proxy marker (`TRN-0`).
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
    if b32.len() >= 26 {
        b32[..26].to_string()
    } else {
        b32
    }
}

fn truncate_session_value(s: &str, max_len: usize) -> &str {
    let limit = if max_len == 0 {
        MAX_SESSION_ID_LEN
    } else {
        max_len.min(MAX_SESSION_ID_LEN)
    };
    if s.len() <= limit {
        return s;
    }
    let mut boundary = 0;
    for (idx, _) in s.char_indices() {
        if idx > limit {
            break;
        }
        boundary = idx;
    }
    &s[..boundary]
}

/// Applies outbound session headers for the provider profile (`TRN-0`, Section 11.2).
///
/// Across all tiers (`Own`, `Shared`, `Global`), client session headers are preserved and the
/// resolved session ID (`raw_session_id` when supplied by the client, or `salted_token` when
/// derived at Step 8 / `fingerprint_generic_ids`) is translated into `profile.headers`.
pub fn apply_outbound_session_affinity(
    headers: &mut HeaderMap,
    _tier: PoolTier,
    profile: &SessionProfile,
    salted_token: &str,
    raw_session_id: &str,
) {
    let raw_token = if raw_session_id.is_empty() {
        salted_token
    } else {
        raw_session_id
    };
    let token = truncate_session_value(raw_token, profile.max_len);
    for h_name in &profile.headers {
        if let Ok(name) = HeaderName::from_bytes(h_name.as_bytes())
            && (!headers.contains_key(&name) || !raw_session_id.is_empty())
            && let Ok(val) = HeaderValue::from_str(token)
        {
            headers.insert(name, val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[doc = "Spec: RTE-1, SEC-16, TRN-0"]
    #[test]
    fn test_salted_affinity_token_format_and_tier_stripping() {
        let secret = [0x42u8; 32];
        let user_id = UserId::generate();
        let raw_sess = "client-session-12345";

        let token = compute_salted_affinity_token(&secret, &user_id, raw_sess);
        assert!(!token.starts_with("llr_"), "no proxy prefix on the wire");
        assert_eq!(token.len(), 26);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c))
        );

        let profile = SessionProfile {
            headers: vec!["x-opencode-session".to_string()],
            body_field: None,
            max_len: 256,
        };

        // 1. Own tier: client session header preserved, profile header synchronized with resolved session ID
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

        // 2. Shared/Global tier: client session headers preserved and translated to profile header;
        // when raw_session_id is empty (Step 8 fallback), the 26-char derived token is used.
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
        assert_eq!(
            shared_headers
                .get("x-claude-code-session-id")
                .expect("claude header preserved on Shared tier"),
            "claude-native-id"
        );
        assert_eq!(
            shared_headers
                .get("x-claude-code-agent-id")
                .expect("agent sub header"),
            "agent-sub-1"
        );
        assert_eq!(
            shared_headers
                .get("x-opencode-session")
                .expect("translated profile header"),
            raw_sess
        );

        let mut step8_headers = HeaderMap::new();
        apply_outbound_session_affinity(&mut step8_headers, PoolTier::Global, &profile, &token, "");
        assert_eq!(
            step8_headers
                .get("x-opencode-session")
                .expect("step8 derived header"),
            token.as_str()
        );
    }
}
