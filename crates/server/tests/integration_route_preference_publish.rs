//! A route preference written through the API must be visible to the data plane immediately
//! (`ISSUE-0233`).
//!
//! # The claim, and what this file actually proves
//!
//! The issue says `put_user_route_preference_handler` runs a bare UPSERT on `state.auth.pool`
//! with "no transaction, no audit entry, no `ConfigDomain` publish, no version bump", and that a
//! row written without the publish leaves the data plane routing on the old preference.
//!
//! `user_route_prefs` feeds routing (`crates/proxy/src/orchestrator/routes.rs`), and it is
//! projected into `ConfigSnapshot` by `load_catalog_snapshot_tx`
//! (`crates/store/src/models.rs:794`), which the reloader rebuilds on `ConfigDomain::Models`.
//!
//! The reloader is **notification-driven** — it blocks on `recv()` and only rebuilds when a
//! `ReloadNotification` arrives (`crates/server/src/reloader.rs:592`). It does not poll. That
//! matters for this file: it means "the snapshot reflects the write" cannot happen by accident
//! from a background timer, so a passing test is not passing for a reason unrelated to the
//! handler.
//!
//! # Why the assertion is on the snapshot, not the row
//!
//! A test that read the database back would pass against the broken code, because the UPSERT
//! always wrote the row. The defect is that the *snapshot* was left stale. So every write test
//! here asserts on `shared.load().user_route_prefs`, and the red proof for this file is recorded
//! in the cycle-36 report: the same assertion against the pre-fix handler fails with the
//! preference absent from the snapshot.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::{ApiAppState, SetupAppState, control_plane_router_full};
use llmroller_core::ids::{ModelPk, UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy, secret_string};
use llmroller_core::snapshot::ConfigSnapshot;
use llmroller_server::reloader::{load_initial_snapshot, start_reloader};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider, create_upstream_key,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};
use llmroller_store::writer::start_writer;

const SESSION_TOKEN: &str = "route_pref_session_token_0123456789";
const CSRF_TOKEN: &str = "route_pref_csrf_token_0123456789ab";
const CANONICAL: &str = "route-pref-target-model";

struct RoutePrefHarness {
    _dir: tempfile::TempDir,
    app: Router,
    pool: sqlx::SqlitePool,
    shared: Arc<ArcSwap<ConfigSnapshot>>,
    user_id: UserId,
    key_id: UpstreamKeyId,
    model_pk: ModelPk,
}

async fn harness() -> RoutePrefHarness {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("route_pref.db");
    let pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let master_key = MasterKey::from_bytes([0xC7; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance");
    seed_defaults(&pool, None, 1).await.expect("settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "carol_c36",
            password: "carol-c36-password",
            role: Role::User,
            must_change_password: false,
        },
        &OsEntropy,
        100,
    )
    .await
    .expect("user");
    let actor = ActorScope::user(user.id, Role::User);

    let model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) \
         VALUES (?, ?, 1, 'smooth_weighted_rr')",
    )
    .bind(model_pk.to_string())
    .bind(CANONICAL)
    .execute(&pool)
    .await
    .expect("model");

    let provider = create_provider(
        &pool,
        &actor,
        NewProvider {
            name: "carol-provider".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(user.id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        100,
    )
    .await
    .expect("provider");

    let key = create_upstream_key(
        &pool,
        &actor,
        &master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "carol-key".to_string(),
            secret: secret_string("sk-carol-route-pref-secret"),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        100,
    )
    .await
    .expect("upstream key");

    sqlx::query(
        "INSERT INTO model_routes \
         (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, 'native-model', '[\"chat\"]', 'active', 1, 100)",
    )
    .bind(key.id.to_string())
    .bind(model_pk.to_string())
    .execute(&pool)
    .await
    .expect("route");

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
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());

    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: None,
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let mut api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    api_state.writer = Some(writer);
    api_state.attach_reloader(
        reloader_handle.notification_sender(),
        reloader_handle.version_receiver(),
    );
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x37; 32]));

    let app = control_plane_router_full(setup_state, auth_state, api_state);

    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let csrf_hash = llmroller_store::crypto::sha256_hex(CSRF_TOKEN.as_bytes());
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(SESSION_TOKEN.as_bytes()),
            user_id: user.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("route-pref-test"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("session");

    RoutePrefHarness {
        _dir: dir,
        app,
        pool,
        shared,
        user_id: user.id,
        key_id: key.id,
        model_pk,
    }
}

impl RoutePrefHarness {
    async fn put_preference(&self, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("PUT")
            .uri(format!("/api/user/routes/{}/{CANONICAL}", self.key_id))
            .header("cookie", format!("llmroller_session={SESSION_TOKEN}"))
            .header("content-type", "application/json")
            .header("x-csrf-token", CSRF_TOKEN)
            .body(Body::from(body.to_string()))
            .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
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

    /// The published snapshot's preference for `(user, key, model)`.
    fn published_preference(&self) -> Option<llmroller_core::snapshot::SnapshotRoutePreference> {
        self.shared
            .load()
            .user_route_prefs
            .get(&(self.user_id, self.key_id, self.model_pk))
            .cloned()
    }

    /// The stored row's `(priority_override, enabled)`, or `None` if absent.
    async fn stored_preference(&self) -> Option<(i64, bool)> {
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT priority_override, enabled FROM user_route_preferences \
             WHERE user_id = ? AND upstream_key_id = ? AND model_pk = ?",
        )
        .bind(self.user_id.to_string())
        .bind(self.key_id.to_string())
        .bind(self.model_pk.to_string())
        .fetch_optional(&self.pool)
        .await
        .expect("read preference");
        row.map(|(priority, enabled)| (priority, enabled != 0))
    }
}

// ---------------------------------------------------------------------------
// Primary red
// ---------------------------------------------------------------------------

/// Writing a route preference must publish a snapshot that already contains it.
///
/// Pre-fix the row was written but no `ConfigDomain` publish happened, so
/// `shared.load().user_route_prefs` did not contain the preference until some *other* mutation
/// triggered a reload. The data plane routes from that map, so the preference was silently
/// ignored — the row said one thing and live traffic did another.
///
/// Nothing else in this test mutates config after the write, and the reloader only rebuilds on a
/// notification, so a passing result cannot come from an unrelated publish.
#[tokio::test]
#[doc = "Spec: REL-4, RTE-4, RTE-11"]
async fn user_route_preference_is_visible_to_the_data_plane_immediately() {
    let h = harness().await;

    // Precondition: no preference is published, so a later hit cannot be pre-existing.
    assert!(
        h.published_preference().is_none(),
        "precondition: the snapshot must not already carry this preference"
    );

    let (status, body) = h
        .put_preference(serde_json::json!({"priority_override": 7, "enabled": true}))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the write must succeed; body: {body}"
    );

    // The row is stored (this passed pre-fix too — it is not the assertion that matters).
    assert_eq!(
        h.stored_preference().await,
        Some((7, true)),
        "the preference row must be written"
    );

    // The published snapshot must already carry it, with no other mutation to trigger a reload.
    let published = h.published_preference().expect(
        "the published snapshot must carry the new preference without another mutation \
         triggering a publish; pre-fix it did not, so the data plane routed on the old value",
    );
    assert_eq!(
        published.priority_override, 7,
        "the published preference must reflect the written priority"
    );
    assert!(
        published.enabled,
        "the published preference must be enabled"
    );
}

/// A second write must also publish — not just the first, which a one-shot fix could fake.
#[tokio::test]
#[doc = "Spec: REL-4, RTE-4"]
async fn a_second_route_preference_write_also_publishes() {
    let h = harness().await;

    let (status, _) = h
        .put_preference(serde_json::json!({"priority_override": 3, "enabled": true}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        h.published_preference().map(|p| p.priority_override),
        Some(3),
        "the first write must publish"
    );

    let (status, _) = h
        .put_preference(serde_json::json!({"priority_override": 9, "enabled": false}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let published = h
        .published_preference()
        .expect("the second write must publish too");
    assert_eq!(
        published.priority_override, 9,
        "the second write must reach the data plane"
    );
    assert!(
        !published.enabled,
        "a disable must be visible, or the data plane keeps routing to a disabled preference"
    );
}

/// Publishing must not be achieved by silently writing something else: an invalid target is
/// still refused and leaves the snapshot alone.
#[tokio::test]
#[doc = "Spec: RTE-4, USR-3"]
async fn an_unknown_model_is_refused_and_does_not_publish() {
    let h = harness().await;

    let request = Request::builder()
        .method("PUT")
        .uri(format!(
            "/api/user/routes/{}/model-that-does-not-exist",
            h.key_id
        ))
        .header("cookie", format!("llmroller_session={SESSION_TOKEN}"))
        .header("content-type", "application/json")
        .header("x-csrf-token", CSRF_TOKEN)
        .body(Body::from(
            serde_json::json!({"priority_override": 5}).to_string(),
        ))
        .expect("request");
    let response = h.app.clone().oneshot(request).await.expect("response");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "an unknown model must be 404"
    );
    assert!(
        h.published_preference().is_none(),
        "a refused write must not publish a preference"
    );
    assert_eq!(
        h.stored_preference().await,
        None,
        "a refused write must not store a preference"
    );
}

// ---------------------------------------------------------------------------
// The version-bump count must not regress
// ---------------------------------------------------------------------------

/// Reaching the atomic path must not bump `config_version` twice for one write.
///
/// This is the trap in the conversion: today the write is a bare statement and the barrier's
/// `publish_control_change_with_invalidations` performs the single bump. If the handler is
/// converted to `execute_atomic_mutation`, `bump_config_version_tx` runs inside the mutation *and*
/// the barrier still fires on the same request — unless the atomic path marks the mutation
/// handled, which it does via `MUTATION_HANDLED`. An extra bump is not a correctness bug (the
/// reloader would reload again) but it is a measurable regression in publish churn, and it would
/// mean the two paths are double-counting.
///
/// `MUTATION_HANDLED` being set is exactly what suppresses the barrier, so asserting one bump
/// pins that the conversion actually reached the atomic path rather than merely wrapping a bare
/// statement.
#[tokio::test]
#[doc = "Spec: REL-4"]
async fn a_route_preference_write_bumps_the_config_version_exactly_once() {
    let h = harness().await;

    let before: i64 = sqlx::query_scalar("SELECT config_version FROM instance_meta")
        .fetch_one(&h.pool)
        .await
        .expect("version before");

    let (status, body) = h
        .put_preference(serde_json::json!({"priority_override": 4, "enabled": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "write must succeed; body: {body}");

    let after: i64 = sqlx::query_scalar("SELECT config_version FROM instance_meta")
        .fetch_one(&h.pool)
        .await
        .expect("version after");

    assert_eq!(
        after - before,
        1,
        "one write must bump the config version exactly once; pre-fix and a naive conversion          differ here ({before} -> {after})"
    );
}
