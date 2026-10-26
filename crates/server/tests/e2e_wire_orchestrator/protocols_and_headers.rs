use super::*;

#[tokio::test]
#[doc = "Spec: API-4, TRN-2 — F03 only Chat streaming may inject stream_options"]
async fn test_non_chat_streaming_does_not_inject_chat_stream_options() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_body.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE1; 32]);
    let token = seed_single_global_route(&pool, &master_key, mock_addr, "{}").await;
    let app = build_wire_app(&pool, &master_key).await;

    let messages = streaming_request(
        "/v1/messages",
        &token,
        r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
    );
    let messages_response = app
        .clone()
        .oneshot(messages)
        .await
        .expect("messages response");
    assert_eq!(messages_response.status(), StatusCode::OK);
    let messages_body = latest_request(&mock_state, "/messages").body;
    assert!(
        !messages_body.contains("stream_options"),
        "Anthropic /v1/messages must not receive stream_options: {messages_body}"
    );

    let responses = streaming_request(
        "/v1/responses",
        &token,
        r#"{"model":"gpt-4o","input":"hi","stream":true}"#,
    );
    let responses_response = app.oneshot(responses).await.expect("responses response");
    assert_eq!(responses_response.status(), StatusCode::OK);
    let responses_body = latest_request(&mock_state, "/responses").body;
    assert!(
        !responses_body.contains("stream_options"),
        "OpenAI /v1/responses must not receive chat stream_options: {responses_body}"
    );
}

#[tokio::test]
#[doc = "Spec: API-4 — F03 provider-native chat_stream mode suppresses injection"]
async fn test_provider_native_chat_stream_mode_preserves_body() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_native.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE2; 32]);
    let config = r#"{"usage_reporting":{"chat_stream":"native"}}"#;
    let token = seed_single_global_route(&pool, &master_key, mock_addr, config).await;
    let app = build_wire_app(&pool, &master_key).await;

    let request = streaming_request(
        "/v1/chat/completions",
        &token,
        r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
    );
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = latest_request(&mock_state, "/chat/completions").body;
    assert!(
        !body.contains("stream_options"),
        "provider-native usage reporting must not be mutated: {body}"
    );

    let _ = mock_state;
}

#[tokio::test]
#[doc = "Spec: API-2, API-3, PRV-3 — F03 honors provider header, UA, auth and response profiles"]
async fn test_provider_profile_applies_headers_ua_auth_and_response_allowlist() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_profile.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE3; 32]);
    let config = r#"{
        "add_headers": {"x-custom-provider": "1"},
        "strip_headers": ["x-client-secret"],
        "strip_header_tokens": ["beta-drop"],
        "ua_policy": "profile",
        "ua_profile": "opencode_go",
        "response_header_allow": ["x-upstream-custom"],
        "auth": {"chat": {"header": "authorization", "prefix": "Bearer "}}
    }"#;
    let token = seed_single_global_route(&pool, &master_key, mock_addr, config).await;
    let app = build_wire_app(&pool, &master_key).await;

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-client-secret", "must-not-leak")
        .header("anthropic-beta", "keep, beta-drop")
        .header(header::USER_AGENT, "curl/8.5.0")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-upstream-custom")
            .map(|v| v.as_bytes()),
        Some(&b"yes"[..]),
        "response_header_allow must forward the configured upstream header"
    );

    let recorded = latest_request(&mock_state, "/chat/completions");
    assert_eq!(
        recorded
            .headers
            .get("x-custom-provider")
            .map(|v| v.as_bytes()),
        Some(&b"1"[..]),
        "add_headers must be applied to the upstream request"
    );
    assert!(
        !recorded.headers.contains_key("x-client-secret"),
        "strip_headers must remove configured deny-listed headers"
    );
    assert_eq!(
        recorded.headers.get("anthropic-beta").map(|v| v.as_bytes()),
        Some(&b"keep"[..]),
        "strip_header_tokens must remove only the configured tokens"
    );
    assert_eq!(
        recorded
            .headers
            .get(header::USER_AGENT)
            .map(|v| v.as_bytes()),
        Some(&b"opencode/1.2.15"[..]),
        "ua_policy=profile must replace generic library User-Agent values with the profile UA"
    );
    assert_eq!(
        recorded
            .headers
            .get(header::AUTHORIZATION)
            .map(|v| v.as_bytes()),
        Some(&b"Bearer sk-mock-key"[..]),
        "the configured auth scheme must receive the decrypted upstream secret"
    );
}

// Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#73-transparency-and-compatibility-trn--com | Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer
#[tokio::test]
#[doc = "Spec: TRN-0, API-3 — upstream sees the real client: no User-Agent invented, unknown headers kept, no llmroller marker"]
async fn test_trn0_upstream_sees_real_client_without_llmroller_markers() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("trn0.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE5; 32]);
    let token = seed_single_global_route(&pool, &master_key, mock_addr, "{}").await;
    let app = build_wire_app(&pool, &master_key).await;
    let body = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#;

    // A client that sends no User-Agent: none may appear upstream.
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-future-agent-header", "kept")
        .body(Body::from(body))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let recorded = latest_request(&mock_state, "/chat/completions");
    assert!(
        !recorded.headers.contains_key(header::USER_AGENT),
        "no client User-Agent means no upstream User-Agent: {:?}",
        recorded.headers
    );
    assert_eq!(
        recorded
            .headers
            .get("x-future-agent-header")
            .map(|v| v.as_bytes()),
        Some(&b"kept"[..]),
        "unknown client headers must pass through"
    );

    // A real client: its User-Agent arrives unchanged.
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, "opencode/1.2.15")
        .body(Body::from(body))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let recorded = latest_request(&mock_state, "/chat/completions");
    assert_eq!(
        recorded
            .headers
            .get(header::USER_AGENT)
            .map(|v| v.as_bytes()),
        Some(&b"opencode/1.2.15"[..])
    );
    for (name, value) in &recorded.headers {
        let line = format!("{name}: {}", value.to_str().unwrap_or_default()).to_ascii_lowercase();
        assert!(
            !line.contains("llmroller"),
            "proxy marker leaked upstream: {line}"
        );
    }
}

// Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md#121-request-and-response-header-policies | Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer
#[tokio::test]
#[doc = "Spec: TRN-0, API-3 — the client's accept-encoding reaches the upstream unchanged"]
async fn test_trn0_accept_encoding_is_forwarded() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("trn0_ae.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE6; 32]);
    let token = seed_single_global_route(&pool, &master_key, mock_addr, "{}").await;
    let app = build_wire_app(&pool, &master_key).await;

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT_ENCODING, "gzip, deflate, br, zstd")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let recorded = latest_request(&mock_state, "/chat/completions");
    assert_eq!(
        recorded
            .headers
            .get(header::ACCEPT_ENCODING)
            .map(|v| v.as_bytes()),
        Some(&b"gzip, deflate, br, zstd"[..]),
        "accept-encoding must not be rewritten to identity"
    );
}

// Spec: ./docs/srdd/23. SRDD - Multi-Provider Access Layer, OAuth Token Exchange, and Hardware Fingerprinting.md#233-deterministic-per-key-hardware--cli-fingerprinting-cratesprotosrcfingerprintrs | Phase: ./docs/bigbang/big-bang-phases-2.md#23-sub-phase-23-request-and-response-header-pipelines-smuggling-guards-user-agent-policy-and-header-fuzzer
#[tokio::test]
#[doc = "Spec: TRN-0, API-3, PRV-3 — transparent providers let the caller win, required-client providers override and merge"]
async fn test_client_mode_transparent_fills_required_overrides() {
    let provider_headers = r#""add_headers": {
            "user-agent": "claude-cli/2.1.63 (external, cli)",
            "x-app": "cli",
            "anthropic-beta": "claude-code-20250219,oauth-2025-04-20",
            "anthropic-version": "2023-06-01"
        }"#;
    for (mode, seed_byte) in [("transparent", 0xE7_u8), ("required", 0xE8_u8)] {
        let (mock_addr, mock_state) = spawn_mock_upstream().await;
        let dir = tempdir().expect("temp dir");
        let pool = open_pool(&dir.path().join("client_mode.db"), PoolRole::Writer)
            .await
            .expect("writer pool");
        migrate(&pool).await.expect("migrate");
        let master_key = MasterKey::from_bytes([seed_byte; 32]);
        let config = format!(r#"{{"client_mode": "{mode}", {provider_headers}}}"#);
        let token = seed_single_global_route(&pool, &master_key, mock_addr, &config).await;
        let app = build_wire_app(&pool, &master_key).await;

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::USER_AGENT, "opencode/1.2.15")
            .header("x-app", "opencode")
            .header("anthropic-beta", "prompt-caching-2024-07-31")
            .body(Body::from(
                r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
            ))
            .expect("request");
        let response = app.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK, "{mode}");
        let recorded = latest_request(&mock_state, "/chat/completions");
        let get = |name: &str| {
            recorded
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        // A header the caller did not send is always provided by the provider.
        assert_eq!(
            get("anthropic-version").as_deref(),
            Some("2023-06-01"),
            "{mode}"
        );
        if mode == "transparent" {
            assert_eq!(get("user-agent").as_deref(), Some("opencode/1.2.15"));
            assert_eq!(get("x-app").as_deref(), Some("opencode"));
            assert_eq!(
                get("anthropic-beta").as_deref(),
                Some("prompt-caching-2024-07-31")
            );
        } else {
            assert_eq!(
                get("user-agent").as_deref(),
                Some("claude-cli/2.1.63 (external, cli)")
            );
            assert_eq!(get("x-app").as_deref(), Some("cli"));
            assert_eq!(
                get("anthropic-beta").as_deref(),
                Some("prompt-caching-2024-07-31,claude-code-20250219,oauth-2025-04-20")
            );
        }
    }
}

#[tokio::test]
#[doc = "Spec: API-2, PRV-3 — F03 supports per-protocol x-api-key authentication"]
async fn test_messages_auth_scheme_uses_configured_x_api_key() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_messages_auth.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE4; 32]);
    let config = r#"{"auth":{"messages":{"header":"x-api-key","prefix":""}}}"#;
    let token = seed_single_global_route(&pool, &master_key, mock_addr, config).await;
    let app = build_wire_app(&pool, &master_key).await;

    let request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = latest_request(&mock_state, "/messages");
    assert_eq!(
        recorded.headers.get("x-api-key").map(|v| v.as_bytes()),
        Some(&b"sk-mock-key"[..]),
        "the Messages protocol must use its configured auth header"
    );
    assert!(
        !recorded.headers.contains_key(header::AUTHORIZATION),
        "client credentials must not leak through the Authorization header"
    );
    assert_eq!(
        recorded
            .headers
            .get("anthropic-version")
            .map(|v| v.as_bytes()),
        Some(&b"2023-06-01"[..]),
        "native Anthropic Messages requests require anthropic-version"
    );
}
