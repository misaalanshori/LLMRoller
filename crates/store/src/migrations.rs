#![forbid(unsafe_code)]
//! Forward-only SQLite schema migrations.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#91-per-connection-pragma-builder-and-complete-31-table-sqlite-ddl-schema

use sqlx::{Connection, SqlitePool};
use thiserror::Error;

/// Current schema version understood by this build.
pub const SCHEMA_VERSION: i64 = 1;

const INITIAL_SCHEMA_SQL: &str = include_str!("../migrations/0001_initial.sql");

/// Schema migration failures with their required startup exit codes.
#[derive(Debug, Error)]
pub enum MigrationError {
    /// The database was written by a newer LLMRoller build.
    #[error("database schema version {found} is newer than supported version {supported}")]
    SchemaTooNew {
        /// Version found in `_llmroller_schema`.
        found: i64,
        /// Highest version this build understands.
        supported: i64,
    },
    /// The database does not use incremental auto-vacuum.
    #[error("database auto_vacuum mode is {actual}, expected 2 (INCREMENTAL)")]
    AutoVacuumNotIncremental {
        /// Value returned by `PRAGMA auto_vacuum`.
        actual: i64,
    },
    /// The database has tables but no usable `_llmroller_schema` version row.
    #[error("database contains an unrecognized or unsupported schema")]
    UnrecognizedDatabase,
    /// SQLite or SQLx failed while reading or writing the schema.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

impl MigrationError {
    /// Process exit code required for this migration failure.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::SchemaTooNew { .. } | Self::UnrecognizedDatabase => 14,
            Self::AutoVacuumNotIncremental { .. } => 13,
            Self::Sqlx(_) => 1,
        }
    }
}

/// Apply all pending forward-only migrations to an empty or versioned database.
///
/// A brand-new database is switched to `PRAGMA auto_vacuum = INCREMENTAL` and
/// vacuumed before the first table is created. The entire initial DDL and the
/// version row commit atomically, so an interrupted first migration leaves an
/// empty database rather than a partial schema.
///
/// # Errors
///
/// Returns [`MigrationError::SchemaTooNew`] for a newer database and
/// [`MigrationError::UnrecognizedDatabase`] for foreign or partial schemas.
pub async fn migrate(pool: &SqlitePool) -> Result<(), MigrationError> {
    let mut connection = pool.acquire().await?;

    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(&mut *connection)
    .await?;

    let schema_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' AND name = '_llmroller_schema'",
    )
    .fetch_one(&mut *connection)
    .await?;

    if schema_table_count > 0 {
        let version: Option<i64> = sqlx::query_scalar("SELECT MAX(version) FROM _llmroller_schema")
            .fetch_one(&mut *connection)
            .await?;
        let Some(version) = version else {
            return Err(MigrationError::UnrecognizedDatabase);
        };
        if version > SCHEMA_VERSION {
            return Err(MigrationError::SchemaTooNew {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version != SCHEMA_VERSION {
            return Err(MigrationError::UnrecognizedDatabase);
        }
        drop(connection);
        return verify_auto_vacuum(pool).await;
    }

    if table_count != 0 {
        return Err(MigrationError::UnrecognizedDatabase);
    }

    sqlx::query("PRAGMA auto_vacuum = INCREMENTAL")
        .execute(&mut *connection)
        .await?;
    sqlx::query("VACUUM").execute(&mut *connection).await?;

    let mut transaction = connection.begin().await?;
    sqlx::raw_sql(INITIAL_SCHEMA_SQL)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("INSERT INTO _llmroller_schema (version) VALUES (?)")
        .bind(SCHEMA_VERSION)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    drop(connection);

    verify_auto_vacuum(pool).await
}

/// Assert that the connected database uses incremental auto-vacuum.
///
/// # Errors
///
/// Returns [`MigrationError::AutoVacuumNotIncremental`] when the pragma is not
/// `2` (`INCREMENTAL`), which startup maps to exit code `13`.
pub async fn verify_auto_vacuum(pool: &SqlitePool) -> Result<(), MigrationError> {
    let actual: i64 = sqlx::query_scalar("PRAGMA auto_vacuum")
        .fetch_one(pool)
        .await?;
    if actual == 2 {
        Ok(())
    } else {
        Err(MigrationError::AutoVacuumNotIncremental { actual })
    }
}

#[cfg(test)]
mod tests;
