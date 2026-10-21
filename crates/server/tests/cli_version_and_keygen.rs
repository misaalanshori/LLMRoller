//! Integration test for `llmroller --version` and `llmroller generate-master-key`.
//!
//! Spec: Section 5.5, Requirements: POR-1, SEC-2.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::process::Command;

fn llmroller_binary() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("current exe");
    path.pop(); // pop test binary name
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("llmroller");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

#[test]
#[doc = "Spec: POR-1"]
fn version_flag_prints_mandated_schema_contract() {
    let output = Command::new(llmroller_binary())
        .arg("--version")
        .output()
        .expect("run llmroller --version");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    // Format: llmroller <semver> (<git_sha>, schema <n>)
    let trimmed = stdout.trim();
    assert!(
        trimmed.starts_with("llmroller "),
        "version output must start with 'llmroller ', got: {trimmed}"
    );
    assert!(
        trimmed.contains(", schema "),
        "version output must contain ', schema ', got: {trimmed}"
    );
    assert!(
        trimmed.ends_with(')'),
        "version output must end with ')', got: {trimmed}"
    );
}

#[test]
#[doc = "Spec: SEC-2"]
fn generate_master_key_cli_outputs_32_bytes_base64() {
    let output = Command::new(llmroller_binary())
        .arg("generate-master-key")
        .output()
        .expect("run llmroller generate-master-key");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let key_b64 = stdout.trim();
    let decoded = BASE64.decode(key_b64).expect("valid base64");
    assert_eq!(decoded.len(), 32, "master key must be exactly 32 bytes");
}
