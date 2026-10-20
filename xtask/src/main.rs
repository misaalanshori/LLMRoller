#![forbid(unsafe_code)]
//! Pure-Rust workspace automation.
//!
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#166-code-test-spec-traceability-matrix-cargo-xtask-traceability
//! Phase: ./docs/bigbang/big-bang-phases-1.md#11-sub-phase-11-cargo-workspace-xtask-governance-tools-foundational-docker-scaffolding-and-multi-arch-ci-matrix

mod docs_bundle;
mod requirements;
mod source_scan;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use requirements::Registry;

const USAGE: &str = "\
usage:
  cargo xtask traceability --milestone M<N>
  cargo xtask doc-consistency
  cargo xtask docs-bundle [--check]";

/// Requirement prefixes defined by SRDD Section 0.2. `G` covers the goal IDs
/// when they are written with a hyphen; prose-only identifiers such as `ADR-12`
/// deliberately do not match.
pub(crate) const KNOWN_PREFIXES: [&str; 23] = [
    "AUTH", "USR", "PRV", "SHR", "CLK", "MDL", "RTE", "API", "USG", "LIM", "TRC", "UI", "AUD",
    "PERF", "REL", "TRN", "COM", "POR", "MNT", "OBS", "PRIV", "SEC", "G",
];

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl IntoIterator<Item = String>) -> Result<(), String> {
    let root = workspace_root();
    let args: Vec<String> = args.into_iter().collect();
    let (command, rest) = args
        .split_first()
        .ok_or_else(|| format!("missing subcommand\n{USAGE}"))?;

    match command.as_str() {
        "traceability" => {
            let milestone = parse_milestone(rest)?;
            let registry = Registry::load(&root)?;
            let scan = source_scan::Scan::scan_workspace(&root)?;
            registry.traceability(&milestone, &scan, &mut std::io::stdout())
        }
        "doc-consistency" => {
            if !rest.is_empty() {
                return Err(format!("doc-consistency takes no arguments\n{USAGE}"));
            }
            let registry = Registry::load(&root)?;
            docs_bundle::doc_consistency(&root, &registry)
        }
        "docs-bundle" => {
            let check = parse_check(rest)?;
            docs_bundle::run(&root, check)
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown subcommand {other:?}\n{USAGE}")),
    }
}

fn parse_milestone(args: &[String]) -> Result<String, String> {
    let mut milestone = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--milestone" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--milestone requires a value such as M0".to_string())?;
                milestone = Some(value.clone());
                index += 2;
            }
            other if other.starts_with("--milestone=") => {
                milestone = Some(other["--milestone=".len()..].to_string());
                index += 1;
            }
            other => {
                return Err(format!(
                    "unexpected traceability argument {other:?}\n{USAGE}"
                ));
            }
        }
    }
    let milestone =
        milestone.ok_or_else(|| format!("traceability requires --milestone\n{USAGE}"))?;
    if !is_milestone(&milestone) {
        return Err(format!(
            "invalid milestone {milestone:?}; expected M<number>[a-z]"
        ));
    }
    Ok(milestone)
}

fn parse_check(args: &[String]) -> Result<bool, String> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--check" => Ok(true),
        _ => Err(format!("unexpected docs-bundle arguments\n{USAGE}")),
    }
}

fn is_milestone(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('M') else {
        return false;
    };
    let digits_end = rest
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits_end == 0 {
        return false;
    }
    let (digits, suffix) = rest.split_at(digits_end);
    digits.chars().all(|character| character.is_ascii_digit())
        && (suffix.is_empty()
            || (suffix.len() == 1
                && suffix
                    .chars()
                    .all(|character| character.is_ascii_lowercase())))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in a workspace subdirectory")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn milestone_parser_accepts_epic_suffixes() {
        assert!(is_milestone("M0"));
        assert!(is_milestone("M3b"));
        assert!(is_milestone("M6"));
        assert!(!is_milestone("M"));
        assert!(!is_milestone("m0"));
        assert!(!is_milestone("M3bb"));
        assert!(!is_milestone("M-1"));
    }

    #[test]
    fn traceability_requires_a_milestone_flag() {
        let error = run(["traceability".to_string()]).expect_err("missing milestone must fail");
        assert!(error.contains("requires --milestone"), "{error}");
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        let error = run(["nonsense".to_string()]).expect_err("unknown command must fail");
        assert!(error.contains("unknown subcommand"), "{error}");
    }
}
