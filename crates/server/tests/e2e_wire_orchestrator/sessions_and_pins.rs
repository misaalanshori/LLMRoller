use super::*;

#[tokio::test]
#[doc = "Spec: SEC-16, RTE-1, TRN-1 — F02 pooled keys receive only the tenant-salted affinity token"]
async fn test_pooled_outbound_session_affinity_strips_client_carriers() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f02_pooled.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF2; 32]);
    seed_base(&pool, &master_key).await;

    let entropy = OsEntropy;
    let mut tx = pool.begin().await.expect("begin");
    let (user_id, token) =
        seed_user_with_client_key(&mut tx, "f02_global_user", None, &entropy).await;
    let config = r#"{"session_profile":{"headers":["x-session-id"],"body_field":"session_id"}}"#;
    seed_provider_route(
        &mut tx,
        &master_key,
        &entropy,
        mock_addr,
        OwnershipScope::Global,
        None,
        config,
    )
    .await;
    tx.commit().await.expect("commit");

    let raw_session = "client-claude-session";
    let expected_token = llmroller_proto::affinity::compute_salted_affinity_token(
        &[0x5A; 32],
        &user_id,
        raw_session,
    );

    let app = build_wire_app(&pool, &master_key).await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-session-id", "lower-precedence-session")
        .header("x-claude-code-session-id", raw_session)
        .header("x-opencode-session", "client-opencode-session")
        .header("x-claude-code-agent-id", "agent-1")
        .header("x-claude-code-parent-agent-id", "parent-1")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"session_id":"body-session","prompt_cache_key":"body-cache-key"}"#,
        ))
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = latest_request(&mock_state, "/chat/completions");
    assert_eq!(
        recorded
            .headers
            .get("x-session-id")
            .and_then(|value| value.to_str().ok()),
        Some(expected_token.as_str()),
        "pooled keys must receive the tenant-salted affinity token"
    );
    for stripped in [
        "x-claude-code-session-id",
        "x-opencode-session",
        "x-session-affinity",
    ] {
        assert!(
            !recorded.headers.contains_key(stripped),
            "recognized client session header {stripped} must be stripped on pooled keys"
        );
    }
    assert_eq!(
        recorded
            .headers
            .get("x-claude-code-agent-id")
            .and_then(|value| value.to_str().ok()),
        Some("agent-1"),
        "subagent identity headers must be preserved"
    );
    assert_eq!(
        recorded
            .headers
            .get("x-claude-code-parent-agent-id")
            .and_then(|value| value.to_str().ok()),
        Some("parent-1"),
        "parent subagent identity headers must be preserved"
    );

    let body: serde_json::Value =
        serde_json::from_str(&recorded.body).expect("recorded body is JSON");
    assert_eq!(
        body.get("session_id").and_then(|value| value.as_str()),
        Some(expected_token.as_str()),
        "the configured body affinity field must carry the salted token"
    );
    assert!(
        body.get("prompt_cache_key").is_none(),
        "every recognized client body session carrier must be removed on pooled keys"
    );
    assert!(!recorded.body.contains("body-session"));
    assert!(!recorded.body.contains("body-cache-key"));
    assert!(
        !recorded.body.contains(raw_session),
        "the raw client session must never reach a pooled upstream"
    );
    assert!(!recorded.body.contains("lower-precedence-session"));
    assert!(!recorded.body.contains("client-opencode-session"));
}

#[tokio::test]
#[doc = "Spec: SEC-16, RTE-1, TRN-1 — F02 Own keys preserve the client raw session"]
async fn test_own_outbound_session_affinity_forwards_raw_session() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f02_own.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF3; 32]);
    seed_base(&pool, &master_key).await;

    let entropy = OsEntropy;
    let mut tx = pool.begin().await.expect("begin");
    let (user_id, token) = seed_user_with_client_key(&mut tx, "f02_own_user", None, &entropy).await;
    let config = r#"{"session_profile":{"headers":["x-session-id"],"body_field":"session_id"}}"#;
    seed_provider_route(
        &mut tx,
        &master_key,
        &entropy,
        mock_addr,
        OwnershipScope::Personal,
        Some(user_id),
        config,
    )
    .await;
    tx.commit().await.expect("commit");

    let raw_session = "own-raw-session";
    let app = build_wire_app(&pool, &master_key).await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-claude-code-session-id", raw_session)
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
            .get("x-session-id")
            .and_then(|value| value.to_str().ok()),
        Some(raw_session),
        "Own keys must write the raw client session into absent profile headers"
    );
    assert_eq!(
        recorded
            .headers
            .get("x-claude-code-session-id")
            .and_then(|value| value.to_str().ok()),
        Some(raw_session),
        "Own keys must forward the client's native session header verbatim"
    );

    let body: serde_json::Value =
        serde_json::from_str(&recorded.body).expect("recorded body is JSON");
    assert_eq!(
        body.get("session_id").and_then(|value| value.as_str()),
        Some(raw_session),
        "Own keys must write the raw client session into an absent body field"
    );
    assert!(
        !recorded.body.contains("llr_"),
        "Own keys must not replace a client-supplied raw session with a salted token"
    );
}

#[tokio::test]
#[doc = "Spec: RTE-1 - F06 Step 7 body session fields pin the same upstream key"]
async fn test_body_session_fields_pin_across_distinct_openings() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f06_body_fields.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF1; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-session-a", 100), ("sk-session-b", 100)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let prompt_cache_key_bodies = [
        r#"{"model":"gpt-4o","prompt_cache_key":"cache-key-1","messages":[{"role":"system","content":"system one"},{"role":"user","content":"question one"}]}"#,
        r#"{"model":"gpt-4o","prompt_cache_key":"cache-key-1","messages":[{"role":"system","content":"system two"},{"role":"user","content":"question two"}]}"#,
    ];
    for body in prompt_cache_key_bodies {
        let response = app
            .clone()
            .oneshot(chat_request_with_body(&token, body))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let metadata_session_bodies = [
        r#"{"model":"gpt-4o","metadata":{"session_id":"meta-session-1"},"messages":[{"role":"system","content":"meta system one"},{"role":"user","content":"meta question one"}]}"#,
        r#"{"model":"gpt-4o","metadata":{"session_id":"meta-session-1"},"messages":[{"role":"system","content":"meta system two"},{"role":"user","content":"meta question two"}]}"#,
    ];
    for body in metadata_session_bodies {
        let response = app
            .clone()
            .oneshot(chat_request_with_body(&token, body))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let authorizations = recorded_authorizations(&mock_state);
    assert_eq!(authorizations.len(), 4);
    assert_eq!(
        authorizations[0], authorizations[1],
        "prompt_cache_key must pin distinct openings to one upstream key"
    );
    assert_eq!(
        authorizations[2], authorizations[3],
        "metadata.session_id must pin distinct openings to one upstream key"
    );
}

#[tokio::test]
#[doc = "Spec: RTE-10, REL-1 - a confirmed session pin is written behind the durable queue"]
async fn test_confirmed_pin_is_persisted_without_manual_sql() {
    let (mock_addr, _mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_pin_persist.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF4; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-pin-persist", 100)],
        "{}",
    )
    .await;
    let (app, writer) = build_wire_app_and_writer(&pool, &master_key, 32).await;

    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-session-id", "durable-session-1")
            .body(Body::from(
                r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
            ))
            .expect("request")
    };

    let response = app.clone().oneshot(request()).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    flush_data_queues(&pool, &writer.queues).await;

    let (session_hash, repin_count, canonical_id, key_label): (String, i64, String, String) =
        sqlx::query_as(
            "SELECT p.session_hash, p.repin_count, m.canonical_id, k.label
             FROM pins p
             JOIN models m ON p.model_pk = m.model_pk
             JOIN upstream_keys k ON p.upstream_key_id = k.id",
        )
        .fetch_one(&pool)
        .await
        .expect("the confirmed pin must be persisted by the writer");
    assert!(
        !session_hash.is_empty(),
        "the persisted pin must carry the tenant-salted session hash"
    );
    assert_eq!(repin_count, 0);
    assert_eq!(canonical_id, "gpt-4o");
    assert_eq!(key_label, "Mock Error Key sk-pin-persist");

    let first_last_used: i64 =
        sqlx::query_scalar("SELECT last_used_at FROM pins WHERE session_hash = ?")
            .bind(&session_hash)
            .fetch_one(&pool)
            .await
            .expect("last_used_at");

    let response = app.oneshot(request()).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    flush_data_queues(&pool, &writer.queues).await;

    let pin_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pins")
        .fetch_one(&pool)
        .await
        .expect("pin count");
    assert_eq!(
        pin_count, 1,
        "the follow-up turn must upsert, not duplicate"
    );
    let second_last_used: i64 =
        sqlx::query_scalar("SELECT last_used_at FROM pins WHERE session_hash = ?")
            .bind(&session_hash)
            .fetch_one(&pool)
            .await
            .expect("last_used_at");
    assert!(
        second_last_used >= first_last_used,
        "a followed pin must refresh last_used_at"
    );
}

#[tokio::test]
#[doc = "Spec: RTE-1 - F06 Step 8 fingerprint pins matching opening prompts"]
async fn test_step8_fingerprint_pins_matching_openings() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f06_step8.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF2; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-step8-a", 100), ("sk-step8-b", 100)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let first_turn = r#"{"model":"gpt-4o","messages":[{"role":"system","content":"stable system prompt"},{"role":"user","content":"first question"}]}"#;
    let second_turn = r#"{"model":"gpt-4o","messages":[{"role":"system","content":"stable system prompt"},{"role":"user","content":"first question"},{"role":"assistant","content":"answer"},{"role":"user","content":"follow-up question"}]}"#;

    for body in [first_turn, second_turn] {
        let response = app
            .clone()
            .oneshot(chat_request_with_body(&token, body))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let authorizations = recorded_authorizations(&mock_state);
    assert_eq!(authorizations.len(), 2);
    assert_eq!(
        authorizations[0], authorizations[1],
        "matching SYS and USER1 must produce one Step 8 pin"
    );
}

#[tokio::test]
#[doc = "Spec: RTE-1 - F06 fingerprint_generic_ids replaces generic IDs with Step 8"]
async fn test_constant_session_mode_fingerprint_generic_ids_uses_step8() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f06_constant_mode.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF3; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-mode-a", 100), ("sk-mode-b", 100)],
        "{}",
    )
    .await;
    sqlx::query("UPDATE client_keys SET constant_session_mode = 'fingerprint_generic_ids'")
        .execute(&pool)
        .await
        .expect("set constant session mode");
    let app = build_wire_app(&pool, &master_key).await;

    let first_opening = r#"{"model":"gpt-4o","session_id":"static-generic-id","messages":[{"role":"system","content":"opening one"},{"role":"user","content":"question one"}]}"#;
    let second_opening = r#"{"model":"gpt-4o","session_id":"static-generic-id","messages":[{"role":"system","content":"opening two"},{"role":"user","content":"question two"}]}"#;

    for body in [first_opening, second_opening, first_opening] {
        let response = app
            .clone()
            .oneshot(chat_request_with_body(&token, body))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let authorizations = recorded_authorizations(&mock_state);
    assert_eq!(authorizations.len(), 3);
    assert_ne!(
        authorizations[0], authorizations[1],
        "fingerprint_generic_ids must ignore the static body session ID"
    );
    assert_eq!(
        authorizations[0], authorizations[2],
        "the same opening must return to its Step 8 pin"
    );
}
