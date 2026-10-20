#![forbid(unsafe_code)]
//! Pure LLMRoller domain logic.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2

pub mod accumulators;
pub mod client_key;
pub mod ids;
pub mod pricing;
pub mod provider;
pub mod rbac;
pub mod runtime_state;
pub mod seams;
pub mod settings;
pub mod snapshot;
pub mod strategy;
pub mod windows;
