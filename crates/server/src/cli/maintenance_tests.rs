//! Redaction coverage for `export-diagnostics` (`ISSUE-0184`, `ISSUE-0210`).
//!
//! # Why the redactor gets its own tests
//!
//! `redact_provider_config_json` had **no test anywhere in the workspace** — it appeared only
//! at its definition and its single call site. That is why it survived with two hardcoded
//! paths and a substring matcher: a redaction function nobody exercises is indistinguishable
//! from a working one, and the difference is a leaked OAuth refresh token.
//!
//! The tests below are grouped by the *way* the old implementation failed:
//!
//! - **Depth** — it walked `add_headers` and `header_edits` and nothing else, so a secret one
//!   level deeper (or in an array, or in an array inside an object) passed through verbatim.
//! - **Vocabulary** — `l.contains("key")` both over- and under-matched. The under-matched half
//!   is the leak (`passphrase`, `credential`, `password`, `bearer`, `signature`); the
//!   over-matched half is why it went unnoticed (`key_id`, `public_key`, `monkey`).
//! - **Failure** — an unparseable config became `{}`, which reads as "no config" instead of
//!   "this config could not be read".

use super::redact_provider_config_json;

/// The marker used throughout. Obviously fake, never a real credential shape.
const MARKER: &str = "LEAK_MARKER_12345";
/// What the redactor must substitute.
const REDACTED: &str = "[REDACTED]";

/// Assert the redacted output contains no trace of [`MARKER`], and report the whole document on
/// failure so a leak names the path it leaked from.
fn assert_no_marker(redacted: &serde_json::Value, context: &str) {
    let rendered = serde_json::to_string(redacted).expect("serialize redacted config");
    assert!(
        !rendered.contains(MARKER),
        "{context}: the secret must not survive redaction, but the output was {rendered}"
    );
}

/// A secret nested one object level down must be redacted (`§3.1`).
///
/// The pre-fix implementation only looked at `add_headers` and `header_edits`, so
/// `{"oauth": {"refresh_token": …}}` came through verbatim.
#[test]
#[doc = "Spec: SEC-6, F-086 — ISSUE-0184"]
fn redaction_reaches_a_secret_nested_in_an_object() {
    let raw = format!(r#"{{"oauth":{{"refresh_token":"{MARKER}","client_secret":"{MARKER}"}}}}"#);
    let redacted = redact_provider_config_json(&raw);
    assert_no_marker(&redacted, "secret nested one object level down");
}

/// A secret inside an array element must be redacted (`§3.2`).
///
/// Arrays are the other half of a depth-first walk, and an implementation that recurses into
/// objects but not into array members passes the object test and fails this one.
#[test]
#[doc = "Spec: SEC-6, F-086 — ISSUE-0184"]
fn redaction_reaches_a_secret_nested_in_an_array() {
    let raw = format!(r#"{{"hooks":[{{"signing_key":"{MARKER}"}},{{"auth_token":"{MARKER}"}}]}}"#);
    let redacted = redact_provider_config_json(&raw);
    assert_no_marker(&redacted, "secret inside an array element");
}

/// A secret three levels deep must still be redacted (`§3.3`).
///
/// This is the depth-limit guard. A walker that recurses one level (routine in hand-written
/// redactors) passes both tests above and fails here, which is why this case exists separately
/// rather than being folded into the nested-object test.
#[test]
#[doc = "Spec: SEC-6, F-086 — ISSUE-0184"]
fn redaction_reaches_a_secret_at_arbitrary_depth() {
    let raw = format!(
        r#"{{"a":{{"b":{{"c":{{"api_key":"{MARKER}"}}}}}},"arr":[[{{"nested_in_array_of_array":{{"password":"{MARKER}"}}}}]]}}"#
    );
    let redacted = redact_provider_config_json(&raw);
    assert_no_marker(&redacted, "secret three levels deep, and in an array of arrays");

    // And the walk must not have flattened or dropped the structure on its way down: a walker
    // that "redacts" by replacing the whole subtree would pass the assertion above while making
    // the export useless.
    assert!(
        redacted["a"]["b"]["c"].is_object(),
        "the surrounding structure must survive: {redacted}"
    );
}

/// Non-secret identifiers must survive (`§3.4`).
///
/// This is the over-inclusion guard and it is a real regression risk, not politeness: a "fix"
/// that redacts every key containing `key` makes the export useless for the thing it is for —
/// correlating a key you can see with a failure you are debugging. `key_id` in particular was
/// redacted by the old `contains("key")`, and `public_key` and `keyboard_shortcut` along with
/// it.
#[test]
#[doc = "Spec: SEC-6, F-086 — ISSUE-0184"]
fn redaction_keeps_non_secret_identifiers() {
    let raw = serde_json::json!({
        "key_id": "key_abc123",
        "public_key": "ssh-ed25519 AAAA",
        "keyboard_shortcut": "ctrl+k",
        "models_per_minute": 60,
        "monkey": "banana",
        "keys_per_second": 5,
        "max_tokens": 4096,
        "preset_id": "openrouter",
        "base_path": "/v1",
        // The one genuine secret, so the test also proves the walk actually ran.
        "api_key": MARKER,
    })
    .to_string();

    let redacted = redact_provider_config_json(&raw);

    for kept in [
        "key_id",
        "public_key",
        "keyboard_shortcut",
        "models_per_minute",
        "monkey",
        "keys_per_second",
        "max_tokens",
        "preset_id",
        "base_path",
    ] {
        assert_eq!(
            redacted[kept],
            serde_json::from_str::<serde_json::Value>(&raw).expect("reparse")[kept],
            "`{kept}` is not a secret and must be exported unchanged; got {redacted}"
        );
    }
    assert_eq!(
        redacted["api_key"], REDACTED,
        "the real secret must still be redacted"
    );
}

/// An unparseable config must say so, not become `{}` (`§3.5`).
///
/// `{}` reads as "this provider has no configuration", which is a different and false
/// statement. The invented-value class: a failure the operator cannot see is worse than a
/// visible one, because it sends them looking in the wrong place.
#[test]
#[doc = "Spec: SEC-6, F-086 — ISSUE-0184"]
fn redaction_reports_an_unparseable_config() {
    let redacted = redact_provider_config_json("{not json at all");
    assert!(
        redacted.get("error").is_some(),
        "a parse failure must be reported, not silently exported as an empty object; got {redacted}"
    );
    assert_ne!(
        redacted,
        serde_json::json!({}),
        "`{{}}` reads as \"no config\", which is a false statement about an unreadable one"
    );
}

/// A redacted value must not preserve the secret's length or prefix.
///
/// Cheap to check, and it catches the "redact by truncation" habit (`sk-ab…`), which is still a
/// leak of the identifying part for keys that encode their own scope.
#[test]
#[doc = "Spec: SEC-6 — ISSUE-0184"]
fn redaction_replaces_rather_than_truncates() {
    let raw = format!(r#"{{"api_key":"{MARKER}"}}"#);
    let rendered = serde_json::to_string(&redact_provider_config_json(&raw)).expect("serialize");
    assert!(!rendered.contains("LEAK"), "no prefix of the secret may survive");
    assert!(rendered.contains(REDACTED));
}
