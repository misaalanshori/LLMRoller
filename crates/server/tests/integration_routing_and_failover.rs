//! Integration tests for routing, pinning, durable response_links, failover loop, and terminal error selection.
//!
//! Spec: Section 6.7, Section 10.4, Section 10.5, Section 11.1.2, Section 21.2,
//! Requirements: RTE-2, RTE-7, RTE-8, RTE-9, RTE-10, RTE-11, RTE-12, CLK-2, PRV-8.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use axum::http::StatusCode;
use bytes::Bytes;
use tempfile::tempdir;

use llmroller_core::eligibility::CandidatePoolTier;
use llmroller_core::ids::{ClientKeyId, ModelPk, UpstreamKeyId, UserId};
use llmroller_core::pins::{PinKey, PinStore};
use llmroller_core::rbac::{ActorScope, Role, RoutingScope, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, FakeClock, OsEntropy};
use llmroller_proxy::failover::{
    HardRouteChecks, HardRouteDecision, TryOutcome, evaluate_hard_route_decision,
    select_terminal_error,
};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pins::{ResponseLinkRow, get_response_link_tx, insert_response_link_tx};
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

async fn setup_failover_test_db() -> (tempfile::TempDir, sqlx::SqlitePool, MasterKey, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("failover_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x99; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "failover_user",
            password: "supersecretpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("user");

    (dir, writer_pool, master_key, user.id)
}

#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-9, RTE-10, RTE-11, RTE-12, CLK-2, PRV-8"]
#[test]
fn test_pin_store_concurrency_loser_adoption_and_recovery_repin() {
    let store = Arc::new(PinStore::new(10_000));
    let clock = Arc::new(FakeClock::new(1_000_000_000_000));

    let client_key_id = ClientKeyId::generate();
    let pin_key = PinKey {
        client_key_id,
        canonical_id: "claude-sonnet-4.6".to_string(),
        session_hash: "sess-hash-100".to_string(),
    };

    let k1 = UpstreamKeyId::generate();
    let k2 = UpstreamKeyId::generate();
    let k3 = UpstreamKeyId::generate();

    // 1. 100 concurrent turn-1 requests select one key and write one confirmed pin on first valid frame (RTE-7)
    let confirmed_count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..100 {
        let st = Arc::clone(&store);
        let clk = Arc::clone(&clock);
        let pk = pin_key.clone();
        let cc = Arc::clone(&confirmed_count);

        handles.push(thread::spawn(move || {
            let ticket = st
                .acquire(
                    &pk,
                    &*clk,
                    86_400_000,
                    true,
                    300,
                    RoutingScope::PersonalFirst,
                    Some(CandidatePoolTier::Own),
                    |_| true,
                    |_| Some((k1, CandidatePoolTier::Own)),
                )
                .expect("ticket");

            if st.first_valid_frame(&pk, &ticket, clk.now_ms(), 86_400_000) {
                cc.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }

    for h in handles {
        h.join().expect("join");
    }

    assert_eq!(store.len(), 1);
    assert!(confirmed_count.load(Ordering::SeqCst) >= 1);

    // 2. 10 concurrent requests where K1 returns 429 coalesce onto K2 via loser adoption (RTE-8)
    let pin_key_failover = PinKey {
        client_key_id,
        canonical_id: "claude-sonnet-4.6".to_string(),
        session_hash: "sess-hash-failover".to_string(),
    };

    let mut tickets = Vec::new();
    for _ in 0..10 {
        let t = store
            .acquire(
                &pin_key_failover,
                &*clock,
                86_400_000,
                true,
                300,
                RoutingScope::PersonalFirst,
                Some(CandidatePoolTier::Own),
                |_| true,
                |_| Some((k1, CandidatePoolTier::Own)),
            )
            .expect("ticket");
        tickets.push(t);
    }

    let k3_calls = Arc::new(AtomicUsize::new(0));
    let mut failover_handles = Vec::new();
    for ticket in tickets {
        let st = Arc::clone(&store);
        let pk = pin_key_failover.clone();
        let k3_c = Arc::clone(&k3_calls);

        failover_handles.push(thread::spawn(move || {
            let ft = st
                .pre_first_byte_failure(
                    &pk,
                    &ticket,
                    k1,
                    1_000_000_001_000,
                    |_| true,
                    |tried| {
                        if tried.contains(&k1) && !tried.contains(&k2) {
                            Some((k2, CandidatePoolTier::Own))
                        } else {
                            k3_c.fetch_add(1, Ordering::SeqCst);
                            Some((k3, CandidatePoolTier::Own))
                        }
                    },
                )
                .expect("failover ticket");
            assert_eq!(ft.upstream_key_id, k2);
        }));
    }

    for h in failover_handles {
        h.join().expect("join");
    }

    assert_eq!(k3_calls.load(Ordering::SeqCst), 0, "zero calls to K3");

    // 3. Admin move during in-flight stream is not reverted by stale FirstValidFrame (RTE-11)
    let ticket_move = store
        .acquire(
            &pin_key,
            &*clock,
            86_400_000,
            true,
            300,
            RoutingScope::PersonalFirst,
            Some(CandidatePoolTier::Own),
            |_| true,
            |_| Some((k1, CandidatePoolTier::Own)),
        )
        .expect("ticket");

    store.admin_move(&pin_key, k2, clock.now_ms());

    // Stale confirm from ticket_move (k1)
    let stale_confirmed =
        store.first_valid_frame(&pin_key, &ticket_move, clock.now_ms(), 86_400_000);
    assert!(!stale_confirmed);

    // Add a pin on k1 to verify that invalidating k2 purges only k2 pins
    let pin_key_k1 = PinKey {
        client_key_id,
        canonical_id: "other-model".to_string(),
        session_hash: "sess-hash-k1".to_string(),
    };
    store.acquire(
        &pin_key_k1,
        &*clock,
        86_400_000,
        true,
        300,
        RoutingScope::PersonalFirst,
        Some(CandidatePoolTier::Own),
        |_| true,
        |_| Some((k1, CandidatePoolTier::Own)),
    );

    // 4. Invalidation on key/owner disable purges pins (PRV-8, CLK-2)
    store.invalidate_key(&k2);
    assert_eq!(store.len(), 1); // pin_key_k1 remains!
}

#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-9, RTE-10, RTE-11, RTE-12, CLK-2, PRV-8"]
#[tokio::test]
async fn test_durable_response_links_and_hard_route_decision() {
    let (_dir, pool, _master_key, user_id) = setup_failover_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let model_pk = ModelPk::generate();
    let key_id = UpstreamKeyId::generate();

    // Insert dummy model and upstream_key for foreign key integrity
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, origin) VALUES (?, 'resp-model', 'global')",
    )
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model");

    let provider_id = llmroller_core::ids::ProviderId::generate();
    sqlx::query("INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, share_config_fingerprint, created_at) VALUES (?, 'P', 'openrouter', 'https://api.com', '{}', 'global', 'fp', 1000)")
        .bind(provider_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert provider");

    sqlx::query("INSERT INTO upstream_keys (id, provider_id, label, secret_enc, secret_last4, priority, master_key_id, created_at) VALUES (?, ?, 'K', X'00', 'test', 100, 'key-0', 1000)")
        .bind(key_id.to_string())
        .bind(provider_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert key");

    // Insert client key for foreign key integrity
    let client_key_id = ClientKeyId::generate();
    sqlx::query("INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at) VALUES (?, ?, 'Key', 'llr_123', 'hash123', 1000)")
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert client key");

    // 1. Chained Responses turn writes durable response_link (RTE-9, RTE-12)
    let link = ResponseLinkRow {
        response_id: "resp_parent_123".to_string(),
        owner_user_id: user_id,
        client_key_id,
        upstream_key_id: key_id,
        model_pk,
        session_hash: "inherited-parent-session-hash".to_string(),
        status: "completed".to_string(),
        store_flag: true,
        created_at: 1000,
        updated_at: 1000,
        expires_at: 1000 + 86400,
    };
    insert_response_link_tx(&mut tx, &link)
        .await
        .expect("insert response link");

    // Retrieve response link
    let fetched = get_response_link_tx(&mut tx, "resp_parent_123")
        .await
        .expect("get link")
        .expect("link must exist");
    assert_eq!(fetched.session_hash, "inherited-parent-session-hash");
    assert_eq!(fetched.upstream_key_id, key_id);

    // 2. HardKey decision table (RTE-12):
    // When linked key passes C1..C5 and C6..C8: routes without failover
    let checks_ok = HardRouteChecks {
        c1_c5_passed: true,
        c6_c8_passed: true,
        c6_failed: false,
        c7_failed: false,
        c8_failed: false,
        c2_or_c3_failed: false,
        c4_or_c5_failed: false,
    };
    let dec_ok = evaluate_hard_route_decision(&checks_ok, key_id, 0);
    assert_eq!(dec_ok, HardRouteDecision::RouteWithoutFailover(key_id));

    // When linked key fails C6 (cooling down) or C8 (window full): returns 429 with Retry-After
    let checks_429 = HardRouteChecks {
        c1_c5_passed: true,
        c6_c8_passed: false,
        c6_failed: true,
        c7_failed: false,
        c8_failed: false,
        c2_or_c3_failed: false,
        c4_or_c5_failed: false,
    };
    let dec_429 = evaluate_hard_route_decision(&checks_429, key_id, 30);
    assert_eq!(
        dec_429,
        HardRouteDecision::RateLimit429 {
            retry_after_secs: 30
        }
    );

    // When linked key fails C2 (share revoked) or C3 (owner disabled): returns 403 KeyUnavailable
    let checks_403 = HardRouteChecks {
        c1_c5_passed: false,
        c6_c8_passed: false,
        c6_failed: false,
        c7_failed: false,
        c8_failed: false,
        c2_or_c3_failed: true,
        c4_or_c5_failed: false,
    };
    let dec_403 = evaluate_hard_route_decision(&checks_403, key_id, 0);
    assert_eq!(dec_403, HardRouteDecision::Forbidden403KeyUnavailable);

    // When linked key fails C4 (model kill switch): returns 403 ModelOrProtocolBlocked
    let checks_kill = HardRouteChecks {
        c1_c5_passed: false,
        c6_c8_passed: false,
        c6_failed: false,
        c7_failed: false,
        c8_failed: false,
        c2_or_c3_failed: false,
        c4_or_c5_failed: true,
    };
    let dec_kill = evaluate_hard_route_decision(&checks_kill, key_id, 0);
    assert_eq!(
        dec_kill,
        HardRouteDecision::Forbidden403ModelOrProtocolBlocked
    );

    tx.commit().await.expect("commit");
}

#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-9, RTE-10, RTE-11, RTE-12, CLK-2, PRV-8"]
#[test]
fn test_terminal_error_selector_precedence_and_retry_after() {
    // 1. If any try ended in RequestRejected, return that verbatim
    let reject_body = Bytes::from_static(b"{\"error\":\"flagged\"}");
    let outcomes_rej = vec![
        TryOutcome::RateLimited {
            retry_after_secs: Some(5),
        },
        TryOutcome::RequestRejected {
            status: StatusCode::FORBIDDEN,
            body: reject_body.clone(),
        },
        TryOutcome::WaitTimeout,
    ];
    let (status, code, _ra, body) = select_terminal_error(&outcomes_rej);
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code, "request_rejected");
    assert_eq!(body, Some(reject_body));

    // 2. Tries with 429 and Retry-After 7, 3, 12 -> returns 429 with lowest positive Retry-After: 3
    let outcomes_429 = vec![
        TryOutcome::RateLimited {
            retry_after_secs: Some(7),
        },
        TryOutcome::QuotaExhausted {
            retry_after_secs: Some(3),
        },
        TryOutcome::RateLimited {
            retry_after_secs: Some(12),
        },
    ];
    let (status, code, ra, _body) = select_terminal_error(&outcomes_429);
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code, "upstream_rate_limited");
    assert_eq!(ra, Some(3)); // Lowest positive!

    // 3. Tries ending in wait_timeout -> HTTP 504
    let outcomes_timeout = vec![TryOutcome::HostFailure, TryOutcome::WaitTimeout];
    let (status, code, _ra, _body) = select_terminal_error(&outcomes_timeout);
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(code, "upstream_timeout");

    // 4. Otherwise HTTP 502
    let outcomes_502 = vec![TryOutcome::HostFailure];
    let (status, code, _ra, _body) = select_terminal_error(&outcomes_502);
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(code, "upstream_unreachable");
}

#[doc = "Spec: RTE-1, RTE-2, RTE-3, RTE-4, RTE-5, RTE-6, RTE-7, RTE-8, RTE-9, RTE-10, RTE-11, RTE-12, RTE-13, RTE-14, Partial: API-1, Partial: PERF-1"]
#[tokio::test]
async fn test_v1_endpoints_and_auxiliary_routes() {
    use arc_swap::ArcSwap;
    use axum::body::Body;
    use axum::http::{Request, header};
    use http_body_util::BodyExt as _;
    use llmroller_core::provider::ProviderKind;
    use llmroller_core::rbac::OwnershipScope;
    use llmroller_core::seams::secret_string;
    use llmroller_proxy::auth::ProxyAuthState;
    use llmroller_proxy::listeners::data_plane_router;
    use llmroller_server::reloader::load_initial_snapshot;
    use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
    use llmroller_store::providers::{
        NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
    };
    use tower::ServiceExt as _;

    let mock_app = axum::Router::new()
        .route(
            "/chat/completions",
            axum::routing::post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"id":"chatcmpl-1","choices":[{"message":{"role":"assistant","content":"hi"}}],"usage":{"prompt_tokens":5,"completion_tokens":5,"total_tokens":10}}"#,
                )
            }),
        )
        .route(
            "/responses",
            axum::routing::post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"id":"resp-1","output":"ok"}"#,
                )
            }),
        )
        .route(
            "/messages",
            axum::routing::post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"id":"msg-1","content":[{"type":"text","text":"ok"}]}"#,
                )
            }),
        )
        .route(
            "/messages/count_tokens",
            axum::routing::post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"input_tokens": 10}"#,
                )
            }),
        );
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let mock_addr = mock_listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_app).await;
    });

    let (_dir, pool, master_key, user_id) = setup_failover_test_db().await;
    let actor = ActorScope::user(user_id, Role::User);
    let mut tx = pool.begin().await.expect("begin");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let provider = create_provider_tx(
        &mut tx,
        &system_actor,
        NewProvider {
            name: "Mock Routing Provider".to_string(),
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
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Mock Upstream Key".to_string(),
            secret: secret_string("sk-mock-routing-key"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");

    let model_pk1 = ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id, exposed) VALUES (?, 'gpt-4o', 1)")
        .bind(model_pk1.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert model gpt-4o");

    let model_pk2 = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed) VALUES (?, 'claude-3-5-sonnet', 1)",
    )
    .bind(model_pk2.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model claude");

    for model_pk in [model_pk1, model_pk2] {
        sqlx::query(
            "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, discovered_at) \
             VALUES (?, ?, 'gpt-4o', '[\"chat\",\"responses\",\"messages\"]', 'active', 1000)",
        )
        .bind(upstream_key.id.to_string())
        .bind(model_pk.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert model route");
    }

    let created_key = create_client_key_tx(
        &mut tx,
        &actor,
        user_id,
        NewClientKey {
            label: "Test Key".to_string(),
            allowed_models: None,
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &OsEntropy,
        1000,
    )
    .await
    .expect("create client key");

    // Insert response_links for resp_test_1 so state-bound operations succeed
    let link = ResponseLinkRow {
        response_id: "resp_test_1".to_string(),
        owner_user_id: user_id,
        client_key_id: created_key.client_key.id,
        upstream_key_id: upstream_key.id,
        model_pk: model_pk1,
        session_hash: "sess_link_1".to_string(),
        status: "completed".to_string(),
        store_flag: true,
        created_at: 1000,
        updated_at: 1000,
        expires_at: 1000 + 86400,
    };
    insert_response_link_tx(&mut tx, &link)
        .await
        .expect("insert response link");

    use llmroller_core::seams::ExposeSecret as _;
    let client_key_token = created_key.raw_token.expose_secret().to_string();

    tx.commit().await.expect("commit");

    let snapshot = load_initial_snapshot(&pool).await.expect("snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let auth_state = ProxyAuthState::new(shared_snapshot, pool.clone(), vec![])
        .with_master_key(master_key.clone());
    auth_state
        .is_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let app = data_plane_router(auth_state);

    // 1. POST /v1/chat/completions
    let req_chat = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .expect("req");
    let resp = app.clone().oneshot(req_chat).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. POST /v1/responses
    let req_resp = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model":"gpt-4o","input":"test prompt"}"#))
        .expect("req");
    let resp = app.clone().oneshot(req_resp).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);

    // 3. POST /v1/messages
    let req_msg = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model":"claude-3-5-sonnet","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("req");
    let resp = app.clone().oneshot(req_msg).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. POST /v1/messages/count_tokens
    let req_count = Request::builder()
        .method("POST")
        .uri("/v1/messages/count_tokens")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model":"claude-3-5-sonnet","messages":[{"role":"user","content":"hello world"}]}"#,
        ))
        .expect("req");
    let resp = app.clone().oneshot(req_count).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(json["input_tokens"].as_i64().is_some());

    // 5. POST /v1/responses/compact
    let req_compact = Request::builder()
        .method("POST")
        .uri("/v1/responses/compact")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model":"gpt-4o","input":"compact me"}"#))
        .expect("req");
    let resp = app.clone().oneshot(req_compact).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);

    // 6. State-bound endpoints: GET/DELETE /v1/responses/{id}, /input_items, /cancel
    let req_get = Request::builder()
        .method("GET")
        .uri("/v1/responses/unknown_id")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req_get).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let req_input_items = Request::builder()
        .method("GET")
        .uri("/v1/responses/resp_test_1/input_items")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req_input_items).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);

    let req_cancel = Request::builder()
        .method("POST")
        .uri("/v1/responses/resp_test_1/cancel")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req_cancel).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);

    let req_del = Request::builder()
        .method("DELETE")
        .uri("/v1/responses/resp_test_1")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req_del).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
}
