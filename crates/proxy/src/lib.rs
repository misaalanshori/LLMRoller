#![forbid(unsafe_code)]
//! Data-plane HTTP service.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2

pub mod auth;
pub mod clock;
pub mod failover;
pub mod forward;
pub mod lifecycle;
pub mod listeners;
pub mod metrics;
pub mod orchestrator;
pub mod ssrf;
pub mod token_manager;
pub mod usage_passthrough;

/// Test-only re-export of the affinity token derivation.
#[doc(hidden)]
pub fn affinity_token_for_test(
    session_hmac_secret: &[u8],
    owner_user_id: &llmroller_core::ids::UserId,
    raw_session_id: &str,
) -> String {
    llmroller_proto::affinity::compute_salted_affinity_token(
        session_hmac_secret,
        owner_user_id,
        raw_session_id,
    )
}
