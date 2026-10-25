#![forbid(unsafe_code)]
//! Command-line interface and offline maintenance commands.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes

mod args;
mod helpers;
mod maintenance;
mod serve;

pub(crate) use args::*;
pub(crate) use maintenance::*;
pub(crate) use serve::*;

use std::process::ExitCode;
use clap::Parser;

/// Parse arguments and dispatch one command.
pub(crate) async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => run_serve(args).await,
        Command::RotateMasterKey(args) => run_rotation(args).await,
        Command::ResetAdmin(args) => run_reset_admin(args).await,
        Command::VerifyAuditLog(args) => run_verify_audit_log(args).await,
        Command::Healthcheck(args) => run_healthcheck(args).await,
        Command::Backup(args) => run_backup_cmd(args).await,
        Command::Restore(args) => run_restore_cmd(args).await,
        Command::Doctor(args) => run_doctor_cmd(args).await,
        Command::ExportDiagnostics(args) => run_export_diagnostics_cmd(args).await,
        Command::GenerateMasterKey => run_generate_master_key(),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod phase7_tests;
