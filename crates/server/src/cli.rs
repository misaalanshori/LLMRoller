#![forbid(unsafe_code)]
//! Command-line interface and offline maintenance commands.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes

use std::env;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use llmroller_core::seams::{Clock, OsEntropy, SystemClock};
use llmroller_store::crypto::MasterKey;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};
use llmroller_store::rotation::rotate_master_key;

const DATA_DIR_ENV: &str = "LLMROLLER_DATA_DIR";
const DEFAULT_DATA_DIR: &str = "/var/lib/llmroller";

/// Top-level `llmroller` command.
#[derive(Debug, Parser)]
#[command(
    name = "llmroller",
    version,
    about = "LLMRoller proxy server and offline maintenance tools"
)]
pub(crate) struct Cli {
    /// Maintenance subcommand to execute.
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Re-seal all encrypted rows under a replacement master key.
    RotateMasterKey(RotateMasterKeyArgs),
}

/// Arguments for `llmroller rotate-master-key`.
#[derive(Debug, Args)]
pub(crate) struct RotateMasterKeyArgs {
    /// Environment variable containing the current base64 master key.
    #[arg(long)]
    pub(crate) old_key_env: String,
    /// Environment variable containing the replacement base64 master key.
    #[arg(long)]
    pub(crate) new_key_env: String,
    /// Identifier to store as the new active master-key id.
    #[arg(long)]
    pub(crate) new_key_id: String,
    /// Data directory containing `llmroller.db` and `llmroller.lock`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Parse arguments and dispatch one command.
pub(crate) async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::RotateMasterKey(args) => run_rotation(args).await,
    }
}

async fn run_rotation(args: RotateMasterKeyArgs) -> ExitCode {
    match rotation_command(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

async fn rotation_command(args: RotateMasterKeyArgs) -> Result<(), CliFailure> {
    let data_dir = resolve_data_dir(args.data_dir);
    fs::create_dir_all(&data_dir).map_err(|error| {
        CliFailure::new(
            13,
            format!(
                "cannot create data directory {}: {error}",
                data_dir.display()
            ),
        )
    })?;
    let _lock = acquire_lock(&data_dir)?;

    let old_key = read_key_from_env(&args.old_key_env, "old")?;
    let new_key = read_key_from_env(&args.new_key_env, "new")?;

    let database = data_dir.join("llmroller.db");
    let pool = open_pool(&database, Role::Writer).await.map_err(|error| {
        CliFailure::new(13, format!("cannot open {}: {error}", database.display()))
    })?;
    if let Err(error) = migrate(&pool).await {
        let code = u8::try_from(error.exit_code()).unwrap_or(1);
        return Err(CliFailure::new(code, error.to_string()));
    }

    let clock = SystemClock::new();
    let report = rotate_master_key(
        &pool,
        old_key,
        new_key,
        &args.new_key_id,
        Arc::new(OsEntropy::new()),
        clock.now_ms(),
    )
    .await
    .map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(1);
        CliFailure::new(code, error.to_string())
    })?;
    pool.close().await;

    eprintln!(
        "rotation complete: {} rows re-sealed in {} batches{}",
        report.rows_re_sealed,
        report.batches,
        if report.resumed { " (resumed)" } else { "" }
    );
    eprintln!(
        "Update the master key to the replacement value and set LLMROLLER_MASTER_KEY_ID={:?} before starting serve.",
        args.new_key_id
    );
    Ok(())
}

fn resolve_data_dir(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| env::var_os(DATA_DIR_ENV).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR))
}

fn acquire_lock(data_dir: &Path) -> Result<File, CliFailure> {
    let path = data_dir.join("llmroller.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| CliFailure::new(13, format!("cannot open {}: {error}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(CliFailure::new(
            4,
            format!(
                "{} is held by another process; stop serve before rotating",
                path.display()
            ),
        )),
        Err(TryLockError::Error(error)) => Err(CliFailure::new(
            13,
            format!("cannot lock {}: {error}", path.display()),
        )),
    }
}

fn read_key_from_env(variable: &str, role: &str) -> Result<MasterKey, CliFailure> {
    let encoded = match env::var(variable) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => {
            return Err(CliFailure::new(
                3,
                format!("{role} master key environment variable {variable:?} is not set"),
            ));
        }
        Err(env::VarError::NotUnicode(_)) => {
            return Err(CliFailure::new(
                3,
                format!("{role} master key environment variable {variable:?} is not UTF-8"),
            ));
        }
    };
    MasterKey::from_base64(&encoded).map_err(|error| CliFailure::new(3, error.to_string()))
}

#[derive(Debug)]
struct CliFailure {
    code: u8,
    message: String,
}

impl CliFailure {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;

    #[test]
    #[doc = "Partial: SEC-2"]
    fn rotate_master_key_parses_required_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "rotate-master-key",
            "--old-key-env",
            "OLD_KEY",
            "--new-key-env",
            "NEW_KEY",
            "--new-key-id",
            "mk_next",
            "--data-dir",
            "/tmp/data",
        ])
        .expect("valid CLI");
        let Command::RotateMasterKey(args) = cli.command;
        assert_eq!(args.old_key_env, "OLD_KEY");
        assert_eq!(args.new_key_env, "NEW_KEY");
        assert_eq!(args.new_key_id, "mk_next");
        assert_eq!(
            args.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/data"))
        );
    }
}
