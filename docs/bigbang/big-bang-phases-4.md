# Epic 4: Usage pipeline, price book, budget windows, and source `/usage` engine

Epic 4 builds the SSE and JSON token extractors, the integer micro-USD price book, and the batching SQLite writer. It also implements budget window ring counters and the source provider `/usage` poller, reconciler, and pass-through endpoints. It corresponds to Milestones `M3` and `M4` (`SRDD Sections 2.4, 2.5, 6.9, 7.2, 9, 12.5, 13`).

## 4.1 Sub-phase 4.1: `SseUsageTap`, `JsonUsageExtractor`, and cache token normalization

This sub-phase extracts token counts across `chat`, `responses`, and `messages` (`SRDD Section 12.5`), normalizes OpenAI vs. Anthropic cache token accounting (`USG-2`), and detects truncated streams (`USG-1`, `R7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.5`, `Section 6.9`, `Section 12.5`; Requirements `USG-1`, `USG-2` |
| Crates touched | `crates/proto::sse_tap`, `crates/proto::json_usage`, `fixtures/sse/*` |
| TDD step (`Red -> Green`) | Write golden fixture tests (`proto::sse_tap::golden_tests`, `#[doc = "Spec: USG-1, USG-2"]`) replaying recorded streams for OpenAI `chat`, OpenAI `responses`, Anthropic `messages`, OpenRouter `usage.cost`, and mid-stream drops missing `[DONE]`, `response.completed`, or `message_stop`. Assert that `input_uncached = prompt_tokens - cached_tokens` on `chat` and `responses`, while `input_uncached = input_tokens` on `messages`, and that truncated streams set `incomplete = 1`. |
| Atomic commits | 1. `feat(proto/4.1): implement SseUsageTap and JsonUsageExtractor with cache normalization and truncation detection [Spec: USG-1, USG-2]` |
| Risks and resolutions | **Risk:** A provider emits usage across two separate SSE events (`message_start` and `message_delta` in Anthropic `messages`) and aborts before `message_stop`. **Resolution:** Accumulate `message_start` input/cache counts immediately and update `output_tokens` on each `message_delta`; if `message_stop` never arrives, emit the partial usage with `incomplete = 1`. |

## 4.2 Sub-phase 4.2: Integer micro-USD price book, context tiers, and UTC peak schedules

This sub-phase implements 128-bit integer micro-USD cost calculation (`SRDD Section 13.1`), 200K+ context-length tier rates, and weekday UTC peak-hour multipliers (`USG-3`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.3`, `Section 6.9`, `Section 13.1`; Requirements `USG-3` |
| Crates touched | `crates/core::pricing`, `presets/*.toml` |
| TDD step (`Red -> Green`) | Write property-based and table-driven tests (`core::pricing::proptests`, `#[doc = "Spec: USG-3"]`) verifying: (1) provider-reported `usage.cost` wins (`cost_source = 'provider'`), (2) price-book math uses `i128` ceiling division with zero overflow up to `10^9` tokens, (3) prompts `> 200,000` tokens select the high-context tier rate, and (4) DeepSeek calls at `02:30 UTC` on Tuesday apply the 2x peak multiplier while `02:30 UTC` on Saturday uses the off-peak rate. |
| Atomic commits | 1. `feat(core/4.2): implement i128 micro-USD pricing engine with context tiers and UTC schedules [Spec: USG-3]` |
| Risks and resolutions | **Risk:** A newly discovered model has no entry in `prices`. **Resolution:** Record `cost_micro_usd = NULL` and `cost_source = 'unpriced'` (never `$0`), surface an "Unpriced models" badge on the dashboard, and support one-click baseline import from `models.dev`. |

## 4.3 Sub-phase 4.3: Batching SQLite usage writer, bounded overflow queue, and hourly rollups

This sub-phase implements the non-blocking `mpsc` usage queue (10,000 slots) and the batching SQLite writer (`SRDD Section 7.2, Section 9.2`). Each transaction commits up to 200 rows every 250 ms and updates `usage_rollups_hourly`.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.2`, `Section 9.1`, `Section 9.2`; Requirements `USG-1`, `REL-2`, `REL-3` |
| Crates touched | `crates/store::usage_writer`, `crates/store::rollups` |
| TDD step (`Red -> Green`) | Write fault-injection and crash tests (`tests/integration_usage_writer.rs`, `#[doc = "Spec: USG-1, REL-2, REL-3"]`): (1) pause the SQLite writer task and send 10,500 usage events; assert that inference requests complete in `< 1 ms`, the oldest 500 events drop cleanly, and `llmroller_usage_dropped_total == 500` (`REL-3`), and (2) verify that every committed batch updates `usage_rollups_hourly` in the same SQLite transaction. |
| Atomic commits | 1. `feat(store/4.3): implement batching SQLite usage writer, hourly rollups, and non-blocking overflow ring [Spec: USG-1, REL-2, REL-3]` |
| Risks and resolutions | **Risk:** On `SIGTERM`, buffered events in the `mpsc` channel could be lost if the process exits immediately. **Resolution:** During the `SIGTERM` drain phase (`REL-1`), close the inbound listener first, wait for active streams to finish, and flush all remaining items in the usage channel before closing the SQLite pool. |

## 4.4 Sub-phase 4.4: Rolling (`5h`, `7d`) and anchored (`1mo`) budget window ring counters

This sub-phase implements the in-memory rolling ring counters and anchored monthly accumulators (`SRDD Section 13.2`). It rebuilds window state from `usage_events` and `budget_adjustments` at startup and enforces the 2% admission reserve.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.9`, `Section 13.2`; Requirements `LIM-1`, `RTE-3` |
| Crates touched | `crates/core::windows`, `crates/store::budgets` |
| TDD step (`Red -> Green`) | Write deterministic clock-driven tests (`core::windows::tests`, `#[doc = "Spec: LIM-1, RTE-3"]`) using `FakeClock`: advance time minute-by-minute across a 5-hour rolling window and a 30-day anchored window, asserting that buckets expire at the exact millisecond boundary, that routes are excluded at `98%` spend (`reserve_pct = 2`), and that restarting the store reconstructs identical bucket sums from SQLite. |
| Atomic commits | 1. `feat(core/4.4): implement rolling bucket ring counters, anchored windows, and admission reserve checks [Spec: LIM-1, RTE-3]` |
| Risks and resolutions | **Risk:** High concurrency allows several new requests to pass admission simultaneously when a window is at `97.9%`. **Resolution:** Increment an optimistic in-flight reservation counter (`reserved_in_flight_micro_usd`, default `$0.05` per active stream) while the stream is open and replace it with actual cost when the stream finishes. |

## 4.5 Sub-phase 4.5: Source provider `/usage` poller, auto-reconciliation, and `/v1/usage` pass-through

This sub-phase implements upstream `/usage` polling and automatic `budget_adjustments` reconciliation (`USG-4`..`USG-6`). It also serves `/v1/usage`, `/v1/key`, and `/v1/usage/raw` with PII sanitization (`USG-7`, `USG-8`, `SEC-18`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.4`, `Section 6.9`, `Section 8.2`, `Section 13.3`, `Section 13.4`; Requirements `USG-4`..`USG-8`, `SEC-18` |
| Crates touched | `crates/proto::usage_poller`, `crates/store::usage_snapshots`, `crates/proxy::usage_passthrough`, `fixtures/usage/*` |
| TDD step (`Red -> Green`) | Write golden parser and integration tests (`tests/integration_usage_and_passthrough.rs`, `#[doc = "Spec: USG-4, USG-5, USG-6, USG-7, USG-8, SEC-18"]`): (1) replay `fixtures/usage/opencode_go.json`, `command_code_credits.json`, and `openrouter_key.json`, (2) assert that when OpenCode Go reports `percent: 25` and `resetsAt` while local spend is `10%`, LLMRoller inserts an automatic `upstream_usage_sync` adjustment so the window gauge reads `25%` (`USG-6`), (3) assert `GET /v1/usage` returns raw `source_provider_usage` for a user's own personal key, and (4) assert `GET /v1/usage` on a shared or global key strips `email`, `whoami`, `user_id`, and `workspace_id` (`USG-8`, `SEC-18`). |
| Atomic commits | 1. `feat(proto/4.5): implement source provider /usage parsers and PII sanitizer [Spec: USG-4, USG-5, SEC-18]` 2. `feat(core/4.5): implement automatic window reset and spend reconciliation from /usage snapshots [Spec: USG-6]` 3. `feat(proxy/4.5): implement GET /v1/usage, GET /v1/key, and GET /v1/usage/raw pass-through endpoints [Spec: USG-7, USG-8]` |
| Risks and resolutions | **Risk:** Command Code's `/alpha/billing/credits` endpoint changes a field name or Open Design AMR's `/v1/usage` returns 404 ([open-design#8674](https://github.com/nexu-io/open-design/issues/8674)). **Resolution:** Keep `raw_json` intact in `upstream_usage_snapshots`, treat missing normalized fields as `None` (falling back cleanly to local window counters without failing key health), and surface a non-fatal schema notice in the admin UI (`R2`). |
