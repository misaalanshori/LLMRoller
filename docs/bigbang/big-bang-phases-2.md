# Epic 2: Transparent wire proxy, protocol normalizer, provider adapters, and orchestrator v1

Epic 2 builds control-plane CRUD APIs (`2.0`), the SSRF connector (`2.1`), client key auth (`2.2`), header pipelines (`2.3`), and the JSON editor (`2.4`). It also adds the SSE forwarder and `RequestLifecycle` guard (`2.5`), provider adapters (`2.6`), and orchestrator v1 with `SIGTERM` drain (`2.7`). It corresponds to Milestone `M1` (`SRDD Sections 2, 5, 6.3, 6.5, 6.8, 7.1–7.4, 8.2, 12, 21`).

## 2.0 Sub-phase 2.0: Minimal control-plane JSON CRUD for providers, keys, client keys, and settings

This sub-phase exposes the JSON control-plane endpoints for providers, upstream keys, client keys, and runtime settings before UI work in Epic 6. End-to-end tests in Epics 2 through 5 use these endpoints to provision state through the `Control` writer and `Reloader` pipeline (`SRDD Section 14.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.3`, `Section 6.5`, `Section 14.2.1`, `Section 14.2.2`; Requirements `PRV-1`, `PRV-4`, `PRV-5`, `Partial: CLK-1` |
| Crates touched | `crates/store::providers`, `crates/store::client_keys`, `crates/admin::api` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/integration_providers_and_keys.rs`, `#[doc = "Spec: PRV-1, PRV-4, PRV-5, Partial: CLK-1"]`) verifying: (1) `admin` can create global providers and keys while `user` can create personal providers and keys (up to 20 providers and 50 keys per user, `SEC-8`), (2) secrets are sealed with `XChaCha20-Poly1305` (`domain = "upstream_key"`) and never echoed in API responses (`PRV-5`), (3) `providers.share_config_fingerprint` is computed and persisted on every provider insert/update, and (4) every mutation writes an `audit_log` row and updates `ConfigSnapshot` within `1 s`. |
| Atomic commits | 1. `feat(store/2.0): implement provider and upstream_key repositories with share_config_fingerprint and per-user quotas [Spec: PRV-1, PRV-4, PRV-5]` 2. `feat(admin/2.0): add JSON CRUD handlers for providers, upstream keys, client keys, and settings [Spec: PRV-1, PRV-4, PRV-5, Partial: CLK-1]` |
| Risks and resolutions | **Risk:** E2E tests that seed SQLite directly bypass the `Reloader` and `CacheInvalidationEvent` path used in production. **Resolution:** Provision all providers, keys, client keys, and settings through the Sub-phase 2.0 JSON APIs in every integration and E2E test from Epic 2 onward. |

## 2.1 Sub-phase 2.1: Socket-level SSRF `GuardedConnector` and URL validation

This sub-phase implements save-time URL validation and the custom `GuardedConnector` (`tower::Service<Uri>`) that validates both DNS-resolved addresses and IP-literal hosts on every socket connect (`PRV-7`, `SEC-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.3`, `Section 8.2`, `Section 21.8`; Requirements `PRV-7`, `SEC-7` |
| Crates touched | `crates/proxy::ssrf`, `crates/proxy::transport` |
| TDD step (`Red -> Green`) | Write SSRF unit and integration tests (`proxy::ssrf::tests::ip_ranges`, `#[doc = "Spec: PRV-7, SEC-7"]`) verifying that `GuardedConnector` blocks: IPv4/IPv6 literals (`https://127.0.0.1`, `https://[::1]`, `https://169.254.169.254`), RFC 1918 (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`), CGNAT (`100.64.0.0/10`), multicast/reserved (`224.0.0.0/4`, `240.0.0.0/4`, `255.255.255.255/32`), IPv4-mapped IPv6 (`[::ffff:127.0.0.1]`), NAT64 (`64:ff9b::/96`, `64:ff9b:1::/48`), 6to4 (`[2002:a9fe:a9fe::1]` embedding `169.254.169.254`), Teredo/IETF (`2001::/23`, `2001:db8::/32`), discard (`100::/64`), ULA/link-local/site-local/multicast (`fc00::/7`, `fe80::/10`, `fec0::/10`, `ff00::/8`), non-standard integer IP hosts (`https://2130706433` rejected with 422 at save), `http://` schemes, and HTTP 301/302/307/308 redirects. Verify `allow_private_network = 1` (admin-only) and cross-origin `usage_url` rules (`usage_url_cross_origin_ack = true` on unshared personal providers only). |
| Atomic commits | 1. `sec(proxy/2.1): implement socket-level GuardedConnector SSRF filter covering IP literals, 6to4, NAT64, and mapped IPv6 [Spec: PRV-7, SEC-7]` |
| Risks and resolutions | **Risk:** hyper's default connector skips `reqwest::dns::Resolve` when the URL host is an IP literal (`https://127.0.0.1/`). **Resolution:** Enforce `is_blocked(ip)` inside `GuardedConnector::call(uri)` for both `IpAddr` literals and `hickory-resolver` DNS lookups on every socket connect (`SRDD Section 21.8`). |

## 2.2 Sub-phase 2.2: Client key (`llr_...`) auth, rotation grace model, request gates `G1`–`G6`, and listeners

This sub-phase implements client key generation, in-place rotation with `prev_key_hash` and `prev_key_expires_at`, and ordered request gates `G1`–`G6`. It also adds per-IP auth throttling, `auth_failure_counters`, and the `/v1/*`, `/healthz`, `/readyz`, and `/metrics` listeners (`CLK-1`, `CLK-4`, `SEC-3`, `SEC-9`, `SEC-12`, `OBS-1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.4`, `Section 6.5`, `Section 7.4`, `Section 8.2`, `Section 10.0`; Requirements `CLK-1`, `CLK-4`, `API-2`, `SEC-3`, `SEC-9`, `SEC-12`, `OBS-1`, `Partial: CLK-2`, `Partial: CLK-5` |
| Crates touched | `crates/core::client_key`, `crates/store::client_keys`, `crates/proxy::auth`, `crates/server::listeners` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`tests/integration_client_keys.rs`, `#[doc = "Spec: CLK-1, CLK-4, API-2, SEC-3, SEC-9, SEC-12, OBS-1, Partial: CLK-2, Partial: CLK-5"]`) verifying: (1) raw `llr_...` token is returned once, (2) rotating with `grace_period_secs = 300` keeps `prev_key_hash` valid at `+299 s` and rejects it at `+301 s` on `FakeClock`, rotating twice replaces the predecessor, and revoking during grace rejects both hashes within `1 s` (`CLK-4`), (3) ordered gates `G1`–`G6` return deterministic status codes (including 401 when `owner.disabled == 1`), (4) 100,000 bad-token requests are throttled at 20/min per IP (`SEC-12`) and increment `auth_failure_counters` without inserting any `usage_events` rows, (5) non-loopback plaintext data plane logs an error and sets `llmroller_plaintext_data_plane = 1` (or exits `2` when `LLMROLLER_REQUIRE_TLS=true`), and (6) every registered Prometheus metric uses only the closed label allow-list (`provider_kind`, `provider_id`, `protocol`, `endpoint`, `status_class`, `outcome`, `strategy`, `job`, `reason`, `model_bucket`), and `llmroller healthcheck --url http://127.0.0.1:8080/readyz` returns exit code `0` when ready and `1` when `/readyz` is 503. |
| Atomic commits | 1. `feat(core/2.2): implement client key generation, prev_key_hash grace rotation, and request gates G1-G6 [Spec: CLK-1, SEC-3, Partial: CLK-2, Partial: CLK-5]` 2. `feat(proxy/2.2): wire client key auth middleware, auth_failure_counters, and /healthz, /readyz, /metrics listeners [Spec: CLK-4, API-2, SEC-9, SEC-12, OBS-1]` |
| Risks and resolutions | **Risk:** Recording unauthenticated 401 floods in `usage_events` would evict legitimate usage rows from the 10,000-slot raw ring buffer. **Resolution:** Record `G1`–`G3` failures solely in the per-minute `auth_failure_counters` table and enforce `usage_events.client_key_id NOT NULL`. |

## 2.3 Sub-phase 2.3: Request and response header pipelines, smuggling guards, `User-Agent` policy, and header fuzzer

This sub-phase implements HTTP smuggling rejection (`SEC-15`), the four-rule request header pipeline (`API-2`, `API-3`), and the five-step response header pipeline (`Section 12.1.1`). It also implements the `User-Agent` policies and the header parser fuzz target (`SRDD Section 12.1, Section 12.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 12.1`, `Section 12.1.1`, `Section 12.2`, `Section 8.2`; Requirements `API-2`, `API-3`, `SEC-6`, `SEC-8`, `SEC-15` |
| Crates touched | `crates/proto::headers`, `crates/proxy::edge`, `fuzz/fuzz_targets/headers.rs` |
| TDD step (`Red -> Green`) | Write table-driven tests (`proto::headers::tests`, `#[doc = "Spec: API-2, API-3, SEC-6, SEC-8, SEC-15"]`) asserting: (1) on `Own`-tier keys, `x-claude-code-session-id` passes through verbatim and multiple `anthropic-beta` headers preserve their per-name relative order, while `strip_header_tokens` removes configured comma-separated tokens, (2) on `Shared` and `Global` tiers, response pipeline step `R4` strips `openai-organization`, `openai-project`, `anthropic-organization-id`, `x-ratelimit-*`, `anthropic-ratelimit-*`, `cf-ray`, `server`, and `via` while step `R2` removes `content-length`, `content-encoding`, and `set-cookie*`, and (3) requests with both `Content-Length` and `Transfer-Encoding` or duplicate `Host` headers return HTTP 400 (`SEC-15`). Run `fuzz/fuzz_targets/headers.rs` for 60 s in CI. |
| Atomic commits | 1. `sec(proto/2.3): implement HTTP smuggling checks, request header pipeline with strip_header_tokens, and response header pipeline R1-R5 [Spec: API-2, API-3, SEC-8, SEC-15]` 2. `feat(proto/2.3): implement User-Agent policies, canary log scrubber, and headers cargo-fuzz target [Spec: API-3, SEC-6]` |
| Risks and resolutions | **Risk:** Forwarding upstream `content-length` after suppressing an injected usage chunk hangs HTTP/1.1 clients, and forwarding `openai-organization` or `x-ratelimit-*` on shared keys leaks grantor identity. **Resolution:** Enforce the five-step response header pipeline (`R1`–`R5`) at response commit (`SRDD Section 12.1.1`). |

## 2.4 Sub-phase 2.4: Zero-copy surgical JSON span editor, duplicate-key guard, and body-edit fuzzer

This sub-phase implements the top-level JSON span scanner and surgical editor in `crates/proto::body_edit` (`SRDD Section 12.3`). It rejects duplicate top-level keys (`400`), validates UTF-8/object envelopes, JSON-escapes inserted strings, and handles all six `usage_reporting.chat_stream` cases (`API-4`, `TRN-1`, `TRN-2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.3`, `Section 12.3`; Requirements `API-4`, `TRN-1`, `TRN-2` |
| Crates touched | `crates/proto::body_edit`, `fuzz/fuzz_targets/body_edit.rs` |
| TDD step (`Red -> Green`) | Write property-based and security tests (`proto::body_edit::proptests`, `#[doc = "Spec: API-4, TRN-1, TRN-2"]`): (1) any request body with a duplicate top-level key (after JSON key unescaping, such as two `"model"` or two `"stream"` keys), a UTF-8 BOM, a non-object top level, trailing non-whitespace bytes, or non-string `model` / non-boolean `stream` returns HTTP 400 before routing, (2) when routing on an `Own`-tier key with no edits needed, the output `Bytes` pointer is identical to the input `Bytes` (`TRN-1`), (3) random `upstream_model_id` strings containing `"` and `\` are JSON-escaped via `serde_json::to_writer` and inserted members land immediately after `{`, keeping `messages`, `tools`, and `system` byte-identical (`TRN-2`), and (4) explicit `"stream_options": null` or `{"include_usage": false}` (including objects with sibling fields such as OpenAI's `"include_obfuscation": true` or numeric extensions) is replaced in place without creating duplicate keys, while non-boolean `include_usage` or duplicate keys inside `stream_options` return HTTP 400. Run `fuzz/fuzz_targets/body_edit.rs` in CI. |
| Atomic commits | 1. `feat(proto/2.4): implement surgical JSON span editor with duplicate-key rejection, envelope validation, and body_edit fuzzer [Spec: API-4, TRN-1, TRN-2]` |
| Risks and resolutions | **Risk:** A client key restricted to a cheap model sends `{"model":"cheap","model":"expensive"}` to exploit a first-wins vs. last-wins parser differential. **Resolution:** Reject any JSON body containing duplicate top-level keys with HTTP 400 in `scan_top_level` before model resolution. |

## 2.5 Sub-phase 2.5: Unbuffered SSE forwarder, 64 KiB pre-first-frame holding buffer, single-frame filter, and `RequestLifecycle` guard

This sub-phase implements the 64 KiB pre-first-valid-frame holding buffer (`Section 10.5.3`), the zero-buffering SSE forwarder (`PERF-2`, `API-6`), and the 64 KiB usage-chunk filter (`Partial: API-5`). It also adds `async-compression` fallback decoding and the `RequestLifecycle` RAII drop guard (`API-7`, `SRDD Section 12.4, Section 21.1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.1`, `Section 7.3`, `Section 10.5.3`, `Section 12.4`, `Section 21.1`; Requirements `API-6`, `API-7`, `TRN-3`, `Partial: API-5`, `Partial: PERF-2` |
| Crates touched | `crates/proxy::forward`, `crates/proxy::lifecycle`, `crates/proto::sse_frame`, `fuzz/fuzz_targets/sse_frame.rs` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/integration_wire_transparency.rs`, `#[doc = "Spec: API-6, API-7, TRN-3, Partial: API-5, Partial: PERF-2"]`): (1) upstream status and headers are held in a 64 KiB buffer until the first valid non-comment frame is verified free of a non-null error object (explicitly permitting `"error": null` on OpenAI and OpenRouter `/v1/responses` `response.created` frames), then committed through the response header pipeline, (2) each complete SSE frame after commit reaches the client within 1 ms (`Partial: PERF-2`), (3) when `strip_injected_usage_chunk == true`, only a complete frame whose single `data:` JSON has `"choices":[]` and an object `"usage"` is suppressed (`Partial: API-5`) and any partial bytes at stream end flush verbatim, (4) unexpected `content-encoding: gzip|br|deflate|zstd` is decoded transparently in the forwarder, and (5) dropping the client connection after chunk 2 aborts the upstream stream within 100 ms (`API-7`) and invokes `RequestLifecycle::drop` exactly once with `Termination::ClientGone`. |
| Atomic commits | 1. `feat(proxy/2.5): implement pre-first-frame holding buffer, single-frame SSE usage filter, and compression fallback decoder [Spec: API-6, TRN-3, Partial: API-5, Partial: PERF-2]` 2. `feat(proxy/2.5): implement RequestLifecycle RAII completion guard and sse_frame fuzz target [Spec: API-7]` |
| Risks and resolutions | **Risk:** When a client disconnects mid-stream, hyper simply drops the response stream future, leaking permits and skipping usage accounting if finalization is tied to end-of-stream polling. **Resolution:** Own all per-request resources inside `RequestLifecycle` so its `Drop` implementation always finalizes accounting and releases permits (`SRDD Section 21.1`). |

## 2.6 Sub-phase 2.6: Built-in `ProviderAdapter` implementations, 5-class error classifier, and preset TOML files

This sub-phase implements the `ProviderAdapter` trait (`MNT-2`) and the five-class ordered error classifier (`QuotaExhausted`, `RequestRejected`, `PlanRestricted`, `CredentialInvalid`, `Unclassified403`). It also ships the four built-in preset TOML files plus `custom` (`SRDD Section 2.3, Section 10.5.2, Section 11.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.3`, `Section 6.3`, `Section 7.4`, `Section 10.5.2`, `Section 11.2`; Requirements `PRV-2`, `PRV-3`, `MNT-2`, `Partial: PRV-6`, `Partial: API-8` |
| Crates touched | `crates/proto::adapters`, `presets/*.toml` |
| TDD step (`Red -> Green`) | Write adapter contract tests (`proto::adapters::tests`, `#[doc = "Spec: PRV-2, PRV-3, MNT-2, Partial: PRV-6, Partial: API-8"]`) verifying: (1) URL construction and `aux_endpoints` capability declarations (`count_tokens`, `responses_compact`), (2) preset `[aliases]` (`claude-sonnet-4-6 -> claude-sonnet-4.6`) and `[protocol_defaults]` glob mappings, (3) `usage_reporting.chat_stream` (`inject` vs. `native`), (4) Stage-1 `/models` key testing (`Partial: PRV-6`), and (5) ordered five-class error classification over the first 8 KiB of response bodies: `QuotaExhausted` (402/403/429 matching quota substrings), `RequestRejected` (OpenRouter 403 `"flagged"`/`"moderation"` and 400/413/422 passed through without health change), `PlanRestricted` (Command Code 403 `"upgrade_required"` blocking `(K, M, P)`), `CredentialInvalid` (401 or 403 `"invalid_api_key"`), and `Unclassified403`. |
| Atomic commits | 1. `feat(proto/2.6): implement ProviderAdapter trait, 5-class error classifier, and preset TOML files [Spec: PRV-2, PRV-3, MNT-2, Partial: PRV-6, Partial: API-8]` |
| Risks and resolutions | **Risk:** A content-moderation 403 from an aggregator or a plan-restriction 403 on one protocol marks a shared or global key `invalid` for all users. **Resolution:** Classify `RequestRejected` and `PlanRestricted` before `CredentialInvalid` so content rejections never touch key health and plan restrictions only block `(upstream_key_id, canonical_id, protocol)` (`SRDD Section 10.5.2`). |

## 2.7 Sub-phase 2.7: `/v1/*` orchestrator v1, pre-flight body memory budget, graceful `SIGTERM` drain, and early load harness

This sub-phase composes gates `G1`–`G6`, pre-flight body admission (`LLMROLLER_PREFLIGHT_BODY_BUDGET_MB`), single-route selection, and SSE forwarding into `/v1/*` orchestrator v1. It also implements the 8-step `SIGTERM` drain (`REL-1`) and runs the early load harness with a log-normal TTFF stub (`SRDD Section 7.1, Section 21.8`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.3`, `Section 5.5`, `Section 7.1`, `Section 7.2`, `Section 21.1`, `Section 21.8`; Requirements `REL-1`, `SEC-8`, `Partial: PERF-1`, `Partial: PERF-2`, `Partial: PERF-3` |
| Crates touched | `crates/proxy::orchestrator`, `crates/server::shutdown`, `benches/proxy_latency.rs`, `tests/e2e_sigterm_drain.rs` |
| TDD step (`Red -> Green`) | Write end-to-end pass-through, memory-admission, and drain tests (`tests/e2e_sigterm_drain.rs`, `#[doc = "Spec: REL-1, SEC-8, Partial: PERF-1, Partial: PERF-2, Partial: PERF-3"]`): (1) single-provider pass-through succeeds across `/v1/chat/completions`, `/v1/responses`, and `/v1/messages` (streaming and non-streaming), (2) `budget_semaphore.acquire_many(ceil(len / 4096))` queues excess pre-first-byte requests up to `preflight_queue_timeout_ms` (`2,000 ms`), releases the permit the instant the first valid frame commits, and returns `503 gateway_busy` (`Retry-After: 1`) on queue timeout, (3) `SIGTERM` runs the 8-step shutdown sequence (`/readyz -> 503` for 5 s grace, stop accept, 60 s stream drain, `Termination::ShutdownCut` on stragglers, stop jobs, flush queues, `wal_checkpoint(TRUNCATE)`, release lock), and (4) the load benchmark against a log-normal TTFF stub (median 2 s, p95 8 s) verifies sustained req/s across 64 KiB, 512 KiB, and 2 MiB bodies. |
| Atomic commits | 1. `feat(proxy/2.7): implement /v1/* orchestrator v1 and cgroup-aware pre-flight body budget semaphore [Spec: SEC-8, Partial: PERF-1, Partial: PERF-2, Partial: PERF-3]` 2. `feat(server/2.7): implement 8-step graceful SIGTERM drain, job panic supervisor, and log-normal TTFF load harness [Spec: REL-1]` |
| Risks and resolutions | **Risk:** Holding buffered request bodies in a tiny 128 MiB cap with immediate rejection throttles 1 MiB coding requests with 5 s TTFF to ~25 req/s. **Resolution:** Size `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` to `min(1024, floor(0.40 * memory_limit_mb))` (819 MiB on a 2 GB host) with a 2,000 ms admission queue (`SRDD Section 7.1`). |
