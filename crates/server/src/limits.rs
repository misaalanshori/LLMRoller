#![forbid(unsafe_code)]
//! Runtime memory and request-body limits derived from the deployment environment.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes
//! Spec: ./docs/srdd/22. SRDD - Deployment and Containers.md#223-environment-variable-contract
//! Requirements: SEC-8, PERF-3, PERF-5

use std::env;

use llmroller_core::strategy::RoutingStrategy;
use thiserror::Error;

/// Environment variable holding the explicit container memory ceiling in MiB.
pub const MEMORY_LIMIT_ENV: &str = "LLMROLLER_MEMORY_LIMIT_MB";
/// Environment variable holding the pre-first-frame body memory budget in MiB.
pub const PREFLIGHT_BODY_BUDGET_ENV: &str = "LLMROLLER_PREFLIGHT_BODY_BUDGET_MB";
/// Environment variable holding the maximum inbound request body in MiB.
pub const MAX_BODY_ENV: &str = "LLMROLLER_MAX_BODY_MB";

/// Environment variable selecting the initial default routing strategy.
pub const ROUTING_STRATEGY_ENV: &str = "LLMROLLER_DEFAULT_ROUTING_STRATEGY";

/// Memory ceiling assumed when neither the environment nor cgroup v2 supplies one.
pub const DEFAULT_MEMORY_LIMIT_MB: usize = 2048;
/// Lower bound for an explicit `LLMROLLER_MEMORY_LIMIT_MB`.
pub const MIN_MEMORY_LIMIT_MB: usize = 256;
/// Upper bound for an explicit `LLMROLLER_MEMORY_LIMIT_MB`.
pub const MAX_MEMORY_LIMIT_MB: usize = 65536;
/// Default maximum inbound request body in MiB.
pub const DEFAULT_MAX_BODY_MB: usize = 32;
/// Lower bound for `LLMROLLER_MAX_BODY_MB`.
pub const MIN_MAX_BODY_MB: usize = 1;
/// Upper bound for `LLMROLLER_MAX_BODY_MB`.
pub const MAX_MAX_BODY_MB: usize = 128;
/// Upper bound applied while deriving the default pre-flight body budget.
pub const MAX_PREFLIGHT_BODY_BUDGET_MB: usize = 1024;
/// Upper bound applied to the trace accumulator budget (PERF-5).
pub const MAX_TRACE_ACCUMULATOR_MB: usize = 256;

const MIB: usize = 1024 * 1024;

/// Validated memory and body limits for the proxy edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyLimits {
    /// Container memory ceiling in MiB.
    pub memory_limit_mb: usize,
    /// Total MiB of buffered request bodies permitted before the first valid frame.
    pub preflight_body_budget_mb: usize,
    /// Maximum inbound request body in MiB (`SEC-8`).
    pub max_body_mb: usize,
}

impl ProxyLimits {
    /// Load and validate limits from the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`LimitsError`] when a variable is not an integer, is outside its
    /// documented range, or violates the `MAX_BODY_MB <= budget / 4` invariant.
    pub fn from_env() -> Result<Self, LimitsError> {
        let memory_limit = env::var(MEMORY_LIMIT_ENV).ok();
        let preflight_body_budget = env::var(PREFLIGHT_BODY_BUDGET_ENV).ok();
        let max_body = env::var(MAX_BODY_ENV).ok();
        Self::parse(
            memory_limit.as_deref(),
            preflight_body_budget.as_deref(),
            max_body.as_deref(),
            detect_cgroup_memory_mb(),
        )
    }

    /// Parse limits from raw environment values and an optional cgroup ceiling.
    ///
    /// # Errors
    ///
    /// See [`Self::from_env`].
    pub fn parse(
        memory_limit: Option<&str>,
        preflight_body_budget: Option<&str>,
        max_body: Option<&str>,
        detected_memory_mb: Option<usize>,
    ) -> Result<Self, LimitsError> {
        let memory_limit_mb = match memory_limit {
            Some(raw) => parse_bounded(
                MEMORY_LIMIT_ENV,
                raw,
                MIN_MEMORY_LIMIT_MB,
                MAX_MEMORY_LIMIT_MB,
            )?,
            None => detected_memory_mb
                .filter(|detected| *detected > 0)
                .unwrap_or(DEFAULT_MEMORY_LIMIT_MB),
        };

        let preflight_body_budget_mb = match preflight_body_budget {
            Some(raw) => parse_bounded(PREFLIGHT_BODY_BUDGET_ENV, raw, 1, MAX_MEMORY_LIMIT_MB)?,
            None => (memory_limit_mb.saturating_mul(2) / 5).min(MAX_PREFLIGHT_BODY_BUDGET_MB),
        };

        let max_body_mb = match max_body {
            Some(raw) => parse_bounded(MAX_BODY_ENV, raw, MIN_MAX_BODY_MB, MAX_MAX_BODY_MB)?,
            None => DEFAULT_MAX_BODY_MB,
        };

        if max_body_mb > preflight_body_budget_mb / 4 {
            return Err(LimitsError::MaxBodyExceedsQuarterBudget {
                max_body_mb,
                preflight_body_budget_mb,
            });
        }

        Ok(Self {
            memory_limit_mb,
            preflight_body_budget_mb,
            max_body_mb,
        })
    }

    /// Global trace accumulator budget: `min(256 MiB, 10% of memory_limit)` (`PERF-5`).
    #[must_use]
    pub const fn trace_accumulator_budget_bytes(&self) -> usize {
        let ten_percent_mb = self.memory_limit_mb / 10;
        let budget_mb = if ten_percent_mb < MAX_TRACE_ACCUMULATOR_MB {
            ten_percent_mb
        } else {
            MAX_TRACE_ACCUMULATOR_MB
        };
        budget_mb.saturating_mul(MIB)
    }
}

/// Parse the optional default routing strategy from a raw environment value.
///
/// # Errors
///
/// Returns [`LimitsError::InvalidRoutingStrategy`] when the value is not one of
/// the supported strategy names or aliases (`RTE-14`, exit `2`).
pub fn parse_routing_strategy(raw: Option<&str>) -> Result<Option<RoutingStrategy>, LimitsError> {
    let Some(value) = raw else {
        return Ok(None);
    };
    if value.trim().is_empty() {
        return Ok(None);
    }
    value
        .trim()
        .parse::<RoutingStrategy>()
        .map(Some)
        .map_err(|_| LimitsError::InvalidRoutingStrategy {
            value: value.to_string(),
        })
}

/// Load the optional default routing strategy from the process environment.
///
/// # Errors
///
/// See [`parse_routing_strategy`].
pub fn routing_strategy_from_env() -> Result<Option<RoutingStrategy>, LimitsError> {
    parse_routing_strategy(env::var(ROUTING_STRATEGY_ENV).ok().as_deref())
}

/// Validation failure for the memory and body-limit environment contract.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LimitsError {
    /// The value could not be parsed as an unsigned integer.
    #[error("{variable} must be a positive integer, got {value:?}")]
    NotAnInteger {
        /// Offending environment variable.
        variable: &'static str,
        /// Raw value supplied by the operator.
        value: String,
    },
    /// The value parsed but is outside the documented range.
    #[error("{variable}={value} is outside the allowed range {min}..={max}")]
    OutOfRange {
        /// Offending environment variable.
        variable: &'static str,
        /// Parsed value.
        value: usize,
        /// Minimum accepted value.
        min: usize,
        /// Maximum accepted value.
        max: usize,
    },
    /// `MAX_BODY_MB` is larger than a quarter of the pre-flight budget.
    #[error(
        "{MAX_BODY_ENV}={max_body_mb} must be <= {PREFLIGHT_BODY_BUDGET_ENV}/4 ({preflight_body_budget_mb}/4)"
    )]
    MaxBodyExceedsQuarterBudget {
        /// Requested maximum body size.
        max_body_mb: usize,
        /// Configured pre-flight budget.
        preflight_body_budget_mb: usize,
    },
    /// The routing-strategy variable holds an unsupported value.
    #[error("invalid {ROUTING_STRATEGY_ENV}={value:?}; expected a supported routing strategy")]
    InvalidRoutingStrategy {
        /// Raw value supplied by the operator.
        value: String,
    },
}

fn parse_bounded(
    variable: &'static str,
    raw: &str,
    min: usize,
    max: usize,
) -> Result<usize, LimitsError> {
    let value = raw
        .trim()
        .parse::<usize>()
        .map_err(|_| LimitsError::NotAnInteger {
            variable,
            value: raw.to_string(),
        })?;
    if !(min..=max).contains(&value) {
        return Err(LimitsError::OutOfRange {
            variable,
            value,
            min,
            max,
        });
    }
    Ok(value)
}

/// Detect the container memory ceiling from cgroup v2 `memory.max`.
///
/// Returns `None` when the file is absent, unreadable, or reports `max`
/// (an unlimited cgroup). On non-Linux platforms this is always `None`.
#[must_use]
pub fn detect_cgroup_memory_mb() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        let raw = std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("max") {
            return None;
        }
        let bytes = trimmed.parse::<u64>().ok()?;
        if bytes == 0 {
            return None;
        }
        usize::try_from(bytes / (MIB as u64))
            .ok()
            .filter(|mb| *mb > 0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: SEC-8"]
    fn max_body_must_not_exceed_a_quarter_of_the_preflight_budget() {
        let error = ProxyLimits::parse(Some("2048"), Some("400"), Some("101"), None)
            .expect_err("101 MiB is more than a quarter of 400 MiB");
        assert_eq!(
            error,
            LimitsError::MaxBodyExceedsQuarterBudget {
                max_body_mb: 101,
                preflight_body_budget_mb: 400,
            }
        );

        let limits = ProxyLimits::parse(Some("2048"), Some("400"), Some("100"), None)
            .expect("100 MiB fits in a quarter of 400 MiB");
        assert_eq!(limits.max_body_mb, 100);
    }

    #[test]
    #[doc = "Spec: SEC-8"]
    fn invalid_integer_is_rejected() {
        let error = ProxyLimits::parse(Some("two gigabytes"), None, None, None)
            .expect_err("non-numeric memory limit must fail");
        assert_eq!(
            error,
            LimitsError::NotAnInteger {
                variable: MEMORY_LIMIT_ENV,
                value: "two gigabytes".to_string(),
            }
        );
    }

    #[test]
    #[doc = "Spec: SEC-8"]
    fn out_of_range_values_are_rejected() {
        assert_eq!(
            ProxyLimits::parse(Some("255"), None, None, None),
            Err(LimitsError::OutOfRange {
                variable: MEMORY_LIMIT_ENV,
                value: 255,
                min: MIN_MEMORY_LIMIT_MB,
                max: MAX_MEMORY_LIMIT_MB,
            })
        );
        assert_eq!(
            ProxyLimits::parse(Some("2048"), None, Some("0"), None),
            Err(LimitsError::OutOfRange {
                variable: MAX_BODY_ENV,
                value: 0,
                min: MIN_MAX_BODY_MB,
                max: MAX_MAX_BODY_MB,
            })
        );
        assert_eq!(
            ProxyLimits::parse(Some("2048"), None, Some("129"), None),
            Err(LimitsError::OutOfRange {
                variable: MAX_BODY_ENV,
                value: 129,
                min: MIN_MAX_BODY_MB,
                max: MAX_MAX_BODY_MB,
            })
        );
    }

    #[test]
    #[doc = "Spec: PERF-3"]
    fn defaults_follow_the_documented_two_gigabyte_formula() {
        let limits = ProxyLimits::parse(None, None, None, None).expect("defaults are valid");
        assert_eq!(limits.memory_limit_mb, DEFAULT_MEMORY_LIMIT_MB);
        assert_eq!(limits.preflight_body_budget_mb, 819);
        assert_eq!(limits.max_body_mb, DEFAULT_MAX_BODY_MB);
        assert_eq!(limits.trace_accumulator_budget_bytes(), 204 * MIB);
    }

    #[test]
    #[doc = "Spec: PERF-3"]
    fn detected_cgroup_memory_sizes_the_default_budget() {
        let limits =
            ProxyLimits::parse(None, None, None, Some(4096)).expect("detected memory is valid");
        assert_eq!(limits.memory_limit_mb, 4096);
        assert_eq!(limits.preflight_body_budget_mb, 1024);
        assert_eq!(limits.trace_accumulator_budget_bytes(), 256 * MIB);
    }

    #[test]
    #[doc = "Spec: PERF-5"]
    fn trace_budget_is_capped_at_two_hundred_fifty_six_mebibytes() {
        let limits =
            ProxyLimits::parse(Some("65536"), None, Some("1"), None).expect("valid limits");
        assert_eq!(limits.trace_accumulator_budget_bytes(), 256 * MIB);
    }

    #[test]
    #[doc = "Spec: RTE-14"]
    fn routing_strategy_accepts_canonical_names_aliases_and_blank() {
        assert_eq!(parse_routing_strategy(None), Ok(None));
        assert_eq!(parse_routing_strategy(Some("  ")), Ok(None));
        assert_eq!(
            parse_routing_strategy(Some("p2c")),
            Ok(Some(RoutingStrategy::PowerOfTwoChoices))
        );
        assert_eq!(
            parse_routing_strategy(Some(" random ")),
            Ok(Some(RoutingStrategy::UniformRandom))
        );
        assert_eq!(
            parse_routing_strategy(Some("unknown_strategy")),
            Err(LimitsError::InvalidRoutingStrategy {
                value: "unknown_strategy".to_string(),
            })
        );
    }
}
