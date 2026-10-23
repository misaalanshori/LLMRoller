//! Schema governance: diff canonical SRDD DDL against `0001_initial.sql`.
//!
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md
//! Requirements: MNT-1, MNT-2

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn strip_line_comments(sql: &str) -> String {
    sql.lines()
        .map(|line| match line.find("--") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize(sql: &str) -> String {
    strip_line_comments(sql)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Extract `CREATE TABLE name (...)` with balanced parens, returning
/// (name, normalized column identifier list, normalized full statement).
fn parse_tables(sql: &str) -> BTreeMap<String, (Vec<String>, String)> {
    let mut out = BTreeMap::new();
    let lowered = sql.to_ascii_lowercase();
    let mut search = 0;
    while let Some(idx) = lowered[search..].find("create table") {
        let start = search + idx;
        let rest = &sql[start..];
        // table name = first identifier after CREATE TABLE (+ optional IF NOT EXISTS)
        let after_kw = rest["create table".len()..].trim_start();
        let after_kw = after_kw
            .strip_prefix("if not exists")
            .map(str::trim_start)
            .unwrap_or(after_kw);
        let name: String = after_kw
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '"')
            .collect::<String>()
            .trim_matches('"')
            .to_ascii_lowercase();
        // find opening paren then balance
        let open = match rest.find('(') {
            Some(o) => o,
            None => break,
        };
        let bytes = rest.as_bytes();
        let mut depth = 0;
        let mut end = None;
        for (i, b) in bytes.iter().enumerate().skip(open) {
            if *b == b'(' {
                depth += 1;
            } else if *b == b')' {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
        }
        let Some(end) = end else { break };
        let stmt = rest[..=end].to_string() + ";";
        let body = &rest[open + 1..end];
        // top-level comma split respecting parens
        let mut cols = Vec::new();
        let mut cur = String::new();
        let mut d = 0;
        for ch in body.chars() {
            match ch {
                '(' => {
                    d += 1;
                    cur.push(ch);
                }
                ')' => {
                    d -= 1;
                    cur.push(ch);
                }
                ',' if d == 0 => {
                    let col = column_name(&cur);
                    if let Some(c) = col {
                        cols.push(c);
                    }
                    cur.clear();
                }
                _ => cur.push(ch),
            }
        }
        let col = column_name(&cur);
        if let Some(c) = col {
            cols.push(c);
        }
        if !name.is_empty() {
            out.insert(name, (cols, normalize(&stmt)));
        }
        search = start + end + 1;
        if search >= sql.len() {
            break;
        }
    }
    out
}

fn column_name(def: &str) -> Option<String> {
    let cleaned = strip_line_comments(def);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    let upper = trimmed.to_ascii_uppercase();
    for kw in [
        "PRIMARY KEY",
        "FOREIGN KEY",
        "CHECK",
        "UNIQUE",
        "CONSTRAINT",
    ] {
        if upper.starts_with(kw) {
            return None;
        }
    }
    trimmed
        .split_whitespace()
        .next()
        .map(|s| s.trim_matches('"').to_ascii_lowercase())
}

/// Extract `CREATE [UNIQUE] INDEX [IF NOT EXISTS] name ...;` normalized by name.
fn parse_indexes(sql: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for chunk in sql.split(';') {
        let norm = normalize(chunk);
        if !(norm.starts_with("create index") || norm.starts_with("create unique index")) {
            continue;
        }
        // name follows INDEX [IF NOT EXISTS]
        let after: &str = if let Some(s) = norm.strip_prefix("create unique index") {
            s
        } else {
            &norm["create index".len()..]
        };
        let after = after
            .trim_start()
            .strip_prefix("if not exists")
            .map(str::trim_start)
            .unwrap_or(after.trim_start());
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.insert(name, norm + ";");
        }
    }
    out
}

/// Extract `CREATE TRIGGER name ... END;` normalized by name.
fn parse_triggers(sql: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let lowered = sql.to_ascii_lowercase();
    let mut search = 0;
    while let Some(idx) = lowered[search..].find("create trigger") {
        let start = search + idx;
        let rest_lower = &lowered[start..];
        // trigger ends at first "\nend;" / " end;"
        let rel_end = rest_lower.find("end;");
        let Some(rel) = rel_end else { break };
        let end = start + rel + "end;".len();
        let stmt = sql[start..end.min(sql.len())].to_string();
        let norm = normalize(&stmt);
        let name: String = norm
            .strip_prefix("create trigger")
            .unwrap_or("")
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.insert(name, norm);
        }
        search = end;
        if search >= sql.len() {
            break;
        }
    }
    out
}

struct SchemaView {
    tables: BTreeMap<String, (Vec<String>, String)>,
    indexes: BTreeMap<String, String>,
    triggers: BTreeMap<String, String>,
}

fn load_view(sql_text: &str) -> SchemaView {
    SchemaView {
        tables: parse_tables(sql_text),
        indexes: parse_indexes(sql_text),
        triggers: parse_triggers(sql_text),
    }
}

fn canonical_srdd_sql(root: &Path) -> Result<String, String> {
    let path = root.join("docs/srdd/9. SRDD - Data Model.md");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    // Concatenate all ```sql fenced blocks (schema lives in the first; second
    // block holds seed/operational SQL that must not affect the diff).
    let mut blocks = Vec::new();
    let mut search = 0;
    while let Some(open) = text[search..].find("```sql") {
        let start = search + open + "```sql".len();
        let Some(close) = text[start..].find("```") else {
            break;
        };
        blocks.push(text[start..start + close].to_string());
        search = start + close + 3;
    }
    if blocks.is_empty() {
        return Err(format!("no ```sql blocks in {}", path.display()));
    }
    // The schema block is the one containing CREATE TABLE _llmroller_schema.
    for block in &blocks {
        if block.contains("CREATE TABLE _llmroller_schema") {
            return Ok(block.clone());
        }
    }
    Ok(blocks.remove(0))
}

pub(crate) fn run(root: &Path) -> Result<(), String> {
    let srdd_sql = canonical_srdd_sql(root)?;
    let mig_path = root.join("crates/store/migrations/0001_initial.sql");
    let mig_sql = std::fs::read_to_string(&mig_path)
        .map_err(|e| format!("cannot read {}: {e}", mig_path.display()))?;

    let srdd = load_view(&srdd_sql);
    let mig = load_view(&mig_sql);
    let mut errors = Vec::new();

    let srdd_tables: BTreeSet<_> = srdd.tables.keys().collect();
    let mig_tables: BTreeSet<_> = mig.tables.keys().collect();
    for missing in srdd_tables.difference(&mig_tables) {
        errors.push(format!(
            "table {missing} in SRDD 9.1 but missing in 0001_initial.sql"
        ));
    }
    for extra in mig_tables.difference(&srdd_tables) {
        errors.push(format!(
            "table {extra} in 0001_initial.sql but missing in SRDD 9.1"
        ));
    }
    for table in srdd_tables.intersection(&mig_tables) {
        let (scols, _) = &srdd.tables[*table];
        let (mcols, _) = &mig.tables[*table];
        if scols != mcols {
            errors.push(format!(
                "table {table} column mismatch:\n  SRDD 9.1: {}\n  0001:     {}",
                scols.join(", "),
                mcols.join(", ")
            ));
        }
    }

    let srdd_idx: BTreeSet<_> = srdd.indexes.keys().collect();
    let mig_idx: BTreeSet<_> = mig.indexes.keys().collect();
    for missing in srdd_idx.difference(&mig_idx) {
        errors.push(format!(
            "index {missing} in SRDD 9.1 but missing in 0001_initial.sql"
        ));
    }
    for extra in mig_idx.difference(&srdd_idx) {
        errors.push(format!(
            "index {extra} in 0001_initial.sql but missing in SRDD 9.1"
        ));
    }
    for idx in srdd_idx.intersection(&mig_idx) {
        if srdd.indexes[*idx] != mig.indexes[*idx] {
            errors.push(format!(
                "index {idx} definition differs between SRDD 9.1 and 0001_initial.sql"
            ));
        }
    }

    let srdd_trg: BTreeSet<_> = srdd.triggers.keys().collect();
    let mig_trg: BTreeSet<_> = mig.triggers.keys().collect();
    for missing in srdd_trg.difference(&mig_trg) {
        errors.push(format!(
            "trigger {missing} in SRDD 9.1 but missing in 0001_initial.sql"
        ));
    }
    for extra in mig_trg.difference(&srdd_trg) {
        errors.push(format!(
            "trigger {extra} in 0001_initial.sql but missing in SRDD 9.1"
        ));
    }
    for trg in srdd_trg.intersection(&mig_trg) {
        if srdd.triggers[*trg] != mig.triggers[*trg] {
            errors.push(format!(
                "trigger {trg} definition differs between SRDD 9.1 and 0001_initial.sql"
            ));
        }
    }

    if errors.is_empty() {
        println!(
            "migration-check: SRDD 9.1 and 0001_initial.sql agree ({} tables, {} indexes, {} triggers)",
            srdd.tables.len(),
            srdd.indexes.len(),
            srdd.triggers.len()
        );
        Ok(())
    } else {
        for error in &errors {
            eprintln!("migration-check: {error}");
        }
        Err(format!("{} schema differences found", errors.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizer_collapses_whitespace_and_case() {
        assert_eq!(
            normalize("CREATE  TABLE\n  Foo ( A )"),
            "create table foo ( a )"
        );
    }

    #[test]
    fn table_parser_extracts_columns_and_ignores_constraints() {
        let sql = "CREATE TABLE t ( id TEXT PRIMARY KEY, a INTEGER NOT NULL, CHECK (a > 0) );";
        let tables = parse_tables(sql);
        assert_eq!(tables["t"].0, vec!["id".to_string(), "a".to_string()]);
    }

    #[test]
    fn trigger_parser_captures_named_bodies() {
        let sql = "CREATE TRIGGER trg BEFORE INSERT ON t BEGIN SELECT 1; END;";
        let triggers = parse_triggers(sql);
        assert!(triggers.contains_key("trg"));
    }
}
