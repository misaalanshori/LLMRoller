#![forbid(unsafe_code)]
//! LLMRoller binary entrypoint.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes

mod cli;

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    cli::main().await
}
