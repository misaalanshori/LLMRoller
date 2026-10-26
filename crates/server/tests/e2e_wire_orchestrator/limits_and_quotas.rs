use super::*;

#[tokio::test]
#[doc = "Spec: SEC-8 - data-plane requests up to the configured maximum are read, and larger bodies are rejected before routing"]
async fn test_data_plane_body_limit_matches_configured_maximum() {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("body_limit.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xDD; 32]);
    seed_base(&pool, &master_key).await;

    let entropy = OsEntropy;
    let mut tx = pool.begin().await.expect("begin");
    let (_user_id, token) =
        seed_user_with_client_key(&mut tx, "body_limit_user", None, &entropy).await;
    tx.commit().await.expect("commit");

    let prefix = r#"{"model":"not-in-catalog","messages":[{"role":"user","content":""#;
    let suffix = r#""}]}"#;
    let padding = 2 * 1024 * 1024 - prefix.len() - suffix.len();
    let body = format!("{prefix}{}{suffix}", "x".repeat(padding));
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.clone()))
            .expect("request")
    };

    let app = build_wire_app_with_max_body(&pool, &master_key, 32).await;
    let response = app.oneshot(request()).await.expect("2 MiB response");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a 2 MiB body must pass the default 32 MiB data-plane limit and reach model resolution"
    );

    let app = build_wire_app_with_max_body(&pool, &master_key, 1).await;
    let response = app.oneshot(request()).await.expect("oversize response");
    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body above the configured maximum must be rejected by the data-plane limit"
    );
}

#[tokio::test]
#[doc = "Spec: USR-5 — F05 own-tier requests bypass a zero user-global budget"]
async fn test_own_tier_bypasses_zero_user_global_budget() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("usr5.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xCC; 32]);
    seed_base(&pool, &master_key).await;

    let entropy = OsEntropy;
    let mut tx = pool.begin().await.expect("begin");
    let (user_id, token) = seed_user_with_client_key(&mut tx, "usr5_user", Some(0), &entropy).await;
    let (_provider_id, _key_id) = seed_provider_route(
        &mut tx,
        &master_key,
        &entropy,
        mock_addr,
        OwnershipScope::Personal,
        Some(user_id),
        "{}",
    )
    .await;
    tx.commit().await.expect("commit");

    let app = build_wire_app(&pool, &master_key).await;
    let response = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "own personal routes must not be blocked by the user-global cap"
    );
    assert_eq!(mock_state.chat_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[doc = "Spec: SHR-1 — F05 shared-tier requests enforce the grantor's zero share cap before egress"]
async fn test_shared_tier_zero_cap_returns_429_without_upstream_call() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("shr1.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xDD; 32]);
    seed_base(&pool, &master_key).await;

    let entropy = OsEntropy;
    let mut tx = pool.begin().await.expect("begin");
    let (grantor_id, _grantor_token) =
        seed_user_with_client_key(&mut tx, "shr1_grantor", None, &entropy).await;
    let (grantee_id, grantee_token) =
        seed_user_with_client_key(&mut tx, "shr1_grantee", None, &entropy).await;
    let (provider_id, _key_id) = seed_provider_route(
        &mut tx,
        &master_key,
        &entropy,
        mock_addr,
        OwnershipScope::Personal,
        Some(grantor_id),
        "{}",
    )
    .await;
    let fingerprint: String =
        sqlx::query_scalar("SELECT share_config_fingerprint FROM providers WHERE id = ?")
            .bind(provider_id.to_string())
            .fetch_one(&mut *tx)
            .await
            .expect("provider fingerprint");

    sqlx::query(
        "INSERT INTO provider_shares (
             id, provider_id, owner_user_id, grantee_user_id, max_monthly_micro_usd,
             accepted_at, accepted_config_fingerprint, admin_approved_at, enabled, created_at
         ) VALUES (?, ?, ?, ?, 0, 1000, ?, 1000, 1, 1000)",
    )
    .bind(llmroller_core::ids::ShareId::generate().to_string())
    .bind(provider_id.to_string())
    .bind(grantor_id.to_string())
    .bind(grantee_id.to_string())
    .bind(fingerprint)
    .execute(&mut *tx)
    .await
    .expect("insert share");
    tx.commit().await.expect("commit");

    let app = build_wire_app(&pool, &master_key).await;
    let response = app
        .oneshot(chat_request(&grantee_token))
        .await
        .expect("response");
    assert_eq!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "an exhausted share cap must fail closed with 429"
    );
    assert_eq!(
        mock_state.chat_calls.load(Ordering::SeqCst),
        0,
        "no upstream request may be issued when the share cap is exhausted"
    );
}

#[tokio::test]
#[doc = "Spec: API-8 - F03 all-quota failures select 429 with Retry-After"]
async fn test_all_quota_failures_return_429_with_retry_after() {
    let (mock_addr, _mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f03_terminal_quota.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0xE9; 32]);
    let token = seed_global_route_with_secrets(
        &pool,
        &master_key,
        mock_addr,
        &[("sk-quota", 1), ("sk-quota-2", 2)],
        "{}",
    )
    .await;
    let app = build_wire_app(&pool, &master_key).await;

    let response = app.oneshot(chat_request(&token)).await.expect("response");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response
            .headers()
            .get("retry-after")
            .map(|value| value.to_str().expect("retry-after")),
        Some("7")
    );
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"]["code"], "upstream_rate_limited");
}
