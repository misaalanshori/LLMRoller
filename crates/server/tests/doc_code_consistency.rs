//! Doc-vs-code consistency guards for the three tables that drift silently.
//!
//! Spec: MNT-1, MNT-2
//! Phase: ./docs/bigbang/big-bang-phases-1.md#11-sub-phase-11-cargo-workspace-xtask-governance-tools-foundational-docker-scaffolding-and-multi-arch-ci-matrix
//!
//! # Why these exist
//!
//! `ISSUE-0036` documented four runtime settings keys that do not exist, and `ISSUE-0034` left five
//! documents citing a table count the schema had outgrown. Both survived review because **nothing
//! compiles against a Markdown table**. `xtask doc-consistency` cannot catch either: it validates
//! requirement IDs and test paths, which are identifiers that appear in both docs and
//! `requirements.toml`. A setting name, an exit code and a table count appear in docs alone.
//!
//! So the guards live here, in the test suite, where CI already runs them. Each one reads the
//! **authority** (the settings registry, the exit-code constructors, the migrations) and asserts the
//! **documents** agree with it — never the reverse. That direction is the whole point: the code is
//! authoritative and the prose conforms.
//!
//! These are cheap on purpose. Each is one grep against files that already exist, and each fails
//! with the offending key, code or count named, so the fix is mechanical.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("server crate must live under <workspace>/crates/server")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = workspace_root().join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// Every `.rs` file under `directory`, recursively, excluding test modules.
///
/// The census is about production exit paths, so `tests/` directories and `*_tests.rs` files are
/// skipped — a fixture seeding `ExitCode::from(7)` would otherwise be miscounted as a live code.
fn production_rust_files(directory: &str) -> Vec<PathBuf> {
    fn walk(directory: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "tests") {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && !path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.ends_with("_tests"))
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&workspace_root().join(directory), &mut out);
    out
}

// ---------------------------------------------------------------------------------------------
// 1. Every settings key the README documents must exist in the closed registry (`ISSUE-0036`).
// ---------------------------------------------------------------------------------------------

/// Extract the `settings.key` values the README's runtime-settings table documents.
///
/// The table is located by its heading, so a key mentioned in prose elsewhere in the README is not
/// mistaken for a documented key. Rows are `| \`key\` | default | effect |`.
fn readme_settings_keys() -> BTreeSet<String> {
    let readme = read("README.md");
    let mut keys = BTreeSet::new();
    let mut in_section = false;
    for line in readme.lines() {
        if line.starts_with("### Dynamic Runtime Settings") {
            in_section = true;
            continue;
        }
        if in_section && line.starts_with("### ") {
            break;
        }
        if !in_section || !line.starts_with('|') {
            continue;
        }
        // First cell, backticked.
        let Some(first_cell) = line.split('|').nth(1) else {
            continue;
        };
        let trimmed = first_cell.trim();
        if let Some(inner) = trimmed
            .strip_prefix('`')
            .and_then(|rest| rest.strip_suffix('`'))
        {
            keys.insert(inner.to_owned());
        }
    }
    keys
}

#[test]
fn every_settings_key_the_readme_documents_exists_in_the_closed_registry() {
    let documented = readme_settings_keys();
    assert!(
        !documented.is_empty(),
        "the README runtime-settings table was not found; if it was renamed, update this guard \
         rather than deleting it (ISSUE-0036)"
    );

    let registered: BTreeSet<&str> = llmroller_core::settings::ALL_SETTINGS_KEYS
        .iter()
        .copied()
        .collect();

    let unknown: Vec<&String> = documented
        .iter()
        .filter(|key| !registered.contains(key.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "README documents settings keys that are not in ALL_SETTINGS_KEYS and would be rejected \
         with HTTP 422 `unknown_setting_key`: {unknown:?}\n\
         documented: {documented:?}\n\
         registered: {registered:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. Every exit code the code can return must appear in the `5.5.1` table, and vice versa
//    (`ISSUE-0055`).
// ---------------------------------------------------------------------------------------------

/// Exit codes documented in `5. SRDD`'s section 5.5.1 table.
///
/// Rows are `| \`N\` | meaning |`. Only the numeric-code rows of that table match, because the
/// section's other tables use non-numeric first cells.
fn documented_exit_codes() -> BTreeSet<u8> {
    let doc = read("docs/srdd/5. SRDD - System Architecture.md");
    let mut codes = BTreeSet::new();
    let mut in_section = false;
    for line in doc.lines() {
        if line.starts_with("### 5.5.1") {
            in_section = true;
            continue;
        }
        if in_section && line.starts_with("### ") {
            break;
        }
        if !in_section || !line.starts_with('|') {
            continue;
        }
        let Some(first_cell) = line.split('|').nth(1) else {
            continue;
        };
        if let Some(inner) = first_cell
            .trim()
            .strip_prefix('`')
            .and_then(|rest| rest.strip_suffix('`'))
            && let Ok(code) = inner.parse::<u8>()
        {
            codes.insert(code);
        }
    }
    codes
}

/// Read the run of ASCII digits at `text[from..]`, skipping leading whitespace.
///
/// Whitespace skipping is load-bearing: `rustfmt` breaks a long call onto several lines, so
/// `CliFailure::new(4, ...)` is frequently written as `CliFailure::new(\n            4, ...)`.
/// A line-oriented scanner misses those and silently under-reports the census — which would make a
/// real undocumented code look like a false positive and invite deleting the guard.
fn digits_after(text: &str, from: usize) -> Option<u8> {
    let mut digits = String::new();
    for ch in text[from..].chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if ch.is_whitespace() && digits.is_empty() {
            continue;
        } else {
            break;
        }
    }
    digits.parse().ok()
}

/// Exit codes production code can actually return.
///
/// Four shapes, all covered:
///
/// - `ExitCode::from(<N>)` — a literal code.
/// - `CliFailure::new(<N>, ...)` — the structured CLI failure.
/// - `<EXIT_CONST>` — a named constant, counted only when it is **used** somewhere other than its
///   own definition. A constant that is declared and documented but never referenced is arguably
///   the most important case: `EXIT_WRITER_PANIC` is exactly that, so counting definitions would
///   hide the very defect the census exists to find.
/// - `Self::<Variant> => <N>,` inside a `fn exit_code(&self) -> i32` — the store error mappings.
///   Scoped to `exit_code` bodies so ordinary `match` arms elsewhere are not miscounted.
fn used_exit_codes() -> BTreeSet<u8> {
    let mut files = production_rust_files("crates/server/src");
    files.extend(production_rust_files("crates/store/src"));

    let mut codes = BTreeSet::new();
    for path in files {
        let text = fs::read_to_string(&path).unwrap_or_default();

        // `ExitCode::from(7)` and `ExitCode::from(EXIT_SCHEMA_OLDER)`.
        for needle in ["ExitCode::from("] {
            let mut from = 0usize;
            while let Some(found) = text[from..].find(needle) {
                let after = from + found + needle.len();
                if let Some(code) = digits_after(&text, after) {
                    codes.insert(code);
                }
                from = after;
            }
        }

        // `CliFailure::new(4, ...)`, possibly wrapped across lines.
        let needle = "CliFailure::new(";
        let mut from = 0usize;
        while let Some(found) = text[from..].find(needle) {
            let after = from + found + needle.len();
            if let Some(code) = digits_after(&text, after) {
                codes.insert(code);
            }
            from = after;
        }

        // Named `EXIT_*` constants: count the code when the constant is referenced anywhere other
        // than its own definition.
        for (index, _) in text.match_indices("const EXIT_").collect::<Vec<_>>() {
            let rest = &text[index + "const ".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let Some(eq) = text[index..].find(": u8 = ") else {
                continue;
            };
            let Some(value) = digits_after(&text, index + eq + ": u8 = ".len()) else {
                continue;
            };
            let references = text.matches(&name).count();
            // One occurrence is the definition itself; more means it is genuinely returned.
            if references > 1 {
                codes.insert(value);
            }
        }

        // `fn exit_code` bodies: `Self::Variant => 8,`
        if text.contains("fn exit_code(&self) -> i32") {
            let mut inside = false;
            for line in text.lines() {
                if line.contains("fn exit_code(&self) -> i32") {
                    inside = true;
                    continue;
                }
                if !inside {
                    continue;
                }
                if let Some(index) = line.find("=> ")
                    && let Some(code) = digits_after(line, index + 3)
                {
                    codes.insert(code);
                }
                if line.trim() == "}" {
                    inside = false;
                }
            }
        }
    }
    codes
}

#[test]
fn every_exit_code_production_code_returns_is_documented_in_5_5_1() {
    let documented = documented_exit_codes();
    let used = used_exit_codes();

    assert!(
        !documented.is_empty(),
        "the 5.5.1 exit-code table was not found; update this guard rather than deleting it"
    );
    assert!(
        !used.is_empty(),
        "no exit codes were found in production code; the scanner is broken, not the tree"
    );

    let undocumented: Vec<&u8> = used.difference(&documented).collect();
    assert!(
        undocumented.is_empty(),
        "production code returns exit codes the 5.5.1 table does not document, so a supervisor \
         cannot tell them apart: {undocumented:?}\n\
         documented: {documented:?}\n\
         used: {used:?}"
    );
}

#[test]
fn every_documented_exit_code_is_reachable_or_declared_reserved() {
    let documented = documented_exit_codes();
    let used = used_exit_codes();

    // A documented code with no constructor anywhere is a lie in the other direction: an operator
    // scripting against it waits for something that never happens. `0` is success and is produced
    // by `ExitCode::SUCCESS`, which the literal scanner does not see.
    let unreachable: Vec<&u8> = documented
        .difference(&used)
        .filter(|code| **code != 0)
        .collect();

    if unreachable.is_empty() {
        return;
    }

    // Codes declared with a named constant are reserved for a path that is not yet wired. The
    // table must say so, or a reader cannot distinguish "reserved" from "implemented".
    let doc = read("docs/srdd/5. SRDD - System Architecture.md");
    let mut silently_declared = Vec::new();
    for code in unreachable {
        let row_marker = format!("| `{code}` |");
        let declared = doc
            .lines()
            .any(|line| line.starts_with(&row_marker) && line.to_lowercase().contains("reserved"));
        if !declared {
            silently_declared.push(*code);
        }
    }
    assert!(
        silently_declared.is_empty(),
        "these exit codes are documented in 5.5.1 but no production path returns them, and the \
         table does not mark them `reserved`: {silently_declared:?}\n\
         Either wire the path, or say in the row that the code is reserved so a reader is not \
         scripting against a code that never fires (ISSUE-0055)."
    );
}

// ---------------------------------------------------------------------------------------------
// 3. Every document's table count must equal the migrations' count (`ISSUE-0034`).
// ---------------------------------------------------------------------------------------------

/// The application table count, derived from the migrations themselves.
///
/// `CREATE TABLE` occurrences minus `_llmroller_schema`, which is the migration ledger rather than
/// an application table. `9. SRDD` calls the same subtraction "33 application tables plus schema".
fn migration_application_table_count() -> usize {
    let migrations = workspace_root().join("crates/store/migrations");
    let mut total = 0usize;
    let mut schema_table = 0usize;
    let Ok(entries) = fs::read_dir(&migrations) else {
        panic!("cannot read {}", migrations.display());
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .collect();
    paths.sort();
    for path in paths {
        let text = fs::read_to_string(&path).unwrap_or_default();
        for line in text.lines() {
            let Some(index) = line.find("CREATE TABLE") else {
                continue;
            };
            total += 1;
            let rest = &line[index + "CREATE TABLE".len()..];
            if rest.contains("_llmroller_schema") {
                schema_table += 1;
            }
        }
    }
    assert_eq!(
        schema_table, 1,
        "exactly one `_llmroller_schema` table is expected; found {schema_table}"
    );
    total - schema_table
}

/// Every `N-table` or `N tables` claim in the documents, with the file and line it came from.
fn documented_table_counts() -> Vec<(String, usize, usize)> {
    const DOCUMENTS: [&str; 6] = [
        "README.md",
        "docs/srdd/0. SRDD - Introduction.md",
        "docs/srdd/15. SRDD - Technology Choices and Workspace Layout.md",
        "docs/srdd/17. SRDD - Roadmap and Milestones.md",
        "docs/bigbang/big-bang-phases-1.md",
        "docs/bigbang/big-bang-phases.md",
    ];

    let mut claims = Vec::new();
    for document in DOCUMENTS {
        let text = read(document);
        for (index, line) in text.lines().enumerate() {
            for pattern in ["-table", " tables"] {
                let mut from = 0usize;
                while let Some(found) = line[from..].find(pattern) {
                    let position = from + found;
                    // Walk backwards over the digits immediately before the pattern.
                    let prefix = &line[..position];
                    let digits: String = prefix
                        .chars()
                        .rev()
                        .take_while(char::is_ascii_digit)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    if !digits.is_empty()
                        && let Ok(count) = digits.parse::<usize>()
                    {
                        claims.push((document.to_owned(), index + 1, count));
                    }
                    from = position + pattern.len();
                }
            }
        }
    }
    claims
}

#[test]
fn every_document_cites_the_table_count_the_migrations_create() {
    let actual = migration_application_table_count();
    let claims = documented_table_counts();
    assert!(
        !claims.is_empty(),
        "no table-count claims were found; the scanner or the documents changed"
    );

    let wrong: Vec<String> = claims
        .iter()
        .filter(|(_, _, count)| *count != actual)
        .map(|(document, line, count)| format!("{document}:{line} says {count}"))
        .collect();
    assert!(
        wrong.is_empty(),
        "the migrations create {actual} application tables, but these documents disagree:\n  {}\n\
         The migrations are the authority; make the documents agree with them (ISSUE-0034).",
        wrong.join("\n  ")
    );
}

// ---------------------------------------------------------------------------------------------
// 4. The master-key id is required by the code, so no document may call it optional (`ISSUE-0037`).
// ---------------------------------------------------------------------------------------------

#[test]
fn no_document_offers_a_default_for_the_required_master_key_id() {
    const DOCUMENTS: [&str; 2] = [
        "docs/srdd/5. SRDD - System Architecture.md",
        "docs/srdd/22. SRDD - Deployment and Containers.md",
    ];

    let mut offenders = Vec::new();
    for document in DOCUMENTS {
        let text = read(document);
        for (index, line) in text.lines().enumerate() {
            // Only the environment-variable *schema* rows describe the variable's own contract;
            // a compose file supplying a value is the deployment choosing one, which is legitimate.
            if !line.starts_with('|') || !line.contains("LLMROLLER_MASTER_KEY_ID") {
                continue;
            }
            if line.contains("Optional") || line.contains("mk_default") {
                offenders.push(format!("{document}:{}", index + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "`crates/store/src/crypto.rs` fails closed when `LLMROLLER_MASTER_KEY_ID` is absent or \
         empty (`MasterKeyError::MissingKeyId`/`EmptyKeyId`, exit 3, SEC-2), so no schema row may \
         call it optional or give it a default. Offending rows: {offenders:?}"
    );
}
