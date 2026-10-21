//! Persistent pin preload regression tests.
//!
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#108-graceful-shutdown-and-restart
//! Requirements: RTE-10. Regression: F02.

use tempfile::tempdir;

use super::load_active_pins;
use crate::migrations::migrate;
use crate::pool::{Role as PoolRole, open_pool};

const USER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const CLIENT_KEY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const PROVIDER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const UPSTREAM_KEY_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const MODEL_PK: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";

async fn setup() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("pins_preload.db");
    let pool = open_pool(&path, PoolRole::Writer)
        .await
        .expect("writer pool must open");
    migrate(&pool).await.expect("schema must migrate");

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at)
         VALUES (?, 'alice', 'hash', 'user', 1000)",
    )
    .bind(USER_ID)
    .execute(&pool)
    .await
    .expect("insert user");

    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at)
         VALUES (?, ?, 'laptop', 'llr_abc', 'hash-client', 1000)",
    )
    .bind(CLIENT_KEY_ID)
    .bind(USER_ID)
    .execute(&pool)
    .await
    .expect("insert client key");

    sqlx::query(
        "INSERT INTO providers (id, ownership_scope, kind, name, base_url, config_json, created_at)
         VALUES (?, 'global', 'openrouter', 'global', 'https://api.example', '{}', 1000)",
    )
    .bind(PROVIDER_ID)
    .execute(&pool)
    .await
    .expect("insert provider");

    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, label, secret_enc, secret_last4, master_key_id, created_at)
         VALUES (?, ?, 'key', X'00', 'test', 'key-0', 1000)",
    )
    .bind(UPSTREAM_KEY_ID)
    .bind(PROVIDER_ID)
    .execute(&pool)
    .await
    .expect("insert upstream key");

    sqlx::query("INSERT INTO models (model_pk, canonical_id) VALUES (?, 'vendor/model')")
        .bind(MODEL_PK)
        .execute(&pool)
        .await
        .expect("insert model");

    (dir, pool)
}

async fn insert_pin(pool: &sqlx::SqlitePool, expires_at: i64) {
    sqlx::query(
        "INSERT INTO pins (
             client_key_id, session_hash, model_pk, upstream_key_id,
             pool_tier, created_at, last_used_at, expires_at, repin_count
         ) VALUES (?, 'session-hash', ?, ?, 0, 1000, 1000, ?, 2)",
    )
    .bind(CLIENT_KEY_ID)
    .bind(MODEL_PK)
    .bind(UPSTREAM_KEY_ID)
    .bind(expires_at)
    .execute(pool)
    .await
    .expect("insert pin");
}

#[tokio::test]
#[doc = "Spec: RTE-10 — F02 startup pin preload joins models.model_pk"]
async fn preloads_unexpired_pins_with_canonical_model_id() {
    let (_dir, pool) = setup().await;
    insert_pin(&pool, 5_000).await;

    let pins = load_active_pins(&pool, 2_000)
        .await
        .expect("preload query must not reference a nonexistent models column");
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].canonical_id, "vendor/model");
    assert_eq!(pins[0].pool_tier, 0);
    assert_eq!(pins[0].repin_count, 2);
}

#[tokio::test]
#[doc = "Spec: RTE-10 — F02 expired pins are excluded from preload"]
async fn excludes_expired_pins() {
    let (_dir, pool) = setup().await;
    insert_pin(&pool, 1_000).await;

    let pins = load_active_pins(&pool, 2_000)
        .await
        .expect("preload query must succeed");
    assert!(pins.is_empty());
}
