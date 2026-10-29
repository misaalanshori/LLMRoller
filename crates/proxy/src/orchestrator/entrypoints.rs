use super::*;

/// Handler for POST /v1/chat/completions.
pub async fn post_chat_completions(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    connect_info: Option<Extension<axum::extract::ConnectInfo<SocketAddr>>>,
    body: Bytes,
) -> Response {
    handle_inference_request(
        state,
        EndpointKind::Chat,
        headers,
        body,
        client_key.map(|e| e.0),
        connect_info.map(|Extension(ci)| ci.0),
    )
    .await
}

/// Handler for POST /v1/responses.
pub async fn post_responses(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    connect_info: Option<Extension<axum::extract::ConnectInfo<SocketAddr>>>,
    body: Bytes,
) -> Response {
    handle_inference_request(
        state,
        EndpointKind::Responses,
        headers,
        body,
        client_key.map(|e| e.0),
        connect_info.map(|Extension(ci)| ci.0),
    )
    .await
}

/// Handler for POST /v1/messages.
pub async fn post_messages(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    connect_info: Option<Extension<axum::extract::ConnectInfo<SocketAddr>>>,
    body: Bytes,
) -> Response {
    handle_inference_request(
        state,
        EndpointKind::Messages,
        headers,
        body,
        client_key.map(|e| e.0),
        connect_info.map(|Extension(ci)| ci.0),
    )
    .await
}

/// Handler for POST /v1/embeddings (`F028`).
pub async fn post_embeddings(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    connect_info: Option<Extension<axum::extract::ConnectInfo<SocketAddr>>>,
    body: Bytes,
) -> Response {
    handle_inference_request(
        state,
        EndpointKind::Embeddings,
        headers,
        body,
        client_key.map(|e| e.0),
        connect_info.map(|Extension(ci)| ci.0),
    )
    .await
}
