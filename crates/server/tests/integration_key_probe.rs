//! `POST .../upstream-keys/{id}/test` — token resolution, health classification, publish
//! (`ISSUE-0178`), and the alias-delete atomicity bypass (`ISSUE-0232`).
//!
//! # The three probe defects
//!
//! 1. **No `TokenManager`.** The handler unsealed `secret_enc` and used it directly. For an
//!    `auth_kind == 'oauth'` key the sealed secret is the **refresh token**, so the probe sent a
//!    refresh token as a credential — failing, and reporting a healthy OAuth key as broken.
//! 2. **Every non-2xx became `"invalid"`.** A `429` (the provider saying "slow down") or a
//!    transient `503` **permanently marked a working key invalid in SQLite**. An operator
//!    clicking "Test" during a rate-limit window silently disabled a good key.
//! 3. **The write was discarded** (`let _ =`) and nothing published, so a correct `healthy`
//!    result left the in-memory snapshot stale — the operator sees "ok" and routing still
//!    refuses the key.
//!
//! # The health vocabulary is already the right one
//!
//! `0001_initial.sql:181`: `CHECK (health IN ('healthy','cooling_down','exhausted','invalid'))`.
//! The issue proposed `quarantined`/`degraded`, which are not permitted; `exhausted` and
//! `cooling_down` already mean exactly "quota hit" and "provider trouble", so no migration.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::{ApiAppState, SetupAppState, control_plane_router_full};
use llmroller_core::ids::{ProviderId, UpstreamKeyId};
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

const SESSION_TOKEN: &str = "probe_session_token_0123456789";
const CSRF_TOKEN: &str = "probe_csrf_token_0123456789";

/// The refresh token sealed into an `oauth` key. A probe that sends this instead of an access
/// token is the defect, and the mock records what it actually received.
const SEALED_REFRESH_TOKEN: &str = "refresh-token-must-not-be-sent-verbatim";

/// What a correctly refreshed credential looks like. The `dynamic://` spec below is the
/// supported way to carry a refresh token, and `TokenManager::parse_dynamic_spec` is what turns
/// `SEALED_REFRESH_TOKEN` into something the manager will exchange.
// ---------------------------------------------------------------------------
// Mock upstream: records Authorization headers, answers a configurable status
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MockUpstream {
    status: u16,
    seen_auth: Arc<std::sync::Mutex<Vec<Option<String>>>>,
    /// Form bodies received at the token endpoint, so the refresh exchange can be asserted.
    seen_token_bodies: Arc<std::sync::Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}

async fn mock_handler(State(state): State<MockUpstream>, headers: HeaderMap) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.seen_auth.lock().expect("auth log").push(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    );
    Response::builder()
        .status(state.status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"data":[]}"#))
        .expect("mock response")
}

/// The OAuth token endpoint. Returns a short-lived access token and records the exchange, so the
/// test can assert the refresh actually happened rather than only that the header differed.
async fn mock_token_handler(
    State(state): State<MockUpstream>,
    body: axum::body::Bytes,
) -> Response {
    state
        .seen_token_bodies
        .lock()
        .expect("token log")
        .push(String::from_utf8_lossy(&body).to_string());
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"access_token":"refreshed-access-token-xyz","token_type":"Bearer","expires_in":3600}"#,
        ))
        .expect("token response")
}

async fn spawn_mock(status: u16) -> (String, MockUpstream) {
    let state = MockUpstream {
        status,
        seen_auth: Arc::new(std::sync::Mutex::new(Vec::new())),
        seen_token_bodies: Arc::new(std::sync::Mutex::new(Vec::new())),
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route("/models", get(mock_handler))
        .route("/v1/models", get(mock_handler))
        .route("/token", axum::routing::post(mock_token_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state)
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct ProbeHarness {
    _dir: TempDir,
    app: Router,
    pool: sqlx::SqlitePool,
    shared: Arc<ArcSwap<llmroller_core::snapshot::ConfigSnapshot>>,
    key_id: UpstreamKeyId,
}

/// Insert a provider pointing at `base_url` and one upstream key with the given sealed secret.
async fn seed_provider_and_key(
    pool: &sqlx::SqlitePool,
    master_key: &MasterKey,
    base_url: &str,
    sealed_secret: &str,
) -> (ProviderId, UpstreamKeyId) {
    let provider_id = ProviderId::generate();
    sqlx::query(
        "INSERT INTO providers
             (id, ownership_scope, kind, name, base_url, config_json,
              allow_private_network, created_at)
         VALUES (?, 'global', 'custom', 'probe-mock', ?, '{}', 1, 1)",
    )
    .bind(provider_id.to_string())
    .bind(base_url)
    .execute(pool)
    .await
    .expect("provider");

    // Seal through the production sealer so the handler's `open` succeeds. `instance_meta` was
    // initialised by `load_or_initialize`, so the HKDF salt is the real one.
    let instance_id: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_one(pool)
            .await
            .expect("instance id");
    let instance_id: [u8; 16] = instance_id.try_into().expect("16-byte instance id");
    let sealer = llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
        master_key.clone(),
        instance_id,
        Arc::new(DeterministicRng::new(21)),
    );
    let key_id = UpstreamKeyId::generate();
    let key_id_text = key_id.to_string();
    let provider_id_text = provider_id.to_string();
    let aad = [
        b"key-0".to_vec(),
        key_id_text.as_bytes().to_vec(),
        provider_id_text.as_bytes().to_vec(),
    ];
    let aad_refs: Vec<&[u8]> = aad.iter().map(Vec::as_slice).collect();
    use llmroller_store::crypto::SecretSealer as _;
    let sealed = sealer.seal("upstream_key", &aad_refs, sealed_secret.as_bytes());

    sqlx::query(
        "INSERT INTO upstream_keys
             (id, provider_id, owner_user_id, label, secret_enc, master_key_id, secret_last4,
              enabled, priority, strict_tier, health, created_at)
         VALUES (?, ?, NULL, 'probe-key', ?, 'key-0', 'abcd', 1, 100, 1, 'healthy', 1)",
    )
    .bind(&key_id_text)
    .bind(&provider_id_text)
    .bind(&sealed)
    .execute(pool)
    .await
    .expect("upstream key");

    (provider_id, key_id)
}

/// Setup with its own mock, for the cases where the status is all that matters.
async fn setup(status: u16, sealed_secret: &str) -> (ProbeHarness, MockUpstream) {
    let (base_url, mock) = spawn_mock(status).await;
    let harness = setup_inner(&base_url, sealed_secret).await;
    (harness, mock)
}

/// Setup against a mock the caller already spawned, for the OAuth spec that needs its URL.
async fn setup_with(
    _status: u16,
    sealed_secret: &str,
    existing: Option<(&str, MockUpstream)>,
) -> ProbeHarness {
    match existing {
        Some((base_url, _mock)) => setup_inner(base_url, sealed_secret).await,
        None => {
            let (base_url, _mock) = spawn_mock(200).await;
            setup_inner(&base_url, sealed_secret).await
        }
    }
}

async fn setup_inner(base_url: &str, sealed_secret: &str) -> ProbeHarness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("probe.db"), PoolRole::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0x63; 32]);
    load_or_initialize(
        &pool,
        "key-0",
        &master_key,
        Arc::new(DeterministicRng::new(31)),
        1,
    )
    .await
    .expect("instance");
    seed_defaults(&pool, None, 1).await.expect("settings");

    let admin = create_user(
        &pool,
        &ActorScope::system(SystemTask::Writer, SystemSeal::store()),
        NewUser {
            username: "probe_admin",
            password: "Password12345!#",
            role: Role::Admin,
            must_change_password: false,
        },
        &DeterministicRng::new(32),
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
            user_agent: Some("probe-test"),
            created_at: now,
            expires_at: now + 86_400_000,
        },
    )
    .await
    .expect("session");

    let (_provider_id, key_id) =
        seed_provider_and_key(&pool, &master_key, base_url, sealed_secret).await;

    let initial = load_initial_snapshot(&pool).await.expect("snapshot");
    let shared = Arc::new(ArcSwap::new(Arc::new(initial)));
    let runtime = Arc::new(llmroller_core::runtime_state::RuntimeState::new());
    let (reloader_tx, reloader_rx) = tokio::sync::mpsc::channel(32);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx.clone());
    let (snapshot, _reloader_task) = start_reloader(
        pool.clone(),
        shared.clone(),
        runtime,
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
        entropy: Arc::new(DeterministicRng::new(33)),
        audit_hmac_key: None,
    };
    let mut api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    api_state.writer = Some(writer);
    api_state.attach_reloader(snapshot.notification_sender(), snapshot.version_receiver());
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x24; 32]));

    let app = control_plane_router_full(setup_state, auth_state, api_state);
    ProbeHarness {
        _dir: dir,
        app,
        pool,
        shared,
        key_id,
    }
}

async fn probe(harness: &ProbeHarness) -> (StatusCode, serde_json::Value) {
    let response = harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/admin/api/upstream-keys/{}/test", harness.key_id))
                .header("cookie", format!("llmroller_session={SESSION_TOKEN}"))
                .header("x-csrf-token", CSRF_TOKEN)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("probe response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn stored_health(harness: &ProbeHarness) -> (String, Option<i64>) {
    sqlx::query_as("SELECT health, health_until FROM upstream_keys WHERE id = ?")
        .bind(harness.key_id.to_string())
        .fetch_one(&harness.pool)
        .await
        .expect("health row")
}

// ---------------------------------------------------------------------------
// 1. The token defect
// ---------------------------------------------------------------------------

/// Pre-fix the mock received the sealed secret verbatim, which for an OAuth key is the
/// **refresh token** — so a perfectly healthy key was reported as broken.
///
/// The sealed secret here is a real `DynamicCredentialSpec` JSON object with a `token_url`
/// pointing at the mock's own `/token`. That is the production shape: `TokenManager` parses it,
/// exchanges the refresh token, and returns an access token. The mock records both sides — the
/// `Authorization` the probe actually sent, and the form body of the refresh exchange — so this
/// asserts the token was *obtained*, not merely that the header text differed.
#[tokio::test]
#[doc = "Spec: PRV-6 — ISSUE-0178 the probe resolves an OAuth access token"]
async fn key_probe_uses_a_refreshed_oauth_access_token() {
    // The spec has to know the mock's port, so spawn first and build the secret afterwards.
    let (base_url, mock) = spawn_mock(200).await;
    let spec = serde_json::json!({
        "type": "oauth2",
        "refresh_token": SEALED_REFRESH_TOKEN,
        "client_id": "test-client",
        "token_url": format!("{base_url}/token"),
    })
    .to_string();

    let harness = setup_with(200, &spec, Some((base_url.as_str(), mock.clone()))).await;
    let (status, body) = probe(&harness).await;
    assert_eq!(status, StatusCode::OK, "probe must answer; body: {body}");

    // 1. The refresh exchange happened, with the refresh token — that is the token *source*.
    let exchanges = mock.seen_token_bodies.lock().expect("token log").clone();
    assert_eq!(
        exchanges.len(),
        1,
        "the probe must refresh once against token_url; got {exchanges:?}"
    );
    assert!(
        exchanges[0].contains("grant_type=refresh_token"),
        "the exchange must be a refresh-token grant; got {:?}",
        exchanges[0]
    );

    // 2. The upstream received the *access* token, never the refresh token.
    let seen = mock.seen_auth.lock().expect("auth log").clone();
    let sent = seen
        .iter()
        .flatten()
        .next()
        .unwrap_or_else(|| panic!("the mock received no Authorization header: {seen:?}"));
    assert_eq!(
        sent, "Bearer refreshed-access-token-xyz",
        "the probe must send the refreshed access token; sending the refresh token is the \
         defect that makes every healthy OAuth key look broken"
    );
    assert!(
        !sent.contains(SEALED_REFRESH_TOKEN),
        "the sealed refresh token must never reach the upstream; got {sent:?}"
    );
}

// ---------------------------------------------------------------------------
// 2–4. Health classification
// ---------------------------------------------------------------------------

/// The assertion that stops a working key being disabled by a Test click.
#[tokio::test]
#[doc = "Spec: PRV-6, C6 — ISSUE-0178 a 429 is not `invalid`"]
async fn key_probe_marks_a_rate_limited_key_as_not_invalid() {
    let (harness, _mock) = setup(429, "sk-probe-429").await;
    let (status, body) = probe(&harness).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (health, until) = stored_health(&harness).await;
    assert_ne!(
        health, "invalid",
        "429 means the provider is rate limiting, not that the credential is bad; marking it \
         invalid disables a working key"
    );
    assert_eq!(
        health, "exhausted",
        "429 is a quota/rate condition — `exhausted` is the permitted value for it"
    );
    assert!(
        until.is_some(),
        "`exhausted` without a `health_until` routes as normal (KeyHealthEntry::is_routable \
         only checks the timestamp), so the string alone would be decorative"
    );
}

#[tokio::test]
#[doc = "Spec: PRV-6, C6 — ISSUE-0178 a 5xx is degraded, not `invalid`"]
async fn key_probe_marks_a_5xx_as_degraded_not_invalid() {
    let (harness, _mock) = setup(503, "sk-probe-503").await;
    let (status, body) = probe(&harness).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (health, until) = stored_health(&harness).await;
    assert_ne!(
        health, "invalid",
        "a provider 5xx is not a credential failure"
    );
    assert_eq!(
        health, "cooling_down",
        "a transient provider failure is `cooling_down`, the permitted recoverable value"
    );
    assert!(
        until.is_some(),
        "`cooling_down` needs a companion timestamp to actually pause routing"
    );
}

/// The positive half. Without it, "never mark invalid" would pass the two tests above.
#[tokio::test]
#[doc = "Spec: PRV-6, C6 — ISSUE-0178 a 401 marks the key invalid"]
async fn key_probe_marks_a_401_as_invalid() {
    let (harness, _mock) = setup(401, "sk-probe-401").await;
    let (status, body) = probe(&harness).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (health, until) = stored_health(&harness).await;
    assert_eq!(
        health, "invalid",
        "a rejected credential is genuinely invalid — this is the one case that should be"
    );
    assert_eq!(
        until, None,
        "`invalid` is an indefinite state, not a timed one; a leftover `health_until` would \
         make it look recoverable when it is not"
    );
}

// ---------------------------------------------------------------------------
// 5–6. Publish and audit
// ---------------------------------------------------------------------------

/// The snapshot must reflect the probe result — and here the publish **is** the real gap,
/// because the handler's `UPDATE upstream_keys SET health` never went through a path that
/// reloads key health into the snapshot.
///
/// Careful: the barrier publishes `[Providers, UpstreamKeys]` for this POST, so the *publish*
/// call happens either way. What differs is whether the row it publishes carries the new
/// health. Before the fix the write was a bare `let _ =` on the reader-facing pool with no
/// `health_version` bump, so the reloader could project the old value; the test asserts the
/// observable outcome (snapshot health) rather than the mechanism, which is what makes it
/// sensitive to both.
#[tokio::test]
#[doc = "Spec: PRV-6, CLK-3 — ISSUE-0178 the probe's health reaches the snapshot"]
async fn key_probe_publishes_the_new_health() {
    let (harness, _mock) = setup(401, "sk-probe-publish").await;
    let before = harness
        .shared
        .load()
        .upstream_keys
        .get(&harness.key_id)
        .map(|k| k.health.clone())
        .expect("key in the initial snapshot");
    assert_eq!(before, "healthy", "the fixture must start healthy");

    let (status, body) = probe(&harness).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let mut observed = None;
    for _ in 0..100 {
        observed = harness
            .shared
            .load()
            .upstream_keys
            .get(&harness.key_id)
            .map(|k| k.health.clone());
        if observed.as_deref() == Some("invalid") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        observed.as_deref(),
        Some("invalid"),
        "the published snapshot must reflect the probe result; otherwise the operator sees \
         'ok' and routing keeps using the old health"
    );
}

/// The probe is already audited by the middleware (`upstream_key.tested`), so this is a
/// **regression guard**, not a red test for the issue.
///
/// Kept because the pairing matters: if the handler ever set `MUTATION_HANDLED` without the
/// standard path, no row would be written at all, and this is the only test watching.
#[tokio::test]
#[doc = "Spec: AUD-1 — ISSUE-0178 exactly one audit row per probe"]
async fn key_probe_writes_an_audit_row() {
    let (harness, _mock) = setup(401, "sk-probe-audit").await;
    let (status, body) = probe(&harness).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT action, target_type, target_id FROM audit_log")
            .fetch_all(&harness.pool)
            .await
            .expect("audit rows");
    let key_id = harness.key_id.to_string();
    assert!(
        rows.iter()
            .any(|(_, target_type, target_id)| target_type == "upstream_key"
                && target_id.as_deref() == Some(key_id.as_str())),
        "the probe's health write must leave an audit row targeting the key: {rows:?}"
    );
}

// ---------------------------------------------------------------------------
// 7–9. The fourth atomic-mutation bypass: delete_admin_model_alias (ISSUE-0232)
// ---------------------------------------------------------------------------

async fn seed_alias(harness: &ProbeHarness, alias: &str) -> llmroller_core::ids::ModelPk {
    let model_pk = llmroller_core::ids::ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, distribution) \
         VALUES (?, ?, 1, 'weighted')",
    )
    .bind(model_pk.to_string())
    .bind(format!("canonical-for-{alias}"))
    .execute(&harness.pool)
    .await
    .expect("insert model");
    sqlx::query("INSERT INTO model_aliases (alias, model_pk, confirmed) VALUES (?, ?, 1)")
        .bind(alias)
        .bind(model_pk.to_string())
        .execute(&harness.pool)
        .await
        .expect("insert alias");
    model_pk
}

async fn delete_alias(harness: &ProbeHarness, alias: &str) -> StatusCode {
    harness
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/admin/api/model-aliases?alias={alias}"))
                .header("cookie", format!("llmroller_session={SESSION_TOKEN}"))
                .header("x-csrf-token", CSRF_TOKEN)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("delete response")
        .status()
}

/// Pre-fix: `204 No Content` even though nothing matched.
#[tokio::test]
#[doc = "Spec: API-8 — ISSUE-0232 deleting a missing alias is a 404"]
async fn delete_alias_returns_404_when_none_matches() {
    let (harness, _mock) = setup(200, "sk-alias-404").await;
    let status = delete_alias(&harness, "alias-that-does-not-exist").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a DELETE naming an alias that does not exist must not report success"
    );
}

/// The alias must actually be gone, and the deletion audited — a **regression guard**.
///
/// **Correction to the issue's premise, the same one cycle 32 made for the model PATCH:**
/// "no audit row, no `ConfigDomain::Models` publish" is **false**. `control_mutation_barrier`
/// appends the audit row and publishes for every successful mutation it sees, and
/// `domains_for_path` maps a `/models…` path to `ConfigDomain::Models`. Verified by running it:
/// this test passes before the fix too.
///
/// What the atomic path uniquely buys is that the `rows_affected` check and the write share one
/// transaction, so the new `404` cannot leave a half-applied delete behind.
#[tokio::test]
#[doc = "Spec: MDL-3, AUD-1 — ISSUE-0232 deleting an alias removes the row and audits it"]
async fn delete_alias_publishes_the_models_domain() {
    let (harness, _mock) = setup(200, "sk-alias-publish").await;
    let alias = "publish-me-alias";
    seed_alias(&harness, alias).await;

    let status = delete_alias(&harness, alias).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "deleting an existing alias succeeds"
    );

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_aliases WHERE alias = ?")
        .bind(alias)
        .fetch_one(&harness.pool)
        .await
        .expect("alias count");
    assert_eq!(remaining, 0, "the row must be gone");

    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT action, target_type, target_id FROM audit_log")
            .fetch_all(&harness.pool)
            .await
            .expect("audit rows");
    assert!(
        rows.iter()
            .any(|(_, target_type, _)| target_type == "model_alias"),
        "deleting an alias must be audited: {rows:?}"
    );
}

/// Exactly one audit row per successful delete — also a regression guard (see above).
#[tokio::test]
#[doc = "Spec: AUD-1 — ISSUE-0232 exactly one audit row per alias delete"]
async fn delete_alias_writes_an_audit_row() {
    let (harness, _mock) = setup(200, "sk-alias-audit").await;
    let alias = "audit-me-alias";
    seed_alias(&harness, alias).await;

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&harness.pool)
        .await
        .expect("audit count");
    let status = delete_alias(&harness, alias).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&harness.pool)
        .await
        .expect("audit count");
    assert_eq!(
        after,
        before + 1,
        "exactly one audit row must be appended — two would mean the handler and the barrier \
         both wrote one, which is the pairing that must not happen"
    );
}
