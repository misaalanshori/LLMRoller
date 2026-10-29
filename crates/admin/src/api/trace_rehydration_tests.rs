//! Unit tests for the trace block reference walker and substitution.
//!
//! These drive [`collect_block_hashes`] and [`substitute`] directly, because the interesting cases
//! are JSON shapes — a marker nested in an array, the same hash at several positions, a marker with
//! sibling keys — that a database fixture would take far longer to express. The endpoint-level
//! behaviour (that the handler calls this, and that the owner predicate is enforced by SQL) is
//! covered by `crates/server/tests/integration_trace_block_rehydration.rs`.
//!
//! Both halves are needed: these pin the *shape handling*, that file pins the *wiring and the
//! tenant boundary*. Cycle 39 showed a correct extracted function with no caller passes every unit
//! test.

use super::*;
use std::collections::HashMap;

fn content(text: &str) -> ResolvedBlock {
    ResolvedBlock::Content(serde_json::Value::String(text.to_string()))
}

#[test]
fn collects_a_marker_from_a_top_level_field() {
    let value = serde_json::json!({ "system": { "$blk": "aaa" } });
    assert_eq!(collect_block_hashes(&value), vec!["aaa".to_string()]);
}

#[test]
fn collects_markers_nested_in_arrays_and_objects() {
    // The real skeleton nests one level deeper still: `tools` is an array of objects each holding
    // `function` and `parameters`, and those are what get hashed.
    let value = serde_json::json!({
        "messages": [
            { "$blk": "m1" },
            { "role": "user", "content": { "$blk": "m2" } }
        ],
        "tools": [{ "function": { "parameters": { "$blk": "t1" } } }],
    });
    let found = collect_block_hashes(&value);
    for expected in ["m1", "m2", "t1"] {
        assert!(
            found.contains(&expected.to_string()),
            "must find {expected} nested inside arrays/objects; got {found:?}"
        );
    }
}

#[test]
fn the_same_hash_at_many_positions_is_collected_once() {
    // `block_refs` keeps one entry per prompt position for LCP alignment, so a repeated block is
    // normal. Fetching it once per position would be wasteful and would make the batch pointless.
    let value = serde_json::json!([
        { "$blk": "same" },
        { "$blk": "same" },
        { "$blk": "other" },
        { "$blk": "same" }
    ]);
    assert_eq!(
        collect_block_hashes(&value),
        vec!["same".to_string(), "other".to_string()]
    );
}

#[test]
fn a_marker_that_is_not_a_string_is_ignored() {
    // A producer writing `{"$blk": 42}` is a bug, but the reader must not hash "42" or panic.
    let value = serde_json::json!({ "system": { "$blk": 42 }, "x": { "$blk": null } });
    assert!(collect_block_hashes(&value).is_empty());
}

#[test]
fn a_tree_with_no_markers_collects_nothing() {
    let value = serde_json::json!({ "model": "gpt-4o", "messages": ["plain"] });
    assert!(collect_block_hashes(&value).is_empty());
}

#[test]
fn substitution_replaces_a_marker_with_its_content() {
    let mut value = serde_json::json!({ "system": { "$blk": "aaa" } });
    let mut resolved = HashMap::new();
    resolved.insert("aaa".to_string(), content("SYSTEM"));
    substitute(&mut value, &resolved);
    assert_eq!(value, serde_json::json!({ "system": "SYSTEM" }));
}

#[test]
fn a_missing_block_keeps_its_hash_and_is_marked_missing() {
    // Never dropped and never `null`: `null` renders as empty content, which is an invented value.
    // The hash stays so an operator can correlate with the written record.
    let mut value = serde_json::json!({ "system": { "$blk": "gone" } });
    substitute(&mut value, &HashMap::new());
    assert_eq!(
        value,
        serde_json::json!({ "system": { "$blk": "gone", "$unresolved": "missing_block" } })
    );
}

#[test]
fn a_decrypt_failure_is_marked_differently_from_a_missing_block() {
    let mut value = serde_json::json!({ "system": { "$blk": "bad" } });
    let mut resolved = HashMap::new();
    resolved.insert("bad".to_string(), ResolvedBlock::DecryptFailed);
    substitute(&mut value, &resolved);
    assert_eq!(
        value,
        serde_json::json!({ "system": { "$blk": "bad", "$unresolved": "decrypt_failed" } })
    );
}

#[test]
fn substitution_reaches_markers_in_arrays_and_leaves_other_values_alone() {
    let mut value = serde_json::json!({
        "model": "gpt-4o",
        "temperature": 0.7,
        "messages": [{ "$blk": "a" }, "plain", { "$blk": "b" }],
        "response": { "text": "answer" },
    });
    let mut resolved = HashMap::new();
    resolved.insert("a".to_string(), content("first"));
    resolved.insert("b".to_string(), content("second"));
    substitute(&mut value, &resolved);
    assert_eq!(value["messages"][0], "first");
    assert_eq!(value["messages"][1], "plain");
    assert_eq!(value["messages"][2], "second");
    // Everything that was not a marker is untouched, including numeric types.
    assert_eq!(value["model"], "gpt-4o");
    assert_eq!(value["temperature"], 0.7);
    assert_eq!(value["response"]["text"], "answer");
}

#[test]
fn a_marker_carrying_sibling_keys_is_still_replaced() {
    // The write path emits bare `{"$blk": …}`, but a reader that required the object to have
    // exactly one key would silently skip a reference that ever gained metadata.
    let mut value = serde_json::json!({ "system": { "$blk": "aaa", "note": "extra" } });
    let mut resolved = HashMap::new();
    resolved.insert("aaa".to_string(), content("SYSTEM"));
    substitute(&mut value, &resolved);
    assert_eq!(value["system"], "SYSTEM");
}

#[test]
fn collect_then_substitute_resolves_everything_it_collected() {
    // Round-trip property: whatever the walker found is exactly what the substitution then
    // replaces. A mismatch between the two walks would leave a marker behind even though the hash
    // had been fetched — the failure mode this pairing exists to prevent.
    let mut value = serde_json::json!({
        "system": { "$blk": "s" },
        "tools": [{ "$blk": "t" }],
        "messages": [{ "$blk": "m" }],
        "x_unknown": { "$blk": "u" },
    });
    let hashes = collect_block_hashes(&value);
    assert_eq!(hashes.len(), 4);
    let resolved: HashMap<String, ResolvedBlock> =
        hashes.into_iter().map(|h| (h, content("V"))).collect();
    substitute(&mut value, &resolved);
    assert!(
        collect_block_hashes(&value).is_empty(),
        "no marker may survive when every collected hash resolved: {value}"
    );
}

#[test]
fn the_chunk_bound_stays_well_below_sqlite_variable_limits() {
    // SQLite's SQLITE_MAX_VARIABLE_NUMBER is 32766 in modern builds and 999 in older ones. The
    // chunk adds one bind for `owner_user_id`, so the bound must leave room even for the older
    // limit or the statement becomes invalid once a turn references many blocks.
    // The bound plus the one `owner_user_id` bind must fit the older limit of 999. Written as a
    // computed comparison so it stays a live check rather than a constant the compiler folds away.
    let binds_per_statement = std::cmp::max(BLOCK_FETCH_CHUNK, 1).saturating_add(1);
    assert!(
        binds_per_statement <= 999,
        "chunk {BLOCK_FETCH_CHUNK} plus the owner bind is {binds_per_statement}, over the older \
         SQLite variable limit"
    );
}
