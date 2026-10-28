//! `PATCH /admin/api/models/{id}` — the partial-update, validation, 404, audit and publish
//! contract (`ISSUE-0170`).
//!
//! # The four defects this file pins
//!
//! 1. **Only `exposed` was accepted.** `PatchModelRequest` had one field, so `distribution` and
//!    `routing_strategy` could not be sent at all; serde's default drops unknown keys silently.
//! 2. **The SQL result was discarded** (`let _ =`), so a PATCH naming a `model_pk` that does not
//!    exist returned `200 OK` and changed nothing.
//! 3. **It bypassed `execute_atomic_mutation`**, so no `audit_log` row, no
//!    `ConfigDomain::Models` publish and no version bump — an operator flips exposure and the
//!    data plane keeps the old snapshot.
//! 4. **It validated nothing**, so a bad `distribution`/`routing_strategy` surfaced as a `500`
//!    from the SQLite CHECK constraint rather than a `422`.
//!
//! The two fields the issue also asked for are **declined** and not tested here because they do
//! not exist: `headroom_scaling` is an instance-wide runtime setting, and `max_context_tokens`
//! must stay upstream-discovered (`context_window`, migration `0002`, audit `F-031`).

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::{ApiAppState, SetupAppState, control_plane_router_full};
use llmroller_core::ids::ModelPk;
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

const SESSION_TOKEN: &str = "patch_model_session_token_0123456789";
const CSRF_TOKEN: &str = "patch_model_csrf_token_0123456789";

struct PatchHarness {
    _dir: TempDir,
    app: Router,
    pool: sqlx::SqlitePool,
    shared: Arc<ArcSwap<llmroller_core::snapshot::ConfigSnapshot>>,
    model_pk: ModelPk,
}

/// One discovered model, so the PATCH has a real row to update.
async fn seed_model(pool: &sqlx::SqlitePool) -> ModelPk {
    let model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, distribution, routing_strategy) \
         VALUES (?, 'patch-target-model', 1, 'weighted', NULL)",
    )
    .bind(model_pk.to_string())
    .execute(pool)
    .await
    .expect("insert model");
    model_pk
}

async fn setup() -> PatchHarness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("patch_model.db"), PoolRole::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0x52; 32]);
    load_or_initialize(
        &pool,
        "key-0",
        &master_key,
        Arc::new(DeterministicRng::new(11)),
        1,
    )
    .await
    .expect("instance");
    seed_defaults(&pool, None, 1).await.expect("settings");

    let admin = create_user(
        &pool,
        &ActorScope::system(SystemTask::Writer, SystemSeal::store()),
        NewUser {
            username: "patch_admin",
            password: "Password12345!#",
            role: Role::Admin,
            must_change_password: false,
        },
        &DeterministicRng::new(12),
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
            user_agent: Some("patch-test"),
            created_at: now,
            expires_at: now + 86_400_000,
        },
    )
    .await
    .expect("session");

    let model_pk = seed_model(&pool).await;

    let initial = load_initial_snapshot(&pool).await.expect("snapshot");
    let shared = Arc::new(ArcSwap::new(Arc::new(initial)));
    let runtime = Arc::new(llmroller_core::runtime_state::RuntimeState::new());
    let (reloader_tx, reloader_rx) = tokio::sync::mpsc::channel(32);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx.clone());
    let (reloader_handle, _reloader_task) = start_reloader(
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
        entropy: Arc::new(DeterministicRng::new(13)),
        audit_hmac_key: None,
    };
    let mut api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    api_state.writer = Some(writer);
    api_state.attach_reloader(
        reloader_handle.notification_sender(),
        reloader_handle.version_receiver(),
    );
    // Without an audit key the mutation path deliberately skips the audit append
    // (`ISSUE-0138`), so the audit test below would pass vacuously.
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x24; 32]));

    let app = control_plane_router_full(setup_state, auth_state, api_state);
    PatchHarness {
        _dir: dir,
        app,
        pool,
        shared,
        model_pk,
    }
}

fn patch_request(model_pk: &ModelPk, body: &str) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(format!("/admin/api/models/{model_pk}"))
        .header("cookie", format!("llmroller_session={SESSION_TOKEN}"))
        .header("content-type", "application/json")
        .header("x-csrf-token", CSRF_TOKEN)
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn patch(harness: &PatchHarness, body: &str) -> (StatusCode, serde_json::Value) {
    let response = harness
        .app
        .clone()
        .oneshot(patch_request(&harness.model_pk, body))
        .await
        .expect("patch response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// Read `(exposed, distribution, routing_strategy)` for the seeded model.
async fn read_row(harness: &PatchHarness) -> (Option<i64>, String, Option<String>) {
    sqlx::query_as("SELECT exposed, distribution, routing_strategy FROM models WHERE model_pk = ?")
        .bind(harness.model_pk.to_string())
        .fetch_one(&harness.pool)
        .await
        .expect("row")
}

// ---------------------------------------------------------------------------
// 1. The field that could not even be sent
// ---------------------------------------------------------------------------

/// Primary red. Pre-fix the body deserialises to a struct with no such fields, so both are
/// dropped and the row keeps `weighted` / `NULL` while the response is `200`.
#[tokio::test]
#[doc = "Spec: MDL-3, MDL-4 — ISSUE-0170 patch updates distribution and routing_strategy"]
async fn patch_model_updates_distribution_and_routing_strategy() {
    let harness = setup().await;
    let (status, body) = patch(
        &harness,
        r#"{"distribution":"strict","routing_strategy":"uniform_random"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch must succeed; body: {body}");
    let (exposed, distribution, routing) = read_row(&harness).await;
    assert_eq!(distribution, "strict", "distribution must be updated");
    assert_eq!(
        routing.as_deref(),
        Some("uniform_random"),
        "routing_strategy must be updated"
    );
    assert_eq!(exposed, Some(1), "an unrelated column must be untouched");
}

// ---------------------------------------------------------------------------
// 2. Validation must be a 422, and must not write
// ---------------------------------------------------------------------------

#[tokio::test]
#[doc = "Spec: MDL-3 — ISSUE-0170 an unknown distribution is a 422 and writes nothing"]
async fn patch_model_rejects_an_unknown_distribution() {
    let harness = setup().await;
    let before = read_row(&harness).await;

    let (status, body) = patch(
        &harness,
        r#"{"distribution":"round_robin_typo","routing_strategy":"uniform_random"}"#,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a bad enum value is client input, not a server fault; body: {body}"
    );
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("weighted") && message.contains("strict"),
        "the message must list the accepted values so the caller can fix it; got {message:?}"
    );
    assert_eq!(
        read_row(&harness).await,
        before,
        "a validation that still writes is not validation — including the valid \
         routing_strategy in the same body"
    );
}

#[tokio::test]
#[doc = "Spec: MDL-4 — ISSUE-0170 an unknown routing_strategy is a 422 and writes nothing"]
async fn patch_model_rejects_an_unknown_routing_strategy() {
    let harness = setup().await;
    let before = read_row(&harness).await;

    let (status, body) = patch(
        &harness,
        r#"{"distribution":"strict","routing_strategy":"least_connections"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("smooth_weighted_rr"),
        "the message must list the accepted strategies; got {message:?}"
    );
    assert_eq!(
        read_row(&harness).await,
        before,
        "must not write on rejection"
    );
}

// ---------------------------------------------------------------------------
// 3. A missing model is a 404, not a silent 200
// ---------------------------------------------------------------------------

#[tokio::test]
#[doc = "Spec: API-8 — ISSUE-0170 patch returns 404 for an unknown model_pk"]
async fn patch_model_returns_404_for_an_unknown_model_pk() {
    let harness = setup().await;
    let missing = ModelPk::generate();
    let response = harness
        .app
        .clone()
        .oneshot(patch_request(&missing, r#"{"exposed":0}"#))
        .await
        .expect("patch response");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a PATCH naming a model that does not exist must not report success"
    );
}

// ---------------------------------------------------------------------------
// 4. Partial update must not wipe the other columns
// ---------------------------------------------------------------------------

/// The guard against "SET three columns from possibly-None values". Without a COALESCE-style
/// statement, a body of `{"exposed":0}` writes `NULL` over `distribution`, which is
/// `NOT NULL` (so it 500s) and would silently clear `routing_strategy` (which is nullable).
#[tokio::test]
#[doc = "Spec: MDL-3, MDL-4 — ISSUE-0170 patching only exposed leaves the other fields alone"]
async fn patch_model_of_only_exposed_leaves_the_other_fields_alone() {
    let harness = setup().await;
    // Put a non-default routing strategy in place first, so "unchanged" is distinguishable
    // from "reset to the default".
    let (status, _) = patch(&harness, r#"{"routing_strategy":"power_of_two_choices"}"#).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = patch(&harness, r#"{"exposed":0}"#).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (exposed, distribution, routing) = read_row(&harness).await;
    assert_eq!(exposed, Some(0), "exposed must be updated");
    assert_eq!(distribution, "weighted", "distribution must be untouched");
    assert_eq!(
        routing.as_deref(),
        Some("power_of_two_choices"),
        "routing_strategy must be untouched, not reset to NULL"
    );
}

// ---------------------------------------------------------------------------
// 5. Audit row and snapshot publish
// ---------------------------------------------------------------------------

/// The mutation must be audited — but the audit row comes from `control_mutation_barrier`,
/// **not** from the handler.
///
/// This is a correction to the issue's premise: defect 3 says bypassing
/// `execute_atomic_mutation` means "no `audit_log` row". That is not true. The barrier
/// middleware inspects every PATCH/POST/PUT/DELETE, and unless the handler sets
/// `MUTATION_HANDLED` (which only `execute_atomic_mutation` does), it publishes the domains
/// **and** appends the audit row itself. So this assertion passes before and after the fix.
///
/// It is kept because the *pairing* still matters: a handler that both bypasses the standard
/// path **and** sets `MUTATION_HANDLED` would write no audit row at all. What the fix actually
/// changes is the `detail_json` — the handler's own mutation record names the changed fields,
/// where the barrier's generic record only names method and path. Asserted below.
#[tokio::test]
#[doc = "Spec: AUD-1, MDL-3 — ISSUE-0170 patch is audited with the changed fields recorded"]
async fn patch_model_writes_an_audit_row() {
    let harness = setup().await;
    let (status, body) = patch(&harness, r#"{"distribution":"strict"}"#).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let rows: Vec<(String, String, Option<String>, String)> =
        sqlx::query_as("SELECT action, target_type, target_id, detail_json FROM audit_log")
            .fetch_all(&harness.pool)
            .await
            .expect("audit rows");

    let row = rows
        .iter()
        .find(|(_, target_type, target_id, _)| {
            target_type == "model"
                && target_id.as_deref() == Some(harness.model_pk.to_string().as_str())
        })
        .unwrap_or_else(|| panic!("no audit row targeting the model: {rows:?}"));

    assert_eq!(
        row.0, "admin.model.updated",
        "action must name the operation"
    );
    // The barrier's fallback detail is just `{"method":..,"path":..}`. A handler-routed
    // mutation records what changed, which is what an auditor needs.
    assert!(
        row.3.contains("strict") || row.3.contains("distribution"),
        "the audit detail must record what changed, not only the method and path; \
         got {}",
        row.3
    );
}

/// The data plane must see the change without waiting for an unrelated reload.
#[tokio::test]
#[doc = "Spec: MDL-3, CLK-3 — ISSUE-0170 patch publishes the Models domain"]
async fn patch_model_publishes_the_models_domain() {
    let harness = setup().await;
    let before = harness
        .shared
        .load()
        .models
        .get(&harness.model_pk)
        .map(|m| m.distribution.clone());

    let (status, body) = patch(&harness, r#"{"distribution":"strict"}"#).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // The reloader is a background task; poll briefly rather than sleep a fixed duration.
    let mut observed = None;
    for _ in 0..100 {
        observed = harness
            .shared
            .load()
            .models
            .get(&harness.model_pk)
            .map(|m| m.distribution.clone());
        if observed.as_deref() == Some("strict") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // Distinguish "not published" from "published but the write never happened": read the
    // DB row too. If the DB has `strict` and the snapshot does not, the publish is broken.
    // If the DB still has `weighted`, the *write* never happened, so this test is really
    // asserting the same thing as the update test.
    let db_distribution: String =
        sqlx::query_scalar("SELECT distribution FROM models WHERE model_pk = ?")
            .bind(harness.model_pk.to_string())
            .fetch_one(&harness.pool)
            .await
            .expect("distribution");
    assert_eq!(
        observed.as_deref(),
        Some("strict"),
        "the published snapshot must reflect the change (before: {before:?}, db now: \
         {db_distribution:?}); without the publish the data plane keeps serving the old value"
    );
}

// ---------------------------------------------------------------------------
// 6. Clearing routing_strategy to inherit
// ---------------------------------------------------------------------------

/// `routing_strategy` is nullable and `NULL` means "inherit `default_routing_strategy`".
/// JSON `null` deserialises to `None`, which is indistinguishable from an absent field, so
/// the explicit way to clear is the empty string — documented in the handler.
#[tokio::test]
#[doc = "Spec: MDL-4 — ISSUE-0170 an empty routing_strategy clears it back to inherit"]
async fn patch_model_can_clear_routing_strategy_to_inherit() {
    let harness = setup().await;
    let (status, _) = patch(&harness, r#"{"routing_strategy":"naive_rr"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_row(&harness).await.2.as_deref(), Some("naive_rr"));

    let (status, body) = patch(&harness, r#"{"routing_strategy":""}"#).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        read_row(&harness).await.2,
        None,
        "an empty routing_strategy must clear the column so the model inherits the global \
         default; there is no other way to express NULL through a partial update"
    );
}

// ---------------------------------------------------------------------------
// The declared-out field must stay unwritable
// ---------------------------------------------------------------------------

/// `context_window` is upstream-discovered and must not become admin-writable.
///
/// Migration `0002` says NULL means the upstream never advertised a length and *"the proxy
/// must omit the field rather than fabricate a capability value (audit F-031)"*. An
/// admin-writable context window is a mechanism for fabricating exactly that. This test fails
/// if a future `PatchModelRequest` grows the field.
#[tokio::test]
#[doc = "Spec: MDL-7, F-031 — ISSUE-0170 context_window stays upstream-discovered"]
async fn patch_model_cannot_fabricate_a_context_window() {
    let harness = setup().await;
    let before: Option<i64> =
        sqlx::query_scalar("SELECT context_window FROM models WHERE model_pk = ?")
            .bind(harness.model_pk.to_string())
            .fetch_one(&harness.pool)
            .await
            .expect("context_window");
    assert_eq!(
        before, None,
        "the fixture must start with no advertised length"
    );

    let (status, _) = patch(&harness, r#"{"context_window":1000000}"#).await;

    let after: Option<i64> =
        sqlx::query_scalar("SELECT context_window FROM models WHERE model_pk = ?")
            .bind(harness.model_pk.to_string())
            .fetch_one(&harness.pool)
            .await
            .expect("context_window");
    assert_eq!(
        after, None,
        "a PATCH must never write context_window (status was {status}); doing so would let an \
         operator fabricate a capability value, which F-031 forbids"
    );
}
