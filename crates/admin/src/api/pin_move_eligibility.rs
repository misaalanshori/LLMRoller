//! Static eligibility of a pin-move target (`14.t.4`, `ISSUE-0167`).
//!
//! Spec: ./docs/srdd/14.t. SRDD - Active Sessions Pin Manager and Client-Key Recently Active Sessions Drawer.md
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md
//! Requirements: RTE-11, USR-3, SEC-17
//!
//! # What this is, and what it is not
//!
//! `14.t.4` requires a pin move to verify the target "passes candidate checks `C1` through `C8`
//! for `(caller, canonical_id)`", answering `422 target_route_ineligible` otherwise. The data
//! plane's real predicate is [`llmroller_core::eligibility::evaluate_candidate`], fed by
//! `eligibility_route` in `crates/proxy/src/orchestrator/routes.rs`. That function needs
//! `&ConfigSnapshot` and `&RuntimeState`.
//!
//! **Neither is reachable from this crate.** `ApiAppState` carries `auth.pool`, `writer`,
//! `reloader_tx`, `published_version`, `pin_store`, `token_manager` — and no `RuntimeState` and no
//! `ConfigSnapshot`. So this module implements the part of C1/C4/C6 that is a function of rows in
//! SQLite, and deliberately **not** the in-memory part.
//!
//! This is a documented partial check, not an approximation dressed up as the real thing. What is
//! checked here is authoritative for what it claims: `upstream_keys.enabled`,
//! `upstream_keys.health`, `model_routes.enabled`, `model_routes.status`, and the existence of the
//! `(model_pk, key)` route row. The parts that cannot be checked are listed in
//! [`NOT_CHECKABLE_HERE`] and in the cycle report.

use super::*;

/// Conditions `14.t.4` asks for that this crate **cannot** verify, with the reason each is
/// unverifiable. Kept in production code rather than only in a comment so a later reader cannot
/// mistake this module for a full C1-C8 implementation.
///
/// Exercised by the unit test that asserts every entry names its check; nothing reads it at
/// runtime, which is the point — it is documentation the compiler keeps honest.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const NOT_CHECKABLE_HERE: [&str; 6] = [
    "C6 memory half: RuntimeState.key_health (quarantine/cooldown/exhaustion reset timers). \
     SQLite `upstream_keys.health` is the durable projection of it and is checked instead.",
    "C7 origin circuit breaker: RuntimeState.is_origin_admitted. In memory only, no durable mirror.",
    "C8 budget admission: RuntimeState accumulators and key windows. In memory only.",
    "C2 tier authorization over shares: needs snapshot.shares_by_grantee. The caller's ownership of \
     a personal key is verified directly instead.",
    "C4 model exposure and merge-review vendor-prefix rules: needs snapshot models and settings.",
    "C5 protocol support for the inbound protocol: the pin move does not know the next request's \
     protocol, so `model_routes.protocols` is not narrowed to one.",
];

/// Why a move target cannot serve the pin's model. Each variant names the condition that failed so
/// the `422` message can say which one, rather than a generic "ineligible".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetIneligibility {
    /// No `model_routes` row for `(model_pk, key)`: the key does not serve this model at all.
    NoRouteForModel,
    /// `model_routes.enabled = 0`.
    RouteDisabled,
    /// `model_routes.status` is not `active` (it is `missing` or `retired`).
    RouteNotActive,
    /// `upstream_keys.enabled = 0`.
    KeyDisabled,
    /// `upstream_keys.health` is `invalid` (quarantined).
    KeyQuarantined,
    /// `upstream_keys.health` is `exhausted` and its reset window has not passed.
    KeyExhausted,
    /// `upstream_keys.health` is `cooling_down` and its window has not passed.
    KeyCoolingDown,
}

impl TargetIneligibility {
    /// Operator-facing message naming the failed condition (`14.t.4` asks for `422` with a reason).
    #[must_use]
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::NoRouteForModel => "the target upstream key has no route for the pinned model",
            Self::RouteDisabled => "the target upstream key's route for this model is disabled",
            Self::RouteNotActive => {
                "the target upstream key's route for this model is not active (missing or retired)"
            }
            Self::KeyDisabled => "the target upstream key is disabled",
            Self::KeyQuarantined => {
                "the target upstream key is quarantined (health: invalid), so it cannot serve \
                 traffic until it is revalidated"
            }
            Self::KeyExhausted => {
                "the target upstream key is exhausted for its current quota window"
            }
            Self::KeyCoolingDown => "the target upstream key is cooling down after failures",
        }
    }

    /// `422` with the code `14.t.4` names and a message naming the failed condition.
    pub(crate) fn reject(self) -> (StatusCode, Json<serde_json::Value>) {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": {
                    "code": "target_route_ineligible",
                    "message": self.message(),
                }
            })),
        )
    }
}

/// Decide whether `target_key` can serve `canonical_id`, returning the first failed condition.
///
/// Order is deliberate: route existence first (the most common operator mistake and the most
/// informative message), then the key's own state, then the route's. A caller fixing one problem
/// at a time is not made to discover them in a random order.
fn judge(
    canonical_route: Option<(bool, String)>,
    key_enabled: bool,
    health: &str,
    health_until: Option<i64>,
    now_ms: i64,
) -> Result<(), TargetIneligibility> {
    let Some((route_enabled, route_status)) = canonical_route else {
        return Err(TargetIneligibility::NoRouteForModel);
    };
    if !key_enabled {
        return Err(TargetIneligibility::KeyDisabled);
    }
    // Mirrors `KeyHealthEntry::is_routable` and `store::health::project_health`, which agree:
    // `invalid` is never routable; `exhausted`/`cooling_down` are routable once their window has
    // passed (`14.t.4` C6, `RTE-10`). A past window is genuinely healthy, not a stale claim.
    // `invalid` carries no reset window: `KeyHealthEntry::is_routable` returns false
    // unconditionally. The timed states are routable once their window has passed, which is why a
    // past `health_until` is genuinely healthy rather than a stale claim.
    let window_open = health_until.is_none_or(|until| now_ms < until);
    match health {
        "invalid" => return Err(TargetIneligibility::KeyQuarantined),
        "exhausted" if window_open => return Err(TargetIneligibility::KeyExhausted),
        "cooling_down" if window_open => return Err(TargetIneligibility::KeyCoolingDown),
        _ => {}
    }
    if !route_enabled {
        return Err(TargetIneligibility::RouteDisabled);
    }
    if route_status != "active" {
        return Err(TargetIneligibility::RouteNotActive);
    }
    Ok(())
}

#[cfg(test)]
#[path = "pin_move_eligibility_tests.rs"]
mod tests;

/// Models a user-owned pin move would actually touch, as `(model_pk, canonical_id)`.
///
/// Scoped by the **same** predicate the user handler's `UPDATE` uses
/// (`client_key_id IN (SELECT id FROM client_keys WHERE owner_user_id = ?)`), so the set verified is
/// exactly the set that would move. `session_hash` is the only other filter there.
async fn owned_pin_models(
    pool: &sqlx::SqlitePool,
    owner_user_id: String,
    session_hash: &str,
    canonical_filter: Option<&str>,
) -> Result<Vec<(String, String)>, (StatusCode, Json<serde_json::Value>)> {
    sqlx::query_as(
        "SELECT p.model_pk, m.canonical_id
         FROM pins p
         JOIN models m ON m.model_pk = p.model_pk
         WHERE p.session_hash = ?
           AND p.client_key_id IN (SELECT id FROM client_keys WHERE owner_user_id = ?)
           AND (? IS NULL OR m.canonical_id = ?)",
    )
    .bind(session_hash)
    .bind(owner_user_id)
    .bind(canonical_filter)
    .bind(canonical_filter)
    .fetch_all(pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Guard the user move: every model this caller owns a pin for in the session must be servable by
/// the target key.
///
/// An empty set means the caller owns no pin there, so the `UPDATE` would match nothing and the
/// handler answers `404` — which is the pre-existing, correct behaviour (`USR-3`), not an
/// eligibility failure. Returning `Ok(())` here lets that path keep producing `404` rather than
/// turning it into a `422` about a key the caller is allowed to name.
/// `canonical_filter` mirrors the `UPDATE`'s own `(? IS NULL OR model_pk IN (…))` predicate, so the
/// set verified is exactly the set moved (`ISSUE-0235`). Verifying every model the caller owns in
/// the session — which is what this did before — rejected a move onto a key that serves the model
/// the caller actually named.
pub(crate) async fn verify_move_target_for_owned_pins(
    state: &ApiAppState,
    owner_user_id: UserId,
    session_hash: &str,
    canonical_filter: Option<&str>,
    target_key: UpstreamKeyId,
    now_ms: i64,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let models = owned_pin_models(
        &state.auth.pool,
        owner_user_id.to_string(),
        session_hash,
        canonical_filter,
    )
    .await?;
    for (model_pk, canonical_id) in models {
        verify_target_serves_model_pk(
            &state.auth.pool,
            target_key,
            &model_pk,
            &canonical_id,
            now_ms,
        )
        .await?;
    }
    Ok(())
}

/// Guard the admin move: same rule over the models the admin `UPDATE` would touch.
///
/// The admin `UPDATE` filters by `session_hash` and, when provided, `canonical_id`. So the checked
/// set is narrowed the same way, and an unknown `canonical_id` yields an empty set — the `UPDATE`
/// then matches nothing and the handler returns `404`, unchanged from cycle 36.
pub(crate) async fn verify_move_target_for_admin(
    state: &ApiAppState,
    session_hash: &str,
    canonical_filter: Option<&str>,
    target_key: UpstreamKeyId,
    now_ms: i64,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let models: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.model_pk, m.canonical_id
         FROM pins p
         JOIN models m ON m.model_pk = p.model_pk
         WHERE p.session_hash = ?
           AND (? IS NULL OR m.canonical_id = ?)",
    )
    .bind(session_hash)
    .bind(canonical_filter)
    .bind(canonical_filter)
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    for (model_pk, canonical_id) in models {
        verify_target_serves_model_pk(
            &state.auth.pool,
            target_key,
            &model_pk,
            &canonical_id,
            now_ms,
        )
        .await?;
    }
    Ok(())
}

/// The columns this crate reads to decide eligibility: the key's own state plus its route for one
/// model. `r.*` columns are `None` when no route row exists.
type KeyRow = (bool, String, Option<i64>, Option<bool>, Option<String>);

const KEY_ROW_SQL: &str = "SELECT k.enabled, k.health, k.health_until, r.enabled, r.status \
     FROM upstream_keys k \
     LEFT JOIN model_routes r ON r.upstream_key_id = k.id AND r.model_pk = ? \
     WHERE k.id = ?";

/// Verify `target_key` can serve the model identified by `model_pk`.
///
/// One query, one row: the key's own columns plus its route for **this exact model**. `LEFT JOIN`
/// so a key with no route yields a row with a `NULL` route, rather than being indistinguishable
/// from a key that does not exist — the caller has already established the key exists and is
/// owned, so a missing key here is a race and returns `404` (`USR-3`).
///
/// `canonical_id` is not used in the query: `model_pk` is the primary key and what the callers
/// resolve from their own rows, so passing both would create two ways to disagree about which
/// model is meant. It is accepted for the error message only.
async fn verify_target_serves_model_pk(
    pool: &sqlx::SqlitePool,
    target_key: UpstreamKeyId,
    model_pk: &str,
    canonical_id: &str,
    now_ms: i64,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    // Named in the log line, not the response: the operator already knows which model they
    // pinned, and echoing input into an error body is how injection starts.
    tracing::debug!(
        target_key = %target_key,
        canonical_id,
        "verifying pin move target eligibility"
    );
    let row: Option<KeyRow> = sqlx::query_as(KEY_ROW_SQL)
        .bind(model_pk)
        .bind(target_key.to_string())
        .fetch_optional(pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let Some((key_enabled, key_health, key_health_until, route_enabled, route_status)) = row else {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "upstream key not found",
        ));
    };

    judge(
        route_enabled.zip(route_status),
        key_enabled,
        &key_health,
        key_health_until,
        now_ms,
    )
    .map_err(TargetIneligibility::reject)
}
