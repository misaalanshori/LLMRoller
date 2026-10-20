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
pub mod usage_passthrough;
