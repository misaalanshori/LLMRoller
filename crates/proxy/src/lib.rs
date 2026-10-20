#![forbid(unsafe_code)]
//! Data-plane HTTP service.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2

pub mod auth;
pub mod clock;
pub mod listeners;
pub mod metrics;
pub mod ssrf;
