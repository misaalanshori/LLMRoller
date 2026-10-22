//! Control-plane integration tests for Service Accounts, key pause/resume, and
//! user preference persistence.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk
//! Spec: ./docs/srdd/14.j. SRDD - Service Accounts Directory and Machine Principal Management.md
//! Spec: ./docs/srdd/14.k. SRDD - Service Account Detail View, CIDR IP Restrictions, and Child Keys.md#14k4

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
use tokio::sync::mpsc;
use tower::ServiceExt as _;

const ADMIN_SESSION: &str = "admin_session_token_0123456789abcdef";
const ADMIN_CSRF: &str = "admin_csrf_token_0123456789abcdef";
const USER_SESSION: &str = "user_session_token_0123456789abcdef0";
const USER_CSRF: &str = "user_csrf_token_0123456789abcdef0";

struct TestContext {
    _dir: TempDir,
    app: axum::Router,
    pool: sqlx::SqlitePool,
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

    let system = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin = create_user(
        &pool,
        &system,
        NewUser {
            username: "sa_admin",
            password: "Password12345!#",
            role: Role::Admin,
            must_change_password: false,
        },
        &DeterministicRng::new(2),
        1,
    )
    .await
    .expect("admin");
    let user = create_user(
        &pool,
        &system,
        NewUser {
            username: "sa_user",
            password: "Password12345!#",
            role: Role::User,
            must_change_password: false,
        },
        &DeterministicRng::new(3),
        1,
    )
    .await
    .expect("user");

    let now = SystemClock::new().now_ms();
    for (token, csrf, user_id) in [
        (ADMIN_SESSION, ADMIN_CSRF, admin.id),
        (USER_SESSION, USER_CSRF, user.id),
    ] {
        insert_web_session(
            &pool,
            NewWebSession {
                id_hash: &llmroller_store::crypto::sha256_hex(token.as_bytes()),
                user_id,
                csrf_hash: &llmroller_store::crypto::sha256_hex(csrf.as_bytes()),
                ip_addr: Some("127.0.0.1"),
                user_agent: Some("test"),
                created_at: now,
                expires_at: now + 86_400_000,
            },
        )
        .await
        .expect("session");
    }

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
        entropy: Arc::new(DeterministicRng::new(4)),
    };
    let mut api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    api_state.writer = Some(writer);
    api_state.attach_reloader(snapshot.notification_sender(), snapshot.version_receiver());
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x24; 32]));

    TestContext {
        _dir: dir,
        app: control_plane_router_full(setup_state, auth_state, api_state),
        pool,
    }
}

fn authed(method: &str, uri: &str, session: &str, csrf: &str) -> axum::http::request::Builder {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", format!("llmroller_session={session}"))
        .header("content-type", "application/json");
    if csrf.is_empty() {
        builder
    } else {
        builder.header("x-csrf-token", csrf)
    }
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[doc = "Spec: CLK-6, Spec: CLK-7, Spec: UI-5, Spec: SEC-17"]
async fn service_account_crud_cidrs_key_pause_and_preferences() {
    let ctx = setup().await;

    // 1. Create a Service Account (`CLK-6`, Section 14.j.4).
    let create = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                "/api/user/service-accounts",
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::from(
                serde_json::json!({
                    "name": "agents",
                    "description": "CI runners",
                    "allowed_cidrs": ["203.0.113.10"]
                })
                .to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("create response");
    assert_eq!(create.status(), StatusCode::CREATED);
    let account = json_body(create).await;
    let account_id = account["id"].as_str().expect("account id").to_string();
    assert_eq!(account["workspace_scope"], "personal");
    assert_eq!(account["allowed_cidrs"][0], "203.0.113.10/32");

    // 2. Duplicate names are reported as 409 `service_account_name_taken`.
    let duplicate = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                "/api/user/service-accounts",
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::from(
                serde_json::json!({"name": "agents"}).to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("duplicate response");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    let duplicate_body = json_body(duplicate).await;
    assert_eq!(
        duplicate_body["error"]["code"],
        "service_account_name_taken"
    );

    // 3. Replace CIDRs with pre-existing host entries and verify canonicalization.
    let ip_restrictions = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "PUT",
                &format!("/api/user/service-accounts/{account_id}/ip-restrictions"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::from(
                serde_json::json!({
                    "allowed_cidrs_text": "203.0.113.10, 2001:db8::1/48\n198.51.100.0/24"
                })
                .to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("cidr response");
    assert_eq!(ip_restrictions.status(), StatusCode::OK);
    let cidrs = json_body(ip_restrictions).await;
    assert_eq!(
        cidrs["allowed_cidrs"],
        serde_json::json!(["203.0.113.10/32", "2001:db8::/48", "198.51.100.0/24"])
    );

    // 4. A standard user must not see another tenant's Service Account (SEC-17).
    let cross_tenant = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "GET",
                &format!("/api/user/service-accounts/{account_id}"),
                USER_SESSION,
                "",
            )
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("cross tenant response");
    assert_eq!(cross_tenant.status(), StatusCode::NOT_FOUND);

    // 5. Create a child client key bound to the Service Account (`CLK-6`).
    let key_response = ctx
        .app
        .clone()
        .oneshot(
            authed("POST", "/api/user/client-keys", ADMIN_SESSION, ADMIN_CSRF)
                .body(Body::from(
                    serde_json::json!({
                        "label": "ci-key",
                        "service_account_id": account_id
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("key response");
    assert_eq!(key_response.status(), StatusCode::CREATED);
    let key = json_body(key_response).await;
    let key_id = key["client_key"]["id"]
        .as_str()
        .expect("client key id")
        .to_string();
    assert_eq!(key["client_key"]["service_account_id"], account_id);

    // 6. Pause and resume the key (`CLK-7`).
    let paused = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                &format!("/api/user/client-keys/{key_id}/pause"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("pause response");
    assert_eq!(paused.status(), StatusCode::OK);
    let paused_key = json_body(paused).await;
    assert!(paused_key["paused_at"].is_number());
    assert_eq!(paused_key["disabled"], true);

    let paused_at: Option<i64> =
        sqlx::query_scalar("SELECT paused_at FROM client_keys WHERE id = ?")
            .bind(&key_id)
            .fetch_one(&ctx.pool)
            .await
            .expect("read paused_at");
    assert!(paused_at.is_some(), "pause must persist to SQLite");

    let resumed = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                &format!("/api/user/client-keys/{key_id}/resume"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("resume response");
    assert_eq!(resumed.status(), StatusCode::OK);
    let resumed_key = json_body(resumed).await;
    assert!(resumed_key["paused_at"].is_null());
    assert_eq!(resumed_key["disabled"], false);

    // 7. Preferences persist (`UI-5`, `UI-6`).
    let prefs = ctx
        .app
        .clone()
        .oneshot(
            authed("PATCH", "/api/user/preferences", ADMIN_SESSION, ADMIN_CSRF)
                .body(Body::from(
                    serde_json::json!({
                        "ui_theme": "light",
                        "ui_nav_layout": "sidebar",
                        "ui_locale": "en"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("prefs response");
    assert_eq!(prefs.status(), StatusCode::OK);
    let row: (String, String, String) = sqlx::query_as(
        "SELECT ui_theme, ui_nav_layout, ui_locale FROM users WHERE username = 'sa_admin'",
    )
    .fetch_one(&ctx.pool)
    .await
    .expect("read preferences");
    assert_eq!(
        row,
        ("light".to_string(), "sidebar".to_string(), "en".to_string())
    );

    // 8. Unsupported locale is rejected with 422.
    let invalid = ctx
        .app
        .clone()
        .oneshot(
            authed("PATCH", "/api/user/preferences", ADMIN_SESSION, ADMIN_CSRF)
                .body(Body::from(
                    serde_json::json!({"ui_locale": "fr"}).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("invalid prefs response");
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
