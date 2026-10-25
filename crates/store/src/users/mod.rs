#![forbid(unsafe_code)]
//! Human accounts, password hashing, TOTP, and web sessions.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Partial: AUTH-1, AUTH-4, AUTH-5, AUTH-6, AUTH-7, AUTH-8, AUTH-9
//! Partial: SEC-4, SEC-12

mod crud;
mod password;
mod sessions;
mod totp;
mod types;

pub use crud::*;
pub use password::*;
pub use sessions::*;
pub use totp::*;
pub use types::*;

#[cfg(test)]
mod tests;
