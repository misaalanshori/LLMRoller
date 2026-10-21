#![forbid(unsafe_code)]
//! Type-safe usage accounting vocabulary shared by the data plane and store.
//!
//! Spec: ./docs/srdd/13. SRDD - Pricing, Limits, and Source Provider Usage Engine.md#131-usage-accounting
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#9.4-usage-events-and-attempts
//! Requirements: USG-1, USG-3.

use serde::{Deserialize, Serialize};

/// Terminal outcome of one upstream attempt.
///
/// Every variant maps 1:1 to the `usage_attempts.outcome` SQLite `CHECK`
/// constraint, so an invalid string can never reach the writer transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Served,
    ConnectError,
    WriteStalled,
    Http429,
    Http402,
    Http401,
    Http403,
    Http5xx,
    WaitTimeout,
    ErrorFrame,
}

impl AttemptOutcome {
    /// Exact string persisted in `usage_attempts.outcome`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Served => "served",
            Self::ConnectError => "connect_error",
            Self::WriteStalled => "write_stalled",
            Self::Http429 => "http_429",
            Self::Http402 => "http_402",
            Self::Http401 => "http_401",
            Self::Http403 => "http_403",
            Self::Http5xx => "http_5xx",
            Self::WaitTimeout => "wait_timeout",
            Self::ErrorFrame => "error_frame",
        }
    }

    /// Parse a persisted outcome string.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "served" => Self::Served,
            "connect_error" => Self::ConnectError,
            "write_stalled" => Self::WriteStalled,
            "http_429" => Self::Http429,
            "http_402" => Self::Http402,
            "http_401" => Self::Http401,
            "http_403" => Self::Http403,
            "http_5xx" => Self::Http5xx,
            "wait_timeout" => Self::WaitTimeout,
            "error_frame" => Self::ErrorFrame,
            _ => return None,
        })
    }

    /// Classify a non-success upstream HTTP status.
    #[must_use]
    pub const fn from_http_status(status: u16) -> Self {
        match status {
            200..=299 => Self::Served,
            401 => Self::Http401,
            402 => Self::Http402,
            403 => Self::Http403,
            429 => Self::Http429,
            _ => Self::Http5xx,
        }
    }

    /// Whether this outcome should trigger failover to another candidate.
    #[must_use]
    pub const fn is_failover(self) -> bool {
        !matches!(self, Self::Served)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: USG-1"]
    fn outcome_strings_round_trip_and_match_sqlite_check_constraint() {
        let all = [
            AttemptOutcome::Served,
            AttemptOutcome::ConnectError,
            AttemptOutcome::WriteStalled,
            AttemptOutcome::Http429,
            AttemptOutcome::Http402,
            AttemptOutcome::Http401,
            AttemptOutcome::Http403,
            AttemptOutcome::Http5xx,
            AttemptOutcome::WaitTimeout,
            AttemptOutcome::ErrorFrame,
        ];
        for outcome in all {
            assert_eq!(
                AttemptOutcome::from_wire(outcome.as_str()),
                Some(outcome),
                "round trip for {outcome:?}"
            );
        }
        assert_eq!(
            AttemptOutcome::from_http_status(429),
            AttemptOutcome::Http429
        );
        assert_eq!(
            AttemptOutcome::from_http_status(401),
            AttemptOutcome::Http401
        );
        assert_eq!(
            AttemptOutcome::from_http_status(402),
            AttemptOutcome::Http402
        );
        assert_eq!(
            AttemptOutcome::from_http_status(403),
            AttemptOutcome::Http403
        );
        assert_eq!(
            AttemptOutcome::from_http_status(503),
            AttemptOutcome::Http5xx
        );
        assert_eq!(
            AttemptOutcome::from_http_status(200),
            AttemptOutcome::Served
        );
        assert!(!AttemptOutcome::Served.is_failover());
        assert!(AttemptOutcome::Http5xx.is_failover());
    }
}
