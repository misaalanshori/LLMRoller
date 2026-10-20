//! Generated documentation bundles and cross-document consistency checks.
//!
//! Spec: ./docs/srdd/0. SRDD - Introduction.md#04-document-map
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#166-code-test-spec-traceability-matrix-cargo-xtask-traceability
//! Phase: ./docs/bigbang/big-bang-phases-1.md#11-sub-phase-11-cargo-workspace-xtask-governance-tools-foundational-docker-scaffolding-and-multi-arch-ci-matrix

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::KNOWN_PREFIXES;
use crate::requirements::Registry;

const SRDD_HEADER: &str = "\
<!-- Generated file - edit the numbered parts -->
# LLMRoller: Software Requirements and Design Document

Version 1.3 · 20 October 2026 · Complete software requirements, architecture, pluggable routing strategy engine, `/usage` pass-through, opt-in content traceability and chat preview, multi-role RBAC, concurrency/recovery model, Docker containerization & deployment specification, and TDD specification.


";

const BIGBANG_HEADER: &str = "\
<!-- Generated file - edit the numbered parts -->
# LLMRoller: Big-Bang Development Phases and TDD Execution Guide

Version 1.3 · 20 October 2026 · Adaptive ground-up engineering guide from `git init` to `v1.0.0` with pluggable routing strategies, Docker deployment lockstep gates, and full traceability to the LLMRoller SRDD.


";

const BIGBANG_PARTS: [&str; 9] = [
    "big-bang-phases-0.md",
    "big-bang-phases-1.md",
    "big-bang-phases-2.md",
    "big-bang-phases-3.md",
    "big-bang-phases-4.md",
    "big-bang-phases-4b.md",
    "big-bang-phases-5.md",
    "big-bang-phases-6.md",
    "big-bang-phases-7.md",
];

pub(crate) fn run(root: &Path, check: bool) -> Result<(), String> {
    for (path, expected) in generated_bundles(root)? {
        if check {
            let actual = fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            if actual != expected {
                return Err(format!(
                    "{} is out of date; run `cargo xtask docs-bundle`",
                    path.display()
                ));
            }
            println!("verified {}", path.display());
        } else {
            fs::write(&path, expected)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
            println!("wrote {}", path.display());
        }
    }
    Ok(())
}

pub(crate) fn doc_consistency(root: &Path, registry: &Registry) -> Result<(), String> {
    let srdd = read_srdd_parts(root)?;
    let bigbang = read_bigbang_parts(root)?;
    let srdd_text = srdd
        .values()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    let bigbang_text = bigbang
        .values()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");

    let mut errors = Vec::new();

    for requirement in registry.requirements.values() {
        if !contains_requirement_id(&srdd_text, &requirement.id) {
            errors.push(format!(
                "{} is registered but never appears in the numbered SRDD files",
                requirement.id
            ));
        }
        if !contains_requirement_id(&bigbang_text, &requirement.id) {
            errors.push(format!(
                "{} is registered but never appears in the Big-Bang phase files",
                requirement.id
            ));
        }
        let heading = format!("## {} Sub-phase", requirement.delivering_subphase);
        if !bigbang_text.contains(&heading) {
            errors.push(format!(
                "{} delivering_subphase {} has no Big-Bang sub-phase heading",
                requirement.id, requirement.delivering_subphase
            ));
        }
        for subphase in &requirement.partial_subphases {
            let heading = format!("## {subphase} Sub-phase");
            if !bigbang_text.contains(&heading) {
                errors.push(format!(
                    "{} partial_subphase {} has no Big-Bang sub-phase heading",
                    requirement.id, subphase
                ));
            }
        }
    }

    let documented_ids = documented_requirement_ids(&srdd_text, &bigbang_text);
    for id in documented_ids {
        if !registry.requirements.contains_key(&id) {
            errors.push(format!(
                "docs reference requirement {id} but requirements.toml does not register it"
            ));
        }
    }

    for (subphase, file) in &srdd {
        let expected = format!("# {subphase}. SRDD");
        if !file.starts_with(&expected) {
            errors.push(format!("SRDD part {subphase} must begin with {expected:?}"));
        }
    }

    for (path, expected) in generated_bundles(root)? {
        match fs::read_to_string(&path) {
            Ok(actual) if actual == expected => {}
            Ok(_) => errors.push(format!(
                "{} is out of date; run `cargo xtask docs-bundle`",
                path.display()
            )),
            Err(error) => errors.push(format!(
                "{} cannot be read ({error}); run `cargo xtask docs-bundle`",
                path.display()
            )),
        }
    }

    if errors.is_empty() {
        let test_targets: usize = registry
            .requirements
            .values()
            .map(|requirement| requirement.tests.len())
            .sum();
        println!(
            "doc-consistency: schema {} with {} requirements ({test_targets} test targets) across {} SRDD files and {} Big-Bang files",
            registry.schema_version,
            registry.requirements.len(),
            srdd.len(),
            bigbang.len()
        );
        Ok(())
    } else {
        for error in &errors {
            eprintln!("doc-consistency: {error}");
        }
        Err(format!(
            "{} documentation inconsistencies found",
            errors.len()
        ))
    }
}

fn generated_bundles(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    Ok(vec![
        (
            root.join("docs/srdd/LLMRoller Software Requirements and Design Document.md"),
            generate_srdd(root)?,
        ),
        (
            root.join("docs/bigbang/big-bang-phases.md"),
            generate_bigbang(root)?,
        ),
    ])
}

fn read_srdd_parts(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let directory = root.join("docs/srdd");
    let mut files = Vec::new();
    let entries = fs::read_dir(&directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("cannot read entry under {}: {error}", directory.display()))?
            .path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.ends_with(".md") && srdd_part_number(name).is_some() {
            files.push(path);
        }
    }
    files.sort_by_key(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(srdd_part_number)
    });

    let mut parts = BTreeMap::new();
    for path in files {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("non-UTF-8 SRDD filename {}", path.display()))?;
        let number = srdd_part_number(name)
            .ok_or_else(|| format!("invalid SRDD filename {name}"))?
            .to_string();
        let contents = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        parts.insert(number, contents);
    }
    Ok(parts)
}

fn read_bigbang_parts(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let directory = root.join("docs/bigbang");
    let mut parts = BTreeMap::new();
    for name in BIGBANG_PARTS {
        let path = directory.join(name);
        let contents = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        parts.insert(name.to_string(), contents);
    }
    Ok(parts)
}

fn generate_srdd(root: &Path) -> Result<String, String> {
    let parts = read_srdd_parts(root)?;
    let expected = 0..=22;
    let present: BTreeSet<u32> = parts
        .keys()
        .filter_map(|key| key.parse::<u32>().ok())
        .collect();
    for number in expected {
        if !present.contains(&number) {
            return Err(format!("missing SRDD part {number} in docs/srdd"));
        }
    }

    let mut contents = parts
        .iter()
        .map(|(number, text)| {
            let transformed = transform_srdd(text);
            (number.parse::<u32>().unwrap_or(u32::MAX), transformed)
        })
        .collect::<Vec<_>>();
    contents.sort_by_key(|(number, _)| *number);

    let mut bundle = String::from(SRDD_HEADER);
    for (index, (_, text)) in contents.iter().enumerate() {
        if index > 0 {
            bundle.push('\n');
        }
        bundle.push_str(text.trim_end());
        bundle.push('\n');
    }
    Ok(bundle)
}

fn generate_bigbang(root: &Path) -> Result<String, String> {
    let parts = read_bigbang_parts(root)?;
    let mut bundle = String::from(BIGBANG_HEADER);
    for (index, name) in BIGBANG_PARTS.iter().enumerate() {
        let text = parts
            .get(*name)
            .ok_or_else(|| format!("missing Big-Bang part {name}"))?;
        if index > 0 {
            bundle.push('\n');
        }
        bundle.push_str(transform_bigbang(text).trim_end());
        bundle.push('\n');
    }
    Ok(bundle)
}

fn transform_srdd(text: &str) -> String {
    let mut in_fence = false;
    text.lines()
        .map(|line| {
            if is_fence(line) {
                in_fence = !in_fence;
                return line.to_string();
            }
            if in_fence {
                return line.to_string();
            }
            if let Some((part, title)) = srdd_heading(line) {
                format!("## {part}. {title}")
            } else {
                shift_heading(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

fn transform_bigbang(text: &str) -> String {
    let mut in_fence = false;
    text.lines()
        .map(|line| {
            if is_fence(line) {
                in_fence = !in_fence;
                return line.to_string();
            }
            if in_fence {
                line.to_string()
            } else {
                shift_heading(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

fn is_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

fn srdd_heading(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("# ")?;
    let (number, tail) = rest.split_once(". ")?;
    if number.is_empty() || !number.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    let title = tail
        .strip_prefix("SRDD: ")
        .or_else(|| tail.strip_prefix("SRDD - "))?;
    Some((number, title))
}

fn shift_heading(line: &str) -> String {
    let level = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if (1..=5).contains(&level) && line.as_bytes().get(level) == Some(&b' ') {
        format!("#{line}")
    } else {
        line.to_string()
    }
}

fn srdd_part_number(name: &str) -> Option<u32> {
    let (number, rest) = name.split_once(". SRDD")?;
    if !rest.starts_with(" - ") || !rest.ends_with(".md") {
        return None;
    }
    number.parse().ok()
}

fn contains_requirement_id(text: &str, id: &str) -> bool {
    text.split(|character: char| !(character.is_ascii_alphanumeric() || character == '-'))
        .any(|token| token == id)
}

fn documented_requirement_ids(srdd: &str, bigbang: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for text in [srdd, bigbang] {
        for token in
            text.split(|character: char| !(character.is_ascii_alphanumeric() || character == '-'))
        {
            let Some((prefix, number)) = token.split_once('-') else {
                continue;
            };
            if KNOWN_PREFIXES.contains(&prefix)
                && !number.is_empty()
                && number.chars().all(|character| character.is_ascii_digit())
            {
                ids.insert(token.to_string());
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements::Registry;

    #[test]
    fn srdd_headings_are_shifted_and_prefix_is_stripped() {
        let text = "# 2. SRDD: Research findings\n\n## 2.1 Evidence\n\n### Table\n";
        assert_eq!(
            transform_srdd(text),
            "## 2. Research findings\n\n### 2.1 Evidence\n\n#### Table"
        );
    }

    #[test]
    fn bigbang_headings_are_shifted_without_touching_code_fences() {
        let text = "# Epic 1: Foundation\n\n## 1.1 Sub-phase\n\n```\n# not a heading\n```\n";
        assert_eq!(
            transform_bigbang(text),
            "## Epic 1: Foundation\n\n### 1.1 Sub-phase\n\n```\n# not a heading\n```"
        );
    }

    #[test]
    fn srdd_part_names_are_parsed_strictly() {
        assert_eq!(srdd_part_number("22. SRDD - Deployment.md"), Some(22));
        assert_eq!(srdd_part_number("9. SRDD - Data Model.md"), Some(9));
        assert_eq!(srdd_part_number("requirements.toml"), None);
        assert_eq!(srdd_part_number("deployment-and-containers.md"), None);
    }

    #[test]
    fn generated_bundles_are_deterministic() {
        let fixture = Fixture::new();
        let first = generate_srdd(fixture.root()).expect("first bundle");
        let second = generate_srdd(fixture.root()).expect("second bundle");
        assert_eq!(first, second);
        assert!(first.starts_with("<!-- Generated file"));
        assert!(first.contains("## 0. Part 0"));
        assert!(first.contains("## 22. Part 22"));
    }

    #[test]
    fn bigbang_bundle_places_epic_4b_between_4_and_5() {
        let fixture = Fixture::new();
        let bundle = generate_bigbang(fixture.root()).expect("bundle");
        let epic4 = bundle.find("## Part 4\n").expect("epic 4");
        let epic4b = bundle.find("## Part 4b\n").expect("epic 4b");
        let epic5 = bundle.find("## Part 5\n").expect("epic 5");
        assert!(epic4 < epic4b && epic4b < epic5);
    }

    #[test]
    fn check_detects_bundle_drift() {
        let fixture = Fixture::new();
        run(fixture.root(), false).expect("write bundles");
        run(fixture.root(), true).expect("fresh bundles verify");
        let path = fixture.root().join("docs/bigbang/big-bang-phases.md");
        let mut text = fs::read_to_string(&path).expect("bundle");
        text.push_str("\nstale\n");
        fs::write(&path, text).expect("mutate bundle");
        let error = run(fixture.root(), true).expect_err("drift must fail");
        assert!(error.contains("out of date"), "{error}");
    }

    #[test]
    fn doc_consistency_accepts_a_consistent_fixture() {
        let fixture = Fixture::new();
        run(fixture.root(), false).expect("write bundles");
        let registry = fixture.registry();
        doc_consistency(fixture.root(), &registry).expect("fixture is consistent");
    }

    #[test]
    fn doc_consistency_rejects_registered_ids_missing_from_split_docs() {
        let fixture = Fixture::new();
        run(fixture.root(), false).expect("write bundles");
        fs::write(
            fixture.root().join("docs/srdd/6. SRDD - Part 6.md"),
            "# 6. SRDD: Functional\n\nNo requirements here.\n",
        )
        .expect("rewrite srdd");
        let registry = fixture.registry();
        let error = doc_consistency(fixture.root(), &registry).expect_err("missing id must fail");
        assert!(error.contains("inconsistencies"), "{error}");
    }

    struct Fixture {
        directory: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("tempdir");
            let root = directory.path();
            fs::create_dir_all(root.join("docs/srdd")).expect("srdd dir");
            fs::create_dir_all(root.join("docs/bigbang")).expect("bigbang dir");

            for number in 0..=22 {
                let body = if number == 6 {
                    "# 6. SRDD: Functional\n\nAUTH-1 is tested here.\n".to_string()
                } else {
                    format!("# {number}. SRDD: Part {number}\n\nContent {number}.\n")
                };
                fs::write(
                    root.join(format!("docs/srdd/{number}. SRDD - Part {number}.md")),
                    body,
                )
                .expect("srdd part");
            }

            for name in BIGBANG_PARTS {
                let label = name
                    .strip_prefix("big-bang-phases-")
                    .and_then(|name| name.strip_suffix(".md"))
                    .expect("big-bang filename");
                let body = if label == "1" {
                    "# Epic 1\n\n## 1.4 Sub-phase 1.4: Auth\n\nAUTH-1.\n".to_string()
                } else {
                    format!("# Part {label}\n\nPart {label}.\n")
                };
                fs::write(root.join("docs/bigbang").join(name), body).expect("bigbang part");
            }

            fs::write(
                root.join("docs/srdd/requirements.toml"),
                r#"
schema_version = 1

[requirements.AUTH-1]
priority = "P0"
milestone = "M0"
delivering_subphase = "1.4"
tests = ["tests/e2e_setup_and_auth.rs"]
"#,
            )
            .expect("requirements");
            Self { directory }
        }

        fn root(&self) -> &Path {
            self.directory.path()
        }

        fn registry(&self) -> Registry {
            Registry::load(self.root()).expect("registry")
        }
    }
}
