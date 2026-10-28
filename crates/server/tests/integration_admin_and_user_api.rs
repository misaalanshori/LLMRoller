//! Control-plane tenancy, pin deletion, and audit context regressions.
//!
//! Spec: Section 4.2, Section 8.5, Section 8.6, Section 14.2
//! Requirements: SEC-17, USR-3, AUD-1, SEC-5

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig, derive_session_csrf_with_secret};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::eligibility::CandidatePoolTier;
use llmroller_core::ids::{ClientKeyId, ModelPk, ProviderId, UpstreamKeyId, UserId};
use llmroller_core::pins::{PinEntry, PinKey, PinState, PinStore};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, ExposeSecret as _, OsEntropy};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};

struct Harness {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    master_key: MasterKey,
    admin: UserId,
    session_hmac_secret: llmroller_core::seams::SecretBytes,
    alice_key: FixtureKey,
    bob_key: FixtureKey,
}

struct FixtureKey {
    owner: UserId,
    client_key_id: ClientKeyId,
    upstream_key_id: UpstreamKeyId,
    cookie: String,
    csrf: String,
}

async fn insert_session(
    pool: &sqlx::SqlitePool,
    user_id: UserId,
    raw_token: &str,
    session_hmac_secret: &[u8],
) -> (String, String) {
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let id_hash = llmroller_store::crypto::sha256_hex(raw_token.as_bytes());
    let csrf = derive_session_csrf_with_secret(session_hmac_secret, raw_token);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf.as_bytes());
    insert_web_session(
        pool,
        NewWebSession {
            id_hash: &id_hash,
            user_id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("f11-test"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("insert web session");
    (format!("llmroller_session={raw_token}"), csrf)
}

async fn insert_personal_key(
    pool: &sqlx::SqlitePool,
    owner: UserId,
    label: &str,
    model_pk: ModelPk,
    session_hash: &str,
    session_hmac_secret: &[u8],
) -> FixtureKey {
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let client_key_id = ClientKeyId::generate();
    let provider_id = ProviderId::generate();
    let upstream_key_id = UpstreamKeyId::generate();
    let client_key = client_key_id.to_string();

    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, prefix, key_hash, label, routing_scope, created_at)
         VALUES (?, ?, ?, ?, ?, 'personal_first', ?)",
    )
    .bind(&client_key)
    .bind(owner.to_string())
    .bind(format!("llr_{label}"))
    .bind(format!("hash-{client_key}"))
    .bind(label)
    .bind(now_ms)
    .execute(pool)
    .await
    .expect("insert client key");

    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, owner_user_id, created_at)
         VALUES (?, ?, 'custom', 'https://example.test', '{}', 'personal', ?, ?)",
    )
    .bind(provider_id.to_string())
    .bind(format!("{label}-provider"))
    .bind(owner.to_string())
    .bind(now_ms)
    .execute(pool)
    .await
    .expect("insert provider");

    sqlx::query(
        "INSERT INTO upstream_keys
         (id, provider_id, owner_user_id, label, secret_enc, master_key_id, secret_last4, created_at)
         VALUES (?, ?, ?, ?, X'00', 'key-0', '1234', ?)",
    )
    .bind(upstream_key_id.to_string())
    .bind(provider_id.to_string())
    .bind(owner.to_string())
    .bind(format!("{label}-upstream"))
    .bind(now_ms)
    .execute(pool)
    .await
    .expect("insert upstream key");

    sqlx::query(
        "INSERT INTO pins
         (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier, created_at, last_used_at, expires_at)
         VALUES (?, ?, ?, ?, 1, ?, ?, ?)",
    )
    .bind(&client_key)
    .bind(session_hash)
    .bind(model_pk.to_string())
    .bind(upstream_key_id.to_string())
    .bind(now_ms)
    .bind(now_ms)
    .bind(now_ms + 3_600_000)
    .execute(pool)
    .await
    .expect("insert pin");

    let (cookie, csrf) = insert_session(
        pool,
        owner,
        &format!("{label}-session-token-0123456789"),
        session_hmac_secret,
    )
    .await;
    FixtureKey {
        owner,
        client_key_id,
        upstream_key_id,
        cookie,
        csrf,
    }
}

async fn harness() -> Harness {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("admin_api.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let master_key = MasterKey::from_bytes([0xC7; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance load");

    let entropy = OsEntropy;
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let alice = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "alice_f11",
            password: "supersecretalicepass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("alice");
    let bob = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "bob_f11",
            password: "supersecretbobpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("bob");
    let admin = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "admin_f11",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("admin");

    let model_pk = ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id) VALUES (?, 'gpt-4o')")
        .bind(model_pk.to_string())
        .execute(&pool)
        .await
        .expect("insert model");

    let alice_key = insert_personal_key(
        &pool,
        alice.id,
        "alice",
        model_pk,
        "shared-session-hash",
        instance_state.session_hmac_secret.expose_secret(),
    )
    .await;
    let bob_key = insert_personal_key(
        &pool,
        bob.id,
        "bob",
        model_pk,
        "shared-session-hash",
        instance_state.session_hmac_secret.expose_secret(),
    )
    .await;

    Harness {
        _dir: dir,
        pool,
        master_key,
        admin: admin.id,
        session_hmac_secret: instance_state.session_hmac_secret.clone(),
        alice_key,
        bob_key,
    }
}

fn build_app(harness: &Harness, pin_store: Arc<PinStore>) -> axum::Router {
    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(harness.pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(harness.session_hmac_secret.clone());
    let setup_state = SetupAppState {
        pool: harness.pool.clone(),
        expected_token: Some("f11-token".to_string()),
        data_dir: harness._dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let mut api_state = ApiAppState::new(
        auth_state.clone(),
        harness.master_key.clone(),
        "key-0".to_string(),
    );
    // An instance audit key, as production supplies (`serve.rs:443`). Without it the mutation path
    // now deliberately writes **no** audit row rather than deriving one from the master key
    // (`ISSUE-0138`, `ADR-11`), so a fixture that asserts an audit row must configure the key it
    // claims to be testing.
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x24; 32]));
    api_state.attach_pin_store(pin_store);
    control_plane_router_full(setup_state, auth_state, api_state)
}

fn pin_key(client_key_id: ClientKeyId) -> PinKey {
    PinKey {
        client_key_id,
        canonical_id: "gpt-4o".to_string(),
        session_hash: "shared-session-hash".to_string(),
    }
}

fn hydrated_entry(upstream_key_id: UpstreamKeyId) -> PinEntry {
    PinEntry {
        state: PinState::Confirmed,
        upstream_key_id,
        generation: 1,
        pool_tier: CandidatePoolTier::Own,
        created_ms: 1,
        last_used_ms: 1,
        expires_ms: i64::MAX,
        repin_count: 0,
        tried_this_generation: Vec::new(),
    }
}

fn delete_request(uri: String, cookie: &str, csrf: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(header::COOKIE, cookie)
        .header("x-csrf-token", csrf)
        .body(Body::empty())
        .expect("delete request")
}

#[doc = "Spec: SEC-17, USR-3 — F11 user pin deletion is tenant-scoped and evicts only the owner's live pin"]
#[tokio::test]
async fn test_standard_user_cannot_delete_another_users_session_pin() {
    let harness = harness().await;
    let pin_store = Arc::new(PinStore::new(1_000));
    let alice_pin = pin_key(harness.alice_key.client_key_id);
    let bob_pin = pin_key(harness.bob_key.client_key_id);
    pin_store.insert_hydrated(
        alice_pin.clone(),
        hydrated_entry(harness.alice_key.upstream_key_id),
    );
    pin_store.insert_hydrated(
        bob_pin.clone(),
        hydrated_entry(harness.bob_key.upstream_key_id),
    );
    let app = build_app(&harness, pin_store.clone());

    let bob_delete_own = delete_request(
        format!(
            "/api/user/pins/{}/shared-session-hash/gpt-4o",
            harness.bob_key.client_key_id
        ),
        &harness.bob_key.cookie,
        &harness.bob_key.csrf,
    );
    let response = app
        .clone()
        .oneshot(bob_delete_own)
        .await
        .expect("delete own pin");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let bob_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pins WHERE client_key_id = ?")
        .bind(harness.bob_key.client_key_id.to_string())
        .fetch_one(&harness.pool)
        .await
        .expect("bob rows");
    assert_eq!(bob_rows, 0, "the owner's pin must be deleted");
    let alice_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pins WHERE client_key_id = ?")
        .bind(harness.alice_key.client_key_id.to_string())
        .fetch_one(&harness.pool)
        .await
        .expect("alice rows");
    assert_eq!(alice_rows, 1, "another tenant's pin must survive");
    assert_eq!(
        pin_store.len(),
        1,
        "only the matching in-memory pin is evicted"
    );
    assert!(
        pin_store.remove(&alice_pin),
        "Alice's live pin must still be present"
    );
    pin_store.insert_hydrated(
        alice_pin.clone(),
        hydrated_entry(harness.alice_key.upstream_key_id),
    );

    let bob_delete_alice = delete_request(
        format!(
            "/api/user/pins/{}/shared-session-hash/gpt-4o",
            harness.alice_key.client_key_id
        ),
        &harness.bob_key.cookie,
        &harness.bob_key.csrf,
    );
    let response = app
        .clone()
        .oneshot(bob_delete_alice)
        .await
        .expect("cross-tenant delete attempt");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "USR-3 requires a foreign client key to be indistinguishable from a missing one"
    );
    let alice_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pins WHERE client_key_id = ?")
        .bind(harness.alice_key.client_key_id.to_string())
        .fetch_one(&harness.pool)
        .await
        .expect("alice rows after attack");
    assert_eq!(alice_rows, 1, "Bob must not delete Alice's pin");
    assert_eq!(pin_store.len(), 1);

    let bob_admin_route = delete_request(
        "/admin/api/sessions/shared-session-hash/pin".to_string(),
        &harness.bob_key.cookie,
        &harness.bob_key.csrf,
    );
    let response = app
        .clone()
        .oneshot(bob_admin_route)
        .await
        .expect("admin deletion attempt");
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "standard users must not use the admin-wide session pin deletion route"
    );
    let alice_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pins WHERE client_key_id = ?")
        .bind(harness.alice_key.client_key_id.to_string())
        .fetch_one(&harness.pool)
        .await
        .expect("alice rows after admin route attack");
    assert_eq!(alice_rows, 1);

    let admin_delete = delete_request(
        "/admin/api/sessions/shared-session-hash/pin".to_string(),
        &harness.alice_key.cookie,
        &harness.alice_key.csrf,
    );
    let response = app
        .clone()
        .oneshot(admin_delete)
        .await
        .expect("non-admin active session deletion attempt");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let admin_session = insert_session(
        &harness.pool,
        harness.admin,
        "admin-f11-session-token-0123456789",
        harness.session_hmac_secret.expose_secret(),
    )
    .await;
    let admin_delete = delete_request(
        "/admin/api/sessions/shared-session-hash/pin".to_string(),
        &admin_session.0,
        &admin_session.1,
    );
    let response = app.oneshot(admin_delete).await.expect("admin delete");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pins")
        .fetch_one(&harness.pool)
        .await
        .expect("all rows");
    assert_eq!(
        rows, 0,
        "admin session deletion clears every tenant's pin for that session hash"
    );
    assert!(
        pin_store.is_empty(),
        "admin deletion evicts matching in-memory pins"
    );
}

#[doc = "Spec: AUD-1, SEC-5 — F11 mutations record the authenticated actor, client IP, and semantic target"]
#[tokio::test]
async fn test_audit_log_captures_authenticated_actor_user_id_on_mutations() {
    let harness = harness().await;
    let pin_store = Arc::new(PinStore::new(1_000));
    let app = build_app(&harness, pin_store);
    let admin_session = insert_session(
        &harness.pool,
        harness.admin,
        "admin-audit-session-token-0123456789",
        harness.session_hmac_secret.expose_secret(),
    )
    .await;

    let request = Request::builder()
        .method("PATCH")
        .uri("/admin/api/settings")
        .header(header::COOKIE, &admin_session.0)
        .header("x-csrf-token", &admin_session.1)
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 54_321))))
        .body(Body::from(r#"{"pin_ttl_secs":86400}"#))
        .expect("settings request");
    let response = app.oneshot(request).await.expect("settings response");
    assert_eq!(response.status(), StatusCode::OK);

    use sqlx::Row as _;
    let row = sqlx::query(
        "SELECT actor_user_id, ip_addr, action, target_type, target_id, detail_json
         FROM audit_log ORDER BY seq DESC LIMIT 1",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("audit row");
    assert_eq!(
        row.try_get::<Option<String>, _>("actor_user_id")
            .expect("actor"),
        Some(harness.admin.to_string()),
        "the authenticated admin must be recorded as the actor"
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("ip_addr").expect("ip"),
        Some("127.0.0.1".to_string()),
        "the client IP must be recorded from ConnectInfo"
    );
    assert_eq!(
        row.try_get::<String, _>("action").expect("action"),
        "admin.setting.updated",
        "the audit action must be semantic, not a generic control.mutation marker"
    );
    assert_eq!(
        row.try_get::<String, _>("target_type")
            .expect("target type"),
        "setting"
    );
    let detail = row
        .try_get::<Option<String>, _>("detail_json")
        .expect("detail");
    assert!(
        detail
            .as_deref()
            .unwrap_or_default()
            .contains("/admin/api/settings"),
        "audit detail must retain the request path"
    );
}

#[doc = "Spec: CLK-4 — the client-key sessions drawer reports live session pins, not a stub"]
#[tokio::test]
async fn test_client_key_sessions_report_live_session_pins() {
    let harness = harness().await;
    let app = build_app(&harness, Arc::new(PinStore::new(1_000)));

    // Pin-only session: no trace_records row exists, so the listing must still
    // surface the session (union of pins and trace records) with its live pin.
    let request = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/user/client-keys/{}/sessions",
            harness.alice_key.client_key_id
        ))
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("sessions request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("sessions response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    let sessions = body["data"].as_array().expect("data array");
    assert_eq!(sessions.len(), 1, "the pinned session must be listed");
    assert_eq!(sessions[0]["session_hash"], "shared-session-hash");
    let pins = sessions[0]["current_pins"]
        .as_array()
        .expect("current_pins array");
    assert_eq!(
        pins.len(),
        1,
        "the hardcoded empty current_pins stub must be gone"
    );
    assert_eq!(
        pins[0]["upstream_key_id"],
        harness.alice_key.upstream_key_id.to_string()
    );
    assert_eq!(pins[0]["canonical_id"], "gpt-4o");
    assert_eq!(pins[0]["key_label"], "alice-upstream");
    assert_eq!(pins[0]["pool_tier"], 1);

    // Create a global provider and upstream key (USR-4, SEC-17)
    let global_prov_id = ProviderId::generate();
    let global_key_id = UpstreamKeyId::generate();
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let model_pk: String =
        sqlx::query_scalar("SELECT model_pk FROM models WHERE canonical_id = 'gpt-4o'")
            .fetch_one(&harness.pool)
            .await
            .expect("model_pk");

    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, created_at)
         VALUES (?, 'global-provider', 'custom', 'https://global.test', '{}', 'global', ?)",
    )
    .bind(global_prov_id.to_string())
    .bind(now_ms)
    .execute(&harness.pool)
    .await
    .expect("insert global provider");

    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, label, secret_enc, master_key_id, secret_last4, created_at)
         VALUES (?, ?, 'global-secret-key', X'00', 'key-0', '1234', ?)",
    )
    .bind(global_key_id.to_string())
    .bind(global_prov_id.to_string())
    .bind(now_ms)
    .execute(&harness.pool)
    .await
    .expect("insert global upstream key");

    // Pin a session to the global key
    sqlx::query(
        "INSERT INTO pins (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier, expires_at, created_at, last_used_at)
         VALUES (?, 'global-session-hash', ?, ?, 3, ?, ?, ?)",
    )
    .bind(harness.alice_key.client_key_id.to_string())
    .bind(&model_pk)
    .bind(global_key_id.to_string())
    .bind(now_ms + 3_600_000)
    .bind(now_ms)
    .bind(now_ms)
    .execute(&harness.pool)
    .await
    .expect("insert global pin");

    // Standard user queries sessions: global key ID must be null and label must be "global pool" (USR-4, SEC-17)
    let user_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/user/client-keys/{}/sessions",
            harness.alice_key.client_key_id
        ))
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("user sessions request");
    let user_res = app
        .clone()
        .oneshot(user_req)
        .await
        .expect("user sessions response");
    assert_eq!(user_res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(user_res.into_body(), 64 * 1024)
        .await
        .expect("body");
    let user_body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    let user_sessions = user_body["data"].as_array().expect("user sessions array");
    let global_sess = user_sessions
        .iter()
        .find(|s| s["session_hash"] == "global-session-hash")
        .expect("global session present");
    let global_pins = global_sess["current_pins"].as_array().expect("global pins");
    assert!(
        global_pins[0]["upstream_key_id"].is_null(),
        "global key id must be redacted to null for standard users"
    );
    assert_eq!(
        global_pins[0]["key_label"], "global pool",
        "global key label must be masked to \"global pool\""
    );

    // Admin queries sessions: receives full unredacted key details
    let (admin_cookie, _admin_csrf) = insert_session(
        &harness.pool,
        harness.admin,
        "admin_sessions_token_12345",
        harness.session_hmac_secret.expose_secret(),
    )
    .await;
    let admin_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/admin/api/client-keys/{}/sessions",
            harness.alice_key.client_key_id
        ))
        .header(header::COOKIE, &admin_cookie)
        .body(Body::empty())
        .expect("admin sessions request");
    let admin_res = app
        .oneshot(admin_req)
        .await
        .expect("admin sessions response");
    assert_eq!(admin_res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(admin_res.into_body(), 64 * 1024)
        .await
        .expect("body");
    let admin_body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    let admin_sessions = admin_body["data"].as_array().expect("admin sessions array");
    let admin_global_sess = admin_sessions
        .iter()
        .find(|s| s["session_hash"] == "global-session-hash")
        .expect("global session present");
    let admin_global_pins = admin_global_sess["current_pins"]
        .as_array()
        .expect("admin global pins");
    assert_eq!(
        admin_global_pins[0]["upstream_key_id"],
        global_key_id.to_string()
    );
    assert_eq!(admin_global_pins[0]["key_label"], "global-secret-key");
}

#[doc = "Spec: LIM-1 — budget window CRUD endpoints with validation and RBAC scoping"]
#[tokio::test]
async fn test_budget_window_crud_endpoints_and_validation() {
    let harness = harness().await;
    let app = build_app(&harness, Arc::new(PinStore::new(1_000)));

    // 1. Alice creates a rolling budget window on her own upstream key
    let create_req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/user/upstream-keys/{}/windows",
            harness.alice_key.upstream_key_id
        ))
        .header(header::COOKIE, &harness.alice_key.cookie)
        .header("x-csrf-token", &harness.alice_key.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{
                "scope_kind": "upstream_key",
                "window_kind": "rolling",
                "period_ms": 86400000,
                "limit_micro_usd": 5000000,
                "reserve_pct": 5
            }"#,
        ))
        .expect("create req");
    let resp = app.clone().oneshot(create_req).await.expect("create resp");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("bytes");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let window_id = body["id"].as_str().expect("window id").to_string();

    // 2. Alice lists windows for her key
    let list_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/user/upstream-keys/{}/windows",
            harness.alice_key.upstream_key_id
        ))
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("list req");
    let resp = app.clone().oneshot(list_req).await.expect("list resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("bytes");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let windows = body["data"].as_array().expect("data array");
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["id"], window_id);
    assert_eq!(windows[0]["limit_micro_usd"], 5000000);
    assert_eq!(windows[0]["reserve_pct"], 5);

    // 3. Alice patches the window
    let patch_req = Request::builder()
        .method("PATCH")
        .uri(format!("/api/user/budget-windows/{window_id}"))
        .header(header::COOKIE, &harness.alice_key.cookie)
        .header("x-csrf-token", &harness.alice_key.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"limit_micro_usd": 10000000}"#))
        .expect("patch req");
    let resp = app.clone().oneshot(patch_req).await.expect("patch resp");
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Bob tries to access Alice's key's windows -> 404 (no existence oracle)
    let bob_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/user/upstream-keys/{}/windows",
            harness.alice_key.upstream_key_id
        ))
        .header(header::COOKIE, &harness.bob_key.cookie)
        .body(Body::empty())
        .expect("bob req");
    let resp = app.clone().oneshot(bob_req).await.expect("bob resp");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 5. Validation failure: negative limit -> 422
    let invalid_req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/user/upstream-keys/{}/windows",
            harness.alice_key.upstream_key_id
        ))
        .header(header::COOKIE, &harness.alice_key.cookie)
        .header("x-csrf-token", &harness.alice_key.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{
                "scope_kind": "upstream_key",
                "window_kind": "rolling",
                "period_ms": 86400000,
                "limit_micro_usd": -100
            }"#,
        ))
        .expect("invalid req");
    let resp = app
        .clone()
        .oneshot(invalid_req)
        .await
        .expect("invalid resp");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // 5b. Alice adds a manual calibration adjustment to the window (F-064, LIM-1)
    let adj_req = Request::builder()
        .method("POST")
        .uri(format!("/api/user/budget-windows/{window_id}/adjustments"))
        .header(header::COOKIE, &harness.alice_key.cookie)
        .header("x-csrf-token", &harness.alice_key.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{
                "delta_micro_usd": 500000,
                "note": "calibration delta"
            }"#,
        ))
        .expect("adj req");
    let resp = app.clone().oneshot(adj_req).await.expect("adj resp");
    assert_eq!(resp.status(), StatusCode::OK);

    let get_adj_req = Request::builder()
        .method("GET")
        .uri(format!("/api/user/budget-windows/{window_id}/adjustments"))
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("get adj req");
    let resp = app
        .clone()
        .oneshot(get_adj_req)
        .await
        .expect("get adj resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("adj body");
    let adj_json: serde_json::Value = serde_json::from_slice(&bytes).expect("adj json");
    assert_eq!(adj_json["adjustments"].as_array().expect("array").len(), 1);
    assert_eq!(adj_json["adjustments"][0]["delta_micro_usd"], 500000);

    // 6. Alice deletes the window -> 204
    let del_req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/user/budget-windows/{window_id}"))
        .header(header::COOKIE, &harness.alice_key.cookie)
        .header("x-csrf-token", &harness.alice_key.csrf)
        .body(Body::empty())
        .expect("del req");
    let resp = app.clone().oneshot(del_req).await.expect("del resp");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[doc = "Spec: USG-1, USG-2 — usage timeseries and breakdown endpoints aggregate from rollups"]
#[tokio::test]
async fn test_usage_timeseries_and_breakdown_rollups() {
    let harness = harness().await;
    let app = build_app(&harness, Arc::new(PinStore::new(1_000)));

    // Insert hourly rollups for Alice
    sqlx::query(
        "INSERT INTO usage_rollups_hourly (hour_ms, owner_user_id, client_key_id, upstream_key_id, provider_id, provider_scope, share_id, canonical_id, requests, errors, input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens, reasoning_tokens, cost_micro_usd, list_cost_micro_usd)
         VALUES (3600000, ?, 'ck1', 'uk1', 'p1', 'personal', '', 'gpt-4o', 10, 0, 1000, 200, 0, 500, 0, 15000, 15000),
                (7200000, ?, 'ck1', 'uk1', 'p1', 'personal', '', 'claude-3-5-sonnet', 5, 0, 2000, 0, 0, 800, 0, 25000, 25000)",
    )
    .bind(harness.alice_key.client_key_id.to_string())
    .bind(harness.alice_key.client_key_id.to_string())
    .execute(&harness.pool)
    .await
    .expect("insert rollups");

    // Query timeseries as Alice
    let ts_req = Request::builder()
        .method("GET")
        .uri("/api/user/usage/timeseries")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("ts req");
    let resp = app.clone().oneshot(ts_req).await.expect("ts resp");
    assert_eq!(resp.status(), StatusCode::OK);

    // Query breakdown as Alice
    let bd_req = Request::builder()
        .method("GET")
        .uri("/api/user/usage/breakdown")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("bd req");
    let resp = app.clone().oneshot(bd_req).await.expect("bd resp");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[doc = "Spec: AUTH-4, AUTH-5 — must_change_password flag restricts session to password change"]
#[tokio::test]
async fn test_must_change_password_blocks_resources_until_password_changed() {
    let harness = harness().await;
    let app = build_app(&harness, Arc::new(PinStore::new(1_000)));

    // 1. Mark Alice as must_change_password = 1
    sqlx::query("UPDATE users SET must_change_password = 1 WHERE id = ?")
        .bind(harness.alice_key.owner.to_string())
        .execute(&harness.pool)
        .await
        .expect("update must_change_password");

    // 2. Querying resources returns 403 must_change_password
    let get_req = Request::builder()
        .method("GET")
        .uri("/api/user/client-keys")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("get req");
    let resp = app.clone().oneshot(get_req).await.expect("get resp");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("b");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(json["error"]["code"], "must_change_password");

    // 3. Changing password clears must_change_password
    sqlx::query("UPDATE users SET must_change_password = 0 WHERE id = ?")
        .bind(harness.alice_key.owner.to_string())
        .execute(&harness.pool)
        .await
        .expect("clear must_change_password");

    // 4. Resource access succeeds now
    let retry_req = Request::builder()
        .method("GET")
        .uri("/api/user/client-keys")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("retry req");
    let retry_resp = app.oneshot(retry_req).await.expect("retry resp");
    assert_eq!(retry_resp.status(), StatusCode::OK);
}

#[doc = "Spec: PRV-6, SEC-7 — cross-origin usage endpoints require explicit acknowledgement"]
#[tokio::test]
async fn test_provider_usage_endpoint_cross_origin_validation() {
    let harness = harness().await;
    let app = build_app(&harness, Arc::new(PinStore::new(1_000)));

    // 1. Cross-origin usage endpoint without ACK returns 422
    let cross_req = Request::builder()
        .method("POST")
        .uri("/api/user/providers")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .header("x-csrf-token", &harness.alice_key.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{
                "name": "cross-test",
                "kind": "custom",
                "base_url": "https://api.openai.com/v1",
                "config_json": "{\"paths\":{\"usage\":\"https://billing.otherdomain.com/v1/usage\"}}",
                "ownership_scope": "personal",
                "usage_url_cross_origin_ack": false
            }"#,
        ))
        .expect("req");
    let resp = app.clone().oneshot(cross_req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // 2. Cross-origin usage endpoint WITH ACK succeeds (201 Created)
    let ack_req = Request::builder()
        .method("POST")
        .uri("/api/user/providers")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .header("x-csrf-token", &harness.alice_key.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{
                "name": "cross-test-acked",
                "kind": "custom",
                "base_url": "https://api.openai.com/v1",
                "config_json": "{\"paths\":{\"usage\":\"https://billing.otherdomain.com/v1/usage\"}}",
                "ownership_scope": "personal",
                "usage_url_cross_origin_ack": true
            }"#,
        ))
        .expect("req");
    let resp = app.oneshot(ack_req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[doc = "Spec: RTE-10, F039 - Expired pins are filtered out by WHERE expires_at > now_ms in /api/user/pins and /app/partials/pins"]
#[tokio::test]
async fn regression_test_f039_expired_pins_excluded_from_api_and_ui() {
    let harness = harness().await;
    let app = build_app(&harness, Arc::new(PinStore::new(1_000)));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    let model_pk: String =
        sqlx::query_scalar("SELECT model_pk FROM models WHERE canonical_id = 'gpt-4o'")
            .fetch_one(&harness.pool)
            .await
            .expect("model_pk");

    // Insert an expired pin
    sqlx::query(
        "INSERT INTO pins (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier, created_at, last_used_at, expires_at)
         VALUES (?, 'expired-sess-hash', ?, ?, 1, ?, ?, ?)",
    )
    .bind(harness.alice_key.client_key_id.to_string())
    .bind(&model_pk)
    .bind(harness.alice_key.upstream_key_id.to_string())
    .bind(now_ms - 200_000)
    .bind(now_ms - 200_000)
    .bind(now_ms - 100_000) // expired!
    .execute(&harness.pool)
    .await
    .expect("insert expired pin");

    // Alice queries /api/user/pins (JSON)
    let req = Request::builder()
        .method("GET")
        .uri("/api/user/pins")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("body");
    let list: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("json");
    // Only the unexpired pin ("shared-session-hash") should be present, NOT "expired-sess-hash"
    assert!(
        list.iter()
            .any(|p| p["session_hash"] == "shared-session-hash")
    );
    assert!(
        !list
            .iter()
            .any(|p| p["session_hash"] == "expired-sess-hash"),
        "expired pin must be excluded"
    );

    // Alice queries /app/partials/pins (HTML partial)
    let partial_req = Request::builder()
        .method("GET")
        .uri("/app/partials/pins")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("req");
    let partial_resp = app.oneshot(partial_req).await.expect("resp");
    assert_eq!(partial_resp.status(), StatusCode::OK);
    let p_bytes = axum::body::to_bytes(partial_resp.into_body(), 64 * 1024)
        .await
        .expect("body");
    let p_html = String::from_utf8(p_bytes.to_vec()).expect("html");
    assert!(
        !p_html.contains("expired-sess-hash"),
        "expired pin must not appear in HTML partial"
    );
}

#[doc = "Spec: USR-4, SEC-17, F082 - /api/user/pins and /app/partials/pins mask global upstream key labels to 'global pool' for standard users"]
#[tokio::test]
async fn regression_test_f082_user_portal_session_pins_endpoint_masks_global_upstream_key_labels() {
    let harness = harness().await;
    let app = build_app(&harness, Arc::new(PinStore::new(1_000)));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    let global_prov_id = ProviderId::generate();
    let global_key_id = UpstreamKeyId::generate();
    let model_pk: String =
        sqlx::query_scalar("SELECT model_pk FROM models WHERE canonical_id = 'gpt-4o'")
            .fetch_one(&harness.pool)
            .await
            .expect("model_pk");

    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, created_at)
         VALUES (?, 'global-provider-f082', 'custom', 'https://global.test', '{}', 'global', ?)",
    )
    .bind(global_prov_id.to_string())
    .bind(now_ms)
    .execute(&harness.pool)
    .await
    .expect("insert global provider");

    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, label, secret_enc, master_key_id, secret_last4, created_at)
         VALUES (?, ?, 'admin-confidential-global-key-label', X'00', 'key-0', '9876', ?)",
    )
    .bind(global_key_id.to_string())
    .bind(global_prov_id.to_string())
    .bind(now_ms)
    .execute(&harness.pool)
    .await
    .expect("insert global key with sensitive label");

    // Pin a session to this global key for Alice (pool_tier = 3)
    sqlx::query(
        "INSERT INTO pins (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier, created_at, last_used_at, expires_at)
         VALUES (?, 'global-pin-session-hash-1234', ?, ?, 3, ?, ?, ?)",
    )
    .bind(harness.alice_key.client_key_id.to_string())
    .bind(&model_pk)
    .bind(global_key_id.to_string())
    .bind(now_ms)
    .bind(now_ms)
    .bind(now_ms + 3_600_000)
    .execute(&harness.pool)
    .await
    .expect("insert pin to global key");

    // Standard user Alice calls /api/user/pins (JSON)
    let user_req = Request::builder()
        .method("GET")
        .uri("/api/user/pins")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("req");
    let user_resp = app.clone().oneshot(user_req).await.expect("resp");
    assert_eq!(user_resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(user_resp.into_body(), 64 * 1024)
        .await
        .expect("body");
    let list: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("json");
    let pin_item = list
        .iter()
        .find(|p| p["session_hash"] == "global-pin-session-hash-1234")
        .expect("pin must be present");
    assert_eq!(
        pin_item["key_label"], "global pool",
        "global key label must be masked to 'global pool' for standard users"
    );
    assert!(
        !pin_item["key_label"]
            .as_str()
            .expect("key_label string")
            .contains("admin-confidential"),
        "must never leak admin upstream key label"
    );

    // Standard user Alice calls /app/partials/pins (HTML)
    let partial_req = Request::builder()
        .method("GET")
        .uri("/app/partials/pins")
        .header(header::COOKIE, &harness.alice_key.cookie)
        .body(Body::empty())
        .expect("req");
    let partial_resp = app.clone().oneshot(partial_req).await.expect("resp");
    assert_eq!(partial_resp.status(), StatusCode::OK);
    let p_bytes = axum::body::to_bytes(partial_resp.into_body(), 64 * 1024)
        .await
        .expect("body");
    let p_html = String::from_utf8(p_bytes.to_vec()).expect("html");
    assert!(
        p_html.contains("global pool"),
        "HTML partial must display 'global pool'"
    );
    assert!(
        !p_html.contains("admin-confidential"),
        "HTML partial must never leak admin upstream key label"
    );

    // Admin user calls /admin/api/pins (JSON): receives real label
    let (admin_cookie, _admin_csrf) = insert_session(
        &harness.pool,
        harness.admin,
        "admin_pins_token_f082",
        harness.session_hmac_secret.expose_secret(),
    )
    .await;
    let admin_req = Request::builder()
        .method("GET")
        .uri("/admin/api/pins")
        .header(header::COOKIE, &admin_cookie)
        .body(Body::empty())
        .expect("req");
    let admin_resp = app.oneshot(admin_req).await.expect("resp");
    assert_eq!(admin_resp.status(), StatusCode::OK);
    let a_bytes = axum::body::to_bytes(admin_resp.into_body(), 64 * 1024)
        .await
        .expect("body");
    let a_list: Vec<serde_json::Value> = serde_json::from_slice(&a_bytes).expect("json");
    let admin_pin = a_list
        .iter()
        .find(|p| p["session_hash"] == "global-pin-session-hash-1234")
        .expect("admin pin present");
    assert_eq!(
        admin_pin["key_label"], "admin-confidential-global-key-label",
        "admin receives real label"
    );
}
