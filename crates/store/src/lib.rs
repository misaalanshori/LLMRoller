#![forbid(unsafe_code)]
//! SQLite persistence, cryptography, and the serialized writer actor.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2

pub mod crypto;
pub mod migrations;
pub mod pool;
