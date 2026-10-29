//! Rehydration of `{"$blk": …}` block references in a stored turn trace (`ISSUE-0165`).
//!
//! Spec: ./docs/srdd/12. SRDD - Protocol Handling and Transparent Proxying.md
//! Spec: ./docs/srdd/14.u. SRDD - Read-Only Session Chat Preview and Turn Trace Inspector UI.md
//! Requirements: TRC-2, TRC-4, USR-3, SEC-17
//!
//! # What it is for
//!
//! `prepare_request_blocks` (`crates/proto/src/trace_tap.rs`) stores a request as a *skeleton* plus
//! content-addressed blocks: every prompt-bearing field (`system`, `instructions`, `tools`,
//! `messages`, `input`) is replaced in place by `{"$blk": "<sha256>"}`, and the bytes live in
//! `trace_blocks`. Reading a trace therefore has to turn those markers back into content.
//!
//! `get_trace_record_internal` did not, so `GET /api/user/traces/{request_id}` returned a trace
//! whose prompt was a list of hashes, while `get_session_preview_internal` in the same file
//! resolved them correctly. Two read paths, two dialects.
//!
//! # Why a tree walk and not a field list
//!
//! The markers live wherever `prepare_request_blocks` put them, which is a fixed list *today* but
//! not something a reader of a trace should have to know. A field-list implementation resolves
//! `system`/`tools`/`messages` and leaves a marker in any field added later, silently — the class of
//! bug that survives every test the writer of that list thought to write. So the collection is a
//! depth-first walk over objects and arrays (the same shape as cycle 40's redaction walker), and the
//! substitution reuses it.
//!
//! # Three absences that must not collapse
//!
//! - **missing** — no `trace_blocks` row. The marker is replaced by an object that keeps the hash
//!   and says `"$unresolved": "missing_block"`, so an operator can tell "the block is gone" from
//!   "the block was empty". It is never dropped and never `null`: `null` renders as *empty content*,
//!   which is an invented value.
//! - **decrypt failure** — a row exists but cannot be opened. Reported as
//!   `"$unresolved": "decrypt_failed"`, **distinctly**, because conflating it with absence hides a
//!   key-rotation fault behind a retention story.
//! - **another tenant's block** — rendered exactly like **missing**. A block hash is not a secret:
//!   it appears in `$blk` markers throughout every trace, so any reader of a trace already knows it.
//!   This is why the fetch query carries `AND owner_user_id = ?` and why that predicate is
//!   load-bearing rather than defensive.
//!
//! # Why this is a second block-reading site rather than a shared helper
//!
//! `get_session_preview_internal` reads the same `trace_blocks` rows, and the duplication is
//! deliberate. Its block handling is not the same operation:
//!
//! - it resolves `block_refs[lcp..]` **positionally** into a flat `new_input_messages` array, not
//!   into the JSON tree that holds the markers;
//! - a missing row becomes `{"role": "system", "content": "content expired", "expired": true}`;
//! - a decrypt failure is a hard `500` (`map_err(|e| map_price_error(...))?`), not a marker.
//!
//! A helper general enough to serve both would need to carry the renderings, the failure policy and
//! the traversal shape as parameters, which is more surface than the two call sites. The cycle's
//! instruction was that the preview's output must not change and that a shared helper which would
//! change it must not be extracted — so the one thing the two sites genuinely share (the
//! `trace_block` AAD triple, `master_key_id || owner_user_id || block_hash`) is documented here and
//! nothing else. Two dialects would be a smell; two *different* presentations of the same blocks,
//! each correct for its own reader, is not.
//!
//! # Failure never fails the read
//!
//! A single unresolvable block must not turn the whole trace into a `500`. The trace is the record
//! an operator consults *during* an incident; a partial answer with the gaps marked is far more
//! useful than an error page.

use super::*;
use llmroller_core::seams::ExposeSecret as _;
use llmroller_store::crypto::SecretSealer as _;
use sqlx::Row as _;

/// Key holding the hash in a block reference (`trace_tap.rs`).
const BLOCK_REF_KEY: &str = "$blk";
/// Key a substitute object carries to say why it is unresolved.
const UNRESOLVED_KEY: &str = "$unresolved";
/// Renderings for the two distinct failure modes.
const UNRESOLVED_MISSING: &str = "missing_block";
const UNRESOLVED_DECRYPT_FAILED: &str = "decrypt_failed";

/// Maximum number of hashes per `IN (...)` batch.
///
/// SQLite's `SQLITE_MAX_VARIABLE_NUMBER` defaults to 32766 in modern builds and was 999 in older
/// ones; the bound used here is far below both, and also keeps each statement's SQL text small.
/// A turn cannot plausibly reference more distinct blocks than this — `trace_tap` caps unique block
/// bytes at 4 MiB per turn — so in practice one batch is always enough. The bound exists so the
/// statement stays valid even if that cap ever rises.
pub(crate) const BLOCK_FETCH_CHUNK: usize = 500;

/// Collect every distinct `{"$blk": <hash>}` in `value`, depth-first.
///
/// Distinct means first-seen order is preserved but each hash appears once: the same block can be
/// referenced from many prompt positions (`block_refs` keeps one entry per position for LCP
/// alignment), and fetching it once is the point of a batch.
#[must_use]
pub(crate) fn collect_block_hashes(value: &serde_json::Value) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    fn walk(value: &serde_json::Value, found: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(fields) => {
                // A reference object carries the hash and nothing else worth descending into,
                // but the walk continues rather than returning: a producer that ever nests extra
                // keys under a marker must not have them skipped.
                if let Some(hash) = fields
                    .get(BLOCK_REF_KEY)
                    .and_then(serde_json::Value::as_str)
                    && !found.iter().any(|seen| seen == hash)
                {
                    found.push(hash.to_string());
                }
                for (_, nested) in fields {
                    walk(nested, found);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, found);
                }
            }
            _ => {}
        }
    }
    walk(value, &mut found);
    found
}

/// What a fetched block turned out to be.
enum ResolvedBlock {
    /// Opened successfully; the payload as parsed JSON, or as text when it is not JSON.
    Content(serde_json::Value),
    /// A row exists but could not be opened (AEAD failure, unknown master key id).
    DecryptFailed,
}

/// Fetch and open `hashes` for `owner_user_id`, chunked by [`BLOCK_FETCH_CHUNK`].
///
/// **`AND owner_user_id = ?` is the tenant boundary.** A hash is not a secret, so dropping it would
/// let any trace reader fetch another tenant's prompt block by referencing its hash (`USR-3`,
/// `SEC-17`). A hash owned by someone else simply is not returned, which makes it indistinguishable
/// from one that was swept — the required behaviour.
async fn fetch_blocks(
    state: &ApiAppState,
    owner_user_id: &str,
    hashes: &[String],
    sealer: &llmroller_store::crypto::XChaCha20Poly1305Sealer,
) -> Result<std::collections::HashMap<String, ResolvedBlock>, (StatusCode, Json<serde_json::Value>)>
{
    let mut resolved = std::collections::HashMap::with_capacity(hashes.len());
    for chunk in hashes.chunks(BLOCK_FETCH_CHUNK) {
        let sql = format!(
            "SELECT block_hash, payload_enc, master_key_id
               FROM trace_blocks
              WHERE owner_user_id = ? AND block_hash IN ({})",
            crate::api::active_pins::placeholders(chunk.len())
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(owner_user_id);
        for hash in chunk {
            query = query.bind(hash);
        }
        let rows = query
            .fetch_all(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

        for row in rows {
            let hash: String = row.try_get("block_hash").unwrap_or_default();
            let payload_enc: Vec<u8> = row.try_get("payload_enc").unwrap_or_default();
            let master_key_id: String = row.try_get("master_key_id").unwrap_or_default();

            // A decode/decrypt failure is recorded per block, not propagated: one bad row must not
            // hide the rest of the trace, and the distinction from absence is what tells an
            // operator whether to look at retention or at key rotation.
            let opened = sealer
                .open(
                    "trace_block",
                    &[
                        master_key_id.as_bytes(),
                        owner_user_id.as_bytes(),
                        hash.as_bytes(),
                    ],
                    &payload_enc,
                )
                .map_err(|_| ())
                .and_then(|secret| {
                    zstd::bulk::decompress(secret.expose_secret(), 10 * 1024 * 1024).map_err(|_| ())
                });

            let resolved_block = match opened {
                Ok(bytes) => ResolvedBlock::Content(
                    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_else(|_| {
                        // A block is canonicalized JSON in the write path, but a non-JSON payload
                        // must render as text rather than fail the read (`TRC-4`).
                        serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string())
                    }),
                ),
                Err(()) => ResolvedBlock::DecryptFailed,
            };
            resolved.insert(hash, resolved_block);
        }
    }
    Ok(resolved)
}

/// Substitute every resolvable `{"$blk": …}` in `value` in place.
///
/// `resolved` maps hash to content; a hash absent from it is rendered as missing.
fn substitute(
    value: &mut serde_json::Value,
    resolved: &std::collections::HashMap<String, ResolvedBlock>,
) {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(hash) = fields
                .get(BLOCK_REF_KEY)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            {
                let replacement = match resolved.get(&hash) {
                    Some(ResolvedBlock::Content(content)) => content.clone(),
                    Some(ResolvedBlock::DecryptFailed) => serde_json::json!({
                        BLOCK_REF_KEY: hash,
                        UNRESOLVED_KEY: UNRESOLVED_DECRYPT_FAILED,
                    }),
                    // Absent, or owned by another tenant — which the fetch made indistinguishable.
                    None => serde_json::json!({
                        BLOCK_REF_KEY: hash,
                        UNRESOLVED_KEY: UNRESOLVED_MISSING,
                    }),
                };
                *value = replacement;
                return;
            }
            for (_, nested) in fields.iter_mut() {
                substitute(nested, resolved);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                substitute(item, resolved);
            }
        }
        _ => {}
    }
}

/// Resolve every block reference in `turn_trace` for `owner_user_id`, in place.
///
/// Takes the parsed trace rather than bytes so the caller keeps one decrypt/decompress path
/// (`get_trace_record_internal` already has one) and this module owns only the block half. Returns
/// the number of distinct hashes that were fetched, for the log line — not for the response, because
/// the response shape is what clients parse and no `TRC` requirement asks for a count.
pub(crate) async fn resolve_block_references(
    state: &ApiAppState,
    owner_user_id: &str,
    turn_trace: &mut serde_json::Value,
    sealer: &llmroller_store::crypto::XChaCha20Poly1305Sealer,
) -> Result<usize, (StatusCode, Json<serde_json::Value>)> {
    let hashes = collect_block_hashes(turn_trace);
    if hashes.is_empty() {
        return Ok(0);
    }
    let resolved = fetch_blocks(state, owner_user_id, &hashes, sealer).await?;
    let fetched = resolved.len();
    substitute(turn_trace, &resolved);
    Ok(fetched)
}

#[cfg(test)]
#[path = "trace_rehydration_tests.rs"]
mod tests;
