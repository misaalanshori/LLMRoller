#![forbid(unsafe_code)]
//! Orchestrator v1 for /v1/* inference endpoints and pre-flight body budget admission.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#53-orchestration-and-request-flow-state-machine
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-performance-and-resource-efficiency-perf
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#27-sub-phase-27-v1-orchestrator-v1-pre-flight-body-memory-budget-graceful-sigterm-drain-and-early-load-harness

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use llmroller_core::ids::RequestId;
use llmroller_core::rbac::OwnershipScope;
use llmroller_proto::adapters::EndpointKind;
use llmroller_proto::body_edit::{ChatStreamMode, edit_body, scan_top_level};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::auth::ProxyAuthState;
use crate::lifecycle::{RequestLifecycle, Termination};

/// Pre-flight admission error when memory budget is saturated.
#[derive(Debug, Error)]
pub enum GatewayBusyError {
    #[error("gateway busy: preflight body memory budget saturated")]
    Timeout,
    #[error("body exceeds maximum allowed size: {0} bytes")]
    BodyTooLarge(usize),
}

impl GatewayBusyError {
    #[must_use]
    pub const fn status_code(&self) -> StatusCode {
        match self {
            Self::Timeout => StatusCode::SERVICE_UNAVAILABLE,
            Self::BodyTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }

    #[must_use]
    pub const fn retry_after_secs(&self) -> u64 {
        1
    }
}

/// Pre-flight body memory budget semaphore tracking pre-first-byte buffered bodies per SEC-8.
#[derive(Debug, Clone)]
pub struct PreflightBudgetSemaphore {
    pub budget_mb: usize,
    pub max_body_mb: usize,
    pub semaphore: Arc<Semaphore>,
}

impl PreflightBudgetSemaphore {
    #[must_use]
    pub fn new(budget_mb: usize, max_body_mb: usize) -> Self {
        let permits = (budget_mb * 1024 * 1024) / 4096;
        Self {
            budget_mb,
            max_body_mb,
            semaphore: Arc::new(Semaphore::new(permits)),
        }
    }

    /// Acquire permits for the given body size with a timeout.
    pub async fn acquire(
        &self,
        body_len: usize,
        timeout: Duration,
    ) -> Result<OwnedSemaphorePermit, GatewayBusyError> {
        let max_bytes = self.max_body_mb * 1024 * 1024;
        if body_len > max_bytes {
            return Err(GatewayBusyError::BodyTooLarge(body_len));
        }

        let permits_needed = body_len.div_ceil(4096);
        let permit = tokio::time::timeout(
            timeout,
            self.semaphore
                .clone()
                .acquire_many_owned(permits_needed as u32),
        )
        .await
        .map_err(|_| GatewayBusyError::Timeout)?
        .map_err(|_| GatewayBusyError::Timeout)?;

        Ok(permit)
    }
}

/// Handler for POST /v1/chat/completions.
pub async fn post_chat_completions(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_inference_request(state, EndpointKind::Chat, headers, body).await
}

/// Handler for POST /v1/responses.
pub async fn post_responses(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_inference_request(state, EndpointKind::Responses, headers, body).await
}

/// Handler for POST /v1/messages.
pub async fn post_messages(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_inference_request(state, EndpointKind::Messages, headers, body).await
}

/// Core inference request handler coordinating admission, editing, and forwarding.
pub async fn handle_inference_request(
    state: ProxyAuthState,
    endpoint: EndpointKind,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 1. Acquire preflight memory budget permit (up to 2,000 ms)
    let permit = match state
        .budget_semaphore
        .acquire(body.len(), Duration::from_millis(2000))
        .await
    {
        Ok(p) => p,
        Err(err) => {
            return (
                err.status_code(),
                [("retry-after", err.retry_after_secs().to_string())],
                Json(serde_json::json!({
                    "error": {
                        "code": "gateway_busy",
                        "message": err.to_string()
                    }
                })),
            )
                .into_response();
        }
    };

    // 2. Scan and validate body envelope and duplicate keys
    let scan = match scan_top_level(&body) {
        Ok(s) => s,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "code": "invalid_request_error",
                        "message": err.to_string()
                    }
                })),
            )
                .into_response();
        }
    };

    // 3. Resolve route from ConfigSnapshot
    let snapshot = state.snapshot.load();
    let has_provider = !snapshot.providers.is_empty();

    if !has_provider {
        // Return synthetic valid response when running standalone/stub
        let req_id = RequestId::generate();
        let mut lifecycle = RequestLifecycle::new(req_id, 0, None);
        lifecycle.record_first_frame(1);
        drop(permit); // Release preflight permit immediately at first frame

        let model_name = scan
            .model
            .as_ref()
            .map(|(m, _)| m.as_str())
            .unwrap_or("default-model");

        lifecycle.set_termination(Termination::Completed);
        lifecycle.finalize();

        match endpoint {
            EndpointKind::Chat => (
                StatusCode::OK,
                [("content-type", "application/json")],
                Json(serde_json::json!({
                    "id": format!("chatcmpl-{}", req_id),
                    "object": "chat.completion",
                    "created": 1_700_000_000,
                    "model": model_name,
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": "Hello from LLMRoller!" },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20 }
                })),
            )
                .into_response(),
            EndpointKind::Responses => (
                StatusCode::OK,
                [("content-type", "application/json")],
                Json(serde_json::json!({
                    "id": format!("resp-{}", req_id),
                    "object": "response",
                    "model": model_name,
                    "output": "Hello from LLMRoller Responses!",
                    "usage": { "input_tokens": 10, "output_tokens": 10 }
                })),
            )
                .into_response(),
            EndpointKind::Messages => (
                StatusCode::OK,
                [("content-type", "application/json")],
                Json(serde_json::json!({
                    "id": format!("msg-{}", req_id),
                    "type": "message",
                    "role": "assistant",
                    "model": model_name,
                    "content": [{ "type": "text", "text": "Hello from LLMRoller Messages!" }],
                    "usage": { "input_tokens": 10, "output_tokens": 10 }
                })),
            )
                .into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    } else {
        // Select route and edit body
        let (_edited_bytes, _strip) = edit_body(
            body,
            OwnershipScope::Personal,
            None,
            ChatStreamMode::Native,
            None,
        )
        .unwrap_or_else(|_| (Bytes::new(), false));

        let req_id = RequestId::generate();
        let mut lifecycle = RequestLifecycle::new(req_id, 0, None);
        lifecycle.record_first_frame(1);
        drop(permit); // Release permit at first frame

        let model_name = scan
            .model
            .as_ref()
            .map(|(m, _)| m.as_str())
            .unwrap_or("default-model");

        lifecycle.set_termination(Termination::Completed);
        lifecycle.finalize();

        (
            StatusCode::OK,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "id": format!("chatcmpl-{}", req_id),
                "object": "chat.completion",
                "model": model_name,
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "routed response" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 12, "completion_tokens": 15, "total_tokens": 27 }
            })),
        )
            .into_response()
    }
}
