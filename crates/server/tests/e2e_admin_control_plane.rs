//! Control-plane mutation barrier, audit hash-chain, and upstream key probe tests.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#611-web-ui-and-audit-logging-ui--aud
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::{ApiAppState, SetupAppState, control_plane_router_full};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, DeterministicRng, SystemClock};
use llmroller_server::reloader::{load_initial_snapshot, start_reloader};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};
use llmroller_store::writer::start_writer;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;
use tower::ServiceExt as _;

const SESSION_TOKEN: &str = "control_plane_session_token_0123456789";
const CSRF_TOKEN: &str = "control_plane_csrf_token_0123456789";

struct TestContext {
    _dir: TempDir,
    app: axum::Router,
    ip: String,
    admin_id: llmroller_core::ids::UserId,
    snapshot: llmroller_server::reloader::ReloaderHandle,
}

async fn setup() -> TestContext {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("control.db"), PoolRole::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0x42; 32]);
    load_or_initialize(
        &pool,
        "key-0",
        &master_key,
        Arc::new(DeterministicRng::new(1)),
        1,
    )
    .await
    .expect("instance");
    seed_defaults(&pool, None, 1).await.expect("settings");

    let admin = create_user(
        &pool,
        &ActorScope::system(SystemTask::Writer, SystemSeal::store()),
        NewUser {
            username: "cp_admin",
            password: "Password12345!#",
            role: Role::Admin,
            must_change_password: false,
        },
        &DeterministicRng::new(2),
        1,
    )
    .await
    .expect("admin");

    let now = SystemClock::new().now_ms();
    let csrf_hash = llmroller_store::crypto::sha256_hex(CSRF_TOKEN.as_bytes());
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(SESSION_TOKEN.as_bytes()),
            user_id: admin.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("test"),
            created_at: now,
            expires_at: now + 86_400_000,
        },
    )
    .await
    .expect("session");

    let initial = load_initial_snapshot(&pool).await.expect("snapshot");
    let shared = Arc::new(ArcSwap::new(Arc::new(initial)));
    let runtime = Arc::new(llmroller_core::runtime_state::RuntimeState::new());
    let (reloader_tx, reloader_rx) = mpsc::channel(32);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx.clone());
    let (snapshot, _reloader_task) =
        start_reloader(pool.clone(), shared, runtime, reloader_rx, reloader_tx);

    let mut auth_state = AuthAppState::new(
        pool.clone(),
        AuthConfig {
            trusted_proxies: Vec::new(),
            dev_insecure_cookies: true,
            require_tls: false,
        },
    );
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();

    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: None,
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(SystemClock::new()),
        entropy: Arc::new(DeterministicRng::new(3)),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let mut api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    api_state.writer = Some(writer);
    api_state.attach_reloader(snapshot.notification_sender(), snapshot.version_receiver());
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x24; 32]));

    let app = control_plane_router_full(setup_state, auth_state, api_state);
    TestContext {
        _dir: dir,
        app,
        ip: "127.0.0.1".to_string(),
        admin_id: admin.id,
        snapshot,
    }
}

fn authed(method: &str, uri: &str, csrf: bool) -> axum::http::request::Builder {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", format!("llmroller_session={SESSION_TOKEN}"))
        .header("content-type", "application/json");
    if csrf {
        builder.header("x-csrf-token", CSRF_TOKEN)
    } else {
        builder
    }
}

/// Spec: REL-4, AUD-1, PRV-6
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutations_publish_before_ack_chain_audit_and_probe_upstream() {
    let ctx = setup().await;

    // Mock upstream that rejects credentials.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = [0_u8; 1024];
            let _ = socket.read(&mut buffer).await;
            let _ = socket
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
        }
    });

    let provider_body = serde_json::json!({
        "name": "local_mock",
        "kind": "custom",
        "base_url": format!("http://{addr}"),
        "allow_private_network": true,
        "enabled": true
    });
    let created = ctx
        .app
        .clone()
        .oneshot(
            authed("POST", "/admin/api/providers", true)
                .body(Body::from(provider_body.to_string()))
                .expect("request"),
        )
        .await
        .expect("provider response");
    assert!(
        created.status().is_success(),
        "provider create failed: {}",
        created.status()
    );
    let body = http_body_util::BodyExt::collect(created.into_body())
        .await
        .expect("body")
        .to_bytes();
    let provider: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let provider_id = provider["id"].as_str().expect("provider id").to_string();

    // Read-your-writes: the published snapshot already contains the provider.
    let published = ctx.snapshot.snapshot();
    assert!(
        published
            .providers
            .keys()
            .any(|id| id.to_string() == provider_id)
    );
    assert!(published.config_version > 0);

    // Audit chain endpoint now returns valid entries written through the canonical appender.
    let audit = ctx
        .app
        .clone()
        .oneshot(
            authed("GET", "/admin/api/audit-log", false)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("audit response");
    assert_eq!(audit.status(), StatusCode::OK);
    let audit_body = http_body_util::BodyExt::collect(audit.into_body())
        .await
        .expect("body")
        .to_bytes();
    let entries: serde_json::Value = serde_json::from_slice(&audit_body).expect("json");
    let entries = entries.as_array().expect("array");
    assert!(!entries.is_empty(), "audit log must contain mutations");
    assert!(entries[0]["hash"].as_str().is_some_and(|h| h.len() == 64));

    // Create an upstream key and probe it against the 401 mock.
    let key_body = serde_json::json!({ "label": "mock", "secret": "sk-test-abcdefgh" });
    let key_resp = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                &format!("/admin/api/providers/{provider_id}/keys"),
                true,
            )
            .body(Body::from(key_body.to_string()))
            .expect("request"),
        )
        .await
        .expect("key response");
    assert!(key_resp.status().is_success(), "key create failed");
    let key_json: serde_json::Value = serde_json::from_slice(
        &http_body_util::BodyExt::collect(key_resp.into_body())
            .await
            .expect("body")
            .to_bytes(),
    )
    .expect("json");
    let key_id = key_json["id"].as_str().expect("key id").to_string();

    let probe = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                &format!("/api/user/upstream-keys/{key_id}/test"),
                true,
            )
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("probe response");
    assert_eq!(probe.status(), StatusCode::OK);
    let probe_json: serde_json::Value = serde_json::from_slice(
        &http_body_util::BodyExt::collect(probe.into_body())
            .await
            .expect("body")
            .to_bytes(),
    )
    .expect("json");
    assert_eq!(probe_json["status"], "error");
    assert_eq!(probe_json["http_status"], 401);

    // Session helper ip field is intentionally unused beyond documentation.
    assert_eq!(ctx.ip, "127.0.0.1");
    assert_ne!(ctx.admin_id.to_string(), key_id);
}
