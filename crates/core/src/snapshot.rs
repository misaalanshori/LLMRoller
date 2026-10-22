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
use crate::rbac::{OwnershipScope, Role};
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

/// In-memory projection of a catalog model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotModel {
    pub id: ModelPk,
    pub canonical_id: String,
    pub display_name: Option<String>,
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

        let initial_pk = if let Some(&pk) = self.aliases.get(trimmed) {
            pk
        } else if let Some(&pk) = self.aliases.get(&normalized) {
            pk
        } else if let Some(&pk) = self.models_by_canonical.get(&normalized) {
            pk
        } else if let Some(&pk) = self.models_by_canonical.get(trimmed) {
            pk
        } else {
            // Fallback: exact upstream model ID match when unambiguous.
            let mut found: Option<ModelPk> = None;
            for (pk, model) in self.models.iter() {
                let _ = model;
                if self
                    .routes_by_model
                    .get(pk)
                    .is_some_and(|routes| routes.iter().any(|r| r.upstream_model_id == trimmed))
                {
                    if let Some(existing) = found
                        && existing != *pk
                    {
                        return Err(SnapshotResolveError::Ambiguous);
                    }
                    found = Some(*pk);
                }
            }
            found.ok_or(SnapshotResolveError::NotFound)?
        };

        // Follow redirects keyed by the old canonical id.
        let mut current = initial_pk;
        for _ in 0..4 {
            let Some(model) = self.models.get(&current) else {
                return Err(SnapshotResolveError::NotFound);
            };
            if let Some(&next) = self.redirects.get(&model.canonical_id) {
                if next == current {
                    return Err(SnapshotResolveError::RedirectLoop);
                }
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
        let specific = PriceKey {
            provider_id: provider_id.to_string(),
            provider_kind: provider_kind.as_str().to_string(),
            upstream_model_id: upstream_model_id.to_string(),
        };
        if let Some(entry) = self.prices.get(&specific) {
            return Some(entry);
        }
        self.prices.get(&PriceKey {
            provider_id: String::new(),
            provider_kind: provider_kind.as_str().to_string(),
            upstream_model_id: upstream_model_id.to_string(),
        })
    }
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
}
