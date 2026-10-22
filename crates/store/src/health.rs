#![forbid(unsafe_code)]
//! Persistence of live upstream-key health so cooldowns survive restarts.
//!
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#553-ordered-graceful-shutdown-sigterm--sigint-and-job-supervision
//! Requirements: REL-1, RTE-7.

use llmroller_core::ids::{ModelPk, UpstreamKeyId};
use llmroller_core::runtime_state::KeyHealthEntry;
use sqlx::SqlitePool;

/// One unexpired `PlanRestricted` protocol block loaded at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRouteProtocolBlock {
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: ModelPk,
    pub protocol: String,
    pub until_ms: i64,
}

/// Persist the in-memory health projection for the supplied keys.
///
/// The mapping mirrors the startup hydration in `serve_command`:
/// `invalid` wins over `exhausted`, which wins over `cooling_down`, and
/// expired deadlines collapse back to `healthy`.
pub async fn persist_key_health(
    pool: &SqlitePool,
    entries: &[(UpstreamKeyId, KeyHealthEntry)],
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    if entries.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    for (key_id, entry) in entries {
        let (health, until) = project_health(entry, now_ms);
        sqlx::query(
            "UPDATE upstream_keys
                SET health = ?, health_until = ?, health_version = health_version + 1
              WHERE id = ?",
        )
        .bind(health)
        .bind(until)
        .bind(key_id.to_string())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Load unexpired `PlanRestricted` protocol blocks for startup preload.
pub async fn load_active_route_protocol_blocks(
    pool: &SqlitePool,
    now_ms: i64,
) -> Result<Vec<ActiveRouteProtocolBlock>, sqlx::Error> {
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT upstream_key_id, model_pk, protocol, until_ms
           FROM route_protocol_blocks
          WHERE until_ms > ?",
    )
    .bind(now_ms)
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for (key, model, protocol, until_ms) in rows {
        let (Ok(upstream_key_id), Ok(model_pk)) =
            (UpstreamKeyId::parse(&key), ModelPk::parse(&model))
        else {
            continue;
        };
        out.push(ActiveRouteProtocolBlock {
            upstream_key_id,
            model_pk,
            protocol,
            until_ms,
        });
    }
    Ok(out)
}

/// Project a live health entry onto the schema's four-state health column.
#[must_use]
pub fn project_health(entry: &KeyHealthEntry, now_ms: i64) -> (&'static str, Option<i64>) {
    if !entry.healthy || entry.quarantine_reason.is_some() {
        return ("invalid", None);
    }
    if let Some(until) = entry.exhausted_until_ms
        && until > now_ms
    {
        return ("exhausted", Some(until));
    }
    if let Some(until) = entry.cooldown_until_ms
        && until > now_ms
    {
        return ("cooling_down", Some(until));
    }
    ("healthy", None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec: RTE-7, REL-1
    #[test]
    fn health_projection_prefers_quarantine_then_exhaustion_then_cooldown() {
        let now = 10_000;
        let quarantined = KeyHealthEntry {
            healthy: false,
            quarantine_reason: Some("credential_rejected".to_string()),
            exhausted_until_ms: Some(now + 1_000),
            cooldown_until_ms: Some(now + 1_000),
            ..KeyHealthEntry::default()
        };
        assert_eq!(project_health(&quarantined, now), ("invalid", None));

        let exhausted = KeyHealthEntry {
            exhausted_until_ms: Some(now + 5_000),
            cooldown_until_ms: Some(now + 1_000),
            ..KeyHealthEntry::default()
        };
        assert_eq!(
            project_health(&exhausted, now),
            ("exhausted", Some(now + 5_000))
        );

        let cooling = KeyHealthEntry {
            cooldown_until_ms: Some(now + 1_000),
            ..KeyHealthEntry::default()
        };
        assert_eq!(
            project_health(&cooling, now),
            ("cooling_down", Some(now + 1_000))
        );

        let expired = KeyHealthEntry {
            cooldown_until_ms: Some(now - 1),
            exhausted_until_ms: Some(now - 1),
            ..KeyHealthEntry::default()
        };
        assert_eq!(project_health(&expired, now), ("healthy", None));
    }
}
