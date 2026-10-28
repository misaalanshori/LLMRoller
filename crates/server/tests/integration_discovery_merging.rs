//! End-to-end tests for upstream `/models` discovery on key creation and manual refresh.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#66-model-discovery-catalog-and-merging-mdl
//! Requirements: MDL-1, API-8

use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http_body_util::BodyExt as _;
use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::{ApiAppState, SetupAppState, control_plane_router_full};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, DeterministicRng, SystemClock};
use llmroller_server::discovery_poller::{DiscoveryPollerDeps, start_discovery_poller};
use llmroller_server::reloader::{load_initial_snapshot, start_reloader};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};
use llmroller_store::writer::start_writer;
use tempfile::TempDir;
use tower::ServiceExt as _;

const SESSION_TOKEN: &str = "discovery_session_token_0123456789";
const CSRF_TOKEN: &str = "discovery_csrf_token_0123456789";

#[derive(Clone, Default)]
struct MockModelsState {
    models: Arc<Mutex<Vec<String>>>,
    authorization: Arc<Mutex<Vec<Option<String>>>>,
}

async fn mock_models_handler(State(state): State<MockModelsState>, headers: HeaderMap) -> Response {
    state.authorization.lock().expect("auth log").push(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    );
    let data: Vec<serde_json::Value> = state
        .models
        .lock()
        .expect("models")
        .iter()
        .map(|id| serde_json::json!({ "id": id, "object": "model" }))
        .collect();
    axum::Json(serde_json::json!({ "object": "list", "data": data })).into_response()
}

async fn spawn_mock_upstream(models: &[&str]) -> (std::net::SocketAddr, MockModelsState) {
    let state = MockModelsState {
        models: Arc::new(Mutex::new(
            models.iter().map(|model| (*model).to_string()).collect(),
        )),
        authorization: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/models", get(mock_models_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, state)
}

struct TestContext {
    _dir: TempDir,
    app: Router,
    pool: sqlx::SqlitePool,
    runtime: Arc<llmroller_core::runtime_state::RuntimeState>,
}

async fn test_instance_id(pool: &sqlx::SqlitePool) -> [u8; 16] {
    let bytes: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_optional(pool)
            .await
            .expect("instance id query")
            .unwrap_or_else(|| vec![0_u8; 16]);
    bytes.try_into().unwrap_or([0_u8; 16])
}

async fn setup() -> TestContext {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("discovery.db"), PoolRole::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0x31; 32]);
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
            username: "discovery_admin",
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
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(SESSION_TOKEN.as_bytes()),
            user_id: admin.id,
            csrf_hash: &llmroller_store::crypto::sha256_hex(CSRF_TOKEN.as_bytes()),
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
    let (reloader_tx, reloader_rx) = tokio::sync::mpsc::channel(32);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx.clone());
    let (reloader_handle, _reloader_task) = start_reloader(
        pool.clone(),
        shared,
        runtime.clone(),
        reloader_rx,
        reloader_tx,
    );

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
    let mut api_state =
        ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    api_state.writer = Some(writer.clone());
    api_state.attach_reloader(
        reloader_handle.notification_sender(),
        reloader_handle.version_receiver(),
    );
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x24; 32]));

    let (discovery_tx, _discovery_task) = start_discovery_poller(DiscoveryPollerDeps {
        pool: pool.clone(),
        writer,
        runtime_state: runtime.clone(),
        master_key,
        instance_id: test_instance_id(&pool).await,
    });
    api_state.discovery_tx = Some(discovery_tx);

    let app = control_plane_router_full(setup_state, auth_state, api_state);
    TestContext {
        _dir: dir,
        app,
        pool,
        runtime,
    }
}

fn authed(method: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", format!("llmroller_session={SESSION_TOKEN}"))
        .header("content-type", "application/json")
        .header("x-csrf-token", CSRF_TOKEN)
}

async fn json_body(response: Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json response")
}

async fn wait_for<F, Fut>(mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if condition().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not met within 5s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[doc = "Spec: MDL-1, API-8 — key creation and manual refresh run upstream /models discovery"]
async fn test_key_addition_triggers_upstream_models_discovery() {
    let (mock_addr, mock_state) = spawn_mock_upstream(&["gpt-4o", "gpt-4o-mini"]).await;
    let ctx = setup().await;

    let provider_body = serde_json::json!({
        "name": "discovery_mock",
        "kind": "custom",
        "base_url": format!("http://{mock_addr}"),
        "allow_private_network": true,
        "enabled": true
    });
    let response = ctx
        .app
        .clone()
        .oneshot(
            authed("POST", "/admin/api/providers")
                .body(Body::from(provider_body.to_string()))
                .expect("provider request"),
        )
        .await
        .expect("provider response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let provider = json_body(response).await;
    let provider_id = provider["id"].as_str().expect("provider id");

    let key_body = serde_json::json!({
        "label": "discovery key",
        "secret": "sk-discovery",
        "enabled": true
    });
    let response = ctx
        .app
        .clone()
        .oneshot(
            authed("POST", &format!("/admin/api/providers/{provider_id}/keys"))
                .body(Body::from(key_body.to_string()))
                .expect("key request"),
        )
        .await
        .expect("key response");
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "key creation succeeds"
    );

    let pool = ctx.pool.clone();
    wait_for(|| {
        let pool = pool.clone();
        async move {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM model_routes r
                 JOIN models m ON m.model_pk = r.model_pk
                 WHERE m.canonical_id IN ('gpt-4o', 'gpt-4o-mini')",
            )
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
            count == 2
        }
    })
    .await;

    let protocols: String = sqlx::query_scalar(
        "SELECT r.protocols FROM model_routes r
         JOIN models m ON m.model_pk = r.model_pk
         WHERE m.canonical_id = 'gpt-4o'",
    )
    .fetch_one(&ctx.pool)
    .await
    .expect("gpt-4o route");
    assert!(
        protocols.contains("chat") && protocols.contains("messages"),
        "discovered routes must advertise the supported protocols: {protocols}"
    );

    let authorization = mock_state
        .authorization
        .lock()
        .expect("auth log")
        .first()
        .cloned()
        .flatten();
    assert_eq!(
        authorization.as_deref(),
        Some("Bearer sk-discovery"),
        "discovery must authenticate with the stored upstream secret"
    );

    // API-8: a successful refresh clears PlanRestricted route blocks for the key.
    let key_id = llmroller_core::ids::UpstreamKeyId::parse(
        &sqlx::query_scalar::<_, String>("SELECT id FROM upstream_keys LIMIT 1")
            .fetch_one(&ctx.pool)
            .await
            .expect("key id"),
    )
    .expect("parse key id");
    let model_pk = llmroller_core::ids::ModelPk::parse(
        &sqlx::query_scalar::<_, String>(
            "SELECT model_pk FROM models WHERE canonical_id = 'gpt-4o'",
        )
        .fetch_one(&ctx.pool)
        .await
        .expect("model pk"),
    )
    .expect("parse model pk");
    let now = SystemClock::new().now_ms();
    ctx.runtime
        .block_route_protocol(key_id, model_pk, "chat", now + 3_600_000);
    assert!(
        ctx.runtime
            .is_route_protocol_blocked(key_id, model_pk, "chat", now)
    );

    // A manual refresh discovers newly available models and retires absent ones.
    *mock_state.models.lock().expect("models") = vec!["gpt-4o".to_string(), "gpt-5".to_string()];
    let response = ctx
        .app
        .clone()
        .oneshot(
            authed("POST", "/admin/api/models/refresh")
                .body(Body::empty())
                .expect("refresh request"),
        )
        .await
        .expect("refresh response");
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let pool = ctx.pool.clone();
    let runtime = ctx.runtime.clone();
    wait_for(move || {
        let pool = pool.clone();
        let runtime = runtime.clone();
        async move {
            let gpt5: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM model_routes r
                 JOIN models m ON m.model_pk = r.model_pk
                 WHERE m.canonical_id = 'gpt-5'",
            )
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
            let missing: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM model_routes r
                 JOIN models m ON m.model_pk = r.model_pk
                 WHERE m.canonical_id = 'gpt-4o-mini' AND r.status = 'missing'",
            )
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
            gpt5 == 1
                && missing == 1
                && !runtime.is_route_protocol_blocked(key_id, model_pk, "chat", now)
        }
    })
    .await;

    assert!(
        !ctx.runtime
            .is_route_protocol_blocked(key_id, model_pk, "chat", now),
        "a successful discovery refresh must clear protocol blocks"
    );
}
