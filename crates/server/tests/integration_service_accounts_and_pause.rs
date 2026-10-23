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
    admin_id: llmroller_core::ids::UserId,
    user_id: llmroller_core::ids::UserId,
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
        audit_hmac_key: None,
    };
    let mut api_state =
        ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    api_state.writer = Some(writer);
    api_state.attach_reloader(snapshot.notification_sender(), snapshot.version_receiver());
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x24; 32]));

    TestContext {
        _dir: dir,
        app: control_plane_router_full(setup_state, auth_state, api_state),
        pool,
        admin_id: admin.id,
        user_id: user.id,
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

    // Verify server-side rendered base template reflects updated preferences (UI-5, UI-6)
    let app_page = ctx
        .app
        .clone()
        .oneshot(
            authed("GET", "/app", ADMIN_SESSION, "")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("app page response");
    assert_eq!(app_page.status(), StatusCode::OK);
    let app_bytes = http_body_util::BodyExt::collect(app_page.into_body())
        .await
        .expect("body")
        .to_bytes();
    let app_html = std::str::from_utf8(&app_bytes).expect("html");
    assert!(app_html.contains("data-theme=\"light\""));
    assert!(app_html.contains("data-nav-layout=\"sidebar\""));
    assert!(app_html.contains("lang=\"en\""));

    // Verify Indonesian locale rendering (UI-5)
    let id_prefs = ctx
        .app
        .clone()
        .oneshot(
            authed("PATCH", "/api/user/preferences", ADMIN_SESSION, ADMIN_CSRF)
                .body(Body::from(
                    serde_json::json!({
                        "ui_locale": "id"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("id prefs response");
    assert_eq!(id_prefs.status(), StatusCode::OK);

    let id_page = ctx
        .app
        .clone()
        .oneshot(
            authed("GET", "/app", ADMIN_SESSION, "")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("id page response");
    assert_eq!(id_page.status(), StatusCode::OK);
    let id_bytes = http_body_util::BodyExt::collect(id_page.into_body())
        .await
        .expect("body")
        .to_bytes();
    let id_html = std::str::from_utf8(&id_bytes).expect("html");
    assert!(id_html.contains("lang=\"id\""));
    assert!(id_html.contains("Ruang Kerja Pribadi"));
    assert!(id_html.contains("Keluar"));

    // 8. Disabling TOTP requires the current password (`AUTH-9`, `SEC-4`).
    let wrong_password = ctx
        .app
        .clone()
        .oneshot(
            authed("POST", "/api/user/totp/disable", ADMIN_SESSION, ADMIN_CSRF)
                .body(Body::from(
                    serde_json::json!({"password": "wrong-password"}).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("wrong password response");
    assert_eq!(wrong_password.status(), StatusCode::UNAUTHORIZED);

    let missing_password = ctx
        .app
        .clone()
        .oneshot(
            authed("POST", "/api/user/totp/disable", ADMIN_SESSION, ADMIN_CSRF)
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("missing password response");
    assert_eq!(missing_password.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let disabled = ctx
        .app
        .clone()
        .oneshot(
            authed("POST", "/api/user/totp/disable", ADMIN_SESSION, ADMIN_CSRF)
                .body(Body::from(
                    serde_json::json!({"password": "Password12345!#"}).to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("disable response");
    assert_eq!(disabled.status(), StatusCode::OK);

    // 9. Unsupported locale is rejected with 422.
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

    // 10. GET /api/user/preferences returns stored preferences (UI-5).
    let get_prefs = ctx
        .app
        .clone()
        .oneshot(
            authed("GET", "/api/user/preferences", ADMIN_SESSION, "")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("get prefs response");
    assert_eq!(get_prefs.status(), StatusCode::OK);
    let prefs_body = json_body(get_prefs).await;
    assert_eq!(prefs_body["ui_theme"], "light");
    assert_eq!(prefs_body["ui_nav_layout"], "sidebar");
    assert_eq!(prefs_body["ui_locale"], "id");

    // 11. Child key creation via POST /api/user/service-accounts/{id}/keys (CLK-6).
    let sa_key_response = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                &format!("/api/user/service-accounts/{account_id}/keys"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::from(
                serde_json::json!({
                    "label": "sa-child-key"
                })
                .to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("sa key response");
    assert_eq!(sa_key_response.status(), StatusCode::CREATED);

    // 12. GET /admin/api/service-accounts (CLK-6).
    let admin_sa_list = ctx
        .app
        .clone()
        .oneshot(
            authed("GET", "/admin/api/service-accounts", ADMIN_SESSION, "")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("admin sa list response");
    assert_eq!(admin_sa_list.status(), StatusCode::OK);

    // 13. Admin pause and resume endpoints (/admin/api/client-keys/{id}/pause|resume).
    let admin_pause = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                &format!("/admin/api/client-keys/{key_id}/pause"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("admin pause response");
    assert_eq!(admin_pause.status(), StatusCode::OK);

    let admin_resume = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                &format!("/admin/api/client-keys/{key_id}/resume"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("admin resume response");
    assert_eq!(admin_resume.status(), StatusCode::OK);

    // 14. GET /app/partials/audit-log renders user self-scoped audit rows (AUD-2).
    let audit_partial = ctx
        .app
        .clone()
        .oneshot(
            authed("GET", "/app/partials/audit-log", ADMIN_SESSION, "")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("audit partial response");
    assert_eq!(audit_partial.status(), StatusCode::OK);

    // 15. Admin service account full CRUD lifecycle (CLK-6).
    let admin_create = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "POST",
                "/admin/api/service-accounts",
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::from(
                serde_json::json!({
                    "name": "admin-agents",
                    "description": "Global admin runners",
                    "allowed_cidrs": ["10.0.0.1"]
                })
                .to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("admin create response");
    assert_eq!(admin_create.status(), StatusCode::CREATED);
    let admin_sa = json_body(admin_create).await;
    let admin_sa_id = admin_sa["id"].as_str().expect("admin sa id").to_string();

    let admin_get = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "GET",
                &format!("/admin/api/service-accounts/{admin_sa_id}"),
                ADMIN_SESSION,
                "",
            )
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("admin get response");
    assert_eq!(admin_get.status(), StatusCode::OK);
    let admin_get_body = json_body(admin_get).await;
    assert_eq!(admin_get_body["name"], "admin-agents");

    let admin_patch = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "PATCH",
                &format!("/admin/api/service-accounts/{admin_sa_id}"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::from(
                serde_json::json!({
                    "name": "admin-agents-renamed",
                    "disabled": true
                })
                .to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("admin patch response");
    assert_eq!(admin_patch.status(), StatusCode::OK);
    let admin_patch_body = json_body(admin_patch).await;
    assert_eq!(admin_patch_body["name"], "admin-agents-renamed");
    assert_eq!(admin_patch_body["disabled"], true);

    let admin_cidrs = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "PUT",
                &format!("/admin/api/service-accounts/{admin_sa_id}/ip-restrictions"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::from(
                serde_json::json!({
                    "allowed_cidrs": ["192.168.1.0/24"]
                })
                .to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("admin cidrs response");
    assert_eq!(admin_cidrs.status(), StatusCode::OK);
    let admin_cidrs_body = json_body(admin_cidrs).await;
    assert_eq!(
        admin_cidrs_body["allowed_cidrs"],
        serde_json::json!(["192.168.1.0/24"])
    );

    let admin_delete = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "DELETE",
                &format!("/admin/api/service-accounts/{admin_sa_id}"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("admin delete response");
    assert_eq!(admin_delete.status(), StatusCode::NO_CONTENT);

    // 16. User patch, CIDR update, and delete through atomic mutations (CLK-6).
    let user_patch = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "PATCH",
                &format!("/api/user/service-accounts/{account_id}"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::from(
                serde_json::json!({
                    "description": "Updated CI runners description"
                })
                .to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("user patch response");
    assert_eq!(user_patch.status(), StatusCode::OK);
    let user_patch_body = json_body(user_patch).await;
    assert_eq!(
        user_patch_body["description"],
        "Updated CI runners description"
    );

    let user_delete = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "DELETE",
                &format!("/api/user/service-accounts/{account_id}"),
                ADMIN_SESSION,
                ADMIN_CSRF,
            )
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("user delete response");
    assert_eq!(user_delete.status(), StatusCode::NO_CONTENT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[doc = "Spec: SEC-17, Spec: USR-3, Spec: SHR-1"]
async fn provider_level_shares_do_not_expose_unrelated_tenant_keys() {
    let ctx = setup().await;
    let admin_id = ctx.admin_id.to_string();
    let user_id = ctx.user_id.to_string();

    let provider_shared = "01ARZ3NDEKTSV4RRFFQ69G5PA1";
    let provider_other = "01ARZ3NDEKTSV4RRFFQ69G5PA2";
    let key_shared = "01ARZ3NDEKTSV4RRFFQ69G5PK1";
    let key_other = "01ARZ3NDEKTSV4RRFFQ69G5PK2";
    let model_pk = "01ARZ3NDEKTSV4RRFFQ69G5PM1";

    for (provider_id, name) in [
        (provider_shared, "shared-provider"),
        (provider_other, "other-provider"),
    ] {
        sqlx::query(
            "INSERT INTO providers (id, ownership_scope, owner_user_id, kind, name, base_url,
                config_json, share_config_fingerprint, usage_url_cross_origin_ack,
                allow_private_network, enabled, created_at)
             VALUES (?, 'personal', ?, 'custom', ?, 'http://127.0.0.1:9', '{}', 'fp-a', 0, 1, 1, 1000)",
        )
        .bind(provider_id)
        .bind(&admin_id)
        .bind(name)
        .execute(&ctx.pool)
        .await
        .expect("insert provider");
    }
    for (key_id, provider_id, label) in [
        (key_shared, provider_shared, "shared-key"),
        (key_other, provider_other, "other-key"),
    ] {
        sqlx::query(
            "INSERT INTO upstream_keys (id, provider_id, owner_user_id, label, secret_enc,
                master_key_id, secret_last4, created_at)
             VALUES (?, ?, ?, ?, X'00', 'key-0', '0000', 1000)",
        )
        .bind(key_id)
        .bind(provider_id)
        .bind(&admin_id)
        .bind(label)
        .execute(&ctx.pool)
        .await
        .expect("insert upstream key");
    }
    sqlx::query("INSERT INTO models (model_pk, canonical_id, exposed) VALUES (?, 'gpt-4o', 1)")
        .bind(model_pk)
        .execute(&ctx.pool)
        .await
        .expect("insert model");
    for key_id in [key_shared, key_other] {
        sqlx::query(
            "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols,
                status, enabled, discovered_at)
             VALUES (?, ?, 'gpt-4o', '[\"chat\"]', 'active', 1, 1000)",
        )
        .bind(key_id)
        .bind(model_pk)
        .execute(&ctx.pool)
        .await
        .expect("insert route");
    }

    // A provider-level grant for `provider_other` must not make `key_shared`
    // (a different provider) visible to the grantee (F042).
    sqlx::query(
        "INSERT INTO provider_shares (id, provider_id, upstream_key_id, owner_user_id,
            grantee_user_id, accepted_at, accepted_config_fingerprint, enabled, created_at)
         VALUES ('01ARZ3NDEKTSV4RRFFQ69G5PS1', ?, NULL, ?, ?, 1000, 'fp-a', 1, 1000)",
    )
    .bind(provider_other)
    .bind(&admin_id)
    .bind(&user_id)
    .execute(&ctx.pool)
    .await
    .expect("insert provider share");

    let routes = ctx
        .app
        .clone()
        .oneshot(
            authed("GET", "/api/user/routes", USER_SESSION, "")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("routes response");
    assert_eq!(routes.status(), StatusCode::OK);
    let routes = json_body(routes).await;
    let model = routes["models"]
        .as_array()
        .expect("models")
        .iter()
        .find(|model| model["canonical_id"] == "gpt-4o")
        .expect("gpt-4o model");
    let initially_shared = model["shared_keys"].as_array().expect("shared keys");
    assert!(
        initially_shared
            .iter()
            .all(|key| key["upstream_key_id"] != key_shared),
        "a provider-level grant must not expose keys from another provider"
    );
    assert!(
        initially_shared
            .iter()
            .any(|key| key["upstream_key_id"] == key_other),
        "the grant's own provider key must remain shared"
    );

    // Mutating a route preference on the unrelated personal key must 404 (F043).
    let denied = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "PUT",
                &format!("/api/user/routes/{key_shared}/gpt-4o"),
                USER_SESSION,
                USER_CSRF,
            )
            .body(Body::from(
                serde_json::json!({"priority_override": 200}).to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("denied preference response");
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);

    // A key-level accepted grant for `key_shared` makes exactly that key visible
    // and mutable by the grantee.
    sqlx::query(
        "INSERT INTO provider_shares (id, provider_id, upstream_key_id, owner_user_id,
            grantee_user_id, accepted_at, accepted_config_fingerprint, enabled, created_at)
         VALUES ('01ARZ3NDEKTSV4RRFFQ69G5PS2', ?, ?, ?, ?, 1000, 'fp-a', 1, 1000)",
    )
    .bind(provider_shared)
    .bind(key_shared)
    .bind(&admin_id)
    .bind(&user_id)
    .execute(&ctx.pool)
    .await
    .expect("insert key share");

    let routes = ctx
        .app
        .clone()
        .oneshot(
            authed("GET", "/api/user/routes", USER_SESSION, "")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("routes response");
    let routes = json_body(routes).await;
    let model = routes["models"]
        .as_array()
        .expect("models")
        .iter()
        .find(|model| model["canonical_id"] == "gpt-4o")
        .expect("gpt-4o model");
    let shared = model["shared_keys"].as_array().expect("shared keys");
    assert_eq!(shared.len(), 2);
    assert!(
        shared
            .iter()
            .any(|key| key["upstream_key_id"] == key_shared)
    );

    let allowed = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "PUT",
                &format!("/api/user/routes/{key_shared}/gpt-4o"),
                USER_SESSION,
                USER_CSRF,
            )
            .body(Body::from(
                serde_json::json!({"priority_override": 200}).to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("allowed preference response");
    assert_eq!(allowed.status(), StatusCode::OK);

    // The key covered by the provider-level grant remains mutable.
    let denied_other = ctx
        .app
        .clone()
        .oneshot(
            authed(
                "PUT",
                &format!("/api/user/routes/{key_other}/gpt-4o"),
                USER_SESSION,
                USER_CSRF,
            )
            .body(Body::from(
                serde_json::json!({"priority_override": 200}).to_string(),
            ))
            .expect("request"),
        )
        .await
        .expect("shared other response");
    assert_eq!(denied_other.status(), StatusCode::OK);
}
