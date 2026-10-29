//! The active-pins read view is a view of **live** state (`ISSUE-0168`).
//!
//! # What this governs
//!
//! `Section 14` specifies that `GET /api/user/pins` returns "live `PinStore` pins", and
//! `Section 14.t.1` that the Active Sessions screen renders "confirmed session pins in
//! `PinStore`". The durable `pins` table is instead the record of what survives a restart, which
//! the writer flushes on a `BATCH_MAX_AGE` (250 ms) tick — so between `acquire` and the flush, a
//! SQLite-only read view was blind to a pin the user had just created.
//!
//! # The half that matters
//!
//! A live `PinEntry` carries no owner: only `upstream_key_id` and `pool_tier`. The SQL predicate
//! that scopes the durable rows therefore has nothing to apply to a live entry, so a naive merge
//! either drops it or shows it to the wrong tenant. `a_live_pin_from_another_tenant_is_never_shown`
//! passes trivially *before* the fix, precisely because nothing was merged yet; it exists to stop
//! the new code from leaking, which is the realistic regression (`USR-3`, `SEC-17`).
//!
//! # Why these drive the real handlers
//!
//! The behaviour under test is the endpoint's own merge: which rows exist, in what order, with
//! which field winning. A unit test of the merge helper would not cover the handler picking it up
//! (the cycle-39 `run_retention_pass` lesson: an extracted function can be perfect and never
//! called). So each test builds a real router over a real database and calls the real endpoint.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig, derive_session_csrf_with_secret};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::eligibility::CandidatePoolTier;
use llmroller_core::ids::{ClientKeyId, ModelPk, ProviderId, UpstreamKeyId, UserId};
use llmroller_core::pins::{PinKey, PinState, PinStore};
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
    session_hmac_secret: llmroller_core::seams::SecretBytes,
    alice_key: FixtureKey,
    bob_key: FixtureKey,
}

struct FixtureKey {
    client_key_id: ClientKeyId,
    upstream_key_id: UpstreamKeyId,
    cookie: String,
}

async fn insert_session(
    pool: &sqlx::SqlitePool,
    user_id: UserId,
    raw_token: &str,
    session_hmac_secret: &[u8],
) -> String {
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
            user_agent: Some("pins-live-test"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("insert web session");
    format!("llmroller_session={raw_token}")
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

    // A durable row, so every test also has the "both sources present" baseline.
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

    let cookie = insert_session(
        pool,
        owner,
        &format!("{label}-pins-live-token-0123456789"),
        session_hmac_secret,
    )
    .await;
    FixtureKey {
        client_key_id,
        upstream_key_id,
        cookie,
    }
}

async fn harness() -> Harness {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("pins_live.db"), PoolRole::Writer)
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
    let make_user = |username: &'static str, password: &'static str| {
        let pool = pool.clone();
        async move {
            create_user(
                &pool,
                &system_actor,
                NewUser {
                    username,
                    password,
                    role: Role::User,
                    must_change_password: false,
                },
                &entropy,
                1,
            )
            .await
            .expect("create user")
        }
    };
    let alice = make_user("alice_pins_live", "supersecretalicepass123").await;
    let bob = make_user("bob_pins_live", "supersecretbobpass123").await;

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
        expected_token: Some("pins-live-token".to_string()),
        data_dir: harness._dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let mut api_state = ApiAppState::new(
        auth_state.clone(),
        harness.master_key.clone(),
        "key-0".to_string(),
    );
    api_state.attach_pin_store(pin_store);
    control_plane_router_full(setup_state, auth_state, api_state)
}

// ---------------------------------------------------------------------------
// The pins view is a view of **live** state (`GET /api/user/pins` returns "live
// `PinStore` pins" per Section 14, and Section 14.t.1 renders "confirmed session
// pins in `PinStore`").
//
// Pins reach SQLite through the async `durable_state` queue, whose flush cadence
// is `BATCH_MAX_AGE` (250 ms). So between `acquire` and the flush the read view
// was blind to a pin the user had just created. These tests pin the union.
//
// The security test (`a_live_pin_from_another_tenant_is_never_shown`) passes
// trivially before the fix — nothing is merged yet. It exists to stop the *new*
// code from leaking, which is the realistic regression: a live `PinEntry` has no
// owner field, so a naive merge either drops the entry or shows it to the wrong
// tenant (USR-3, SEC-17).
// ---------------------------------------------------------------------------

/// Read the `/api/user/pins` JSON list as `cookie`.
async fn get_pins_json(app: &axum::Router, cookie: &str) -> Vec<serde_json::Value> {
    let req = Request::builder()
        .method("GET")
        .uri("/api/user/pins")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

/// Read the `/app/partials/pins` HTML as `cookie`.
async fn get_pins_partial(app: &axum::Router, cookie: &str) -> String {
    let req = Request::builder()
        .method("GET")
        .uri("/app/partials/pins")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .expect("req");
    let resp = app.clone().oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .expect("body");
    String::from_utf8(bytes.to_vec()).expect("html")
}

/// Put one confirmed pin in the live store only — no SQLite row, exactly the state a
/// pin is in between `acquire` and the 250 ms durable flush.
fn seed_live_pin(
    store: &PinStore,
    client_key_id: ClientKeyId,
    upstream_key_id: UpstreamKeyId,
    session_hash: &str,
    canonical_id: &str,
    tier: CandidatePoolTier,
    last_used_ms: i64,
) {
    store.upsert_confirmed(
        PinKey {
            client_key_id,
            canonical_id: canonical_id.to_string(),
            session_hash: session_hash.to_string(),
        },
        upstream_key_id,
        tier,
        last_used_ms,
        3_600_000,
    );
}

#[doc = "Spec: RTE-10, USG-9, Section 14 (GET /api/user/pins returns live PinStore pins)"]
#[doc = "Spec: Section 14.t.1"]
#[tokio::test]
async fn a_pin_acquired_but_not_yet_flushed_appears_in_the_pins_view() {
    let harness = harness().await;
    let store = Arc::new(PinStore::new(1_000));
    let app = build_app(&harness, Arc::clone(&store));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    // In the live store only. `insert_personal_key` already wrote a SQLite row for
    // "shared-session-hash", which is why the assertion below is about *this* session.
    seed_live_pin(
        &store,
        harness.alice_key.client_key_id,
        harness.alice_key.upstream_key_id,
        "just-acquired-not-flushed",
        "gpt-4o",
        CandidatePoolTier::Own,
        now_ms,
    );

    let list = get_pins_json(&app, &harness.alice_key.cookie).await;
    let found = list
        .iter()
        .find(|p| p["session_hash"] == "just-acquired-not-flushed")
        .expect("a pin present in the live store must appear in the view");
    assert_eq!(
        found["canonical_id"], "gpt-4o",
        "the live entry's canonical id must be carried through"
    );
    assert_eq!(
        found["client_key_id"],
        harness.alice_key.client_key_id.to_string(),
        "the live entry's client key must be carried through"
    );
}

#[doc = "Spec: USR-3, SEC-17 — a live entry carries no owner, so merging must resolve one"]
#[doc = "Spec: Section 14.t.1"]
#[tokio::test]
async fn a_live_pin_from_another_tenant_is_never_shown() {
    let harness = harness().await;
    let store = Arc::new(PinStore::new(1_000));
    let app = build_app(&harness, Arc::clone(&store));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    // Bob's client key, bob's upstream key, a session hash only bob has.
    seed_live_pin(
        &store,
        harness.bob_key.client_key_id,
        harness.bob_key.upstream_key_id,
        "bobs-private-live-session",
        "gpt-4o",
        CandidatePoolTier::Own,
        now_ms,
    );

    let alice_list = get_pins_json(&app, &harness.alice_key.cookie).await;
    assert!(
        !alice_list
            .iter()
            .any(|p| p["session_hash"] == "bobs-private-live-session"),
        "a live pin belonging to another tenant must never be shown to alice: {alice_list:?}"
    );
    let alice_html = get_pins_partial(&app, &harness.alice_key.cookie).await;
    assert!(
        !alice_html.contains("bobs-private-live-sess"),
        "nor via the HTML partial"
    );

    // And bob does see his own — otherwise the test would pass with the merge disabled.
    let bob_list = get_pins_json(&app, &harness.bob_key.cookie).await;
    assert!(
        bob_list
            .iter()
            .any(|p| p["session_hash"] == "bobs-private-live-session"),
        "the owner must still see their own live pin; dropping everyone would also pass the \
         assertion above, so this half is what makes the test meaningful"
    );
}

#[doc = "Spec: RTE-10, Section 14.t.1 — one session is one row, whatever the source"]
#[tokio::test]
async fn a_sqlite_row_and_its_live_entry_produce_one_row() {
    let harness = harness().await;
    let store = Arc::new(PinStore::new(1_000));
    let app = build_app(&harness, Arc::clone(&store));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    let model_pk: String =
        sqlx::query_scalar("SELECT model_pk FROM models WHERE canonical_id = 'gpt-4o'")
            .fetch_one(&harness.pool)
            .await
            .expect("model_pk");

    // The same PinKey in both places: durable row is already flushed, and the live
    // entry is the newer version of the same thing.
    sqlx::query(
        "INSERT INTO pins (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier,
                           created_at, last_used_at, expires_at)
         VALUES (?, 'both-sources-session', ?, ?, 0, ?, ?, ?)",
    )
    .bind(harness.alice_key.client_key_id.to_string())
    .bind(&model_pk)
    .bind(harness.alice_key.upstream_key_id.to_string())
    .bind(now_ms - 100_000)
    .bind(now_ms - 100_000)
    .bind(now_ms + 3_600_000)
    .execute(&harness.pool)
    .await
    .expect("insert durable pin");

    seed_live_pin(
        &store,
        harness.alice_key.client_key_id,
        harness.alice_key.upstream_key_id,
        "both-sources-session",
        "gpt-4o",
        CandidatePoolTier::Own,
        now_ms,
    );

    let list = get_pins_json(&app, &harness.alice_key.cookie).await;
    let matching: Vec<&serde_json::Value> = list
        .iter()
        .filter(|p| p["session_hash"] == "both-sources-session")
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "a durable row and its live entry are two versions of one thing, not two rows: {matching:?}"
    );
}

#[doc = "Spec: Section 14.t.1 — the view is capped at 50 rows"]
#[tokio::test]
async fn the_live_view_does_not_exceed_the_row_limit() {
    let harness = harness().await;
    let store = Arc::new(PinStore::new(10_000));
    let app = build_app(&harness, Arc::clone(&store));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    // 60 live pins, plus the one SQLite row the fixture already wrote. Appending after
    // the SQL `LIMIT 50` would produce 61.
    for index in 0..60 {
        seed_live_pin(
            &store,
            harness.bob_key.client_key_id,
            harness.bob_key.upstream_key_id,
            &format!("limit-session-{index:02}"),
            "gpt-4o",
            CandidatePoolTier::Own,
            now_ms + index,
        );
    }

    let list = get_pins_json(&app, &harness.bob_key.cookie).await;
    assert!(
        list.len() <= 50,
        "the merged view must honour the 50-row cap, got {}",
        list.len()
    );
    assert_eq!(
        list.len(),
        50,
        "with 61 candidates the cap must be reached exactly, not under-filled"
    );
}

#[doc = "Spec: Section 14.t.1 — per-field precedence between the two versions of one pin"]
#[tokio::test]
async fn created_at_comes_from_sqlite_and_last_used_at_from_the_newer_of_the_two() {
    let harness = harness().await;
    let store = Arc::new(PinStore::new(1_000));
    let app = build_app(&harness, Arc::clone(&store));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    let model_pk: String =
        sqlx::query_scalar("SELECT model_pk FROM models WHERE canonical_id = 'gpt-4o'")
            .fetch_one(&harness.pool)
            .await
            .expect("model_pk");

    // SQLite: created 100 s ago, last used 100 s ago.
    sqlx::query(
        "INSERT INTO pins (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier,
                           created_at, last_used_at, expires_at)
         VALUES (?, 'precedence-session', ?, ?, 0, ?, ?, ?)",
    )
    .bind(harness.alice_key.client_key_id.to_string())
    .bind(&model_pk)
    .bind(harness.alice_key.upstream_key_id.to_string())
    .bind(now_ms - 100_000)
    .bind(now_ms - 100_000)
    .bind(now_ms + 3_600_000)
    .execute(&harness.pool)
    .await
    .expect("insert durable pin");

    // Live: last used 1 s ago, which is newer than SQLite's.
    seed_live_pin(
        &store,
        harness.alice_key.client_key_id,
        harness.alice_key.upstream_key_id,
        "precedence-session",
        "gpt-4o",
        CandidatePoolTier::Own,
        now_ms - 1_000,
    );

    let list = get_pins_json(&app, &harness.alice_key.cookie).await;
    let row = list
        .iter()
        .find(|p| p["session_hash"] == "precedence-session")
        .expect("row must exist once");

    assert_eq!(
        row["created_at"].as_i64(),
        Some(now_ms - 100_000),
        "created_at must come from SQLite: it is the durable record of when the pin was made, \
         whereas a live entry's created_ms is only when this process first saw it"
    );
    assert_eq!(
        row["last_used_at"].as_i64(),
        Some(now_ms - 1_000),
        "last_used_at must come from the store, which advances it every turn while SQLite lags \
         by up to BATCH_MAX_AGE"
    );
}

#[doc = "Spec: Section 14.t.1 — the screen renders *confirmed* pins"]
#[tokio::test]
async fn a_provisional_pin_is_not_shown_in_the_pins_view() {
    let harness = harness().await;
    let store = Arc::new(PinStore::new(1_000));
    let app = build_app(&harness, Arc::clone(&store));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    // A provisional pin: mid-handshake, may still change keys or be discarded.
    let canonical = "gpt-4o".to_string();
    let outcome = store.acquire(
        &PinKey {
            client_key_id: harness.alice_key.client_key_id,
            canonical_id: canonical.clone(),
            session_hash: "provisional-session".to_string(),
        },
        &llmroller_core::seams::SystemClock::new(),
        3_600_000,
        false,
        0,
        llmroller_core::rbac::RoutingScope::PersonalFirst,
        Some(CandidatePoolTier::Own),
        |_| true,
        |_| Some((harness.alice_key.upstream_key_id, CandidatePoolTier::Own)),
    );
    assert!(
        outcome.is_some(),
        "precondition: the acquisition must produce a provisional pin"
    );
    assert!(
        !store
            .list_active(now_ms)
            .iter()
            .any(|(_, e)| e.state == PinState::Confirmed),
        "precondition: the pin is provisional, not confirmed"
    );

    let list = get_pins_json(&app, &harness.alice_key.cookie).await;
    assert!(
        !list
            .iter()
            .any(|p| p["session_hash"] == "provisional-session"),
        "a provisional pin must not be shown: the spec's screen lists confirmed pins, and a \
         provisional one is mid-handshake and may vanish or change keys"
    );
}

#[doc = "Spec: USR-3, SEC-17 — an owner that cannot be resolved must drop the entry"]
#[tokio::test]
async fn a_live_pin_whose_client_key_is_unknown_is_dropped() {
    let harness = harness().await;
    let store = Arc::new(PinStore::new(1_000));
    let app = build_app(&harness, Arc::clone(&store));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    // A client key that exists in the store's key space but never in `client_keys`. Its
    // owner cannot be checked, so it must not be shown to anyone.
    let orphan_client_key = ClientKeyId::generate();
    seed_live_pin(
        &store,
        orphan_client_key,
        harness.alice_key.upstream_key_id,
        "orphan-owner-session",
        "gpt-4o",
        CandidatePoolTier::Own,
        now_ms,
    );

    let list = get_pins_json(&app, &harness.alice_key.cookie).await;
    assert!(
        !list
            .iter()
            .any(|p| p["session_hash"] == "orphan-owner-session"),
        "an entry whose client key does not resolve has no provable owner, so it must be \
         dropped rather than shown (fail closed)"
    );
}

#[doc = "Spec: Section 14.t.1 — the HTML partial is the same view"]
#[tokio::test]
async fn the_pins_partial_shows_a_live_pin_too() {
    let harness = harness().await;
    let store = Arc::new(PinStore::new(1_000));
    let app = build_app(&harness, Arc::clone(&store));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    seed_live_pin(
        &store,
        harness.alice_key.client_key_id,
        harness.alice_key.upstream_key_id,
        "partial-live-session-hash",
        "gpt-4o",
        CandidatePoolTier::Own,
        now_ms,
    );

    let html = get_pins_partial(&app, &harness.alice_key.cookie).await;
    assert!(
        html.contains("partial-live-"),
        "the HTML partial must show the live pin exactly as the JSON view does: {html}"
    );
}

#[doc = "Spec: Section 14.t.1 — the table and the JSON view are one view"]
#[tokio::test]
async fn both_pins_views_return_the_same_live_event_set() {
    let harness = harness().await;
    let store = Arc::new(PinStore::new(1_000));
    let app = build_app(&harness, Arc::clone(&store));
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();

    // One live entry, one durable row, and one live entry belonging to somebody else.
    seed_live_pin(
        &store,
        harness.alice_key.client_key_id,
        harness.alice_key.upstream_key_id,
        "shared-live-only-session",
        "gpt-4o",
        CandidatePoolTier::Own,
        now_ms,
    );
    seed_live_pin(
        &store,
        harness.bob_key.client_key_id,
        harness.bob_key.upstream_key_id,
        "bobs-live-only-session",
        "gpt-4o",
        CandidatePoolTier::Own,
        now_ms,
    );

    let json = get_pins_json(&app, &harness.alice_key.cookie).await;
    let html = get_pins_partial(&app, &harness.alice_key.cookie).await;

    for session_hash in ["shared-live-only-session", "shared-session-hash"] {
        assert!(
            json.iter().any(|p| p["session_hash"] == session_hash),
            "JSON view must contain {session_hash}: {json:?}"
        );
        // The partial renders a 12-char prefix, so compare on a prefix that cannot be
        // another row's.
        let prefix = &session_hash[..12];
        assert!(
            html.contains(prefix),
            "HTML view must contain the same row {session_hash} (prefix {prefix})"
        );
    }
    assert!(
        !html.contains("bobs-live-o"),
        "the HTML partial must apply the same tenant predicate as the JSON view: {html}"
    );
}
