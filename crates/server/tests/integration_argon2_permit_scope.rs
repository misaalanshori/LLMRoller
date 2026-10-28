//! Argon2id permit scope and role validation on the first-run setup and admin user paths.
//!
//! Requirements: AUTH-1, AUTH-4, AUTH-6, SEC-4, USR-5.
//! Issues: #180 (hash must not run under SQLite's write lock), #181 (roles must not be
//! silently coerced; the permit must bound the hash and not the database work).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, DeterministicRng, EntropySource, OsEntropy};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewWebSession, create_user, insert_web_session};

/// Generous ceiling for any single request. Setup's own permit bound is 5 s, so reaching
/// this is a genuine hang rather than a slow machine.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const TEST_PASSWORD: &str = "Password12345!#";
const SETUP_TOKEN: &str = "setup-token-for-permit-scope-tests";

struct Harness {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    master_key: MasterKey,
    /// Absolute path of the database, so a probe can open an independent connection.
    db_path: std::path::PathBuf,
}

async fn harness() -> Harness {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("permit_scope.db");
    let pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    seed_defaults(&pool, None, 1).await.expect("seed settings");
    let master_key = MasterKey::from_bytes([0x3B; 32]);
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance load");
    Harness {
        _dir: dir,
        pool,
        master_key,
        db_path,
    }
}

fn setup_state_for(harness: &Harness, token: Option<&str>) -> SetupAppState {
    SetupAppState {
        pool: harness.pool.clone(),
        expected_token: token.map(str::to_owned),
        data_dir: harness._dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    }
}

fn auth_state_for(harness: &Harness) -> AuthAppState {
    AuthAppState::new(
        harness.pool.clone(),
        AuthConfig {
            dev_insecure_cookies: true,
            require_tls: false,
            trusted_proxies: vec![],
        },
    )
}

/// One `POST /admin/api/setup` against the real router.
async fn attempt_setup(
    app: &axum::Router,
    username: &str,
    password: &str,
    token: Option<&str>,
) -> StatusCode {
    let mut payload = serde_json::json!({ "username": username, "password": password });
    if let Some(token) = token {
        payload["setup_token"] = serde_json::Value::String(token.to_owned());
    }
    let request = Request::builder()
        .method("POST")
        .uri("/admin/api/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .expect("valid request");
    let response = app.clone().oneshot(request).await.expect("router call");
    let status = response.status();
    let _ = axum::body::to_bytes(response.into_body(), 64 * 1024).await;
    status
}

// ---------------------------------------------------------------------------
// #180 — the write lock must not be held across Argon2id
// ---------------------------------------------------------------------------

/// Entropy source that pauses the first `fill_bytes` call until released.
///
/// The first draw inside `post_setup` is Argon2id's salt, so "paused" means "the hash is
/// about to run". That lets a test observe *what the lock state is at hash time* rather
/// than trying to measure how long a hash took — the timing assertion this project has
/// been burned by before. Opened once, then transparent.
struct GatedEntropy {
    inner: DeterministicRng,
    calls: std::sync::atomic::AtomicUsize,
    at_hash: Arc<tokio::sync::Notify>,
    release: Arc<std::sync::atomic::AtomicBool>,
}

impl EntropySource for GatedEntropy {
    fn fill_bytes(&self, dest: &mut [u8]) {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            self.at_hash.notify_one();
            let deadline = std::time::Instant::now() + REQUEST_TIMEOUT;
            while std::time::Instant::now() < deadline {
                if self.release.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        self.inner.fill_bytes(dest);
    }

    fn next_u64_bounded(&self, bound: u64) -> u64 {
        self.inner.next_u64_bounded(bound)
    }
}

/// Setup must hash **before** it takes SQLite's write lock (`ISSUE-0180`, `AUTH-1`).
///
/// The assertion is a **lock-ordering** check, not a timing check. The entropy source
/// pauses setup at the instant its Argon2id hash begins; while setup is paused there, the
/// test asks an independent connection to take SQLite's write lock.
///
/// - Hash-first (correct): setup holds no lock at that instant, so the probe acquires the
///   write lock.
/// - Hash-inside-`BEGIN IMMEDIATE` (the defect): setup already holds the write lock, so
///   the probe cannot have it.
///
/// The probe must be a *separate connection to the same file*. The server's writer pool is
/// capped at one connection, so probing through it would measure sqlx pool exhaustion
/// rather than SQLite lock contention. The probe's bound covers lock acquisition only,
/// never hash duration, so a slow machine cannot make this flaky.
#[tokio::test]
#[doc = "Spec: AUTH-1"]
async fn setup_does_not_hold_the_write_lock_across_hashing() {
    let harness = harness().await;
    let at_hash = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut setup_state = setup_state_for(&harness, Some(SETUP_TOKEN));
    setup_state.entropy = Arc::new(GatedEntropy {
        inner: DeterministicRng::new(0x7777),
        calls: std::sync::atomic::AtomicUsize::new(0),
        at_hash: at_hash.clone(),
        release: release.clone(),
    });
    let app = llmroller_admin::control_plane_router(setup_state, auth_state_for(&harness));

    let setup = tokio::spawn(async move {
        attempt_setup(&app, "first_admin", TEST_PASSWORD, Some(SETUP_TOKEN)).await
    });

    tokio::time::timeout(REQUEST_TIMEOUT, at_hash.notified())
        .await
        .expect("setup must reach its Argon2id hash");

    // Setup is paused at the hash. Who owns SQLite's write lock?
    let probe_owned_the_lock = {
        use sqlx::Connection as _;
        let mut probe =
            sqlx::SqliteConnection::connect(&format!("sqlite://{}", harness.db_path.display()))
                .await
                .expect("independent probe connection");
        match tokio::time::timeout(
            std::time::Duration::from_millis(2_000),
            probe.begin_with("BEGIN IMMEDIATE"),
        )
        .await
        {
            Ok(Ok(tx)) => {
                let _ = tx.rollback().await;
                true
            }
            _ => false,
        }
    };

    release.store(true, std::sync::atomic::Ordering::SeqCst);
    let status = tokio::time::timeout(REQUEST_TIMEOUT, setup)
        .await
        .expect("setup must complete")
        .expect("setup task must not panic");
    assert_eq!(status, StatusCode::CREATED, "setup must still succeed");

    assert!(
        probe_owned_the_lock,
        "a competing writer could not take the write lock while setup was hashing, which \
         means setup held BEGIN IMMEDIATE across Argon2id; hash before opening the \
         transaction (`ISSUE-0180`)"
    );

    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&harness.pool)
        .await
        .expect("count users");
    assert_eq!(users, 1, "exactly one admin must exist");
}

/// Setup must bound concurrent Argon2id work with the shared semaphore (`ISSUE-0180`).
///
/// Pre-fix `SetupAppState` has no semaphore, so a saturated pool is irrelevant and the
/// request proceeds. Post-fix it refuses with `503` inside the permit bound, and succeeds
/// once capacity returns — so the test asserts both halves.
#[tokio::test]
#[doc = "Spec: AUTH-1, Spec: AUTH-6"]
async fn setup_takes_the_argon2_semaphore() {
    let harness = harness().await;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let mut setup_state = setup_state_for(&harness, Some(SETUP_TOKEN));
    setup_state.argon2_semaphore = semaphore.clone();
    let app = llmroller_admin::control_plane_router(setup_state, auth_state_for(&harness));

    let held = semaphore
        .clone()
        .acquire_owned()
        .await
        .expect("hold permit");

    let status = tokio::time::timeout(
        REQUEST_TIMEOUT,
        attempt_setup(&app, "blocked_admin", TEST_PASSWORD, Some(SETUP_TOKEN)),
    )
    .await
    .expect("a saturated semaphore must produce a refusal, not a hang");
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "setup must refuse with 503 while the Argon2id pool is exhausted"
    );
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&harness.pool)
        .await
        .expect("count users");
    assert_eq!(users, 0, "a refused setup must not create a user");

    drop(held);

    let status = tokio::time::timeout(
        REQUEST_TIMEOUT,
        attempt_setup(&app, "blocked_admin", TEST_PASSWORD, Some(SETUP_TOKEN)),
    )
    .await
    .expect("setup must proceed once the permit is released");
    assert_eq!(
        status,
        StatusCode::CREATED,
        "setup must succeed once the Argon2id permit is available again"
    );
}

/// Moving the hash earlier must not weaken the in-transaction `COUNT(*)` re-check
/// (`ISSUE-0180`, `AUTH-1`).
///
/// Passes before and after the reorder; it is the non-regression that catches a reorder
/// which drops the double-check, or checks outside the transaction.
#[tokio::test]
#[doc = "Spec: AUTH-1"]
async fn concurrent_setups_produce_exactly_one_admin() {
    let harness = harness().await;
    let setup_state = setup_state_for(&harness, Some(SETUP_TOKEN));
    let auth_state = auth_state_for(&harness);

    let batch = async {
        let mut set = tokio::task::JoinSet::new();
        for i in 0..20 {
            let app =
                llmroller_admin::control_plane_router(setup_state.clone(), auth_state.clone());
            set.spawn(async move {
                attempt_setup(
                    &app,
                    &format!("racing_admin_{i}"),
                    TEST_PASSWORD,
                    Some(SETUP_TOKEN),
                )
                .await
            });
        }
        let mut statuses = Vec::with_capacity(20);
        while let Some(joined) = set.join_next().await {
            statuses.push(joined.expect("setup task must not panic"));
        }
        statuses
    };

    let statuses = tokio::time::timeout(REQUEST_TIMEOUT, batch)
        .await
        .expect("concurrent setups must not hang");

    let created = statuses
        .iter()
        .filter(|s| **s == StatusCode::CREATED)
        .count();
    assert_eq!(
        created, 1,
        "exactly one concurrent setup must win, not {created}; statuses: {statuses:?}"
    );
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&harness.pool)
        .await
        .expect("count users");
    assert_eq!(users, 1, "exactly one user must exist after the race");
}

// ---------------------------------------------------------------------------
// #181 — role validation and permit scope on the admin user API
// ---------------------------------------------------------------------------

struct AdminSession {
    cookie: String,
    csrf: String,
}

async fn seed_admin(harness: &Harness) -> llmroller_core::ids::UserId {
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    create_user(
        &harness.pool,
        &system_actor,
        llmroller_store::users::NewUser {
            username: "permit_scope_admin",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &OsEntropy,
        1,
    )
    .await
    .expect("seed admin")
    .id
}

async fn admin_session(harness: &Harness, admin: llmroller_core::ids::UserId) -> AdminSession {
    let raw = "permit-scope-admin-session-token-0123456789";
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let id_hash = llmroller_store::crypto::sha256_hex(raw.as_bytes());
    let csrf = "permit-scope-csrf-token-0123456789";
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf.as_bytes());
    insert_web_session(
        &harness.pool,
        NewWebSession {
            id_hash: &id_hash,
            user_id: admin,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("permit-scope-test"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("insert session");
    AdminSession {
        cookie: format!("llmroller_session={raw}"),
        csrf: csrf.to_owned(),
    }
}

fn build_api(harness: &Harness, auth_state: AuthAppState) -> axum::Router {
    let mut api_state = ApiAppState::new(
        auth_state.clone(),
        harness.master_key.clone(),
        "key-0".to_owned(),
    );
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x24; 32]));
    api_state.attach_pin_store(Arc::new(llmroller_core::pins::PinStore::new(1_000)));
    control_plane_router_full(setup_state_for(harness, None), auth_state, api_state)
}

fn create_user_request(role: &str, username: &str, session: &AdminSession) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/admin/api/users")
        .header(header::COOKIE, &session.cookie)
        .header("x-csrf-token", &session.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))))
        .body(Body::from(
            serde_json::json!({
                "username": username,
                "password": TEST_PASSWORD,
                "role": role,
            })
            .to_string(),
        ))
        .expect("create user request")
}

/// An unrecognized role must be refused with `422`, not silently coerced to `user`
/// (`ISSUE-0181`, `AUTH-4`, `USR-5`).
///
/// The "no user created" assertion is the load-bearing one: a `422` that still inserted a
/// row would be the same defect with a better status code.
#[tokio::test]
#[doc = "Spec: AUTH-4, Spec: USR-5"]
async fn create_user_rejects_an_unknown_role() {
    let harness = harness().await;
    let admin = seed_admin(&harness).await;
    let session = admin_session(&harness, admin).await;
    let app = build_api(&harness, auth_state_for(&harness));

    for bad_role in ["superadmin", "adm", "root", "", "administrator", "Admin"] {
        let response = app
            .clone()
            .oneshot(create_user_request(
                bad_role,
                &format!("typo_role_{}", bad_role.len()),
                &session,
            ))
            .await
            .expect("router call");
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "role {bad_role:?} must be refused with 422, not coerced"
        );
    }

    let created: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username LIKE 'typo_role_%'")
            .fetch_one(&harness.pool)
            .await
            .expect("count typo users");
    assert_eq!(
        created, 0,
        "a refused role must not create an account; {created} were created"
    );
}

/// The two accepted spellings must create the role the caller asked for, and validation
/// must not have broken the happy path (`ISSUE-0181`).
#[tokio::test]
#[doc = "Spec: AUTH-4"]
async fn create_user_accepts_the_two_known_roles() {
    let harness = harness().await;
    let admin = seed_admin(&harness).await;
    let session = admin_session(&harness, admin).await;
    let app = build_api(&harness, auth_state_for(&harness));

    for (role, expected) in [("admin", "admin"), ("user", "user")] {
        let response = app
            .clone()
            .oneshot(create_user_request(
                role,
                &format!("known_role_{role}"),
                &session,
            ))
            .await
            .expect("router call");
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "role {role:?} must be accepted"
        );
        let stored: String = sqlx::query_scalar("SELECT role FROM users WHERE username = ?")
            .bind(format!("known_role_{role}"))
            .fetch_one(&harness.pool)
            .await
            .expect("stored role");
        assert_eq!(stored, expected, "role {role:?} must be stored verbatim");
    }
}

/// `PATCH /admin/api/users/{id}` must validate `role` the same way `POST` does
/// (`ISSUE-0181`). This is the second of the two role parses the issue calls out.
#[tokio::test]
#[doc = "Spec: AUTH-4, Spec: USR-5"]
async fn patch_user_rejects_an_unknown_role() {
    let harness = harness().await;
    let admin = seed_admin(&harness).await;
    let session = admin_session(&harness, admin).await;
    let app = build_api(&harness, auth_state_for(&harness));

    let target = llmroller_core::ids::UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, must_change_password,
                            totp_enabled, can_use_global_providers, disabled, created_at)
         VALUES (?, 'patch_target', 'not-a-real-hash', 'user', 0, 0, 1, 0, 1)",
    )
    .bind(target.to_string())
    .execute(&harness.pool)
    .await
    .expect("seed target user");

    let request = Request::builder()
        .method("PATCH")
        .uri(format!("/admin/api/users/{target}"))
        .header(header::COOKIE, &session.cookie)
        .header("x-csrf-token", &session.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))))
        .body(Body::from(
            serde_json::json!({ "role": "superadmin" }).to_string(),
        ))
        .expect("patch request");
    let response = app.oneshot(request).await.expect("router call");
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "PATCH with an unknown role must be refused with 422, not coerced"
    );

    let role_after: String = sqlx::query_scalar("SELECT role FROM users WHERE id = ?")
        .bind(target.to_string())
        .fetch_one(&harness.pool)
        .await
        .expect("role after");
    assert_eq!(
        role_after, "user",
        "a refused role patch must not silently change the stored role"
    );
}

/// Creating a user must refuse with `503` instead of hashing when the Argon2id pool is
/// exhausted (`ISSUE-0181`).
///
/// This is the assertion that distinguishes "the permit covers the hash" from "a permit
/// exists somewhere": the pool the handler uses is the one this test saturates, so a
/// handler whose permit does not gate the hash would still return `201`.
#[tokio::test]
#[doc = "Spec: SEC-4"]
async fn create_user_hash_is_bounded_by_the_semaphore() {
    let harness = harness().await;
    let admin = seed_admin(&harness).await;
    let session = admin_session(&harness, admin).await;

    let mut auth_state = auth_state_for(&harness);
    let permits = Arc::new(tokio::sync::Semaphore::new(1));
    auth_state.argon2_semaphore = permits.clone();
    let app = build_api(&harness, auth_state);

    let held = permits.clone().acquire_owned().await.expect("hold permit");

    let attempt = app
        .clone()
        .oneshot(create_user_request("user", "unbounded_hash_user", &session));
    let status = tokio::time::timeout(REQUEST_TIMEOUT, attempt)
        .await
        .expect("a saturated Argon2id pool must produce a refusal, not an unbounded wait")
        .expect("router call")
        .status();
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "user creation must fail fast with 503 while the Argon2id pool is exhausted"
    );

    let created: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = 'unbounded_hash_user'")
            .fetch_one(&harness.pool)
            .await
            .expect("count");
    assert_eq!(created, 0, "a refused creation must not create an account");
    drop(held);
}
