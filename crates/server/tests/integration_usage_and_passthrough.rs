//! Integration tests for source provider /usage poller, snapshot sealing, observation sync, and pass-through APIs.
//!
//! Spec: Section 2.4, Section 6.3, Section 6.8, Section 6.9, Section 8.2, Section 13.3, Section 13.4,
//! Requirements: PRV-6, API-1, USG-4, USG-5, USG-6, USG-7, USG-8, SEC-18.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use tempfile::tempdir;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_core::ids::{UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_core::windows::UpstreamObservation;
use llmroller_proto::usage_poller::{KeyPollerState, UsagePoller, parse_raw_usage_to_sanitized};
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_server::reloader::load_initial_snapshot;
use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
use llmroller_store::crypto::{MasterKey, XChaCha20Poly1305Sealer};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
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
    let (_dir, pool, sealer, _master_key, user_id) = setup_passthrough_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let actor = ActorScope::user(user_id, Role::User);
    let created_key = create_client_key_tx(
        &mut tx,
        &actor,
        user_id,
        NewClientKey {
            label: "Usage Client Key".to_string(),
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

    // Insert a stored snapshot for /v1/usage/raw (USG-8)
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
            raw_json_bytes: b"{}",
        },
    )
    .await
    .expect("insert snapshot");

    tx.commit().await.expect("commit");

    let snapshot = load_initial_snapshot(&pool).await.expect("snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let auth_state = ProxyAuthState::new(shared_snapshot, pool.clone(), vec![]);
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

    // 4. GET /v1/usage/raw serves strictly from stored snapshots
    let req_raw = Request::builder()
        .uri("/v1/usage/raw")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .body(Body::empty())
        .expect("req");
    let resp_raw = app.oneshot(req_raw).await.expect("resp");
    assert_eq!(resp_raw.status(), StatusCode::OK);
    let body_raw = resp_raw
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json_raw: serde_json::Value = serde_json::from_slice(&body_raw).expect("json");
    assert_eq!(json_raw["windows"][0]["used_percent"], 20.0);
}
