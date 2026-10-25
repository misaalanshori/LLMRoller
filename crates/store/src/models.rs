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
    upsert_discovered_models_with_context_tx(
        tx,
        upstream_key_id,
        is_global,
        owner_user_id,
        now_ms,
        discovered_raw_models,
        &HashMap::new(),
        response_bytes_len,
    )
    .await
}

/// Infer the supported inference protocols for a discovered model (`F020`, `MDL-1`, `RTE-3`).
#[must_use]
pub fn infer_discovered_model_protocols(
    provider_kind: Option<&str>,
    raw_model_id: &str,
    advertised_protocols: Option<&[String]>,
    provider_supported: Option<&[String]>,
) -> Vec<String> {
    let mut base: Vec<String> = if let Some(adv) = advertised_protocols
        && !adv.is_empty()
    {
        adv.to_vec()
    } else if provider_kind == Some("opencode_go") {
        let slug = raw_model_id
            .rsplit_once('/')
            .map(|(_, s)| s)
            .unwrap_or(raw_model_id)
            .to_ascii_lowercase();
        if slug.starts_with("minimax-") {
            vec!["messages".to_string()]
        } else if matches!(slug.as_str(), "gpt-5.4" | "gpt-6-luna" | "grok-4.7")
            || slug.starts_with("muse-spark")
        {
            vec!["responses".to_string()]
        } else if slug.starts_with("glm-")
            || slug.starts_with("kimi-")
            || slug.starts_with("qwen")
            || slug.starts_with("mimo-")
        {
            vec!["chat".to_string()]
        } else {
            vec![
                "chat".to_string(),
                "responses".to_string(),
                "messages".to_string(),
            ]
        }
    } else {
        vec![
            "chat".to_string(),
            "responses".to_string(),
            "messages".to_string(),
        ]
    };

    if let Some(prov) = provider_supported
        && !prov.is_empty()
    {
        let intersected: Vec<String> = base
            .iter()
            .filter(|p| prov.iter().any(|allowed| allowed == *p))
            .cloned()
            .collect();
        if !intersected.is_empty() {
            base = intersected;
        }
    }
    base
}

/// Upserts discovered models while persisting optional advertised `context_window`
/// values keyed by upstream model id (`MDL-1`, `F-031`).
#[allow(clippy::too_many_arguments)]
pub async fn upsert_discovered_models_with_context_tx(
    tx: &mut Transaction<'_, Sqlite>,
    upstream_key_id: &UpstreamKeyId,
    is_global: bool,
    owner_user_id: Option<&UserId>,
    now_ms: i64,
    discovered_raw_models: &[String],
    context_windows: &HashMap<String, i64>,
    response_bytes_len: usize,
) -> Result<DiscoveryResult, sqlx::Error> {
    upsert_discovered_models_with_metadata_tx(
        tx,
        upstream_key_id,
        is_global,
        owner_user_id,
        now_ms,
        discovered_raw_models,
        context_windows,
        &HashMap::new(),
        response_bytes_len,
    )
    .await
}

/// Upserts discovered models while persisting optional `context_window` and per-model
/// `supported_protocols` metadata (`F020`, `MDL-1`, `RTE-3`).
#[allow(clippy::too_many_arguments)]
pub async fn upsert_discovered_models_with_metadata_tx(
    tx: &mut Transaction<'_, Sqlite>,
    upstream_key_id: &UpstreamKeyId,
    is_global: bool,
    owner_user_id: Option<&UserId>,
    now_ms: i64,
    discovered_raw_models: &[String],
    context_windows: &HashMap<String, i64>,
    model_protocols: &HashMap<String, Vec<String>>,
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

        // Consult confirmed model_aliases and model_redirects during discovery route attachment (MDL-6)
        let resolved_alias: Option<String> = sqlx::query_scalar(
            "SELECT model_pk FROM model_aliases WHERE alias = ? AND confirmed = 1",
        )
        .bind(&canonical_id)
        .fetch_optional(&mut **tx)
        .await?;

        let resolved_redirect: Option<String> = if resolved_alias.is_none() {
            sqlx::query_scalar("SELECT model_pk FROM model_redirects WHERE old_canonical_id = ?")
                .bind(&canonical_id)
                .fetch_optional(&mut **tx)
                .await?
        } else {
            None
        };

        let target_model_pk = resolved_alias.or(resolved_redirect);

        let model_pk = if let Some(ref pk_str) = target_model_pk {
            ModelPk::parse(pk_str).map_err(|e| sqlx::Error::Decode(Box::new(e)))?
        } else {
            // Check if canonical_id exists in models
            let existing = sqlx::query(
                "SELECT model_pk, origin, merge_state FROM models WHERE canonical_id = ?",
            )
            .bind(&canonical_id)
            .fetch_optional(&mut **tx)
            .await?;

            match existing {
                Some(row) => {
                    let pk_str: String = row.try_get("model_pk")?;
                    let origin_str: String = row.try_get("origin")?;
                    let pk =
                        ModelPk::parse(&pk_str).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
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
            }
        };

        discovered_pks.push(model_pk);

        // Persist the advertised capability. Multiple providers may report the
        // same model, so keep the largest observed context window (`F-031`).
        if let Some(context_window) = context_windows.get(raw.as_str()) {
            sqlx::query(
                "UPDATE models SET context_window = MAX(COALESCE(context_window, 0), ?) WHERE model_pk = ?",
            )
            .bind(*context_window)
            .bind(model_pk.to_string())
            .execute(&mut **tx)
            .await?;
        }

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

        // Upsert into model_routes, intersecting provider supported_protocols if configured (MDL-1, RTE-3, PRV-2)
        let pk_str = model_pk.to_string();
        let key_str = upstream_key_id.to_string();
        let raw_str = raw.as_str();
        let provider_row = sqlx::query(
            "SELECT p.kind, p.config_json FROM upstream_keys k JOIN providers p ON p.id = k.provider_id WHERE k.id = ?",
        )
        .bind(&key_str)
        .fetch_optional(&mut **tx)
        .await?;

        let provider_kind_str: Option<String> = provider_row
            .as_ref()
            .and_then(|r| r.try_get::<String, _>("kind").ok());
        let provider_supported: Option<Vec<String>> = provider_row
            .as_ref()
            .and_then(|r| r.try_get::<String, _>("config_json").ok())
            .and_then(|cfg_str| serde_json::from_str::<serde_json::Value>(&cfg_str).ok())
            .and_then(|val| {
                val.get("supported_protocols")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect::<Vec<String>>()
                    })
            })
            .filter(|v| !v.is_empty());

        let inferred_protocols = infer_discovered_model_protocols(
            provider_kind_str.as_deref(),
            raw_str,
            model_protocols.get(raw_str).map(Vec::as_slice),
            provider_supported.as_deref(),
        );
        let protocols_json = serde_json::to_string(&inferred_protocols)
            .unwrap_or_else(|_| r#"["chat","responses","messages"]"#.to_string());

        sqlx::query(
            r#"INSERT INTO model_routes (
                   upstream_key_id, model_pk, upstream_model_id, vendor_prefix,
                   protocols, status, missing_since, discovered_at
               )
               VALUES (?, ?, ?, ?, ?, 'active', NULL, ?)
               ON CONFLICT (upstream_key_id, model_pk) DO UPDATE SET
                   upstream_model_id = excluded.upstream_model_id,
                   vendor_prefix = excluded.vendor_prefix,
                   protocols = excluded.protocols,
                   status = 'active',
                   missing_since = NULL,
                   discovered_at = excluded.discovered_at"#,
        )
        .bind(&key_str)
        .bind(&pk_str)
        .bind(raw_str)
        .bind(vendor_prefix)
        .bind(&protocols_json)
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

    // Repoint any existing model_redirects rows that targeted source_pk so chained merges (A->B, B->C) resolve A->C (MDL-3, MDL-4)
    sqlx::query("UPDATE model_redirects SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

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
    // Clean duplicate conflicting rows from source_pk first to prevent UNIQUE constraint violations (F080, MDL-4).
    sqlx::query(
        r#"DELETE FROM model_aliases WHERE model_pk = ? AND alias IN (
               SELECT alias FROM model_aliases WHERE model_pk = ?
           )"#,
    )
    .bind(&source_str)
    .bind(&target_str)
    .execute(&mut **tx)
    .await?;

    sqlx::query("UPDATE model_aliases SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    sqlx::query(
        r#"DELETE FROM user_route_preferences WHERE model_pk = ? AND (user_id, upstream_key_id) IN (
               SELECT user_id, upstream_key_id FROM user_route_preferences WHERE model_pk = ?
           )"#,
    )
    .bind(&source_str)
    .bind(&target_str)
    .execute(&mut **tx)
    .await?;

    sqlx::query("UPDATE user_route_preferences SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    sqlx::query(
        r#"DELETE FROM budget_windows WHERE model_pk = ? AND (scope_kind, COALESCE(upstream_key_id, '')) IN (
               SELECT scope_kind, COALESCE(upstream_key_id, '') FROM budget_windows WHERE model_pk = ?
           )"#,
    )
    .bind(&source_str)
    .bind(&target_str)
    .execute(&mut **tx)
    .await?;

    sqlx::query("UPDATE budget_windows SET model_pk = ? WHERE model_pk = ?")
        .bind(&target_str)
        .bind(&source_str)
        .execute(&mut **tx)
        .await?;

    sqlx::query(
        r#"DELETE FROM pins WHERE model_pk = ? AND (client_key_id, session_hash) IN (
               SELECT client_key_id, session_hash FROM pins WHERE model_pk = ?
           )"#,
    )
    .bind(&source_str)
    .bind(&target_str)
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

    sqlx::query(
        r#"DELETE FROM route_protocol_blocks WHERE model_pk = ? AND (upstream_key_id, protocol) IN (
               SELECT upstream_key_id, protocol FROM route_protocol_blocks WHERE model_pk = ?
           )"#,
    )
    .bind(&source_str)
    .bind(&target_str)
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

/// All catalog domain slices required by [`llmroller_core::snapshot::ConfigSnapshot`].
#[derive(Debug, Default, Clone)]
pub struct CatalogSnapshot {
    pub models: HashMap<llmroller_core::ids::ModelPk, llmroller_core::snapshot::SnapshotModel>,
    pub models_by_canonical: HashMap<String, llmroller_core::ids::ModelPk>,
    pub aliases: HashMap<String, llmroller_core::ids::ModelPk>,
    pub redirects: HashMap<String, llmroller_core::ids::ModelPk>,
    pub routes_by_model:
        HashMap<llmroller_core::ids::ModelPk, Vec<llmroller_core::snapshot::SnapshotModelRoute>>,
    pub user_route_prefs: HashMap<
        (UserId, UpstreamKeyId, llmroller_core::ids::ModelPk),
        llmroller_core::snapshot::SnapshotRoutePreference,
    >,
}

/// Loads the complete model catalog in one consistent read transaction (`PERF-1`).
pub async fn load_catalog_snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<CatalogSnapshot, sqlx::Error> {
    use llmroller_core::catalog::{MergeState, ModelOrigin, RouteStatus};

    let mut snapshot = CatalogSnapshot::default();

    let rows = sqlx::query(
        "SELECT model_pk, canonical_id, display_name, context_window, exposed, distribution,
                routing_strategy, merge_state, origin, created_by_user_id
         FROM models",
    )
    .fetch_all(&mut **tx)
    .await?;

    for row in rows {
        let pk_str: String = row.try_get("model_pk")?;
        let Ok(pk) = ModelPk::parse(&pk_str) else {
            continue;
        };
        let canonical_id: String = row.try_get("canonical_id")?;
        let exposed: Option<i64> = row.try_get("exposed").unwrap_or(None);
        let distribution: String = row
            .try_get("distribution")
            .unwrap_or_else(|_| "weighted".to_string());
        let strategy_str: Option<String> = row.try_get("routing_strategy").unwrap_or(None);
        let merge_str: String = row
            .try_get("merge_state")
            .unwrap_or_else(|_| "confirmed".to_string());
        let origin_str: String = row
            .try_get("origin")
            .unwrap_or_else(|_| "global".to_string());
        let created_by: Option<String> = row.try_get("created_by_user_id").unwrap_or(None);

        let model = llmroller_core::snapshot::SnapshotModel {
            id: pk,
            canonical_id: canonical_id.clone(),
            display_name: row.try_get("display_name").unwrap_or(None),
            context_window: row.try_get("context_window").unwrap_or(None),
            exposed: exposed.map(|v| v != 0),
            merge_state: if merge_str == "pending_review" {
                MergeState::PendingReview
            } else {
                MergeState::Confirmed
            },
            majority_vendor_prefix: None,
            distribution,
            routing_strategy: strategy_str
                .and_then(|s| s.parse::<llmroller_core::strategy::RoutingStrategy>().ok()),
            origin: match origin_str.as_str() {
                "personal" => ModelOrigin::Personal,
                "preset" => ModelOrigin::Preset,
                _ => ModelOrigin::Global,
            },
            owner_user_id: created_by.and_then(|s| UserId::parse(&s).ok()),
        };
        snapshot.models.insert(pk, model);
        snapshot.models_by_canonical.insert(canonical_id, pk);
    }

    let aliases = sqlx::query("SELECT alias, model_pk FROM model_aliases WHERE confirmed = 1")
        .fetch_all(&mut **tx)
        .await?;
    for row in aliases {
        let alias: String = row.try_get("alias")?;
        let pk_str: String = row.try_get("model_pk")?;
        if let Ok(pk) = ModelPk::parse(&pk_str) {
            snapshot.aliases.insert(alias, pk);
        }
    }

    let redirects = sqlx::query("SELECT old_canonical_id, model_pk FROM model_redirects")
        .fetch_all(&mut **tx)
        .await?;
    for row in redirects {
        let old: String = row.try_get("old_canonical_id")?;
        let pk_str: String = row.try_get("model_pk")?;
        if let Ok(pk) = ModelPk::parse(&pk_str) {
            snapshot.redirects.insert(old, pk);
        }
    }

    let routes = sqlx::query(
        "SELECT upstream_key_id, model_pk, upstream_model_id, vendor_prefix, protocols, status,
                enabled, priority_override, route_health, route_health_until
         FROM model_routes",
    )
    .fetch_all(&mut **tx)
    .await?;
    for row in routes {
        let key_str: String = row.try_get("upstream_key_id")?;
        let pk_str: String = row.try_get("model_pk")?;
        let (Ok(key_id), Ok(model_pk)) = (UpstreamKeyId::parse(&key_str), ModelPk::parse(&pk_str))
        else {
            continue;
        };
        let protocols_json: String = row
            .try_get("protocols")
            .unwrap_or_else(|_| "[]".to_string());
        let protocols: Vec<String> = serde_json::from_str(&protocols_json).unwrap_or_default();
        let status_str: String = row
            .try_get("status")
            .unwrap_or_else(|_| "active".to_string());
        let status = match status_str.as_str() {
            "missing" => RouteStatus::Missing,
            "retired" => RouteStatus::Retired,
            _ => RouteStatus::Active,
        };
        let route = llmroller_core::snapshot::SnapshotModelRoute {
            upstream_key_id: key_id,
            model_pk,
            upstream_model_id: row.try_get("upstream_model_id")?,
            vendor_prefix: row.try_get("vendor_prefix").unwrap_or(None),
            protocols,
            status,
            enabled: row.try_get::<i64, _>("enabled").unwrap_or(1) != 0,
            priority_override: row.try_get("priority_override").unwrap_or(None),
            route_health: row
                .try_get("route_health")
                .unwrap_or_else(|_| "healthy".to_string()),
            route_health_until: row.try_get("route_health_until").unwrap_or(None),
        };
        snapshot
            .routes_by_model
            .entry(model_pk)
            .or_default()
            .push(route);
    }

    // Resolve the majority vendor prefix for pending-review models (MDL-2, `C4`).
    // Ties resolve to the lexicographically smallest prefix so routing is stable
    // across snapshot reloads.
    let mut vendor_counts: HashMap<ModelPk, HashMap<String, usize>> = HashMap::new();
    for (model_pk, routes) in &snapshot.routes_by_model {
        for route in routes {
            if route.enabled
                && route.status == llmroller_core::catalog::RouteStatus::Active
                && let Some(prefix) = &route.vendor_prefix
            {
                *vendor_counts
                    .entry(*model_pk)
                    .or_default()
                    .entry(prefix.clone())
                    .or_default() += 1;
            }
        }
    }
    for (model_pk, counts) in vendor_counts {
        let majority = counts
            .into_iter()
            .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
            .map(|(prefix, _)| prefix);
        if let Some(model) = snapshot.models.get_mut(&model_pk) {
            model.majority_vendor_prefix = majority;
        }
    }

    let prefs = sqlx::query(
        "SELECT user_id, upstream_key_id, model_pk, priority_override, enabled
         FROM user_route_preferences",
    )
    .fetch_all(&mut **tx)
    .await?;
    for row in prefs {
        let user_str: String = row.try_get("user_id")?;
        let key_str: String = row.try_get("upstream_key_id")?;
        let pk_str: String = row.try_get("model_pk")?;
        let (Ok(user_id), Ok(key_id), Ok(model_pk)) = (
            UserId::parse(&user_str),
            UpstreamKeyId::parse(&key_str),
            ModelPk::parse(&pk_str),
        ) else {
            continue;
        };
        snapshot.user_route_prefs.insert(
            (user_id, key_id, model_pk),
            llmroller_core::snapshot::SnapshotRoutePreference {
                user_id,
                upstream_key_id: key_id,
                model_pk,
                priority_override: row.try_get("priority_override").unwrap_or(100),
                enabled: row.try_get::<i64, _>("enabled").unwrap_or(1) != 0,
            },
        );
    }

    Ok(snapshot)
}

/// Confirms a pending cross-vendor merge review and optionally merges `source_pk` into `target_pk` (`MDL-2`, `MDL-3`, `MDL-4`).
pub async fn confirm_merge_review_tx(
    tx: &mut Transaction<'_, Sqlite>,
    model_pk: &ModelPk,
    target_pk: Option<&ModelPk>,
    now_ms: i64,
) -> Result<bool, sqlx::Error> {
    if let Some(target) = target_pk
        && target != model_pk
    {
        merge_models_tx(tx, model_pk, target, now_ms).await?;
        sqlx::query("UPDATE models SET merge_state = 'confirmed' WHERE model_pk = ?")
            .bind(target.to_string())
            .execute(&mut **tx)
            .await?;
        return Ok(true);
    }

    let res = sqlx::query("UPDATE models SET merge_state = 'confirmed' WHERE model_pk = ?")
        .bind(model_pk.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(res.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::migrate;
    use crate::pool::{Role, open_pool};
    use llmroller_core::catalog::ModelOrigin;
    use llmroller_core::snapshot::ConfigSnapshot;
    use std::sync::Arc;

    #[tokio::test]
    #[doc = "Spec: MDL-1, MDL-2, MDL-3, MDL-4, RTE-3, PRV-2"]
    async fn chained_model_merge_redirects_and_preset_origin_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = open_pool(&dir.path().join("models.db"), Role::Writer)
            .await
            .expect("open pool");
        migrate(&pool).await.expect("migrate");

        let mut tx = pool.begin().await.expect("begin");
        let pk_a = ModelPk::generate();
        let pk_b = ModelPk::generate();
        let pk_c = ModelPk::generate();

        sqlx::query("INSERT INTO models (model_pk, canonical_id, origin, merge_state) VALUES (?, 'model-a', 'global', 'pending_review')")
            .bind(pk_a.to_string())
            .execute(&mut *tx)
            .await
            .expect("insert a");
        sqlx::query("INSERT INTO models (model_pk, canonical_id, origin, merge_state) VALUES (?, 'model-b', 'global', 'pending_review')")
            .bind(pk_b.to_string())
            .execute(&mut *tx)
            .await
            .expect("insert b");
        sqlx::query("INSERT INTO models (model_pk, canonical_id, origin, merge_state) VALUES (?, 'model-c', 'preset', 'pending_review')")
            .bind(pk_c.to_string())
            .execute(&mut *tx)
            .await
            .expect("insert c");

        // Merge A -> B, then confirm merge review B -> C
        merge_models_tx(&mut tx, &pk_a, &pk_b, 1_000)
            .await
            .expect("merge A -> B");
        let confirmed = confirm_merge_review_tx(&mut tx, &pk_b, Some(&pk_c), 2_000)
            .await
            .expect("confirm merge B -> C");
        assert!(confirmed);

        let catalog = load_catalog_snapshot_tx(&mut tx)
            .await
            .expect("load catalog snapshot");
        tx.commit().await.expect("commit");

        let model_c = catalog.models.get(&pk_c).expect("model c present");
        assert_eq!(model_c.origin, ModelOrigin::Preset);
        assert_eq!(
            model_c.merge_state,
            llmroller_core::catalog::MergeState::Confirmed
        );

        let snap = ConfigSnapshot {
            models: Arc::new(catalog.models),
            models_by_canonical: Arc::new(catalog.models_by_canonical),
            aliases: Arc::new(catalog.aliases),
            redirects: Arc::new(catalog.redirects),
            ..ConfigSnapshot::default()
        };

        assert_eq!(
            snap.resolve_model("model-a").expect("resolve model-a").id,
            pk_c
        );
        assert_eq!(
            snap.resolve_model("model-b").expect("resolve model-b").id,
            pk_c
        );
        assert_eq!(
            snap.resolve_model("model-c").expect("resolve model-c").id,
            pk_c
        );
    }

    #[test]
    #[doc = "Spec: MDL-1, RTE-3, PRV-2, F096 - opencode_go per-model protocol dispatch mapping"]
    fn regression_test_f096_opencode_go_preset_and_protocol_routing_() {
        for m in ["minimax-m2.5", "minimax-m2.7"] {
            assert_eq!(
                infer_discovered_model_protocols(Some("opencode_go"), m, None, None),
                vec!["messages".to_string()]
            );
        }
        for m in [
            "gpt-5.4",
            "gpt-6-luna",
            "muse-spark-20260408",
            "muse-spark-thinking-20260408",
            "grok-4.7",
        ] {
            assert_eq!(
                infer_discovered_model_protocols(Some("opencode_go"), m, None, None),
                vec!["responses".to_string()]
            );
        }
        for m in [
            "glm-5",
            "glm-5.1",
            "kimi-k2.5",
            "qwen3.5-plus",
            "qwen3.6-plus",
        ] {
            assert_eq!(
                infer_discovered_model_protocols(Some("opencode_go"), m, None, None),
                vec!["chat".to_string()]
            );
        }
        let preset_toml = llmroller_core::provider::ProviderKind::OpencodeGo.preset_toml();
        assert!(preset_toml.contains("x-opencode-client"));
        assert!(preset_toml.contains("x-opencode-session"));
    }
}
