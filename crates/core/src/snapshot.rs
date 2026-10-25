#![forbid(unsafe_code)]
//! Immutable configuration snapshot and cache invalidation definitions.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#2162-versioned-configsnapshot-publication-reloader

use std::collections::HashMap;
use std::sync::Arc;

use crate::catalog::{MergeState, ModelOrigin, RouteStatus};
use crate::client_key::{ClientKeyRecord, HashRole};
use crate::ids::{ClientKeyId, ModelPk, ProviderId, UpstreamKeyId, UserId};
use crate::pricing::PriceEntry;
use crate::provider::{ProviderConfig, ProviderKind};
use crate::rbac::{OwnershipScope, Role, RoutingScope};
use crate::settings::RuntimeSettings;
use crate::shares::ShareConfig;
use crate::strategy::RoutingStrategy;
use serde::{Deserialize, Serialize};

/// Configuration domains for selective slice reloading per Table 1.6.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDomain {
    Users,
    Settings,
    Providers,
    UpstreamKeys,
    ServiceAccounts,
    ClientKeys,
    Prices,
    BudgetWindows,
    Models,
    Shares,
    Traceability,
}

/// In-memory projection of a user account for hot-path authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRecord {
    pub id: UserId,
    pub username: String,
    pub role: Role,
    pub disabled: bool,
    pub can_use_global_providers: bool,
    pub global_budget_micro_usd_month: Option<i64>,
    pub default_routing_scope: RoutingScope,
}

/// In-memory projection of a provider for hot-path routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub id: ProviderId,
    pub ownership_scope: OwnershipScope,
    pub owner_user_id: Option<UserId>,
    pub kind: ProviderKind,
    pub name: String,
    pub base_url: String,
    pub config_json: String,
    /// Parsed projection of `config_json` for hot-path protocol behavior (`PRV-3`).
    pub config: ProviderConfig,
    pub share_config_fingerprint: String,
    pub usage_url_cross_origin_ack: bool,
    pub allow_private_network: bool,
    pub enabled: bool,
    /// Subscription coverage mode used when pricing this provider (`USG-10`).
    pub billing_coverage_mode: String,
    /// Basis-point multiplier applied to list price for `subscription_plan`.
    pub plan_multiplier_bps: i64,
}

/// In-memory projection of an upstream key for hot-path routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamKeyRecord {
    pub id: UpstreamKeyId,
    pub provider_id: ProviderId,
    pub owner_user_id: Option<UserId>,
    pub label: String,
    pub secret_last4: String,
    pub secret_enc: Vec<u8>,
    pub master_key_id: String,
    pub enabled: bool,
    pub priority: i64,
    pub strict_tier: i64,
    pub health: String,
    pub health_version: i64,
}

/// In-memory projection of a Service Account machine principal (`CLK-6`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceAccountRecord {
    pub id: String,
    pub owner_user_id: UserId,
    pub workspace_scope: String,
    pub name: String,
    pub description: Option<String>,
    /// Inherited CIDR allow-list applied at Gate `G4` for every child key.
    pub allowed_cidrs: Option<Vec<String>>,
    pub disabled: bool,
}

/// In-memory projection of a catalog model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotModel {
    pub id: ModelPk,
    pub canonical_id: String,
    pub display_name: Option<String>,
    /// Discovered context length in tokens, when the upstream advertised one (`MDL-1`, `F-031`).
    pub context_window: Option<i64>,
    pub exposed: Option<bool>,
    pub merge_state: MergeState,
    /// Majority `vendor_prefix` across routes when `merge_state = pending_review`.
    pub majority_vendor_prefix: Option<String>,
    pub distribution: String,
    pub routing_strategy: Option<RoutingStrategy>,
    pub origin: ModelOrigin,
    pub owner_user_id: Option<UserId>,
}

/// In-memory projection of one discovered model route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotModelRoute {
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: ModelPk,
    pub upstream_model_id: String,
    pub vendor_prefix: Option<String>,
    pub protocols: Vec<String>,
    pub status: RouteStatus,
    pub enabled: bool,
    pub priority_override: Option<i64>,
    pub route_health: String,
    pub route_health_until: Option<i64>,
}

/// In-memory projection of a per-user route preference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRoutePreference {
    pub user_id: UserId,
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: ModelPk,
    pub priority_override: i64,
    pub enabled: bool,
}

/// In-memory projection of a configured budget window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotBudgetWindow {
    pub id: String,
    pub scope_kind: String,
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: Option<ModelPk>,
    pub window_kind: String,
    pub period_ms: Option<i64>,
    /// Billing anchor timestamp for anchored window kinds (Section 13.2).
    pub anchor_at: Option<i64>,
    /// Canonical provider window label (`5h`, `7d`, `1mo`) used by `/usage`
    /// reconciliation, when configured.
    pub upstream_window_kind: Option<String>,
    pub limit_micro_usd: i64,
    pub reserve_pct: i64,
}

/// Composite key identifying a row in the price book.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PriceKey {
    pub provider_id: String,
    pub provider_kind: String,
    pub upstream_model_id: String,
}

/// Error returned during in-memory model resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotResolveError {
    NotFound,
    Ambiguous,
    KillSwitched,
    RedirectLoop,
}

/// Cache invalidation events applied immediately after atomic snapshot publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheInvalidationEvent {
    UserChanged(UserId),
    OwnerDisabled(UserId),
    SettingsChanged,
    StrategyChanged,
    CapChanged,
    ProviderChanged(ProviderId),
    UpstreamKeyChanged(UpstreamKeyId),
    ClientKeyChanged(ClientKeyId),
    ModelRoutesChanged(ModelPk),
    ShareChanged(UserId),
    BudgetWindowChanged(String),
}

/// Immutable configuration snapshot loaded into [`arc_swap::ArcSwap`].
///
/// The inference hot path reads every routing input from this structure and must
/// never issue a SQLite query (`PERF-1`, SRDD Section 5.4).
#[derive(Debug, Clone)]
pub struct ConfigSnapshot {
    pub config_version: i64,
    pub instance_id: [u8; 16],
    pub users: Arc<HashMap<UserId, UserRecord>>,
    pub settings: Arc<RuntimeSettings>,
    pub providers: Arc<HashMap<ProviderId, ProviderRecord>>,
    pub upstream_keys: Arc<HashMap<UpstreamKeyId, UpstreamKeyRecord>>,
    pub client_keys_by_hash: Arc<HashMap<[u8; 32], (ClientKeyRecord, HashRole)>>,
    pub service_accounts: Arc<HashMap<String, ServiceAccountRecord>>,
    pub models: Arc<HashMap<ModelPk, SnapshotModel>>,
    pub models_by_canonical: Arc<HashMap<String, ModelPk>>,
    pub aliases: Arc<HashMap<String, ModelPk>>,
    pub redirects: Arc<HashMap<String, ModelPk>>,
    pub routes_by_model: Arc<HashMap<ModelPk, Vec<SnapshotModelRoute>>>,
    pub shares_by_grantee: Arc<HashMap<UserId, Vec<ShareConfig>>>,
    pub user_route_prefs: Arc<HashMap<(UserId, UpstreamKeyId, ModelPk), SnapshotRoutePreference>>,
    pub prices: Arc<HashMap<PriceKey, PriceEntry>>,
    pub budget_windows: Arc<HashMap<UpstreamKeyId, Vec<SnapshotBudgetWindow>>>,
}

impl ConfigSnapshot {
    /// Lookup an in-memory user account by identifier.
    #[must_use]
    pub fn get_user(&self, user_id: &UserId) -> Option<&UserRecord> {
        self.users.get(user_id)
    }

    /// Access active runtime settings slice.
    #[must_use]
    pub fn settings(&self) -> &RuntimeSettings {
        &self.settings
    }

    /// Lookup an in-memory provider record by identifier.
    #[must_use]
    pub fn get_provider(&self, provider_id: &ProviderId) -> Option<&ProviderRecord> {
        self.providers.get(provider_id)
    }

    /// Lookup an in-memory upstream key record by identifier.
    #[must_use]
    pub fn get_upstream_key(&self, key_id: &UpstreamKeyId) -> Option<&UpstreamKeyRecord> {
        self.upstream_keys.get(key_id)
    }

    /// Resolves a client-supplied model string through aliases, redirects (max depth 4,
    /// cycle-detected), the canonical map, and finally an unambiguous upstream model ID.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotResolveError`] when the model is unknown, ambiguous, disabled,
    /// or the redirect chain loops.
    pub fn resolve_model(&self, raw_model: &str) -> Result<&SnapshotModel, SnapshotResolveError> {
        let trimmed = raw_model.trim();
        let normalized = crate::catalog::normalize_model_id(trimmed)
            .map_err(|_| SnapshotResolveError::NotFound)?;
        let stripped_vendor = trimmed.rsplit_once('/').map(|(_, s)| s).unwrap_or(trimmed);
        let stripped_norm = crate::catalog::normalize_model_id(stripped_vendor)
            .unwrap_or_else(|_| normalized.clone());

        let ci_lookup = |map: &HashMap<String, ModelPk>, needle: &str| -> Option<ModelPk> {
            map.get(needle).copied().or_else(|| {
                map.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(needle))
                    .map(|(_, &pk)| pk)
            })
        };

        let initial_pk = if let Some(pk) = ci_lookup(&self.aliases, trimmed) {
            pk
        } else if let Some(pk) = ci_lookup(&self.aliases, &normalized) {
            pk
        } else if let Some(pk) = ci_lookup(&self.aliases, stripped_vendor) {
            pk
        } else if let Some(pk) = ci_lookup(&self.models_by_canonical, &normalized) {
            pk
        } else if let Some(pk) = ci_lookup(&self.models_by_canonical, trimmed) {
            pk
        } else if let Some(pk) = ci_lookup(&self.models_by_canonical, &stripped_norm) {
            pk
        } else if let Some(pk) = ci_lookup(&self.redirects, trimmed) {
            pk
        } else if let Some(pk) = ci_lookup(&self.redirects, &normalized) {
            pk
        } else if let Some(pk) = ci_lookup(&self.redirects, &stripped_norm) {
            pk
        } else {
            // Fallback 1: exact or case-insensitive upstream model ID match when unambiguous.
            let mut found: Option<ModelPk> = None;
            for pk in self.models.keys() {
                if self.routes_by_model.get(pk).is_some_and(|routes| {
                    routes
                        .iter()
                        .any(|r| r.upstream_model_id.eq_ignore_ascii_case(trimmed))
                }) {
                    if let Some(existing) = found
                        && existing != *pk
                    {
                        return Err(SnapshotResolveError::Ambiguous);
                    }
                    found = Some(*pk);
                }
            }
            // Fallback 2: vendor-stripped suffix match (`{vendor}/{requested}`) when unambiguous (F027).
            if found.is_none() {
                for pk in self.models.keys() {
                    if self.routes_by_model.get(pk).is_some_and(|routes| {
                        routes.iter().any(|r| {
                            r.upstream_model_id
                                .rsplit_once('/')
                                .is_some_and(|(_, suffix)| {
                                    suffix.eq_ignore_ascii_case(stripped_vendor)
                                        || suffix.eq_ignore_ascii_case(&stripped_norm)
                                })
                        })
                    }) {
                        if let Some(existing) = found
                            && existing != *pk
                        {
                            return Err(SnapshotResolveError::Ambiguous);
                        }
                        found = Some(*pk);
                    }
                }
            }
            found.ok_or(SnapshotResolveError::NotFound)?
        };

        // Follow redirects keyed by the old canonical id (max 4 hops, multi-hop cycle detected; F027).
        let mut current = initial_pk;
        let mut visited = vec![current];
        for hop in 0..=4 {
            let Some(model) = self.models.get(&current) else {
                return Err(SnapshotResolveError::NotFound);
            };
            if let Some(next) = ci_lookup(&self.redirects, &model.canonical_id) {
                if hop == 4 || visited.contains(&next) {
                    return Err(SnapshotResolveError::RedirectLoop);
                }
                visited.push(next);
                current = next;
            } else {
                break;
            }
        }

        let model = self
            .models
            .get(&current)
            .ok_or(SnapshotResolveError::NotFound)?;
        if model.exposed == Some(false) {
            return Err(SnapshotResolveError::KillSwitched);
        }
        Ok(model)
    }

    /// Resolves `effective_share(grantee, provider, key)` per SRDD Section 10.1.
    ///
    /// A key-level grant takes strict precedence over a provider-wide grant, but only
    /// when that grant is currently eligible. Disabled, fingerprint-suspended, or
    /// unapproved custom-provider key grants fall back to an eligible provider grant.
    #[must_use]
    pub fn effective_share(
        &self,
        grantee: UserId,
        provider_id: ProviderId,
        key_id: UpstreamKeyId,
        provider_kind: ProviderKind,
        current_provider_fingerprint: &str,
        grantor_disabled: bool,
    ) -> Option<&ShareConfig> {
        let grants = self.shares_by_grantee.get(&grantee)?;
        crate::shares::effective_share(
            grants,
            grantee,
            provider_id,
            key_id,
            provider_kind,
            current_provider_fingerprint,
            grantor_disabled,
        )
    }

    /// Look up the price entry for a route, preferring a provider-specific row.
    #[must_use]
    pub fn price_for(
        &self,
        provider_id: &ProviderId,
        provider_kind: ProviderKind,
        upstream_model_id: &str,
    ) -> Option<&PriceEntry> {
        let pid = provider_id.to_string();
        let pkind = provider_kind.as_str().to_string();
        let raw_model = upstream_model_id.to_string();
        let stripped_tag = upstream_model_id
            .split_once(':')
            .map(|(base, _)| base.to_string());
        let norm_model = crate::catalog::normalize_model_id(upstream_model_id).ok();
        let date_stripped = norm_model
            .as_deref()
            .and_then(strip_model_date_suffix)
            .or_else(|| strip_model_date_suffix(upstream_model_id));

        let mut candidates = Vec::with_capacity(4);
        candidates.push(raw_model);
        if let Some(tag_base) = stripped_tag
            && !candidates.contains(&tag_base)
        {
            candidates.push(tag_base);
        }
        if let Some(norm) = norm_model
            && !candidates.contains(&norm)
        {
            candidates.push(norm);
        }
        if let Some(ds) = date_stripped
            && !candidates.contains(&ds)
        {
            candidates.push(ds);
        }

        for model_candidate in &candidates {
            if let Some(entry) = self.prices.get(&PriceKey {
                provider_id: pid.clone(),
                provider_kind: pkind.clone(),
                upstream_model_id: model_candidate.clone(),
            }) {
                return Some(entry);
            }
            if let Some(entry) = self.prices.get(&PriceKey {
                provider_id: String::new(),
                provider_kind: pkind.clone(),
                upstream_model_id: model_candidate.clone(),
            }) {
                return Some(entry);
            }
            if let Some(entry) = self.prices.get(&PriceKey {
                provider_id: "bundled".to_string(),
                provider_kind: "global".to_string(),
                upstream_model_id: model_candidate.clone(),
            }) {
                return Some(entry);
            }
            if let Some(entry) = self.prices.get(&PriceKey {
                provider_id: String::new(),
                provider_kind: "global".to_string(),
                upstream_model_id: model_candidate.clone(),
            }) {
                return Some(entry);
            }
        }

        self.prices
            .get(&PriceKey {
                provider_id: pid,
                provider_kind: pkind.clone(),
                upstream_model_id: "*".to_string(),
            })
            .or_else(|| {
                self.prices.get(&PriceKey {
                    provider_id: String::new(),
                    provider_kind: pkind,
                    upstream_model_id: "*".to_string(),
                })
            })
    }
}

/// Strips a trailing `-YYYY-MM-DD`, `-YYYY.MM.DD`, or `-YYYYMMDD` release-date suffix from a model slug (`F044`).
#[must_use]
pub fn strip_model_date_suffix(model: &str) -> Option<String> {
    let trimmed = model.trim();
    if trimmed.len() > 11 {
        let tail = &trimmed[trimmed.len() - 11..];
        let bytes = tail.as_bytes();
        if bytes[0] == b'-'
            && bytes[1..5].iter().all(u8::is_ascii_digit)
            && matches!(bytes[5], b'-' | b'.')
            && bytes[6..8].iter().all(u8::is_ascii_digit)
            && matches!(bytes[8], b'-' | b'.')
            && bytes[9..11].iter().all(u8::is_ascii_digit)
        {
            return Some(trimmed[..trimmed.len() - 11].to_string());
        }
    }
    if trimmed.len() > 9 {
        let tail = &trimmed[trimmed.len() - 9..];
        let bytes = tail.as_bytes();
        if bytes[0] == b'-'
            && bytes[1..9].iter().all(u8::is_ascii_digit)
            && (tail.starts_with("-202") || tail.starts_with("-203"))
        {
            return Some(trimmed[..trimmed.len() - 9].to_string());
        }
    }
    None
}

impl Default for ConfigSnapshot {
    fn default() -> Self {
        Self {
            config_version: 0,
            instance_id: [0; 16],
            users: Arc::new(HashMap::new()),
            settings: Arc::new(RuntimeSettings::default()),
            providers: Arc::new(HashMap::new()),
            upstream_keys: Arc::new(HashMap::new()),
            client_keys_by_hash: Arc::new(HashMap::new()),
            service_accounts: Arc::new(HashMap::new()),
            models: Arc::new(HashMap::new()),
            models_by_canonical: Arc::new(HashMap::new()),
            aliases: Arc::new(HashMap::new()),
            redirects: Arc::new(HashMap::new()),
            routes_by_model: Arc::new(HashMap::new()),
            shares_by_grantee: Arc::new(HashMap::new()),
            user_route_prefs: Arc::new(HashMap::new()),
            prices: Arc::new(HashMap::new()),
            budget_windows: Arc::new(HashMap::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ShareId;

    fn model(pk: ModelPk, canonical: &str) -> SnapshotModel {
        SnapshotModel {
            id: pk,
            canonical_id: canonical.to_string(),
            display_name: None,
            context_window: None,
            exposed: None,
            merge_state: MergeState::Confirmed,
            majority_vendor_prefix: None,
            distribution: "weighted".to_string(),
            routing_strategy: None,
            origin: ModelOrigin::Global,
            owner_user_id: None,
        }
    }

    #[test]
    #[doc = "Spec: MDL-6, SHR-1"]
    fn snapshot_resolution_and_key_specific_share_precedence() {
        let pk = ModelPk::generate();
        let mut snapshot = ConfigSnapshot::default();
        Arc::make_mut(&mut snapshot.models).insert(pk, model(pk, "gpt-4o"));
        Arc::make_mut(&mut snapshot.models_by_canonical).insert("gpt-4o".to_string(), pk);
        Arc::make_mut(&mut snapshot.aliases).insert("gpt4".to_string(), pk);
        assert_eq!(snapshot.resolve_model("gpt4").expect("alias").id, pk);
        assert_eq!(
            snapshot
                .resolve_model("OpenAI/GPT-4o")
                .expect("normalized")
                .id,
            pk
        );
        assert_eq!(
            snapshot.resolve_model("missing"),
            Err(SnapshotResolveError::NotFound)
        );

        let user = UserId::generate();
        let provider = ProviderId::generate();
        let key = UpstreamKeyId::generate();
        let make_share = |key_id: Option<UpstreamKeyId>, enabled: bool| ShareConfig {
            share_id: ShareId::generate(),
            grantor_user_id: UserId::generate(),
            grantee_user_id: user,
            provider_id: provider,
            upstream_key_id: key_id,
            allowed_models: None,
            max_monthly_micro_usd: None,
            accepted_config_fingerprint: Some("fp-provider".to_string()),
            admin_approved_at: None,
            enabled,
        };
        Arc::make_mut(&mut snapshot.shares_by_grantee).insert(
            user,
            vec![make_share(None, false), make_share(Some(key), true)],
        );
        let share = snapshot
            .effective_share(
                user,
                provider,
                key,
                ProviderKind::Openrouter,
                "fp-provider",
                false,
            )
            .expect("key-specific grant");
        assert!(share.enabled);
        assert_eq!(share.upstream_key_id, Some(key));

        // A disabled key-level grant must not shadow an active provider-wide grant.
        Arc::make_mut(&mut snapshot.shares_by_grantee).insert(
            user,
            vec![make_share(None, true), make_share(Some(key), false)],
        );
        let share = snapshot
            .effective_share(
                user,
                provider,
                key,
                ProviderKind::Openrouter,
                "fp-provider",
                false,
            )
            .expect("provider-wide fallback");
        assert_eq!(share.upstream_key_id, None);
    }

    #[test]
    #[doc = "Spec: MDL-6, F027 - case-insensitive alias, vendor-prefixed suffix, and multi-hop redirect cycle detection"]
    fn snapshot_resolves_case_insensitive_vendor_suffix_and_detects_multi_hop_redirect_cycle() {
        let pk_a = ModelPk::generate();
        let pk_b = ModelPk::generate();
        let mut snapshot = ConfigSnapshot::default();
        Arc::make_mut(&mut snapshot.models).insert(pk_a, model(pk_a, "gpt-5.4"));
        Arc::make_mut(&mut snapshot.models).insert(pk_b, model(pk_b, "gpt-5.4-legacy"));
        Arc::make_mut(&mut snapshot.models_by_canonical).insert("gpt-5.4".to_string(), pk_a);
        Arc::make_mut(&mut snapshot.models_by_canonical).insert("gpt-5.4-legacy".to_string(), pk_b);
        Arc::make_mut(&mut snapshot.aliases).insert("GPT5-Fast".to_string(), pk_a);

        assert_eq!(
            snapshot
                .resolve_model("gpt5-fast")
                .expect("case-insensitive alias")
                .id,
            pk_a
        );
        assert_eq!(
            snapshot
                .resolve_model("openai/gpt-5.4")
                .expect("vendor-prefixed canonical")
                .id,
            pk_a
        );

        // Two-hop redirect cycle pk_a <-> pk_b must return RedirectLoop
        Arc::make_mut(&mut snapshot.redirects).insert("gpt-5.4".to_string(), pk_b);
        Arc::make_mut(&mut snapshot.redirects).insert("gpt-5.4-legacy".to_string(), pk_a);
        assert_eq!(
            snapshot.resolve_model("gpt-5.4"),
            Err(SnapshotResolveError::RedirectLoop)
        );
    }
}
