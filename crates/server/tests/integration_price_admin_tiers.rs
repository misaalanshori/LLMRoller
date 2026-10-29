//! The admin price API must round-trip the pricing configuration the resolver actually uses.
//!
//! Spec: Section 9.3.2, Section 13.1, Section 14.2.1
//! Requirements: USG-3, PRIV-1
//!
//! `ISSUE-0163`. `GET /admin/api/prices` read only the four flat per-MTok columns while
//! `prices.tiers_json` and `prices.schedule_json` existed and were honoured by the cost resolver
//! — cycle 46 made the *usage breakdown* read them. So an admin could see a cost produced by a
//! context tier and had no way to inspect the tier, and no way to configure one. The system priced
//! with data the admin API could not round-trip.
//!
//! These tests drive the HTTP surface, because the blindness was in the projection, not the store:
//! `PUT` already plumbed `tiers`/`schedules` into `upsert_price_tx`, which already wrote
//! `tiers_json`/`schedule_json`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tempfile::TempDir;
use tower::ServiceExt as _;

use llmroller_admin::auth::AuthAppState;
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::pricing::{ContextTier, Rates, Schedule, validate_schedules, validate_tiers};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::prices::UpsertPriceParams;
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

struct Harness {
    _dir: TempDir,
    pool: sqlx::SqlitePool,
    master_key: MasterKey,
    cookie: String,
    csrf: String,
    session_hmac_secret: llmroller_core::seams::SecretBytes,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("price_admin.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let master_key = MasterKey::from_bytes([0x3E; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance load");

    let entropy = OsEntropy;
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "price_admin",
            password: "supersecretpricepass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("admin");

    // The control plane authenticates through the real session table, so the fixture mirrors it:
    // `csrf_hash` must be the SHA-256 of the token the client sends, which is how the app stores
    // it (`auth.rs:964`). A literal hash here produces a 403 that would hide the real assertions.
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let raw_token = "price-admin-session-token-0123456789";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_token.as_bytes());
    let csrf = llmroller_admin::auth::derive_session_csrf_with_secret(
        llmroller_core::seams::ExposeSecret::expose_secret(&instance_state.session_hmac_secret),
        raw_token,
    );
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf.as_bytes());
    sqlx::query(
        "INSERT INTO web_sessions (id_hash, user_id, csrf_hash, created_at, last_seen_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id_hash)
    .bind(admin.id.to_string())
    .bind(&csrf_hash)
    .bind(now_ms)
    .bind(now_ms)
    .bind(now_ms + 86_400_000)
    .execute(&pool)
    .await
    .expect("insert admin session");
    Harness {
        _dir: dir,
        pool,
        master_key,
        cookie: format!("llmroller_session={raw_token}"),
        csrf,
        session_hmac_secret: instance_state.session_hmac_secret.clone(),
    }
}

fn build_app(harness: &Harness) -> axum::Router {
    let mut auth_state = AuthAppState::new(
        harness.pool.clone(),
        llmroller_admin::auth::AuthConfig {
            dev_insecure_cookies: true,
            require_tls: false,
            trusted_proxies: vec![],
        },
    );
    auth_state.session_hmac_secret = Some(harness.session_hmac_secret.clone());
    let setup_state = SetupAppState {
        pool: harness.pool.clone(),
        expected_token: Some("c48-token".to_string()),
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
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x24; 32]));
    control_plane_router_full(setup_state, auth_state, api_state)
}

/// A tier that matches prompts above 200k tokens (`USG-3`, Section 9.3.2).
fn a_context_tier() -> ContextTier {
    ContextTier {
        min_total_input_tokens: 200_000,
        max_total_input_tokens: None,
        input_per_mtok_micro_usd: 6_000_000,
        output_per_mtok_micro_usd: 22_500_000,
        cache_read_per_mtok_micro_usd: 600_000,
        cache_write_per_mtok_micro_usd: 7_500_000,
    }
}

/// An off-peak multiplier schedule.
fn a_schedule() -> Schedule {
    Schedule {
        name: "off_peak".to_string(),
        iso_weekdays: vec![1, 2, 3, 4, 5],
        utc_minute_ranges: vec![[0, 480]],
        multiplier_num: 1,
        multiplier_den: 2,
    }
}

fn price_body(tiers: serde_json::Value, schedules: serde_json::Value) -> String {
    serde_json::json!({
        "provider_id": "",
        "provider_kind": "openrouter",
        "upstream_model_id": "vendor/tiered-model",
        "input_per_mtok_micro_usd": 3_000_000,
        "output_per_mtok_micro_usd": 15_000_000,
        "cache_read_per_mtok_micro_usd": 300_000,
        "cache_write_per_mtok_micro_usd": 3_750_000,
        "tiers": tiers,
        "schedules": schedules,
    })
    .to_string()
}

async fn get_prices(app: &axum::Router, cookie: &str) -> serde_json::Value {
    let req = Request::builder()
        .method("GET")
        .uri("/admin/api/prices")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .expect("get prices request");
    let resp = app.clone().oneshot(req).await.expect("get prices response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

async fn put_price(
    app: &axum::Router,
    cookie: &str,
    cookie_csrf: &str,
    body: String,
) -> StatusCode {
    let req = Request::builder()
        .method("PUT")
        .uri("/admin/api/prices")
        .header(header::COOKIE, cookie)
        .header("x-csrf-token", cookie_csrf.to_string())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("put price request");
    app.clone()
        .oneshot(req)
        .await
        .expect("put price response")
        .status()
}

/// Find the row for `vendor/tiered-model` in a `GET /admin/api/prices` payload.
fn row_for<'a>(payload: &'a serde_json::Value, model: &str) -> &'a serde_json::Value {
    payload
        .as_array()
        .expect("prices array")
        .iter()
        .find(|row| row["upstream_model_id"] == model)
        .unwrap_or_else(|| panic!("no price row for {model} in {payload}"))
}

// Spec: Section 9.3.2, Section 13.1 | Requirements: USG-3
#[tokio::test]
async fn an_admin_can_read_the_tiers_and_schedules_that_priced_an_event() {
    let harness = harness().await;
    let app = build_app(&harness);

    // Seed through the store, exactly as the catalogue sync does, so the test asserts the
    // projection of a row the resolver would actually use.
    let mut tx = harness.pool.begin().await.expect("begin");
    llmroller_store::prices::upsert_price_tx(
        &mut tx,
        UpsertPriceParams {
            provider_id: String::new(),
            provider_kind: "openrouter".to_string(),
            upstream_model_id: "vendor/tiered-model".to_string(),
            rates: Rates {
                input_per_mtok_micro_usd: 3_000_000,
                output_per_mtok_micro_usd: 15_000_000,
                cache_read_per_mtok_micro_usd: 300_000,
                cache_write_per_mtok_micro_usd: 3_750_000,
            },
            tiers: Some(vec![a_context_tier()]),
            schedules: Some(vec![a_schedule()]),
            source: "admin".to_string(),
            now_ms: 1_000,
        },
    )
    .await
    .expect("upsert");
    tx.commit().await.expect("commit");

    let payload = get_prices(&app, &harness.cookie).await;
    let row = row_for(&payload, "vendor/tiered-model");

    let tiers = row["tiers"].as_array().expect("tiers must be returned");
    assert_eq!(
        tiers.len(),
        1,
        "the tier that priced the event must be visible: {row}"
    );
    assert_eq!(tiers[0]["min_total_input_tokens"], 200_000);
    assert_eq!(tiers[0]["input_per_mtok_micro_usd"], 6_000_000);

    let schedules = row["schedules"]
        .as_array()
        .expect("schedules must be returned");
    assert_eq!(schedules.len(), 1, "the schedule must be visible: {row}");
    assert_eq!(schedules[0]["name"], "off_peak");
    assert_eq!(schedules[0]["multiplier_den"], 2);
}

// Spec: Section 9.3.2 | Requirements: USG-3
#[tokio::test]
async fn a_get_put_round_trip_preserves_tiers_and_schedules() {
    let harness = harness().await;
    let app = build_app(&harness);

    let status = put_price(
        &app,
        &harness.cookie,
        &harness.csrf,
        price_body(
            serde_json::json!([a_context_tier()]),
            serde_json::json!([a_schedule()]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The acceptance test: read what the API reports, write it back verbatim, read again.
    let first = get_prices(&app, &harness.cookie).await;
    let row = row_for(&first, "vendor/tiered-model").clone();
    assert!(
        row["tiers"].as_array().is_some_and(|t| t.len() == 1),
        "GET must report the tiers that were written: {row}"
    );

    let echoed = serde_json::json!({
        "provider_id": row["provider_id"],
        "provider_kind": row["provider_kind"],
        "upstream_model_id": row["upstream_model_id"],
        "input_per_mtok_micro_usd": row["input_per_mtok_micro_usd"],
        "output_per_mtok_micro_usd": row["output_per_mtok_micro_usd"],
        "cache_read_per_mtok_micro_usd": row["cache_read_per_mtok_micro_usd"],
        "cache_write_per_mtok_micro_usd": row["cache_write_per_mtok_micro_usd"],
        "tiers": row["tiers"],
        "schedules": row["schedules"],
    })
    .to_string();

    let status = put_price(&app, &harness.cookie, &harness.csrf, echoed).await;
    assert_eq!(status, StatusCode::OK);

    let second = get_prices(&app, &harness.cookie).await;
    let row_after = row_for(&second, "vendor/tiered-model");
    assert_eq!(
        row_after["tiers"], row["tiers"],
        "a GET -> PUT round trip must not alter the tiers"
    );
    assert_eq!(
        row_after["schedules"], row["schedules"],
        "a GET -> PUT round trip must not alter the schedules"
    );
}

// Spec: Section 9.3.2 | Requirements: USG-3
#[tokio::test]
async fn a_put_without_tiers_preserves_the_stored_ones() {
    let harness = harness().await;
    let app = build_app(&harness);

    // First write establishes tiers and a schedule.
    let status = put_price(
        &app,
        &harness.cookie,
        &harness.csrf,
        price_body(
            serde_json::json!([a_context_tier()]),
            serde_json::json!([a_schedule()]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A later PUT that omits both fields, like a UI that only edits the flat rates.
    let rates_only = serde_json::json!({
        "provider_id": "",
        "provider_kind": "openrouter",
        "upstream_model_id": "vendor/tiered-model",
        "input_per_mtok_micro_usd": 4_000_000,
        "output_per_mtok_micro_usd": 20_000_000,
        "cache_read_per_mtok_micro_usd": 400_000,
        "cache_write_per_mtok_micro_usd": 5_000_000,
    })
    .to_string();
    let status = put_price(&app, &harness.cookie, &harness.csrf, rates_only).await;
    assert_eq!(status, StatusCode::OK);

    let payload = get_prices(&app, &harness.cookie).await;
    let row = row_for(&payload, "vendor/tiered-model");
    assert_eq!(
        row["input_per_mtok_micro_usd"], 4_000_000,
        "the flat rate must update"
    );
    assert!(
        row["tiers"].as_array().is_some_and(|t| t.len() == 1),
        "omitting `tiers` must preserve the stored tiers, not clear them: {row}"
    );
    assert!(
        row["schedules"].as_array().is_some_and(|s| s.len() == 1),
        "omitting `schedules` must preserve the stored schedules, not clear them: {row}"
    );
}

// Spec: Section 9.3.2 | Requirements: USG-3
#[tokio::test]
async fn a_malformed_tiers_payload_is_rejected_before_write() {
    let harness = harness().await;
    let app = build_app(&harness);

    // Two overlapping tiers: rejected by `validate_tiers`, and the row must not exist afterwards.
    let overlapping = serde_json::json!([
        {
            "min_total_input_tokens": 0,
            "max_total_input_tokens": 200_000,
            "input_per_mtok_micro_usd": 3_000_000,
            "output_per_mtok_micro_usd": 15_000_000,
            "cache_read_per_mtok_micro_usd": 300_000,
            "cache_write_per_mtok_micro_usd": 3_750_000,
        },
        {
            "min_total_input_tokens": 100_000,
            "max_total_input_tokens": null,
            "input_per_mtok_micro_usd": 6_000_000,
            "output_per_mtok_micro_usd": 22_500_000,
            "cache_read_per_mtok_micro_usd": 600_000,
            "cache_write_per_mtok_micro_usd": 7_500_000,
        }
    ]);

    let status = put_price(
        &app,
        &harness.cookie,
        &harness.csrf,
        price_body(overlapping, serde_json::json!([])),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a malformed tiers payload is a 422, not a 500 and not a stored invalid row"
    );

    let stored: Option<String> = sqlx::query_scalar(
        "SELECT tiers_json FROM prices WHERE upstream_model_id = 'vendor/tiered-model'",
    )
    .fetch_optional(&harness.pool)
    .await
    .expect("query");
    assert!(
        stored.is_none(),
        "a rejected payload must not write a row: {stored:?}"
    );
}

// Spec: Section 9.3.2 | Requirements: USG-3
#[tokio::test]
async fn a_malformed_schedules_payload_is_rejected_before_write() {
    let harness = harness().await;
    let app = build_app(&harness);

    // Zero denominator: rejected by `validate_schedules`.
    let bad = serde_json::json!([{
        "name": "broken",
        "iso_weekdays": [1],
        "utc_minute_ranges": [[0, 60]],
        "multiplier_num": 1,
        "multiplier_den": 0,
    }]);

    let status = put_price(
        &app,
        &harness.cookie,
        &harness.csrf,
        price_body(serde_json::json!([]), bad),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// Spec: Section 9.3.2 | Requirements: USG-3
#[tokio::test]
async fn the_validation_the_api_relies_on_accepts_what_it_stores() {
    // The API's 422 path is `validate_tiers`/`validate_schedules`, so the fixtures above must be
    // valid/invalid for the stated reason and not by accident.
    assert!(validate_tiers(&[a_context_tier()]).is_ok());
    assert!(validate_schedules(&[a_schedule()]).is_ok());
    assert!(
        validate_schedules(&[Schedule {
            name: "broken".to_string(),
            iso_weekdays: vec![1],
            utc_minute_ranges: vec![[0, 60]],
            multiplier_num: 1,
            multiplier_den: 0,
        }])
        .is_err()
    );
}

// Spec: Section 9.3.2 | Requirements: USG-3
#[tokio::test]
async fn an_explicit_empty_array_clears_the_stored_tiers() {
    let harness = harness().await;
    let app = build_app(&harness);

    let status = put_price(
        &app,
        &harness.cookie,
        &harness.csrf,
        price_body(
            serde_json::json!([a_context_tier()]),
            serde_json::json!([a_schedule()]),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // `[]` is an explicit clear, which is why `None`-means-unchanged is safe: the operator still
    // has a way to remove a tier, and it must not be swallowed by the `COALESCE` that preserves
    // omitted fields.
    let status = put_price(
        &app,
        &harness.cookie,
        &harness.csrf,
        price_body(serde_json::json!([]), serde_json::json!([])),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let payload = get_prices(&app, &harness.cookie).await;
    let row = row_for(&payload, "vendor/tiered-model");
    assert!(
        row["tiers"].as_array().is_some_and(|t| t.is_empty()),
        "`tiers: []` must clear the stored tiers: {row}"
    );
    assert!(
        row["schedules"].as_array().is_some_and(|s| s.is_empty()),
        "`schedules: []` must clear the stored schedules: {row}"
    );

    let stored: Option<String> = sqlx::query_scalar(
        "SELECT tiers_json FROM prices WHERE upstream_model_id = 'vendor/tiered-model'",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("query");
    assert_eq!(
        stored.as_deref(),
        Some("[]"),
        "the cleared value is stored as an explicit empty array, not NULL"
    );
}

// Spec: Section 9.3.2 | Requirements: USG-3
#[tokio::test]
async fn an_unparseable_stored_tiers_column_is_reported_null_not_flattened_to_empty() {
    let harness = harness().await;
    let app = build_app(&harness);

    // A row written outside this API (a hand edit, a bad migration) whose `tiers_json` will not
    // parse. The resolver would choke on it too, so reporting "no tiers" would be an invented
    // fact: the distinction between "configured none" and "configured something unreadable" is
    // exactly what an operator debugging a surprise cost needs (`ISSUE-0163`, and cycle 46's rule
    // that a missing financial fact is reported rather than defaulted).
    sqlx::query(
        "INSERT INTO prices (provider_id, provider_kind, upstream_model_id,
             input_per_mtok_micro_usd, output_per_mtok_micro_usd,
             cache_read_per_mtok_micro_usd, cache_write_per_mtok_micro_usd,
             tiers_json, schedule_json, source, updated_at)
         VALUES ('', 'openrouter', 'vendor/corrupt', 1000, 2000, 0, 0,
                 '{not json', NULL, 'admin', 1)",
    )
    .execute(&harness.pool)
    .await
    .expect("insert corrupt row");

    let payload = get_prices(&app, &harness.cookie).await;
    let row = row_for(&payload, "vendor/corrupt");
    assert_eq!(
        row["tiers"],
        serde_json::Value::Null,
        "an unparseable stored value must be reported as null, not as an empty list: {row}"
    );
    // A NULL column, by contrast, genuinely means "none configured".
    assert_eq!(
        row["schedules"],
        serde_json::json!([]),
        "a NULL column means no schedules configured: {row}"
    );
}

// Spec: Section 9.3.3, Section 13.1 | Requirements: USG-3
#[tokio::test]
async fn there_is_no_reasoning_rate_because_reasoning_is_priced_as_output() {
    // `ISSUE-0163` asked for `reasoning_per_mtok_micro_usd` to be plumbed through the admin API.
    // That field exists nowhere: `prices` has no such column (0001_initial.sql), there is no
    // `rates_json` to hold one, and `Rates` has exactly four fields. Adding it would be a schema
    // change.
    //
    // It is also not needed. `TokenUsage::reasoning` is documented as a *diagnostic subset of
    // `output`*, and `calculate_cost_micro_usd` prices `output` — so reasoning tokens are already
    // charged at the output rate. This test pins that, so a future cycle does not add a
    // `reasoning_per_mtok` column that would double-charge the same tokens.
    let tokens = llmroller_core::pricing::TokenUsage {
        input_uncached: 0,
        cache_read: 0,
        cache_write: 0,
        output: 1_000_000,
        reasoning: 400_000, // subset of `output`, not additional to it
    };
    let rates = Rates {
        input_per_mtok_micro_usd: 1_000_000,
        output_per_mtok_micro_usd: 15_000_000,
        cache_read_per_mtok_micro_usd: 0,
        cache_write_per_mtok_micro_usd: 0,
    };
    let cost = llmroller_core::pricing::calculate_cost_micro_usd(tokens, rates, 1, 1);
    assert_eq!(
        cost, 15_000_000,
        "1M output tokens at $15/MTok is $15; the 400k reasoning subset adds nothing"
    );
}
