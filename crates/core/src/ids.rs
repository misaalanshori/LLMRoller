#![forbid(unsafe_code)]
//! ULID-backed domain identifiers.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#91-per-connection-pragma-builder-and-complete-31-table-sqlite-ddl-schema

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Error returned when a canonical ULID string cannot be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdParseError(String);

impl IdParseError {
    fn from_decode(error: ulid::DecodeError) -> Self {
        Self(error.to_string())
    }
}

impl fmt::Display for IdParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid ULID: {}", self.0)
    }
}

impl std::error::Error for IdParseError {}

macro_rules! define_ulid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Ulid);

        impl $name {
            /// Generate a new time-sortable identifier from the operating system clock and entropy.
            #[must_use]
            pub fn generate() -> Self {
                Self(Ulid::generate())
            }

            /// Wrap an existing ULID.
            #[must_use]
            pub const fn from_ulid(value: Ulid) -> Self {
                Self(value)
            }

            /// Return the underlying ULID.
            #[must_use]
            pub const fn as_ulid(self) -> Ulid {
                self.0
            }

            /// Parse a canonical 26-character ULID.
            ///
            /// # Errors
            ///
            /// Returns [`IdParseError`] when the input is not a valid Crockford base32 ULID.
            pub fn parse(value: &str) -> Result<Self, IdParseError> {
                Ulid::from_string(value)
                    .map(Self)
                    .map_err(IdParseError::from_decode)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(self, formatter)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdParseError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<Ulid> for $name {
            fn from(value: Ulid) -> Self {
                Self::from_ulid(value)
            }
        }

        impl From<$name> for Ulid {
            fn from(value: $name) -> Self {
                value.as_ulid()
            }
        }

        impl AsRef<Ulid> for $name {
            fn as_ref(&self) -> &Ulid {
                &self.0
            }
        }
    };
}

define_ulid_id! {
    /// Identifier of a human LLMRoller user.
    UserId
}

define_ulid_id! {
    /// Identifier of a provider record.
    ProviderId
}

define_ulid_id! {
    /// Identifier of an upstream credential.
    UpstreamKeyId
}

define_ulid_id! {
    /// Identifier of a client bearer key (`llr_...`).
    ClientKeyId
}

define_ulid_id! {
    /// Identifier of a personal-provider sharing grant.
    ShareId
}

define_ulid_id! {
    /// Identifier of a machine principal Service Account (`CLK-6`).
    ServiceAccountId
}

define_ulid_id! {
    /// Immutable surrogate key of a canonical model.
    ModelPk
}

define_ulid_id! {
    /// Identifier of one proxied request.
    RequestId
}

define_ulid_id! {
    /// Identifier of one finalized usage event.
    UsageEventId
}

/// Validate that a string is a canonical 26-character Crockford base32 ULID.
///
/// # Errors
///
/// Returns [`IdParseError`] when the input is not a canonical ULID.
pub fn validate_ulid(value: &str) -> Result<(), IdParseError> {
    Ulid::from_string(value)
        .map(|_| ())
        .map_err(IdParseError::from_decode)
}

#[cfg(test)]
mod tests;
