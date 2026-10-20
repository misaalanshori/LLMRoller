#![forbid(unsafe_code)]
//! SQLite connection roles and per-connection pragmas.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#91-per-connection-pragma-builder-and-complete-31-table-sqlite-ddl-schema
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md

use std::env;
use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

/// Environment variable selecting the writer's SQLite `synchronous` mode.
pub const SQLITE_SYNCHRONOUS_ENV: &str = "LLMROLLER_SQLITE_SYNCHRONOUS";

/// Number of read-only SQLite connections in the server reader pool.
pub const READER_POOL_SIZE: u32 = 8;

const BUSY_TIMEOUT_SERVER: Duration = Duration::from_secs(5);
const BUSY_TIMEOUT_CLI: Duration = Duration::from_secs(30);
const JOURNAL_SIZE_LIMIT: &str = "67108864";

/// Query access mode for offline CLI commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CliMode {
    /// Commands that mutate state, such as `rotate-master-key` and `reset-admin`.
    ReadWrite,
    /// Commands that only inspect state, such as `verify-audit-log` and `backup`.
    ReadOnly,
}

impl CliMode {
    /// Whether the connection must set `PRAGMA query_only = ON`.
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

/// SQLite connection class for the writer, readers, or CLI commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Single serialized writer connection.
    Writer,
    /// One of eight read-only server connections.
    Reader,
    /// Offline CLI connection with command-specific query access.
    Cli(CliMode),
}

impl Role {
    /// Construct a read-write CLI role.
    #[must_use]
    pub const fn cli_read_write() -> Self {
        Self::Cli(CliMode::ReadWrite)
    }

    /// Construct a read-only CLI role.
    #[must_use]
    pub const fn cli_read_only() -> Self {
        Self::Cli(CliMode::ReadOnly)
    }

    /// Maximum number of pooled connections for this role.
    #[must_use]
    pub const fn max_connections(self) -> u32 {
        match self {
            Self::Reader => READER_POOL_SIZE,
            Self::Writer | Self::Cli(_) => 1,
        }
    }

    /// Whether this role must set `PRAGMA query_only = ON`.
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::Reader) || matches!(self, Self::Cli(mode) if mode.is_read_only())
    }

    const fn create_if_missing(self) -> bool {
        match self {
            Self::Writer => true,
            Self::Reader => false,
            Self::Cli(mode) => !mode.is_read_only(),
        }
    }
}

/// Build the single per-connection pragma policy for a role.
///
/// Each connection must be created through this function so `foreign_keys`,
/// `busy_timeout`, `query_only`, `synchronous`, `temp_store`, and
/// `journal_size_limit` cannot drift between the writer, readers, and CLI.
#[must_use]
pub fn connect_options(role: Role) -> SqliteConnectOptions {
    let busy_timeout = match role {
        Role::Cli(_) => BUSY_TIMEOUT_CLI,
        Role::Writer | Role::Reader => BUSY_TIMEOUT_SERVER,
    };

    let mut options = SqliteConnectOptions::new()
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(busy_timeout)
        .pragma("temp_store", "MEMORY")
        .create_if_missing(role.create_if_missing());

    options = match role {
        Role::Writer => options
            .synchronous(writer_synchronous())
            .pragma("query_only", "OFF")
            .pragma("journal_size_limit", JOURNAL_SIZE_LIMIT),
        Role::Reader => options.pragma("query_only", "ON"),
        Role::Cli(mode) => options
            .synchronous(SqliteSynchronous::Full)
            .pragma("query_only", if mode.is_read_only() { "ON" } else { "OFF" })
            .pragma("journal_size_limit", JOURNAL_SIZE_LIMIT),
    };

    options
}

fn writer_synchronous() -> SqliteSynchronous {
    match env::var(SQLITE_SYNCHRONOUS_ENV) {
        Ok(value) if value.eq_ignore_ascii_case("FULL") => SqliteSynchronous::Full,
        Ok(value) if value.eq_ignore_ascii_case("NORMAL") => SqliteSynchronous::Normal,
        Ok(_) | Err(_) => SqliteSynchronous::Normal,
    }
}

/// Open a role-specific SQLite pool without running migrations.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] when the database cannot be opened or a
/// connection cannot be established.
pub async fn open_pool(path: &Path, role: Role) -> Result<SqlitePool, sqlx::Error> {
    let options = connect_options(role).filename(path);
    SqlitePoolOptions::new()
        .max_connections(role.max_connections())
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
}

#[cfg(test)]
mod tests;
