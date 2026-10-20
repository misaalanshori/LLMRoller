#![forbid(unsafe_code)]
//! LLMRoller binary entrypoint.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes

fn main() {
    // The CLI surface is implemented in sub-phase 1.4; the workspace starts as
    // a compiling architectural skeleton so boundaries can be verified first.
    let _ = env!("CARGO_PKG_VERSION");
}
