#![forbid(unsafe_code)]
//! Wire protocols and provider adapters.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2

pub mod adapters;
pub mod affinity;
pub mod body_edit;
pub mod headers;
pub mod json_usage;
pub mod sse_frame;
pub mod sse_tap;
