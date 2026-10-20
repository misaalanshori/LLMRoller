#![forbid(unsafe_code)]
//! Provider kinds and definitions.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv

use serde::{Deserialize, Serialize};

/// Supported provider kinds matching the SQLite schema CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpencodeGo,
    CommandCode,
    OpenDesignAmr,
    Openrouter,
    Custom,
}

impl ProviderKind {
    /// Lowercase string representation matching the database enum.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpencodeGo => "opencode_go",
            Self::CommandCode => "command_code",
            Self::OpenDesignAmr => "open_design_amr",
            Self::Openrouter => "openrouter",
            Self::Custom => "custom",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Error returned when parsing an unrecognized provider kind string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidProviderKindError(pub String);

impl std::fmt::Display for InvalidProviderKindError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "invalid provider kind {:?}; expected opencode_go, command_code, open_design_amr, openrouter, or custom",
            self.0
        )
    }
}

impl std::error::Error for InvalidProviderKindError {}

impl std::str::FromStr for ProviderKind {
    type Err = InvalidProviderKindError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "opencode_go" => Ok(Self::OpencodeGo),
            "command_code" => Ok(Self::CommandCode),
            "open_design_amr" => Ok(Self::OpenDesignAmr),
            "openrouter" => Ok(Self::Openrouter),
            "custom" => Ok(Self::Custom),
            other => Err(InvalidProviderKindError(other.to_owned())),
        }
    }
}
