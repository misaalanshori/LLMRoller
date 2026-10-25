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
/// Active key polling interval (`300 s` = `300,000 ms`, SRDD Section 13.3).
pub const ACTIVE_KEY_POLL_INTERVAL_MS: i64 = 300_000;
/// Idle key polling interval (`3600 s` = `3,600,000 ms`, SRDD Section 13.3).
pub const IDLE_KEY_POLL_INTERVAL_MS: i64 = 3_600_000;

#[derive(Debug, Default)]
pub struct KeyPollerState {
    pub poll_in_flight: AtomicBool,
    pub last_poll_started_ms: std::sync::atomic::AtomicI64,
    pub next_scheduled_poll_ms: std::sync::atomic::AtomicI64,
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

    /// Evaluate scheduled background poll eligibility honoring active (`300s`) vs idle (`3600s`)
    /// cadence, `30s` minimum interval, and `±20%` (`8000..=12000` bps) jitter (`USG-6`, `F-071`).
    pub fn try_start_scheduled_poll(
        &self,
        now_ms: i64,
        is_active_key: bool,
        jitter_bps: i64,
    ) -> bool {
        let last = self.last_poll_started_ms.load(Ordering::Acquire);
        if last > 0 && now_ms.saturating_sub(last) < 30_000 {
            return false;
        }
        let next_due = self.next_scheduled_poll_ms.load(Ordering::Acquire);
        let base_interval = if is_active_key {
            ACTIVE_KEY_POLL_INTERVAL_MS
        } else {
            IDLE_KEY_POLL_INTERVAL_MS
        };
        let clamped_jitter = jitter_bps.clamp(8_000, 12_000);
        let jittered_interval = ((base_interval * clamped_jitter) / 10_000).max(30_000);

        // If a key becomes active while waiting on an idle schedule, allow it after the active interval.
        let effective_due = if last == 0 {
            0
        } else if is_active_key {
            next_due.min(last + jittered_interval)
        } else {
            next_due.max(last + jittered_interval)
        };
        if now_ms < effective_due {
            return false;
        }

        if self
            .poll_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.last_poll_started_ms.store(now_ms, Ordering::Release);
            self.next_scheduled_poll_ms
                .store(now_ms + jittered_interval, Ordering::Release);
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

/// Normalize provider window labels to the canonical `upstream_window_kind`
/// values enforced by `budget_windows` (`5h`, `7d`, `1mo`).
#[must_use]
pub fn canonical_window_kind(kind: &str) -> &str {
    match kind.trim().to_ascii_lowercase().as_str() {
        "5h" | "rolling" | "rolling5h" | "rolling_5h" | "rolling-5h" => "5h",
        "7d" | "weekly" | "week" | "1w" => "7d",
        "1mo" | "monthly" | "month" | "30d" => "1mo",
        _ => kind.trim(),
    }
}

fn parse_window_obj(kind: &str, obj: &serde_json::Value, now_ms: i64) -> SanitizedWindow {
    let used_percent = obj
        .get("usedPercent")
        .or_else(|| obj.get("used_percent"))
        .or_else(|| obj.get("percent"))
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
        kind: canonical_window_kind(kind).to_string(),
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

/// Detailed OpenRouter `/api/v1/key` usage breakdown (`F098`, `USG-7`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenrouterKeyUsage {
    pub label: Option<String>,
    pub usage_usd: f64,
    pub usage_monthly_usd: f64,
    pub limit_usd: Option<f64>,
    pub limit_remaining_usd: Option<f64>,
    pub is_free_tier: bool,
}

/// Parse an OpenRouter `/api/v1/key` response even when `limit` is `null` (`F098`).
#[must_use]
pub fn parse_openrouter_key_usage(raw_json: &serde_json::Value) -> Option<OpenrouterKeyUsage> {
    let data = raw_json.get("data")?;
    let label = data
        .get("label")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let usage_usd = data
        .get("usage")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let usage_monthly_usd = data
        .get("usage_monthly")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(usage_usd);
    let limit_usd = data
        .get("limit")
        .and_then(serde_json::Value::as_f64)
        .or_else(|| {
            data.pointer("/effective_budget/limit_usd")
                .and_then(serde_json::Value::as_f64)
        })
        .filter(|&v| v.is_finite() && v > 0.0);
    let limit_remaining_usd = data
        .get("limit_remaining")
        .and_then(serde_json::Value::as_f64)
        .or_else(|| {
            data.pointer("/effective_budget/remaining_usd")
                .and_then(serde_json::Value::as_f64)
        })
        .filter(|v| v.is_finite());
    let is_free_tier = data
        .get("is_free_tier")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Some(OpenrouterKeyUsage {
        label,
        usage_usd,
        usage_monthly_usd,
        limit_usd,
        limit_remaining_usd,
        is_free_tier,
    })
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
                for (field, label) in [("rolling", "5h"), ("weekly", "7d"), ("monthly", "1mo")] {
                    if let Some(window) = usage.get(field) {
                        windows.push(parse_window_obj(label, window, now_ms));
                    }
                }
            }
            // F179: Codex /wham/usage rate_limit.primary_window & secondary_window
            if let Some(rl) = raw_json
                .get("rate_limit")
                .and_then(serde_json::Value::as_object)
            {
                for (field, label) in [("primary_window", "5h"), ("secondary_window", "7d")] {
                    if let Some(w) = rl.get(field) {
                        windows.push(parse_window_obj(label, w, now_ms));
                    }
                }
            }
            // F179: Claude OAuth /api/oauth/usage five_hour & seven_day
            for (field, label) in [("five_hour", "5h"), ("seven_day", "7d")] {
                if let Some(w) = raw_json.get(field) {
                    let pct = w
                        .get("utilization")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0);
                    let mut obj = w.clone();
                    if let Some(map) = obj.as_object_mut() {
                        map.entry("used_percent".to_string())
                            .or_insert_with(|| serde_json::json!(pct));
                    }
                    windows.push(parse_window_obj(label, &obj, now_ms));
                }
            }
            // F179: Antigravity / Gemini CLI retrieveUserQuota buckets
            if let Some(buckets) = raw_json
                .get("buckets")
                .and_then(serde_json::Value::as_array)
                && let Some(first) = buckets.first()
            {
                let rem_frac = first
                    .get("remainingFraction")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(1.0);
                let used_pct = ((1.0 - rem_frac) * 100.0).clamp(0.0, 100.0);
                windows.push(parse_window_obj(
                    "5h",
                    &serde_json::json!({
                        "used_percent": used_pct,
                        "resetsAt": first.get("resetTime"),
                    }),
                    now_ms,
                ));
            }
            // F179: DeepSeek /user/balance balance_infos
            let ds_balance = raw_json
                .get("balance_infos")
                .and_then(serde_json::Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(|b| b.get("total_balance"))
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse::<f64>().ok())
                        .or_else(|| v.as_f64())
                })
                .map(usd_to_micro);
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
                balance_micro_usd: ds_balance,
            })
        }
        ProviderKind::CommandCode => {
            let plan = raw_json
                .get("plan")
                .or_else(|| raw_json.get("user").and_then(|u| u.get("tier")))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let mut windows = Vec::new();
            if let Some(limits) = raw_json
                .get("rateLimits")
                .and_then(serde_json::Value::as_object)
            {
                for (field, label) in [("rolling5h", "5h"), ("weekly", "7d"), ("monthly", "1mo")] {
                    if let Some(window) = limits.get(field) {
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
            let usage_monthly_usd = data
                .get("usage_monthly")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(usage_usd);
            let limit_usd = data
                .get("limit")
                .and_then(serde_json::Value::as_f64)
                .or_else(|| {
                    data.pointer("/effective_budget/limit_usd")
                        .and_then(serde_json::Value::as_f64)
                })
                .filter(|&v| v.is_finite() && v > 0.0);
            let limit_remaining = data
                .get("limit_remaining")
                .and_then(serde_json::Value::as_f64)
                .or_else(|| {
                    data.pointer("/effective_budget/remaining_usd")
                        .and_then(serde_json::Value::as_f64)
                })
                .filter(|v| v.is_finite());

            if let Some(limit) = limit_usd {
                let used_pct = ((usage_monthly_usd / limit) * 100.0).clamp(0.0, 100.0);
                let balance =
                    limit_remaining.unwrap_or_else(|| (limit - usage_monthly_usd).max(0.0));
                let mut windows = vec![parse_window_obj(
                    "1mo",
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
            } else {
                // F-012: Unlimited OpenRouter keys (`"limit": null`) must not synthesize a $100 cap
                Some(SanitizedUsageSnapshot {
                    status: "ok".to_string(),
                    plan_name: None,
                    windows: Vec::new(),
                    balance_micro_usd: limit_remaining.map(usd_to_micro),
                })
            }
        }
        ProviderKind::OpenDesignAmr | ProviderKind::Custom => {
            if raw_json
                .get("data")
                .and_then(|d| d.get("totalBalance"))
                .is_some()
            {
                let bal = raw_json
                    .pointer("/data/totalBalance")
                    .and_then(|v| {
                        v.as_str()
                            .and_then(|s| s.parse::<f64>().ok())
                            .or_else(|| v.as_f64())
                    })
                    .map(usd_to_micro);
                return Some(SanitizedUsageSnapshot {
                    status: "ok".to_string(),
                    plan_name: raw_json
                        .pointer("/data/role")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    windows: Vec::new(),
                    balance_micro_usd: bal,
                });
            }
            if raw_json.get("data").is_some() {
                return parse_raw_usage_to_sanitized(ProviderKind::Openrouter, raw_json);
            }
            let plan = raw_json
                .get("plan")
                .or_else(|| raw_json.get("plan_name"))
                .or_else(|| raw_json.get("account").and_then(|a| a.get("plan")))
                .or_else(|| raw_json.get("user").and_then(|u| u.get("tier")))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let mut windows = Vec::new();
            if let Some(usage) = raw_json.get("usage").and_then(serde_json::Value::as_object) {
                for (field, label) in [
                    ("rolling", "5h"),
                    ("5h", "5h"),
                    ("weekly", "7d"),
                    ("7d", "7d"),
                    ("monthly", "1mo"),
                    ("1mo", "1mo"),
                ] {
                    if let Some(window) = usage.get(field) {
                        windows.push(parse_window_obj(label, window, now_ms));
                    }
                }
            }
            if let Some(limits) = raw_json
                .get("rateLimits")
                .and_then(serde_json::Value::as_object)
            {
                for (field, label) in [("rolling5h", "5h"), ("weekly", "7d"), ("monthly", "1mo")] {
                    if let Some(window) = limits.get(field) {
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
            let ds_balance = raw_json
                .get("balance_infos")
                .and_then(serde_json::Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(|b| b.get("total_balance"))
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse::<f64>().ok())
                        .or_else(|| v.as_f64())
                })
                .map(usd_to_micro);
            let balance_micro_usd = ds_balance.or_else(|| {
                raw_json
                    .get("balance_micro_usd")
                    .and_then(serde_json::Value::as_i64)
                    .or_else(|| {
                        raw_json
                            .get("balance_usd")
                            .or_else(|| raw_json.get("balanceUsd"))
                            .and_then(serde_json::Value::as_f64)
                            .map(usd_to_micro)
                    })
                    .or_else(|| {
                        raw_json.get("credits").and_then(|credits| {
                            credits
                                .get("balance_micro_usd")
                                .and_then(serde_json::Value::as_i64)
                                .or_else(|| {
                                    credits
                                        .get("balanceUsd")
                                        .and_then(serde_json::Value::as_f64)
                                        .map(usd_to_micro)
                                })
                        })
                    })
            });
            if plan.is_none() && windows.is_empty() && balance_micro_usd.is_none() {
                None
            } else {
                Some(SanitizedUsageSnapshot {
                    status: "ok".to_string(),
                    plan_name: plan,
                    windows,
                    balance_micro_usd,
                })
            }
        }
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
        assert_eq!(parsed.windows[0].kind, "5h");
        assert_eq!(parsed.windows[0].used_percent, 82.5);
        assert_eq!(parsed.windows[0].status, "warning");
        assert_eq!(parsed.windows[0].resets_at, Some(1_792_403_600_000));
        assert_eq!(parsed.windows[1].kind, "7d");
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
        assert_eq!(parsed.windows[0].kind, "5h");
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
        assert_eq!(parsed.windows[0].kind, "1mo");
        assert!((parsed.windows[0].used_percent - 12.5).abs() < f64::EPSILON);
    }

    #[test]
    #[doc = "Spec: USG-4, USG-5 — F10 canonical window kinds"]
    fn normalizes_legacy_window_labels_to_schema_values() {
        assert_eq!(canonical_window_kind("rolling_5h"), "5h");
        assert_eq!(canonical_window_kind("weekly"), "7d");
        assert_eq!(canonical_window_kind("monthly"), "1mo");
        assert_eq!(canonical_window_kind("5h"), "5h");

        // The `windows[]` fallback must not leak provider-native labels into the
        // schema-constrained `upstream_window_kind` column.
        let raw = serde_json::json!({
            "windows": [
                {"kind": "rolling_5h", "percent": 33.0},
                {"kind": "Month", "usedPercent": 5.0}
            ]
        });
        let parsed = parse_raw_usage_to_sanitized(ProviderKind::OpencodeGo, &raw).expect("parsed");
        assert_eq!(parsed.windows[0].kind, "5h");
        assert_eq!(parsed.windows[0].used_percent, 33.0);
        assert_eq!(parsed.windows[1].kind, "1mo");
    }

    #[test]
    #[doc = "Spec: USG-6, USG-7, PRV-1"]
    fn openrouter_null_limit_does_not_synthesize_fake_100_dollar_cap() {
        let raw = serde_json::json!({
            "data": {
                "usage": 142.75,
                "usage_daily": 12.0,
                "usage_weekly": 45.0,
                "usage_monthly": 142.75,
                "limit": null,
                "limit_remaining": null,
                "is_free_tier": false
            }
        });
        let parsed = parse_raw_usage_to_sanitized(ProviderKind::Openrouter, &raw).expect("parsed");
        assert!(
            parsed.windows.is_empty(),
            "unlimited OpenRouter key must not fabricate a 1mo window"
        );
        assert_eq!(parsed.balance_micro_usd, None);
    }

    #[test]
    #[doc = "Spec: USG-4, USG-5, USG-6, PRV-1"]
    fn command_code_weekly_monthly_and_open_design_amr_windows_parse() {
        let cc_raw = serde_json::json!({
            "now_ms": 1_792_400_000_000_i64,
            "rateLimits": {
                "rolling5h": {"usedPercent": 20.0},
                "weekly": {"usedPercent": 55.0},
                "monthly": {"usedPercent": 80.0}
            }
        });
        let cc = parse_raw_usage_to_sanitized(ProviderKind::CommandCode, &cc_raw).expect("cc");
        assert_eq!(cc.windows.len(), 3);
        assert_eq!(cc.windows[1].kind, "7d");
        assert_eq!(cc.windows[2].kind, "1mo");

        let amr_raw = serde_json::json!({
            "plan": "Design Plan",
            "usage": {
                "rolling": {"usedPercent": 15.0},
                "weekly": {"usedPercent": 35.0}
            },
            "balance_usd": 42.5
        });
        let amr = parse_raw_usage_to_sanitized(ProviderKind::OpenDesignAmr, &amr_raw).expect("amr");
        assert_eq!(amr.plan_name.as_deref(), Some("Design Plan"));
        assert_eq!(amr.windows.len(), 2);
        assert_eq!(amr.balance_micro_usd, Some(42_500_000));
    }

    #[test]
    #[doc = "Spec: PRV-2, USG-7, F098 - OpenRouter attribution headers, /key usage extraction with null limit, and prompt cache injection"]
    fn regression_test_f098_openrouter_preset_missing_attribution_he() {
        let toml = ProviderKind::Openrouter.preset_toml();
        assert!(toml.contains("\"HTTP-Referer\""));
        assert!(toml.contains("\"X-Title\" = \"LLMRoller\""));

        let raw = serde_json::json!({
            "data": {
                "label": "sk-or-v1-prepaid",
                "usage": 4.25,
                "limit": null,
                "limit_remaining": 45.75,
                "is_free_tier": false
            }
        });
        let detailed = parse_openrouter_key_usage(&raw).expect("detailed openrouter usage");
        assert_eq!(detailed.usage_usd, 4.25);
        assert_eq!(detailed.limit_usd, None);
        assert_eq!(detailed.limit_remaining_usd, Some(45.75));
        assert!(!detailed.is_free_tier);

        let sanitized =
            parse_raw_usage_to_sanitized(ProviderKind::Openrouter, &raw).expect("sanitized");
        assert_eq!(sanitized.balance_micro_usd, Some(45_750_000));
    }
}
