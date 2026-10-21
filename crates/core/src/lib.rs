#![forbid(unsafe_code)]
//! Pure LLMRoller domain logic.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2

pub mod accumulators;
pub mod breaker;
pub mod catalog;
pub mod chat_preview;
pub mod client_key;
pub mod eligibility;
pub mod health;
pub mod ids;
pub mod pins;
pub mod pricing;
pub mod provider;
pub mod rbac;
pub mod runtime_state;
pub mod seams;
pub mod session;
pub mod settings;
pub mod shares;
pub mod snapshot;
pub mod ssrf;
pub mod strategy;
pub mod trace_config;
pub mod windows;
pub mod wrr;
