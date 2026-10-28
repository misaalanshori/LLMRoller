//! Coverage for the maintenance tick's retention sweep (`ISSUE-0230`).
//!
//! # Why this test exists in this shape
//!
//! The maintenance sweep used to be ~25 inline statements inside a closure passed to
//! `JobSupervisor::run_supervised`, itself inside `tokio::spawn`, itself inside `run_serve`.
//! None of that is reachable from a test: there is no handle to the closure, and driving
//! `run_serve` would bind a listener and run until `SIGTERM`.
//!
//! That unreachability is *why* the defect survived. `serve.rs` swept eleven tables and silently
//! omitted `control_plane_logs`; the omission was invisible because nothing ever asserted what
//! the tick did. `run_retention_pass` — the function written to sweep it — was tested and never
//! called, so the suite was green while the table grew without bound.
//!
//! The sweep block is therefore extracted into [`run_maintenance_retention_sweep`], which takes
//! its inputs explicitly and is called from the tick. The test below drives that function
//! directly, so "the live path sweeps `control_plane_logs`" is now an executable claim rather
//! than a reading of the source.

use super::*;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};

const DAY_MS: i64 = 86_400_000;

async fn fresh_pool(name: &str) -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = open_pool(&dir.path().join(name), Role::Writer)
        .await
        .expect("open pool");
    migrate(&pool).await.expect("migrate");
    (dir, pool)
}

async fn insert_log(pool: &SqlitePool, id: &str, ts: i64) {
    sqlx::query(
        "INSERT INTO control_plane_logs
             (id, ts, request_id, workspace_scope, method, path, status, latency_ms)
         VALUES (?, ?, ?, 'global', 'GET', '/admin/api/overview', 200, 3)",
    )
    .bind(id)
    .bind(ts)
    .bind(format!("req_{id}"))
    .execute(pool)
    .await
    .expect("insert control_plane_log");
}

/// Build the settings the sweep reads. Only the two retention keys matter here.
fn settings_with(usage_events_retention_days: i64) -> llmroller_core::settings::RuntimeSettings {
    llmroller_core::settings::RuntimeSettings {
        usage_events_retention_days,
        ..Default::default()
    }
}

/// **The test that would have caught the defect.**
///
/// The live maintenance sweep must delete `control_plane_logs` rows older than the configured
/// window. Pre-fix, the identical row was left in place forever.
#[tokio::test]
#[doc = "Spec: AUD-2, A6, REL-3 — ISSUE-0230 the maintenance tick sweeps control_plane_logs"]
async fn live_retention_loop_sweeps_control_plane_logs() {
    let (_dir, pool) = fresh_pool("tick_cpl.db").await;

    let now_ms = 100 * DAY_MS;
    // Older than the 30-day default window.
    insert_log(&pool, "cpl_expired", now_ms - 40 * DAY_MS).await;
    // Well inside it.
    insert_log(&pool, "cpl_live", now_ms - 1 * DAY_MS).await;

    run_maintenance_retention_sweep(&pool, now_ms, &settings_with(30))
        .await
        .expect("maintenance sweep");

    let expired: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_plane_logs WHERE id = 'cpl_expired'")
            .fetch_one(&pool)
            .await
            .expect("count expired");
    let live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_plane_logs WHERE id = 'cpl_live'")
        .fetch_one(&pool)
        .await
        .expect("count live");

    assert_eq!(
        expired, 0,
        "the maintenance tick must sweep expired control-plane logs; leaving them is the \
         unbounded growth this issue is about"
    );
    assert_eq!(live, 1, "the sweep must not touch rows inside the window");
}

/// The window must come from `usage_events_retention_days`, as `AUD-2` and `14.l` specify.
///
/// `AUD-2`: *"retained for 30 days"*. `14.l`: *"kept for 30 days (`usage_events_retention_days =
/// 30`)"* — the spec names the setting, so the window is configured, not fixed at 30 or at the
/// dead code's 90.
#[tokio::test]
#[doc = "Spec: AUD-2, 14.l — ISSUE-0230 the control-plane window follows usage_events_retention_days"]
async fn control_plane_logs_retention_matches_the_specified_window() {
    let (_dir, pool) = fresh_pool("tick_cpl_window.db").await;

    let now_ms = 100 * DAY_MS;
    // 40 days old: expired under a 30-day window, live under a 60-day one.
    insert_log(&pool, "cpl_mid", now_ms - 40 * DAY_MS).await;

    // A 60-day window must keep it...
    run_maintenance_retention_sweep(&pool, now_ms, &settings_with(60))
        .await
        .expect("sweep at 60 days");
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_plane_logs")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(
        kept, 1,
        "a 60-day window must keep a 40-day-old row; deleting it means the window is not read"
    );

    // ...and a 30-day window must remove it. Both readings together prove the value is read
    // rather than being any particular constant.
    run_maintenance_retention_sweep(&pool, now_ms, &settings_with(30))
        .await
        .expect("sweep at 30 days");
    let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_plane_logs")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(gone, 0, "a 30-day window must delete a 40-day-old row");
}

/// The `A6` disk budget assumes a 30-day window, so 30 must actually be what the default
/// produces — not the dead code's 90.
#[tokio::test]
#[doc = "Spec: A6, AUD-2 — ISSUE-0230 the default window is 30 days, not the dead code's 90"]
async fn the_dead_codes_ninety_day_window_is_not_used() {
    let (_dir, pool) = fresh_pool("tick_cpl_ninety.db").await;

    let now_ms = 100 * DAY_MS;
    // 45 days old: expired at 30 days, but still inside the 90-day window the uncalled
    // `run_retention_pass` used.
    insert_log(&pool, "cpl_45d", now_ms - 45 * DAY_MS).await;

    let default_days = llmroller_core::settings::RuntimeSettings::default()
        .usage_events_retention_days;
    assert_eq!(default_days, 30, "the registry's default must be 30 (`AUD-2`)");

    run_maintenance_retention_sweep(&pool, now_ms, &settings_with(default_days))
        .await
        .expect("sweep");

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_plane_logs")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(
        remaining, 0,
        "a 45-day-old row must be gone under the default window; if it survives, the sweep is \
         using the dead code's 90-day cutoff instead of the configured 30"
    );
}
