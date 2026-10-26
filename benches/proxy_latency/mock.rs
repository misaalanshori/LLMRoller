use super::*;

#[derive(Clone, Default)]
pub(crate) struct MockState {
    pub(crate) calls: Arc<AtomicUsize>,
}

pub(crate) async fn mock_chat(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    state.calls.fetch_add(1, Ordering::Relaxed);

    let delay_ms = headers
        .get("x-mock-delay-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let is_streaming = String::from_utf8_lossy(&body).contains("\"stream\":true");
    drop(body);
    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }

    if !is_streaming {
        let payload = json!({
            "id": "bench-json",
            "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
        .to_string();
        let mut response = Response::new(Body::from(payload));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        return response;
    }

    let hold = headers.contains_key("x-mock-hold");
    let first = Bytes::from_static(
        b"data: {\"id\":\"chunk-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}\n\n",
    );
    let tail = Bytes::from_static(
        b"data: {\"id\":\"chunk-2\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"y\"}}]}\n\ndata: [DONE]\n\n",
    );

    let stream: BoxStream<'static, Result<Bytes, io::Error>> = if hold {
        Box::pin(
            futures_util::stream::iter(vec![Ok::<Bytes, io::Error>(first)])
                .chain(futures_util::stream::pending::<Result<Bytes, io::Error>>()),
        )
    } else {
        Box::pin(futures_util::stream::iter(vec![
            Ok::<Bytes, io::Error>(first),
            Ok(tail),
        ]))
    };

    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

pub(crate) async fn spawn_mock_upstream() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock upstream");
    let address = listener.local_addr().expect("mock upstream address");
    let router = Router::new()
        .route("/chat/completions", post(mock_chat))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(MockState::default());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    address
}
