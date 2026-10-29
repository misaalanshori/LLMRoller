//! Budget calibration adjustments must be bound to the window instance the server computes
//! (`ISSUE-0162`).
//!
//! Spec: Section 13.2, Section 14.q
//! Requirements: LIM-1, F-064

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tempfile::TempDir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig, derive_session_csrf_with_secret};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::ids::{ClientKeyId, ProviderId, UpstreamKeyId};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, ExposeSecret as _, OsEntropy};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};

struct Harness {
    _dir: TempDir,
    pool: sqlx::SqlitePool,
    master_key: MasterKey,
    session_hmac_secret: llmroller_core::seams::SecretBytes,
    cookie: String,
    csrf: String,
    upstream_key_id: UpstreamKeyId,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("budget_stamp.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let master_key = MasterKey::from_bytes([0x5A; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance load");

    let entropy = OsEntropy;
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let owner = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "stamp_owner",
            password: "supersecretstamppass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("owner");

    let client_key_id = ClientKeyId::generate();
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, prefix, key_hash, label, routing_scope, created_at)
         VALUES (?, ?, 'llr_stamp', ?, 'stamp', 'personal_first', 1)",
    )
    .bind(client_key_id.to_string())
    .bind(owner.id.to_string())
    .bind(format!("hash-{client_key_id}"))
    .execute(&pool)
    .await
    .expect("insert client key");

    let provider_id = ProviderId::generate();
    sqlx::query(
        "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope, created_at)
         VALUES (?, 'stamp-provider', 'custom', 'https://example.test', '{}', 'global', 1)",
    )
    .bind(provider_id.to_string())
    .execute(&pool)
    .await
    .expect("insert provider");

    let upstream_key_id = UpstreamKeyId::generate();
    sqlx::query(
        "INSERT INTO upstream_keys
         (id, provider_id, label, secret_enc, master_key_id, secret_last4, created_at)
         VALUES (?, ?, 'stamp-upstream', X'00', 'key-0', '1234', 1)",
    )
    .bind(upstream_key_id.to_string())
    .bind(provider_id.to_string())
    .execute(&pool)
    .await
    .expect("insert upstream key");

    let raw_token = "stamp-session-token-0123456789";
    let id_hash = llmroller_store::crypto::sha256_hex(raw_token.as_bytes());
    let csrf = derive_session_csrf_with_secret(
        instance_state.session_hmac_secret.expose_secret(),
        raw_token,
    );
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf.as_bytes());
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: &id_hash,
            user_id: owner.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("c48"),
            created_at: llmroller_core::seams::SystemClock::new().now_ms(),
            expires_at: llmroller_core::seams::SystemClock::new().now_ms() + 86_400_000,
        },
    )
    .await
    .expect("insert web session");

    Harness {
        _dir: dir,
        pool,
        master_key,
        session_hmac_secret: instance_state.session_hmac_secret.clone(),
        cookie: format!("llmroller_session={raw_token}"),
        csrf,
        upstream_key_id,
    }
}

fn build_app(harness: &Harness) -> axum::Router {
    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(harness.pool.clone(), auth_config);
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

/// Create an `anchored_calendar_month` window owned by the fixture's upstream key, returning its id.
async fn create_window(harness: &Harness, anchor_at: i64) -> String {
    sqlx::query(
        "INSERT INTO budget_windows
         (id, scope_kind, upstream_key_id, window_kind, period_ms, anchor_at, limit_micro_usd, reserve_pct)
         VALUES (?, 'upstream_key', ?, 'anchored_calendar_month', NULL, ?, 5000000, 5)",
    )
    .bind("win_calendar")
    .bind(harness.upstream_key_id.to_string())
    .bind(anchor_at)
    .execute(&harness.pool)
    .await
    .expect("insert window");
    "win_calendar".to_string()
}

fn adjustment_request(harness: &Harness, window_id: &str, delta: i64) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/admin/api/budget-windows/{window_id}/adjustments"))
        .header(header::COOKIE, &harness.cookie)
        .header("x-csrf-token", &harness.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            r#"{{"delta_micro_usd": {delta}, "note": "calibration"}}"#
        )))
        .expect("adjustment request")
}

// Spec: Section 13.2, Section 14.q | Requirements: LIM-1, F-064
#[tokio::test]
async fn a_calibration_is_stamped_with_the_calendar_month_instance_start() {
    let harness = harness().await;
    // 31 January anchor: February clamps to the 28th, so April 17 belongs to the instance that
    // started on 31 March.
    let anchor_at = llmroller_core::windows::utc_to_epoch_millis(2026, 1, 31, 0);
    let window_id = create_window(&harness, anchor_at).await;
    let app = build_app(&harness);

    let clock = llmroller_core::seams::SystemClock::new();
    let before = clock.now_ms();
    let resp = app
        .clone()
        .oneshot(adjustment_request(&harness, &window_id, 250_000))
        .await
        .expect("adjustment response");
    let after = clock.now_ms();
    assert_eq!(resp.status(), StatusCode::OK);

    let stored: Option<i64> =
        sqlx::query_scalar("SELECT window_instance_start FROM budget_adjustments WHERE id = ?")
            .bind(
                sqlx::query_scalar::<_, String>(
                    "SELECT id FROM budget_adjustments ORDER BY created_at DESC LIMIT 1",
                )
                .fetch_one(&harness.pool)
                .await
                .expect("adjustment id"),
            )
            .fetch_one(&harness.pool)
            .await
            .expect("window_instance_start");

    // The handler derives from its own clock, so the expected value is read from the clock
    // either side of the request rather than assumed. The exact calendar arithmetic is pinned by
    // `windows::tests::a_calendar_month_instance_start_is_the_clamped_month_the_anchor_selects`
    // and `..._matches_the_live_counters_for_every_window_kind`; what this test pins is the
    // wiring: that a value is derived at all, and that it is an instance start rather than NULL.
    let derived = |now_ms: i64| {
        llmroller_core::windows::current_instance_start_ms(
            llmroller_core::windows::WindowKind::AnchoredCalendarMonth,
            None,
            Some(anchor_at),
            now_ms,
        )
    };
    let stored =
        stored.expect("the server must bind the calibration to an instance, not store NULL");
    assert!(
        Some(stored) == derived(before) || Some(stored) == derived(after),
        "stored {stored} is not the instance start the handler could have derived \
         ({:?} .. {:?})",
        derived(before),
        derived(after)
    );
    // A 31 January anchor means every instance starts on the 31st or the month's last day, so the
    // stamp must be a month boundary rather than, say, the adjustment's own timestamp.
    let (year, month, _day, _minute) = llmroller_core::windows::epoch_millis_to_utc(stored);
    let clamped_day = 31_u8.min(llmroller_core::windows::days_in_month(year, month));
    assert_eq!(
        stored,
        llmroller_core::windows::utc_to_epoch_millis(year, month, clamped_day, 0),
        "the stamp must be a month-instance boundary for the 31 January anchor"
    );
}

// Spec: Section 13.2 | Requirements: LIM-1, F-064
#[tokio::test]
async fn a_calibration_on_a_rolling_window_is_stamped_null() {
    let harness = harness().await;
    sqlx::query(
        "INSERT INTO budget_windows
         (id, scope_kind, upstream_key_id, window_kind, period_ms, limit_micro_usd, reserve_pct)
         VALUES ('win_rolling', 'upstream_key', ?, 'rolling', 3600000, 5000000, 5)",
    )
    .bind(harness.upstream_key_id.to_string())
    .execute(&harness.pool)
    .await
    .expect("insert rolling window");
    let app = build_app(&harness);

    let resp = app
        .clone()
        .oneshot(adjustment_request(&harness, "win_rolling", 100_000))
        .await
        .expect("adjustment response");
    assert_eq!(resp.status(), StatusCode::OK);

    let stored: Option<i64> =
        sqlx::query_scalar("SELECT window_instance_start FROM budget_adjustments")
            .fetch_one(&harness.pool)
            .await
            .expect("window_instance_start");
    assert_eq!(
        stored, None,
        "a rolling window has no instance boundary, so the adjustment must keep the rolling \
         lookback rather than being pinned to an instance"
    );
}

// Spec: Section 13.2, Section 14.q | Requirements: LIM-1, F-064
#[tokio::test]
async fn a_client_supplied_instance_start_is_ignored_in_favour_of_the_server_derivation() {
    let harness = harness().await;
    let anchor_at = llmroller_core::windows::utc_to_epoch_millis(2026, 1, 31, 0);
    let window_id = create_window(&harness, anchor_at).await;
    let app = build_app(&harness);

    // A caller-supplied instance start would let a calibration be scoped to an arbitrary period,
    // which `13. SRDD:82` does not permit: applicability is "bound to the current instance".
    let bogus = 1_i64;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/admin/api/budget-windows/{window_id}/adjustments"))
        .header(header::COOKIE, &harness.cookie)
        .header("x-csrf-token", &harness.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(
            r#"{{"delta_micro_usd": 111, "window_instance_start": {bogus}}}"#
        )))
        .expect("adjustment request");

    let resp = app.clone().oneshot(req).await.expect("adjustment response");
    assert_eq!(resp.status(), StatusCode::OK);

    let stored: Option<i64> =
        sqlx::query_scalar("SELECT window_instance_start FROM budget_adjustments")
            .fetch_one(&harness.pool)
            .await
            .expect("window_instance_start");
    assert_ne!(
        stored,
        Some(bogus),
        "a client-supplied instance start must not be trusted; applicability depends on it"
    );
    let stored = stored.expect("the server must still derive an instance start");
    let (year, month, _day, _minute) = llmroller_core::windows::epoch_millis_to_utc(stored);
    let clamped_day = 31_u8.min(llmroller_core::windows::days_in_month(year, month));
    assert_eq!(
        stored,
        llmroller_core::windows::utc_to_epoch_millis(year, month, clamped_day, 0),
        "the server-derived month boundary must win over the caller's value"
    );
}
