//! Integration tests for source provider /usage poller, snapshot sealing, observation sync, and pass-through APIs.
//!
//! Spec: Section 2.4, Section 6.3, Section 6.8, Section 6.9, Section 8.2, Section 13.3, Section 13.4,
//! Requirements: PRV-6, API-1, USG-4, USG-5, USG-6, USG-7, USG-8, SEC-18.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use tempfile::tempdir;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_core::ids::{UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{ExposeSecret as _, OsEntropy, secret_string};
use llmroller_core::windows::UpstreamObservation;
use llmroller_proto::usage_poller::{KeyPollerState, UsagePoller, parse_raw_usage_to_sanitized};
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_server::reloader::load_initial_snapshot;
use llmroller_store::client_keys::{NewClientKey, create_client_key, create_client_key_tx};
use llmroller_store::crypto::{MasterKey, XChaCha20Poly1305Sealer};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::usage_snapshots::{NewUsageSnapshot, insert_usage_snapshot_tx};
use llmroller_store::users::{NewUser, create_user};

async fn setup_passthrough_test_db() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    Arc<XChaCha20Poly1305Sealer>,
    MasterKey,
    UserId,
) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("passthrough_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xDD; 32]);
    let instance = load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let sealer = Arc::new(XChaCha20Poly1305Sealer::new(
        master_key.clone(),
        instance.instance_id,
        Arc::new(entropy),
    ));

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "passthrough_user",
            password: "supersecretpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("user");

    (dir, writer_pool, sealer, master_key, user.id)
}

#[doc = "Spec: PRV-6, API-1, USG-4, USG-5, USG-6, USG-7, USG-8, SEC-18"]
#[tokio::test]
async fn test_golden_usage_parsing_and_sealed_raw_json_storage() {
    let (_dir, pool, sealer, _master_key, _user_id) = setup_passthrough_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let key_id = UpstreamKeyId::generate();
    let provider_id = llmroller_core::ids::ProviderId::generate();

    sqlx::query("INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, share_config_fingerprint, created_at) VALUES (?, 'P', 'opencode_go', 'https://api.com', '{}', 'global', 'fp', 1000)")
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

    // 1. Replay fixtures/usage/opencode_go.json
    let opencode_raw = include_bytes!("../../../fixtures/usage/opencode_go.json");
    let opencode_val: serde_json::Value = serde_json::from_slice(opencode_raw).expect("parse json");
    let sanitized_opencode =
        parse_raw_usage_to_sanitized(ProviderKind::OpencodeGo, &opencode_val).expect("sanitized");
    assert_eq!(sanitized_opencode.windows.len(), 2);
    assert_eq!(sanitized_opencode.windows[0].kind, "5h");
    assert_eq!(sanitized_opencode.windows[0].used_percent, 25.0);

    // 2. Replay fixtures/usage/command_code_credits.json
    let cmd_raw = include_bytes!("../../../fixtures/usage/command_code_credits.json");
    let cmd_val: serde_json::Value = serde_json::from_slice(cmd_raw).expect("parse json");
    let sanitized_cmd =
        parse_raw_usage_to_sanitized(ProviderKind::CommandCode, &cmd_val).expect("sanitized");
    assert_eq!(sanitized_cmd.balance_micro_usd, Some(15_000_000));

    // 3. Replay fixtures/usage/openrouter_key.json
    let or_raw = include_bytes!("../../../fixtures/usage/openrouter_key.json");
    let or_val: serde_json::Value = serde_json::from_slice(or_raw).expect("parse json");
    let sanitized_or =
        parse_raw_usage_to_sanitized(ProviderKind::Openrouter, &or_val).expect("sanitized");
    assert_eq!(sanitized_or.windows[0].used_percent, 12.5);

    // 4. Sealed raw_json_enc storage with XChaCha20Poly1305 (SEC-18, USG-5)
    let sanitized_json_str = serde_json::to_string(&sanitized_opencode).expect("serialize");
    let snap_id = insert_usage_snapshot_tx(
        &mut tx,
        &sealer,
        NewUsageSnapshot {
            key_id: &key_id,
            now_ms: 1000,
            http_status: 200,
            plan_name: sanitized_opencode.plan_name.as_deref(),
            windows_json: "[]",
            balance_micro_usd: None,
            master_key_id: "key-0",
            sanitized_json: &sanitized_json_str,
            raw_json_bytes: opencode_raw,
        },
    )
    .await
    .expect("insert snapshot");

    assert!(snap_id.starts_with("snap-"));
    tx.commit().await.expect("commit");
}

#[doc = "Spec: PRV-6, API-1, USG-4, USG-5, USG-6, USG-7, USG-8, SEC-18"]
#[test]
fn test_single_flight_poller_and_concurrency_bounds() {
    let poller_state = Arc::new(KeyPollerState::default());

    // 1. First poll acquires in-flight lock
    assert!(poller_state.try_start_poll(1000, false));
    // Concurrent poll immediately rejected (single-flight deduplication) (USG-4)
    assert!(!poller_state.try_start_poll(1000, false));

    // Cooldown test: after finish, automatic poll rejected within 30s
    poller_state.finish_poll();
    assert!(!poller_state.try_start_poll(15_000, false)); // 14s elapsed < 30s
    // Manual poll permitted after 10s cooldown
    assert!(poller_state.try_start_poll(15_000, true)); // 14s > 10s
    poller_state.finish_poll();

    // 2. 40 concurrent requests trigger exactly ONE poll under concurrency semaphore
    let global_poller = Arc::new(UsagePoller::default());
    let poll_counter = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..40 {
        let ps = Arc::clone(&poller_state);
        let pc = Arc::clone(&poll_counter);
        let gp = Arc::clone(&global_poller);

        handles.push(thread::spawn(move || {
            if ps.try_start_poll(100_000, false) {
                let _permit = gp.semaphore.try_acquire();
                pc.fetch_add(1, Ordering::SeqCst);
                thread::sleep(std::time::Duration::from_millis(10));
                ps.finish_poll();
            }
        }));
    }

    for h in handles {
        h.join().expect("join");
    }

    assert_eq!(
        poll_counter.load(Ordering::SeqCst),
        1,
        "exactly 1 poll triggered among 40 concurrent triggers"
    );
}

#[doc = "Spec: PRV-6, API-1, USG-4, USG-5, USG-6, USG-7, USG-8, SEC-18"]
#[test]
fn test_upstream_observation_reconciliation_and_resets_at_expiry() {
    let t0 = 1_000_000_000_000i64;
    let resets_at = t0 + 1_800_000; // in 30 min

    // Provider reports 25% ($2.50 of $10) at t0 while local was 10% ($1.00) (USG-6)
    let obs = UpstreamObservation {
        used_micro_usd: 2_500_000,
        observed_at_ms: t0,
        local_at_observation: 1_000_000,
        resets_at_ms: Some(resets_at),
    };

    // Effective used projects 2.50M + 0 = 2.50M
    assert_eq!(obs.project(1_000_000, t0 + 10_000), 2_500_000);

    // Subsequent local spend of +2% ($0.20 -> local = 1.20M) reads 2.70M (USG-6)
    assert_eq!(obs.project(1_200_000, t0 + 20_000), 2_700_000);

    // When now_ms >= resets_at_ms + 1ms: observation expires and local ring governs alone
    assert!(obs.is_expired(resets_at + 1));
    assert_eq!(obs.project(1_200_000, resets_at + 1), 1_200_000);
}

#[doc = "Spec: PRV-6, API-1, USG-4, USG-5, USG-6, USG-7, USG-8, SEC-18"]
#[tokio::test]
async fn test_v1_usage_key_and_raw_passthrough_endpoints() {
    let (_dir, pool, sealer, master_key, user_id) = setup_passthrough_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let actor = ActorScope::user(user_id, Role::User);
    let created_key = create_client_key_tx(
        &mut tx,
        &actor,
        user_id,
        NewClientKey {
            label: "Usage Client Key".to_string(),
            service_account_id: None,
            allowed_models: None,
            routing_scope: None,
            rate_limit_rpm: Some(60),
            budget_micro_usd_month: Some(10_000_000),
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &OsEntropy,
        1000,
    )
    .await
    .expect("create client key");

    use llmroller_core::seams::ExposeSecret as _;
    let client_key_token = created_key.raw_token.expose_secret().to_string();

    let up_key = UpstreamKeyId::generate();
    let p_id = llmroller_core::ids::ProviderId::generate();

    sqlx::query("INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, share_config_fingerprint, created_at) VALUES (?, 'P', 'openrouter', 'https://api.com', '{}', 'global', 'fp', 1000)")
        .bind(p_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert provider");

    sqlx::query("INSERT INTO upstream_keys (id, provider_id, label, secret_enc, secret_last4, priority, master_key_id, created_at) VALUES (?, ?, 'K', X'00', 'test', 100, 'key-0', 1000)")
        .bind(up_key.to_string())
        .bind(p_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert key");

    // Insert a stored snapshot on Global key for /v1/usage/raw (USG-8)
    let sanitized_doc =
        r#"{"status":"ok","windows":[{"kind":"5h","status":"ok","used_percent":20.0}]}"#;
    insert_usage_snapshot_tx(
        &mut tx,
        &sealer,
        NewUsageSnapshot {
            key_id: &up_key,
            now_ms: 1000,
            http_status: 200,
            plan_name: Some("pro"),
            windows_json: "[]",
            balance_micro_usd: None,
            master_key_id: "key-0",
            sanitized_json: sanitized_doc,
            raw_json_bytes: b"{\"raw_field\":\"global_hidden\"}",
        },
    )
    .await
    .expect("insert snapshot");

    // Insert a stored snapshot on Own personal key for /v1/usage/raw (USG-8)
    let p_personal_id = llmroller_core::ids::ProviderId::generate();
    let up_key_personal = UpstreamKeyId::generate();
    sqlx::query("INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, owner_user_id, share_config_fingerprint, created_at) VALUES (?, 'P_Personal', 'openrouter', 'https://api.com', '{}', 'personal', ?, 'fp', 1000)")
        .bind(p_personal_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert personal provider");

    sqlx::query("INSERT INTO upstream_keys (id, provider_id, label, secret_enc, secret_last4, priority, master_key_id, owner_user_id, created_at) VALUES (?, ?, 'K_Personal', X'00', 'test', 100, 'key-0', ?, 1000)")
        .bind(up_key_personal.to_string())
        .bind(p_personal_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert personal key");

    insert_usage_snapshot_tx(
        &mut tx,
        &sealer,
        NewUsageSnapshot {
            key_id: &up_key_personal,
            now_ms: 2000,
            http_status: 200,
            plan_name: Some("pro"),
            windows_json: "[]",
            balance_micro_usd: None,
            master_key_id: "key-0",
            sanitized_json: sanitized_doc,
            raw_json_bytes: b"{\"raw_field\":\"personal_secret\"}",
        },
    )
    .await
    .expect("insert personal snapshot");

    tx.commit().await.expect("commit");

    let snapshot = load_initial_snapshot(&pool).await.expect("snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let auth_state = ProxyAuthState::new(shared_snapshot.clone(), pool.clone(), vec![])
        .with_master_key(master_key);
    auth_state
        .is_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let app = data_plane_router(auth_state);

    // 1. GET /v1/usage rejects ?session_id= query string with HTTP 400 (SEC-9)
    let req_rejected = Request::builder()
        .uri("/v1/usage?session_id=injected_in_query")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .body(Body::empty())
        .expect("req");
    let resp_rej = app.clone().oneshot(req_rejected).await.expect("resp");
    assert_eq!(resp_rej.status(), StatusCode::BAD_REQUEST);

    // 2. GET /v1/usage with session header succeeds
    let req_valid = Request::builder()
        .uri("/v1/usage")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .header("x-llmroller-session-id", "header-session-ok")
        .body(Body::empty())
        .expect("req");
    let resp_val = app.clone().oneshot(req_valid).await.expect("resp");
    assert_eq!(resp_val.status(), StatusCode::OK);
    let body = resp_val
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["session_id"], "header-session-ok");

    // 3. GET /v1/key returns key quota info
    let req_key = Request::builder()
        .uri("/v1/key")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .body(Body::empty())
        .expect("req");
    let resp_key = app.clone().oneshot(req_key).await.expect("resp");
    assert_eq!(resp_key.status(), StatusCode::OK);
    let body_key = resp_key
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json_key: serde_json::Value = serde_json::from_slice(&body_key).expect("json");
    assert_eq!(json_key["rate_limit_rpm"], 60);

    // 4. GET /v1/usage/raw serves decrypted raw_json on own personal key (USG-8)
    let req_raw = Request::builder()
        .uri("/v1/usage/raw")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .body(Body::empty())
        .expect("req");
    let resp_raw = app.clone().oneshot(req_raw).await.expect("resp");
    assert_eq!(resp_raw.status(), StatusCode::OK);
    let body_raw = resp_raw
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json_raw: serde_json::Value = serde_json::from_slice(&body_raw).expect("json");
    assert_eq!(
        json_raw["raw_field"], "personal_secret",
        "Own personal key must decrypt and expose raw_json on /v1/usage/raw (USG-8)"
    );

    // 5. Client key of another user (bob) querying Global key receives allow-list sanitized_json only (USG-8, SEC-18)
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let bob_user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "bob_usage_test",
            password: "supersecretbobpass123",
            role: Role::User,
            must_change_password: false,
        },
        &OsEntropy,
        2000,
    )
    .await
    .expect("create bob user");
    let bob_id = bob_user.id;
    let bob_actor = ActorScope::user(bob_id, Role::User);
    let bob_key = create_client_key(
        &pool,
        &bob_actor,
        bob_id,
        NewClientKey {
            label: "Bob Key".to_string(),
            service_account_id: None,
            allowed_models: None,
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &OsEntropy,
        2000,
    )
    .await
    .expect("create bob key");
    let bob_token = bob_key.raw_token.expose_secret().to_string();

    let updated_snapshot = load_initial_snapshot(&pool)
        .await
        .expect("updated snapshot");
    shared_snapshot.store(Arc::new(updated_snapshot));

    let req_bob_raw = Request::builder()
        .uri("/v1/usage/raw")
        .header(header::AUTHORIZATION, format!("Bearer {bob_token}"))
        .body(Body::empty())
        .expect("req");
    let resp_bob_raw = app.oneshot(req_bob_raw).await.expect("resp");
    assert_eq!(resp_bob_raw.status(), StatusCode::OK);
    let body_bob_raw = resp_bob_raw
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json_bob_raw: serde_json::Value = serde_json::from_slice(&body_bob_raw).expect("json");
    assert_eq!(
        json_bob_raw["windows"][0]["used_percent"], 20.0,
        "Non-owner must receive allow-list sanitized_json (USG-8, SEC-18)"
    );
    assert!(
        json_bob_raw.get("raw_field").is_none(),
        "Non-owner must NOT see raw_json fields on Global/Shared keys (USG-8, SEC-18)"
    );
}

#[derive(Clone, Default)]
struct MockUsageState {
    calls: Arc<AtomicUsize>,
}

async fn mock_usage_handler(State(state): State<MockUsageState>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "data": {
                "label": "pro",
                "limit": 10.0,
                "usage": 2.5,
                "limit_remaining": 7.5
            }
        })),
    )
        .into_response()
}

async fn spawn_mock_usage() -> (SocketAddr, MockUsageState) {
    let state = MockUsageState::default();
    let app = Router::new()
        .route("/usage", get(mock_usage_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock usage listener");
    let addr = listener.local_addr().expect("mock usage addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, state)
}

#[doc = "Spec: USG-7, USG-8, SEC-9, SEC-18"]
#[tokio::test]
async fn test_usage_fresh_roles_stale_header_and_missing_snapshot_404() {
    let (_dir, pool, sealer, master_key, user_id) = setup_passthrough_test_db().await;
    let (mock_addr, mock_state) = spawn_mock_usage().await;

    let mut tx = pool.begin().await.expect("begin");
    let user_actor = ActorScope::user(user_id, Role::User);
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    let client_key = create_client_key_tx(
        &mut tx,
        &user_actor,
        user_id,
        NewClientKey {
            label: "Live Usage Key".to_string(),
            service_account_id: None,
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
    .expect("client key");
    let client_token = client_key.raw_token.expose_secret().to_string();

    let personal_provider = create_provider_tx(
        &mut tx,
        &user_actor,
        NewProvider {
            name: "Personal Live Usage".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: format!("http://{mock_addr}"),
            config_json: r#"{"usage_endpoint":"/usage"}"#.to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(user_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: true,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("personal provider");

    let personal_key = create_upstream_key_tx(
        &mut tx,
        &user_actor,
        &master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: personal_provider.id,
            label: "Personal Live Key".to_string(),
            secret: secret_string("sk-live-usage"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("personal key");

    let no_snapshot_key = create_upstream_key_tx(
        &mut tx,
        &user_actor,
        &master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: personal_provider.id,
            label: "Personal No Snapshot".to_string(),
            secret: secret_string("sk-no-snapshot"),
            priority: Some(90),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1001,
    )
    .await
    .expect("no-snapshot key");

    let global_provider = create_provider_tx(
        &mut tx,
        &system_actor,
        NewProvider {
            name: "Global Live Usage".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://global.example".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("global provider");

    let global_key = create_upstream_key_tx(
        &mut tx,
        &system_actor,
        &master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: global_provider.id,
            label: "Global Live Key".to_string(),
            secret: secret_string("sk-global-usage"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("global key");

    let global_sanitized =
        r#"{"status":"ok","windows":[{"kind":"monthly","status":"ok","used_percent":20.0}]}"#;
    insert_usage_snapshot_tx(
        &mut tx,
        &sealer,
        NewUsageSnapshot {
            key_id: &global_key.id,
            now_ms: 1000,
            http_status: 200,
            plan_name: Some("global"),
            windows_json: "[]",
            balance_micro_usd: None,
            master_key_id: "key-0",
            sanitized_json: global_sanitized,
            raw_json_bytes: b"{\"raw_field\":\"global_hidden\"}",
        },
    )
    .await
    .expect("global snapshot");
    tx.commit().await.expect("commit");

    let snapshot = load_initial_snapshot(&pool).await.expect("snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let auth_state = ProxyAuthState::new(shared_snapshot.clone(), pool.clone(), vec![])
        .with_master_key(master_key);
    auth_state
        .is_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let app = data_plane_router(auth_state);

    // Own personal key: fresh=1 performs exactly one live fetch.
    let personal_uri = format!("/v1/usage?upstream_key_id={}&fresh=1", personal_key.id);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&personal_uri)
                .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let stale_header = response
        .headers()
        .get("x-llmroller-usage-stale-ms")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .expect("stale header is always present");
    assert_eq!(stale_header, "0");
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["source"], "upstream");
    assert_eq!(json["stale"], false);
    assert_eq!(json["upstream_key_id"], personal_key.id.to_string());
    assert_eq!(json["source_provider_usage"]["data"]["usage"], 2.5);
    assert_eq!(
        json["pinned_route"]["upstream_key_id"],
        personal_key.id.to_string()
    );
    assert_eq!(mock_state.calls.load(Ordering::SeqCst), 1);

    // A second fresh request inside the 10 s gate must not hit upstream again.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&personal_uri)
                .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(mock_state.calls.load(Ordering::SeqCst), 1);

    // Global key: fresh=1 is ignored for a standard user, cached sanitized data is served.
    let global_uri = format!("/v1/usage?upstream_key_id={}&fresh=1", global_key.id);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&global_uri)
                .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        mock_state.calls.load(Ordering::SeqCst),
        1,
        "global fresh requests must not trigger upstream polling"
    );
    let stale_ms = response
        .headers()
        .get("x-llmroller-usage-stale-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .expect("numeric stale header");
    assert!(stale_ms > 60_000, "old snapshot must be reported stale");
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json["source_provider_usage"]["windows"][0]["used_percent"],
        20.0
    );
    assert!(json["source_provider_usage"].get("raw_field").is_none());

    // Stored-only endpoint: no snapshot for this key means 404, not an empty success.
    let raw_missing = format!("/v1/usage/raw?upstream_key_id={}", no_snapshot_key.id);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&raw_missing)
                .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Inaccessible/nonexistent key is 404, not 403 (no existence oracle).
    let inaccessible = format!(
        "/v1/usage/raw?upstream_key_id={}",
        UpstreamKeyId::generate()
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri(&inaccessible)
                .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
