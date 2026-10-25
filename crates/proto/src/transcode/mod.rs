#![forbid(unsafe_code)]
//! Bidirectional cross-protocol request, response, and SSE stream transcoder (`F099`, SRDD Section 24.1).
//!
//! Spec: ./docs/srdd/24. SRDD - Cross-Protocol Transcoding and OpenAI Client Normalization.md#241-bidirectional-protocol-transcoding-bridge-cratesprotosrctranscode
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#123-surgical-json-body-validation-and-edits

mod request;
mod response;
mod types;

pub use request::*;
pub use response::*;
pub use types::*;

#[cfg(test)]
mod tests;
