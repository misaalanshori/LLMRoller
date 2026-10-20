#![forbid(unsafe_code)]
//! Immutable configuration snapshot and cache invalidation definitions.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#2162-versioned-configsnapshot-publication-reloader

use std::collections::HashMap;
use std::sync::Arc;

use crate::client_key::{ClientKeyRecord, HashRole};
use crate::ids::{ClientKeyId, ProviderId, UpstreamKeyId, UserId};
use crate::provider::ProviderKind;
use crate::rbac::{OwnershipScope, Role};
use crate::settings::RuntimeSettings;
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
    pub enabled: bool,
    pub priority: i64,
    pub strict_tier: i64,
    pub health: String,
    pub health_version: i64,
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
}

/// Immutable configuration snapshot loaded into [`arc_swap::ArcSwap`].
#[derive(Debug, Clone)]
pub struct ConfigSnapshot {
    pub config_version: i64,
    pub users: Arc<HashMap<UserId, UserRecord>>,
    pub settings: Arc<RuntimeSettings>,
    pub providers: Arc<HashMap<ProviderId, ProviderRecord>>,
    pub upstream_keys: Arc<HashMap<UpstreamKeyId, UpstreamKeyRecord>>,
    pub client_keys_by_hash: Arc<HashMap<[u8; 32], (ClientKeyRecord, HashRole)>>,
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
}

impl Default for ConfigSnapshot {
    fn default() -> Self {
        Self {
            config_version: 0,
            users: Arc::new(HashMap::new()),
            settings: Arc::new(RuntimeSettings::default()),
            providers: Arc::new(HashMap::new()),
            upstream_keys: Arc::new(HashMap::new()),
            client_keys_by_hash: Arc::new(HashMap::new()),
        }
    }
}
