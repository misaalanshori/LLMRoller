//! Bounded sweeping of `control_plane_logs` (`ISSUE-0230`).
//!
//! # Why the limit matters
//!
//! Every other sweeper in `retention.rs` takes a chunk limit (`5_000` or `1_000`), and that is
//! deliberate: they run on a 30-second tick against live tables, so a single unbounded `DELETE`
//! would hold the SQLite write lock for as long as it takes to scan and remove the backlog.
//! `REL-3` states the requirement plainly — *"a slow or locked SQLite writer must never block or
//! fail an inference request"*.
//!
//! `sweep_expired_control_plane_logs` was the exception: a bare `DELETE FROM control_plane_logs
//! WHERE ts < ?`. That was harmless only because nothing called it. The moment the live
//! maintenance loop invokes it — which is the other half of this fix — the first tick after
//! upgrade would delete months of accumulated rows in one statement.
//!
//! So the two halves must land together: the unbounded statement is safe only while dead, and
//! the call site is only safe once the statement is bounded.

use super::*;
use crate::migrations::migrate;
use crate::pool::{Role, open_pool};

/// Insert one expired control-plane log row. `ts` is the retention key.
async fn insert_log(pool: &SqlitePool, n: usize, ts: i64) {
    sqlx::query(
        "INSERT INTO control_plane_logs
             (id, ts, request_id, workspace_scope, method, path, status, latency_ms)
         VALUES (?, ?, ?, 'global', 'GET', '/admin/api/overview', 200, 3)",
    )
    .bind(format!("cpl_{n:05}"))
    .bind(ts)
    .bind(format!("req_cpl_{n:05}"))
    .execute(pool)
    .await
    .expect("insert control_plane_log");
}

async fn count_logs(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM control_plane_logs")
        .fetch_one(pool)
        .await
        .expect("count control_plane_logs")
}

async fn fresh_pool(name: &str) -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = open_pool(&dir.path().join(name), Role::Writer)
        .await
        .expect("open pool");
    migrate(&pool).await.expect("migrate");
    (dir, pool)
}

/// One call must delete at most `chunk_limit` rows, never the whole backlog.
#[tokio::test]
#[doc = "Spec: REL-3, AUD-2 — ISSUE-0230 the control-plane sweep is bounded by its chunk limit"]
async fn sweep_expired_control_plane_logs_respects_its_limit() {
    let (_dir, pool) = fresh_pool("cpl_limit.db").await;

    const LIMIT: i64 = 10;
    // More expired rows than the limit, so an unbounded DELETE is distinguishable: it would
    // remove all 25 rather than 10.
    for n in 0..25 {
        insert_log(&pool, n, 1_000).await;
    }
    // One row that must never be touched, proving the cutoff is honoured rather than the
    // sweep simply deleting whatever it finds.
    insert_log(&pool, 999, i64::MAX / 2).await;

    let cutoff = 10_000;
    let deleted = sweep_expired_control_plane_logs(&pool, cutoff, LIMIT)
        .await
        .expect("sweep");

    assert_eq!(
        deleted, LIMIT as u64,
        "one call must delete exactly `chunk_limit` rows; deleting all 25 would hold the \
         write lock for the whole backlog"
    );
    assert_eq!(
        count_logs(&pool).await,
        26 - LIMIT,
        "exactly the limit must be gone: 25 expired inserted, 10 removed, plus the live row"
    );
}

/// Repeated calls must eventually drain the backlog — the limit must not stall progress.
#[tokio::test]
#[doc = "Spec: REL-3, AUD-2 — ISSUE-0230 repeated bounded sweeps drain the backlog"]
async fn repeated_sweeps_eventually_remove_every_expired_row() {
    let (_dir, pool) = fresh_pool("cpl_drain.db").await;
    for n in 0..25 {
        insert_log(&pool, n, 1_000).await;
    }

    const LIMIT: i64 = 10;
    let mut total = 0;
    // Three calls suffice for 25 rows at 10 per call; a fourth would return 0.
    for _ in 0..4 {
        total += sweep_expired_control_plane_logs(&pool, 10_000, LIMIT)
            .await
            .expect("sweep");
    }

    assert_eq!(total, 25, "every expired row must be removed across calls");
    assert_eq!(count_logs(&pool).await, 0, "no expired rows may remain");
}

/// The cutoff must be exclusive, and rows at or after it must survive.
#[tokio::test]
#[doc = "Spec: AUD-2 — ISSUE-0230 the sweep cutoff is exclusive"]
async fn sweep_keeps_rows_at_or_after_the_cutoff() {
    let (_dir, pool) = fresh_pool("cpl_cutoff.db").await;

    insert_log(&pool, 0, 999).await; // before  → deleted
    insert_log(&pool, 1, 1_000).await; // exactly at → kept (`ts < cutoff`)
    insert_log(&pool, 2, 1_001).await; // after   → kept

    let deleted = sweep_expired_control_plane_logs(&pool, 1_000, 100)
        .await
        .expect("sweep");

    assert_eq!(deleted, 1, "only the row strictly before the cutoff goes");
    assert_eq!(count_logs(&pool).await, 2);
}
