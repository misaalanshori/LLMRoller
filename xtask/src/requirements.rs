//! Requirement registry parsing and milestone traceability gates.
//!
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#166-code-test-spec-traceability-matrix-cargo-xtask-traceability
//! Phase: ./docs/bigbang/big-bang-phases-1.md#11-sub-phase-11-cargo-workspace-xtask-governance-tools-foundational-docker-scaffolding-and-multi-arch-ci-matrix

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::KNOWN_PREFIXES;
use crate::source_scan::Scan;

const SUPPORTED_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone)]
pub(crate) struct Requirement {
    pub(crate) id: String,
    pub(crate) priority: String,
    pub(crate) milestone: String,
    pub(crate) delivering_subphase: String,
    pub(crate) partial_subphases: Vec<String>,
    pub(crate) tests: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Registry {
    pub(crate) schema_version: i64,
    pub(crate) requirements: BTreeMap<String, Requirement>,
}

impl Registry {
    pub(crate) fn load(root: &Path) -> Result<Self, String> {
        let path = root.join("docs/srdd/requirements.toml");
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        Self::parse(&text)
    }

    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(text)
            .map_err(|error| format!("requirements.toml is not valid TOML: {error}"))?;
        let schema_version = value
            .get("schema_version")
            .and_then(toml::Value::as_integer)
            .ok_or_else(|| "requirements.toml must define integer schema_version".to_string())?;
        if schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(format!(
                "unsupported requirements.toml schema_version {schema_version}; expected {SUPPORTED_SCHEMA_VERSION}"
            ));
        }

        let table = value
            .get("requirements")
            .and_then(toml::Value::as_table)
            .ok_or_else(|| "requirements.toml must define a [requirements] table".to_string())?;

        let mut requirements = BTreeMap::new();
        for (id, raw) in table {
            validate_requirement_id(id)?;
            let raw = raw
                .as_table()
                .ok_or_else(|| format!("requirement {id} must be a TOML table"))?;
            let priority = required_string(id, raw, "priority")?;
            if !matches!(priority.as_str(), "P0" | "P1" | "P2") {
                return Err(format!(
                    "requirement {id} has invalid priority {priority:?}; expected P0, P1, or P2"
                ));
            }
            let milestone = required_string(id, raw, "milestone")?;
            if !crate::is_milestone(&milestone) {
                return Err(format!(
                    "requirement {id} has invalid milestone {milestone:?}"
                ));
            }
            let delivering_subphase = required_string(id, raw, "delivering_subphase")?;
            validate_subphase(&delivering_subphase)?;
            let partial_subphases = optional_string_array(id, raw, "partial_subphases")?;
            for subphase in &partial_subphases {
                validate_subphase(subphase)?;
            }
            let tests = optional_string_array(id, raw, "tests")?;
            if tests.is_empty() {
                return Err(format!(
                    "requirement {id} must list at least one test target"
                ));
            }
            for test in &tests {
                if test.trim().is_empty() || test.contains('\\') {
                    return Err(format!(
                        "requirement {id} has invalid test path {test:?}; use forward-slash relative paths"
                    ));
                }
            }
            requirements.insert(
                id.clone(),
                Requirement {
                    id: id.clone(),
                    priority,
                    milestone,
                    delivering_subphase,
                    partial_subphases,
                    tests,
                },
            );
        }

        if requirements.is_empty() {
            return Err("requirements.toml contains no requirements".to_string());
        }

        Ok(Self {
            schema_version,
            requirements,
        })
    }

    pub(crate) fn milestone(&self, milestone: &str) -> Vec<&Requirement> {
        self.requirements
            .values()
            .filter(|requirement| requirement.milestone == milestone)
            .collect()
    }

    pub(crate) fn traceability(
        &self,
        milestone: &str,
        scan: &Scan,
        out: &mut impl Write,
    ) -> Result<(), String> {
        self.reject_unknown_annotations(scan)?;

        let selected = self.milestone(milestone);
        if selected.is_empty() {
            return Err(format!(
                "no requirements are assigned to milestone {milestone}"
            ));
        }

        let mut missing = Vec::new();
        let mut partial_only = Vec::new();
        for requirement in &selected {
            let full_links = scan.spec_links(&requirement.id);
            let partial_links = scan.partial_links(&requirement.id);
            if full_links == 0 {
                if partial_links > 0 {
                    partial_only.push(requirement);
                } else {
                    missing.push(requirement);
                }
            }
        }

        let traced = selected.len() - missing.len() - partial_only.len();
        writeln!(
            out,
            "{milestone}: {traced}/{} requirements have full Spec links ({} partial-only, {} missing)",
            selected.len(),
            partial_only.len(),
            missing.len()
        )
        .map_err(|error| format!("cannot write traceability report: {error}"))?;

        for requirement in &partial_only {
            writeln!(
                out,
                "PARTIAL [{}] {} (delivering {}): only Partial links; a milestone gate requires #[doc = \"Spec: {}\"]",
                requirement.priority, requirement.id, requirement.delivering_subphase, requirement.id
            )
            .map_err(|error| format!("cannot write traceability report: {error}"))?;
        }
        for requirement in &missing {
            writeln!(
                out,
                "MISSING [{}] {} (delivering {}): no #[doc = \"Spec: {}\"] link",
                requirement.priority,
                requirement.id,
                requirement.delivering_subphase,
                requirement.id
            )
            .map_err(|error| format!("cannot write traceability report: {error}"))?;
        }

        if missing.is_empty() && partial_only.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "milestone {milestone} traceability gate failed: {} full, {} partial-only, {} missing",
                traced,
                partial_only.len(),
                missing.len()
            ))
        }
    }

    fn reject_unknown_annotations(&self, scan: &Scan) -> Result<(), String> {
        let mut unknown: BTreeMap<&str, &Path> = BTreeMap::new();
        for id in scan.all_ids() {
            if !self.requirements.contains_key(id)
                && let Some(path) = scan.path_for(id)
            {
                unknown.insert(id, path);
            }
        }
        if unknown.is_empty() {
            return Ok(());
        }
        let details = unknown
            .iter()
            .map(|(id, path)| format!("{id} ({})", path.display()))
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "Rust annotations reference requirement IDs absent from requirements.toml: {details}"
        ))
    }
}

fn required_string(
    id: &str,
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
) -> Result<String, String> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("requirement {id} must define string {key}"))
}

fn optional_string_array(
    id: &str,
    table: &toml::map::Map<String, toml::Value>,
    key: &str,
) -> Result<Vec<String>, String> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("requirement {id}.{key} must be an array of strings"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("requirement {id}.{key} must contain only strings"))
        })
        .collect()
}

fn validate_requirement_id(id: &str) -> Result<(), String> {
    let Some((prefix, number)) = id.split_once('-') else {
        return Err(format!("requirement ID {id:?} must use <PREFIX>-<number>"));
    };
    if !KNOWN_PREFIXES.contains(&prefix) {
        return Err(format!(
            "requirement ID {id:?} uses unknown prefix {prefix:?}"
        ));
    }
    if number.is_empty() || !number.chars().all(|character| character.is_ascii_digit()) {
        return Err(format!(
            "requirement ID {id:?} must end with a decimal number"
        ));
    }
    Ok(())
}

fn validate_subphase(value: &str) -> Result<(), String> {
    let Some((major, minor)) = value.split_once('.') else {
        return Err(format!(
            "invalid sub-phase {value:?}; expected <major>.<minor>"
        ));
    };
    let major_digits = major
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let major_suffix = &major[major_digits.len()..];
    let minor_digits = minor
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    let minor_suffix = &minor[minor_digits.len()..];
    let valid_suffix = |suffix: &str| {
        suffix.is_empty() || (suffix.len() == 1 && suffix.chars().all(|c| c.is_ascii_lowercase()))
    };
    if major_digits.is_empty()
        || minor_digits.is_empty()
        || !valid_suffix(major_suffix)
        || !valid_suffix(minor_suffix)
    {
        return Err(format!(
            "invalid sub-phase {value:?}; expected <major>.<minor>"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_scan::Scan;
    use std::path::PathBuf;

    const REGISTRY: &str = r#"
schema_version = 1

[requirements.AUTH-1]
priority = "P0"
milestone = "M0"
delivering_subphase = "1.4"
tests = ["tests/e2e_setup_and_auth.rs"]

[requirements.SEC-1]
priority = "P0"
milestone = "M0"
delivering_subphase = "1.3"
partial_subphases = ["1.1"]
tests = ["crates/store/src/crypto/tests.rs"]

[requirements.RTE-4]
priority = "P0"
milestone = "M2"
delivering_subphase = "3.5"
tests = ["crates/core/src/wrr/proptests.rs"]
"#;

    #[test]
    fn parses_registry_and_indexes_milestones() {
        let registry = Registry::parse(REGISTRY).expect("registry must parse");
        assert_eq!(registry.schema_version, 1);
        assert_eq!(registry.requirements.len(), 3);
        assert_eq!(registry.milestone("M0").len(), 2);
        assert_eq!(registry.milestone("M2").len(), 1);
        assert!(registry.milestone("M1").is_empty());
    }

    #[test]
    fn rejects_unknown_prefix_and_missing_fields() {
        let unknown = REGISTRY.replace("AUTH-1", "WAT-1");
        let error = Registry::parse(&unknown).expect_err("unknown prefix must fail");
        assert!(error.contains("unknown prefix"), "{error}");

        let missing = REGISTRY.replace("priority = \"P0\"\n", "");
        let error = Registry::parse(&missing).expect_err("missing priority must fail");
        assert!(error.contains("must define string priority"), "{error}");
    }

    #[test]
    fn milestone_gate_rejects_partial_only_and_missing_links() {
        let registry = Registry::parse(REGISTRY).expect("registry must parse");
        let mut scan = Scan::default();
        scan.record("AUTH-1", true, PathBuf::from("a.rs"));
        scan.record("SEC-1", false, PathBuf::from("b.rs"));
        let mut output = Vec::new();
        let error = registry
            .traceability("M0", &scan, &mut output)
            .expect_err("partial-only must fail");
        assert!(
            error.contains("1 full, 1 partial-only, 0 missing"),
            "{error}"
        );
        let report = String::from_utf8(output).expect("utf-8 report");
        assert!(report.contains("PARTIAL [P0] SEC-1"), "{report}");
    }

    #[test]
    fn milestone_gate_passes_when_every_requirement_has_a_spec_link() {
        let registry = Registry::parse(REGISTRY).expect("registry must parse");
        let mut scan = Scan::default();
        scan.record("AUTH-1", true, PathBuf::from("a.rs"));
        scan.record("SEC-1", true, PathBuf::from("b.rs"));
        let mut output = Vec::new();
        registry
            .traceability("M0", &scan, &mut output)
            .expect("full coverage must pass");
        assert!(
            String::from_utf8(output)
                .expect("utf-8 report")
                .contains("2/2 requirements")
        );
    }

    #[test]
    fn unknown_annotation_ids_are_rejected() {
        let registry = Registry::parse(REGISTRY).expect("registry must parse");
        let mut scan = Scan::default();
        scan.record("AUTH-1", true, PathBuf::from("a.rs"));
        scan.record("SEC-1", true, PathBuf::from("b.rs"));
        scan.record("AUTH-99", true, PathBuf::from("typo.rs"));
        let mut output = Vec::new();
        let error = registry
            .traceability("M0", &scan, &mut output)
            .expect_err("unknown ID must fail");
        assert!(error.contains("AUTH-99"), "{error}");
    }
}
