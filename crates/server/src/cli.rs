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
use llmroller_core::seams::{Clock, ExposeSecret, OsEntropy, SystemClock};
use llmroller_store::crypto::{MasterKey, load_master_key_from_env, load_master_key_id_from_env};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};
use llmroller_store::rotation::rotate_master_key;
use llmroller_store::users::reset_admin;

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
    /// Restore an administrator account offline.
    ResetAdmin(ResetAdminArgs),
    /// Verify the audit-log HMAC chain.
    VerifyAuditLog(VerifyAuditLogArgs),
}

/// Arguments for `llmroller verify-audit-log`.
#[derive(Debug, Args)]
pub(crate) struct VerifyAuditLogArgs {
    /// Expected anchor sequence number.
    #[arg(long)]
    pub(crate) anchor_seq: Option<i64>,
    /// Expected anchor HMAC-SHA256 hex hash.
    #[arg(long)]
    pub(crate) anchor_hash: Option<String>,
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
    /// Output summary in JSON format.
    #[arg(long)]
    pub(crate) json: bool,
}

/// Arguments for `llmroller reset-admin`.
#[derive(Debug, Args)]
pub(crate) struct ResetAdminArgs {
    /// Username of the administrator account to restore.
    #[arg(long)]
    pub(crate) username: String,
    /// Data directory containing `llmroller.db` and `llmroller.lock`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
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
        Command::ResetAdmin(args) => run_reset_admin(args).await,
        Command::VerifyAuditLog(args) => run_verify_audit_log(args).await,
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

async fn run_reset_admin(args: ResetAdminArgs) -> ExitCode {
    match reset_admin_command(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

async fn reset_admin_command(args: ResetAdminArgs) -> Result<(), CliFailure> {
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

    let master_key = load_master_key_from_env().map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;
    let master_key_id = load_master_key_id_from_env().map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;

    let database = data_dir.join("llmroller.db");
    let pool = open_pool(&database, Role::cli_read_write())
        .await
        .map_err(|error| {
            CliFailure::new(13, format!("cannot open {}: {error}", database.display()))
        })?;
    if let Err(error) = migrate(&pool).await {
        let code = u8::try_from(error.exit_code()).unwrap_or(1);
        return Err(CliFailure::new(code, error.to_string()));
    }

    let clock = SystemClock::new();
    let entropy = Arc::new(OsEntropy::new());
    let instance_state = load_or_initialize(
        &pool,
        &master_key_id,
        &master_key,
        entropy.clone(),
        clock.now_ms(),
    )
    .await
    .map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;

    let result = reset_admin(
        &pool,
        &args.username,
        entropy.as_ref(),
        instance_state.audit_hmac_key.expose_secret(),
        clock.now_ms(),
    )
    .await
    .map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(13);
        CliFailure::new(code, error.to_string())
    })?;
    pool.close().await;

    eprintln!(
        "temporary password for administrator {:?}: {}",
        args.username,
        result.temporary_password.expose_secret()
    );
    Ok(())
}

async fn run_verify_audit_log(args: VerifyAuditLogArgs) -> ExitCode {
    match verify_audit_log_command(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

async fn verify_audit_log_command(args: VerifyAuditLogArgs) -> Result<(), CliFailure> {
    let data_dir = resolve_data_dir(args.data_dir);
    let master_key = load_master_key_from_env().map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;
    let master_key_id = load_master_key_id_from_env().map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;

    let database = data_dir.join("llmroller.db");
    let pool = open_pool(&database, Role::cli_read_only())
        .await
        .map_err(|error| {
            CliFailure::new(13, format!("cannot open {}: {error}", database.display()))
        })?;

    let clock = SystemClock::new();
    let entropy = Arc::new(OsEntropy::new());
    let instance_state =
        load_or_initialize(&pool, &master_key_id, &master_key, entropy, clock.now_ms())
            .await
            .map_err(|error| {
                let code = u8::try_from(error.exit_code()).unwrap_or(3);
                CliFailure::new(code, error.to_string())
            })?;

    let report = llmroller_store::audit::verify_audit_log(
        &pool,
        instance_state.audit_hmac_key.expose_secret(),
        args.anchor_seq,
        args.anchor_hash.as_deref(),
    )
    .await
    .map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(13);
        CliFailure::new(code, error.to_string())
    })?;
    pool.close().await;

    if args.json {
        let json_output =
            serde_json::to_string(&report).map_err(|e| CliFailure::new(13, e.to_string()))?;
        println!("{json_output}");
    } else {
        eprintln!(
            "audit log verified: {} rows intact (tail seq {}, hash {})",
            report.rows_verified, report.max_seq, report.tail_hash
        );
    }
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
        match cli.command {
            Command::RotateMasterKey(args) => {
                assert_eq!(args.old_key_env, "OLD_KEY");
                assert_eq!(args.new_key_env, "NEW_KEY");
                assert_eq!(args.new_key_id, "mk_next");
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
            }
            _ => panic!("expected RotateMasterKey"),
        }
    }

    #[test]
    #[doc = "Spec: AUTH-5"]
    fn reset_admin_parses_required_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "reset-admin",
            "--username",
            "admin",
            "--data-dir",
            "/tmp/data",
        ])
        .expect("valid reset-admin CLI");
        match cli.command {
            Command::ResetAdmin(args) => {
                assert_eq!(args.username, "admin");
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
            }
            _ => panic!("expected ResetAdmin"),
        }
    }

    #[test]
    #[doc = "Spec: AUD-1, Spec: SEC-13"]
    fn verify_audit_log_parses_required_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "verify-audit-log",
            "--anchor-seq",
            "100",
            "--anchor-hash",
            "abcdef123456",
            "--data-dir",
            "/tmp/data",
            "--json",
        ])
        .expect("valid verify-audit-log CLI");
        match cli.command {
            Command::VerifyAuditLog(args) => {
                assert_eq!(args.anchor_seq, Some(100));
                assert_eq!(args.anchor_hash.as_deref(), Some("abcdef123456"));
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
                assert!(args.json);
            }
            _ => panic!("expected VerifyAuditLog"),
        }
    }
}
