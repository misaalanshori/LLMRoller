//! Property-based and unit tests for core pricing engine.
//!
//! Spec: Section 6.9, Section 9.3.2, Section 13.1, Requirements Partial: USG-3.

use super::*;
use proptest::prelude::*;

#[doc = "Partial: USG-3"]
#[test]
fn test_provider_cost_parsing_and_fallback_sources() {
    // Finite valid costs in [0.0, 1000.0]
    assert_eq!(parse_provider_cost(Some(0.0)), Some(0));
    assert_eq!(parse_provider_cost(Some(0.000001)), Some(1));
    assert_eq!(parse_provider_cost(Some(1.234567)), Some(1_234_567));
    assert_eq!(parse_provider_cost(Some(1000.0)), Some(1_000_000_000));

    // Invalid costs: negative, NaN, infinity, > 1000.0
    assert_eq!(parse_provider_cost(Some(-0.01)), None);
    assert_eq!(parse_provider_cost(Some(f64::NAN)), None);
    assert_eq!(parse_provider_cost(Some(f64::INFINITY)), None);
    assert_eq!(parse_provider_cost(Some(f64::NEG_INFINITY)), None);
    assert_eq!(parse_provider_cost(Some(1000.001)), None);
    assert_eq!(parse_provider_cost(None), None);

    let tokens = TokenUsage {
        input_uncached: 1_000,
        cache_read: 0,
        cache_write: 0,
        output: 100,
    };
    let fallback = Rates::default();

    // Valid provider cost takes precedence
    let res = compute_request_cost(Some(0.05), tokens, None, fallback, 0);
    assert_eq!(res.cost_micro_usd, 50_000);
    assert_eq!(res.cost_source, CostSource::Provider);

    // Negative cost falls back to fallback rate
    let res = compute_request_cost(Some(-1.0), tokens, None, fallback, 0);
    assert_eq!(res.cost_source, CostSource::FallbackRate);
    assert!(res.cost_micro_usd > 0);
}

#[doc = "Partial: USG-3"]
#[test]
fn test_missing_price_rows_charge_unpriced_fallback_rates() {
    let fallback = Rates::default(); // $3 / $15 / $0.30 / $3.75
    assert_eq!(fallback.input_per_mtok_micro_usd, 3_000_000);
    assert_eq!(fallback.output_per_mtok_micro_usd, 15_000_000);
    assert_eq!(fallback.cache_read_per_mtok_micro_usd, 300_000);
    assert_eq!(fallback.cache_write_per_mtok_micro_usd, 3_750_000);

    let tokens = TokenUsage {
        input_uncached: 1_000_000,
        cache_read: 1_000_000,
        cache_write: 1_000_000,
        output: 1_000_000,
    };

    let res = compute_request_cost(None, tokens, None, fallback, 0);
    assert_eq!(res.cost_source, CostSource::FallbackRate);
    // 3M + 0.3M + 3.75M + 15M = 22.05M micro-USD ($22.05)
    assert_eq!(res.cost_micro_usd, 22_050_000);
    assert_ne!(res.cost_micro_usd, 0);
}

#[doc = "Partial: USG-3"]
#[test]
fn test_context_tiers_and_schedules_deterministic_evaluation() {
    let tier1 = ContextTier {
        min_total_input_tokens: 0,
        max_total_input_tokens: Some(100_000),
        input_per_mtok_micro_usd: 1_000_000,
        output_per_mtok_micro_usd: 2_000_000,
        cache_read_per_mtok_micro_usd: 100_000,
        cache_write_per_mtok_micro_usd: 1_250_000,
    };
    let tier2 = ContextTier {
        min_total_input_tokens: 100_001,
        max_total_input_tokens: None,
        input_per_mtok_micro_usd: 2_000_000,
        output_per_mtok_micro_usd: 4_000_000,
        cache_read_per_mtok_micro_usd: 200_000,
        cache_write_per_mtok_micro_usd: 2_500_000,
    };

    assert!(validate_tiers(&[tier1.clone(), tier2.clone()]).is_ok());

    // Overlapping tiers rejected
    let overlapping_tier = ContextTier {
        min_total_input_tokens: 50_000,
        max_total_input_tokens: Some(150_000),
        input_per_mtok_micro_usd: 1_500_000,
        output_per_mtok_micro_usd: 3_000_000,
        cache_read_per_mtok_micro_usd: 150_000,
        cache_write_per_mtok_micro_usd: 1_875_000,
    };
    assert!(validate_tiers(&[tier1.clone(), overlapping_tier]).is_err());

    // Peak schedule: weekdays Monday through Friday, 02:00 to 04:00 UTC (120 to 240 mins)
    let peak_sched = Schedule {
        name: "peak".to_string(),
        iso_weekdays: vec![1, 2, 3, 4, 5],
        utc_minute_ranges: vec![[120, 240]],
        multiplier_num: 2,
        multiplier_den: 1,
    };
    assert!(validate_schedules(std::slice::from_ref(&peak_sched)).is_ok());

    let price_entry = PriceEntry {
        provider_id: "p1".to_string(),
        provider_kind: "openai".to_string(),
        upstream_model_id: "m1".to_string(),
        base_rates: Rates {
            input_per_mtok_micro_usd: 500_000,
            output_per_mtok_micro_usd: 1_000_000,
            cache_read_per_mtok_micro_usd: 50_000,
            cache_write_per_mtok_micro_usd: 625_000,
        },
        tiers: vec![tier1, tier2],
        schedules: vec![peak_sched],
    };

    // Thursday 2026-10-22 at 01:00 UTC (60 mins, off-peak)
    // 2026-10-22 00:00:00 UTC = 1792627200 seconds
    let ts_off_peak = (1_792_627_200 + 3600) * 1000;
    // Thursday 2026-10-22 at 03:00 UTC (180 mins, peak)
    let ts_peak = (1_792_627_200 + 3 * 3600) * 1000;

    let tokens_low = TokenUsage {
        input_uncached: 50_000,
        cache_read: 0,
        cache_write: 0,
        output: 0,
    };
    // In tier 1: rate_in = 1_000_000 micro-USD per MTok ($1.00/MTok)
    // Off-peak: 50_000 * 1_000_000 / 1_000_000 = 50_000 micro-USD ($0.05)
    let res_low_off = compute_request_cost(
        None,
        tokens_low,
        Some(&price_entry),
        Rates::default(),
        ts_off_peak,
    );
    assert_eq!(res_low_off.cost_source, CostSource::PriceBook);
    assert_eq!(res_low_off.cost_micro_usd, 50_000);

    // Peak: 2x multiplier = 100_000 micro-USD
    let res_low_peak = compute_request_cost(
        None,
        tokens_low,
        Some(&price_entry),
        Rates::default(),
        ts_peak,
    );
    assert_eq!(res_low_peak.cost_micro_usd, 100_000);

    // High tokens: total_input = 150_000 -> tier 2 (rate_in = 2_000_000 micro-USD)
    let tokens_high = TokenUsage {
        input_uncached: 150_000,
        cache_read: 0,
        cache_write: 0,
        output: 0,
    };
    let res_high_off = compute_request_cost(
        None,
        tokens_high,
        Some(&price_entry),
        Rates::default(),
        ts_off_peak,
    );
    assert_eq!(res_high_off.cost_micro_usd, 300_000);

    let res_high_peak = compute_request_cost(
        None,
        tokens_high,
        Some(&price_entry),
        Rates::default(),
        ts_peak,
    );
    assert_eq!(res_high_peak.cost_micro_usd, 600_000);
}

#[doc = "Partial: USG-3"]
#[test]
fn test_binary_aware_token_estimator_rules() {
    // 1. Text-only JSON body
    let text_body =
        br#"{"model": "gpt-4", "messages": [{"role": "user", "content": "Hello, world!"}]}"#;
    let tokens = estimate_input_tokens(text_body);
    // "gpt-4" (5) + "user" (4) + "Hello, world!" (13) = 22 bytes text.
    // ceil(22 / 4) = 6 tokens.
    assert_eq!(tokens, 6);

    // 2. Large screenshot under "data" (e.g. 4 MiB of simulated base64)
    let mut large_image_body = Vec::new();
    large_image_body.extend_from_slice(br#"{"messages": [{"role": "user", "content": [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": ""#);
    // 4 MiB base64
    large_image_body.resize(large_image_body.len() + 4 * 1024 * 1024, b'A');
    large_image_body.extend_from_slice(br#""}}]}]}"#);

    let est = estimate_input_tokens(&large_image_body);
    // 1 binary block = 1,600 tokens plus minor text tokens <= 2,000 tokens
    assert!(
        est <= 2000,
        "4 MiB screenshot estimated at {est} tokens, must be <= 2,000"
    );
    assert!(
        est >= 1600,
        "4 MiB screenshot must have at least 1,600 tokens"
    );

    // 3. Large screenshot under "image_url": {"url": "data:image/png;base64,..."}
    let mut openai_image_body = Vec::new();
    openai_image_body.extend_from_slice(br#"{"messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,"#);
    openai_image_body.resize(openai_image_body.len() + 100_000, b'B');
    openai_image_body.extend_from_slice(br#""}}]}]}"#);

    let est_openai = estimate_input_tokens(&openai_image_body);
    assert!(
        (1600..=2000).contains(&est_openai),
        "OpenAI image estimated at {est_openai}"
    );

    // 4. Multiple binary blocks (2 blocks of 2000 bytes each)
    let mut multi_image_body = Vec::new();
    multi_image_body.extend_from_slice(
        br#"{"messages": [{"role": "user", "content": [{"type": "file", "file_data": ""#,
    );
    multi_image_body.resize(multi_image_body.len() + 2000, b'C');
    multi_image_body.extend_from_slice(br#""}, {"type": "file", "b64_json": ""#);
    multi_image_body.resize(multi_image_body.len() + 2000, b'D');
    multi_image_body.extend_from_slice(br#""}]}]}"#);

    let est_multi = estimate_input_tokens(&multi_image_body);
    // 2 binary blocks * 1600 = 3200 + text <= 3300
    assert!(
        (3200..=3300).contains(&est_multi),
        "Multi-image estimated at {est_multi}"
    );
}

proptest! {
    #[doc = "Partial: USG-3"]
    #[test]
    fn prop_i128_ceiling_division_no_overflow(
        uncached in 0i64..1_000_000_000i64,
        cache_read in 0i64..1_000_000_000i64,
        cache_write in 0i64..1_000_000_000i64,
        output in 0i64..1_000_000_000i64,
        rate_in in 0i64..1_000_000_000i64,
        rate_out in 0i64..1_000_000_000i64,
        rate_cr in 0i64..1_000_000_000i64,
        rate_cw in 0i64..1_000_000_000i64,
        multiplier_num in 1i64..100i64,
        multiplier_den in 1i64..100i64,
    ) {
        let tokens = TokenUsage {
            input_uncached: uncached,
            cache_read,
            cache_write,
            output,
        };
        let rates = Rates {
            input_per_mtok_micro_usd: rate_in,
            output_per_mtok_micro_usd: rate_out,
            cache_read_per_mtok_micro_usd: rate_cr,
            cache_write_per_mtok_micro_usd: rate_cw,
        };

        let cost = calculate_cost_micro_usd(tokens, rates, multiplier_num, multiplier_den);
        prop_assert!(cost >= 0);
    }
}

#[doc = "Spec: USG-10 — subscription plan multiplier scaling and dual-cost calculation"]
#[test]
fn test_subscription_plan_multiplier_dual_cost_math() {
    // 1. subscription_plan applies plan_multiplier_bps scaling:
    // 10_000 bps = 1.0x (unadjusted)
    assert_eq!(apply_plan_multiplier(100_000, 10_000, "subscription_plan"), 100_000);
    // 5_000 bps = 50%
    assert_eq!(apply_plan_multiplier(100_000, 5_000, "subscription_plan"), 50_000);
    // 2_500 bps = 25%
    assert_eq!(apply_plan_multiplier(100_000, 2_500, "subscription_plan"), 25_000);
    // 0 bps = 0
    assert_eq!(apply_plan_multiplier(100_000, 0, "subscription_plan"), 0);
    // 20_000 bps = 2.0x
    assert_eq!(apply_plan_multiplier(100_000, 20_000, "subscription_plan"), 200_000);

    // 2. Other billing coverage modes always charge unadjusted list price:
    assert_eq!(apply_plan_multiplier(100_000, 5_000, "pay_as_you_go"), 100_000);
    assert_eq!(apply_plan_multiplier(100_000, 5_000, "shared_grant"), 100_000);
    assert_eq!(apply_plan_multiplier(100_000, 5_000, "free_tier"), 100_000);
    assert_eq!(apply_plan_multiplier(100_000, 0, "pay_as_you_go"), 100_000);
}
