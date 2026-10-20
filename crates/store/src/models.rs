//! Database repositories for models, model_routes, model_aliases, and model_redirects.
//!
//! Spec: Section 6.6, Section 10.6, Requirements: MDL-1, MDL-2, MDL-3, MDL-4, MDL-5, MDL-6, MDL-7.

use sqlx::{Row as _, Sqlite, Transaction};
use std::collections::HashMap;

use llmroller_core::catalog::{
    MAX_PERSONAL_MODELS_PER_USER, collapse_hyphens, normalize_keep_vendor, normalize_model_id,
};
use llmroller_core::ids::{ModelPk, UpstreamKeyId, UserId};

/// Validates that an upstream model ID matches `^[!-~]{1,256}$`.
pub fn is_valid_upstream_model_id(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() || s.len() > 256 {
        return false;
    }
    s.bytes().all(|b| (0x21..=0x7E).contains(&b))
}

/// Result of discovery refresh transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryResult {
    pub total_discovered: usize,
    pub routes_added_or_updated: usize,
    pub routes_marked_missing: usize,
    pub routes_retired: usize,
    pub is_partial: bool,
}

/// Upserts discovered models from provider /models response.
pub async fn upsert_discovered_models_tx(
    tx: &mut Transaction<'_, Sqlite>,
    upstream_key_id: &UpstreamKeyId,
    is_global: bool,
    owner_user_id: Option<&UserId>,
    now_ms: i64,
    discovered_raw_models: &[String],
    response_bytes_len: usize,
) -> Result<DiscoveryResult, sqlx::Error> {
    // Check 2 MiB (2_097_152 bytes) or > 1,000 models partial guard (MDL-1)
    let is_partial = discovered_raw_models.len() > 1000 || response_bytes_len > 2 * 1024 * 1024;

    // Filter valid IDs matching ^[!-~]{1,256}$
    let valid_models: Vec<&String> = discovered_raw_models
        .iter()
        .filter(|m| is_valid_upstream_model_id(m))
        .collect();

    // Check for collisions within the same key's /models list (MDL-2)
    let mut slug_counts: HashMap<String, usize> = HashMap::new();
    for raw in &valid_models {
        if let Ok(slug) = normalize_model_id(raw) {
            *slug_counts.entry(slug).or_default() += 1;
        }
    }

    let mut added_or_updated = 0;
    let mut discovered_pks = Vec::new();

    // Track personal models count if personal discovery
    let mut personal_models_count = 0;
    if !is_global && let Some(uid) = owner_user_id {
        let row: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM models WHERE origin = 'personal' AND created_by_user_id = ?",
        )
        .bind(uid.to_string())
        .fetch_one(&mut **tx)
        .await?;
        personal_models_count = row as usize;
    }

    for raw in &valid_models {
        let (canonical_id, vendor_prefix) = {
            let raw_str = raw.as_str();
            let prefix = raw_str
                .rfind('/')
                .map(|idx| collapse_hyphens(&raw_str[..idx]));

            let norm_slug = match normalize_model_id(raw_str) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let final_id = if slug_counts.get(&norm_slug).copied().unwrap_or(0) > 1 {
                normalize_keep_vendor(raw_str).unwrap_or(norm_slug)
            } else {
                norm_slug
            };
            (final_id, prefix)
        };

        // Check if canonical_id exists in models
        let existing =
            sqlx::query("SELECT model_pk, origin, merge_state FROM models WHERE canonical_id = ?")
                .bind(&canonical_id)
                .fetch_optional(&mut **tx)
                .await?;

        let model_pk = match existing {
            Some(row) => {
                let pk_str: String = row.try_get("model_pk")?;
                let origin_str: String = row.try_get("origin")?;
                let pk = ModelPk::parse(&pk_str).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
                // If global discovery finds an existing personal model, upgrade to global (MDL-5)
                if is_global && origin_str == "personal" {
                    sqlx::query(
                        "UPDATE models SET origin = 'global', created_by_user_id = NULL WHERE model_pk = ?",
                    )
                    .bind(&pk_str)
                    .execute(&mut **tx)
                    .await?;
                }
                pk
            }
            None => {
                // If personal, enforce max 2,000 personal models per user (MDL-5)
                if !is_global && personal_models_count >= MAX_PERSONAL_MODELS_PER_USER {
                    continue;
                }
                let pk = ModelPk::generate();
                let origin_str = if is_global { "global" } else { "personal" };
                let owner_str = owner_user_id.map(|u| u.to_string());
                let pk_str = pk.to_string();

                sqlx::query(
                    "INSERT INTO models (model_pk, canonical_id, origin, created_by_user_id) VALUES (?, ?, ?, ?)",
                )
                .bind(&pk_str)
                .bind(&canonical_id)
                .bind(origin_str)
                .bind(owner_str)
                .execute(&mut **tx)
                .await?;

                if !is_global {
                    personal_models_count += 1;
                }
                pk
            }
        };

        discovered_pks.push(model_pk);

        // Check if route exists with different vendor_prefix across keys -> pending_review (MDL-2)
        if let Some(ref vp) = vendor_prefix {
            let other_vp: Option<String> = sqlx::query_scalar(
                "SELECT vendor_prefix FROM model_routes WHERE model_pk = ? AND vendor_prefix IS NOT NULL AND vendor_prefix != ? LIMIT 1",
            )
            .bind(model_pk.to_string())
            .bind(vp)
            .fetch_optional(&mut **tx)
            .await?;

            if other_vp.is_some() {
                sqlx::query("UPDATE models SET merge_state = 'pending_review' WHERE model_pk = ?")
                    .bind(model_pk.to_string())
                    .execute(&mut **tx)
                    .await?;
            }
        }

        // Upsert into model_routes
        let pk_str = model_pk.to_string();
        let key_str = upstream_key_id.to_string();
        let raw_str = raw.as_str();
        let protocols_json = r#"["chat","responses","messages"]"#;

        sqlx::query(
            r#"INSERT INTO model_routes (
                   upstream_key_id, model_pk, upstream_model_id, vendor_prefix,
                   protocols, status, missing_since, discovered_at
               )
               VALUES (?, ?, ?, ?, ?, 'active', NULL, ?)
               ON CONFLICT (upstream_key_id, model_pk) DO UPDATE SET
                   status = 'active',
                   missing_since = NULL,
                   discovered_at = excluded.discovered_at"#,
        )
        .bind(&key_str)
        .bind(&pk_str)
        .bind(raw_str)
        .bind(vendor_prefix)
        .bind(protocols_json)
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;

        added_or_updated += 1;
    }

    let mut marked_missing = 0;
    let mut retired = 0;

    // If not partial, transition absent routes to missing and 7d+ missing to retired (MDL-1)
    if !is_partial {
        let key_str = upstream_key_id.to_string();
        let active_routes = sqlx::query(
            "SELECT model_pk, status, missing_since FROM model_routes WHERE upstream_key_id = ?",
        )
        .bind(&key_str)
        .fetch_all(&mut **tx)
        .await?;

        let seven_days_ms = 7 * 86_400_000;

        for r in active_routes {
            let pk_str: String = r.try_get("model_pk")?;
            let status_str: String = r.try_get("status")?;
            let missing_since: Option<i64> = r.try_get("missing_since")?;

            let pk = match ModelPk::parse(&pk_str) {
                Ok(p) => p,
                Err(_) => continue,
            };

            if !discovered_pks.contains(&pk) {
                if status_str == "active" {
                    sqlx::query(
                        "UPDATE model_routes SET status = 'missing', missing_since = ? WHERE upstream_key_id = ? AND model_pk = ?",
                    )
                    .bind(now_ms)
                    .bind(&key_str)
                    .bind(&pk_str)
                    .execute(&mut **tx)
                    .await?;
                    marked_missing += 1;
                } else if status_str == "missing"
                    && let Some(since) = missing_since
                    && now_ms - since >= seven_days_ms
                {
                    sqlx::query(
                        "UPDATE model_routes SET status = 'retired' WHERE upstream_key_id = ? AND model_pk = ?",
                    )
                    .bind(&key_str)
                    .bind(&pk_str)
                    .execute(&mut **tx)
                    .await?;
                    retired += 1;
                }
            }
        }
    }

    Ok(DiscoveryResult {
        total_discovered: valid_models.len(),
        routes_added_or_updated: added_or_updated,
        routes_marked_missing: marked_missing,
        routes_retired: retired,
        is_partial,
    })
}

/// Transactionally merges source model into target model (MDL-4).
pub async fn merge_models_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source_pk: &ModelPk,
    target_pk: &ModelPk,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    if source_pk == target_pk {
        return Ok(());
    }

    let source_str = source_pk.to_string();
    let target_str = target_pk.to_string();

    // Fetch source canonical_id
    let source_canonical: Option<String> =
        sqlx::query_scalar("SELECT canonical_id FROM models WHERE model_pk = ?")
            .bind(&source_str)
            .fetch_optional(&mut **tx)
            .await?;

    let canonical = match source_canonical {
        Some(c) => c,
        None => return Ok(()),
    };

    // Insert redirect from source canonical_id to target model_pk
    sqlx::query(
        r#"INSERT INTO model_redirects (old_canonical_id, model_pk, created_at)
           VALUES (?, ?, ?)
           ON CONFLICT (old_canonical_id) DO UPDATE SET model_pk = excluded.model_pk"#,
    )
    .bind(&canonical)
    .bind(&target_str)
    .bind(now_ms)
    .execute(&mut **tx)
    .await?;

    // Repoint dependent tables to target_pk (RESTRICT foreign keys)
    sqlx::query("UPDATE model_aliases SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    sqlx::query("UPDATE user_route_preferences SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    sqlx::query("UPDATE budget_windows SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    sqlx::query("UPDATE pins SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    sqlx::query("UPDATE response_links SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    sqlx::query("UPDATE route_protocol_blocks SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    // For model_routes: delete duplicates on target_pk first, then repoint
    sqlx::query(
        r#"DELETE FROM model_routes WHERE model_pk = ? AND upstream_key_id IN (
               SELECT upstream_key_id FROM model_routes WHERE model_pk = ?
           )"#,
    )
    .bind(&source_str)
    .bind(&target_str)
    .execute(&mut **tx)
    .await?;

    sqlx::query("UPDATE model_routes SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    // Delete source model from models
    sqlx::query("DELETE FROM models WHERE model_pk = ?")
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    Ok(())
}

/// Creates a confirmed alias mapping alias -> model_pk (MDL-6).
pub async fn add_model_alias_tx(
    tx: &mut Transaction<'_, Sqlite>,
    alias: &str,
    model_pk: &ModelPk,
) -> Result<(), sqlx::Error> {
    let pk_str = model_pk.to_string();
    sqlx::query(
        r#"INSERT INTO model_aliases (alias, model_pk, confirmed)
           VALUES (?, ?, 1)
           ON CONFLICT (alias) DO UPDATE SET model_pk = excluded.model_pk, confirmed = 1"#,
    )
    .bind(alias)
    .bind(pk_str)
    .execute(&mut **tx)
    .await?;

    Ok(())
}
