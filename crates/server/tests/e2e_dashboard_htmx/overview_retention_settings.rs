//! `/admin/api/overview` must report the *configured* retention figures (`ISSUE-0132`).
//!
//! # The defect
//!
//! The handler emitted `"max_usage_events_rows": 50_000_000` as a literal while the
//! configured value lives in the 14-key registry (`usage_events_max_rows`) and is already
//! loaded into `RuntimeSettings` by the time the handler runs.
//!
//! # Why a wrong number is worse than no number
//!
//! `A6` has the operator reason about disk growth as
//! `daily_requests * retention_days * 750 B * 1.3`, and this endpoint exists to make that
//! legible. Reporting a policy that may not be in force means an operator sizing a volume
//! from the dashboard gets the wrong answer — and a *displayed* wrong number invites a
//! decision, where an absent one does not.
//!
//! # Why these tests cannot run against defaults
//!
//! `usage_events_max_rows` defaults to exactly `50_000_000` and `usage_events_retention_days`
//! to `30`. A test that leaves them alone cannot tell "reads the setting" from "hardcodes the
//! default" — the cycle-28 fixture-can't-discriminate class. Every test here sets a
//! distinctive value first.

use super::*;
use axum::http::header;

async fn set_setting(pool: &sqlx::SqlitePool, key: &str, value_json: &str) {
    llmroller_store::settings::update_setting(pool, key, value_json, 1)
        .await
        .expect("update setting");
}

/// Build the admin overview response for an admin session.
async fn overview(
    pool: &sqlx::SqlitePool,
    master_key: MasterKey,
    dir: &std::path::Path,
    session_name: &str,
) -> serde_json::Value {
    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let admin: String = sqlx::query_scalar("SELECT id FROM users WHERE role = 'admin' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("admin id");
    let raw_session = format!("overview-settings-session-{session_name}");
    let id_hash = llmroller_store::crypto::sha256_hex(raw_session.as_bytes());
    let csrf = auth_state.derive_session_csrf(&raw_session);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf.as_bytes());
    llmroller_store::users::insert_web_session(
        pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: admin.parse().expect("parse user id"),
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("overview-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("insert session");

    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state, master_key, "key-0".to_string());
    let app =
        llmroller_admin::control_plane_router_full(setup_state, api_state.auth.clone(), api_state);

    let req = Request::builder()
        .method("GET")
        .uri("/admin/api/overview")
        .header(header::COOKIE, format!("llmroller_session={raw_session}"))
        .body(Body::empty())
        .expect("req");
    let res = app.oneshot(req).await.expect("overview");
    assert_eq!(res.status(), StatusCode::OK, "overview must succeed");
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("overview json")
}

/// The row cap must come from `usage_events_max_rows`, not a literal.
#[tokio::test]
#[doc = "Spec: A6, UI-2 — ISSUE-0132 the overview reports the configured row cap"]
async fn overview_reports_the_configured_row_cap() {
    let (dir, pool, master_key, _admin, _inst) = setup_dashboard_test_db().await;

    // Distinctive, non-default, and above the registry's 1_000_000 floor.
    const CAP: i64 = 7_654_321;
    set_setting(&pool, "usage_events_max_rows", &CAP.to_string()).await;

    let json = overview(&pool, master_key, dir.path(), "cap").await;
    assert_eq!(
        json["max_usage_events_rows"], CAP,
        "the overview must report the configured row cap, not the 50_000_000 default; \
         got {}",
        json["max_usage_events_rows"]
    );
}

/// The retention window must come from `usage_events_retention_days`.
#[tokio::test]
#[doc = "Spec: A6, UI-2 — ISSUE-0132 the overview reports the configured retention window"]
async fn overview_reports_the_configured_retention() {
    let (dir, pool, master_key, _admin, _inst) = setup_dashboard_test_db().await;

    // Run it once at the default first, so the second reading is provably a change.
    let baseline = overview(&pool, master_key.clone(), dir.path(), "ret-baseline").await;
    let baseline_days = baseline["retention_days"]
        .as_i64()
        .expect("retention_days is a number");

    const DAYS: i64 = 47;
    set_setting(&pool, "usage_events_retention_days", &DAYS.to_string()).await;

    let json = overview(&pool, master_key, dir.path(), "ret-configured").await;
    assert_eq!(
        json["retention_days"], DAYS,
        "the overview must report the configured retention window"
    );
    assert_ne!(
        json["retention_days"].as_i64(),
        Some(baseline_days),
        "the value must move when the setting moves; if both readings agree, the handler \
         is not reading the setting at all"
    );
}

/// The projected disk figure must be computed from the configured window too.
///
/// `projected_bytes = daily_requests * retention_days * 600 * 1.3 + trace_bytes`. A handler
/// that reported the configured `retention_days` but projected with a different window would
/// be self-inconsistent in the same card — the number an operator actually sizes from.
#[tokio::test]
#[doc = "Spec: A6, UI-2 — ISSUE-0132 the disk projection uses the configured window"]
async fn overview_disk_projection_tracks_the_configured_retention() {
    let (dir, pool, master_key, _admin, _inst) = setup_dashboard_test_db().await;

    let short = overview(&pool, master_key.clone(), dir.path(), "disk-short").await;
    let short_mb = short["projected_disk_mb"]
        .as_f64()
        .expect("projected_disk_mb is a number");

    // A much longer window must project strictly more disk for the same request rate.
    const LONG_DAYS: i64 = 120;
    set_setting(&pool, "usage_events_retention_days", &LONG_DAYS.to_string()).await;

    let long = overview(&pool, master_key, dir.path(), "disk-long").await;
    let long_mb = long["projected_disk_mb"]
        .as_f64()
        .expect("projected_disk_mb is a number");

    assert!(
        long_mb > short_mb,
        "a {LONG_DAYS}-day window must project more disk than the default; got {long_mb} MB \
         vs {short_mb} MB — equal or smaller means the projection ignored the setting"
    );
}
