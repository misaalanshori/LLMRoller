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
        collect_files_with_extension(&source_root, "rs", &mut files);
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

/// The single `MNT-3` line ceiling, shared by every file kind so the codebase has one number
/// rather than one per extension (`ISSUE-0056`).
const MNT3_MAX_LINES: usize = 1600;

#[test]
#[doc = "Spec: MNT-3"]
fn all_rust_files_are_under_1600_lines() {
    let mut files = Vec::new();
    for sub in ["crates", "tests", "benches", "xtask", "fuzz"] {
        let dir = workspace_root().join(sub);
        if dir.is_dir() {
            collect_files_with_extension(&dir, "rs", &mut files);
        }
    }
    assert_no_file_exceeds(files, MNT3_MAX_LINES, "Rust");
}

/// The same ceiling, applied to the front-end assets it was always meant to cover.
///
/// `all_rust_files_are_under_1600_lines` collected only `.rs`, so `app.js` sat at 1,649 lines
/// — 49 over — while CI stayed green. A gate that names a number and does not check it
/// manufactures confidence rather than providing it, which is worse than having no gate:
/// nobody re-measures. (How long it went unnoticed is not derivable from this repository —
/// every commit carries the same author date — so no duration is claimed.)
///
/// Scope note: the vendored libraries in the same directory (`htmx.min.js`,
/// `uPlot.iife.min.js`, `uPlot.min.css`) are single-line minified builds, so a line ceiling
/// is meaningless for them and they are excluded by name. That is deliberate rather than an
/// oversight — see the assertion below, which pins the exclusion list so a new vendored file
/// cannot slip in unnoticed by being minified.
#[test]
#[doc = "Spec: MNT-3 — ISSUE-0056 static assets and templates are under the ceiling"]
fn all_static_assets_are_under_1600_lines() {
    let root = workspace_root();
    let mut files = Vec::new();
    for sub in ["crates/admin/static", "crates/admin/templates"] {
        let dir = root.join(sub);
        if dir.is_dir() {
            for ext in ["js", "css", "html"] {
                collect_files_with_extension(&dir, ext, &mut files);
            }
        }
    }

    // Third-party minified bundles are one line by construction; a line ceiling cannot
    // express anything about them. Pinned so this list is a decision, not a side effect.
    const VENDORED: [&str; 3] = ["htmx.min.js", "uPlot.iife.min.js", "uPlot.min.css"];
    files.retain(|p| {
        !p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| VENDORED.contains(&n))
    });

    assert!(
        !files.is_empty(),
        "the static-asset ceiling check found no files, so it proves nothing"
    );
    assert_no_file_exceeds(files, MNT3_MAX_LINES, "static/template");
}

/// The split `app-*.js` modules must load in order in one shared scope without a
/// `ReferenceError`, and the delegated dispatcher must still reach its handlers.
///
/// This is the only executable check of the *behaviour* of the split, and it runs the real
/// JavaScript in Node rather than reading the source. Two failure modes it catches that no
/// Rust assertion can: a load-order temporal-dead-zone error (a `let` read by an earlier
/// file), and a dispatcher calling a function that did not move to the shared scope.
///
/// Skips — with a message, not a silent pass — when Node is absent, so the suite still works
/// in a Rust-only container. It does not use a JS test framework: the repo has none, and
/// adding one for a single file would be a build-time dependency this project does not take.
#[test]
#[doc = "Spec: MNT-3, UI-3 — ISSUE-0013 the split modules load in order and dispatch"]
fn static_asset_modules_load_in_order_and_dispatch() {
    let script = workspace_root().join("tests/js/app-modules-smoke.js");
    assert!(script.is_file(), "{} must exist", script.display());

    let output = match Command::new("node").arg(&script).output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("skipping: node is not available ({error})");
            return;
        }
    };
    assert!(
        output.status.success(),
        "app module smoke check failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `APP_JS_MODULES` and the `<script>` tags must list the same modules in the same order.
///
/// Load order is load-bearing (the files share one global scope), so "same set" is not
/// enough: `app-core.js` must come before anything that reads a declaration it owns. This
/// cross-checks the Rust table against the templates' tags, which is the pair that drifts.
#[test]
#[doc = "Spec: MNT-3, UI-3 — ISSUE-0013 asset table and template script order agree"]
fn app_module_order_matches_between_assets_and_templates() {
    let root = workspace_root();
    let assets = fs::read_to_string(root.join("crates/admin/src/assets.rs"))
        .expect("assets.rs must be readable");

    // Parse `("app-x.js", APP_X_JS),` entries in table order.
    let from_assets: Vec<String> = assets
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("(\"app-")?;
            let end = rest.find(".js\"")?;
            Some(format!("app-{}.js", &rest[..end]))
        })
        .collect();
    assert!(
        !from_assets.is_empty(),
        "parsed no modules out of APP_JS_MODULES, so this test proves nothing"
    );

    let template =
        fs::read_to_string(root.join("crates/admin/templates/base.html")).expect("base.html");
    let mut from_template: Vec<String> = Vec::new();
    for line in template.lines() {
        if let Some(idx) = line.find("src=\"/static/app-") {
            let rest = &line[idx + "src=\"/static/".len()..];
            if let Some(end) = rest.find('"') {
                from_template.push(rest[..end].to_string());
            }
        }
    }

    assert_eq!(
        from_template, from_assets,
        "base.html's script order must match APP_JS_MODULES exactly, because the files share \
         one global scope and `let` declarations are not hoisted across files"
    );
}

/// Collect files with `extension` under `directory`, skipping build output and VCS metadata.
fn collect_files_with_extension(directory: &Path, extension: &str, out: &mut Vec<PathBuf>) {
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
            collect_files_with_extension(&path, extension, out);
        } else if path
            .extension()
            .is_some_and(|candidate| candidate == extension)
        {
            out.push(path);
        }
    }
}

fn assert_no_file_exceeds(files: Vec<PathBuf>, ceiling: usize, label: &str) {
    let mut violations = Vec::new();
    for file in files {
        let content = fs::read_to_string(&file).expect("source must be UTF-8");
        let line_count = content.lines().count();
        if line_count > ceiling {
            violations.push(format!(
                "{}: {} lines (exceeds {ceiling} ceiling)",
                file.display(),
                line_count
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "{label} files exceed the {ceiling} LOC ceiling (MNT-3):\n{}",
        violations.join("\n")
    );
}
