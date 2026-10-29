//! The active-pins read view (`Section 14.t.1`).
//!
//! Spec: ./docs/srdd/14.t. SRDD - Active Sessions Pin Manager and Client-Key Recently Active Sessions Drawer.md
//! Requirements: RTE-10, USR-3, USG-9, SEC-17
//!
//! The view is **live**: `Section 14` specifies that `GET /api/user/pins` returns "live `PinStore`
//! pins", and `Section 14.t.1` that the screen renders "confirmed session pins in `PinStore`".
//! The durable `pins` table is the record of what survives a restart, flushed on a
//! `BATCH_MAX_AGE` (250 ms) writer tick, so reading only SQLite hid a pin for up to a tick after it
//! was acquired (`ISSUE-0168`).
//!
//! Both the JSON handler and the HTML partial read this module's [`load_active_pins`], so they
//! cannot disagree about which rows exist; they differ only in rendering and in their
//! no-label placeholder text.

use super::*;
use sqlx::Row as _;

/// Row limit for the active-pins view (`Section 14.t.1`).
pub(crate) const ACTIVE_PINS_LIMIT: i64 = 50;

/// One row of the active-pins view, before label masking.
///
/// Deliberately raw: the JSON handler and the HTML partial each apply their own masking fallback
/// text, but they must agree on *which* rows exist and what the numbers are. Both therefore read
/// this struct from [`load_active_pins`] and differ only in rendering.
pub(crate) struct ActivePinRow {
    pub session_hash: String,
    pub client_key_id: String,
    pub canonical_id: Option<String>,
    pub pool_tier: i64,
    pub created_at: i64,
    pub last_used_at: i64,
    pub key_label: Option<String>,
    pub key_owner: Option<String>,
}

/// Mask an upstream key label for a non-admin caller (`USR-4`, `SEC-17`).
///
/// Shared by both pins views so the redaction predicate exists once. Returns `None` when there is
/// no label to show, leaving the caller to decide its own placeholder text.
pub(crate) fn mask_key_label(
    is_admin: bool,
    pool_tier: i64,
    key_owner: Option<&str>,
    caller_user_id: Option<&str>,
    raw_label: Option<String>,
) -> Option<String> {
    if is_admin {
        raw_label
    } else if pool_tier == 2 {
        Some("shared pool".to_string())
    } else if pool_tier == 3 || key_owner != caller_user_id {
        Some("global pool".to_string())
    } else {
        raw_label
    }
}

/// Build a `(?, ?, ?)` placeholder list of `count` items for a dynamic `IN` clause.
///
/// Shared with the trace block rehydration query, which has the same problem (`trace_rehydration`).
pub(crate) fn placeholders(count: usize) -> String {
    let mut out = String::with_capacity(count * 2);
    for index in 0..count {
        if index > 0 {
            out.push(',');
        }
        out.push('?');
    }
    out
}

/// Load the active pins shown by `/api/user/pins`, `/admin/api/pins` and `/app/partials/pins`.
///
/// # The view is live, not durable
///
/// `Section 14` specifies that `GET /api/user/pins` returns "live `PinStore` pins", and
/// `Section 14.t.1` that the screen renders "confirmed session pins in `PinStore`". So the durable
/// `pins` table is not the source of truth for *which* rows exist — it is the record of what will
/// survive a restart, which the writer flushes on a `BATCH_MAX_AGE` (250 ms) tick. Reading only
/// SQLite therefore hid a pin for up to a tick after it was acquired, which is the defect this
/// function closes.
///
/// # Two versions of one thing
///
/// A live `PinEntry` and a durable row for the same `PinKey` are the same pin, not two. The merge
/// is keyed by `(client_key_id, session_hash, canonical_id)` and resolves each field by where it
/// is authoritative:
///
/// - `created_at` — **SQLite**, when a durable row exists. It records when the pin was originally
///   made; a live entry's `created_ms` is only when *this process* first saw it, which resets on
///   restart because startup hydration (`serve.rs`) sets `generation: 1`.
/// - `last_used_at` — **the later of the two.** The store advances it every turn
///   (`first_valid_frame`), while SQLite lags by up to `BATCH_MAX_AGE`; the newer value is the
///   true one.
/// - `pool_tier`, `upstream_key_id`, `canonical_id` — **the live entry**, for the same reason: it
///   is the current route, and a repin changes it before the flush lands.
///
/// # Tenant scoping (`USR-3`, `SEC-17`)
///
/// A live `PinEntry` carries no owner — only `upstream_key_id` and `pool_tier` — so the SQL
/// predicate that scopes the durable rows has nothing to apply to it. Ownership is therefore
/// resolved through the same `client_keys` table, in **one** scoped query, and an entry whose
/// owner cannot be resolved is **dropped**. It is never shown to a caller who cannot prove
/// ownership of it: dropping is the only safe direction, because the alternative is showing one
/// tenant another's session.
///
/// # Provisional entries
///
/// Excluded. `Section 14.t.1` lists *confirmed* pins, and a provisional entry is mid-handshake: it
/// may still be discarded, or repointed at a different key before the first valid frame confirms
/// it. Showing it would put a pin in the user's list that can vanish without any action, and whose
/// key may already be wrong.
pub(crate) async fn load_active_pins(
    state: &ApiAppState,
    actor: &ActorScope,
    now_ms: i64,
) -> Result<Vec<ActivePinRow>, (StatusCode, Json<serde_json::Value>)> {
    let is_admin = actor.is_admin();
    let uid_opt = actor.user_id().map(|u| u.to_string());

    // The durable rows: unchanged predicates, ordering and limit.
    let query_str = if is_admin {
        "SELECT p.session_hash, p.client_key_id, p.model_pk, p.upstream_key_id, p.pool_tier, p.created_at, p.last_used_at, m.canonical_id, k.label as key_label, k.owner_user_id as key_owner
         FROM pins p
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         WHERE p.expires_at > ?
         ORDER BY p.last_used_at DESC LIMIT 50"
    } else {
        "SELECT p.session_hash, p.client_key_id, p.model_pk, p.upstream_key_id, p.pool_tier, p.created_at, p.last_used_at, m.canonical_id, k.label as key_label, k.owner_user_id as key_owner
         FROM pins p
         JOIN client_keys ck ON p.client_key_id = ck.id
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         WHERE ck.owner_user_id = ? AND p.expires_at > ?
         ORDER BY p.last_used_at DESC LIMIT 50"
    };

    let rows = if is_admin {
        sqlx::query(query_str)
            .bind(now_ms)
            .fetch_all(&state.auth.pool)
            .await
    } else {
        let uid = uid_opt.clone().unwrap_or_default();
        sqlx::query(query_str)
            .bind(uid)
            .bind(now_ms)
            .fetch_all(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut merged: Vec<ActivePinRow> = Vec::with_capacity(rows.len());
    // Index of each already-present PinKey, so a live entry updates its row instead of
    // appending a duplicate.
    let mut position: HashMap<(String, String, Option<String>), usize> = HashMap::new();
    for r in rows {
        let session_hash: String = r.try_get("session_hash").unwrap_or_default();
        let client_key_id: String = r.try_get("client_key_id").unwrap_or_default();
        let canonical_id: Option<String> = r.try_get("canonical_id").unwrap_or_default();
        let key = (
            client_key_id.clone(),
            session_hash.clone(),
            canonical_id.clone(),
        );
        position.insert(key, merged.len());
        merged.push(ActivePinRow {
            session_hash,
            client_key_id,
            canonical_id,
            pool_tier: r.try_get("pool_tier").unwrap_or(1),
            created_at: r.try_get("created_at").unwrap_or(0),
            last_used_at: r.try_get("last_used_at").unwrap_or(0),
            key_label: r.try_get("key_label").ok().flatten(),
            key_owner: r.try_get("key_owner").ok().flatten(),
        });
    }

    merge_live_pins(state, actor, now_ms, &mut merged, position).await?;

    // Ordering stays `last_used_at` descending, and the cap is applied *after* the merge — a
    // merge that appended past this point would exceed the limit the SQL `LIMIT` was enforcing.
    merged.sort_by(|a, b| {
        b.last_used_at
            .cmp(&a.last_used_at)
            .then_with(|| a.session_hash.cmp(&b.session_hash))
    });
    merged.truncate(usize::try_from(ACTIVE_PINS_LIMIT).unwrap_or(50));
    Ok(merged)
}

/// Fold confirmed, unexpired live pins into `merged` (`Section 14.t.1`).
///
/// Split from [`load_active_pins`] so the merge is readable on its own; the caller owns the
/// ordering and the cap.
async fn merge_live_pins(
    state: &ApiAppState,
    actor: &ActorScope,
    now_ms: i64,
    merged: &mut Vec<ActivePinRow>,
    mut position: HashMap<(String, String, Option<String>), usize>,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let Some(store) = &state.pin_store else {
        return Ok(());
    };

    // Confirmed only, unexpired only. Then keep the newest `ACTIVE_PINS_LIMIT` by last use: an
    // entry that ranks below that many others cannot reach the final top-`ACTIVE_PINS_LIMIT`, so
    // this bounds the `IN` lists below without changing the result.
    let mut live: Vec<(llmroller_core::pins::PinKey, llmroller_core::pins::PinEntry)> = store
        .list_active(now_ms)
        .into_iter()
        .filter(|(_, entry)| entry.state == llmroller_core::pins::PinState::Confirmed)
        .collect();
    if live.is_empty() {
        return Ok(());
    }
    live.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.last_used_ms));
    live.truncate(usize::try_from(ACTIVE_PINS_LIMIT).unwrap_or(50));

    let mut client_key_ids: Vec<String> = live
        .iter()
        .map(|(key, _)| key.client_key_id.to_string())
        .collect();
    client_key_ids.sort_unstable();
    client_key_ids.dedup();

    let mut upstream_key_ids: Vec<String> = live
        .iter()
        .map(|(_, entry)| entry.upstream_key_id.to_string())
        .collect();
    upstream_key_ids.sort_unstable();
    upstream_key_ids.dedup();

    // One scoped query resolves ownership *and* applies the tenant predicate. For a non-admin the
    // `owner_user_id = ?` is what drops a foreign entry: an id absent from the result has no
    // provable owner, and is treated exactly like a foreign one.
    let visible_client_keys: HashSet<String> = {
        let sql = format!(
            "SELECT id FROM client_keys WHERE id IN ({}){}",
            placeholders(client_key_ids.len()),
            if actor.is_admin() {
                String::new()
            } else {
                " AND owner_user_id = ?".to_string()
            }
        );
        let mut query = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(sql));
        for id in &client_key_ids {
            query = query.bind(id);
        }
        if !actor.is_admin() {
            query = query.bind(actor.user_id().map(|u| u.to_string()).unwrap_or_default());
        }
        query
            .fetch_all(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
            .into_iter()
            .collect()
    };

    // Labels and owners for the keys the surviving entries point at.
    let key_info: HashMap<String, (Option<String>, Option<String>)> = {
        let sql = format!(
            "SELECT id, label, owner_user_id FROM upstream_keys WHERE id IN ({})",
            placeholders(upstream_key_ids.len())
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for id in &upstream_key_ids {
            query = query.bind(id);
        }
        query
            .fetch_all(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
            .into_iter()
            .map(|row| {
                (
                    row.try_get::<String, _>("id").unwrap_or_default(),
                    (
                        row.try_get("label").ok().flatten(),
                        row.try_get("owner_user_id").ok().flatten(),
                    ),
                )
            })
            .collect()
    };

    for (pin_key, entry) in live {
        let client_key_id = pin_key.client_key_id.to_string();
        if !visible_client_keys.contains(&client_key_id) {
            // No provable owner (or another tenant's). Fail closed.
            continue;
        }
        let upstream_key_id = entry.upstream_key_id.to_string();
        let (key_label, key_owner) = key_info.get(&upstream_key_id).cloned().unwrap_or_default();
        let identity = (
            client_key_id.clone(),
            pin_key.session_hash.clone(),
            Some(pin_key.canonical_id.clone()),
        );

        if let Some(&index) = position.get(&identity) {
            // Same pin seen twice: keep SQLite's `created_at`, take the newer `last_used_at`, and
            // prefer the live route (`Section 14.t.1` precedence, documented on `load_active_pins`).
            let row = &mut merged[index];
            row.last_used_at = row.last_used_at.max(entry.last_used_ms);
            row.pool_tier = i64::from(entry.pool_tier.as_u8());
            row.key_label = key_label;
            row.key_owner = key_owner;
        } else {
            position.insert(identity, merged.len());
            merged.push(ActivePinRow {
                session_hash: pin_key.session_hash,
                client_key_id,
                canonical_id: Some(pin_key.canonical_id),
                pool_tier: i64::from(entry.pool_tier.as_u8()),
                // No durable row exists yet, so the store's own creation stamp is the only record
                // of when this pin was made.
                created_at: entry.created_ms,
                last_used_at: entry.last_used_ms,
                key_label,
                key_owner,
            });
        }
    }
    Ok(())
}
