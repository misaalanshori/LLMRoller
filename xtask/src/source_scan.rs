//! Rust test-annotation scanner for `#[doc = "Spec: ..."]` links.
//!
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#166-code-test-spec-traceability-matrix-cargo-xtask-traceability
//! Phase: ./docs/bigbang/big-bang-phases-1.md#11-sub-phase-11-cargo-workspace-xtask-governance-tools-foundational-docker-scaffolding-and-multi-arch-ci-matrix

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::KNOWN_PREFIXES;

const SCAN_ROOTS: [&str; 5] = ["crates", "tests", "xtask", "benches", "fuzz"];
const SKIPPED_DIRECTORIES: [&str; 3] = ["target", ".git", "mutants.out"];

#[derive(Debug, Default)]
pub(crate) struct Scan {
    spec: BTreeMap<String, BTreeSet<PathBuf>>,
    partial: BTreeMap<String, BTreeSet<PathBuf>>,
    unjustified_mutants_skips: Vec<String>,
    empty_test_stubs: Vec<String>,
    pub(crate) files_scanned: usize,
}

impl Scan {
    pub(crate) fn scan_workspace(root: &Path) -> Result<Self, String> {
        let mut scan = Self::default();
        for relative in SCAN_ROOTS {
            let directory = root.join(relative);
            if directory.is_dir() {
                scan.walk(&directory)?;
            }
        }
        Ok(scan)
    }

    pub(crate) fn record(&mut self, id: &str, full_spec: bool, path: PathBuf) {
        let target = if full_spec {
            &mut self.spec
        } else {
            &mut self.partial
        };
        target.entry(id.to_string()).or_default().insert(path);
    }

    pub(crate) fn spec_links(&self, id: &str) -> usize {
        self.spec.get(id).map_or(0, BTreeSet::len)
    }

    pub(crate) fn partial_links(&self, id: &str) -> usize {
        self.partial.get(id).map_or(0, BTreeSet::len)
    }

    pub(crate) fn has_spec_link_in_paths(&self, id: &str, allowed_rel_paths: &[String]) -> bool {
        let Some(recorded_paths) = self.spec.get(id) else {
            return false;
        };
        recorded_paths.iter().any(|recorded| {
            let norm = recorded.to_string_lossy().replace('\\', "/");
            allowed_rel_paths
                .iter()
                .any(|allowed| norm == *allowed || norm.ends_with(&format!("/{allowed}")))
        })
    }

    pub(crate) fn all_ids(&self) -> BTreeSet<&str> {
        self.spec
            .keys()
            .chain(self.partial.keys())
            .map(String::as_str)
            .collect()
    }

    pub(crate) fn path_for(&self, id: &str) -> Option<&Path> {
        self.spec
            .get(id)
            .or_else(|| self.partial.get(id))
            .and_then(|paths| paths.iter().next())
            .map(PathBuf::as_path)
    }

    pub(crate) fn reject_unjustified_mutants_skips(&self) -> Result<(), String> {
        if self.unjustified_mutants_skips.is_empty() {
            return Ok(());
        }
        Err(format!(
            "every #[mutants::skip] requires an `equivalent:` reason comment:\n  {}",
            self.unjustified_mutants_skips.join("\n  ")
        ))
    }

    pub(crate) fn reject_empty_test_stubs(&self) -> Result<(), String> {
        if self.empty_test_stubs.is_empty() {
            return Ok(());
        }
        Err(format!(
            "empty stub test functions are forbidden by TDD governance:\n  {}",
            self.empty_test_stubs.join("\n  ")
        ))
    }

    fn walk(&mut self, directory: &Path) -> Result<(), String> {
        let entries = fs::read_dir(directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!("cannot read entry under {}: {error}", directory.display())
            })?;
            let path = entry.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if !SKIPPED_DIRECTORIES.contains(&name) {
                    self.walk(&path)?;
                }
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                self.scan_file(&path)?;
            }
        }
        Ok(())
    }

    fn scan_file(&mut self, path: &Path) -> Result<(), String> {
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        self.files_scanned += 1;
        let lines = contents.lines().collect::<Vec<_>>();
        for line in &lines {
            let Some(value) = doc_attribute_value(line) else {
                continue;
            };
            for (id, full_spec) in parse_annotation(value) {
                self.record(&id, full_spec, path.to_path_buf());
            }
        }
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[test]")
                || trimmed.starts_with("#[tokio::test")
                || trimmed.starts_with("#[sqlx::test")
            {
                for candidate_idx in (index + 1)..(index + 7).min(lines.len()) {
                    let candidate = lines[candidate_idx].trim();
                    if candidate.starts_with("#[") || candidate.starts_with("//") {
                        continue;
                    }
                    if candidate.starts_with("fn ") || candidate.starts_with("async fn ") {
                        let empty_inline = candidate.ends_with("{}") || candidate.ends_with("{ }");
                        let empty_two_line = candidate.ends_with('{')
                            && lines
                                .get(candidate_idx + 1)
                                .is_some_and(|next| next.trim() == "}");
                        if empty_inline || empty_two_line {
                            self.empty_test_stubs.push(format!(
                                "{}:{}",
                                path.display(),
                                candidate_idx + 1
                            ));
                        }
                    }
                    break;
                }
            }
            if !trimmed.starts_with("#[mutants::skip]") {
                continue;
            }
            let start = index.saturating_sub(1);
            let end = (index + 2).min(lines.len());
            let justified = lines[start..end]
                .iter()
                .any(|candidate| candidate.contains("equivalent:"));
            if !justified {
                self.unjustified_mutants_skips
                    .push(format!("{}:{}", path.display(), index + 1));
            }
        }
        Ok(())
    }
}

fn doc_attribute_value(line: &str) -> Option<&str> {
    let start = line
        .find("#[doc = \"")
        .or_else(|| line.find("#![doc = \""))?;
    let marker_len = if line[start..].starts_with("#![doc") {
        "#![doc = \"".len()
    } else {
        "#[doc = \"".len()
    };
    let after = line.get(start + marker_len..)?;
    let end = after.find('"')?;
    after.get(..end)
}

fn parse_annotation(value: &str) -> Vec<(String, bool)> {
    let mut markers = Vec::new();
    for (index, _) in value.match_indices("Spec:") {
        markers.push((index, true, "Spec:".len()));
    }
    for (index, _) in value.match_indices("Partial:") {
        markers.push((index, false, "Partial:".len()));
    }
    markers.sort_by_key(|(index, _, _)| *index);

    let mut links = Vec::new();
    for (position, (index, full_spec, marker_len)) in markers.iter().enumerate() {
        let start = index + marker_len;
        let end = markers
            .get(position + 1)
            .map_or(value.len(), |(next_index, _, _)| *next_index);
        let Some(segment) = value.get(start..end) else {
            continue;
        };
        for id in extract_ids(segment) {
            links.push((id, *full_spec));
        }
    }
    links
}

fn extract_ids(segment: &str) -> Vec<String> {
    segment
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '-'))
        .filter(|token| {
            token.split_once('-').is_some_and(|(prefix, number)| {
                KNOWN_PREFIXES.contains(&prefix)
                    && !prefix.is_empty()
                    && prefix
                        .chars()
                        .all(|character| character.is_ascii_uppercase())
                    && !number.is_empty()
                    && number.chars().all(|character| character.is_ascii_digit())
            })
        })
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_spec_and_partial_segments_without_cross_contamination() {
        let links = parse_annotation("Spec: MNT-1, MNT-2, Partial: POR-1, Partial: SEC-11");
        assert_eq!(
            links,
            vec![
                ("MNT-1".to_string(), true),
                ("MNT-2".to_string(), true),
                ("POR-1".to_string(), false),
                ("SEC-11".to_string(), false),
            ]
        );
    }

    #[test]
    fn ignores_prose_tokens_that_are_not_requirement_ids() {
        let links = parse_annotation("Spec: SEC-1, regression G1..G7 and ADR-12");
        assert_eq!(links, vec![("SEC-1".to_string(), true)]);
    }

    #[test]
    fn scans_files_and_skips_target_directories() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        fs::create_dir_all(root.join("crates/core/src")).expect("source dir");
        fs::create_dir_all(root.join("target/generated")).expect("target dir");
        fs::write(
            root.join("crates/core/src/lib.rs"),
            "#[doc = \"Spec: SEC-1\"]\n#[doc = \"Partial: SEC-2\"]\n",
        )
        .expect("source file");
        fs::write(
            root.join("target/generated/ignored.rs"),
            "#[doc = \"Spec: SEC-3\"]",
        )
        .expect("ignored file");

        let scan = Scan::scan_workspace(root).expect("scan");
        assert_eq!(scan.files_scanned, 1);
        assert_eq!(scan.spec_links("SEC-1"), 1);
        assert_eq!(scan.partial_links("SEC-2"), 1);
        assert_eq!(scan.spec_links("SEC-3"), 0);
    }

    #[test]
    fn flags_mutants_skip_without_an_equivalent_reason() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        fs::create_dir_all(root.join("crates/core/src")).expect("source dir");
        fs::write(
            root.join("crates/core/src/lib.rs"),
            concat!(
                "#[mutants::skip]\n",
                "fn unjustified() {}\n",
                "#[mutants::skip] // equivalent: only reachable after abort\n",
                "fn same_line() {}\n",
                "#[mutants::skip]\n",
                "// equivalent: generated code\n",
                "fn next_line() {}\n",
                "const MENTION: &str = \"#[mutants::skip]\";\n",
            ),
        )
        .expect("source file");

        let scan = Scan::scan_workspace(root).expect("scan");
        let error = scan
            .reject_unjustified_mutants_skips()
            .expect_err("unjustified skip must fail");
        assert!(error.contains(":1"), "{error}");
        assert!(error.contains("every #[mutants::skip]"), "{error}");
    }

    #[test]
    #[doc = "Spec: MNT-1"]
    fn flags_empty_test_stubs_and_checks_registered_test_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        fs::create_dir_all(root.join("crates/core/src")).expect("source dir");
        fs::write(
            root.join("crates/core/src/lib.rs"),
            concat!(
                "#[test]\n",
                "#[doc = \"Spec: SEC-1\"]\n",
                "fn empty_stub() {}\n",
            ),
        )
        .expect("source file");

        let scan = Scan::scan_workspace(root).expect("scan");
        let err = scan
            .reject_empty_test_stubs()
            .expect_err("empty test stub must fail");
        assert!(err.contains("empty stub test functions"), "{err}");
        assert!(scan.has_spec_link_in_paths("SEC-1", &["crates/core/src/lib.rs".to_string()]));
        assert!(!scan.has_spec_link_in_paths("SEC-1", &["crates/other/src/lib.rs".to_string()]));
    }

    #[test]
    fn deduplicates_repeated_links_in_one_file() {
        let mut scan = Scan::default();
        let path = PathBuf::from("crates/core/src/lib.rs");
        scan.record("SEC-1", true, path.clone());
        scan.record("SEC-1", true, path);
        assert_eq!(scan.spec_links("SEC-1"), 1);
    }
}
