# Epic 4: Usage taps, three-queue batching writer, 5-minute & hourly rollups, orchestrator v3, and retention

Epic 4 builds the inline SSE and JSON token extractors (`4.1`), the price-book APIs (`4.2`), and the three-queue batching SQLite writer (`4.3`). It also builds `/v1/*` orchestrator v3 (`4.6`) and the retention sweeper and WAL checkpoint supervisor (`4.7`). Pure-core pricing, window rings, and monthly accumulators were built in Sub-phases `3.0a`–`3.0c`, and startup window reconstruction plus source `/usage` polling live in `big-bang-phases-4b.md`. Epic 4 corresponds to Milestone `M3` (`SRDD Sections 2.5, 6.8, 6.9, 7.2, 9, 12.5, 13.1, 21`).

## 4.1 Sub-phase 4.1: Inline `SseUsageTap`, `JsonUsageExtractor`, unified cache subtraction, and `sse_tap` fuzzer

This sub-phase extracts token counts across `chat`, `responses`, and `messages` inline in `poll_next` (`SRDD Section 12.4.1, Section 12.5`) and subtracts `cache_read` and `cache_write` (`USG-2`). It also verifies injected usage chunk capture (`API-5`), accumulates decoded output text for aborted streams (`USG-3`), and adds the `sse_tap` fuzzer (`USG-1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.5`, `Section 6.8`, `Section 6.9`, `Section 12.4.1`, `Section 12.5`; Requirements `API-5`, `USG-1`, `USG-2` |
| Crates touched | `crates/proto::sse_tap`, `crates/proto::json_usage`, `fixtures/sse/*`, `fuzz/fuzz_targets/sse_tap.rs` |
| TDD step (`Red -> Green`) | Write golden fixture and fuzz tests (`proto::sse_tap::golden_tests`, `#[doc = "Spec: API-5, USG-1, USG-2"]`) replaying recorded streams for OpenAI `chat`, OpenAI `responses` (`response.created`, `response.completed`, `response.incomplete`, `response.failed`), Anthropic `messages` (`message_start`, `message_delta`, `message_stop`), OpenRouter `usage.cost`, and truncated streams: (1) assert `input_uncached = prompt_cache_miss_tokens` when present, else `max(0, prompt_tokens - cache_read - cache_write)` on `chat` and `max(0, input_tokens - cache_read - cache_write)` on `responses`, while `messages` uses the last observed `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, and `output_tokens` across `message_start` and `message_delta`, (2) assert that an injected `include_usage` chunk (`"choices":[]`) is captured by `SseUsageTap` and omitted from the client stream (`API-5`), and (3) assert that an aborted stream with `10 KiB` of decoded delta text estimates `2,500` output tokens (`ceil(decoded_text_bytes / 4)`). Run `fuzz/fuzz_targets/sse_tap.rs` in CI. |
| Atomic commits | 1. `feat(proto/4.1): implement inline SseUsageTap and JsonUsageExtractor with unified cache_read/cache_write subtraction and sse_tap fuzzer [Spec: API-5, USG-1, USG-2]` |
| Risks and resolutions | **Risk:** Subtracting only `cached_tokens` on OpenRouter-routed Anthropic `chat` calls double-charges `cache_write_tokens`, and estimating aborted output from raw SSE framing inflates token counts by 3–5x. **Resolution:** Subtract both `cache_read` and `cache_write` on `chat` and `responses`, and estimate aborted output strictly from `decoded_text_bytes` accumulated from content, reasoning, and tool-argument deltas (`SRDD Section 12.5, Section 13.1`). |

## 4.2 Sub-phase 4.2: Body-based price book APIs and opt-in `models.dev` price catalog sync

This sub-phase connects the `crates/core::pricing` engine (`3.0a`) to the `prices` table and builds the opt-in `models.dev` catalog importer (`USG-3`). It also implements the body-based price upsert endpoints (`PUT /admin/api/prices` and `PUT /api/user/providers/{id}/prices`) supporting `/` in `upstream_model_id` (`SRDD Section 14.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.9`, `Section 9.1`, `Section 13.1`, `Section 14.2.1`, `Section 14.2.2`; Requirements `USG-3`, `Partial: PRIV-1` |
| Crates touched | `crates/store::prices`, `crates/admin::prices` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/integration_price_book.rs`, `#[doc = "Spec: USG-3, Partial: PRIV-1"]`) verifying: (1) `PUT /admin/api/prices` and `PUT /api/user/providers/{id}/prices` accept vendor-prefixed IDs containing `/` (such as `deepseek/deepseek-v4.1-flash`) in the JSON body and work behind a reverse proxy that decodes `%2F`, (2) `ConfigSnapshot` keys prices by `(provider_id_or_empty, provider_kind, upstream_model_id)` and resolves provider-specific overrides before preset defaults `''`, (3) overlapping tiers or overlapping UTC schedule minute ranges are rejected with HTTP 422 on write, and (4) `POST /admin/api/prices/sync` fetches `price_sync_catalog_url` only when enabled and inserts suggested aliases with `confirmed = 0`. |
| Atomic commits | 1. `feat(admin/4.2): implement body-based price book endpoints, provider-specific price lookup, and opt-in models.dev importer [Spec: USG-3, Partial: PRIV-1]` |
| Risks and resolutions | **Risk:** Placing `upstream_model_id` (`deepseek/deepseek-v4.1-flash`) in a URL path segment fails routing in `axum` or when reverse proxies decode `%2F`. **Resolution:** Pass `upstream_model_id` in the JSON request body on both admin and user price endpoints (`SRDD Section 14.2`). |

## 4.3 Sub-phase 4.3: Three-queue batching SQLite writer (`durable_state`, `rollups`, `raw_events`), `usage_attempts`, and `usage_rollups_5min`

This sub-phase implements the three-queue data-plane writer (`durable_state`, `rollups`, and `raw_events` `Mutex<VecDeque<RawItem>>` dropping oldest). It commits `usage_events`, `usage_attempts`, `usage_rollups_5min`, and `usage_rollups_hourly` (with `''`/`'none'` sentinels for pre-route rejections) and verifies `REL-2` and `REL-3` (`USG-1`, `SHR-6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.2`, `Section 9.1`, `Section 9.2`, `Section 21.6`; Requirements `USG-1`, `SHR-6`, `REL-2`, `REL-3`, `Partial: TRC-9` |
| Crates touched | `crates/store::usage_writer`, `crates/store::rollups` |
| TDD step (`Red -> Green`) | Write fault-injection and crash-recovery tests (`tests/integration_usage_writer.rs`, `#[doc = "Spec: USG-1, SHR-6, REL-2, REL-3, Partial: TRC-9"]`): (1) pause the SQLite writer task and push 10,500 `UsageEvent` items; assert that inference completes in `< 1 ms`, the `Mutex<VecDeque<RawItem>>` evicts the **oldest** 500 raw events (`llmroller_usage_dropped_total{kind="usage_event"} == 500`), and 100% of all 10,500 token and cost totals are preserved in `usage_rollups_5min` and `usage_rollups_hourly` once the writer resumes (`REL-3`), (2) pre-route rejections (`503 no_eligible_route`) fold into rollups using `upstream_key_id = ''`, `provider_id = ''`, and `provider_scope = 'none'`, while `/v1/models`, `/v1/usage`, and `/v1/key` emit metrics only and zero `usage_events` rows, (3) failed upstream tries record `usage_attempts` rows (`attempt_no 1..3`) in the same batch, and (4) confirm a pin, response link, health write, and rollup delta, wait `1.2 s`, send `SIGKILL` (`kill -9`), restart, and assert zero lost rows (`REL-2`). |
| Atomic commits | 1. `feat(store/4.3): implement three-queue batching writer, usage_attempts, 5-minute and hourly rollups, and SIGKILL recovery [Spec: USG-1, SHR-6, REL-2, REL-3]` |
| Risks and resolutions | **Risk:** A standard `tokio::sync::mpsc` channel cannot drop its oldest item on overflow and would either block inference or drop the newest item. **Resolution:** Use a `Mutex<VecDeque<RawItem>>` (capped at `10,000` items and `64 MiB`) that pops the oldest item after its cost has already been folded into the non-dropping `rollups` map (`SRDD Section 21.6.1`). |

## 4.6 Sub-phase 4.6: `/v1/*` orchestrator v3 (live usage accounting, attempt charging, and reservation finalization)

This sub-phase wires live `UsageEvent` and `usage_attempts` emission plus conservative `estimated_input` charging into `/v1/*` orchestrator v3. It also wires `Reservation` finalization in `RequestLifecycle::drop` and threshold `/usage` poll triggers (`SRDD Section 5.3, Section 13.2.1, Section 21.1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.3`, `Section 6.9`, `Section 10.5.2`, `Section 13.2.1`, `Section 21.1`; Requirements `USG-1`..`USG-3`, `LIM-1` |
| Crates touched | `crates/proxy::orchestrator`, `crates/proxy::lifecycle`, `tests/integration_usage_writer.rs` |
| TDD step (`Red -> Green`) | Write end-to-end accounting tests (`tests/integration_usage_writer.rs`, `#[doc = "Spec: USG-1, USG-2, USG-3, LIM-1"]`) verifying: (1) an upstream try that fails with HTTP 502 after the request body is written records a `usage_attempts` row with `charge_basis = 'estimated_input'` and posts `charged_micro_usd = ceil(estimate_input_tokens(body) * rate_in / 1_000_000)` to that key's windows, share accumulator, and user global accumulator before failing over to key 2, (2) a client disconnecting mid-stream on each protocol (`chat`, `responses`, `messages`) emits exactly one `usage_events` row with `termination = 'client_gone'`, `incomplete = 1`, and non-zero cost (`cost_source = 'estimated_incomplete'`), and (3) `Reservation::drop` posts actual cost to rings/accumulators before decrementing `reserved_in_flight`. Re-run the `2.7` load benchmark. |
| Atomic commits | 1. `feat(proxy/4.6): wire live usage emission, usage_attempts charging, and Reservation finalization into orchestrator v3 [Spec: USG-1, USG-2, USG-3, LIM-1]` |
| Risks and resolutions | **Risk:** Failed upstream tries that consumed prefill tokens (502/504 after body write or opt-in wait timeouts) leave local window and share counters under-counted. **Resolution:** Record every upstream try in `usage_attempts` and charge `estimated_input` for tries in the possibly-consumed class (`SRDD Section 9.1, Section 10.5.2`). |

## 4.7 Sub-phase 4.7: General retention sweeper, WAL checkpoint supervisor, and `SQLITE_FULL` degraded mode

This sub-phase implements the portable ULID-range retention sweeper for `usage_events` (30 days or `50,000,000` rows), `usage_attempts`, `usage_rollups_5min`, `response_links`, `pins`, and `web_sessions`. It also adds the 30 s WAL checkpoint supervisor and `SQLITE_FULL` degraded-mode handling (`SRDD Section 3.3 A6, Section 9.2, Section 21.6.3`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 3.3 (A6)`, `Section 9.2`, `Section 21.6.3`; Requirements `REL-3`, `Partial: PERF-4` |
| Crates touched | `crates/store::retention`, `crates/store::checkpoint`, `crates/server::jobs` |
| TDD step (`Red -> Green`) | Write retention and WAL tests (`tests/integration_usage_writer.rs`, `#[doc = "Spec: REL-3, Partial: PERF-4"]`) verifying: (1) the sweeper deletes expired `usage_events` in 1,000-row chunks using portable `WHERE id IN (SELECT id FROM usage_events WHERE id < :cutoff_ulid ORDER BY id LIMIT 1000)` and enforces `usage_events_max_rows = 50_000_000` followed by `PRAGMA incremental_vacuum(500)`, (2) the checkpoint supervisor runs `wal_checkpoint(PASSIVE)` every 30 s, triggers `wal_checkpoint(RESTART)` between writer batches when the WAL exceeds `256 MiB`, and sets `/readyz` to HTTP 503 when the WAL exceeds `1 GiB`, and (3) simulated `SQLITE_FULL` drops incoming raw events (`llmroller_storage_full = 1`, `/readyz` -> 503) while continuing to retry `durable_state` and `rollups` commits every 5 s. |
| Atomic commits | 1. `feat(store/4.7): implement portable ULID-range retention sweeper, WAL checkpoint supervisor, and SQLITE_FULL handler [Spec: REL-3, Partial: PERF-4]` |
| Risks and resolutions | **Risk:** Long read transactions pin the WAL tail and prevent `PASSIVE` checkpoints from reclaiming space. **Resolution:** Bound all read-pool queries by paging (`<= 5,000` rows or `<= 50` turns per read transaction) and trigger `wal_checkpoint(RESTART)` from the writer between batches when WAL size exceeds `256 MiB` (`SRDD Section 21.6.3`). |
