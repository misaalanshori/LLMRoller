//! Per-role SQLite connection pragma tests.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#91-per-connection-pragma-builder-and-complete-31-table-sqlite-ddl-schema
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md
//! Phase: ./docs/bigbang/big-bang-phases-1.md#13-sub-phase-13-cryptographic-sealer-xchacha20-poly1305-31-table-sqlite-wal-store-and-resumable-key-rotation

use std::path::Path;
use std::time::Duration;

use sqlx::{AssertSqlSafe, SqlitePool};
use tempfile::TempDir;

use super::{CliMode, Role, open_pool};
use crate::migrations::migrate;

async fn migrated_database() -> (TempDir, SqlitePool) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("llmroller.db");
    let pool = open_pool(&path, Role::Writer)
        .await
        .expect("writer pool must open");
    migrate(&pool).await.expect("schema must migrate");
    (directory, pool)
}

async fn pragma_i64(pool: &SqlitePool, pragma: &str) -> i64 {
    let sql = format!("PRAGMA {pragma}");
    sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .expect("integer pragma must be readable")
}

async fn pragma_text(pool: &SqlitePool, pragma: &str) -> String {
    let sql = format!("PRAGMA {pragma}");
    sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .expect("text pragma must be readable")
}

#[tokio::test]
#[doc = "Spec: SEC-17"]
async fn per_connection_pragmas_match_the_role_matrix() {
    let (directory, writer) = migrated_database().await;
    let path = directory.path().join("llmroller.db");

    assert_eq!(pragma_text(&writer, "journal_mode").await, "wal");
    assert_eq!(pragma_i64(&writer, "synchronous").await, 1);
    assert_eq!(pragma_i64(&writer, "foreign_keys").await, 1);
    assert_eq!(
        pragma_i64(&writer, "busy_timeout").await,
        i64::try_from(Duration::from_secs(5).as_millis()).expect("timeout fits i64")
    );
    assert_eq!(pragma_i64(&writer, "query_only").await, 0);
    assert_eq!(pragma_i64(&writer, "temp_store").await, 2);
    assert_eq!(pragma_i64(&writer, "journal_size_limit").await, 67_108_864);

    let reader = open_pool(&path, Role::Reader)
        .await
        .expect("reader pool must open");
    assert_eq!(pragma_text(&reader, "journal_mode").await, "wal");
    assert_eq!(pragma_i64(&reader, "foreign_keys").await, 1);
    assert_eq!(
        pragma_i64(&reader, "busy_timeout").await,
        i64::try_from(Duration::from_secs(5).as_millis()).expect("timeout fits i64")
    );
    assert_eq!(pragma_i64(&reader, "query_only").await, 1);
    assert_eq!(pragma_i64(&reader, "temp_store").await, 2);

    let cli_write = open_pool(&path, Role::Cli(CliMode::ReadWrite))
        .await
        .expect("read-write CLI pool must open");
    assert_eq!(pragma_text(&cli_write, "journal_mode").await, "wal");
    assert_eq!(pragma_i64(&cli_write, "synchronous").await, 2);
    assert_eq!(pragma_i64(&cli_write, "foreign_keys").await, 1);
    assert_eq!(
        pragma_i64(&cli_write, "busy_timeout").await,
        i64::try_from(Duration::from_secs(30).as_millis()).expect("timeout fits i64")
    );
    assert_eq!(pragma_i64(&cli_write, "query_only").await, 0);
    assert_eq!(pragma_i64(&cli_write, "temp_store").await, 2);
    assert_eq!(
        pragma_i64(&cli_write, "journal_size_limit").await,
        67_108_864
    );

    let cli_read = open_pool(&path, Role::Cli(CliMode::ReadOnly))
        .await
        .expect("read-only CLI pool must open");
    assert_eq!(pragma_i64(&cli_read, "synchronous").await, 2);
    assert_eq!(pragma_i64(&cli_read, "query_only").await, 1);
    assert_eq!(
        pragma_i64(&cli_read, "busy_timeout").await,
        i64::try_from(Duration::from_secs(30).as_millis()).expect("timeout fits i64")
    );

    cli_read.close().await;
    cli_write.close().await;
    reader.close().await;
    writer.close().await;
}

#[tokio::test]
#[doc = "Spec: SEC-17"]
async fn writer_and_reader_pool_sizes_are_fixed_by_role() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("pool-size.db");
    let writer = open_pool(&path, Role::Writer)
        .await
        .expect("writer pool must open");

    assert_eq!(writer.options().get_max_connections(), 1);
    migrate(&writer).await.expect("schema must migrate");

    let reader = open_pool(&path, Role::Reader)
        .await
        .expect("reader pool must open");
    assert_eq!(reader.options().get_max_connections(), 8);

    let cli = open_pool(&path, Role::Cli(CliMode::ReadWrite))
        .await
        .expect("CLI pool must open");
    assert_eq!(cli.options().get_max_connections(), 1);

    cli.close().await;
    reader.close().await;
    writer.close().await;
}

#[tokio::test]
#[doc = "Partial: SEC-17"]
async fn opening_a_missing_reader_database_fails_instead_of_creating_it() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("missing.db");

    let error = open_pool(&missing, Role::Reader)
        .await
        .expect_err("readers must not create a missing database");
    assert!(
        !Path::new(&missing).exists(),
        "reader must not create the file"
    );
    assert!(
        error.to_string().to_lowercase().contains("unable to open"),
        "unexpected reader error: {error}"
    );
}
