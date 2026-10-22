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

use llmroller_admin::auth::{AuthAppState, AuthConfig, derive_session_csrf};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::eligibility::CandidatePoolTier;
use llmroller_core::ids::{ClientKeyId, ModelPk, ProviderId, UpstreamKeyId, UserId};
use llmroller_core::pins::{PinEntry, PinKey, PinState, PinStore};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy};
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
    alice_key: FixtureKey,
    bob_key: FixtureKey,
}

struct FixtureKey {
    client_key_id: ClientKeyId,
    upstream_key_id: UpstreamKeyId,
    cookie: String,
    csrf: String,
}

async fn insert_session(
    pool: &sqlx::SqlitePool,
    user_id: UserId,
    raw_token: &str,
) -> (String, String) {
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let id_hash = llmroller_store::crypto::sha256_hex(raw_token.as_bytes());
    let csrf = derive_session_csrf(raw_token);
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

    let (cookie, csrf) =
        insert_session(pool, owner, &format!("{label}-session-token-0123456789")).await;
    FixtureKey {
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
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
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

    let alice_key =
        insert_personal_key(&pool, alice.id, "alice", model_pk, "shared-session-hash").await;
    let bob_key = insert_personal_key(&pool, bob.id, "bob", model_pk, "shared-session-hash").await;

    Harness {
        _dir: dir,
        pool,
        master_key,
        admin: admin.id,
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
    let auth_state = AuthAppState::new(harness.pool.clone(), auth_config);
    let setup_state = SetupAppState {
        pool: harness.pool.clone(),
        expected_token: Some("f11-token".to_string()),
        data_dir: harness._dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
    };
    let mut api_state = ApiAppState::new(
        auth_state.clone(),
        harness.master_key.clone(),
        "key-0".to_string(),
    );
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
