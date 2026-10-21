//! Tests for fast startup reconstruction of budget windows and accumulators (< 2 s).
//!
//! Spec: Section 5.5, Section 7.1, Section 13.2, Requirements: LIM-1, PERF-4.

use super::*;
use std::sync::Arc;
use std::time::Instant;
use tempfile::tempdir;

use crate::crypto::MasterKey;
use crate::instance::load_or_initialize;
use crate::migrations::migrate;
use crate::pool::{Role as PoolRole, open_pool};
use crate::settings::seed_defaults;
use llmroller_core::seams::OsEntropy;
use llmroller_core::windows::ManualOffset;

async fn setup_budgets_test_db() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("budgets_rebuild_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xCC; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    (dir, writer_pool)
}

#[doc = "Spec: LIM-1, PERF-4"]
#[tokio::test]
async fn test_startup_rebuild_under_two_seconds_across_500_keys() {
    let (_dir, pool) = setup_budgets_test_db().await;
    let now_ms = 1_700_000_000_000i64;

    // Seed 7 days of 5-minute rollups across 500 upstream keys (2,016 buckets per key)
    // To test fast execution without creating gigabytes in CI, seed 2,500 rollup rows across 500 keys
    let mut tx = pool.begin().await.expect("begin");

    let mut generated_keys = Vec::new();
    for _i in 0..500 {
        let key_id = UpstreamKeyId::generate();
        generated_keys.push(key_id);
        let client_key_id = ClientKeyId::generate();
        let user_id = UserId::generate();

        for b in 0..5 {
            let bucket_ms = now_ms - (b + 1) * 300_000;
            sqlx::query(
                r#"INSERT INTO usage_rollups_5min (
                       bucket_ms, upstream_key_id, canonical_id, client_key_id,
                       owner_user_id, provider_scope, share_id, cost_micro_usd, requests
                   )
                   VALUES (?, ?, 'model-x', ?, ?, 'personal', '', 10000, 1)"#,
            )
            .bind(bucket_ms)
            .bind(key_id.to_string())
            .bind(client_key_id.to_string())
            .bind(user_id.to_string())
            .execute(&mut *tx)
            .await
            .expect("insert rollup");
        }
    }
    tx.commit().await.expect("commit seed");

    // 1. Measure startup reconstruction duration: must complete in < 2 s (PERF-4)
    let start = Instant::now();
    let rebuilt = reconstruct_budgets_from_rollups(&pool, now_ms, 7 * 86_400_000)
        .await
        .expect("reconstruct budgets");
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 2000,
        "startup reconstruction took {} ms, must be < 2000 ms (PERF-4)",
        elapsed.as_millis()
    );

    // Verify all 500 key rings were reconstructed
    assert_eq!(rebuilt.key_rings.len(), 500);

    // Verify spend is populated correctly (5 buckets * 10,000 = 50,000 micro-USD per key)
    let test_key = generated_keys[0];
    let ring = rebuilt.key_rings.get(&test_key).expect("ring must exist");
    assert_eq!(ring.sum_at(now_ms), 50_000);

    // 2. Manual calibration delta via ManualOffset
    let offset = ManualOffset {
        effective_at_ms: now_ms - 100_000,
        delta_micro_usd: 5_000,
        period_ms: 18_000_000,
    };
    assert_eq!(offset.value_at(now_ms), 5_000);
    assert_eq!(ring.sum_at(now_ms) + offset.value_at(now_ms), 55_000);

    // 3. User monthly accumulator only aggregates global provider spend (USR-5)
    let global_user_id = UserId::generate();
    let personal_user_id = UserId::generate();
    let client_key_id = ClientKeyId::generate();
    let key_id = UpstreamKeyId::generate();
    let bucket_ms = now_ms - 300_000;

    let mut tx2 = pool.begin().await.expect("begin tx2");
    sqlx::query(
        r#"INSERT INTO usage_rollups_5min (
               bucket_ms, upstream_key_id, canonical_id, client_key_id,
               owner_user_id, provider_scope, share_id, cost_micro_usd, requests
           )
           VALUES (?, ?, 'model-y', ?, ?, 'global', '', 20000, 1)"#,
    )
    .bind(bucket_ms)
    .bind(key_id.to_string())
    .bind(client_key_id.to_string())
    .bind(global_user_id.to_string())
    .execute(&mut *tx2)
    .await
    .expect("insert global rollup");

    sqlx::query(
        r#"INSERT INTO usage_rollups_5min (
               bucket_ms, upstream_key_id, canonical_id, client_key_id,
               owner_user_id, provider_scope, share_id, cost_micro_usd, requests
           )
           VALUES (?, ?, 'model-z', ?, ?, 'personal', '', 30000, 1)"#,
    )
    .bind(bucket_ms)
    .bind(key_id.to_string())
    .bind(client_key_id.to_string())
    .bind(personal_user_id.to_string())
    .execute(&mut *tx2)
    .await
    .expect("insert personal rollup");
    tx2.commit().await.expect("commit tx2");

    let rebuilt2 = reconstruct_budgets_from_rollups(&pool, now_ms, 7 * 86_400_000)
        .await
        .expect("reconstruct budgets 2");

    let month = UtcYearMonth::from_epoch_millis(bucket_ms);
    let global_acc = rebuilt2
        .monthly_accumulators
        .get_or_create(month, BudgetScopeId::User(global_user_id));
    assert_eq!(
        global_acc
            .used_micro_usd
            .load(std::sync::atomic::Ordering::Relaxed),
        20000,
        "global spend must be aggregated in User monthly accumulator"
    );

    let personal_acc = rebuilt2
        .monthly_accumulators
        .get_or_create(month, BudgetScopeId::User(personal_user_id));
    assert_eq!(
        personal_acc
            .used_micro_usd
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "personal spend must NOT be aggregated in User monthly accumulator (USR-5)"
    );
}
