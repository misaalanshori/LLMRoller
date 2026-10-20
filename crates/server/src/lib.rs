#![forbid(unsafe_code)]
//! LLMRoller server coordination and background supervisors.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#2162-versioned-configsnapshot-publication-reloader

pub mod reloader;
