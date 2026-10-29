//! Mock upstream fixtures shared by every integration test in this module tree.
//!
//! Extracted from `integration_streaming_accounting.rs` to stay under the enforced
//! 1,600-line MNT-3 ceiling. The SSE body constants stay in the parent module (they are
//! asserted directly by streaming tests); everything that *serves* them lives here.

use super::*;

#[derive(Clone, Copy)]
pub(crate) enum MockMode {
    StreamOk,
    StreamOverloaded,
    StreamModelNotFound,
    StreamStall,
    /// Emit the first SSE frame after `first_frame_ms`, then stay open `hold_ms` longer and
    /// complete normally (`ISSUE-0161`).
    ///
    /// Two separate delays are needed to tell three behaviours apart on one successful attempt:
    ///
    /// | Code under test | EWMA (weight 0.2, first observation) |
    /// | --- | --- |
    /// | records TTFB (correct) | `0.2 * first_frame_ms` |
    /// | records at stream end | `0.2 * (first_frame_ms + hold_ms)` |
    /// | records nothing (`None`) | `0` |
    ///
    /// A single "hold after an immediate first frame" cannot distinguish the first from the third,
    /// because both land at or near zero. `StreamStall` is unusable here: it never completes, so
    /// the attempt would be scored as a timeout instead of a success.
    StreamFirstFrameAfterThenHold {
        first_frame_ms: u64,
        hold_ms: u64,
    },
    ChatJson,
    /// Non-streaming JSON completion after a fixed delay, so latency measurements
    /// have a real elapsed time to observe (`ISSUE-0148`).
    ChatJsonAfter(u64),
    Chat429,
    /// HTTP 200 whose JSON body embeds an `invalid_request_error`, which the classifier
    /// maps to `RequestRejected` (`ISSUE-0150`).
    EmbeddedRequestRejected,
    /// A plain non-retryable upstream 400, passed through unchanged (`ISSUE-0151`).
    NonRetryable400,
    /// An SSE stream whose first frame is an `invalid_request_error` (`ISSUE-0150`).
    StreamRequestRejected,
    /// `429` with `Retry-After: 1` on the first call, then hangs past the send timeout
    /// (`ISSUE-0152`). Exists to exercise the short-retry failure branch.
    Retry429ThenHang,
    /// `429` with `Retry-After: 1` on the first call, then a successful JSON completion
    /// (`ISSUE-0152`). The non-regression counterpart.
    Retry429ThenOk,
    /// `429` with `Retry-After: 1` on every call, to pin that the retry does not loop.
    Retry429Always,
    /// An Anthropic-Messages-shaped JSON response whose usage block carries cache-write
    /// and reasoning tokens, so those metric classes are reachable end to end
    /// (`ISSUE-0154`). The Messages extractor is the branch that reads both.
    ChatJsonWithAllTokenClasses,
    ResponsesFailed,
    SleepDelay(u64),
}

#[derive(Clone)]
pub(crate) struct MockUpstreamState {
    mode: MockMode,
    pub(crate) calls: Arc<AtomicUsize>,
}

impl MockUpstreamState {
    pub(crate) fn new(mode: MockMode) -> Self {
        Self {
            mode,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

async fn mock_chat(State(state): State<MockUpstreamState>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    match state.mode {
        MockMode::StreamOk => sse_response(SSE_OK),
        MockMode::StreamOverloaded => sse_response(SSE_OVERLOADED),
        MockMode::StreamModelNotFound => sse_response(SSE_MODEL_NOT_FOUND),
        MockMode::StreamStall => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(
                stream::once(async {
                    Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        SSE_STALL_AFTER_FIRST_FRAME.as_bytes(),
                    ))
                })
                .chain(stream::pending()),
            ))
            .expect("stall response"),
        MockMode::StreamFirstFrameAfterThenHold {
            first_frame_ms,
            hold_ms,
        } => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(
                stream::once(async move {
                    tokio::time::sleep(Duration::from_millis(first_frame_ms)).await;
                    Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        SSE_STALL_AFTER_FIRST_FRAME.as_bytes(),
                    ))
                })
                // ...then the rest of the body arrives only after `hold_ms`, and the stream ends.
                // So total duration ~= hold_ms while TTFB ~= 0, which is the discrimination the
                // test needs.
                .chain(stream::once(async move {
                    tokio::time::sleep(Duration::from_millis(hold_ms)).await;
                    Ok::<Bytes, std::io::Error>(Bytes::from_static(SSE_OK.as_bytes()))
                })),
            ))
            .expect("hold response"),
        MockMode::ChatJson => (
            StatusCode::OK,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "id": "chatcmpl-json",
                "object": "chat.completion",
                "created": 1_700_000_000,
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "ok" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
            })),
        )
            .into_response(),
        MockMode::ChatJsonAfter(ms) => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                Json(serde_json::json!({
                    "id": "chatcmpl-json-delayed",
                    "object": "chat.completion",
                    "created": 1_700_000_000,
                    "model": "gpt-4o",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": "delayed" },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
                })),
            )
                .into_response()
        }
        MockMode::Chat429 => (
            StatusCode::TOO_MANY_REQUESTS,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "error": { "message": "rate limited", "code": "rate_limit_exceeded" }
            })),
        )
            .into_response(),
        // The short-retry fixtures are call-count dependent: the first call answers 429
        // with a 1-second `Retry-After` (the only shape that triggers the retry), and
        // the second call does whatever the variant name says.
        MockMode::Retry429ThenHang if state.calls.load(Ordering::SeqCst) == 1 => (
            StatusCode::TOO_MANY_REQUESTS,
            [("content-type", "application/json"), ("retry-after", "1")],
            Json(serde_json::json!({
                "error": { "message": "rate limited", "code": "rate_limit_exceeded" }
            })),
        )
            .into_response(),
        MockMode::Retry429ThenHang => {
            // Never respond: the client's send timeout is what ends this call.
            std::future::pending::<()>().await;
            unreachable!("pending never resolves")
        }
        MockMode::Retry429ThenOk if state.calls.load(Ordering::SeqCst) == 1 => (
            StatusCode::TOO_MANY_REQUESTS,
            [("content-type", "application/json"), ("retry-after", "1")],
            Json(serde_json::json!({
                "error": { "message": "rate limited", "code": "rate_limit_exceeded" }
            })),
        )
            .into_response(),
        MockMode::Retry429ThenOk => (
            StatusCode::OK,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "id": "chatcmpl-retry-ok",
                "object": "chat.completion",
                "created": 1_700_000_000,
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "after retry" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
            })),
        )
            .into_response(),
        MockMode::Retry429Always => (
            StatusCode::TOO_MANY_REQUESTS,
            [("content-type", "application/json"), ("retry-after", "1")],
            Json(serde_json::json!({
                "error": { "message": "rate limited", "code": "rate_limit_exceeded" }
            })),
        )
            .into_response(),
        MockMode::ChatJsonWithAllTokenClasses => (
            StatusCode::OK,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "id": "msg_all_classes",
                "type": "message",
                "role": "assistant",
                "model": "gpt-4o",
                "content": [{ "type": "text", "text": "ok" }],
                "stop_reason": "end_turn",
                // `extract_messages_json_usage` is the branch that reads all four classes.
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_read_input_tokens": 10,
                    "cache_creation_input_tokens": 20,
                    "reasoning_tokens": 30
                }
            })),
        )
            .into_response(),
        MockMode::EmbeddedRequestRejected => (
            StatusCode::OK,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "error": {
                    "type": "invalid_request_error",
                    "message": "context_length_exceeded: too many tokens"
                }
            })),
        )
            .into_response(),
        MockMode::NonRetryable400 => (
            StatusCode::BAD_REQUEST,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "error": {
                    "type": "invalid_request_error",
                    "message": "unsupported parameter: temperature"
                }
            })),
        )
            .into_response(),
        MockMode::StreamRequestRejected => sse_response(
            "data: {\"error\":{\"type\":\"invalid_request_error\",\"message\":\"context_length_exceeded\"}}\n\n",
        ),
        MockMode::ResponsesFailed => (
            StatusCode::OK,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "id": "resp_failed_1",
                "status": "failed",
                "last_error": { "code": "server_error", "message": "Model execution failed" }
            })),
        )
            .into_response(),
        MockMode::SleepDelay(ms) => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            sse_response(SSE_OK)
        }
    }
}

fn sse_response(body: &'static str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .expect("sse response")
}

pub(crate) async fn spawn_mock(mode: MockMode) -> (SocketAddr, MockUpstreamState) {
    let state = MockUpstreamState::new(mode);
    let app = Router::new()
        .route("/chat/completions", post(mock_chat))
        // The Anthropic Messages inbound endpoint maps to an upstream `/messages` path,
        // and its usage extractor is the one branch that reads all four token classes.
        .route("/messages", post(mock_chat))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, state)
}
