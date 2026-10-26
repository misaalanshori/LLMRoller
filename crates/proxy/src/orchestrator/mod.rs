#![forbid(unsafe_code)]
//! Orchestrator for /v1/* inference endpoints and pre-flight body budget admission.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#53-orchestration-and-request-flow-state-machine
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-performance-and-resource-efficiency-perf
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#27-sub-phase-27-v1-orchestrator-v1-pre-flight-body-memory-budget-graceful-sigterm-drain-and-early-load-harness

use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_compression::tokio::bufread::{
    BrotliDecoder, DeflateDecoder, GzipDecoder, ZlibDecoder, ZstdDecoder,
};
use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE, HeaderName, HeaderValue};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::stream;
use llmroller_core::accumulators::{BudgetScopeId, GateG7Error, MonthReservationGuard};
use llmroller_core::breaker::OriginKey;
use llmroller_core::client_key::ClientKeyRecord;
use llmroller_core::eligibility::{
    CallerContext, CandidatePoolTier, CandidateRoute as EligibilityRoute, PoolSelectionError,
    select_pool,
};
use llmroller_core::health::StrikeResult;
use llmroller_core::ids::{
    ClientKeyId, ModelPk, ProviderId, RequestId, ShareId, UpstreamKeyId, UsageEventId, UserId,
};
use llmroller_core::pins::{PinEntry, PinKey, PinOutcome, PinTicket};
use llmroller_core::pricing::{
    CostSource, PriceEntry, Rates, compute_request_cost, estimate_input_tokens,
};
use llmroller_core::provider::{ProviderAuthScheme, ProviderEndpoint, ProviderKind};
use llmroller_core::rbac::{PoolTier, Role, RoutingScope};
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::seams::{Clock as _, EntropySource, ExposeSecret as _, OsEntropy, SystemClock};
use llmroller_core::snapshot::{
    ConfigSnapshot, ProviderRecord, SnapshotModel, SnapshotModelRoute, UpstreamKeyRecord,
};
use llmroller_core::strategy::{RouteId, RoutingStrategy, WrrGroupKey};
use llmroller_core::windows::{Reservation, estimate_reservation_cost_micro_usd};
use llmroller_proto::adapters::{
    EndpointKind, ErrorClass, classify_error_with_classifiers, get_adapter,
};
use llmroller_proto::affinity::{apply_outbound_session_affinity, compute_salted_affinity_token};
use llmroller_proto::body_edit::{
    ChatStreamMode, edit_body, edit_body_with_session_limit, scan_top_level,
};
use llmroller_proto::json_usage::extract_json_usage;
use llmroller_proto::sse_tap::SseUsageTap;
use llmroller_proto::trace_tap::{
    NormalizedTurnOutput, TraceTap, prepare_request_blocks, replace_binary_blocks,
};
use llmroller_store::crypto::{SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_store::pins::ResponseLinkRow;
use llmroller_store::traces::{SealJob, StoredTurnTraceV2};
use llmroller_store::usage_writer::{UsageAttemptRecord, UsageEventRecord};
use llmroller_store::writer::{DurableKey, DurableOp, WriterHandle};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::{ReaderStream, StreamReader};

use crate::auth::ProxyAuthState;
use crate::failover::{TryOutcome, select_terminal_error};
use crate::forward::{ForwardError, PreFirstFrameBuffer, SseChunkForwarder, decompress_chunk};
use crate::lifecycle::{RequestLifecycle, Termination};

/// Maximum upstream tries per request; mirrors the `usage_attempts.attempt_no` bound.
const MAX_UPSTREAM_ATTEMPTS: usize = 3;

/// Maximum bytes retained from an upstream error body for classification and passthrough.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Size of the body prefix passed to the ordered error classifier (Section 10.5).
const ERROR_CLASSIFY_BYTES: usize = 8 * 1024;

/// Fallback lifetime of a `PlanRestricted` route block until the next `/models` refresh.
const PROTOCOL_BLOCK_MS: i64 = 6 * 60 * 60 * 1_000;

/// How long an auxiliary endpoint probe result is cached (API-8).
const AUX_UNSUPPORTED_TTL_MS: i64 = 60 * 60 * 1_000;

pub(crate) mod admission;
pub(crate) mod attempt_context;
pub(crate) mod attempt_non_streaming;
pub(crate) mod attempt_streaming;
pub(crate) mod entrypoints;
pub(crate) mod errors;
pub(crate) mod pipeline;
pub(crate) mod preflight;
pub(crate) mod routes;
pub(crate) mod session;
pub(crate) mod state_bound;
pub(crate) mod streaming;
pub(crate) mod trace;

pub(crate) use admission::*;
pub(crate) use attempt_context::*;
pub(crate) use attempt_non_streaming::*;
pub(crate) use attempt_streaming::*;
pub use entrypoints::*;
pub use errors::*;
pub use pipeline::*;
pub use preflight::*;
pub(crate) use routes::*;
pub(crate) use session::*;
pub use state_bound::*;
pub(crate) use streaming::*;
pub use trace::*;

#[cfg(test)]
mod tests;
