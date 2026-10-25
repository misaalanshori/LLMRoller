use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

pub(crate) fn version_string() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        let sha = option_env!("LLMROLLER_BUILD_GIT_SHA")
            .or(option_env!("GIT_SHA"))
            .unwrap_or("dev");
        format!(
            "{} ({}, schema {})",
            env!("CARGO_PKG_VERSION"),
            sha,
            llmroller_store::migrations::SCHEMA_VERSION
        )
    })
}

/// Top-level `llmroller` command.
#[derive(Debug, Parser)]
#[command(
    name = "llmroller",
    version = version_string(),
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
    /// Run the proxy and management server daemon (POR-1, REL-1).
    Serve(ServeArgs),
    /// Re-seal all encrypted rows under a replacement master key.
    RotateMasterKey(RotateMasterKeyArgs),
    /// Restore an administrator account offline.
    ResetAdmin(ResetAdminArgs),
    /// Verify the audit-log HMAC chain.
    VerifyAuditLog(VerifyAuditLogArgs),
    /// Probe the container readiness endpoint.
    Healthcheck(HealthcheckArgs),
    /// Create a crash-safe atomic backup of the database (REL-6).
    Backup(BackupArgs),
    /// Restore a verified SQLite backup into the data directory (`REL-6`, `F083`).
    Restore(RestoreArgs),
    /// Verify database integrity, schema, master key, and audit chain (`F083`).
    Doctor(DoctorArgs),
    /// Export a redacted support diagnostics JSON bundle (`F086`).
    ExportDiagnostics(ExportDiagnosticsArgs),
    /// Generate a 32-byte cryptographically secure master key (SEC-2).
    GenerateMasterKey,
}

/// Arguments for `llmroller serve`.
#[derive(Debug, Args)]
pub(crate) struct ServeArgs {
    /// Data-plane HTTP listener address.
    #[arg(long, env = "LLMROLLER_LISTEN", default_value = "0.0.0.0:8080")]
    pub(crate) listen: String,

    /// Control-plane HTTP listener address.
    #[arg(long, env = "LLMROLLER_ADMIN_LISTEN", default_value = "127.0.0.1:8081")]
    pub(crate) admin_listen: String,

    /// Metrics listener address.
    #[arg(
        long,
        env = "LLMROLLER_METRICS_LISTEN",
        default_value = "127.0.0.1:9090"
    )]
    pub(crate) metrics_listen: String,

    /// Data directory containing `llmroller.db` and `llmroller.lock`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,

    /// Allow non-HTTPS cookies for local dev.
    #[arg(long, default_value_t = false)]
    pub(crate) dev_insecure_cookies: bool,
}

/// Arguments for `llmroller backup`.
#[derive(Debug, Args)]
pub(crate) struct BackupArgs {
    /// Destination output path.
    #[arg(long)]
    pub(crate) out: PathBuf,
    /// Overwrite destination if it already exists.
    #[arg(long, default_value_t = false)]
    pub(crate) overwrite: bool,
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Arguments for `llmroller restore` (`F083`, `REL-6`).
#[derive(Debug, Args)]
pub(crate) struct RestoreArgs {
    /// Source backup file path to restore from.
    #[arg(long)]
    pub(crate) from: PathBuf,
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Arguments for `llmroller doctor` (`F083`).
#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Arguments for `llmroller export-diagnostics` (`F086`).
#[derive(Debug, Args)]
pub(crate) struct ExportDiagnosticsArgs {
    /// Optional output JSON file path (writes to stdout when omitted).
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Arguments for `llmroller healthcheck`.
#[derive(Debug, Args)]
pub(crate) struct HealthcheckArgs {
    /// URL to probe.
    #[arg(long, default_value = "http://127.0.0.1:8080/readyz")]
    pub(crate) url: String,
    /// Timeout in milliseconds.
    #[arg(long, default_value_t = 2000)]
    pub(crate) timeout_ms: u64,
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
