#![forbid(unsafe_code)]
//! Immutable configuration snapshot and cache invalidation definitions.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#2162-versioned-configsnapshot-publication-reloader

use std::collections::HashMap;
use std::sync::Arc;

use crate::ids::UserId;
use crate::rbac::Role;
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

/// Cache invalidation events applied immediately after atomic snapshot publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheInvalidationEvent {
    UserChanged(UserId),
    OwnerDisabled(UserId),
    SettingsChanged,
    StrategyChanged,
}

/// Immutable configuration snapshot loaded into [`arc_swap::ArcSwap`].
#[derive(Debug, Clone)]
pub struct ConfigSnapshot {
    pub config_version: i64,
    pub users: Arc<HashMap<UserId, UserRecord>>,
    pub settings_json: Arc<HashMap<String, String>>,
}

impl Default for ConfigSnapshot {
    fn default() -> Self {
        Self {
            config_version: 0,
            users: Arc::new(HashMap::new()),
            settings_json: Arc::new(HashMap::new()),
        }
    }
}
