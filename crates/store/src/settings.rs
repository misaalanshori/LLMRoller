#![forbid(unsafe_code)]
//! Closed 14-key runtime settings registry and validation.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#933-closed-runtime-settings-registry
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#56-environment-variables-and-runtime-settings-registry

use std::str::FromStr;

use llmroller_core::strategy::RoutingStrategy;
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};
use thiserror::Error;

/// Closed 14 settings keys.
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

impl FromStr for GlobalExposureMode {
    type Err = SettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "union" => Ok(Self::Union),
            "intersection" => Ok(Self::Intersection),
            other => Err(SettingsError::InvalidValue {
                key: "global_exposure_mode".to_owned(),
                message: format!("invalid exposure mode {other:?}; expected union or intersection"),
            }),
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

/// Settings errors mapped to HTTP 422 Unprocessable Entity.
#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("unknown setting key: {0}")]
    UnknownKey(String),
    #[error("invalid value for {key}: {message}")]
    InvalidValue { key: String, message: String },
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl SettingsError {
    #[must_use]
    pub const fn status_code(&self) -> u16 {
        422
    }
}

/// Validate a settings key and value against the closed 14-key registry.
pub fn validate_setting(key: &str, value_json: &str) -> Result<String, SettingsError> {
    if !ALL_SETTINGS_KEYS.contains(&key) {
        return Err(SettingsError::UnknownKey(key.to_owned()));
    }

    match key {
        "pin_ttl_secs" => {
            let val: i64 = serde_json::from_str(value_json)?;
            if !(300..=2_592_000).contains(&val) {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "pin_ttl_secs must be between 300 and 2,592,000 seconds (30 days)"
                        .to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        "max_active_pins" => {
            let val: usize = serde_json::from_str(value_json)?;
            if !(1_000..=1_000_000).contains(&val) {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "max_active_pins must be between 1,000 and 1,000,000".to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        "usage_poll_interval_secs" => {
            let val: i64 = serde_json::from_str(value_json)?;
            if !(60..=3600).contains(&val) {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "usage_poll_interval_secs must be between 60 and 3600 seconds"
                        .to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        "default_routing_strategy" => {
            let strategy: RoutingStrategy =
                serde_json::from_str(value_json).map_err(|e| SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: e.to_string(),
                })?;
            Ok(serde_json::to_string(&strategy)?)
        }
        "headroom_scaling_enabled" => {
            let val: bool = serde_json::from_str(value_json)?;
            Ok(serde_json::to_string(&val)?)
        }
        "repin_on_personal_recovery" => {
            let val: bool = serde_json::from_str(value_json)?;
            Ok(serde_json::to_string(&val)?)
        }
        "recovery_repin_min_idle_secs" => {
            let val: i64 = serde_json::from_str(value_json)?;
            if !(0..=86400).contains(&val) {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "recovery_repin_min_idle_secs must be between 0 and 86400 seconds"
                        .to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        "global_exposure_mode" => {
            let raw: String = serde_json::from_str(value_json)?;
            let mode = GlobalExposureMode::from_str(&raw)?;
            Ok(serde_json::to_string(&mode)?)
        }
        "usage_events_retention_days" => {
            let val: i64 = serde_json::from_str(value_json)?;
            if !(1..=365).contains(&val) {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "usage_events_retention_days must be between 1 and 365 days"
                        .to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        "usage_events_max_rows" => {
            let val: i64 = serde_json::from_str(value_json)?;
            if val < 1_000_000 {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "usage_events_max_rows must be at least 1,000,000".to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        "drain_timeout_secs" => {
            let val: i64 = serde_json::from_str(value_json)?;
            if !(0..=3600).contains(&val) {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "drain_timeout_secs must be between 0 and 3600 seconds".to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        "price_sync_catalog_url" => {
            let val: PriceSyncCatalogUrl = serde_json::from_str(value_json)?;
            if val.enabled && !val.url.starts_with("https://") {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "price_sync_catalog_url must use https://".to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        "unpriced_fallback_rates" => {
            let val: UnpricedFallbackRates = serde_json::from_str(value_json)?;
            if val.input < 0 || val.output < 0 || val.cache_read < 0 || val.cache_write < 0 {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "unpriced_fallback_rates must be non-negative".to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        "traceability_config" => {
            let val: TraceabilityConfig = serde_json::from_str(value_json)?;
            if !(1..=90).contains(&val.retention_days) {
                return Err(SettingsError::InvalidValue {
                    key: key.to_owned(),
                    message: "traceability retention_days must be between 1 and 90 days".to_owned(),
                });
            }
            Ok(serde_json::to_string(&val)?)
        }
        other => Err(SettingsError::UnknownKey(other.to_owned())),
    }
}

/// Seed defaults for any missing settings keys into SQLite.
pub async fn seed_defaults(
    pool: &SqlitePool,
    default_strategy: Option<RoutingStrategy>,
    now_ms: i64,
) -> Result<(), SettingsError> {
    let mut defaults = RuntimeSettings::default();
    if let Some(strat) = default_strategy {
        defaults.default_routing_strategy = strat;
    }

    let default_entries: [(&str, String); 14] = [
        (
            "pin_ttl_secs",
            serde_json::to_string(&defaults.pin_ttl_secs)?,
        ),
        (
            "max_active_pins",
            serde_json::to_string(&defaults.max_active_pins)?,
        ),
        (
            "usage_poll_interval_secs",
            serde_json::to_string(&defaults.usage_poll_interval_secs)?,
        ),
        (
            "default_routing_strategy",
            serde_json::to_string(&defaults.default_routing_strategy)?,
        ),
        (
            "headroom_scaling_enabled",
            serde_json::to_string(&defaults.headroom_scaling_enabled)?,
        ),
        (
            "repin_on_personal_recovery",
            serde_json::to_string(&defaults.repin_on_personal_recovery)?,
        ),
        (
            "recovery_repin_min_idle_secs",
            serde_json::to_string(&defaults.recovery_repin_min_idle_secs)?,
        ),
        (
            "global_exposure_mode",
            serde_json::to_string(&defaults.global_exposure_mode)?,
        ),
        (
            "usage_events_retention_days",
            serde_json::to_string(&defaults.usage_events_retention_days)?,
        ),
        (
            "usage_events_max_rows",
            serde_json::to_string(&defaults.usage_events_max_rows)?,
        ),
        (
            "drain_timeout_secs",
            serde_json::to_string(&defaults.drain_timeout_secs)?,
        ),
        (
            "price_sync_catalog_url",
            serde_json::to_string(&defaults.price_sync_catalog_url)?,
        ),
        (
            "unpriced_fallback_rates",
            serde_json::to_string(&defaults.unpriced_fallback_rates)?,
        ),
        (
            "traceability_config",
            serde_json::to_string(&defaults.traceability_config)?,
        ),
    ];

    for (key, val) in default_entries {
        sqlx::query(
            "INSERT OR IGNORE INTO settings (key, value_json, updated_at) VALUES (?, ?, ?)",
        )
        .bind(key)
        .bind(val)
        .bind(now_ms)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Load the complete strongly typed [`RuntimeSettings`] from SQLite.
pub async fn load_settings(pool: &SqlitePool) -> Result<RuntimeSettings, SettingsError> {
    let rows = sqlx::query("SELECT key, value_json FROM settings")
        .fetch_all(pool)
        .await?;

    let mut settings = RuntimeSettings::default();
    for row in rows {
        let key: String = row.try_get("key")?;
        let value_json: String = row.try_get("value_json")?;
        match key.as_str() {
            "pin_ttl_secs" => settings.pin_ttl_secs = serde_json::from_str(&value_json)?,
            "max_active_pins" => settings.max_active_pins = serde_json::from_str(&value_json)?,
            "usage_poll_interval_secs" => {
                settings.usage_poll_interval_secs = serde_json::from_str(&value_json)?
            }
            "default_routing_strategy" => {
                settings.default_routing_strategy = serde_json::from_str(&value_json)?
            }
            "headroom_scaling_enabled" => {
                settings.headroom_scaling_enabled = serde_json::from_str(&value_json)?
            }
            "repin_on_personal_recovery" => {
                settings.repin_on_personal_recovery = serde_json::from_str(&value_json)?
            }
            "recovery_repin_min_idle_secs" => {
                settings.recovery_repin_min_idle_secs = serde_json::from_str(&value_json)?
            }
            "global_exposure_mode" => {
                settings.global_exposure_mode = serde_json::from_str(&value_json)?
            }
            "usage_events_retention_days" => {
                settings.usage_events_retention_days = serde_json::from_str(&value_json)?
            }
            "usage_events_max_rows" => {
                settings.usage_events_max_rows = serde_json::from_str(&value_json)?
            }
            "drain_timeout_secs" => {
                settings.drain_timeout_secs = serde_json::from_str(&value_json)?
            }
            "price_sync_catalog_url" => {
                settings.price_sync_catalog_url = serde_json::from_str(&value_json)?
            }
            "unpriced_fallback_rates" => {
                settings.unpriced_fallback_rates = serde_json::from_str(&value_json)?
            }
            "traceability_config" => {
                settings.traceability_config = serde_json::from_str(&value_json)?
            }
            _ => {}
        }
    }
    Ok(settings)
}

/// Update a single setting in the database after validating against the closed registry.
pub async fn update_setting(
    pool: &SqlitePool,
    key: &str,
    value_json: &str,
    now_ms: i64,
) -> Result<String, SettingsError> {
    let validated = validate_setting(key, value_json)?;
    sqlx::query(
        "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(&validated)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(validated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Partial: RTE-14"]
    fn settings_validation_enforces_14_closed_keys_and_rejects_unknown() {
        assert!(validate_setting("unknown_key", "123").is_err());
        assert_eq!(
            validate_setting("unknown_key", "123")
                .expect_err("unknown")
                .status_code(),
            422
        );

        // Routing strategy with alias
        let valid_strat =
            validate_setting("default_routing_strategy", "\"random\"").expect("random");
        assert_eq!(valid_strat, "\"uniform_random\"");

        let valid_p2c = validate_setting("default_routing_strategy", "\"p2c\"").expect("p2c");
        assert_eq!(valid_p2c, "\"power_of_two_choices\"");

        assert!(validate_setting("default_routing_strategy", "\"invalid_strategy\"").is_err());

        // Range checks
        assert!(validate_setting("pin_ttl_secs", "299").is_err());
        assert!(validate_setting("pin_ttl_secs", "300").is_ok());

        assert!(validate_setting("max_active_pins", "999").is_err());
        assert!(validate_setting("max_active_pins", "250000").is_ok());
    }
}
