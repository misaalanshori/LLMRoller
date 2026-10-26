use super::*;

#[tokio::test]
#[doc = "Spec: API-8 — F03 moderation 403 is returned verbatim without failover"]
async fn test_moderation_403_does_not_fail_over_or_forward_as_credential_error() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_moderation.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE5; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-moderate", 1), ("sk-ok", 2)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let response = app
        .oneshot(chat_request_with_header(&token, "x-fail-moderation", "1"))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("moderation"),
        "the upstream moderation body must be forwarded verbatim: {text}"
    );
    assert_eq!(
        mock_state.chat_calls.load(Ordering::SeqCst),
        1,
        "a RequestRejected 403 must never fail over to another key"
    );
}

#[tokio::test]
#[doc = "Spec: API-8 — F03 moderation 403 leaves key health untouched"]
async fn test_moderation_403_does_not_quarantine_key() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(
        &dir.path().join("f03_moderation_health.db"),
        PoolRole::Writer,
    )
    .await
    .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE6; 32]);
    let token =
        seed_global_route_with_secrets(&pool, &master_key, mock_addr, &[("sk-moderate", 1)], "{}")
            .await;
    let app = build_wire_app(&pool, &master_key).await;

    let rejected = app
        .clone()
        .oneshot(chat_request_with_header(&token, "x-fail-moderation", "1"))
        .await
        .expect("response");
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let recovered = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(
        recovered.status(),
        StatusCode::OK,
        "a content-moderation 403 must not quarantine the upstream key"
    );
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[doc = "Spec: API-8 — F03 PlanRestricted blocks only that key/model/protocol route"]
async fn test_plan_restricted_blocks_route_until_next_discovery() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_plan.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE7; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-plan", 1), ("sk-ok", 2)],
        "{}",
    )
    .await;
    let (app, writer) = build_wire_app_and_writer(&pool, &master_key, 32).await;

    let first = app
        .clone()
        .oneshot(chat_request(&token))
        .await
        .expect("response");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        mock_state.chat_calls.load(Ordering::SeqCst),
        2,
        "PlanRestricted must fail over to the next candidate"
    );
    flush_data_queues(&pool, &writer.queues).await;
    let (protocol, reason): (String, String) = sqlx::query_as(
        "SELECT b.protocol, b.reason
         FROM route_protocol_blocks b
         JOIN upstream_keys k ON b.upstream_key_id = k.id
         WHERE k.label = 'Mock Error Key sk-plan'",
    )
    .fetch_one(&pool)
    .await
    .expect("PlanRestricted must persist a durable protocol block");
    assert_eq!(protocol, "chat");
    assert!(!reason.is_empty());

    let second = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        mock_state.chat_calls.load(Ordering::SeqCst),
        3,
        "the blocked (key, model, protocol) route must be skipped after PlanRestricted"
    );
}

#[tokio::test]
#[doc = "Spec: API-8 — F03 terminal selector is used for mixed failures"]
async fn test_mixed_quota_and_server_failure_returns_terminal_502() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_terminal_mixed.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE8; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-quota", 1), ("sk-503", 2)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let response = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"], "upstream_unreachable");
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[doc = "Spec: REL-1 - key health transitions are written behind the durable queue"]
async fn test_key_health_transition_is_persisted() {
    let (mock_addr, _mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_health_persist.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xF5; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-health-persist", 100)],
        "{}",
    )
    .await;
    let (app, writer) = build_wire_app_and_writer(&pool, &master_key, 32).await;

    let response = app
        .clone()
        .oneshot(chat_request(&token))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    flush_data_queues(&pool, &writer.queues).await;
    let health: String = sqlx::query_scalar(
        "SELECT health FROM upstream_keys WHERE label = 'Mock Error Key sk-health-persist'",
    )
    .fetch_one(&pool)
    .await
    .expect("health row");
    assert_eq!(health, "healthy");

    let response = app
        .oneshot(chat_request_with_header(&token, "x-fail-429", "1"))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    flush_data_queues(&pool, &writer.queues).await;

    let (health, health_until): (String, Option<i64>) = sqlx::query_as(
        "SELECT health, health_until FROM upstream_keys WHERE label = 'Mock Error Key sk-health-persist'",
    )
    .fetch_one(&pool)
    .await
    .expect("health row");
    assert_eq!(
        health, "cooling_down",
        "a 429 cooldown must be persisted before shutdown"
    );
    assert!(
        health_until.is_some(),
        "a persisted cooldown must carry its retry deadline"
    );
}

#[tokio::test]
#[doc = "Spec: RTE-8 - F022 Short Retry-After (<=2s) on attempt 1 sleeps and retries candidate successfully"]
async fn test_retry_after_short_sleep_retries_candidate_successfully() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f022_retry_after.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xEE; 32]);
    let token =
        seed_global_route_with_secrets(&pool, &master_key, mock_addr, &[("sk-f022", 1)], "{}")
            .await;

    let app = build_wire_app(&pool, &master_key).await;

    let request = chat_request_with_header(&token, "x-fail-429-once-then-200", "1");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "short Retry-After <=2s should succeed on retry rather than failing over"
    );
    assert_eq!(
        mock_state.chat_calls.load(Ordering::SeqCst),
        2,
        "upstream must have been called twice (initial 429 + retry 200)"
    );
}
