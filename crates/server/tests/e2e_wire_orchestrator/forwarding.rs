use super::*;

#[tokio::test]
#[doc = "Spec: API-1, Spec: RTE-9, Spec: TRN-3, Spec: USG-1"]
async fn test_orchestrator_forwards_to_mock_upstream_and_enforces_gates() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;

    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("wire_test.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xBB; 32]);
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let mut tx = pool.begin().await.expect("begin");
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user_id = UserId::generate();

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, 'wire_user', 'h', 'user', 1000)",
    )
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert user");

    let client_key = create_client_key_tx(
        &mut tx,
        &system_actor,
        user_id,
        NewClientKey {
            label: "wire_key".to_string(),
            service_account_id: None,
            allowed_models: Some(vec!["gpt-4o".to_string()]), // Gate G6: only gpt-4o allowed
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &entropy,
        1000,
    )
    .await
    .expect("create client key");

    let client_token = client_key.raw_token.expose_secret().to_string();

    // An admin caller on the same Global key must still receive diagnostic headers.
    let admin_user_id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, 'wire_admin', 'h', 'admin', 1000)",
    )
    .bind(admin_user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert admin user");
    let admin_client_key = create_client_key_tx(
        &mut tx,
        &system_actor,
        admin_user_id,
        NewClientKey {
            label: "wire_admin_key".to_string(),
            service_account_id: None,
            allowed_models: Some(vec!["gpt-4o".to_string()]),
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &entropy,
        1000,
    )
    .await
    .expect("create admin client key");
    let admin_token = admin_client_key.raw_token.expose_secret().to_string();

    let provider = create_provider_tx(
        &mut tx,
        &system_actor,
        NewProvider {
            name: "Mock Provider".to_string(),
            kind: ProviderKind::Custom,
            base_url: format!("http://{}", mock_addr),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: true,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("create provider");

    let upstream_key = create_upstream_key_tx(
        &mut tx,
        &system_actor,
        &master_key,
        "key-0",
        Arc::new(entropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Mock Key".to_string(),
            secret: secret_string("sk-mock-key"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");

    let model_pk = ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id, exposed) VALUES (?, 'gpt-4o', 1)")
        .bind(model_pk.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert model gpt-4o");

    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, discovered_at) \
         VALUES (?, ?, 'gpt-4o', '[\"chat\",\"responses\",\"messages\"]', 'active', 1000)",
    )
    .bind(upstream_key.id.to_string())
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert gpt-4o route");

    let forbidden_model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed) VALUES (?, 'claude-3-5-sonnet', 1)",
    )
    .bind(forbidden_model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model claude");

    tx.commit().await.expect("commit");

    let (reloader_tx, _reloader_rx) = tokio::sync::mpsc::channel(100);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx);

    let snapshot = load_initial_snapshot(&pool).await.expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));

    let mut proxy_auth_state = ProxyAuthState::new(shared_snapshot.clone(), pool.clone(), vec![])
        .with_master_key(master_key.clone());
    proxy_auth_state.is_ready.store(true, Ordering::SeqCst);
    proxy_auth_state.writer = Some(writer);

    let app = data_plane_router(proxy_auth_state);

    // 1. Gate G6 test: model not found in catalog returns HTTP 404
    let req_unknown = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"definitely-not-in-catalog","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("req");
    let res_unknown = app.clone().oneshot(req_unknown).await.expect("res");
    assert_eq!(
        res_unknown.status(),
        StatusCode::NOT_FOUND,
        "Unknown model must return 404 model_not_found"
    );

    // 2. Gate G6 test: model forbidden by client_key.allowed_models returns HTTP 403
    let req_forbidden = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"claude-3-5-sonnet","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("req");
    let res_forbidden = app.clone().oneshot(req_forbidden).await.expect("res");
    assert_eq!(
        res_forbidden.status(),
        StatusCode::FORBIDDEN,
        "Disallowed model must return 403 permission_error"
    );

    // 3. Non-streaming request forwards to mock upstream and returns real response (API-1)
    let req_chat = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("req");
    let res_chat = app.clone().oneshot(req_chat).await.expect("res");
    assert_eq!(res_chat.status(), StatusCode::OK);
    assert!(
        !res_chat.headers().contains_key("openai-organization"),
        "non-admin non-streaming responses must not leak org identity"
    );
    assert!(
        !res_chat
            .headers()
            .contains_key("x-ratelimit-remaining-requests"),
        "non-admin non-streaming responses must not leak pooled rate limits"
    );
    let chat_body = res_chat
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let chat_json: serde_json::Value = serde_json::from_slice(&chat_body).expect("json");
    assert_eq!(
        chat_json["choices"][0]["message"]["content"],
        "mock upstream answer"
    );
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 1);

    // 4. Streaming SSE request receives forwarded chunks from mock upstream (API-6)
    let req_stream = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"stream me"}],"stream":true}"#))
        .expect("req");
    let res_stream = app.clone().oneshot(req_stream).await.expect("res");
    assert_eq!(res_stream.status(), StatusCode::OK);
    assert!(
        !res_stream.headers().contains_key("openai-organization"),
        "non-admin streaming responses must not leak org identity"
    );
    assert!(
        !res_stream
            .headers()
            .contains_key("x-ratelimit-remaining-requests"),
        "non-admin streaming responses must not leak pooled rate limits"
    );
    let stream_body = res_stream
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let stream_text = String::from_utf8_lossy(&stream_body);
    assert!(stream_text.contains("Hello from "));
    assert!(stream_text.contains("mock upstream!"));
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 2);

    // Admin callers keep diagnostic rate-limit headers on both paths.
    let req_admin_stream = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"admin stream"}],"stream":true}"#,
        ))
        .expect("req");
    let res_admin_stream = app.clone().oneshot(req_admin_stream).await.expect("res");
    assert_eq!(res_admin_stream.status(), StatusCode::OK);
    assert_eq!(
        res_admin_stream
            .headers()
            .get("x-ratelimit-remaining-requests")
            .and_then(|value| value.to_str().ok()),
        Some("999")
    );
    let _ = res_admin_stream
        .into_body()
        .collect()
        .await
        .expect("admin stream body");

    let req_admin_plain = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"admin plain"}]}"#,
        ))
        .expect("req");
    let res_admin_plain = app.clone().oneshot(req_admin_plain).await.expect("res");
    assert_eq!(res_admin_plain.status(), StatusCode::OK);
    assert_eq!(
        res_admin_plain
            .headers()
            .get("x-ratelimit-remaining-requests")
            .and_then(|value| value.to_str().ok()),
        Some("999")
    );
}

