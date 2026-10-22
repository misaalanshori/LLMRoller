//! Non-streaming JSON response token and cost extraction across protocols.
//!
//! Spec: Section 12.5, Requirements: USG-2, USG-3.

use crate::adapters::EndpointKind;
use llmroller_core::pricing::{TokenUsage, parse_provider_cost};

/// Normalized token usage and cost extracted from an upstream response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractedUsage {
    pub input_uncached_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub provider_cost_micro_usd: Option<i64>,
}

impl ExtractedUsage {
    pub fn to_token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_uncached: self.input_uncached_tokens,
            cache_read: self.cache_read_tokens,
            cache_write: self.cache_write_tokens,
            output: self.output_tokens,
        }
    }
}

/// Extracts normalized token usage from non-streaming JSON responses.
pub fn extract_json_usage(
    endpoint: EndpointKind,
    json: &serde_json::Value,
) -> Option<ExtractedUsage> {
    match endpoint {
        EndpointKind::Chat => extract_chat_json_usage(json),
        EndpointKind::Responses => extract_responses_json_usage(json),
        EndpointKind::Messages => extract_messages_json_usage(json),
        _ => None,
    }
}

pub fn extract_chat_json_usage(json: &serde_json::Value) -> Option<ExtractedUsage> {
    let usage = json.get("usage")?;
    if !usage.is_object() {
        return None;
    }
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let prompt_details = usage.get("prompt_tokens_details");
    let cached_tokens = prompt_details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
        .or_else(|| {
            usage
                .get("prompt_cache_hit_tokens")
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0);

    let cache_write_tokens = prompt_details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let input_uncached_tokens = if let Some(miss) = usage
        .get("prompt_cache_miss_tokens")
        .and_then(|v| v.as_i64())
    {
        miss
    } else {
        (prompt_tokens - cached_tokens - cache_write_tokens).max(0)
    };

    let comp_details = usage.get("completion_tokens_details");
    let reasoning_tokens = comp_details
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let cost_f64 = usage.get("cost").and_then(|v| v.as_f64());
    let provider_cost_micro_usd = parse_provider_cost(cost_f64);

    Some(ExtractedUsage {
        input_uncached_tokens,
        cache_read_tokens: cached_tokens,
        cache_write_tokens,
        output_tokens: completion_tokens,
        reasoning_tokens,
        provider_cost_micro_usd,
    })
}

pub fn extract_responses_json_usage(json: &serde_json::Value) -> Option<ExtractedUsage> {
    // Non-streaming responses expose top-level `usage`; streaming terminal events
    // (`response.completed`, `response.incomplete`, `response.failed`) nest it
    // under `response.usage` (Section 2.5, Section 12.5).
    let usage = json.get("usage").or_else(|| {
        json.get("response")
            .and_then(|response| response.get("usage"))
    })?;
    if !usage.is_object() {
        return None;
    }
    let input_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let input_details = usage.get("input_tokens_details");
    let cached_tokens = input_details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let cache_write_tokens = input_details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let input_uncached_tokens = (input_tokens - cached_tokens - cache_write_tokens).max(0);

    let output_details = usage.get("output_tokens_details");
    let reasoning_tokens = output_details
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let cost_f64 = usage.get("cost").and_then(|v| v.as_f64());
    let provider_cost_micro_usd = parse_provider_cost(cost_f64);

    Some(ExtractedUsage {
        input_uncached_tokens,
        cache_read_tokens: cached_tokens,
        cache_write_tokens,
        output_tokens,
        reasoning_tokens,
        provider_cost_micro_usd,
    })
}

pub fn extract_messages_json_usage(json: &serde_json::Value) -> Option<ExtractedUsage> {
    let usage = json.get("usage")?;
    if !usage.is_object() {
        return None;
    }
    let input_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cache_write = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("reasoning_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let cost_f64 = usage.get("cost").and_then(|v| v.as_f64());
    let provider_cost_micro_usd = parse_provider_cost(cost_f64);

    Some(ExtractedUsage {
        input_uncached_tokens: input_tokens,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        output_tokens,
        reasoning_tokens,
        provider_cost_micro_usd,
    })
}
