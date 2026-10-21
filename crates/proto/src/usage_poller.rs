//! Source provider /usage endpoint polling, single-flight deduplication, and parsing.
//!
//! Spec: Section 2.4, Section 6.3, Section 6.9, Section 13.3, Requirements: PRV-6, USG-4, USG-5, SEC-18.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Semaphore;

use llmroller_core::provider::ProviderKind;

/// Sanitized window representation safe for public UI and logging (SEC-18).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SanitizedWindow {
    pub kind: String,
    pub status: String,
    pub used_percent: f64,
    pub resets_at: Option<i64>,
    /// Remaining request count when the provider reports one.
    #[serde(default)]
    pub remaining_requests: Option<i64>,
    /// Remaining token count when the provider reports one.
    #[serde(default)]
    pub remaining_tokens: Option<i64>,
    /// Remaining monetary allowance in micro-USD when reported.
    #[serde(default)]
    pub remaining_micro_usd: Option<i64>,
    /// Seconds until reset when the provider reports a countdown.
    #[serde(default)]
    pub reset_in_sec: Option<i64>,
}

/// Allow-listed sanitized usage snapshot containing zero secret/PII fields (SEC-18).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SanitizedUsageSnapshot {
    pub status: String,
    pub plan_name: Option<String>,
    pub windows: Vec<SanitizedWindow>,
    pub balance_micro_usd: Option<i64>,
}

/// Per-key single-flight poller state.
#[derive(Debug, Default)]
pub struct KeyPollerState {
    pub poll_in_flight: AtomicBool,
    pub last_poll_started_ms: std::sync::atomic::AtomicI64,
}

impl KeyPollerState {
    pub fn try_start_poll(&self, now_ms: i64, is_manual: bool) -> bool {
        let min_interval = if is_manual { 10_000 } else { 30_000 };
        let last = self.last_poll_started_ms.load(Ordering::Acquire);
        if last > 0 && now_ms.saturating_sub(last) < min_interval {
            return false;
        }

        if self
            .poll_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.last_poll_started_ms.store(now_ms, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub fn finish_poll(&self) {
        self.poll_in_flight.store(false, Ordering::Release);
    }
}

/// Global usage poller managing the 8-permit concurrency semaphore.
#[derive(Debug)]
pub struct UsagePoller {
    pub semaphore: Arc<Semaphore>,
}

impl Default for UsagePoller {
    fn default() -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(8)),
        }
    }
}

fn parse_resets_at_value(
    value: Option<&serde_json::Value>,
    now_ms: i64,
    reset_in_sec: Option<i64>,
) -> Option<i64> {
    if let Some(value) = value {
        if let Some(ms) = value.as_i64() {
            return Some(ms);
        }
        if let Some(text) = value.as_str()
            && let Ok(parsed) =
                time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        {
            return Some(i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000).unwrap_or(0));
        }
    }
    reset_in_sec.map(|sec| now_ms + sec.saturating_mul(1000))
}

fn parse_window_obj(kind: &str, obj: &serde_json::Value, now_ms: i64) -> SanitizedWindow {
    let used_percent = obj
        .get("usedPercent")
        .or_else(|| obj.get("used_percent"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);
    let reset_in_sec = obj
        .get("resetInSec")
        .or_else(|| obj.get("reset_in_sec"))
        .and_then(serde_json::Value::as_i64);
    let resets_at = parse_resets_at_value(
        obj.get("resetsAt").or_else(|| obj.get("resets_at")),
        now_ms,
        reset_in_sec,
    );
    let reset_in_sec = reset_in_sec.or_else(|| resets_at.map(|at| ((at - now_ms).max(0)) / 1000));
    let status = obj
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || {
                if used_percent >= 100.0 {
                    "exhausted".to_string()
                } else if used_percent >= 80.0 {
                    "warning".to_string()
                } else {
                    "ok".to_string()
                }
            },
            str::to_string,
        );
    let remaining = |camel: &str, snake: &str| {
        obj.get(camel)
            .or_else(|| obj.get(snake))
            .and_then(serde_json::Value::as_i64)
    };
    SanitizedWindow {
        kind: kind.to_string(),
        status,
        used_percent,
        resets_at,
        remaining_requests: remaining("remainingRequests", "remaining_requests"),
        remaining_tokens: remaining("remainingTokens", "remaining_tokens"),
        remaining_micro_usd: remaining("remainingMicroUsd", "remaining_micro_usd"),
        reset_in_sec,
    }
}

fn usd_to_micro(value: f64) -> i64 {
    (value * 1_000_000.0).round() as i64
}

fn now_hint(raw_json: &serde_json::Value) -> i64 {
    raw_json
        .get("now_ms")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

/// Parses raw provider /usage JSON into SanitizedUsageSnapshot.
pub fn parse_raw_usage_to_sanitized(
    kind: ProviderKind,
    raw_json: &serde_json::Value,
) -> Option<SanitizedUsageSnapshot> {
    let now_ms = now_hint(raw_json);
    match kind {
        ProviderKind::OpencodeGo => {
            let plan = raw_json
                .get("plan")
                .or_else(|| raw_json.get("account").and_then(|a| a.get("plan")))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let mut windows = Vec::new();
            if let Some(usage) = raw_json.get("usage").and_then(serde_json::Value::as_object) {
                for (field, label) in [
                    ("rolling", "rolling_5h"),
                    ("weekly", "weekly"),
                    ("monthly", "monthly"),
                ] {
                    if let Some(window) = usage.get(field) {
                        windows.push(parse_window_obj(label, window, now_ms));
                    }
                }
            }
            if windows.is_empty()
                && let Some(arr) = raw_json.get("windows").and_then(|w| w.as_array())
            {
                for item in arr {
                    let label = item
                        .get("kind")
                        .and_then(|k| k.as_str())
                        .unwrap_or("window");
                    windows.push(parse_window_obj(label, item, now_ms));
                }
            }
            Some(SanitizedUsageSnapshot {
                status: "ok".to_string(),
                plan_name: plan,
                windows,
                balance_micro_usd: None,
            })
        }
        ProviderKind::CommandCode => {
            let plan = raw_json
                .get("plan")
                .or_else(|| raw_json.get("user").and_then(|u| u.get("tier")))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let mut windows = Vec::new();
            if let Some(window) = raw_json
                .get("rateLimits")
                .and_then(serde_json::Value::as_object)
                .and_then(|limits| limits.get("rolling5h"))
            {
                windows.push(parse_window_obj("rolling_5h", window, now_ms));
            }
            if windows.is_empty()
                && let Some(arr) = raw_json.get("windows").and_then(|w| w.as_array())
            {
                for item in arr {
                    let label = item
                        .get("kind")
                        .and_then(|k| k.as_str())
                        .unwrap_or("window");
                    windows.push(parse_window_obj(label, item, now_ms));
                }
            }
            let balance_micro_usd = raw_json.get("credits").and_then(|credits| {
                credits
                    .get("balance_micro_usd")
                    .and_then(serde_json::Value::as_i64)
                    .or_else(|| {
                        credits
                            .get("balanceUsd")
                            .and_then(serde_json::Value::as_f64)
                            .map(usd_to_micro)
                    })
            });
            let balance_micro_usd = balance_micro_usd.or_else(|| {
                raw_json
                    .get("credits_remaining")
                    .and_then(serde_json::Value::as_i64)
            });
            Some(SanitizedUsageSnapshot {
                status: "ok".to_string(),
                plan_name: plan,
                windows,
                balance_micro_usd,
            })
        }
        ProviderKind::Openrouter => {
            let data = raw_json.get("data")?;
            let usage_usd = data
                .get("usage")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            let limit_usd = data
                .get("limit")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(100.0);
            let used_pct = if limit_usd > 0.0 {
                (usage_usd / limit_usd) * 100.0
            } else {
                0.0
            };
            let balance = data
                .get("limit_remaining")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or_else(|| (limit_usd - usage_usd).max(0.0));
            let mut windows = vec![parse_window_obj(
                "monthly",
                &serde_json::json!({ "used_percent": used_pct }),
                now_ms,
            )];
            windows[0].remaining_micro_usd = Some(usd_to_micro(balance));
            Some(SanitizedUsageSnapshot {
                status: "ok".to_string(),
                plan_name: None,
                windows,
                balance_micro_usd: Some(usd_to_micro(balance)),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[doc = "Spec: USG-4, USG-5, SEC-18"]
    fn parses_authoritative_opencode_go_usage_schema() {
        let raw = serde_json::json!({
            "now_ms": 1_792_400_000_000_i64,
            "account": {"email": "secret@example.com", "plan": "pro"},
            "usage": {
                "rolling": {"usedPercent": 82.5, "resetInSec": 3600},
                "weekly": {"usedPercent": 40.0, "resetsAt": "2026-10-25T00:00:00Z"},
                "monthly": {"used_percent": 100.0, "resets_at": 1_793_000_000_000_i64}
            }
        });
        let parsed = parse_raw_usage_to_sanitized(ProviderKind::OpencodeGo, &raw).expect("parsed");
        assert_eq!(parsed.plan_name.as_deref(), Some("pro"));
        assert_eq!(parsed.windows.len(), 3);
        assert_eq!(parsed.windows[0].kind, "rolling_5h");
        assert_eq!(parsed.windows[0].status, "warning");
        assert_eq!(parsed.windows[0].resets_at, Some(1_792_403_600_000));
        assert_eq!(parsed.windows[1].kind, "weekly");
        assert_eq!(parsed.windows[1].resets_at, Some(1_792_886_400_000));
        assert_eq!(parsed.windows[2].status, "exhausted");
        let serialized = serde_json::to_string(&parsed).expect("json");
        assert!(!serialized.contains("secret@example.com"));
    }

    #[test]
    #[doc = "Spec: USG-4, USG-5, SEC-18"]
    fn parses_command_code_credits_and_rate_limits() {
        let raw = serde_json::json!({
            "now_ms": 1_792_400_000_000_i64,
            "user": {"email": "secret@example.com", "tier": "max"},
            "credits": {"balanceUsd": 12.5},
            "rateLimits": {
                "rolling5h": {"usedPercent": 25.0, "resetsAt": "2026-10-21T12:00:00Z"}
            }
        });
        let parsed = parse_raw_usage_to_sanitized(ProviderKind::CommandCode, &raw).expect("parsed");
        assert_eq!(parsed.balance_micro_usd, Some(12_500_000));
        assert_eq!(parsed.windows[0].kind, "rolling_5h");
        assert_eq!(parsed.windows[0].resets_at, Some(1_792_584_000_000));
        let serialized = serde_json::to_string(&parsed).expect("json");
        assert!(!serialized.contains("secret@example.com"));
    }

    #[test]
    #[doc = "Spec: USG-7"]
    fn parses_openrouter_limit_remaining() {
        let raw = serde_json::json!({
            "data": {"usage": 12.5, "limit": 100.0, "limit_remaining": 87.5}
        });
        let parsed = parse_raw_usage_to_sanitized(ProviderKind::Openrouter, &raw).expect("parsed");
        assert_eq!(parsed.balance_micro_usd, Some(87_500_000));
        assert_eq!(parsed.windows[0].kind, "monthly");
        assert!((parsed.windows[0].used_percent - 12.5).abs() < f64::EPSILON);
    }
}
