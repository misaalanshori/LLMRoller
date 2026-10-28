//! The logs explorer must report the retention window actually in force (`ISSUE-0133`).
//!
//! # The defect
//!
//! `get_logs_internal` emitted `"retention_days": 30` at **two** sites — the `api`
//! (control-plane) branch and the `inference` branch — while the policy lives in
//! `RetentionPolicy.usage_events_retention_days` and is applied by the sweeper.
//!
//! # Why both sites, in one test
//!
//! Fixing one site leaves the two streams disagreeing about the same policy, which is the
//! cross-screen divergence cycles 22–24 each spent a cycle closing. A test that asserts one
//! site passes while the divergence persists, so every assertion here checks both *and* their
//! agreement.
//!
//! # Which window each stream is describing
//!
//! The two streams are not the same policy, and the response says so:
//!
//! - **`inference`** → `usage_events`, swept at `usage_events_retention_days` (the sweeper's
//!   `raw_cutoff`, `cli/serve.rs`, `retention.rs:296`). This is the configured value.
//! - **`api`** → `control_plane_logs`. **Nothing sweeps this table today**: the live
//!   maintenance loop touches `web_sessions`, `response_links`, `pins`, `trace_records`,
//!   `usage_events`, the rollups, `upstream_usage_snapshots` and `auth_failure_counters`, but
//!   not `control_plane_logs`; `run_retention_pass` — which does sweep it — has no production
//!   caller. So the honest `api` window is **unbounded**, which is what the response now says
//!   instead of claiming 30 days. Reporting a number nothing enforces is the defect.

use super::*;
use axum::http::header;

/// Build the logs endpoint response for an admin session.
async fn logs_json(
    pool: &sqlx::SqlitePool,
    master_key: MasterKey,
    dir: &std::path::Path,
    stream: &str,
    session_name: &str,
) -> serde_json::Value {
    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let auth_state = AuthAppState::new(pool.clone(), auth_config);
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let admin: String = sqlx::query_scalar("SELECT id FROM users WHERE role = 'admin' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("admin id");
    let raw_session = format!("logs-retention-session-{session_name}");
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
            user_agent: Some("logs-agent"),
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
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let api_state = ApiAppState::new(auth_state, master_key, "key-0".to_string());
    let app =
        llmroller_admin::control_plane_router_full(setup_state, api_state.auth.clone(), api_state);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/admin/api/logs?stream={stream}"))
        .header(header::COOKIE, format!("llmroller_session={raw_session}"))
        .body(Body::empty())
        .expect("req");
    let res = app.oneshot(req).await.expect("logs");
    assert_eq!(res.status(), StatusCode::OK, "logs must succeed");
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("logs json")
}

async fn set_retention(pool: &sqlx::SqlitePool, days: i64) {
    llmroller_store::settings::update_setting(
        pool,
        "usage_events_retention_days",
        &days.to_string(),
        1,
    )
    .await
    .expect("update retention");
}

/// Both streams must report the policy actually in force, and must not contradict each other
/// about the *inference* window the setting controls.
#[tokio::test]
#[doc = "Spec: AUD-2, USG-8, A6 — ISSUE-0133 both log streams report the configured retention"]
async fn logs_response_reports_the_configured_retention() {
    let (dir, pool, master_key, _admin, _inst) = setup_dashboard_test_db().await;

    const DAYS: i64 = 61;
    set_retention(&pool, DAYS).await;

    let inference = logs_json(&pool, master_key.clone(), dir.path(), "inference", "inf").await;
    let api = logs_json(&pool, master_key, dir.path(), "api", "api").await;

    // The inference stream reads `usage_events`, which the sweeper prunes at exactly
    // `usage_events_retention_days`.
    assert_eq!(
        inference["retention_days"], DAYS,
        "the inference stream must report the configured usage_events window; got {}",
        inference["retention_days"]
    );

    // The api stream reads `control_plane_logs`, which nothing prunes. Reporting `null` is
    // the honest answer; reporting a number would claim a policy that is not in force.
    assert!(
        api["retention_days"].is_null(),
        "the api stream must report `null` (unbounded) because nothing sweeps \
         control_plane_logs; got {}",
        api["retention_days"]
    );

    // The two must not both claim 30 as a hardcoded constant, which is what the bug did.
    assert_ne!(
        inference["retention_days"], 30,
        "the inference stream must not fall back to a hardcoded 30"
    );
}

/// Each stream carries the name of the table whose policy it describes.
///
/// Without this, `retention_days: null` on the api stream is ambiguous — a reader cannot tell
/// "we do not bound this table" from "the field is missing".
#[tokio::test]
#[doc = "Spec: AUD-2, USG-8 — ISSUE-0133 each stream names the retention window it describes"]
async fn logs_response_names_the_window_each_stream_describes() {
    let (dir, pool, master_key, _admin, _inst) = setup_dashboard_test_db().await;
    let inference = logs_json(&pool, master_key.clone(), dir.path(), "inference", "inf2").await;
    let api = logs_json(&pool, master_key, dir.path(), "api", "api2").await;

    assert_eq!(
        inference["retention_scope"], "usage_events",
        "the inference stream's window applies to usage_events"
    );
    assert_eq!(
        api["retention_scope"], "control_plane_logs",
        "the api stream's window applies to control_plane_logs"
    );
}

/// The reported window must move when the setting moves.
///
/// A fixture that only runs against the default cannot tell "reads the setting" from
/// "hardcodes 30": the default *is* 30.
#[tokio::test]
#[doc = "Spec: AUD-2, USG-8 — ISSUE-0133 the reported window tracks the setting"]
async fn logs_retention_tracks_the_configured_value() {
    let (dir, pool, master_key, _admin, _inst) = setup_dashboard_test_db().await;

    let baseline = logs_json(&pool, master_key.clone(), dir.path(), "inference", "tr-a").await;
    let baseline_days = baseline["retention_days"].as_i64().expect("number");

    const OTHER: i64 = 91;
    set_retention(&pool, OTHER).await;

    let after = logs_json(&pool, master_key, dir.path(), "inference", "tr-b").await;
    assert_eq!(after["retention_days"], OTHER);
    assert_ne!(
        after["retention_days"].as_i64(),
        Some(baseline_days),
        "the value must move with the setting; equal readings mean it is not being read"
    );
}
