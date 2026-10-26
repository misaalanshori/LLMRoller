//! Non-streaming JSON token usage, stop_reason, and provider cost extractor across protocols.
//!
//! Spec: Section 2.5, Section 6.8, Section 6.9, Section 12.5,
//! Requirements: API-5, USG-1, USG-2, USG-9.

use crate::adapters::EndpointKind;
use llmroller_core::pricing::TokenUsage;

/// Normalized token usage and provider cost extracted from upstream responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExtractedUsage {
    pub input_uncached_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub provider_cost_micro_usd: Option<i64>,
}

impl ExtractedUsage {
    pub fn to_token_usage(self) -> TokenUsage {
        TokenUsage {
            input_uncached: self.input_uncached_tokens,
            cache_read: self.cache_read_tokens,
            cache_write: self.cache_write_tokens,
            output: self.output_tokens,
            reasoning: self.reasoning_tokens,
        }
    }
}

/// Extracts normalized usage from Google Gemini / Cloud Code Assist (`usageMetadata`), AWS Bedrock (`usage.inputTokens`), or Cohere (`meta.billed_units`) JSON objects (`FND-014`, `USG-1`).
pub fn extract_extended_provider_json_usage(json: &serde_json::Value) -> Option<ExtractedUsage> {
    // 1. Google Gemini / Vertex AI / Cloud Code Assist: usageMetadata or response.usageMetadata
    if let Some(um) = json
        .get("usageMetadata")
        .or_else(|| json.pointer("/response/usageMetadata"))
        && um.is_object()
    {
        let prompt_tokens = um
            .get("promptTokenCount")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let cache_read_tokens = um
            .get("cachedContentTokenCount")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let output_tokens = um
            .get("candidatesTokenCount")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let reasoning_tokens = um
            .get("thoughtsTokenCount")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let input_uncached_tokens = prompt_tokens.saturating_sub(cache_read_tokens).max(0);
        return Some(ExtractedUsage {
            input_uncached_tokens,
            cache_read_tokens,
            cache_write_tokens: 0,
            output_tokens,
            reasoning_tokens,
            provider_cost_micro_usd: extract_provider_cost(um),
        });
    }

    // 2. AWS Bedrock Converse / ConverseStream or Command Code camelCase usage
    if let Some(usage) = json
        .get("usage")
        .or_else(|| json.pointer("/metadata/usage"))
        && usage.is_object()
        && (usage.get("inputTokens").is_some()
            || usage.get("outputTokens").is_some()
            || usage.get("promptTokens").is_some()
            || usage.get("completionTokens").is_some())
    {
        let input_tokens = usage
            .get("inputTokens")
            .or_else(|| usage.get("promptTokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let output_tokens = usage
            .get("outputTokens")
            .or_else(|| usage.get("completionTokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let cache_read_tokens = usage
            .get("cacheReadInputTokens")
            .or_else(|| usage.get("cachedTokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let cache_write_tokens = usage
            .get("cacheWriteInputTokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        return Some(ExtractedUsage {
            input_uncached_tokens: input_tokens,
            cache_read_tokens,
            cache_write_tokens,
            output_tokens,
            reasoning_tokens: 0,
            provider_cost_micro_usd: extract_provider_cost(usage),
        });
    }

    // 3. Cohere v2 / Command Code billed_units
    if let Some(bu) = json
        .pointer("/meta/billed_units")
        .or_else(|| json.pointer("/usage/billed_units"))
        .or_else(|| json.pointer("/usage/tokens"))
        && bu.is_object()
    {
        let input_tokens = bu
            .get("input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let output_tokens = bu
            .get("output_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        return Some(ExtractedUsage {
            input_uncached_tokens: input_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            output_tokens,
            reasoning_tokens: 0,
            provider_cost_micro_usd: None,
        });
    }

    None
}

/// Extracts normalized usage from a parsed JSON response object according to the endpoint protocol.
pub fn extract_json_usage(
    endpoint: EndpointKind,
    json: &serde_json::Value,
) -> Option<ExtractedUsage> {
    if let Some(ext) = extract_extended_provider_json_usage(json) {
        return Some(ext);
    }
    match endpoint {
        EndpointKind::Chat | EndpointKind::Embeddings => extract_chat_json_usage(json),
        EndpointKind::Responses | EndpointKind::ResponsesCompact => {
            extract_responses_json_usage(json)
        }
        EndpointKind::Messages => extract_messages_json_usage(json),
        _ => None,
    }
}

/// Extracts normalized `stop_reason` from a parsed JSON response or SSE event (`USG-9`, Section 12.5).
pub fn extract_json_stop_reason(
    endpoint: EndpointKind,
    json: &serde_json::Value,
) -> Option<String> {
    match endpoint {
        EndpointKind::Chat => json
            .pointer("/choices/0/finish_reason")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        EndpointKind::Responses | EndpointKind::ResponsesCompact => {
            let resp = json.get("response").unwrap_or(json);
            if let Some(reason) = resp
                .pointer("/incomplete_details/reason")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return Some(if reason == "max_output_tokens" {
                    "length".to_string()
                } else {
                    reason.to_string()
                });
            }
            resp.get("status")
                .and_then(serde_json::Value::as_str)
                .and_then(|status| match status {
                    "completed" => Some("stop".to_string()),
                    "incomplete" => Some("length".to_string()),
                    "failed" | "cancelled" => Some(status.to_string()),
                    _ => None,
                })
        }
        EndpointKind::Messages => json
            .pointer("/delta/stop_reason")
            .or_else(|| json.pointer("/message/stop_reason"))
            .or_else(|| json.get("stop_reason"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Extracts usage from OpenAI `chat` format (`POST /v1/chat/completions`).
pub fn extract_chat_json_usage(json: &serde_json::Value) -> Option<ExtractedUsage> {
    let usage = json.get("usage")?;
    if usage.is_null() {
        return None;
    }

    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);

    let details = usage.get("prompt_tokens_details");
    let cache_read_tokens = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
        .or_else(|| {
            usage
                .get("prompt_cache_hit_tokens")
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0)
        .max(0);

    let cache_write_tokens = details
        .and_then(|d| {
            d.get("cache_write_tokens")
                .or_else(|| d.get("cache_creation_input_tokens"))
        })
        .and_then(|v| v.as_i64())
        .or_else(|| {
            usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0)
        .max(0);

    let input_uncached_tokens = if let Some(miss) = usage
        .get("prompt_cache_miss_tokens")
        .and_then(|v| v.as_i64())
    {
        miss.max(0)
    } else {
        prompt_tokens
            .saturating_sub(cache_read_tokens)
            .saturating_sub(cache_write_tokens)
            .max(0)
    };

    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .and_then(|d| {
            d.get("reasoning_tokens")
                .or_else(|| d.get("thinking_tokens"))
        })
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);

    let provider_cost_micro_usd = extract_provider_cost(usage);

    Some(ExtractedUsage {
        input_uncached_tokens,
        cache_read_tokens,
        cache_write_tokens,
        output_tokens: completion_tokens,
        reasoning_tokens,
        provider_cost_micro_usd,
    })
}

/// Extracts usage from OpenAI `responses` format (`POST /v1/responses`, `F021`).
pub fn extract_responses_json_usage(json: &serde_json::Value) -> Option<ExtractedUsage> {
    let usage = json
        .get("usage")
        .or_else(|| json.get("response").and_then(|r| r.get("usage")))
        .or_else(|| json.get("item").and_then(|i| i.get("usage")))?;
    if usage.is_null() {
        return None;
    }

    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);

    let in_details = usage
        .get("input_tokens_details")
        .or_else(|| usage.get("prompt_tokens_details"));
    let cache_read_tokens = in_details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_i64())
        .or_else(|| usage.get("cached_tokens").and_then(|v| v.as_i64()))
        .unwrap_or(0)
        .max(0);
    let cache_write_tokens = in_details
        .and_then(|d| {
            d.get("cache_write_tokens")
                .or_else(|| d.get("cache_creation_input_tokens"))
        })
        .and_then(|v| v.as_i64())
        .or_else(|| {
            usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0)
        .max(0);

    let input_uncached_tokens = input_tokens
        .saturating_sub(cache_read_tokens)
        .saturating_sub(cache_write_tokens)
        .max(0);

    let reasoning_tokens = usage
        .get("output_tokens_details")
        .or_else(|| usage.get("completion_tokens_details"))
        .and_then(|d| {
            d.get("reasoning_tokens")
                .or_else(|| d.get("thinking_tokens"))
        })
        .and_then(|v| v.as_i64())
        .or_else(|| usage.get("reasoning_tokens").and_then(|v| v.as_i64()))
        .unwrap_or(0)
        .max(0);

    let provider_cost_micro_usd = extract_provider_cost(usage);

    Some(ExtractedUsage {
        input_uncached_tokens,
        cache_read_tokens,
        cache_write_tokens,
        output_tokens,
        reasoning_tokens,
        provider_cost_micro_usd,
    })
}

/// Extracts usage from Anthropic `messages` format (`POST /v1/messages`).
///
/// Supports both top-level `cache_creation_input_tokens` and nested OpenRouter/Anthropic
/// `cache_creation.{ephemeral_5m_input_tokens, ephemeral_1h_input_tokens}` as well as
/// `output_tokens_details.thinking_tokens` (`F-005`, SRDD Section 12.5).
pub fn extract_messages_json_usage(json: &serde_json::Value) -> Option<ExtractedUsage> {
    let usage = json
        .get("usage")
        .or_else(|| json.get("message").and_then(|m| m.get("usage")))
        .or_else(|| json.get("delta").and_then(|d| d.get("usage")))?;
    if usage.is_null() {
        return None;
    }

    let input_uncached_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let cache_read_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let cache_write_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_i64())
        .or_else(|| {
            usage.get("cache_creation").map(|cc| {
                let e5m = cc
                    .get("ephemeral_5m_input_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    .max(0);
                let e1h = cc
                    .get("ephemeral_1h_input_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    .max(0);
                e5m.saturating_add(e1h)
            })
        })
        .unwrap_or(0)
        .max(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let reasoning_tokens = usage
        .get("output_tokens_details")
        .and_then(|d| {
            d.get("thinking_tokens")
                .or_else(|| d.get("reasoning_tokens"))
        })
        .and_then(|v| v.as_i64())
        .or_else(|| {
            usage
                .get("reasoning_tokens")
                .or_else(|| usage.get("thinking_tokens"))
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0)
        .max(0);
    let provider_cost_micro_usd = extract_provider_cost(usage);

    Some(ExtractedUsage {
        input_uncached_tokens,
        cache_read_tokens,
        cache_write_tokens,
        output_tokens,
        reasoning_tokens,
        provider_cost_micro_usd,
    })
}

fn extract_provider_cost(usage: &serde_json::Value) -> Option<i64> {
    if let Some(cost_f64) = usage.get("cost").and_then(|v| v.as_f64())
        && cost_f64.is_finite()
        && (0.0..=1000.0).contains(&cost_f64)
    {
        return Some((cost_f64 * 1_000_000.0).round() as i64);
    }
    None
}
