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

/// Parses raw provider /usage JSON into SanitizedUsageSnapshot.
pub fn parse_raw_usage_to_sanitized(
    kind: ProviderKind,
    raw_json: &serde_json::Value,
) -> Option<SanitizedUsageSnapshot> {
    match kind {
        ProviderKind::OpencodeGo => {
            let plan = raw_json
                .get("plan")
                .and_then(|p| p.as_str())
                .map(str::to_string);
            let mut windows = Vec::new();
            if let Some(arr) = raw_json.get("windows").and_then(|w| w.as_array()) {
                for item in arr {
                    let w_kind = item
                        .get("kind")
                        .and_then(|k| k.as_str())
                        .unwrap_or("")
                        .to_string();
                    let status = item
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("ok")
                        .to_string();
                    let used_percent = item
                        .get("used_percent")
                        .and_then(|u| u.as_f64())
                        .unwrap_or(0.0);
                    let resets_at = item.get("resets_at").and_then(|r| r.as_i64());
                    windows.push(SanitizedWindow {
                        kind: w_kind,
                        status,
                        used_percent,
                        resets_at,
                    });
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
                .and_then(|p| p.as_str())
                .map(str::to_string);
            let credits = raw_json.get("credits_remaining").and_then(|c| c.as_i64());
            let mut windows = Vec::new();
            if let Some(arr) = raw_json.get("windows").and_then(|w| w.as_array()) {
                for item in arr {
                    let w_kind = item
                        .get("kind")
                        .and_then(|k| k.as_str())
                        .unwrap_or("")
                        .to_string();
                    let status = item
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("ok")
                        .to_string();
                    let used_percent = item
                        .get("used_percent")
                        .and_then(|u| u.as_f64())
                        .unwrap_or(0.0);
                    let resets_at = item.get("resets_at").and_then(|r| r.as_i64());
                    windows.push(SanitizedWindow {
                        kind: w_kind,
                        status,
                        used_percent,
                        resets_at,
                    });
                }
            }
            Some(SanitizedUsageSnapshot {
                status: "ok".to_string(),
                plan_name: plan,
                windows,
                balance_micro_usd: credits,
            })
        }
        ProviderKind::Openrouter => {
            let data = raw_json.get("data")?;
            let usage_usd = data.get("usage").and_then(|u| u.as_f64()).unwrap_or(0.0);
            let limit_usd = data.get("limit").and_then(|l| l.as_f64()).unwrap_or(100.0);
            let used_pct = if limit_usd > 0.0 {
                (usage_usd / limit_usd) * 100.0
            } else {
                0.0
            };
            let balance = (limit_usd - usage_usd).max(0.0);
            let balance_micro = (balance * 1_000_000.0).round() as i64;
            Some(SanitizedUsageSnapshot {
                status: "ok".to_string(),
                plan_name: None,
                windows: vec![SanitizedWindow {
                    kind: "1mo".to_string(),
                    status: "ok".to_string(),
                    used_percent: used_pct,
                    resets_at: None,
                }],
                balance_micro_usd: Some(balance_micro),
            })
        }
        _ => None,
    }
}
