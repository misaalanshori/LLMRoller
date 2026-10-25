//! Core pricing engine, fallback rates, and binary-aware token estimator.
//!
//! Spec: Section 6.9, Section 9.3.2, Section 13.1, Requirements Partial: USG-3.

use serde::{Deserialize, Serialize};

/// Integer ceiling division for non-negative `i128` values.
pub const fn div_ceil_i128(a: i128, b: i128) -> i128 {
    if a <= 0 || b <= 0 { 0 } else { (a + b - 1) / b }
}

/// Integer ceiling division for non-negative `i64` values.
pub const fn div_ceil_i64(a: i64, b: i64) -> i64 {
    if a <= 0 || b <= 0 { 0 } else { (a + b - 1) / b }
}

/// Token usage broken down into cache and uncached categories.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_uncached: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    /// Diagnostic subset of `output` that the provider attributed to reasoning/thinking.
    pub reasoning: i64,
}

impl TokenUsage {
    pub const fn total_input(&self) -> i64 {
        self.input_uncached + self.cache_read + self.cache_write
    }
}

/// Micro-USD rates per 1,000,000 tokens (MTok).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rates {
    pub input_per_mtok_micro_usd: i64,
    pub output_per_mtok_micro_usd: i64,
    pub cache_read_per_mtok_micro_usd: i64,
    pub cache_write_per_mtok_micro_usd: i64,
}

impl Default for Rates {
    fn default() -> Self {
        // Unpriced fallback rates per Section 9.3.3: $3 / $15 / $0.30 / $3.75 per MTok
        Self {
            input_per_mtok_micro_usd: 3_000_000,
            output_per_mtok_micro_usd: 15_000_000,
            cache_read_per_mtok_micro_usd: 300_000,
            cache_write_per_mtok_micro_usd: 3_750_000,
        }
    }
}

impl From<&crate::settings::UnpricedFallbackRates> for Rates {
    fn from(f: &crate::settings::UnpricedFallbackRates) -> Self {
        Self {
            input_per_mtok_micro_usd: f.input,
            output_per_mtok_micro_usd: f.output,
            cache_read_per_mtok_micro_usd: f.cache_read,
            cache_write_per_mtok_micro_usd: f.cache_write,
        }
    }
}

/// Context-length pricing tier from `prices.tiers_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextTier {
    pub min_total_input_tokens: i64,
    pub max_total_input_tokens: Option<i64>,
    pub input_per_mtok_micro_usd: i64,
    pub output_per_mtok_micro_usd: i64,
    pub cache_read_per_mtok_micro_usd: i64,
    pub cache_write_per_mtok_micro_usd: i64,
}

impl ContextTier {
    pub fn rates(&self) -> Rates {
        Rates {
            input_per_mtok_micro_usd: self.input_per_mtok_micro_usd,
            output_per_mtok_micro_usd: self.output_per_mtok_micro_usd,
            cache_read_per_mtok_micro_usd: self.cache_read_per_mtok_micro_usd,
            cache_write_per_mtok_micro_usd: self.cache_write_per_mtok_micro_usd,
        }
    }

    pub fn matches(&self, total_input_tokens: i64) -> bool {
        total_input_tokens >= self.min_total_input_tokens
            && self
                .max_total_input_tokens
                .is_none_or(|max| total_input_tokens <= max)
    }
}

/// Validates that context tiers do not overlap and have valid bounds.
pub fn validate_tiers(tiers: &[ContextTier]) -> Result<(), String> {
    for (i, a) in tiers.iter().enumerate() {
        if a.min_total_input_tokens < 0 {
            return Err(format!("Tier {i} has negative min_total_input_tokens"));
        }
        if a.input_per_mtok_micro_usd < 0
            || a.output_per_mtok_micro_usd < 0
            || a.cache_read_per_mtok_micro_usd < 0
            || a.cache_write_per_mtok_micro_usd < 0
        {
            return Err(format!("Tier {i} has negative rate"));
        }
        if let Some(max_a) = a.max_total_input_tokens
            && max_a < a.min_total_input_tokens
        {
            return Err(format!(
                "Tier {i} has max_total_input_tokens < min_total_input_tokens"
            ));
        }
        for (j, b) in tiers.iter().enumerate().skip(i + 1) {
            let max_a = a.max_total_input_tokens.unwrap_or(i64::MAX);
            let max_b = b.max_total_input_tokens.unwrap_or(i64::MAX);
            let start = a.min_total_input_tokens.max(b.min_total_input_tokens);
            let end = max_a.min(max_b);
            if start <= end {
                return Err(format!("Context tiers {i} and {j} overlap"));
            }
        }
    }
    Ok(())
}

/// Time-of-day schedule from `prices.schedule_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub name: String,
    pub iso_weekdays: Vec<u8>,            // 1 = Monday, ..., 7 = Sunday
    pub utc_minute_ranges: Vec<[u16; 2]>, // [start, end) half-open
    pub multiplier_num: i64,
    pub multiplier_den: i64,
}

impl Schedule {
    pub fn matches(&self, ts_epoch_millis: i64) -> bool {
        if self.multiplier_den <= 0 || self.multiplier_num < 0 {
            return false;
        }
        let secs = ts_epoch_millis / 1000;
        let day_index = secs.div_euclid(86400);
        // 1970-01-01 was Thursday (ISO weekday 4)
        let iso_weekday = (((day_index + 3) % 7) + 1) as u8;
        if !self.iso_weekdays.contains(&iso_weekday) {
            return false;
        }
        let minute_of_day = (secs.rem_euclid(86400) / 60) as u16;
        for range in &self.utc_minute_ranges {
            if minute_of_day >= range[0] && minute_of_day < range[1] {
                return true;
            }
        }
        false
    }
}

/// Validates that schedules have valid bounds and do not overlap.
pub fn validate_schedules(schedules: &[Schedule]) -> Result<(), String> {
    for (i, a) in schedules.iter().enumerate() {
        if a.multiplier_den <= 0 {
            return Err(format!(
                "Schedule '{name}' has non-positive multiplier_den",
                name = a.name
            ));
        }
        if a.multiplier_num < 0 {
            return Err(format!(
                "Schedule '{name}' has negative multiplier_num",
                name = a.name
            ));
        }
        for day in &a.iso_weekdays {
            if !(1..=7).contains(day) {
                return Err(format!(
                    "Schedule '{name}' has invalid ISO weekday {day}",
                    name = a.name
                ));
            }
        }
        for range in &a.utc_minute_ranges {
            if range[0] >= range[1] || range[1] > 1440 {
                return Err(format!(
                    "Schedule '{name}' has invalid minute range {range:?}",
                    name = a.name
                ));
            }
        }
        for (_j, b) in schedules.iter().enumerate().skip(i + 1) {
            let common_days = a.iso_weekdays.iter().any(|d| b.iso_weekdays.contains(d));
            if common_days {
                for r_a in &a.utc_minute_ranges {
                    for r_b in &b.utc_minute_ranges {
                        let start = r_a[0].max(r_b[0]);
                        let end = r_a[1].min(r_b[1]);
                        if start < end {
                            return Err(format!(
                                "Schedules '{name_a}' and '{name_b}' have overlapping time ranges",
                                name_a = a.name,
                                name_b = b.name
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Source of the request cost determination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    Provider,
    PriceBook,
    FallbackRate,
    EstimatedIncomplete,
}

impl CostSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::PriceBook => "price_book",
            Self::FallbackRate => "fallback_rate",
            Self::EstimatedIncomplete => "estimated_incomplete",
        }
    }
}

/// Parses provider-reported usage.cost if valid and finite in `[0.0, 1000.0]`.
pub fn parse_provider_cost(cost: Option<f64>) -> Option<i64> {
    match cost {
        Some(c) if c.is_finite() && (0.0..=1000.0).contains(&c) => {
            Some((c * 1_000_000.0).round() as i64)
        }
        _ => None,
    }
}

/// Computes integer micro-USD cost with 128-bit ceiling division.
pub fn calculate_cost_micro_usd(
    tokens: TokenUsage,
    rates: Rates,
    multiplier_num: i64,
    multiplier_den: i64,
) -> i64 {
    let subtotal = (tokens.input_uncached as i128) * (rates.input_per_mtok_micro_usd as i128)
        + (tokens.cache_read as i128) * (rates.cache_read_per_mtok_micro_usd as i128)
        + (tokens.cache_write as i128) * (rates.cache_write_per_mtok_micro_usd as i128)
        + (tokens.output as i128) * (rates.output_per_mtok_micro_usd as i128);

    if subtotal <= 0 {
        return 0;
    }

    let num = multiplier_num.max(1) as i128;
    let den = multiplier_den.max(1) as i128;
    let numerator = subtotal * num;
    let denominator = 1_000_000_i128 * den;

    div_ceil_i128(numerator, denominator) as i64
}

/// In-memory price entry for a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceEntry {
    pub provider_id: String,
    pub provider_kind: String,
    pub upstream_model_id: String,
    pub base_rates: Rates,
    pub tiers: Vec<ContextTier>,
    pub schedules: Vec<Schedule>,
}

/// Result of pricing computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostResult {
    pub cost_micro_usd: i64,
    pub cost_source: CostSource,
}

/// Pure cost calculation resolving provider cost, price book context tiers/schedules, or fallback rates.
pub fn compute_request_cost(
    provider_cost: Option<f64>,
    tokens: TokenUsage,
    price_entry: Option<&PriceEntry>,
    fallback_rates: Rates,
    ts_epoch_millis: i64,
) -> CostResult {
    if let Some(cost_micro_usd) = parse_provider_cost(provider_cost) {
        return CostResult {
            cost_micro_usd,
            cost_source: CostSource::Provider,
        };
    }

    if let Some(entry) = price_entry {
        let total_input = tokens.total_input();
        let rates = entry
            .tiers
            .iter()
            .find(|t| t.matches(total_input))
            .map(|t| t.rates())
            .unwrap_or(entry.base_rates);

        let (num, den) = entry
            .schedules
            .iter()
            .find(|s| s.matches(ts_epoch_millis))
            .map(|s| (s.multiplier_num, s.multiplier_den))
            .unwrap_or((1, 1));

        let cost_micro_usd = calculate_cost_micro_usd(tokens, rates, num, den);
        CostResult {
            cost_micro_usd,
            cost_source: CostSource::PriceBook,
        }
    } else {
        let cost_micro_usd = calculate_cost_micro_usd(tokens, fallback_rates, 1, 1);
        CostResult {
            cost_micro_usd,
            cost_source: CostSource::FallbackRate,
        }
    }
}

/// Applies a provider's subscription coverage to a list-price cost (`USG-10`).
///
/// Only `subscription_plan` coverage scales the effective cost by
/// `plan_multiplier_bps` (10_000 = 1.0x, 5_000 = 50%); `pay_as_you_go`,
/// `shared_grant`, and `free_tier` always charge the unadjusted list price.
#[must_use]
pub fn apply_plan_multiplier(
    list_cost_micro_usd: i64,
    plan_multiplier_bps: i64,
    billing_coverage_mode: &str,
) -> i64 {
    if billing_coverage_mode == "free_tier" && plan_multiplier_bps == 0 {
        return 0;
    }
    if billing_coverage_mode != "subscription_plan" {
        return list_cost_micro_usd;
    }
    let bps = plan_multiplier_bps.clamp(0, 100_000);
    if list_cost_micro_usd <= 0 || bps <= 0 {
        return 0;
    }
    let scaled = div_ceil_i128(i128::from(list_cost_micro_usd) * i128::from(bps), 10_000);
    i64::try_from(scaled).unwrap_or(i64::MAX)
}

/// Computes the effective upstream key/provider cost in micro-USD, returning `0` when
/// the upstream key or model is marked `free_tier` (`:free` suffix or `billing_coverage_mode == "free_tier"` with `free_tier = true`, `F035`).
#[must_use]
pub fn apply_effective_upstream_cost(
    list_cost_micro_usd: i64,
    plan_multiplier_bps: i64,
    billing_coverage_mode: &str,
    is_free_tier_key: bool,
) -> i64 {
    if is_free_tier_key || billing_coverage_mode == "free_tier" {
        return 0;
    }
    apply_plan_multiplier(
        list_cost_micro_usd,
        plan_multiplier_bps,
        billing_coverage_mode,
    )
}

/// Streaming binary-aware JSON string tokenizer counting text vs binary blocks.
///
/// Walks JSON string values without building a DOM:
/// If a string is the value of a key in {"data", "image_url", "file_data", "b64_json"}
/// (or "url" whose value starts with "data:") and its byte length > 1_024:
///     binary_blocks += 1;
/// else:
///     text_bytes += decoded_utf8_byte_len;
/// div_ceil(text_bytes, 4) + binary_blocks * 1_600
pub fn estimate_input_tokens(body: &[u8]) -> i64 {
    let mut text_bytes: i64 = 0;
    let mut binary_blocks: i64 = 0;

    let len = body.len();
    let mut i = 0;
    let mut current_key: Option<&[u8]> = None;
    let mut expecting_value = false;

    while i < len {
        let b = body[i];
        match b {
            b'"' => {
                i += 1;
                let start = i;
                let mut is_escaped = false;
                let mut decoded_len = 0;
                while i < len {
                    let c = body[i];
                    if is_escaped {
                        is_escaped = false;
                        decoded_len += 1;
                        i += 1;
                    } else if c == b'\\' {
                        is_escaped = true;
                        i += 1;
                    } else if c == b'"' {
                        break;
                    } else {
                        decoded_len += 1;
                        i += 1;
                    }
                }
                let raw_str = &body[start..i];
                if i < len && body[i] == b'"' {
                    i += 1;
                }

                if expecting_value {
                    expecting_value = false;
                    let is_binary_key = match current_key {
                        Some(k) => matches!(k, b"data" | b"image_url" | b"file_data" | b"b64_json"),
                        None => false,
                    };
                    let is_url_data = match current_key {
                        Some(k) if k == b"url" => raw_str.starts_with(b"data:"),
                        _ => false,
                    };

                    let is_data_uri = raw_str.starts_with(b"data:");
                    let is_large_inline_base64 =
                        raw_str.len() > 16_384 && is_mostly_base64(raw_str);
                    if ((is_url_data
                        || is_data_uri
                        || (is_binary_key && is_mostly_base64(raw_str)))
                        && raw_str.len() > 1024)
                        || is_large_inline_base64
                    {
                        binary_blocks += 1;
                    } else {
                        text_bytes += decoded_len as i64;
                    }
                    current_key = None;
                } else {
                    let mut j = i;
                    while j < len && body[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < len && body[j] == b':' {
                        current_key = Some(raw_str);
                        expecting_value = true;
                        i = j + 1;
                    } else {
                        text_bytes += decoded_len as i64;
                    }
                }
            }
            b'{' | b'[' => {
                expecting_value = false;
                i += 1;
            }
            b',' => {
                current_key = None;
                expecting_value = false;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    div_ceil_i64(text_bytes, 4) + binary_blocks * 1600
}

fn is_mostly_base64(raw_str: &[u8]) -> bool {
    let payload = if let Some(idx) = raw_str.windows(7).position(|w| w == b"base64,") {
        &raw_str[idx + 7..]
    } else {
        raw_str
    };
    let sample_len = payload.len().min(512);
    sample_len > 0
        && payload[..sample_len].iter().all(|&b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_' | b'\r' | b'\n')
        })
}

#[cfg(test)]
pub mod proptests;
