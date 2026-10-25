//! Architectural boundary, container, and CI governance checks.
//!
//! Spec: MNT-1, MNT-2, SEC-10, Partial: POR-1, Partial: SEC-11
//! Phase: ./docs/bigbang/big-bang-phases-1.md#11-sub-phase-11-cargo-workspace-xtask-governance-tools-foundational-docker-scaffolding-and-multi-arch-ci-matrix

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const MEMBER_PACKAGES: [&str; 7] = [
    "llmroller-core",
    "llmroller-proto",
    "llmroller-store",
    "llmroller-proxy",
    "llmroller-admin",
    "llmroller-server",
    "xtask",
];

const FORBIDDEN_CORE_DEPENDENCIES: [&str; 5] = ["tokio", "sqlx", "reqwest", "hyper", "axum"];

const POR1_TARGETS: [&str; 5] = [
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "aarch64-apple-darwin",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("server crate must live under <workspace>/crates/server")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = workspace_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("cannot read {path:?}: {error}"))
}

fn cargo_metadata() -> Value {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata must run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata must emit valid JSON")
}

#[test]
#[doc = "Spec: MNT-1"]
fn workspace_lints_forbid_unsafe_code_and_members_declare_it() {
    let manifest: toml::Value = toml::from_str(&read("Cargo.toml")).expect("valid Cargo.toml");
    let unsafe_level = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("lints"))
        .and_then(|lints| lints.get("rust"))
        .and_then(|rust| rust.get("unsafe_code"))
        .and_then(toml::Value::as_str);

    assert_eq!(
        unsafe_level,
        Some("forbid"),
        "[workspace.lints.rust] unsafe_code must be \"forbid\" (MNT-1)"
    );

    let metadata = cargo_metadata();
    let packages = metadata["packages"].as_array().expect("metadata packages");
    let names: BTreeSet<&str> = packages
        .iter()
        .filter_map(|package| package["name"].as_str())
        .collect();
    for expected in MEMBER_PACKAGES {
        assert!(
            names.contains(expected),
            "workspace member package {expected} is missing from cargo metadata"
        );
    }

    for package in packages {
        let name = package["name"].as_str().expect("package name");
        if !MEMBER_PACKAGES.contains(&name) {
            continue;
        }
        let manifest_path = Path::new(package["manifest_path"].as_str().expect("manifest path"));
        let crate_root = manifest_path.parent().expect("crate directory");
        let source = ["src/lib.rs", "src/main.rs"]
            .iter()
            .map(|relative| crate_root.join(relative))
            .find(|path| path.is_file())
            .unwrap_or_else(|| panic!("{name} has no src/lib.rs or src/main.rs"));
        let contents = fs::read_to_string(&source).expect("crate source must be UTF-8");
        let header = contents
            .lines()
            .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with("//"))
            .unwrap_or("");
        assert_eq!(
            header.trim(),
            "#![forbid(unsafe_code)]",
            "{name} must declare #![forbid(unsafe_code)] as its first item (MNT-1)"
        );
    }
}

#[test]
#[doc = "Spec: MNT-1, MNT-2"]
fn core_has_no_io_or_runtime_dependency_edges() {
    let metadata = cargo_metadata();
    let core = metadata["packages"]
        .as_array()
        .expect("metadata packages")
        .iter()
        .find(|package| package["name"] == "llmroller-core")
        .expect("llmroller-core package");

    let dependency_names: Vec<&str> = core["dependencies"]
        .as_array()
        .expect("core dependencies")
        .iter()
        .filter_map(|dependency| dependency["name"].as_str())
        .collect();

    for forbidden in FORBIDDEN_CORE_DEPENDENCIES {
        assert!(
            !dependency_names.contains(&forbidden),
            "crates/core must not depend on {forbidden} (MNT-2)"
        );
    }

    for internal in MEMBER_PACKAGES {
        if internal == "llmroller-core" {
            continue;
        }
        assert!(
            !dependency_names.contains(&internal),
            "crates/core must not depend on workspace member {internal} (MNT-2)"
        );
    }

    let store = metadata["packages"]
        .as_array()
        .expect("metadata packages")
        .iter()
        .find(|package| package["name"] == "llmroller-store")
        .expect("llmroller-store package");
    let store_deps: Vec<&str> = store["dependencies"]
        .as_array()
        .expect("store dependencies")
        .iter()
        .filter_map(|dep| dep["name"].as_str())
        .collect();
    assert!(
        !store_deps.contains(&"llmroller-proxy"),
        "crates/store must not depend on crates/proxy (MNT-2)"
    );
}

#[test]
#[doc = "Partial: USR-1, Partial: SEC-17"]
fn admin_and_proxy_do_not_construct_system_actor_scope() {
    for crate_name in ["admin", "proxy", "proto"] {
        let source_root = workspace_root().join("crates").join(crate_name).join("src");
        let mut files = Vec::new();
        collect_rust_files(&source_root, &mut files);
        for file in files {
            let source = fs::read_to_string(&file).expect("source must be UTF-8");
            assert!(
                !source.contains("ActorScope::system("),
                "{} must not construct ActorScope::System (USR-1/SEC-17); route through ActorScope::user",
                file.display()
            );
            assert!(
                !source.contains("SystemSeal"),
                "{} must not touch the System construct seal (USR-1/SEC-17)",
                file.display()
            );
        }
    }
}

fn collect_rust_files(directory: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => panic!("cannot read {directory:?}: {error}"),
    };
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|name| name == "target" || name == ".git")
            {
                continue;
            }
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
#[doc = "Spec: POR-1, Spec: SEC-11"]
fn dockerfile_is_multi_stage_hardened_and_distroless_by_default() {
    let dockerfile = read("Dockerfile");
    let stages: Vec<&str> = dockerfile
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("FROM ")?;
            rest.split_whitespace()
                .position(|token| token == "AS")
                .map(|index| rest.split_whitespace().nth(index + 1).unwrap_or(""))
        })
        .collect();

    for stage in ["planner", "builder", "runtime-distroless", "runtime-dev"] {
        assert!(
            stages.contains(&stage),
            "Dockerfile must define stage {stage}; found {stages:?} (POR-1)"
        );
    }
    assert_eq!(
        stages.last().copied(),
        Some("runtime-distroless"),
        "runtime-distroless must be the default final stage (POR-1/SEC-11)"
    );

    assert!(
        dockerfile.contains("rust:1.99.0-bookworm"),
        "Rust image must be pinned to 1.99.0"
    );
    assert!(dockerfile.contains("cargo-chef --locked --version 0.1.78"));
    assert!(
        dockerfile.contains("--locked"),
        "all cargo builds must pass --locked"
    );
    assert!(dockerfile.contains("gcr.io/distroless/static-debian12:nonroot"));
    assert!(dockerfile.contains("HEALTHCHECK"));
    assert!(dockerfile.contains("healthcheck\", \"--url\", \"http://127.0.0.1:8080/readyz"));
    assert!(dockerfile.contains("USER 65532:65532"));
    assert!(dockerfile.contains("ENTRYPOINT [\"/usr/local/bin/llmroller\"]"));

    let distroless = stage_body(&dockerfile, "runtime-distroless");
    assert!(
        !distroless.contains("RUN "),
        "runtime-distroless must not execute package managers or shell commands (SEC-11)"
    );
    assert!(
        !distroless.contains("/bin/sh"),
        "runtime-distroless must not contain a shell"
    );

    let dev = stage_body(&dockerfile, "runtime-dev");
    for package in ["ca-certificates", "curl", "sqlite3", "tini"] {
        assert!(dev.contains(package), "runtime-dev must install {package}");
    }
    assert!(dev.contains("ENTRYPOINT [\"/usr/bin/tini\", \"--\", \"/usr/local/bin/llmroller\"]"));
}

fn stage_body<'a>(dockerfile: &'a str, stage: &str) -> &'a str {
    let marker = format!(" AS {stage}");
    let start = dockerfile
        .find(&marker)
        .unwrap_or_else(|| panic!("stage {stage} must exist"));
    let after_marker = start + marker.len();
    let remainder = &dockerfile[after_marker..];
    let end = remainder.find("\nFROM ").unwrap_or(remainder.len());
    &remainder[..end]
}

#[test]
#[doc = "Spec: POR-1, Spec: SEC-11"]
fn dockerignore_excludes_secrets_and_build_output() {
    let dockerignore = read(".dockerignore");
    for pattern in [
        "target/",
        "*.db",
        "*.db-wal",
        "setup-token",
        ".env",
        "secrets/",
        ".git/",
    ] {
        assert!(
            dockerignore.lines().any(|line| line.trim() == pattern),
            ".dockerignore must contain {pattern} (SEC-11)"
        );
    }
    assert!(
        dockerignore
            .lines()
            .any(|line| line.trim() == "!.env.example")
    );
    assert!(
        dockerignore
            .lines()
            .any(|line| line.trim() == "!Cargo.lock"),
        ".dockerignore must un-ignore Cargo.lock for --locked builds"
    );
}

#[test]
#[doc = "Spec: POR-1"]
fn compose_manifest_is_valid_and_contains_production_and_dev_topologies() {
    let compose = read("docker-compose.yml");
    for service in [
        "llmroller-init",
        "llmroller:",
        "caddy:",
        "prometheus:",
        "mock-upstream:",
        "llmroller-dev:",
    ] {
        assert!(
            compose.contains(service),
            "docker-compose.yml must define {service}"
        );
    }
    for invariant in [
        "read_only: true",
        "cap_drop:",
        "no-new-privileges:true",
        "stop_grace_period: 90s",
        "target: runtime-distroless",
        "target: runtime-dev",
        "LLMROLLER_TRUSTED_PROXIES",
    ] {
        assert!(
            compose.contains(invariant),
            "docker-compose.yml must contain {invariant}"
        );
    }

    let output = Command::new("docker")
        .args(["compose", "version"])
        .current_dir(workspace_root())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let output = Command::new("docker")
                .args(["compose", "config", "--quiet"])
                .current_dir(workspace_root())
                .output()
                .expect("docker compose config must run");
            assert!(
                output.status.success(),
                "docker compose config --quiet failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        _ => {
            eprintln!(
                "docker compose is unavailable; structural manifest checks passed and the \
                 governance CI job enforces `docker compose config --quiet`"
            );
        }
    }
}

#[test]
#[doc = "Spec: POR-1, MNT-1"]
fn ci_matrix_covers_all_por1_targets_and_gates() {
    let workflow = read(".github/workflows/ci.yml");
    for target in POR1_TARGETS {
        assert!(
            workflow.contains(target),
            "CI matrix must include POR-1 target {target}"
        );
    }
    for gate in [
        "cargo fmt --all --check",
        "cargo clippy --workspace --all-targets -- -D warnings",
        "check bans licenses advisories sources",
        "cargo test --workspace --all-targets",
        "docker compose config --quiet",
        "stage: [runtime-distroless, runtime-dev]",
        "50 * 1024 * 1024",
        "llmroller-smoke",
        "http://127.0.0.1:18080/readyz",
    ] {
        assert!(workflow.contains(gate), "CI workflow must run {gate}");
    }
}

#[test]
#[doc = "Spec: SEC-11, POR-1"]
fn release_workflow_emits_sbom_and_signs_every_artifact() {
    let workflow = read(".github/workflows/release.yml");
    for gate in [
        "cargo-cyclonedx --locked --version 0.5.9",
        "cargo cyclonedx --manifest-path crates/server/Cargo.toml",
        "name: llmroller-sbom",
        "cosign sign-blob --yes --bundle",
        "name: llmroller-release-signed",
        "SHA256SUMS",
    ] {
        assert!(
            workflow.contains(gate),
            "release workflow must include {gate}"
        );
    }
}

#[test]
#[doc = "Partial: MNT-2"]
fn cargo_xtask_alias_is_configured() {
    let config = read(".cargo/config.toml");
    assert!(
        config.contains("xtask = \"run --locked --package xtask --\""),
        "cargo xtask alias must invoke the workspace xtask package"
    );
}

#[test]
#[doc = "Spec: SEC-10, Spec: POR-1"]
fn lockfile_is_committed_despite_generic_lock_ignore() {
    let gitignore = read(".gitignore");
    assert!(
        gitignore.lines().any(|line| line.trim() == "*.lock"),
        ".gitignore must ignore runtime lock files"
    );
    assert!(
        gitignore.lines().any(|line| line.trim() == "!Cargo.lock"),
        ".gitignore must un-ignore the committed Cargo.lock"
    );

    let output = Command::new("git")
        .args(["check-ignore", "--quiet", "Cargo.lock"])
        .current_dir(workspace_root())
        .status();
    match output {
        Ok(status) => assert!(
            !status.success(),
            "Cargo.lock must be committable (git check-ignore says it is ignored)"
        ),
        Err(error) => panic!("git must be available: {error}"),
    }
}

#[test]
#[doc = "Spec: MNT-3"]
fn all_rust_files_are_under_1600_lines() {
    let mut files = Vec::new();
    for sub in ["crates", "tests", "benches", "xtask", "fuzz"] {
        let dir = workspace_root().join(sub);
        if dir.is_dir() {
            collect_rust_files(&dir, &mut files);
        }
    }
    let mut violations = Vec::new();
    for file in files {
        let content = fs::read_to_string(&file).expect("source must be UTF-8");
        let line_count = content.lines().count();
        if line_count > 1600 {
            violations.push(format!(
                "{}: {} lines (exceeds 1600 ceiling)",
                file.display(),
                line_count
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "Files exceed 1600 LOC ceiling (MNT-3):\n{}",
        violations.join("\n")
    );
}
