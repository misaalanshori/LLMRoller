#![forbid(unsafe_code)]
//! Wire protocols and provider adapters.
//!
//! Spec: ./docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md#15.2

pub mod adapters;
pub mod affinity;
pub mod body_edit;
pub mod discovery;
pub mod fingerprint;
pub mod geo;
pub mod headers;
pub mod json_usage;
pub mod providers;
pub mod scrub;
pub mod sse_frame;
pub mod sse_tap;
pub mod trace_tap;
pub mod transcode;
pub mod usage_poller;
