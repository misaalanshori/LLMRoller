#![forbid(unsafe_code)]
//! SQLite persistence, cryptography, and the serialized writer actor.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2

pub mod audit;
pub mod crypto;
pub mod instance;
pub mod migrations;
pub mod pool;
pub mod rotation;
pub mod users;
