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

// ---------------------------------------------------------------------------
// ISSUE-0179 — the catalog fetch must bound bytes, not just time, and must stamp
// written rows from the injected clock.
// ---------------------------------------------------------------------------

/// A source of body chunks: yields `Ok(Some(chunk))`, then `Ok(None)` at end.
///
/// Fed to the bounded reader so the byte-ceiling logic is exercised without a network, a
/// TLS endpoint, or a hostile server. The reader's contract is "stop at the ceiling", and
/// that is exactly what these fakes measure.
struct FakeChunks {
    remaining: usize,
    chunk_len: usize,
    /// Total bytes actually handed to the reader, so a test can prove the reader stopped
    /// early rather than draining the body and checking afterwards.
    delivered: usize,
}

impl FakeChunks {
    fn new(total: usize, chunk_len: usize) -> Self {
        Self {
            remaining: total,
            chunk_len,
            delivered: 0,
        }
    }
}

impl super::settings::CatalogChunkSource for FakeChunks {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let take = self.remaining.min(self.chunk_len);
        self.remaining -= take;
        self.delivered += take;
        Ok(Some(vec![b'x'; take]))
    }
}

/// A catalog body larger than `SEC-8`'s ceiling must be refused, not parsed (`ISSUE-0179`).
///
/// The ceiling is [`llmroller_proto::discovery::MAX_DISCOVERY_BYTES`] — `SEC-8`'s "2 MiB max
/// `/models` or `/usage` response size". One number for every remote catalog this codebase
/// reads, rather than a locally-tuned second constant.
#[tokio::test]
#[doc = "Spec: SEC-8"]
async fn price_sync_rejects_an_oversized_catalog_body() {
    use llmroller_proto::discovery::MAX_DISCOVERY_BYTES;

    // One byte over the ceiling, in 64 KiB chunks, so the reader must decide mid-stream
    // rather than trusting a `content-length` it may not have been given.
    let mut source = FakeChunks::new(MAX_DISCOVERY_BYTES + 1, 64 * 1024);
    let err = super::settings::read_catalog_body(&mut source)
        .await
        .expect_err("a body over the ceiling must be refused, not returned");
    assert!(
        err.contains("exceed"),
        "the error must say the body was too large; got {err:?}"
    );
    assert!(
        source.delivered <= MAX_DISCOVERY_BYTES + 64 * 1024,
        "the reader must stop near the ceiling instead of draining the whole body; \
         delivered {} bytes",
        source.delivered
    );
}

/// A body exactly at the ceiling is accepted — the bound must not be off by one.
#[tokio::test]
#[doc = "Spec: SEC-8"]
async fn price_sync_accepts_a_catalog_body_at_the_ceiling() {
    use llmroller_proto::discovery::MAX_DISCOVERY_BYTES;

    let mut source = FakeChunks::new(MAX_DISCOVERY_BYTES, 64 * 1024);
    let body = super::settings::read_catalog_body(&mut source)
        .await
        .expect("a body at the ceiling must be accepted");
    assert_eq!(body.len(), MAX_DISCOVERY_BYTES);
}

/// A small body is returned unchanged, so the bound cannot corrupt normal payloads.
#[tokio::test]
#[doc = "Spec: SEC-8"]
async fn price_sync_passes_a_small_catalog_body_through() {
    let mut source = FakeChunks::new(1024, 256);
    let body = super::settings::read_catalog_body(&mut source)
        .await
        .expect("a small body must be accepted");
    assert_eq!(body.len(), 1024);
    assert!(body.iter().all(|b| *b == b'x'));
}

/// A mid-stream transport error must surface as an error, never as a short body.
///
/// A truncated body that parsed would import partial prices and report success.
#[tokio::test]
#[doc = "Spec: SEC-8"]
async fn price_sync_surfaces_a_mid_stream_error() {
    struct Broken;
    impl super::settings::CatalogChunkSource for Broken {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
            Err("connection reset".to_string())
        }
    }
    let err = super::settings::read_catalog_body(&mut Broken)
        .await
        .expect_err("a transport failure must not look like a complete body");
    assert!(err.contains("connection reset"), "got {err:?}");
}

/// The sync must stamp written price rows from the injected clock (`ISSUE-0179`).
///
/// `chrono_like_now_ms()` reads `SystemTime::now()`, so a row written by the sync carries
/// wall-clock time regardless of the `Clock` the handler was handed. Every other handler in
/// this file uses `state.auth.clock`. Asserted by source scan because a live sync's
/// wall-clock stamp is indistinguishable from a fake-clock one without the seam, and the
/// seam does not exist for this handler.
#[test]
#[doc = "Spec: USG-3"]
fn price_sync_stamps_rows_from_the_injected_clock() {
    let source = include_str!("settings.rs");
    let sync_handler = source
        .split("pub(crate) async fn post_admin_prices_sync_handler")
        .nth(1)
        .and_then(|rest| rest.split("pub(crate) fn chrono_like_now_ms").next())
        .expect("sync handler body");
    assert!(
        !sync_handler.contains("chrono_like_now_ms"),
        "the price sync must use `state.auth.clock.now_ms()`, not the wall-clock helper"
    );
    assert!(
        sync_handler.contains("state.auth.clock.now_ms()"),
        "the price sync must stamp rows from the injected clock"
    );
}
