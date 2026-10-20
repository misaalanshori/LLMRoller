#![forbid(unsafe_code)]
//! Strongly typed runtime settings representing the closed 14-key registry.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#933-closed-runtime-settings-registry
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation

use crate::strategy::RoutingStrategy;
use serde::{Deserialize, Serialize};

/// The closed allowlist of all 14 supported runtime setting keys.
pub const ALL_SETTINGS_KEYS: [&str; 14] = [
    "pin_ttl_secs",
    "max_active_pins",
    "usage_poll_interval_secs",
    "default_routing_strategy",
    "headroom_scaling_enabled",
    "repin_on_personal_recovery",
    "recovery_repin_min_idle_secs",
    "global_exposure_mode",
    "usage_events_retention_days",
    "usage_events_max_rows",
    "drain_timeout_secs",
    "price_sync_catalog_url",
    "unpriced_fallback_rates",
    "traceability_config",
];

/// Global model exposure mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalExposureMode {
    #[default]
    Union,
    Intersection,
}

impl GlobalExposureMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Union => "union",
            Self::Intersection => "intersection",
        }
    }
}

/// Error returned when parsing an invalid global exposure mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidGlobalExposureModeError(pub String);

impl std::fmt::Display for InvalidGlobalExposureModeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "invalid exposure mode {:?}; expected union or intersection",
            self.0
        )
    }
}

impl std::error::Error for InvalidGlobalExposureModeError {}

impl std::str::FromStr for GlobalExposureMode {
    type Err = InvalidGlobalExposureModeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "union" => Ok(Self::Union),
            "intersection" => Ok(Self::Intersection),
            other => Err(InvalidGlobalExposureModeError(other.to_owned())),
        }
    }
}

/// External price sync catalog configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceSyncCatalogUrl {
    pub enabled: bool,
    pub url: String,
}

impl Default for PriceSyncCatalogUrl {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "https://models.dev/api.json".to_owned(),
        }
    }
}

/// Fallback micro-USD rates per MTok when no price entry exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnpricedFallbackRates {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

impl Default for UnpricedFallbackRates {
    fn default() -> Self {
        Self {
            input: 3_000_000,
            output: 15_000_000,
            cache_read: 300_000,
            cache_write: 3_750_000,
        }
    }
}

/// Traceability filter match lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceabilityMatch {
    #[serde(default)]
    pub user_ids: Vec<String>,
    #[serde(default)]
    pub client_key_ids: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
}

/// Admin opt-in content traceability mode configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceabilityConfig {
    pub enabled: bool,
    pub match_filters: TraceabilityMatch,
    pub retention_days: i64,
    pub auto_disable_at: Option<i64>,
}

impl Default for TraceabilityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            match_filters: TraceabilityMatch::default(),
            retention_days: 14,
            auto_disable_at: None,
        }
    }
}

/// Strongly typed runtime settings representing all 14 keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSettings {
    pub pin_ttl_secs: i64,
    pub max_active_pins: usize,
    pub usage_poll_interval_secs: i64,
    pub default_routing_strategy: RoutingStrategy,
    pub headroom_scaling_enabled: bool,
    pub repin_on_personal_recovery: bool,
    pub recovery_repin_min_idle_secs: i64,
    pub global_exposure_mode: GlobalExposureMode,
    pub usage_events_retention_days: i64,
    pub usage_events_max_rows: i64,
    pub drain_timeout_secs: i64,
    pub price_sync_catalog_url: PriceSyncCatalogUrl,
    pub unpriced_fallback_rates: UnpricedFallbackRates,
    pub traceability_config: TraceabilityConfig,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            pin_ttl_secs: 86_400,
            max_active_pins: 250_000,
            usage_poll_interval_secs: 300,
            default_routing_strategy: RoutingStrategy::SmoothWeightedRr,
            headroom_scaling_enabled: false,
            repin_on_personal_recovery: true,
            recovery_repin_min_idle_secs: 300,
            global_exposure_mode: GlobalExposureMode::Union,
            usage_events_retention_days: 30,
            usage_events_max_rows: 50_000_000,
            drain_timeout_secs: 60,
            price_sync_catalog_url: PriceSyncCatalogUrl::default(),
            unpriced_fallback_rates: UnpricedFallbackRates::default(),
            traceability_config: TraceabilityConfig::default(),
        }
    }
}
