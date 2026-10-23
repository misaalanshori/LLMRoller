//! Log and diagnostic scrubbing helpers.
//!
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//!
//! Secrets must never reach application logs, error bodies, panic hooks, or the
//! `control_plane_logs` table (`SEC-6`). [`scrub_text`] is the shared entry point
//! for free-form strings such as request paths, query strings, and User-Agent
//! values.

pub use crate::headers::{scrub_canary_tokens, scrub_secret_prefixes};

/// Scrub any known secret prefixes out of a free-form log field.
///
/// This covers bearer/API tokens (`sk-*`, `oc_sk_*`, `llr_*`) regardless of
/// where they appear in the string, including inside query strings. Known
/// canaries can be removed first with [`scrub_canary_tokens`].
#[must_use]
pub fn scrub_text(input: &str) -> String {
    scrub_secret_prefixes(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_text_removes_api_tokens_from_paths_and_user_agents() {
        assert_eq!(
            scrub_text("/v1/chat?token=sk-abcdefgh12345678"),
            "/v1/chat?token=[REDACTED]"
        );
        assert_eq!(
            scrub_text("client/1.0 oc_sk_abcdefgh1234"),
            "client/1.0 [REDACTED]"
        );
        assert_eq!(scrub_text("no secrets here"), "no secrets here");
    }

    #[test]
    fn scrub_text_leaves_short_prefixes_alone() {
        assert_eq!(scrub_text("task-list"), "task-list");
        assert_eq!(scrub_text("sk-short"), "sk-short");
    }
}
