use super::settings::parse_catalog_prices;

/// `price_sync_catalog_url` catalogues publish context-length tiers and aliases;
/// the sync handler must import both instead of reporting a hardcoded zero (F012).
#[test]
fn parses_context_tiers_and_aliases_from_the_catalogue() {
    let catalog = serde_json::json!({
        "openrouter": {
            "models": {
                "vendor/model-a": {
                    "cost": {
                        "input": 3.0,
                        "output": 15.0,
                        "cache_read": 0.3,
                        "cache_write": 3.75,
                        "input_over_200k": 6.0,
                        "output_over_200k": 22.5
                    },
                    "alias": "model-a",
                    "aliases": ["model-a-latest", "model-a"]
                }
            }
        }
    });
    let (prices, aliases) = parse_catalog_prices(&catalog);
    let price = prices.get("vendor/model-a").expect("price template");
    assert_eq!(price.rates.input_per_mtok_micro_usd, 3_000_000);
    assert_eq!(price.rates.output_per_mtok_micro_usd, 15_000_000);
    assert_eq!(price.tiers.len(), 1);
    let tier = &price.tiers[0];
    assert_eq!(tier.min_total_input_tokens, 200_000);
    assert_eq!(tier.input_per_mtok_micro_usd, 6_000_000);
    assert_eq!(tier.output_per_mtok_micro_usd, 22_500_000);
    assert_eq!(
        aliases.get("vendor/model-a").cloned().unwrap_or_default(),
        vec![
            "model-a".to_string(),
            "model-a-latest".to_string(),
            "model-a".to_string()
        ]
    );
}

#[test]
#[doc = "Spec: USG-3, PRV-2"]
fn parses_openrouter_models_data_array_and_normalizes_model_ids() {
    let catalog = serde_json::json!({
        "data": [
            {
                "id": "anthropic/claude-sonnet-4.6",
                "pricing": {
                    "prompt": "0.000003",
                    "completion": "0.000015",
                    "input_cache_read": "0.0000003",
                    "input_cache_write": "0.00000375"
                }
            }
        ]
    });
    let (prices, _) = parse_catalog_prices(&catalog);
    let norm = prices
        .get("claude-sonnet-4.6")
        .expect("normalized model id entry");
    assert_eq!(norm.rates.input_per_mtok_micro_usd, 3_000_000);
    assert_eq!(norm.rates.output_per_mtok_micro_usd, 15_000_000);
    assert_eq!(norm.rates.cache_read_per_mtok_micro_usd, 300_000);
    assert_eq!(norm.rates.cache_write_per_mtok_micro_usd, 3_750_000);
}

#[test]
#[doc = "Spec: SEC-5, SEC-19, UI-1, UI-3; F077, F079, F080, F094"]
fn templates_have_zero_inline_handlers_and_correct_data_plane_port() {
    let app_html = include_str!("../../templates/app.html");
    let admin_html = include_str!("../../templates/admin.html");
    let login_html = include_str!("../../templates/login.html");
    let setup_html = include_str!("../../templates/setup.html");
    assert!(!app_html.contains("onchange="));
    assert!(!app_html.contains("onclick="));
    assert!(app_html.contains("http://127.0.0.1:8080"));
    assert!(!app_html.contains("http://127.0.0.1:8082"));
    // F077: login.html and setup.html must have zero inline <style> or <script> blocks
    assert!(!login_html.contains("<style"));
    assert!(!login_html.contains("<script>"));
    assert!(!setup_html.contains("<style"));
    assert!(!setup_html.contains("<script>"));
    // F079: admin.html must include #modal-new-upstream-key
    assert!(admin_html.contains("id=\"modal-new-upstream-key\""));
    // F080 & F094: render_logs_table_html emits exactly one <thead> and escapes XSS payloads
    let logs_json = serde_json::json!({
        "rows": [{
            "request_id": "req_<script>",
            "ts": 1000,
            "status": 200,
            "canonical_id": "<script>alert(1)</script>",
            "client_key_label": "key_<b>",
            "latency_ms": 42,
            "total_tokens": 100,
            "cost_formatted": "$0.001"
        }]
    });
    let rendered = super::render_logs_table_html(&logs_json, false, "/admin/partials/logs");
    assert_eq!(rendered.matches("<thead>").count(), 1);
    assert_eq!(rendered.matches("<tbody>").count(), 1);
    assert!(!rendered.contains("<script>"));
    assert!(rendered.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
}

#[test]
#[doc = "Spec: PRV-2, CLK-7, UI-2"]
fn web_ui_preset_urls_and_proxy_port_match_presets_and_cli_defaults() {
    let app_html = include_str!("../../templates/app.html");
    let app_js = crate::assets::APP_JS;

    // Verify Quick Connect snippets in app.html use default data-plane port 8080 and zero 8082
    assert!(app_html.contains("export ANTHROPIC_BASE_URL=\"http://127.0.0.1:8080\""));
    assert!(app_html.contains("export OPENAI_BASE_URL=\"http://127.0.0.1:8080/v1\""));
    assert!(!app_html.contains("8082"));

    // Verify onProviderKindChange in app.js synchronizes with authoritative presets/*.toml base URLs
    assert!(app_js.contains("https://openrouter.ai/api/v1"));
    assert!(app_js.contains("https://opencode.ai/zen/go/v1"));
    assert!(app_js.contains("https://api.commandcode.ai/provider/v1"));
    assert!(app_js.contains("https://amr-link.open-design.ai/v1"));
}

/// Models without a `cost` block are not priced, and the catalogue shape is
/// tolerated defensively (no `models` key, non-object roots).
#[test]
fn ignores_entries_without_cost_and_tolerates_unknown_shapes() {
    let catalog = serde_json::json!({
        "provider": { "models": { "unpriced": { "alias": "u" } } },
        "broken": 7
    });
    let (prices, aliases) = parse_catalog_prices(&catalog);
    assert!(prices.is_empty());
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases["unpriced"], vec!["u".to_string()]);
}
