//! SQLite schema, pragma, and trigger migration tests.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#91-per-connection-pragma-builder-and-complete-31-table-sqlite-ddl-schema
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-1.md#13-sub-phase-13-cryptographic-sealer-xchacha20-poly1305-31-table-sqlite-wal-store-and-resumable-key-rotation

use std::collections::BTreeSet;

use sqlx::SqlitePool;
use tempfile::TempDir;

use super::{INITIAL_SCHEMA_SQL, MigrationError, SCHEMA_VERSION, migrate, verify_auto_vacuum};
use crate::pool::{Role, open_pool};

const USER_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const USER_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const PROVIDER_PERSONAL: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const PROVIDER_CROSS_ORIGIN: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const PROVIDER_OTHER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
const PROVIDER_GLOBAL: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
const KEY_PERSONAL: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
const KEY_OTHER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB2";
const KEY_CROSS_ORIGIN: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB3";
const KEY_GLOBAL: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB4";
const KEY_BAD_OWNER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB5";
const MODEL_PK: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB6";

async fn migrated_database() -> (TempDir, SqlitePool) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("llmroller.db");
    let pool = open_pool(&path, Role::Writer)
        .await
        .expect("writer pool must open");
    migrate(&pool).await.expect("schema must migrate");
    (directory, pool)
}

fn assert_trigger<T>(result: Result<T, sqlx::Error>, expected: &str) {
    match result {
        Ok(_) => panic!("statement must be rejected by trigger: {expected}"),
        Err(error) => {
            let message = error.to_string();
            assert!(
                message.contains(expected),
                "expected trigger error containing {expected:?}, got {message}"
            );
        }
    }
}

async fn insert_user(pool: &SqlitePool, id: &str, username: &str) {
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) \
         VALUES (?, ?, 'password-hash', 'user', 1)",
    )
    .bind(id)
    .bind(username)
    .execute(pool)
    .await
    .expect("user insert must succeed");
}

async fn insert_provider(
    pool: &SqlitePool,
    id: &str,
    ownership_scope: &str,
    owner_user_id: Option<&str>,
    name: &str,
    cross_origin_ack: i64,
) {
    sqlx::query(
        "INSERT INTO providers \
         (id, ownership_scope, owner_user_id, kind, name, base_url, config_json, \
          usage_url_cross_origin_ack, created_at) \
         VALUES (?, ?, ?, 'custom', ?, 'https://example.test', '{}', ?, 1)",
    )
    .bind(id)
    .bind(ownership_scope)
    .bind(owner_user_id)
    .bind(name)
    .bind(cross_origin_ack)
    .execute(pool)
    .await
    .expect("provider insert must succeed");
}

async fn insert_upstream_key(
    pool: &SqlitePool,
    id: &str,
    provider_id: &str,
    owner_user_id: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO upstream_keys \
         (id, provider_id, owner_user_id, label, secret_enc, master_key_id, secret_last4, created_at) \
         VALUES (?, ?, ?, 'key', X'00', 'mk_default', '0000', 1)",
    )
    .bind(id)
    .bind(provider_id)
    .bind(owner_user_id)
    .execute(pool)
    .await
    .expect("upstream key insert must succeed");
}

async fn personal_share_fixture(pool: &SqlitePool) {
    insert_user(pool, USER_A, "alice").await;
    insert_user(pool, USER_B, "bob").await;

    insert_provider(
        pool,
        PROVIDER_PERSONAL,
        "personal",
        Some(USER_A),
        "personal-a",
        0,
    )
    .await;
    insert_provider(
        pool,
        PROVIDER_CROSS_ORIGIN,
        "personal",
        Some(USER_A),
        "personal-cross",
        1,
    )
    .await;
    insert_provider(
        pool,
        PROVIDER_OTHER,
        "personal",
        Some(USER_A),
        "personal-other",
        0,
    )
    .await;
    insert_provider(pool, PROVIDER_GLOBAL, "global", None, "global-pool", 0).await;

    insert_upstream_key(pool, KEY_PERSONAL, PROVIDER_PERSONAL, Some(USER_A)).await;
    insert_upstream_key(pool, KEY_OTHER, PROVIDER_OTHER, Some(USER_A)).await;
    insert_upstream_key(pool, KEY_CROSS_ORIGIN, PROVIDER_CROSS_ORIGIN, Some(USER_A)).await;
    insert_upstream_key(pool, KEY_GLOBAL, PROVIDER_GLOBAL, None).await;
}

async fn insert_share(
    pool: &SqlitePool,
    id: &str,
    provider_id: &str,
    upstream_key_id: Option<&str>,
    owner_user_id: &str,
    grantee_user_id: &str,
) {
    sqlx::query(
        "INSERT INTO provider_shares \
         (id, provider_id, upstream_key_id, owner_user_id, grantee_user_id, created_at) \
         VALUES (?, ?, ?, ?, ?, 1)",
    )
    .bind(id)
    .bind(provider_id)
    .bind(upstream_key_id)
    .bind(owner_user_id)
    .bind(grantee_user_id)
    .execute(pool)
    .await
    .expect("valid share insert must succeed");
}

#[tokio::test]
#[doc = "Spec: SEC-17"]
async fn schema_creates_exactly_33_application_tables_and_expected_indexes() {
    let (_directory, pool) = migrated_database().await;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("table names must be readable");
    let tables: BTreeSet<String> = tables.into_iter().collect();

    let expected: BTreeSet<&str> = [
        "_llmroller_schema",
        "instance_meta",
        "instance_secrets",
        "users",
        "deleted_users",
        "web_sessions",
        "service_accounts",
        "client_keys",
        "providers",
        "upstream_keys",
        "provider_shares",
        "models",
        "model_redirects",
        "model_aliases",
        "model_routes",
        "route_protocol_blocks",
        "user_route_preferences",
        "budget_windows",
        "budget_adjustments",
        "upstream_usage_snapshots",
        "prices",
        "pins",
        "response_links",
        "usage_events",
        "usage_attempts",
        "usage_rollups_5min",
        "usage_rollups_hourly",
        "auth_failure_counters",
        "trace_blocks",
        "trace_records",
        "trace_record_blocks",
        "audit_log",
        "control_plane_logs",
        "settings",
    ]
    .into_iter()
    .collect();
    let actual: BTreeSet<&str> = tables.iter().map(String::as_str).collect();
    assert_eq!(actual, expected);
    assert_eq!(tables.len() - 1, 33, "33 application tables plus schema");

    let triggers: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'trigger' ORDER BY name")
            .fetch_all(&pool)
            .await
            .expect("trigger names must be readable");
    let triggers: BTreeSet<String> = triggers.into_iter().collect();
    let expected_triggers: BTreeSet<&str> = [
        "trg_client_keys_hash_disjoint_ins",
        "trg_client_keys_hash_disjoint_upd",
        "trg_client_keys_sa_owner_ins",
        "trg_client_keys_sa_owner_upd",
        "trg_upstream_keys_owner_ins",
        "trg_upstream_keys_owner_upd",
        "trg_shares_integrity_ins",
        "trg_shares_integrity_upd",
        "trg_alias_not_canonical",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        triggers.iter().map(String::as_str).collect::<BTreeSet<_>>(),
        expected_triggers
    );

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("index names must be readable");
    let indexes: BTreeSet<String> = indexes.into_iter().collect();
    for required in [
        "uq_client_keys_prev_hash",
        "uq_service_accounts_owner_name",
        "uq_providers_global_name",
        "uq_providers_personal_name",
        "uq_share_key_active",
        "uq_share_provider_active",
    ] {
        assert!(
            indexes.contains(required),
            "missing partial unique index {required}"
        );
    }

    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _llmroller_schema")
        .fetch_one(&pool)
        .await
        .expect("schema version must be readable");
    assert_eq!(version, SCHEMA_VERSION);
    verify_auto_vacuum(&pool)
        .await
        .expect("incremental auto-vacuum must be enabled");
    let auto_vacuum: i64 = sqlx::query_scalar("PRAGMA auto_vacuum")
        .fetch_one(&pool)
        .await
        .expect("auto_vacuum must be readable");
    assert_eq!(auto_vacuum, 2, "INCREMENTAL auto-vacuum is enum value 2");

    migrate(&pool).await.expect("migrate must be idempotent");
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(&pool)
    .await
    .expect("table count must be readable");
    assert_eq!(count, 34);

    pool.close().await;
}

#[tokio::test]
#[doc = "Partial: SEC-17"]
async fn newer_schema_and_non_incremental_vacuum_exit_codes_are_stable() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("newer.db");
    let pool = open_pool(&path, Role::Writer)
        .await
        .expect("writer pool must open");
    sqlx::query("CREATE TABLE _llmroller_schema (version INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .expect("fake schema table");
    sqlx::query("INSERT INTO _llmroller_schema (version) VALUES (999)")
        .execute(&pool)
        .await
        .expect("fake schema version");

    let error = migrate(&pool)
        .await
        .expect_err("newer schema must be rejected");
    assert!(matches!(
        error,
        MigrationError::SchemaTooNew {
            found: 999,
            supported: SCHEMA_VERSION
        }
    ));
    assert_eq!(error.exit_code(), 14);
    pool.close().await;

    let fresh_directory = tempfile::tempdir().expect("temporary directory");
    let fresh_path = fresh_directory.path().join("fresh.db");
    let fresh = open_pool(&fresh_path, Role::Writer)
        .await
        .expect("writer pool must open");
    let error = verify_auto_vacuum(&fresh)
        .await
        .expect_err("unmigrated database is not incremental");
    assert!(matches!(
        error,
        MigrationError::AutoVacuumNotIncremental { actual: 0 }
    ));
    assert_eq!(error.exit_code(), 13);
    fresh.close().await;
}

#[tokio::test]
#[doc = "Partial: SEC-17, Partial: USR-3"]
async fn service_account_ownership_and_name_uniqueness_are_enforced() {
    let (_directory, pool) = migrated_database().await;
    insert_user(&pool, USER_A, "alice").await;
    insert_user(&pool, USER_B, "bob").await;

    sqlx::query(
        "INSERT INTO service_accounts (id, owner_user_id, name, created_at, updated_at) \
         VALUES ('sa-a', ?, 'agents', 1, 1)",
    )
    .bind(USER_A)
    .execute(&pool)
    .await
    .expect("service account insert must succeed");

    let duplicate_name = sqlx::query(
        "INSERT INTO service_accounts (id, owner_user_id, name, created_at, updated_at) \
         VALUES ('sa-dup', ?, 'agents', 1, 1)",
    )
    .bind(USER_A)
    .execute(&pool)
    .await;
    assert!(
        duplicate_name.is_err(),
        "duplicate (owner, workspace, name) must be rejected"
    );

    sqlx::query(
        "INSERT INTO client_keys \
         (id, owner_user_id, service_account_id, label, prefix, key_hash, created_at) \
         VALUES ('key-sa', ?, 'sa-a', 'sa key', 'llr_sa', 'hash-sa', 1)",
    )
    .bind(USER_A)
    .execute(&pool)
    .await
    .expect("owner-matching service account key must succeed");

    let cross_owner = sqlx::query(
        "INSERT INTO client_keys \
         (id, owner_user_id, service_account_id, label, prefix, key_hash, created_at) \
         VALUES ('key-bad', ?, 'sa-a', 'bad', 'llr_bad', 'hash-bad', 1)",
    )
    .bind(USER_B)
    .execute(&pool)
    .await;
    assert_trigger(
        cross_owner,
        "client_keys.owner_user_id must equal service_accounts.owner_user_id",
    );

    let paused: Option<i64> =
        sqlx::query_scalar("SELECT paused_at FROM client_keys WHERE id = 'key-sa'")
            .fetch_one(&pool)
            .await
            .expect("paused_at column must exist");
    assert_eq!(paused, None);

    let theme: String = sqlx::query_scalar("SELECT ui_theme FROM users WHERE id = ?")
        .bind(USER_A)
        .fetch_one(&pool)
        .await
        .expect("ui_theme column must exist");
    assert_eq!(theme, "dark");

    pool.close().await;
}

#[tokio::test]
#[doc = "Partial: SEC-17, Partial: USR-2"]
async fn client_key_hash_disjoint_trigger_rejects_cross_collisions() {
    let (_directory, pool) = migrated_database().await;
    insert_user(&pool, USER_A, "alice").await;

    sqlx::query(
        "INSERT INTO client_keys \
         (id, owner_user_id, label, prefix, key_hash, created_at) \
         VALUES ('key-a', ?, 'a', 'llr_a', 'hash-a', 1)",
    )
    .bind(USER_A)
    .execute(&pool)
    .await
    .expect("first client key");

    let collision = sqlx::query(
        "INSERT INTO client_keys \
         (id, owner_user_id, label, prefix, key_hash, prev_key_hash, created_at) \
         VALUES ('key-b', ?, 'b', 'llr_b', 'hash-b', 'hash-a', 1)",
    )
    .bind(USER_A)
    .execute(&pool)
    .await;
    assert_trigger(collision, "client key hash collision");

    sqlx::query(
        "INSERT INTO client_keys \
         (id, owner_user_id, label, prefix, key_hash, created_at) \
         VALUES ('key-c', ?, 'c', 'llr_c', 'hash-c', 1)",
    )
    .bind(USER_A)
    .execute(&pool)
    .await
    .expect("third client key");

    let update_collision =
        sqlx::query("UPDATE client_keys SET prev_key_hash = 'hash-a' WHERE id = 'key-c'")
            .execute(&pool)
            .await;
    assert_trigger(update_collision, "client key hash collision");

    pool.close().await;
}

#[tokio::test]
#[doc = "Partial: SEC-17, Partial: USR-2"]
async fn upstream_key_owner_triggers_match_provider_ownership() {
    let (_directory, pool) = migrated_database().await;
    insert_user(&pool, USER_A, "alice").await;
    insert_user(&pool, USER_B, "bob").await;
    insert_provider(
        &pool,
        PROVIDER_PERSONAL,
        "personal",
        Some(USER_A),
        "personal-a",
        0,
    )
    .await;
    insert_provider(&pool, PROVIDER_GLOBAL, "global", None, "global-pool", 0).await;
    insert_upstream_key(&pool, KEY_PERSONAL, PROVIDER_PERSONAL, Some(USER_A)).await;
    insert_upstream_key(&pool, KEY_GLOBAL, PROVIDER_GLOBAL, None).await;

    let bad_insert = sqlx::query(
        "INSERT INTO upstream_keys \
         (id, provider_id, owner_user_id, label, secret_enc, master_key_id, secret_last4, created_at) \
         VALUES (?, ?, ?, 'bad', X'00', 'mk_default', '0000', 1)",
    )
    .bind(KEY_BAD_OWNER)
    .bind(PROVIDER_PERSONAL)
    .bind(USER_B)
    .execute(&pool)
    .await;
    assert_trigger(
        bad_insert,
        "upstream_keys.owner_user_id must equal providers.owner_user_id",
    );

    let bad_update = sqlx::query("UPDATE upstream_keys SET owner_user_id = ? WHERE id = ?")
        .bind(USER_B)
        .bind(KEY_PERSONAL)
        .execute(&pool)
        .await;
    assert_trigger(
        bad_update,
        "upstream_keys.owner_user_id must equal providers.owner_user_id",
    );

    pool.close().await;
}

#[tokio::test]
#[doc = "Partial: SEC-17, Partial: USR-3, Partial: SHR-3"]
async fn share_integrity_triggers_reject_global_cross_origin_and_cross_provider_shares() {
    let (_directory, pool) = migrated_database().await;
    personal_share_fixture(&pool).await;

    insert_share(
        &pool,
        "share-valid",
        PROVIDER_PERSONAL,
        Some(KEY_PERSONAL),
        USER_A,
        USER_B,
    )
    .await;

    let global_share = sqlx::query(
        "INSERT INTO provider_shares \
         (id, provider_id, upstream_key_id, owner_user_id, grantee_user_id, created_at) \
         VALUES ('share-global', ?, ?, ?, ?, 1)",
    )
    .bind(PROVIDER_GLOBAL)
    .bind(KEY_GLOBAL)
    .bind(USER_A)
    .bind(USER_B)
    .execute(&pool)
    .await;
    assert_trigger(global_share, "invalid provider share");

    let cross_origin_share = sqlx::query(
        "INSERT INTO provider_shares \
         (id, provider_id, upstream_key_id, owner_user_id, grantee_user_id, created_at) \
         VALUES ('share-cross', ?, ?, ?, ?, 1)",
    )
    .bind(PROVIDER_CROSS_ORIGIN)
    .bind(KEY_CROSS_ORIGIN)
    .bind(USER_A)
    .bind(USER_B)
    .execute(&pool)
    .await;
    assert_trigger(cross_origin_share, "invalid provider share");

    let owner_mismatch = sqlx::query(
        "INSERT INTO provider_shares \
         (id, provider_id, upstream_key_id, owner_user_id, grantee_user_id, created_at) \
         VALUES ('share-owner', ?, ?, ?, ?, 1)",
    )
    .bind(PROVIDER_PERSONAL)
    .bind(KEY_PERSONAL)
    .bind(USER_B)
    .bind(USER_A)
    .execute(&pool)
    .await;
    assert_trigger(owner_mismatch, "invalid provider share");

    let cross_provider_key = sqlx::query(
        "INSERT INTO provider_shares \
         (id, provider_id, upstream_key_id, owner_user_id, grantee_user_id, created_at) \
         VALUES ('share-key', ?, ?, ?, ?, 1)",
    )
    .bind(PROVIDER_PERSONAL)
    .bind(KEY_OTHER)
    .bind(USER_A)
    .bind(USER_B)
    .execute(&pool)
    .await;
    assert_trigger(cross_provider_key, "invalid provider share");

    let update_to_global =
        sqlx::query("UPDATE provider_shares SET provider_id = ? WHERE id = 'share-valid'")
            .bind(PROVIDER_GLOBAL)
            .execute(&pool)
            .await;
    assert_trigger(update_to_global, "invalid provider share");

    pool.close().await;
}

#[tokio::test]
#[doc = "Partial: SEC-17, Partial: USR-3"]
async fn alias_trigger_rejects_canonical_id_collisions() {
    let (_directory, pool) = migrated_database().await;
    sqlx::query("INSERT INTO models (model_pk, canonical_id) VALUES (?, 'vendor/canonical')")
        .bind(MODEL_PK)
        .execute(&pool)
        .await
        .expect("model insert");

    let collision =
        sqlx::query("INSERT INTO model_aliases (alias, model_pk) VALUES ('vendor/canonical', ?)")
            .bind(MODEL_PK)
            .execute(&pool)
            .await;
    assert_trigger(collision, "alias collides with canonical_id");

    sqlx::query("INSERT INTO model_aliases (alias, model_pk) VALUES ('vendor/alias', ?)")
        .bind(MODEL_PK)
        .execute(&pool)
        .await
        .expect("non-canonical alias must be accepted");

    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: MDL-1, API-1"]
async fn version_one_database_upgrades_to_version_two_without_losing_rows() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("upgrade.db");
    let pool = open_pool(&path, Role::Writer)
        .await
        .expect("writer pool must open");

    // Build a genuine version-1 database: the frozen initial DDL plus its
    // version row, exactly as the previous release would have left it.
    sqlx::query("PRAGMA auto_vacuum = INCREMENTAL")
        .execute(&pool)
        .await
        .expect("incremental auto-vacuum");
    sqlx::query("VACUUM").execute(&pool).await.expect("vacuum");
    sqlx::raw_sql(INITIAL_SCHEMA_SQL)
        .execute(&pool)
        .await
        .expect("version 1 schema");
    sqlx::query("INSERT INTO _llmroller_schema (version) VALUES (1)")
        .execute(&pool)
        .await
        .expect("version 1 row");
    sqlx::query("INSERT INTO models (model_pk, canonical_id) VALUES (?, 'vendor/legacy')")
        .bind(MODEL_PK)
        .execute(&pool)
        .await
        .expect("legacy model");

    // Version 1 has no context_window column yet.
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('models')")
        .fetch_all(&pool)
        .await
        .expect("version 1 columns");
    assert!(!columns.iter().any(|name| name == "context_window"));

    migrate(&pool).await.expect("upgrade must succeed");

    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _llmroller_schema")
        .fetch_one(&pool)
        .await
        .expect("schema version");
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(version, 2);

    let (canonical_id, context_window): (String, Option<i64>) =
        sqlx::query_as("SELECT canonical_id, context_window FROM models WHERE model_pk = ?")
            .bind(MODEL_PK)
            .fetch_one(&pool)
            .await
            .expect("legacy row must survive the upgrade");
    assert_eq!(canonical_id, "vendor/legacy");
    assert_eq!(
        context_window, None,
        "unknown context windows stay NULL instead of being fabricated"
    );

    // Re-running is a no-op and must not re-apply the ALTER TABLE.
    migrate(&pool).await.expect("upgrade must be idempotent");
    pool.close().await;
}

#[tokio::test]
#[doc = "Spec: MNT-1, F065 - Migration checksum verification detects tampered migration scripts"]
async fn regression_test_f065_migration_checksum_mismatch_fails_fast() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("tampered.db");
    let pool = open_pool(&path, Role::Writer)
        .await
        .expect("writer pool must open");

    sqlx::query("PRAGMA auto_vacuum = INCREMENTAL")
        .execute(&pool)
        .await
        .expect("incremental auto-vacuum");
    sqlx::query("VACUUM").execute(&pool).await.expect("vacuum");
    sqlx::raw_sql(INITIAL_SCHEMA_SQL)
        .execute(&pool)
        .await
        .expect("version 1 schema");

    // Insert an invalid / tampered checksum for version 1
    sqlx::query("INSERT INTO _llmroller_schema (version, checksum) VALUES (1, 'tampered-sha256-digest-value')")
        .execute(&pool)
        .await
        .expect("tampered version 1 row");

    let error = migrate(&pool)
        .await
        .expect_err("migration runner must reject tampered migration checksum");
    assert!(matches!(
        error,
        MigrationError::ChecksumMismatch {
            version: 1,
            ref actual,
            ..
        } if actual == "tampered-sha256-digest-value"
    ));
    assert_eq!(error.exit_code(), 14);

    pool.close().await;
}
