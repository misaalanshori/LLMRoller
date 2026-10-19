# Epic 2: Transparent wire proxy, protocol normalizer, and provider adapters

Epic 2 builds the data-plane HTTP proxy (`:8080`), client key authentication, and SSRF-safe outbound transport. It also adds the surgical JSON editor, the unbuffered SSE tee, and the provider adapters. It corresponds to Milestone `M1` (`SRDD Sections 2, 5, 6.3, 6.5, 6.8, 7.1, 7.3, 8.2, 12`).

## 2.1 Sub-phase 2.1: Global and personal provider records and SSRF-safe DNS connector

This sub-phase implements storage for `global` and `personal` providers (`PRV-1`..`PRV-5`, `PRV-8`). It also adds the custom `reqwest` DNS resolver that blocks private, loopback, and link-local IPs (`PRV-7`, `SEC-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.3`, `Section 6.3`, `Section 8.2`; Requirements `PRV-1`..`PRV-8`, `SEC-7` |
| Crates touched | `crates/store::providers`, `crates/proxy::ssrf`, `crates/proxy::transport` |
| TDD step (`Red -> Green`) | Write SSRF unit and integration tests (`proxy::ssrf::tests::ip_ranges`, `#[doc = "Spec: PRV-7, SEC-7"]`) testing IPv4 and IPv6 loopback (`127.0.0.1`, `::1`), cloud metadata (`169.254.169.254`), RFC 1918 (`10.0.0.1`, `172.16.0.1`, `192.168.1.1`), CGNAT (`100.64.0.1`), IPv4-mapped IPv6 (`::ffff:127.0.0.1`), HTTP (`http://`) rejection, and HTTP 301/302/307/308 redirect rejection. Verify that `allow_private_network = 1` (settable only by `admin`) permits local model servers when explicitly enabled. |
| Atomic commits | 1. `sec(proxy/2.1): implement SSRF-filtering DNS resolver and no-redirect reqwest client [Spec: PRV-7, SEC-7]` 2. `feat(store/2.1): implement global and personal provider and upstream_key repositories [Spec: PRV-1, PRV-2, PRV-3, PRV-4, PRV-8]` |
| Risks and resolutions | **Risk:** DNS rebinding attacks where a hostname resolves to a public IP during config validation and `127.0.0.1` at request time. **Resolution:** Enforce the IP-range check inside `reqwest`'s `Resolve` trait on **every socket connect**, not just when the provider URL is saved in the UI. |

## 2.2 Sub-phase 2.2: Client key (`llr_...`) issuance, constant-time auth, and rate/CIDR guards

This sub-phase implements client key generation (`llr_` + 32 base62 bytes), SHA-256 hashing, and constant-time lookup in `ConfigSnapshot`. It also enforces per-IP failure throttling, RPM token buckets, and CIDR allow lists (`SRDD Section 6.5, Section 8.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.5`, `Section 8.2`; Requirements `CLK-1`..`CLK-5`, `API-2`, `SEC-3`, `SEC-12` |
| Crates touched | `crates/core::client_key`, `crates/store::client_keys`, `crates/proxy::auth` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`tests/integration_client_keys.rs`, `#[doc = "Spec: CLK-1, CLK-2, CLK-3, CLK-4, CLK-5, SEC-3, SEC-12"]`) verifying: (1) raw token is returned only on creation/rotation, (2) both `Authorization: Bearer llr_...` and `x-api-key: llr_...` authenticate, (3) revoking a key rejects calls within 1 s (`CLK-4`), (4) CIDR mismatches return 403, and (5) 21 failed bearer requests from one IP within 1 minute return HTTP 429 (`SEC-12`). |
| Atomic commits | 1. `feat(core/2.2): implement client key generation, constant-time verification, CIDR and RPM guards [Spec: CLK-1, CLK-2, CLK-3, CLK-5, SEC-3]` 2. `feat(proxy/2.2): wire client key auth middleware and IP failure rate limiter [Spec: CLK-4, API-2, SEC-12]` |
| Risks and resolutions | **Risk:** Updating `client_keys.last_used_at` synchronously on every request would hit SQLite on the hot path. **Resolution:** Record `last_used_at` in an in-memory `DashMap<ClientKeyId, i64>` and flush dirty timestamps to SQLite inside the 250 ms batch writer task. |

## 2.3 Sub-phase 2.3: Four-rule header pipeline, smuggling guards, and `User-Agent` policy

This sub-phase implements request smuggling rejection (`SEC-15`), hop-by-hop and credential stripping (`API-2`), and agent header passthrough (`API-3`). It also implements the `pass`, `label`, and `reject` `User-Agent` policies (`SRDD Section 12.1, Section 12.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 12.1`, `Section 12.2`, `Section 8.2`; Requirements `API-2`, `API-3`, `SEC-6`, `SEC-8`, `SEC-15` |
| Crates touched | `crates/proto::headers`, `crates/proxy::edge` |
| TDD step (`Red -> Green`) | Write table-driven tests (`proto::headers::tests`, `#[doc = "Spec: API-2, API-3, SEC-8, SEC-15"]`) asserting that `x-claude-code-session-id`, `x-claude-code-agent-id`, `anthropic-beta`, `openai-beta`, `x-stainless-*`, and `x-cmd-zdr` pass through untouched, while `authorization`, `x-api-key`, `cookie`, `x-forwarded-for`, and `x-llmroller-*` are stripped and replaced with the upstream credential. Verify that requests with both `Content-Length` and `Transfer-Encoding` or duplicate `Host` headers are rejected with HTTP 400 (`SEC-15`). |
| Atomic commits | 1. `sec(proto/2.3): implement HTTP smuggling checks and four-rule header pipeline [Spec: API-2, API-3, SEC-8, SEC-15]` 2. `feat(proto/2.3): implement pass, label, and reject User-Agent policies [Spec: API-3]` |
| Risks and resolutions | **Risk:** An upstream provider rejects a new experimental `anthropic-beta` flag sent by Claude Code (`SRDD Section 2.6 F7`). **Resolution:** Support glob patterns in `provider.config.strip_headers` (or comma-separated token filtering inside `anthropic-beta`) so operators can strip one beta token without losing other headers. |

## 2.4 Sub-phase 2.4: Zero-copy surgical JSON span editor

This sub-phase implements the top-level JSON span scanner in `crates/proto::body_edit` (`SRDD Section 12.3`). It rewrites `model` and injects session or `include_usage` fields while keeping all other bytes identical.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 12.3`, `Section 7.3`; Requirements `API-4`, `TRN-1`, `TRN-2` |
| Crates touched | `crates/proto::body_edit` |
| TDD step (`Red -> Green`) | Write property-based tests (`proto::body_edit::proptests`, `#[doc = "Spec: API-4, TRN-1, TRN-2"]`) generating arbitrary nested JSON requests with weird whitespace, escaped Unicode, and specific key orders in `tools` and `messages`. Assert that: (1) when no edit is needed, the output `Bytes` pointer is identical to the input `Bytes` (`TRN-1`), and (2) when `model` or `include_usage` is edited, slicing out the `messages`, `tools`, and `system` byte ranges from the output yields exact byte equality with the input (`TRN-2`). |
| Atomic commits | 1. `feat(proto/2.4): implement zero-copy top-level JSON span scanner and surgical editor [Spec: API-4, TRN-1, TRN-2]` |
| Risks and resolutions | **Risk:** Full DOM deserialization (`serde_json::Value`) would both permute object keys (breaking upstream prefix caches) and allocate tens of megabytes on 32 MiB prompts. **Resolution:** Use a custom streaming lexer or `serde_json::value::RawValue` top-level slice so nested arrays and objects are never parsed into DOM nodes on the forwarding path. |

## 2.5 Sub-phase 2.5: Unbuffered SSE streaming tee, `include_usage` suppression, and disconnect abort

This sub-phase implements the zero-buffering chunk forwarder (`PERF-2`, `API-6`) and suppresses injected empty-choices usage chunks (`API-5`). It also handles compressed streams and aborts upstream calls within 1 s on client disconnect (`API-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 12.4`, `Section 7.1`, `Section 7.3`; Requirements `API-5`, `API-6`, `API-7`, `PERF-2`, `TRN-3` |
| Crates touched | `crates/proxy::forward`, `crates/proto::sse_frame` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/integration_wire_transparency.rs`, `#[doc = "Spec: API-5, API-6, API-7, PERF-2, TRN-3"]`) using a local stub server that emits 5 SSE chunks spaced 50 ms apart: (1) assert each chunk arrives at the client within 1 ms of emission (`PERF-2`), (2) assert that an injected `include_usage` chunk (`"choices":[]`) is captured by the tap and omitted from the client stream when the client did not ask for it (`API-5`), and (3) drop the client TCP connection after chunk 2 and assert the stub server observes socket closure within 100 ms (`API-7`). |
| Atomic commits | 1. `feat(proxy/2.5): implement unbuffered SSE stream forwarder and injected usage chunk filter [Spec: API-5, API-6, PERF-2, TRN-3]` 2. `feat(proxy/2.5): wire client disconnect drop guard to abort upstream request within 1s [Spec: API-7]` |
| Risks and resolutions | **Risk:** An SSE frame (`data: ...

`) splits across two TCP `Bytes` chunks right in the middle of the final usage event. **Resolution:** For standard pass-through chunks (`strip_injected_usage_chunk == false`), forward raw `Bytes` immediately without waiting for `

` while feeding a line-reassembling ring buffer inside the asynchronous `SseUsageTap`. When `strip_injected_usage_chunk == true`, hold only an incomplete trailing line at the very end of the stream (after `finish_reason` has already been flushed). |

## 2.6 Sub-phase 2.6: Built-in `ProviderAdapter` implementations and preset TOML files

This sub-phase implements the `ProviderAdapter` trait (`MNT-2`) and ships the four built-in provider presets (`opencode_go.toml`, `command_code.toml`, `open_design_amr.toml`, `openrouter.toml`) plus the `custom` adapter (`SRDD Section 2.3, Section 11.2, Section 15.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.3`, `Section 6.3`, `Section 7.4`, `Section 11.2`; Requirements `PRV-2`, `PRV-3`, `PRV-6`, `API-1`, `API-8`, `MNT-2` |
| Crates touched | `crates/proto::adapters`, `presets/*.toml` |
| TDD step (`Red -> Green`) | Write adapter contract tests (`proto::adapters::tests`, `#[doc = "Spec: PRV-2, PRV-3, PRV-6, API-8, MNT-2"]`) verifying URL construction, auth header injection (`Bearer` vs. `x-api-key`), outbound session hint injection (`x-opencode-session`, `x-session-id`, `x-session-affinity`, `prompt_cache_key`), and upstream error classification (401/403 auth vs. 403 `upgrade_required` on Command Code vs. 429 rate limit vs. 400 client error). |
| Atomic commits | 1. `feat(proto/2.6): implement ProviderAdapter trait and built-in presets for OpenCode Go, Command Code, Open Design AMR, and OpenRouter [Spec: PRV-2, PRV-3, PRV-6, API-8, MNT-2]` |
| Risks and resolutions | **Risk:** OpenCode Go changes which models sit behind `/chat/completions` vs. `/messages` (`SRDD Section 2.3`). **Resolution:** Store supported protocols per route in `model_routes.protocols` discovered from `/models` or configured in the preset, never hard-coded in Rust branches. |
