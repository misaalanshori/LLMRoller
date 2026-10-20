#![forbid(unsafe_code)]
//! Pluggable routing strategy definitions.
//!
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#103-pluggable-routing-strategy-engine-effective-priority-and-headroom-scaling
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#1031-common-routeselector-trait-and-routingstrategy-enum-rte-4-rte-14

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Configurable routing strategy algorithms.
///
/// Partial: RTE-14
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum RoutingStrategy {
    /// Nginx smooth weighted round-robin (default).
    #[default]
    SmoothWeightedRr,
    /// Unweighted cyclic round-robin.
    NaiveRr,
    /// Classical GCD block-weighted cyclic round-robin.
    WeightedRr,
    /// Uniform pseudo-random distribution (alias "random").
    UniformRandom,
    /// Cumulative-weight binary search random distribution.
    WeightedRandom,
    /// Power of two choices sampling (alias "p2c").
    PowerOfTwoChoices,
}

impl RoutingStrategy {
    /// Canonical snake_case name of the strategy.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SmoothWeightedRr => "smooth_weighted_rr",
            Self::NaiveRr => "naive_rr",
            Self::WeightedRr => "weighted_rr",
            Self::UniformRandom => "uniform_random",
            Self::WeightedRandom => "weighted_random",
            Self::PowerOfTwoChoices => "power_of_two_choices",
        }
    }

    /// Whether this strategy considers route priority weights `W_eff`.
    #[must_use]
    pub const fn is_weighted(self) -> bool {
        match self {
            Self::SmoothWeightedRr
            | Self::WeightedRr
            | Self::WeightedRandom
            | Self::PowerOfTwoChoices => true,
            Self::NaiveRr | Self::UniformRandom => false,
        }
    }
}

impl fmt::Display for RoutingStrategy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Parse error for invalid routing strategy strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidStrategyError(pub String);

impl fmt::Display for InvalidStrategyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid routing strategy {:?}; valid strategies: smooth_weighted_rr, naive_rr, weighted_rr, uniform_random (random), weighted_random, power_of_two_choices (p2c)",
            self.0
        )
    }
}

impl std::error::Error for InvalidStrategyError {}

impl FromStr for RoutingStrategy {
    type Err = InvalidStrategyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "smooth_weighted_rr" => Ok(Self::SmoothWeightedRr),
            "naive_rr" => Ok(Self::NaiveRr),
            "weighted_rr" => Ok(Self::WeightedRr),
            "uniform_random" | "random" => Ok(Self::UniformRandom),
            "weighted_random" => Ok(Self::WeightedRandom),
            "power_of_two_choices" | "p2c" => Ok(Self::PowerOfTwoChoices),
            other => Err(InvalidStrategyError(other.to_owned())),
        }
    }
}

impl Serialize for RoutingStrategy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RoutingStrategy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Partial: RTE-14"]
    fn strategy_from_str_and_alias_deserialization() {
        assert_eq!(
            RoutingStrategy::from_str("smooth_weighted_rr").expect("parse SWRR"),
            RoutingStrategy::SmoothWeightedRr
        );
        assert_eq!(
            RoutingStrategy::from_str("naive_rr").expect("parse Naive"),
            RoutingStrategy::NaiveRr
        );
        assert_eq!(
            RoutingStrategy::from_str("weighted_rr").expect("parse WRR"),
            RoutingStrategy::WeightedRr
        );
        assert_eq!(
            RoutingStrategy::from_str("uniform_random").expect("parse Uniform"),
            RoutingStrategy::UniformRandom
        );
        assert_eq!(
            RoutingStrategy::from_str("random").expect("parse random alias"),
            RoutingStrategy::UniformRandom
        );
        assert_eq!(
            RoutingStrategy::from_str("weighted_random").expect("parse WeightedRandom"),
            RoutingStrategy::WeightedRandom
        );
        assert_eq!(
            RoutingStrategy::from_str("power_of_two_choices").expect("parse P2C"),
            RoutingStrategy::PowerOfTwoChoices
        );
        assert_eq!(
            RoutingStrategy::from_str("p2c").expect("parse p2c alias"),
            RoutingStrategy::PowerOfTwoChoices
        );
        assert!(RoutingStrategy::from_str("unknown_strategy").is_err());

        // Serde JSON roundtrip and alias deserialization
        let json_str = "\"random\"";
        let deserialized: RoutingStrategy =
            serde_json::from_str(json_str).expect("deserialize random");
        assert_eq!(deserialized, RoutingStrategy::UniformRandom);

        let json_p2c = "\"p2c\"";
        let deserialized_p2c: RoutingStrategy =
            serde_json::from_str(json_p2c).expect("deserialize p2c");
        assert_eq!(deserialized_p2c, RoutingStrategy::PowerOfTwoChoices);

        let serialized = serde_json::to_string(&RoutingStrategy::UniformRandom).expect("serialize");
        assert_eq!(serialized, "\"uniform_random\"");
    }
}
