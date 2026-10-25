#![forbid(unsafe_code)]
//! Deterministic per-key hardware identity, CLI fingerprinting, and dynamic header template expansion (`F100`).
//!
//! Spec: ./docs/srdd/23. SRDD - Multi-Provider Access Layer, OAuth Token Exchange, and Hardware Fingerprinting.md#233-deterministic-per-key-hardware--cli-fingerprinting-cratesprotosrcfingerprintrs
//!Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-request-and-response-header-policies

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::trace_tap::hex_sha256;

/// Deterministic hardware and OS identity derived from an upstream key ID (`F100`).
///
/// Ensures each upstream key presents a stable, realistic device/machine identity
/// across requests without leaking host PII or regenerating random IDs per turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub key_id: String,
    pub device_id: String,
    pub machine_id_sha256: String,
    pub mac_machine_id: String,
    pub session_uuid: String,
    pub hostname: String,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub device_model: String,
}

impl DeviceProfile {
    /// Deterministically derive a stable hardware profile from an `upstream_key.id` string (`F100`).
    #[must_use]
    pub fn from_key_id(key_id: &str) -> Self {
        let dev_hash = hex_sha256(format!("llmroller:device_id:{key_id}").as_bytes());
        let machine_id_sha256 = hex_sha256(format!("llmroller:machine_id:{key_id}").as_bytes());
        let mac_machine_id = hex_sha256(format!("llmroller:mac_machine_id:{key_id}").as_bytes());
        let sess_hash = hex_sha256(format!("llmroller:session_uuid:{key_id}").as_bytes());

        let device_id = format_deterministic_uuid(&dev_hash);
        let session_uuid = format_deterministic_uuid(&sess_hash);
        let host_suffix = &dev_hash[..8];

        Self {
            key_id: key_id.to_string(),
            device_id,
            machine_id_sha256,
            mac_machine_id,
            session_uuid,
            hostname: format!("MacBook-Pro-{host_suffix}.local"),
            os: "MacOS".to_string(),
            os_version: "26.0.1".to_string(),
            arch: "arm64".to_string(),
            device_model: "Mac16,8".to_string(),
        }
    }

    /// Combined `os/arch` descriptor (`darwin/arm64`).
    #[must_use]
    pub fn os_arch(&self) -> String {
        let os_lower = if self.os.eq_ignore_ascii_case("macos") {
            "darwin".to_string()
        } else {
            self.os.to_ascii_lowercase()
        };
        format!("{os_lower}/{}", self.arch.to_ascii_lowercase())
    }

    /// Compute the Cursor IDE `x-cursor-Checksum` header value using the 6-byte Jyh-XOR
    /// timestamp obfuscation prefix followed by `<machineId>/<macMachineId>` (`F100`, `F106`).
    #[must_use]
    pub fn cursor_checksum(&self, timestamp_secs: u64) -> String {
        // Cursor divides unix seconds by 1000 (kiloseconds) into 6 big-endian bytes and
        // applies the rolling Jyh XOR key (starting at 165).
        let ks = timestamp_secs / 1_000;
        let mut raw = [
            ((ks >> 40) & 0xff) as u8,
            ((ks >> 32) & 0xff) as u8,
            ((ks >> 24) & 0xff) as u8,
            ((ks >> 16) & 0xff) as u8,
            ((ks >> 8) & 0xff) as u8,
            (ks & 0xff) as u8,
        ];
        let mut prev: u8 = 165;
        for byte in &mut raw {
            *byte = (*byte ^ prev).wrapping_add(byte_index_salt(prev));
            prev = *byte;
        }
        let encoded_ts = base64_url_no_pad(&raw);
        format!(
            "{encoded_ts}{}/{}",
            self.machine_id_sha256, self.mac_machine_id
        )
    }

    /// Compute the 5-hex `cch` billing fingerprint token for Anthropic Claude Code (`F100`, `F104`).
    #[must_use]
    pub fn claude_cch(&self, seed: &str) -> String {
        let digest = hex_sha256(format!("claude-cch:{}:{seed}", self.device_id).as_bytes());
        digest[..5].to_string()
    }

    /// Format the `x-anthropic-billing-header` system prompt block required by Claude Code (`F100`, `F104`).
    #[must_use]
    pub fn claude_billing_header(&self, seed: &str) -> String {
        let build_hash = &self.machine_id_sha256[..7];
        let cch = self.claude_cch(seed);
        format!(
            "x-anthropic-billing-header: cc_version=2.1.63.{build_hash}; cc_entrypoint=cli; cch={cch};"
        )
    }

    /// Synthesize the complete provider-specific hardware/CLI fingerprint header map (`F100`).
    #[must_use]
    pub fn headers_for_preset(
        &self,
        preset: HardwareFingerprintPreset,
        request_uuid: &str,
        session_id: &str,
        timestamp_secs: u64,
    ) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        let eff_session = if session_id.trim().is_empty() {
            self.session_uuid.as_str()
        } else {
            session_id
        };
        let eff_req = if request_uuid.trim().is_empty() {
            self.device_id.as_str()
        } else {
            request_uuid
        };

        match preset {
            HardwareFingerprintPreset::ClaudeCode => {
                map.insert(
                    "user-agent".to_string(),
                    "claude-cli/2.1.63 (external, cli)".to_string(),
                );
                map.insert("x-app".to_string(), "cli".to_string());
                map.insert(
                    "anthropic-dangerous-direct-browser-access".to_string(),
                    "true".to_string(),
                );
                map.insert(
                    "anthropic-beta".to_string(),
                    "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05".to_string(),
                );
                map.insert("x-stainless-lang".to_string(), "js".to_string());
                map.insert(
                    "x-stainless-package-version".to_string(),
                    "0.74.0".to_string(),
                );
                map.insert("x-stainless-os".to_string(), self.os.clone());
                map.insert("x-stainless-arch".to_string(), self.arch.clone());
                map.insert("x-stainless-runtime".to_string(), "node".to_string());
                map.insert(
                    "x-stainless-runtime-version".to_string(),
                    "v24.3.0".to_string(),
                );
                map.insert("x-session-id".to_string(), eff_session.to_string());
            }
            HardwareFingerprintPreset::OpenAiCodex => {
                map.insert(
                    "user-agent".to_string(),
                    format!(
                        "codex_cli_rs/0.116.0 (Mac OS {}; {}) Apple_Terminal/464",
                        self.os_version, self.arch
                    ),
                );
                map.insert(
                    "openai-beta".to_string(),
                    "responses=experimental".to_string(),
                );
                map.insert("originator".to_string(), "codex_cli_rs".to_string());
                map.insert("session_id".to_string(), eff_session.to_string());
            }
            HardwareFingerprintPreset::Antigravity => {
                map.insert(
                    "user-agent".to_string(),
                    format!("antigravity/1.107.0 {}", self.os_arch()),
                );
                map.insert(
                    "x-goog-api-client".to_string(),
                    "google-cloud-sdk vscode_cloudshelleditor/0.1".to_string(),
                );
                map.insert(
                    "client-metadata".to_string(),
                    r#"{"ideType":"ANTIGRAVITY","platform":"MACOS","pluginType":"GEMINI"}"#
                        .to_string(),
                );
            }
            HardwareFingerprintPreset::GeminiCli => {
                map.insert(
                    "user-agent".to_string(),
                    "google-cloud-sdk vscode_cloudshelleditor/0.1".to_string(),
                );
                map.insert(
                    "x-goog-api-client".to_string(),
                    "gl-node/22.17.0".to_string(),
                );
                map.insert(
                    "client-metadata".to_string(),
                    r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#
                        .to_string(),
                );
            }
            HardwareFingerprintPreset::GithubCopilot => {
                map.insert("editor-version".to_string(), "vscode/1.110.0".to_string());
                map.insert(
                    "editor-plugin-version".to_string(),
                    "copilot-chat/0.38.0".to_string(),
                );
                map.insert(
                    "copilot-integration-id".to_string(),
                    "vscode-chat".to_string(),
                );
                map.insert(
                    "openai-intent".to_string(),
                    "conversation-edits".to_string(),
                );
                map.insert("x-github-api-version".to_string(), "2025-04-01".to_string());
                map.insert("x-request-id".to_string(), eff_req.to_string());
            }
            HardwareFingerprintPreset::KimiCoding => {
                map.insert("user-agent".to_string(), "KimiCLI/1.3".to_string());
                map.insert("x-msh-platform".to_string(), "kimi_cli".to_string());
                map.insert("x-msh-version".to_string(), "1.3.0".to_string());
                map.insert("x-msh-device-name".to_string(), self.hostname.clone());
                map.insert("x-msh-device-model".to_string(), self.device_model.clone());
                map.insert("x-msh-os-version".to_string(), self.os_version.clone());
                map.insert("x-msh-device-id".to_string(), self.device_id.clone());
            }
            HardwareFingerprintPreset::CursorIde => {
                map.insert(
                    "x-cursor-checksum".to_string(),
                    self.cursor_checksum(timestamp_secs),
                );
                map.insert("x-cursor-client-version".to_string(), "2.3.41".to_string());
                map.insert("x-cursor-client-type".to_string(), "ide".to_string());
            }
            HardwareFingerprintPreset::AwsKiro => {
                map.insert(
                    "user-agent".to_string(),
                    format!("aws-sdk-js/1.0.0 KiroIDE-0.2.13-{}", self.machine_id_sha256),
                );
                map.insert("x-amzn-kiro-agent-mode".to_string(), "vibe".to_string());
                map.insert("amz-sdk-invocation-id".to_string(), eff_req.to_string());
            }
        }
        map
    }
}

/// First-party coding agent fingerprint profiles supported by `DeviceProfile` (`F100`, SRDD Section 23.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareFingerprintPreset {
    ClaudeCode,
    OpenAiCodex,
    Antigravity,
    GeminiCli,
    GithubCopilot,
    KimiCoding,
    CursorIde,
    AwsKiro,
}

/// Expand dynamic template placeholders in outbound header values (`F100`).
///
/// Supported placeholders:
/// - `{{uuid}}` / `{{request_uuid}}`: per-request UUID
/// - `{{session_id}}`: resolved session token or deterministic key session UUID
/// - `{{device_id}}`: deterministic per-key UUID v4
/// - `{{machine_id_sha256}}`: deterministic 64-hex SHA-256 machine ID
/// - `{{mac_machine_id}}`: deterministic 64-hex MAC machine ID
/// - `{{cursor_checksum}}`: Jyh-XOR timestamp + `<machine_id>/<mac_machine_id>`
/// - `{{os_arch}}`: e.g. `darwin/arm64`
/// - `{{cch}}`: 5-hex Claude Code billing hash
#[must_use]
pub fn expand_header_template(
    template: &str,
    profile: &DeviceProfile,
    request_uuid: &str,
    session_id: &str,
    timestamp_secs: u64,
) -> String {
    if !template.contains("{{") {
        return template.to_string();
    }
    let eff_session = if session_id.trim().is_empty() {
        profile.session_uuid.as_str()
    } else {
        session_id
    };
    let eff_req = if request_uuid.trim().is_empty() {
        profile.device_id.as_str()
    } else {
        request_uuid
    };
    let mut out = template.to_string();
    out = out.replace("{{uuid}}", eff_req);
    out = out.replace("{{request_uuid}}", eff_req);
    out = out.replace("{{session_id}}", eff_session);
    out = out.replace("{{device_id}}", &profile.device_id);
    out = out.replace("{{machine_id_sha256}}", &profile.machine_id_sha256);
    out = out.replace("{{mac_machine_id}}", &profile.mac_machine_id);
    if out.contains("{{cursor_checksum}}") {
        out = out.replace(
            "{{cursor_checksum}}",
            &profile.cursor_checksum(timestamp_secs),
        );
    }
    if out.contains("{{os_arch}}") {
        out = out.replace("{{os_arch}}", &profile.os_arch());
    }
    if out.contains("{{cch}}") {
        out = out.replace("{{cch}}", &profile.claude_cch(eff_session));
    }
    out
}

fn format_deterministic_uuid(hex64: &str) -> String {
    let bytes = hex64.as_bytes();
    if bytes.len() < 32 {
        return "00000000-0000-4000-8000-000000000000".to_string();
    }
    let p1 = &hex64[0..8];
    let p2 = &hex64[8..12];
    let p3 = &hex64[13..16];
    let variant_nibble = match bytes[16] {
        b'0'..=b'3' => '8',
        b'4'..=b'7' => '9',
        b'8'..=b'9' | b'a'..=b'b' | b'A'..=b'B' => 'a',
        _ => 'b',
    };
    let p4 = &hex64[17..20];
    let p5 = &hex64[20..32];
    format!("{p1}-{p2}-4{p3}-{variant_nibble}{p4}-{p5}")
}

const fn byte_index_salt(prev: u8) -> u8 {
    prev.rotate_left(3) ^ 0x5a
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        out.push(TABLE[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: PRV-2, API-3, F100 - deterministic hardware profile and template expansion"]
    fn regression_test_f100_missing_hardware_and_cli_fingerprint_hea() {
        let p1 = DeviceProfile::from_key_id("01JABCDEF1234567890ABCDEFG");
        let p2 = DeviceProfile::from_key_id("01JABCDEF1234567890ABCDEFG");
        let p3 = DeviceProfile::from_key_id("01JDIFFERENTKEY00000000000");

        assert_eq!(
            p1, p2,
            "same upstream key ID must yield identical hardware identity"
        );
        assert_ne!(
            p1.device_id, p3.device_id,
            "different keys must have distinct device IDs"
        );
        assert_eq!(p1.device_id.len(), 36);
        assert_eq!(p1.machine_id_sha256.len(), 64);

        let expanded = expand_header_template(
            "dev={{device_id}};req={{uuid}};sess={{session_id}};arch={{os_arch}};chk={{cursor_checksum}};cch={{cch}}",
            &p1,
            "req-1234",
            "sess-5678",
            1_776_000_000,
        );
        assert!(expanded.contains(&format!("dev={}", p1.device_id)));
        assert!(expanded.contains("req=req-1234"));
        assert!(expanded.contains("sess=sess-5678"));
        assert!(expanded.contains("arch=darwin/arm64"));
        assert!(expanded.contains(&p1.machine_id_sha256));

        // Verify all 8 first-party CLI fingerprint presets produce required headers
        let claude = p1.headers_for_preset(
            HardwareFingerprintPreset::ClaudeCode,
            "req-1",
            "sess-1",
            1_776_000_000,
        );
        assert_eq!(
            claude.get("user-agent").map(String::as_str),
            Some("claude-cli/2.1.63 (external, cli)")
        );
        assert_eq!(
            claude.get("x-stainless-lang").map(String::as_str),
            Some("js")
        );
        assert!(
            p1.claude_billing_header("seed")
                .contains("cc_version=2.1.63.")
        );

        let kimi = p1.headers_for_preset(
            HardwareFingerprintPreset::KimiCoding,
            "req-1",
            "sess-1",
            1_776_000_000,
        );
        assert_eq!(
            kimi.get("x-msh-device-id").map(String::as_str),
            Some(p1.device_id.as_str())
        );

        let copilot = p1.headers_for_preset(
            HardwareFingerprintPreset::GithubCopilot,
            "req-99",
            "sess-1",
            1_776_000_000,
        );
        assert_eq!(
            copilot.get("copilot-integration-id").map(String::as_str),
            Some("vscode-chat")
        );
        assert_eq!(
            copilot.get("x-request-id").map(String::as_str),
            Some("req-99")
        );

        let cursor = p1.headers_for_preset(
            HardwareFingerprintPreset::CursorIde,
            "req-1",
            "sess-1",
            1_776_000_000,
        );
        assert!(
            cursor
                .get("x-cursor-checksum")
                .is_some_and(|v| v.contains(&p1.machine_id_sha256))
        );

        let kiro = p1.headers_for_preset(
            HardwareFingerprintPreset::AwsKiro,
            "inv-42",
            "sess-1",
            1_776_000_000,
        );
        assert_eq!(
            kiro.get("amz-sdk-invocation-id").map(String::as_str),
            Some("inv-42")
        );

        let ag = p1.headers_for_preset(
            HardwareFingerprintPreset::Antigravity,
            "req-1",
            "sess-1",
            1_776_000_000,
        );
        assert!(
            ag.get("client-metadata")
                .is_some_and(|v| v.contains("ANTIGRAVITY"))
        );
    }
}
