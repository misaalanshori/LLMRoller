# Epic 5: Opt-in content traceability mode and read-only session chat preview

Epic 5 builds the admin opt-in content traceability mode (`TRC-1`..`TRC-8`) and non-blocking SSE delta reassembly. It also implements `zstd` + `XChaCha20-Poly1305` trace storage and the multi-turn Session Chat Preview reconstructor (`SRDD Section 6.10, Section 12.6`).

## 5.1 Sub-phase 5.1: Admin opt-in `content_traceability_mode` configuration and scope matching

This sub-phase implements the default-off `content_traceability_mode` setting, scope filters, and the 14-day retention TTL (`TRC-1`, `TRC-7`). Every change to traceability settings writes a hash-chained `audit_log` entry (`AUD-1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 6.10`, `Section 6.11`; Requirements `TRC-1`, `TRC-7`, `AUD-1` |
| Crates touched | `crates/core::trace_config`, `crates/store::settings`, `crates/admin::trace_settings` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`core::trace_config::tests`, `#[doc = "Spec: TRC-1, TRC-7, AUD-1"]`) verifying: (1) fresh instances have `content_traceability_mode = Disabled`, (2) standard `user` callers receive HTTP 403 on `PUT /admin/api/settings/traceability`, (3) scoping traceability to `users: [alice_id]` records traces for Alice's client keys while recording zero trace rows for Bob's client keys, and (4) toggling the setting appends a hash-chained `audit_log` row. |
| Atomic commits | 1. `feat(core/5.1): implement default-off TraceabilityConfig and scope matcher in ConfigSnapshot [Spec: TRC-1, TRC-7]` 2. `feat(admin/5.1): implement PUT /admin/api/settings/traceability with audit logging [Spec: TRC-1, AUD-1]` |
| Risks and resolutions | **Risk:** An admin enables content traceability for debugging and forgets to turn it off. **Resolution:** Support an optional `auto_disable_after_hours` timer in `TraceabilityConfig` and display a persistent top banner in the web UI whenever content traceability mode is active (`UI-4`). |

## 5.2 Sub-phase 5.2: Non-blocking request capture and SSE delta reassembler (`TraceTap`)

This sub-phase extends `crates/proto::trace_tap` to capture inbound prompts and tools (`TRC-2`). It reassembles streamed text, reasoning, and tool-call deltas across `chat`, `responses`, and `messages` into a `NormalizedTurnTrace` without slowing chunk forwarding (`TRC-3`, `PERF-5`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.10`, `Section 7.1`, `Section 12.6`; Requirements `TRC-2`, `TRC-3`, `PERF-2`, `PERF-5` |
| Crates touched | `crates/proto::trace_tap` |
| TDD step (`Red -> Green`) | Write golden SSE reassembly tests (`proto::trace_tap::tests`, `#[doc = "Spec: TRC-2, TRC-3, PERF-5"]`) replaying multi-chunk streams across `chat`, `responses`, and `messages` that interleave extended thinking/reasoning blocks, parallel tool calls with fragmented JSON arguments, and markdown text. Assert that `TraceTap::finish()` produces the exact reconstructed `reasoning_text`, `ToolCall` structs with valid JSON arguments, and `assistant_text`, and truncates turns exceeding `4 MiB` with `truncated_content = 1` (`PERF-5`). |
| Atomic commits | 1. `feat(proto/5.2): implement non-blocking TraceTap and SSE delta reassembler for chat, responses, and messages [Spec: TRC-2, TRC-3, PERF-5]` |
| Risks and resolutions | **Risk:** A streaming tool call emits malformed partial JSON when the client disconnects mid-stream. **Resolution:** Store tool-call argument fragments in a `String` buffer during streaming and parse JSON only at block completion; if the stream aborts early, preserve the raw partial argument string with `incomplete = 1` so the trace preview still shows what arrived before disconnect. |

## 5.3 Sub-phase 5.3: Background `zstd` compression, `XChaCha20-Poly1305` encryption, and TTL purge

This sub-phase implements background `zstd` compression and `XChaCha20-Poly1305` encryption for `trace_records` (`TRC-4`, `SEC-19`). It also adds on-demand trace purge endpoints and the 14-day retention sweeper (`TRC-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.10`, `Section 8.2`, `Section 9.1`, `Section 9.2`; Requirements `TRC-4`, `TRC-7`, `PERF-5`, `SEC-19` |
| Crates touched | `crates/store::traces`, `crates/server::jobs` |
| TDD step (`Red -> Green`) | Write cryptographic and storage tests (`store::traces::tests`, `#[doc = "Spec: TRC-4, TRC-7, SEC-19"]`) verifying: (1) raw SQLite inspection of `trace_records.payload_enc` contains zero plaintext substrings of the prompt or completion, (2) tampering with `owner_user_id` or `session_hash` on a `trace_records` row causes AEAD decryption to fail (`SEC-19`), and (3) `DELETE /admin/api/sessions/{session_hash}/trace` deletes all `trace_records` for that session while leaving `usage_events` token and cost rows intact (`TRC-7`). |
| Atomic commits | 1. `sec(store/5.3): implement zstd compression and XChaCha20-Poly1305 encryption for trace_records [Spec: TRC-4, PERF-5, SEC-19]` 2. `feat(store/5.3): implement trace retention sweeper and selective session trace purge [Spec: TRC-7]` |
| Risks and resolutions | **Risk 1:** `zstd` compression on large prompts competes with Tokio HTTP worker threads. **Resolution 1:** Run `zstd::encode_all` (level 3) and `XChaCha20-Poly1305` sealing on the background `spawn_blocking` pool (2 threads max, `PERF-5`). **Risk 2:** `trace_records.usage_event_id` references `usage_events(id)` under `PRAGMA foreign_keys = ON`, so inserting a trace before its parent usage event commits would fail with `SQLITE_CONSTRAINT_FOREIGNKEY`. **Resolution 2:** Send the sealed `TraceRecord` into the single SQLite batch writer (`SRDD Section 9.2`), which commits `(1) usage_events -> (2) usage_rollups_hourly -> (3) trace_records` in one atomic transaction. |

## 5.4 Sub-phase 5.4: Multi-turn session chat timeline reconstructor and privacy-isolated preview API

This sub-phase implements `crates/core::chat_preview` (`SRDD Section 12.6`) to reconstruct multi-turn conversations per `session_hash`. It deduplicates stateless message prefixes, marks context compaction, sanitizes HTML via `ammonia`, and serves the read-only chat preview APIs (`TRC-5`, `TRC-8`, `SEC-19`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 4.6`, `Section 6.10`, `Section 8.2`, `Section 12.6`, `Section 14.2`; Requirements `TRC-5`, `TRC-6`, `TRC-8`, `SEC-17`, `SEC-19` |
| Crates touched | `crates/core::chat_preview`, `crates/admin::traces` |
| TDD step (`Red -> Green`) | Write unit, privacy, and XSS security tests (`tests/integration_content_traceability.rs`, `#[doc = "Spec: TRC-5, TRC-6, TRC-8, SEC-17, SEC-19"]`): (1) feed a 5-turn stateless coding session where each request resends prior messages plus new tool results; assert the reconstructed timeline emits each user prompt, reasoning block, tool call, tool result, and assistant message **once** in chronological order, (2) simulate mid-session context compaction on turn 4 and assert a `context_compacted_or_branched` marker appears, (3) assert that when Grantee Bob routes through Grantor Alice's shared personal key, Alice receives HTTP 403/404 when querying Bob's `session_hash` chat preview (`TRC-8`), and (4) inject `<script>alert(1)</script><img src=x onerror=alert(1)>` into a recorded prompt and completion and assert the rendered HTML preview strips all active scripts and event handlers (`SEC-19`). |
| Atomic commits | 1. `feat(core/5.4): implement multi-turn session chat timeline reconstructor with prefix deduplication and compaction detection [Spec: TRC-5]` 2. `sec(admin/5.4): implement RBAC-isolated session chat preview endpoints with ammonia XSS sanitization [Spec: TRC-6, TRC-8, SEC-17, SEC-19]` |
| Risks and resolutions | **Risk:** A long coding session has 150 turns, making a single chat preview payload large. **Resolution:** Support cursor pagination (`?from_turn=1&limit=50`) on `/sessions/{session_hash}/chat` and lazy-load raw request/response JSON per turn via `GET /api/user/traces/{request_id}`. |
