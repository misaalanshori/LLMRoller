#![forbid(unsafe_code)]
//! SQLite persistence, cryptography, and the serialized writer actor.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2

pub mod audit;
pub mod client_keys;
pub mod crypto;
pub mod instance;
pub mod migrations;
pub mod models;
pub mod pool;
pub mod providers;
pub mod rotation;
pub mod settings;
pub mod shares;
pub mod users;
pub mod writer;
