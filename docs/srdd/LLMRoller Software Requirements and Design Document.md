<!-- Generated file - edit the numbered parts -->
# LLMRoller: Software Requirements and Design Document

Version 1.3 · 20 October 2026 · Complete software requirements, architecture, pluggable routing strategy engine, `/usage` pass-through, opt-in content traceability and chat preview, multi-role RBAC, concurrency/recovery model, Docker containerization & deployment specification, and TDD specification.

## 0. Introduction

LLMRoller is a self-hosted Rust gateway that pools upstream coding-plan and aggregator API keys behind one client key per user. It pins each conversation session to the upstream key that started it. It also tracks token usage, dollar budgets, and upstream quota windows.

### 0.1 Purpose and scope

This document specifies the software requirements and system design for LLMRoller version 1.0 (dated 19 October 2026). Engineers use this specification to build, test, and run the proxy, router, model catalog, usage pipeline, and web interface.

Sections 1 through 8 define what the system must do, who uses it, and how security boundaries work. Sections 9 through 19 define the data model, internal architecture, algorithms, interfaces, and test governance. Section 20 lists the primary sources verified on 19 October 2026. Section 21 consolidates the concurrency, lifecycle, and failure-recovery model, and Section 22 specifies containerization and deployment.

| In scope for version 1.0 | Out of scope for version 1.0 |
| --- | --- |
| Proxy for OpenAI Chat Completions, OpenAI Responses, and Anthropic Messages | Image generation, audio transcription, and vector embedding endpoints |
| Providers: OpenCode Go, Command Code, Open Design AMR, OpenRouter, and custom OpenAI or Anthropic compatible endpoints | OAuth consumer browser logins for ChatGPT Plus or Claude Pro web accounts |
| Global providers managed by admins and personal providers managed and shared by any user with explicit grantee acceptance | Commercial billing, customer invoicing, or public resale to external third parties |
| Admin and standard user roles with full role-based access control | Multi-node clustering and distributed consensus across hosts |
| Session pinning, response-link inheritance, pluggable routing strategies (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, `power_of_two_choices`), and per-model priority overrides | Automatic cross-protocol request translation in the base release |
| Local cost accounting plus source provider `/usage` polling, decaying observation reconciliation, and pass-through | Unconditional or silent prompt logging (content recording is off by default and strictly opt-in) |
| Admin opt-in content traceability mode with content-addressed encrypted turn storage and read-only session chat preview | Automatic cross-tenant prompt inspection on shared personal keys |
| Single static Rust binary, distroless Docker and Compose deployment profiles, SQLite in WAL mode, and an embedded HTMX web interface | External single-page JavaScript build pipelines or CDN asset dependencies |

### 0.2 Requirement priorities and conventions

Every requirement in this specification carries a unique identifier and a release priority level. Every requirement is written so an automated test can pass or fail it.

| Priority | Keyword | Meaning |
| --- | --- | --- |
| P0 | must | Mandatory for version 1.0. The release cannot ship until all linked tests pass. |
| P1 | should | Expected in version 1.0 unless a documented engineering trade-off defers it to a point release. |
| P2 | may | Planned for a future release. Schema hooks and trait boundaries accommodate it now. |

Requirement identifiers use a short prefix for each subsystem. Functional prefixes include `AUTH`, `USR`, `PRV`, `SHR`, `CLK`, `MDL`, `RTE`, `API`, `USG`, `LIM`, `TRC`, `UI`, and `AUD`. Non-functional requirements use `PERF`, `REL`, `TRN`, `COM`, `POR`, `MNT`, `OBS`, and `PRIV`, while security requirements use `SEC`.

### 0.3 Definitions and glossary

The terms in the table below have one fixed meaning throughout all twenty-three sections of this specification.

| Term | Definition |
| --- | --- |
| Provider | An upstream inference service with defined endpoints, authentication rules, and session behavior, such as OpenCode Go or OpenRouter. |
| Global provider | A provider record created by an admin. Its upstream keys are available to all authorized users on the instance. |
| Personal provider | A provider record created by an individual user or admin for their own account. Only the owner and explicitly selected grantees who have accepted the share can route through it. |
| Provider share | A permission grant from a personal provider owner that allows another specific user on the instance to route through that personal provider or key once accepted by the grantee. |
| Upstream key | One API credential for one provider account. Each upstream key carries a default priority weight, a versioned health state, and budget windows. |
| Client | A coding agent or SDK that calls LLMRoller, such as Claude Code, Codex CLI, opencode, or Hermes. |
| Client key | A credential (`llr_...`) that LLMRoller issues to an admin or standard user. Clients send it to LLMRoller; LLMRoller never forwards it upstream. |
| Session | One multi-turn conversation or agent task identified by a client-supplied session header, body field, inherited response link, or derived opening-message fingerprint. |
| Pin | The stored binding from `(client_key_id, model_pk, session_hash)` to one `upstream_key_id`, `pool_tier`, and monotonic `generation`. |
| Canonical model | The normalized model identifier that LLMRoller exposes to clients across providers, such as `deepseek-v4.1-flash`, backed by an immutable surrogate primary key (`model_pk`). |
| Route | The pairing of one `upstream_key_id` and one `model_pk` (`canonical_id`), holding the raw upstream model name, vendor prefix, lifecycle status, supported protocols, and optional priority override. |
| Candidate | A route that passes all request gates (`G1` through `G7`) and candidate checks (`C1` through `C8`) for the current request. |
| Protocol | One of three wire formats: `chat` (OpenAI Chat Completions), `responses` (OpenAI Responses), or `messages` (Anthropic Messages). |
| Affinity hint | The session identifier field or header that LLMRoller writes into an outbound request according to the target provider's session profile and tier isolation rules. |
| Upstream usage snapshot | The quota, credit, and reset-time record retrieved from a source provider's native usage or key endpoint, feeding a time-bounded `UpstreamObservation`. |
| Content traceability mode | An admin-controlled opt-in setting that records compressed, encrypted request and response contents (prompts, reasoning, tool calls, and completions) using content-addressed blocks (`trace_blocks`) and turn records (`trace_records`). |
| Session chat preview | A read-only dashboard view that reconstructs and renders the multi-turn conversation timeline across all traced requests sharing a pinned `session_hash`. |

### 0.4 Document map

The specification is split into twenty-three numbered Markdown files (`0` through `22`) plus the machine-readable `requirements.toml` registry so engineers can review and trace individual subsystems. The numbered split files and `requirements.toml` are the single authoritative source of truth. The combined file (`LLMRoller Software Requirements and Design Document.md`) is a generated bundle produced by `cargo xtask docs-bundle`. CI checks that concatenating the numbered files with shifted heading levels produces zero diff.

| File | Title | Focus |
| --- | --- | --- |
| `0. SRDD - Introduction.md` | Introduction | Scope, priority conventions, glossary, and authoritative document map |
| `1. SRDD - Summary.md` | Summary | Problem statement, product vision, and core architectural choices |
| `2. SRDD - Research Findings.md` | Research findings | Empirical data on session headers, provider limits, and `/usage` endpoints |
| `3. SRDD - Goals, Non-Goals, and Assumptions.md` | Goals, non-goals, and assumptions | Targets, boundaries, and memory and disk sizing assumptions |
| `4. SRDD - Users, Roles, and Provider Ownership.md` | Users, roles, and provider ownership | Admin, user, and system actor scopes, three-tier pools, and consented user sharing |
| `5. SRDD - System Architecture.md` | System architecture | Component layout, request flow, hot-path state, CLI contracts, and listeners |
| `6. SRDD - Functional Requirements.md` | Functional requirements | Testable P0, P1, and P2 functional specifications |
| `7. SRDD - Non-Functional Requirements.md` | Non-functional requirements | Latency, memory budgets, streaming, compatibility, and operational targets |
| `8. SRDD - Security and Privacy.md` | Security and privacy | Threat model, key hierarchy, SSRF connector, isolation, and supply-chain controls |
| `9. SRDD - Data Model.md` | Data model | 31-table SQLite WAL schema, canonical AAD encryption rules, dual-queue writer, and retention policies |
| `10. SRDD - Routing Design.md` | Routing design | Request gates, candidate checks, pluggable routing strategy engine, pin state machine, error classifier, and failover |
| `11. SRDD - Session Identification and Hints.md` | Session identification and hints | Eight-step inbound session extraction, response-link inheritance, hashing, and outbound hints |
| `12. SRDD - Protocol Handling and Transparent Proxying.md` | Protocol handling and transparent proxying | Request and response header policies, surgical body edits, streaming tee, and token extraction |
| `13. SRDD - Pricing, Limits, and Source Provider Usage Engine.md` | Pricing, limits, and source provider usage engine | Price book math, generalized rolling and anchored windows, reservations, and `/usage` observation sync |
| `14. SRDD - API and Web UI Specification.md` | API and web UI specification | Proxy routes, error envelopes, user and admin JSON APIs, and HTMX dashboard screens |
| `15. SRDD - Technology Choices and Workspace Layout.md` | Technology choices and workspace layout | Rust crates, versions, `xtask` tooling, and multi-crate repository structure |
| `16. SRDD - Testing Plan and TDD Governance.md` | Testing plan and TDD governance | Strict TDD, boundary rules, failure taxonomy, mocking, mutation tests, and traceability |
| `17. SRDD - Roadmap and Milestones.md` | Roadmap and milestones | Dependency-ordered delivery milestones and exit criteria |
| `18. SRDD - Risks and Mitigations.md` | Risks and mitigations | Technical, vendor, and operational risk register |
| `19. SRDD - Architectural Decisions and Open Questions.md` | Architectural decisions and open questions | Resolved design trade-offs and operator configuration choices |
| `20. SRDD - Sources.md` | Sources | Verified primary links for providers, clients, gateways, and standards |
| `21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md` | Concurrency, lifecycle, and failure-recovery model | Memory budgets, lock hierarchy, reloader ordering, writer durability queues, `RequestLifecycle`, and supervision |
| `22. SRDD - Deployment and Containers.md` | Deployment and containers | Multi-stage `Dockerfile`, distroless and dev images, `docker-compose.yml` topologies, environment contract, and container healthchecks |
| `requirements.toml` | Machine-readable requirement registry | Authoritative TOML mapping of requirement IDs to priorities, milestones, and delivering sub-phases |

## 1. Summary

LLMRoller is a self-hosted LLM proxy and API key router written in Rust. It sits between coding agents and multiple paid upstream subscriptions or aggregator accounts. Each user authenticates to LLMRoller with a single client key, and LLMRoller distributes conversations across a pool of upstream keys.

### 1.1 The problem

Subscription coding plans such as [OpenCode Go](https://www.google.com/search?q=https%3A%2F%2Fopencode.ai%2Fdocs%2Fgo), [Command Code GOAT](https://www.google.com/search?q=https%3A%2F%2Fcommandcode.ai%2Fdocs%2Fplans%2Fgoat), and [Open Design](https://www.google.com/search?q=https%3A%2F%2Fopen-design.ai%2Fpricing%2F) sell monthly tiers with rolling five-hour, seven-day, and monthly dollar caps. Heavy users and small engineering teams often hold several keys across these providers alongside pay-as-you-go aggregators like [OpenRouter](https://www.google.com/search?q=https%3A%2F%2Fopenrouter.ai%2Fdocs%2Fapi_reference%2Flimits).

Managing these keys by hand causes two failures. First, switching keys or providers mid-conversation destroys prompt cache locality, which multiplies input token costs up to tenfold and slows responses. Second, different providers give different monthly dollar limits to the same model, so static key rotation wastes high-allowance tiers on the wrong models.

### 1.2 The solution

LLMRoller solves both problems in one single-binary gateway. It reads the session identifier that coding agents already send, or inherits the parent session across `previous_response_id` chains. It then pins each conversation to one upstream key for its entire lifetime. Recognized fields include `x-claude-code-session-id`, `session_id`, `x-session-affinity`, `x-opencode-session`, and `prompt_cache_key`.

When a new session starts, LLMRoller selects an eligible upstream key using a configurable routing strategy within the caller's active pool tier. Supported strategies include `smooth_weighted_rr` (default), `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, and `power_of_two_choices`. Operators and users can override route weights per model to match each provider's dollar allowance.

### 1.3 Core architectural pillars

The system design rests on nine engineering decisions that govern every module in the workspace.

| Pillar | Design decision |
| --- | --- |
| Transparent forwarding | Buffer each inbound request body once in reference-counted memory (`bytes::Bytes`, shared zero-copy across pre-first-byte failover retries) to parse `model` and session fields, forward the body byte-identically except for surgical top-level edits, and stream response SSE chunks without buffering after the first valid frame. |
| Native session pinning | Extract the client's existing session identifier (or inherit the session across `response_links`), bind `(client_key_id, model_pk, session_hash)` to one upstream key, and emit the exact affinity field that the chosen provider expects. |
| Pluggable routing strategies and per-model weights | Use the upstream key's default priority weight unless the route or user defines a per-model override. Route new sessions through a pluggable `RouteSelector` engine (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, or `power_of_two_choices`) inside per-user selection groups. |
| Dual provider scopes and three-tier pools | Support both admin-managed `global` providers and user-managed `personal` providers across three ordered tiers (`Own`, `Shared`, `Global`) so grantees spend their own keys before consented shared keys under `personal_first`. |
| First-class standard users | Ship full role-based access control in version 1.0 for both `admin` and `user` accounts, giving standard users self-service personal providers, consented sharing controls, client keys, and scoped analytics. |
| Hybrid usage and `/usage` pass-through | Compute request cost locally in integer micro-USD while polling source provider `/usage` endpoints to update time-bounded `UpstreamObservation` projections, and expose upstream usage via role-sanitized pass-through APIs. |
| Opt-in content traceability | Record prompts, reasoning, tool calls, and responses when enabled by an admin using content-addressed blocks (`trace_blocks`) and a bounded two-thread compression pool, and render a read-only session chat preview in the dashboard. |
| Zero-dependency operations | Ship one static Rust binary with an embedded HTMX web interface and one SQLite database in WAL mode. |
| Strict TDD and mutation testing | Develop all controlled logic under Red-Green-Refactor discipline with full requirement-to-test traceability (`cargo xtask traceability`) and automated mutation testing via [cargo-mutants](https://mutants.rs/). |

## 2. Research findings

This section records the empirical findings gathered on 19 October 2026 from vendor documentation, source repositories, issue trackers, and live endpoint checks. These facts ground every requirement in this specification.

### 2.1 Session pinning across providers and gateways

No industry standard or RFC defines session pinning for LLM APIs. Each model provider and gateway uses its own field name, placement, and expiration policy.

| Service or layer | Field or header | Location | Observed behavior and source |
| --- | --- | --- | --- |
| [OpenAI API](https://developers.openai.com/api/docs/guides/prompt-caching) | `prompt_cache_key` | JSON body | Soft routing hint combined with a prefix hash. Traffic above roughly 15 requests per minute per prefix and key overflows to other machines. |
| Anthropic API | `cache_control` | JSON body | Explicit prompt breakpoint markers inside messages. Caches are isolated per organization and workspace with no session routing header. |
| [Fireworks AI](https://docs.fireworks.ai/guides/prompt-caching) | `x-session-affinity`, `x-multi-turn-session-id` | HTTP header | Routes multi-turn requests to the same replica where the KV cache lives. |
| [OpenRouter](https://openrouter.ai/docs/guides/best-practices/prompt-caching) | `session_id`, `x-session-id`, `prompt_cache_key` | Body or header | Pins a conversation to one provider endpoint for a sliding 10-minute window (up to 256 characters). Setting `provider.order` disables sticky routing. |
| [Vercel AI Gateway](https://vercel.com/docs/ai-gateway/models-and-providers/session-affinity) | `x-session-id`, `x-session-affinity`, `x-claude-code-session-id` | HTTP header | Pins the upstream provider and forwards `x-session-affinity` upstream. |
| [LLM Gateway](https://docs.llmgateway.io/features/sessions) | `x-session-id`, `x-session-affinity` | HTTP header | Uses deterministic rendezvous hashing to keep a session on one provider and region. |
| [Bifrost](https://docs.getbifrost.ai/providers/session-affinity.md) | `x-bf-session-id` or client-native headers | HTTP header | Scopes bindings to `(virtual_key, session_id, model)`. Confirms bindings only when a request is served and deletes them on failure. |
| [Eden AI](https://www.edenai.co/docs/v3/llms/prompt-caching) | `x-session-id` | Header or body | Uses the session identifier only inside the router and never forwards it to the upstream provider. |
| [LiteLLM](https://docs.litellm.ai/docs/tutorials/claude_code_prompt_cache_routing) | `previous_response_id` or internal cache table | Body or router state | Remembers which deployment wrote a cache entry and routes follow-up turns there. |
| [OpenCode Go](https://opencode.ai/docs/go) | `x-opencode-session` | HTTP header | Mandatory since 5 September 2026 ([vscode#334186](https://github.com/microsoft/vscode/issues/334186)). Also recognizes native Claude Code and Codex session headers. |
| [Command Code](https://commandcode.ai/docs/provider) | None documented | None | The Provider API documentation lists no session affinity header. |
| [Open Design AMR](https://open-design.ai/pricing/) | None documented | None | Public documentation lists no session affinity header. |

### 2.2 Session identifiers sent by coding agents

Modern coding harnesses already attach a stable session identifier to every turn of a conversation.

| Client agent | Headers and body fields sent | Verified behavior and source |
| --- | --- | --- |
| [Claude Code](https://code.claude.com/docs/en/llm-gateway) | `x-claude-code-session-id`, `x-claude-code-agent-id`, `x-claude-code-parent-agent-id` | Sends `x-claude-code-session-id` on every turn ([rollout guide](https://code.claude.com/docs/en/llm-gateway-rollout)). Subagents add agent headers while sharing the root session ID ([NVIDIA Dynamo docs](https://docs.nvidia.com/dynamo/user-guides/agents/session-i-ds)). |
| [Codex CLI](https://github.com/openai/codex/issues/24973) | `session-id` or `session_id` header, plus body `prompt_cache_key` | Sends both header and body hints. Some proxy setups strip one spelling ([codex-router#957](https://github.com/duolahypercho/codex-router/pull/957)). |
| [opencode](https://github.com/anomalyco/opencode/issues/55243) | `x-session-affinity`, `X-Session-Id`, `x-opencode-session`, `x-parent-session-id`, `promptCacheKey` | Emits multiple headers and body fields from one affinity ID ([opencode#35982](https://github.com/anomalyco/opencode/pull/35982)). Forked and child sessions reuse the parent session ID. |
| Generic gateway clients | `x-session-id` header or `session_id` body field | Standard convention across [OpenRouter](https://openrouter.ai/docs/guides/best-practices/prompt-caching) and [Merge Gateway](https://docs.merge.dev/merge-gateway/features/prompt-caching). |

### 2.3 Target provider endpoints, protocols, and quotas

The four primary providers differ in base URL, wire protocol per model, model naming convention, and quota window structure.

| Attribute | [OpenCode Go](https://opencode.ai/docs/go) | [Command Code](https://commandcode.ai/docs/provider) | [Open Design AMR](https://open-design.ai/pricing/) | [OpenRouter](https://openrouter.ai/docs/api_reference/limits) |
| --- | --- | --- | --- | --- |
| Base URL | `https://opencode.ai/zen/go/v1` | `https://api.commandcode.ai/provider/v1` | `https://amr-link.open-design.ai/v1` | `https://openrouter.ai/api/v1` |
| Inference paths | `/chat/completions`, `/responses`, `/messages` (varies by model) | `/chat/completions`, `/responses`, `/messages` (Claude is `/messages` only) | `/chat/completions`, `/responses`, `/messages` ([issue #8687](https://github.com/nexu-io/open-design/issues/8687)) | `/chat/completions`, `/responses`, `/messages` |
| Model catalog path | `GET /models` | `GET /models` (includes `supported_endpoints` array) | `GET /models` | `GET /models` |
| Model ID format | Bare, e.g. `deepseek-v4-flash` | Vendor-prefixed, e.g. `deepseek/deepseek-v4-flash` | Bare, e.g. `deepseek-v4.1-flash` | Vendor-prefixed, e.g. `anthropic/claude-sonnet-4` |
| Authentication | `Authorization: Bearer` or `x-api-key` | `Authorization: Bearer` or `x-api-key` (`user_...`) | `Authorization: Bearer sk-amr-...` ([plugins#84](https://github.com/magpie-community/plugins/pull/84)) | `Authorization: Bearer sk-or-...` |
| Quota structure | Rolling 5 h ($12), weekly ($30), monthly ($60), plus per-model caps and optional Zen balance fallback | Rolling 5 h ($14), weekly ($35), monthly ($70) on GOAT ([GOAT plan](https://commandcode.ai/docs/plans/goat)), plus per-model boosted credits | Rolling 5 h ($10), 7 d ($30), monthly ($120) on Go; DeepSeek V4.1 Flash counts double | Prepaid account credits with optional per-key dollar limit and reset period |
| Rate or pricing quirks | DeepSeek peak rates on weekdays 01:00–04:00 and 06:00–10:00 UTC; 200K–272K context tiers | Go plan returns `403 upgrade_required` on API calls; DeepSeek 2x peak rates on weekdays 01:00–04:00 and 06:00–10:00 UTC ([pi-commandcode-provider](https://pi.dev/packages/pi-commandcode-provider)) | Stops serving when window credits run out | Returns exact `usage.cost` on every response ([usage accounting](https://openrouter.ai/docs/cookbook/administration/usage-accounting)) |

### 2.4 Source provider `/usage` endpoints

Three of the four target providers already expose API-key-authenticated usage or quota endpoints. The fourth provider has an open tracking issue confirmed by its maintainers.

| Provider | Upstream usage endpoint | Auth method | Returned payload and verification source |
| --- | --- | --- | --- |
| OpenCode Go | `GET https://opencode.ai/zen/go/v1/usage` | `Authorization: Bearer <key>` | Verified live in [opencode#43983](https://github.com/anomalyco/opencode/issues/43983). Returns HTTP 200 with `usage.rolling`, `usage.weekly`, and `usage.monthly`, each holding `status` (`ok`), `percent` (integer 0–100), and `resetsAt` (ISO-8601 UTC timestamp). Does not return per-model or historical rows. |
| Command Code | `GET https://api.commandcode.ai/alpha/billing/credits`, `/alpha/usage/summary`, `/alpha/billing/subscriptions` (plus `/internal/billing/credits` fallback; `/alpha/whoami` is never polled to avoid ingesting account PII) | `Authorization: Bearer <key>` | Verified in [pi-commandcode-provider](https://pi.dev/packages/pi-commandcode-provider) and [commandcode-goat-usagebar](https://open-vsx.org/extension/berkeduruu/commandcode-goat-usagebar). Returns 5 h, 7 d, and monthly quota percentages, credits used and remaining, plan tier, and credit sources. |
| OpenRouter | `GET https://openrouter.ai/api/v1/key` (alias `/api/v1/auth/key`) | `Authorization: Bearer <key>` | Documented in [OpenRouter Limits](https://openrouter.ai/docs/api_reference/limits). Returns `data.usage` (all-time USD), `usage_daily`, `usage_weekly`, `usage_monthly`, `limit`, `limit_remaining`, `limit_reset`, and `is_free_tier`. |
| Open Design AMR | `GET https://amr-link.open-design.ai/v1/usage` (tracked) | `Authorization: Bearer sk-amr-...` | Verified in [open-design#8674](https://github.com/nexu-io/open-design/issues/8674). Returns 404 as of 19 October 2026 because billing sits on `amr-api.open-design.ai` behind a desktop sign-in key. Maintainers confirmed a service API feature tracking `plan`, `windows` (`7d`, `5h` with `used_percent` and `resets_at`), and `wallet_balance_usd`. |

### 2.5 Where final token usage appears in each wire protocol

Each inference protocol places token usage in a different event or JSON object during streaming and non-streaming calls.

| Protocol | Non-streaming JSON field | Streaming SSE event and field |
| --- | --- | --- |
| OpenAI Chat Completions (`chat`) | `usage.prompt_tokens`, `completion_tokens`, `prompt_tokens_details.cached_tokens`, `completion_tokens_details.reasoning_tokens` | Final chunk before `data: [DONE]` with empty `choices` when `stream_options.include_usage: true` ([OpenAI cookbook](https://nbviewer.org/format/script/github/openai/openai-cookbook/blob/main/examples/How_to_stream_completions.ipynb)). Command Code and OpenRouter send it without opt-in. |
| OpenAI Responses (`responses`) | `usage.input_tokens`, `output_tokens`, `input_tokens_details.cached_tokens`, `output_tokens_details.reasoning_tokens` | `response.completed` event under `response.usage` ([OpenAI community thread](https://community.openai.com/t/openai-api-get-usage-tokens-in-response-when-set-stream-true/141866/35)). |
| Anthropic Messages (`messages`) | `usage.input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens` | `message_start` carries input and cache token counts; `message_delta` carries cumulative output tokens ([Anthropic Messages reference](https://abacus.ai/help/developer-platform/route-llm/anthropic-messages)). |
| OpenRouter extension | `usage.cost` and `usage.cost_details` | Included in the final usage object on every stream ([OpenRouter usage accounting](https://openrouter.ai/docs/cookbook/administration/usage-accounting)). |

### 2.6 Summary of findings that drive the architecture

Eleven empirical findings shape the requirements and design in the remaining sections.

| Finding ID | Topic | Architectural implication |
| --- | --- | --- |
| F1 | Session profile adapters | Because session fields differ per provider, each provider adapter declares which headers and body fields to write. |
| F2 | No constant session IDs | Sending a static constant merges all users into one bucket ([vscode#334186](https://github.com/microsoft/vscode/issues/334186)). LLMRoller passes the client's ID or derives a per-conversation hash. |
| F3 | Hybrid local and `/usage` tracking | OpenCode Go, Command Code, and OpenRouter expose live quota endpoints, while Open Design AMR does not yet. LLMRoller combines local micro-USD math with upstream `/usage` polling and pass-through. |
| F4 | Model ID normalization | Providers name the same model differently (`deepseek-v4-flash` vs. `deepseek/deepseek-v4-flash`), so LLMRoller keeps a canonical model registry and alias table. |
| F5 | Per-model protocol eligibility | Providers restrict certain models to specific endpoints (such as Claude on `/messages` at Command Code), so route eligibility checks native protocol support per model. |
| F6 | Stateful Responses affinity | OpenAI Responses calls carrying `previous_response_id` must reach the upstream key that created that response ID ([LiteLLM commit](https://git.kvant.cloud/phoenix/litellm-mirror/commit/043055c7b3a893e358b2e592d140f2be83823104)). |
| F7 | Header transparency | Providers monitor client headers and reject unsupported beta flags ([opencode-starter](https://github.com/jacob-bd/opencode-starter)), while [Claude Code](https://code.claude.com/docs/en/llm-gateway) expects gateways to forward its headers verbatim. |
| F8 | Stream truncation detection | Upstreams occasionally close streams early without a terminal event ([command-code#785](https://github.com/CommandCodeAI/command-code/issues/785)). LLMRoller detects missing terminal events and marks usage rows `incomplete`. |
| F9 | Multi-user key isolation | Teams share pooled global keys while individuals also bring personal subscriptions or share a personal key with a teammate. The router enforces strict ownership and share scopes. |
| F10 | Supply-chain hardening | Gateways hold high-value credentials and are prime targets, as shown by the [March 2026 LiteLLM PyPI compromise](https://docs.litellm.ai/blog/security-update-march-2026). LLMRoller ships as a minimal, audited Rust binary. |
| F11 | Session-pinned chat reconstruction | Because agents send a stable session ID per conversation, opt-in recording of request and SSE response turns lets the dashboard render a read-only chat preview per session. |

## 3. Goals, non-goals, and assumptions

This section establishes the operational goals, explicit version 1.0 boundaries, and sizing assumptions for LLMRoller.

### 3.1 Goals

LLMRoller pursues nine measurable engineering goals in version 1.0.

| ID | Goal | Success measure |
| --- | --- | --- |
| G1 | Preserve upstream prompt cache locality across multi-turn coding sessions. | Requests carrying a valid session ID (or `previous_response_id` link) stay on their pinned upstream key while that route remains eligible, and recovery repins wait until the session has been idle for at least `recovery_repin_min_idle_secs` (default 300 s). |
| G2 | Pool global, personal, and shared upstream keys with predictable session distribution across configurable routing strategies. | Under `smooth_weighted_rr` and `weighted_rr`, fixed candidate sets stay within 0.5% of `W_eff_i / sum(W_eff)` over 100,000 selections (`1.0%` under subset churn); under `naive_rr`, each of `N` healthy routes receives `1 / N` exactly over `K * N` selections; under `uniform_random` and `weighted_random`, empirical frequencies over 100,000 selections stay within `1.0%` of target probabilities (`p > 0.001` chi-squared gate). |
| G3 | Support both admin-managed global providers and user-managed personal providers with granular, consented user-to-user sharing. | Standard users can add personal providers, share them with specific peers who accept the endpoint disclosure, and route traffic across ordered `Own`, `Shared`, and `Global` tiers without admin intervention. |
| G4 | Stay transparent to coding agents and upstream providers. | Streaming chunks incur under 1 ms of forwarding delay per complete frame, and request bodies on own personal keys keep their exact key and message order. |
| G5 | Provide accurate cost, token, and quota visibility by combining local price-book math with source provider `/usage` endpoints. | Local window counters combine with time-bounded `UpstreamObservation` projections from `/usage` snapshots without permanent stale-baseline lockouts, and clients can query `/v1/usage` directly. |
| G6 | Protect upstream secrets, client keys, and user privacy at rest, in memory, and in logs. | Zero secret leaks in canary log tests, XChaCha20-Poly1305 encryption at rest with canonical length-prefixed AAD, and zero prompt storage unless content traceability is enabled. |
| G7 | Operate as a single, low-overhead static Rust binary with an embedded web interface. | Idle memory stays under 50 MB, and p99 proxy overhead before first upstream byte stays under 5 ms at 500 req/s. |
| G8 | Enforce strict Test-Driven Development and mutation testing across all controlled logic. | Every requirement maps to automated tests in `requirements.toml`, and `cargo-mutants` reports zero unannotated surviving mutants in core routing and protocol crates. |
| G9 | Support admin opt-in content traceability and a read-only session chat preview. | When enabled by an admin, traced sessions store deduplicated content-addressed blocks and render prompts, reasoning, tool calls, and responses in chronological `(ts, request_id)` order with zero XSS risk. |

### 3.2 Non-goals for version 1.0

Eight capabilities are intentionally excluded from version 1.0 to keep the attack surface and complexity small.

| ID | Non-goal | Rationale |
| --- | --- | --- |
| NG1 | Client or TLS fingerprint impersonation | LLMRoller never spoofs another vendor's `User-Agent` or JA3/JA4 TLS fingerprint. It forwards the caller's real headers or a neutral default. |
| NG2 | Public commercial resale or multi-organization billing | Subscription coding plans forbid public reselling. LLMRoller targets a single person or a trusted internal team. |
| NG3 | Automatic cross-protocol translation in Milestone 1 | Translating between `chat`, `responses`, and `messages` can drop reasoning blocks ([open-design#8687](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2Fnexu-io%2Fopen-design%2Fissues%2F8687)). Base routing matches native support; opt-in translation is deferred to a later milestone. |
| NG4 | Multi-node active-active clustering | A single Rust process with SQLite WAL handles 500 req/s and 2,000 open streams, which exceeds single-team coding-agent load. |
| NG5 | Payment processing, invoicing, or Stripe integration | Budget tracking exists to prevent quota exhaustion and monitor spend, not to bill external customers. |
| NG6 | Unconditional or silent prompt logging | Prompt and response recording is strictly off by default and activates only when an admin enables content traceability mode (`TRC-1`). |
| NG7 | Consumer OAuth browser session scraping | Version 1.0 authenticates upstream using standard API keys only. |
| NG8 | Non-inference media endpoints | Image, audio, and embedding endpoints do not use multi-turn prompt caching and are outside version 1.0 scope. |

### 3.3 Assumptions and constraints

The design relies on six operational assumptions listed in the table below.

| ID | Assumption | Operational impact |
| --- | --- | --- |
| A1 | Terms of use compliance | The instance operator and individual users hold valid accounts with each configured provider and comply with each provider's terms ([Open Design Terms](https://www.google.com/search?q=https%3A%2F%2Fdownload.open-design.ai%2Fterms%2F), [OpenCode Go Docs](https://www.google.com/search?q=https%3A%2F%2Fopencode.ai%2Fdocs%2Fgo)). |
| A2 | Sizing and memory envelope | One instance on a 2 vCPU, 2 GB RAM reference host supports up to 200 users, 500 upstream keys, 1,000 client keys, 2,000 open SSE streams, and 500 req/s at peak for requests with bodies up to 256 KiB (or `preflight_body_budget / (mean_body * ttff)` in general, bounded by `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB = min(1024, floor(0.40 * memory_limit_mb))`, which is 819 MiB on a 2 GB host). |
| A3 | Configurable client base URL | Every target client tool (Claude Code, Codex CLI, opencode, Hermes, and standard OpenAI or Anthropic SDKs) lets the user configure a custom base URL and API key. |
| A4 | TLS endpoint handling | Production deployments handle HTTPS either at an upstream reverse proxy (Caddy, nginx, or Traefik) listed in `LLMROLLER_TRUSTED_PROXIES` with response buffering off, or via LLMRoller's built-in `rustls` listener (`LLMROLLER_TLS_CERT` and `LLMROLLER_TLS_KEY`). |
| A5 | Rust toolchain and runtime | The project compiles with a pinned stable Rust toolchain (Rust 2024 edition) and runs on Linux (`x86_64` and `aarch64`) and macOS without external runtime dependencies. |
| A6 | Disk capacity budget | The data directory provides at least `daily_requests * retention_days * 600 B * 1.3` bytes for raw `usage_events` (default 30-day retention capped at 50,000,000 rows) plus `usage_rollups_5min`, `usage_rollups_hourly`, and opt-in `trace_records` and `trace_blocks` storage; the web UI displays projected disk usage. |

## 4. Users, roles, and provider ownership

LLMRoller is a multi-user system in version 1.0. It defines two human user classes (`admin` and `user`), one internal system actor class (`SystemTask`), and one machine caller class (`client`). It also defines two provider ownership scopes (`global` and `personal`) with explicit, consented user-to-user sharing across three ordered routing tiers (`Own`, `Shared`, and `Global`).

### 4.1 User classes and `ActorScope`

Every human account has a `role` column set to either `admin` or `user`. Machine callers authenticate to the data plane with a client key owned by a human account. Internal background jobs and CLI maintenance commands run under a sealed `ActorScope::System` variant so system operations never masquerade as a synthetic admin user.

```rust
pub enum ActorScope {
    User { user_id: UserId, role: Role },
    System { task: SystemTask },
}

pub enum SystemTask {
    Writer,
    Reloader,
    Discovery,
    UsagePoller,
    RetentionSweeper,
    DataPlaneUsage,
    Cli(CliCommand),
}
```

`ActorScope::System` constructors are restricted (`pub(crate)`) so only `crates/server` and `crates/store` can construct them; `crates/admin` handlers can construct only `ActorScope::User`. Audit rows written by `ActorScope::System` set `actor_user_id = NULL` and record `detail_json.system_task`.

| User or actor class | Version 1.0 status | Summary of capabilities |
| --- | --- | --- |
| Admin (`admin`) | P0 (full support) | Manages instance settings, user accounts, global providers, global upstream keys, canonical model catalog, global price book, all personal providers and shares (via explicit `/admin/api/*` routes), all pins, and system-wide usage analytics. |
| Standard user (`user`) | P0 (full support) | Manages own account credentials, own client keys, own personal providers and upstream keys, outgoing and incoming sharing grants (including explicit share acceptance), per-model priority overrides on own personal and shared keys, own active pins, and own usage analytics. |
| System task (`SystemTask`) | P0 (internal only) | Runs scheduled `/models` discovery, `/usage` polling, retention sweeps, snapshot reloading, data-plane usage batching, and offline CLI maintenance with explicit audit attribution. |
| Client agent (`client`) | P0 (full support) | Calls `/v1/chat/completions`, `/v1/responses`, `/v1/messages`, `/v1/models`, `/v1/usage`, `/v1/usage/raw`, and `/v1/key` using a client key (`llr_...`). Has no web UI or control-plane access. |

### 4.2 Role-based access control matrix

The control plane enforces strict authorization checks on every HTTP handler and database query using the `&ActorScope` passed from the authentication middleware. Every user-facing API and HTML template projects upstream key metadata through `crates/admin::projection::project_key(viewer, key) -> KeyView`. When a non-admin views a `global` key, `upstream_key_id` becomes `null` and `upstream_key_label` becomes `"global pool"`. The projection also omits `priority`, `priority_override`, `secret_last4`, and `health_until`.

| Resource or action | Admin (`admin`) | Standard user (`user`) | Client agent (`client`) |
| --- | --- | --- | --- |
| Complete initial first-admin setup | Yes (when zero users exist) | No | No |
| Create, disable, delete users, or reset passwords | Yes (cannot disable, demote, or delete the last active admin; deleting a user takes `?confirm=<username>` and records `deleted_users`) | No (can change own password and TOTP only) | No |
| Create, edit, test, or delete `global` providers and keys | Yes | No | No |
| View `global` provider availability and exposed models | Yes (full details and last-4 of secrets) | Yes (provider name, exposed models, and aggregated `"Global pool"` column in the routing matrix via `project_key`; never sees global key IDs, labels, priorities, or secret suffixes) | Via `GET /v1/models` |
| Create, edit, test, calibrate, or delete `personal` providers and keys | Yes (own personal keys via `/api/user/*`, plus admin override on any user's keys via `/admin/api/users/{id}/providers` and `/admin/api/providers/{id}`) | Yes (own personal providers and keys only; receives HTTP 404 on any other user's resources) | No |
| Share a `personal` provider or key with another user | Yes (for own personal keys or any personal key; approves `custom` provider shares via `POST /admin/api/shares/{id}/approve`) | Yes (for own personal providers and keys only; grantee must accept via `POST /api/user/shares/{id}/accept`) | No |
| View or accept a `personal` provider or key shared by another user | Yes | Yes (grantee sees provider kind, base URL host, key label, health, allowed models, destination disclosure, and own share usage or cap; never the secret or `secret_last4`) | Routes through it once accepted and eligible |
| Poll or view source provider `/usage` for an upstream key | Yes (all global and personal keys, including live `?fresh=1`) | Yes (own personal keys including live `?fresh=1`, plus cached sanitized window status on keys shared with them) | Via `GET /v1/usage`, `GET /v1/usage/raw`, and `GET /v1/key` (scoped) |
| Manage canonical model exposure (`auto`, `forced_on`, `forced_off`), merges, splits, and aliases | Yes | Read-only view of catalog; can enable or disable models on own personal and shared routes | No |
| Set per-model priority overrides on routes | Yes (global routes and own personal routes) | Yes (own personal and shared routes only via `user_route_preferences`; cannot override weights on global routes) | No |
| Create, rotate, restrict, or revoke client keys (`llr_...`) | Yes (own keys and any user's keys) | Yes (own client keys only) | Uses key for bearer auth |
| View usage dashboard, time series, and CSV or JSON exports | Yes (instance-wide and filtered by any user, key, provider, or model) | Yes (strictly scoped to own client keys and traffic served by own personal keys) | Via `GET /v1/usage` for own client key |
| View active pins, recently active sessions per client key, or unpin/move pins | Yes (all pins and per-client-key recently active sessions with live pinned provider and upstream key via `/admin/api/pins`, `/admin/api/users/{id}/pins`, and `/admin/api/client-keys/{id}/sessions`) | Yes (pins and recently active sessions belonging to own client keys via `/api/user/pins` and `/api/user/client-keys/{id}/sessions`, with live pinned provider and `project_key`-redacted upstream key) | Implicit via requests (and current session pin via `GET /v1/usage`) |
| Enable, scope, or purge content traceability mode | Yes (instance-wide and scoped targets; lowering retention takes `"confirm_purge": true`) | No (read-only visibility into whether traceability is active on own traffic) | No |
| View read-only Session Chat Preview of recorded turns | Yes (all traced sessions, logged once per 30 minutes per `(admin, session_hash)` in `audit_log`) | Yes (traced sessions belonging to own client keys only; never sees grantee transcripts on shared keys) | No |
| View audit log | Yes (all instance events) | Yes (events where actor or target belongs to the user) | No |

### 4.3 Provider ownership scopes: global and personal

Every provider record in LLMRoller has an `ownership_scope` field set to either `global` or `personal`.

#### 4.3.1 Global providers

Only an admin can create, modify, or delete a `global` provider and its upstream keys. Global providers hold shared team or organization subscriptions. Every active user with `can_use_global_providers = 1` can route requests through eligible global upstream keys. Admins can also set an optional monthly global spend cap (`global_budget_micro_usd_month`) on any user account.

#### 4.3.2 Personal providers

Any authenticated user (`user` or `admin`) can create a `personal` provider and attach one or more personal upstream keys to it. A personal provider has a non-null `owner_user_id` referencing the user who created it. Database triggers enforce `upstream_keys.owner_user_id == providers.owner_user_id` on every attached key.

Personal providers let users bring their own OpenCode Go, Command Code, Open Design AMR, OpenRouter, or custom API keys into LLMRoller. Users get session pinning and per-model priority overrides across their own keys, and can combine personal keys with shared and global keys. Disabling a user account (`users.disabled = 1`) immediately makes that user's personal providers and keys ineligible for everyone. The underlying share rows stay in SQLite so re-enabling the user restores them.

### 4.4 Sharing personal providers and keys

A personal provider owner can share a personal provider or a specific personal upstream key with other registered users on the same instance. Routing through a personal provider sends the grantee's prompts and tool outputs to that provider's endpoint and account. Every share therefore needs explicit grantee consent, plus admin approval when the provider is `custom`.

| Share attribute | Behavior and enforcement |
| --- | --- |
| Grantor (`owner_user_id`) | Must be the active owner (`users.disabled = 0`) of the `personal` provider (or an admin). Cannot share a `global` provider and cannot re-share a key that was only shared with them (no transitive sharing, enforced by SQL trigger `trg_shares_integrity`). Personal providers with a cross-origin `usage_url` (`usage_url_cross_origin_ack = true`) cannot be shared (returns HTTP 409). |
| Grantee (`grantee_user_id`) | Any active `user` or `admin` account on the instance other than `owner_user_id`, selected by username. |
| Target scope and non-overlap invariant | Either an entire `personal` provider (`upstream_key_id IS NULL`) or one specific `personal` upstream key (`upstream_key_id IS NOT NULL`). Partial unique indexes `uq_share_key_active` and `uq_share_provider_active` prevent duplicate active grants, and `POST /api/user/shares` rejects creating a key-level share when an active provider-level share exists for the same `(grantee_user_id, provider_id)` (or vice versa) with HTTP 409 `share_overlap`. For any legacy or migrated rows, `effective_share(U, K)` deterministically selects the key-level share first, then the provider-level share. |
| Grantee consent and config fingerprint (`accepted_at`, `accepted_config_fingerprint`) | Each provider stores `share_config_fingerprint = hex(SHA-256(canonical_json({kind, base_url, allow_private_network, paths, auth, strip_headers, add_headers, ua_policy, session_profile, usage_profile, response_header_allow})))`. A newly created share starts with `accepted_at = NULL` and is ineligible for routing until the grantee views the provider kind, `base_url` host, and destination disclosure in the UI and calls `POST /api/user/shares/{id}/accept` echoing the current `share_config_fingerprint`. Any edit to the provider's routing-relevant configuration changes `share_config_fingerprint` in the same transaction, automatically suspending all shares on that provider until each grantee re-accepts. |
| Custom provider admin approval (`admin_approved_at`) | When `providers.kind == 'custom'`, the share also stays ineligible until an admin calls `POST /admin/api/shares/{id}/approve` to set `admin_approved_at`. |
| Model restriction (`allowed_models`) | Optional JSON list of canonical model IDs that the grantee may invoke on the shared key. `NULL` permits all models served by the key. |
| Monthly spend cap (`max_monthly_micro_usd`) | Optional micro-USD cap on how much the grantee's client keys can spend on this share in a UTC calendar month, enforced via the reservation ledger and UTC monthly accumulator. |
| Permission level | Usage only. The grantee can route inference requests through the shared key and see its health badge and remaining share allowance, but cannot view the key's last four characters, edit its priority or settings, trigger calibration, or delete it. |
| Revocation | The owner (or an admin) can revoke a share at any time. Revocation deletes the grantee's `user_route_preferences` rows for the affected keys in the same transaction, updates the in-memory `ConfigSnapshot` via the reloader before returning, and invalidates any active session pins where the grantee's client key was pinned to the revoked upstream key. |

### 4.5 Routing interaction between personal, shared, and global providers

When a client key owned by user `U` requests canonical model `M`, the router classifies eligible candidates into three ordered tiers. Grantees thus never drain a teammate's shared quota while their own personal key has headroom.

| Tier (`PoolTier`) | Numeric code | Candidate membership criteria after eligibility filtering (`C1` through `C8`) |
| --- | --- | --- |
| `Own` | `0` | Routes whose provider has `ownership_scope == 'personal'` and `owner_user_id == U`. |
| `Shared` | `1` | Routes reachable via `effective_share(U, K) == Some(s)` where `s` is enabled, not revoked, accepted against the current `share_config_fingerprint`, admin-approved if `custom`, permits model `M` (`s.allowed_models IS NULL` or contains `M`), and has remaining monthly share budget. |
| `Global` | `2` | Routes whose provider has `ownership_scope == 'global'`, provided `U.can_use_global_providers == 1` and `U` has remaining monthly global budget. |
| `Blended` | `3` | Combined tier code recorded on pins selected when `routing_scope == 'blended'` merges `Own`, `Shared`, and `Global` into a single WRR pool. |

Each client key (and user account default) defines a `routing_scope` policy that maps to an ordered list of tier groups. The router evaluates the tier groups in order and runs smooth weighted round-robin inside the first non-empty group.

| Routing scope mode | Ordered tier groups (first non-empty group wins) | Candidate selection behavior |
| --- | --- | --- |
| `personal_first` (default) | `[Own]`, then `[Shared]`, then `[Global]` | Routes through the user's own personal keys first; falls back to consented shared keys only when no `Own` route is eligible for model `M`; falls back to `Global` keys only when neither `Own` nor `Shared` has an eligible route. |
| `blended` | `[Own U Shared U Global]` | Combines eligible routes from the `Own`, `Shared`, and `Global` tiers into a single smooth weighted round-robin pool (`PoolTier::Blended`). |
| `personal_only` | `[Own]`, then `[Shared]` | Routes through `Own` keys first, then `Shared` keys; never uses `Global` keys. |
| `own_only` | `[Own]` | Restricts routing strictly to the user's own personal keys (`Own`), ignoring both shared and global keys. |
| `global_only` | `[Global]` | Restricts routing strictly to the `Global` pool, ignoring personal and shared keys. |

### 4.6 Visibility and accounting for shared personal keys

When Grantee `B` routes a request through Grantor `A`'s personal key, the usage row records `client_key_id`, `upstream_key_id`, and `provider_scope = 'shared'`. It also records the deterministic `share_id` returned by `effective_share(B, K)`.

| Actor | Visibility and privacy boundaries |
| --- | --- |
| Grantor `A` (key owner) | Sees the total tokens and cost consumed on their personal upstream key, plus a per-grantee breakdown showing how much Grantee `B` consumed against the share grant (`SHR-6`). Grantor `A` never sees Grantee `B`'s client key labels, session hashes, or recorded chat transcripts inside LLMRoller (`TRC-8`). |
| Grantee `B` (share recipient) | Sees the provider kind, `base_url` host, and upstream key label before accepting the share, and sees all requests made by `B`'s own client keys (`shared: <owner_username>/<key_label>`) along with `B`'s remaining monthly share allowance. Grantee `B` never sees Grantor `A`'s own usage, other grantees' usage, account-identifying response headers (`Section 12.1.1` / Section 12.1.1), or raw `/usage` account metadata (`SEC-18`). |

## 5. System architecture

LLMRoller runs as one Rust process made of six internal subsystems. These subsystems share an atomic in-memory configuration snapshot (`ArcSwap<ConfigSnapshot>`), a mutex-guarded in-memory pin store (`PinStore`), and one SQLite database in Write-Ahead Logging (WAL) mode.

### 5.1 High-level architecture diagram

The diagram below shows how client agents, standard users, and admins interact with LLMRoller's data plane and control plane.

```
Client agents (Claude Code, Codex CLI, opencode, Hermes, SDKs)
   |  HTTPS + Client Key (llr_...)
   v
+============================= LLMRoller Process =============================+
|                                                                             |
|  +----------------------- Data Plane Listener (:8080) -------------------+  |
|  | Proxy Edge (Preflight Body Budget) -> Request Gates (G1..G7)          |  |
|  |     -> Model & Constraint Resolver -> Session & Link Resolver         |  |
|  |     -> Router (PinStore / Pluggable RouteSelector / Failover)         |  |
|  |     -> GuardedConnector -> Stream Forwarder (64 KiB Hold Buffer)      |  |
|  |     -> Inline SseUsageTap, Bounded TraceTap & RequestLifecycle Guard  |  |
|  |     -> /v1/usage & /v1/key Pass-Through Handler                       |  |
|  +-----------------------------------------------------------------------+  |
|                     |                                     ^                 |
|                     v (lock-free / short mutex read)      | (ordered swap)  |
|  +-----------------------------------------------------------------------+  |
|  | Hot-Path In-Memory State                                              |  |
|  |  - ArcSwap<ConfigSnapshot> (per-domain slices + config_version)       |  |
|  |  - PinStore (DashMap<PinKey, Arc<Mutex<PinEntry>>>, max 250k pins)    |  |
|  |  - Moka Response-Link Cache (backed by SQLite response_links)         |  |
|  |  - RuntimeState: Strategy Groups, RouteLoadTracker, OriginBreakers,   |  |
|  |    UpstreamObservations, UTC Month Accumulators, Health Versions      |  |
|  +-----------------------------------------------------------------------+  |
|                     |                                     ^                 |
|                     v (Control queue + 3 Data queues)     | (Reloader task) |
|  +----------------------- Storage & Background Jobs ---------------------+  |
|  |  - SQLite (WAL mode): 31 tables, dual-queue batching writer actor     |  |
|  |  - 2-Thread OS Trace Sealer Pool (crossbeam bounded(64) + zstd bulk)  |  |
|  |  - Supervised Jobs: Model Discovery, Single-Flight /usage Poller,     |  |
|  |          Price Sync, 5-Min & Hourly Rollups, Retention & Checkpointer |  |
|  +-----------------------------------------------------------------------+  |
|                                       ^                                     |
|  +------------------- Control Plane Listener (127.0.0.1:8081) -----------+  |
|  |  - Admin UI & API (/admin/*): Setup, Users, Global Providers, Catalog |  |
|  |  - User UI & API (/app/*, /api/user/*): Personal Providers, Shares,   |  |
|  |    Client Keys, Personal Routing Overrides, Scoped Usage & Pins       |  |
|  |  - Metrics Listener (127.0.0.1:9090): Bounded Prometheus /metrics     |  |
|  +-----------------------------------------------------------------------+  |
+=============================================================================+
   |  HTTPS + Upstream API Key + Provider Affinity Hint
   v
Upstream Providers (OpenCode Go, Command Code, Open Design AMR, OpenRouter)
```

### 5.2 Subsystem responsibilities

Each subsystem lives in a dedicated module or workspace crate with explicit trait boundaries so business logic stays independent of network and database I/O.

| Subsystem | Core responsibilities | Primary Rust crates |
| --- | --- | --- |
| Proxy edge (`proxy`) | Enforces `LLMROLLER_MAX_BODY_MB` and the `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` admission semaphore, evaluates request gates `G1` through `G7`, dispatches `/v1/*` routes through `GuardedConnector`, holds pre-first-frame bytes up to 64 KiB, and streams responses under an RAII `RequestLifecycle` completion guard. | `axum`, `hyper-util`, `tower`, `reqwest` (`rustls`), `async-compression` |
| Routing core (`core`) | Pure deterministic logic with zero I/O: resolves eight-step session IDs and response-link inheritance, evaluates candidate checks `C1` through `C8` across `Own`, `Shared`, and `Global` tiers, runs the pluggable `RouteSelector` strategy engine (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, `power_of_two_choices`), and drives the `PinStore`, `OriginBreaker`, window ring, and reservation state machines. | `arc-swap`, `dashmap`, `parking_lot`, `smallvec`, `rand` |
| Provider adapters (`proto`) | Implements `ProviderAdapter` per provider type: validates top-level JSON without duplicate keys, applies surgical edits, enforces request and response header policies, runs inline `SseUsageTap` and bounded `TraceTap`, parses `/models` and `/usage` payloads, and runs the five-class error classifier. | `serde`, `serde_json`, `bytes` |
| Storage, sealer pool, and writer (`store`) | Manages connection pragmas via `connect_options(role)`, seals secrets and content-addressed `trace_blocks` on a dedicated 2-thread `zstd::bulk::Compressor` pool, and serializes writes through a dual-queue writer (`Control` priority queue plus `durable_state`, `rollups`, and `raw_events` data queues). | `sqlx`, `crossbeam-channel`, `zstd`, `chacha20poly1305`, `hkdf` |
| Catalog, health, and `/usage` service (`server`) | Supervises background tasks with exponential-backoff panic recovery: `/models` discovery, single-flight `/usage` polling (8-permit concurrency cap), 60 s pin/WRR/detector sweep, 30 s WAL checkpoint and retention sweeper, and the single `Reloader` task. | `tokio`, `time`, `moka` |
| Control plane (`admin`) | Serves the JSON APIs and server-rendered HTMX web interface for `admin` and `user` roles, applies `project_key` tenant redaction, handles cookie sessions, Argon2id logins, TOTP, CSRF verification, and hash-chained audit logging. | `askama`, `rust-embed`, `argon2`, `pulldown-cmark`, `ammonia` |

### 5.3 Ten-step inference request lifecycle

Every inference request on `/v1/chat/completions`, `/v1/responses`, or `/v1/messages` follows ten sequential steps.

| Step | Stage | Action taken |
| --- | --- | --- |
| 1 | Buffer body and evaluate request gates (`G1`..`G5`) | Read the request body up to `LLMROLLER_MAX_BODY_MB` into a shared `Bytes` buffer, acquire pre-first-byte memory permits from `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` (up to `2,000 ms` queue wait, else `503 gateway_busy`), and evaluate gates `G1` (hash match including rotation grace), `G2` (key active), `G3` (owner active), `G4` (CIDR), and `G5` (RPM bucket). Failed `G1`..`G3` checks record in `auth_failure_counters` rather than `usage_events`. |
| 2 | Validate JSON envelope (`WIRE`) | Identify the endpoint kind (`Inference`, `Auxiliary`, or `StateBound`) and wire protocol (`chat`, `responses`, or `messages`), scan top-level JSON spans rejecting duplicate keys (`400 invalid_request_error`), non-UTF-8 or BOM prefixes, and type-confused `model` or `stream` fields. |
| 3 | Resolve canonical model and evaluate gates (`G6`, `G7`) | Normalize the requested model string, resolve through confirmed `model_aliases`, `model_redirects`, `models`, or unambiguous raw `upstream_model_id` to `model_pk`, and evaluate `G6` (`client_key.allowed_models`) and `G7` (client-key monthly accumulator reservation, returning `429 client_key_budget_exhausted` on exhaustion). |
| 4 | Resolve session identity and routing constraint | Run `resolve_session_and_constraint` (Section 11.1.2): for `responses` requests with `previous_response_id` (or `/v1/responses/{id}/*`), look up `response_links` in `moka` or the SQLite read pool, verify `owner_user_id`, inherit `link.session_hash`, and set `RoutingConstraint::HardKey(link.upstream_key_id)`; otherwise extract the session ID via the eight-step precedence order and compute `session_hash`. |
| 5 | Acquire pin ticket from `PinStore` | For `HardKey(K)`, validate `K` under the hard-route decision table (Section 10.4). Otherwise call `PinStore::acquire(req)` under the entry mutex: follow a valid `Confirmed` or `Provisional` pin (checking `repin_on_personal_recovery` after `recovery_repin_min_idle_secs`), or on miss/ineligibility select a route and insert `Provisional { K, gen, expires = now + 50s }`. |
| 6 | Filter candidates (`C1`..`C8`) and dispatch configured `RoutingStrategy` | When selection runs inside `PinStore::acquire`, evaluate candidate checks `C1` through `C8` (including cache-aware `try_reserve` across key windows, route windows, share monthly cap, and user global cap) across the ordered tier groups for `routing_scope`, resolve the effective `RoutingStrategy` (`models.routing_strategy` or `settings.default_routing_strategy`), and select the winning route via `RouteSelector::select_next` for `(user_id, canonical_id, protocol, pool_tier)`. |
| 7 | Build and send outbound request | Strip client credentials and (on `Shared` and `Global` tiers) all recognized client session carriers, inject the decrypted upstream credential and provider affinity hint, apply surgical JSON edits with `serde_json` string escaping, and connect through `GuardedConnector`. |
| 8 | Hold pre-first-frame bytes and classify failures | Hold upstream status, headers, and up to `64 KiB` of initial bytes until the first valid non-comment frame is verified. If a failover-eligible failure occurs before commit (and constraint is not `HardKey`), record a `usage_attempts` item, update key/route health or `OriginBreaker`, and call `PinStore::pre_first_byte_failure` (adopting the leader's key if a concurrent turn already advanced `generation`) for up to two retries within `failover_wall_clock_budget_ms` (`60 s`). |
| 9 | Commit response, confirm pin, and stream | Upon verifying the first valid frame, release the preflight body budget permit, call `PinStore::first_valid_frame(ticket)` to promote the pin to `Confirmed` and enqueue a durable `pins` upsert (plus a durable `response_links` row on `response.created`), apply the response-header policy (Section 12.1.1), flush the held bytes, and stream chunks to the client while running inline `SseUsageTap` and bounded `TraceTap`. |
| 10 | Finalize via `RequestLifecycle` guard | When the stream completes or aborts (`Completed`, `UpstreamTruncated`, `ClientGone`, `IdleTimeout`, or `ShutdownCut`), `RequestLifecycle::drop` computes actual or estimated cost, updates window rings and monthly accumulators before releasing the in-flight reservation, enqueues `usage_events`, `usage_attempts`, and 5-minute/hourly rollup deltas, and submits any finished turn to the 2-thread trace sealer pool. |

### 5.4 Hot-path state, `Reloader` ordering, and listener isolation

To keep p99 routing latency under 5 ms, the inference hot path never queries SQLite. Only `GET /v1/usage`, `GET /v1/usage/raw`, `GET /v1/key`, `GET /v1/auth/key`, and `response_links` cache misses may query the 8-connection read pool.

Static configuration tables compile into an immutable `ConfigSnapshot` made of per-domain `Arc` slices. These slices cover `users`, `providers`, `upstream_keys`, `client_keys_by_hash`, `models`, `aliases`, `redirects`, `routes`, `shares_by_grantee`, `user_route_prefs`, `prices`, `budget_windows`, `settings`, and `traceability`. A single background `Reloader` task owns snapshot publication:
1. Whenever the writer commits a control-plane transaction that mutates configuration tables, it increments `instance_meta.config_version` in that same transaction and sends a `ReloadRequest { min_version, changed_domains, invalidations }` to the reloader.
2. The reloader coalesces pending requests, reads `config_version` in one read transaction, rebuilds changed domain `Arc` slices, and stores the new snapshot when `v > published.version`.
3. Immediately after the `ArcSwap` store, the reloader applies all coalesced `CacheInvalidationEvent` items to `PinStore`, `WrrState`, and `RuntimeState`. It then notifies awaiting control-plane HTTP handlers so revocations take effect before HTTP 2xx returns.

LLMRoller binds three independent TCP listeners by default, as listed in the table below.

| Listener | Default bind address | Served routes and purpose |
| --- | --- | --- |
| Data plane listener | `0.0.0.0:8080` | Serves `/v1/*`, `/healthz`, and `/readyz` for coding agents. |
| Control plane listener | `127.0.0.1:8081` | Serves `/admin/*`, `/app/*`, `/api/*`, `/setup`, and `/login`. Refuses to start on a non-loopback address without TLS unless `LLMROLLER_TRUSTED_PROXIES` is configured (exit code 2). |
| Metrics listener | `127.0.0.1:9090` | Serves `/metrics` in Prometheus text format on loopback with a closed label allow-list. |

### 5.5 Command-line interface (`llmroller`) contract, lifecycle, and exit codes

The `llmroller` binary in `crates/server` uses `clap` derive macros to expose one daemon command (`serve`), five maintenance subcommands, and `--version`. Every subcommand opens SQLite through `crates/store::pool::connect_options(role)` and returns a deterministic POSIX exit code from the unified table below.

| Subcommand and arguments | Operational behavior |
| --- | --- |
| `llmroller serve [--listen <addr>] [--admin-listen <addr>] [--metrics-listen <addr>] [--data-dir <path>] [--dev-insecure-cookies]` | Runs the ordered startup sequence (Section 5.5.2), binds listeners with `/readyz = 503` until preload completes, supervises background jobs with panic recovery, and on `SIGTERM` or `SIGINT` runs the eight-step graceful drain (Section 5.5.3). |
| `llmroller backup --out <dest_path> [--overwrite] [--data-dir <path>]` | Refuses if `<dest_path>` exists unless `--overwrite` is passed. Opens the active SQLite WAL database in shared mode, runs the SQLite Online Backup API (`sqlite3_backup_init` / `step` / `finish`) into `<dest_path>.tmp` in the destination directory, calls `fsync` on `<dest_path>.tmp`, verifies `PRAGMA integrity_check`, atomically renames `<dest_path>.tmp` to `<dest_path>`, calls `fsync` on the destination directory, and outputs a JSON summary including `master_key_id` (`REL-6`). |
| `llmroller rotate-master-key --old-key-env <VAR> --new-key-env <VAR> --new-key-id <id> [--data-dir <path>]` | Acquires `flock(LOCK_EX \| LOCK_NB)` on `llmroller.lock` (exiting with code `4` if `serve` is running). Verifies the old key unseals `instance_secrets`, sets `instance_meta.rotation_state = 'in_progress'`, and re-seals all rows in 500-row batched `BEGIN IMMEDIATE` transactions across `instance_secrets`, `users` (`totp_secret_enc`), `upstream_keys`, `upstream_usage_snapshots`, `trace_blocks`, and `trace_records`, advancing `rotation_cursor_table` and `rotation_cursor_id` after each batch so interrupted runs resume cleanly (`SEC-1`). Appends an `audit_log` row (`master_key.rotated`) using the unchanged `audit_hmac_key`, sets `active_master_key_id = new_id` and `rotation_state = 'idle'`, and prints reminder instructions to `stderr`. |
| `llmroller verify-audit-log [--anchor-seq <N>] [--anchor-hash <H>] [--data-dir <path>] [--json]` | Unseals `audit_hmac_key` from `instance_secrets` using the master key, streams all rows from `audit_log` ordered by `seq ASC`, verifies that no `seq` gaps exist and that every row's `HMAC_SHA256` matches from the 64-ASCII-zero genesis `prev_hash`, and when `--anchor-seq N --anchor-hash H` is given, verifies that row `N` has hash `H` and `max(seq) >= N` (`SEC-13`). |
| `llmroller reset-admin --username <name> [--data-dir <path>]` | Offline break-glass recovery command. Acquires `flock(LOCK_EX \| LOCK_NB)` on `llmroller.lock`, sets `role = 'admin'`, `disabled = 0`, and `must_change_password = 1` on `<name>`, generates a temporary password printed once to `stderr`, and appends an `audit_log` row (`action = 'admin.break_glass'`, `actor_user_id = NULL`). |
| `llmroller generate-master-key` | Reads 32 bytes from `OsRng`, base64-encodes the key, writes it to `stdout` with a trailing newline, and zeroizes the buffer before exiting. |
| `llmroller healthcheck [--url <http_url>] [--timeout-ms <ms>]` | Container healthcheck subcommand for shell-free distroless images (`Section 22.3`). Issues a loopback HTTP `GET` to `<http_url>` (default `http://127.0.0.1:8080/readyz`, timeout `2,000 ms`) and exits `0` on HTTP `200` or `1` on any non-200 status or connection error. |
| `llmroller --version` | Prints `llmroller <semver> (<git_sha>, schema <n>)` to `stdout` and exits with code `0`. |

#### 5.5.1 Unified CLI exit codes

Every subcommand uses the following non-overlapping exit codes so supervisors and automation scripts can distinguish lock contention from schema or cryptographic errors.

| Exit code | Meaning across all subcommands |
| --- | --- |
| `0` | Success (clean drain, verified backup, completed rotation, intact audit log, or completed admin reset) |
| `2` | Invalid CLI flag, invalid environment variable, or non-loopback plaintext listener rejected at startup |
| `3` | Master key missing, decoded length not equal to 32 bytes, insecure key-file permissions, `rotation_state == 'in_progress'`, `LLMROLLER_MASTER_KEY_ID != active_master_key_id`, or failure to unseal `instance_secrets` |
| `4` | `llmroller.lock` held by another process |
| `5` | TCP listener bind error |
| `6` | Backup destination I/O error or target file already exists without `--overwrite` |
| `7` | AEAD authentication failure on an existing row during `rotate-master-key` |
| `8` | `audit_log` HMAC mismatch or `seq` gap detected |
| `9` | `audit_log` tail truncation detected against `--anchor-seq` and `--anchor-hash` |
| `10` | Target username not found (`reset-admin`) |
| `11` | Graceful shutdown completed with unflushed items remaining in writer or sealer queues after timeout |
| `12` | Controlled shutdown triggered by an unrecoverable SQLite writer panic |
| `13` | SQLite migration run failure or `auto_vacuum != INCREMENTAL` |
| `14` | Database `_llmroller_schema.version` is newer than this binary supports |

#### 5.5.2 Ordered startup sequence (`llmroller serve`)

The `serve` command runs ten startup steps in strict order before marking `/readyz` ready:
1. Acquire `flock(LOCK_EX | LOCK_NB)` on `${LLMROLLER_DATA_DIR}/llmroller.lock` (exit `4` on contention).
2. Validate listener TLS and proxy settings (`LLMROLLER_ADMIN_LISTEN`, `LLMROLLER_LISTEN`, `LLMROLLER_TRUSTED_PROXIES`, `LLMROLLER_REQUIRE_TLS`; exit `2` on invalid configuration) and load the 32-byte master key from `LLMROLLER_MASTER_KEY_FILE` or `LLMROLLER_MASTER_KEY` (exit `3` on error).
3. Open the SQLite pool via `connect_options`, check `_llmroller_schema.version` (exit `14` if newer), run forward-only migrations, and assert `PRAGMA auto_vacuum == 2` (exit `13`).
4. Load or initialize `instance_meta` and `instance_secrets`; exit `3` if `rotation_state == 'in_progress'`, if `LLMROLLER_MASTER_KEY_ID != active_master_key_id`, or if `instance_secrets` fails AEAD decryption.
5. When `users` is empty and a setup token is needed, create or reuse `${LLMROLLER_DATA_DIR}/setup-token` (`0600`) and print only its path to `stderr`.
6. Start the dual-queue SQLite writer actor, the 2-thread OS trace sealer pool, and the `Reloader` task, and build the initial `ArcSwap<ConfigSnapshot>`.
7. Rebuild window states and UTC monthly accumulators from `usage_rollups_5min`, `budget_adjustments`, and `upstream_usage_snapshots` (`< 2 s`). Load persisted `exhausted` and `invalid` health states into `RuntimeState`.
8. Preload up to `max_active_pins` (`250,000`) non-expired confirmed `pins` ordered by `last_used_at DESC` into `PinStore` and preload active `response_links` into `moka`.
9. Bind the data-plane, control-plane, and metrics TCP listeners (exit `5` on bind error) with `/readyz` returning `503` until step 10 finishes.
10. Spawn supervised background tasks (discovery, `/usage` poller, 60 s sweeper, 30 s WAL checkpointer/retention task) and flip `/readyz` to `200 OK`.

#### 5.5.3 Ordered graceful shutdown (`SIGTERM` / `SIGINT`) and job supervision

When `SIGTERM` or `SIGINT` arrives, the server runs an eight-step shutdown sequence:

| Step | Action taken | Time bound |
| --- | --- | --- |
| 1 | Flip `/readyz` to HTTP 503 immediately while continuing to accept connections for `drain_grace_secs` (default `5 s`) so upstream load balancers stop routing new traffic. | `5 s` |
| 2 | Stop accepting new TCP connections on the data-plane and control-plane listeners. | Immediate |
| 3 | Wait for in-flight inference requests and SSE streams to complete naturally. | `drain_timeout_secs` (default `60 s`) |
| 4 | Cancel any remaining open streams with `Termination::ShutdownCut` so `RequestLifecycle::drop` records estimated usage and releases reservations. | Immediate |
| 5 | Stop background tasks (`Discovery`, `UsagePoller`, `RetentionSweeper`, and `Reloader`). | `5 s` |
| 6 | Close the `crossbeam` sealer channel and wait for the 2 sealer threads to finish in-flight jobs. | `min(10 s, remaining drain budget)` |
| 7 | Drain the writer's `Control`, `durable_state`, `rollups`, and `raw_events` queues in that strict priority order. | `15 s` |
| 8 | Run `PRAGMA wal_checkpoint(TRUNCATE);`, close the SQLite pool, release `llmroller.lock`, and exit `0` (or `11` if any queue was not fully flushed). | Immediate |

Each background task (`Discovery`, `UsagePoller`, `RetentionSweeper`, `Reloader`) runs inside `catch_unwind`. If a task panics, the supervisor logs the panic and restarts the task with exponential backoff starting at `1 s` and doubling up to `300 s`. Three panics within 10 minutes mark that job failed (`llmroller_job_failed{job}=1`) and leave it stopped until process restart. The SQLite writer actor is never restarted on panic: a writer panic flips `/readyz` to `503` and triggers immediate controlled shutdown with exit code `12`.

### 5.6 Environment variables and runtime `settings` registry

LLMRoller separates static bootstrap parameters from dynamic runtime policies. Static bootstrap parameters come from environment variables or CLI flags at startup. Runtime policies live in the SQLite `settings` table (Section 9.3.3) and hot-reload into `ArcSwap<ConfigSnapshot>` within 1 s (`REL-4`).

| Environment variable | Type & default | Validation rule and architectural effect |
| --- | --- | --- |
| `LLMROLLER_MASTER_KEY_FILE` | Filesystem path (`None`, preferred) | Path to a file with `0400` or `0600` permissions (group/other bits rejected with exit `3`) containing the base64 master key; mutually exclusive with `LLMROLLER_MASTER_KEY`. |
| `LLMROLLER_MASTER_KEY` | Base64 string (mandatory unless `_FILE` set) | Must decode to **exactly** 32 raw bytes (`SEC-2`, exit `3` if shorter or longer). Because `std::env::remove_var` is `unsafe` in Rust 2024 and the workspace enforces `#![forbid(unsafe_code)]`, startup logs a one-time warning when this env var is used instead of `LLMROLLER_MASTER_KEY_FILE`. |
| `LLMROLLER_MASTER_KEY_ID` | ASCII string (`"mk_default"`) | Identifier checked against `instance_meta.active_master_key_id` at startup (exit `3` on mismatch) and bound into AEAD `AAD` on newly sealed rows (`SEC-1`). |
| `LLMROLLER_DATA_DIR` | Filesystem path (`"/var/lib/llmroller"`) | Directory holding `llmroller.db`, `llmroller.db-wal`, `llmroller.db-shm`, `llmroller.lock`, and `setup-token`. |
| `LLMROLLER_LISTEN` | Socket address (`"0.0.0.0:8080"`) | Data-plane HTTP/HTTPS bind address serving `/v1/*`, `/healthz`, and `/readyz`. |
| `LLMROLLER_ADMIN_LISTEN` | Socket address (`"127.0.0.1:8081"`) | Control-plane HTTP/HTTPS bind address serving `/admin/*`, `/app/*`, `/api/*`, `/setup`, and `/login` (`SEC-9`). |
| `LLMROLLER_METRICS_LISTEN` | Socket address (`"127.0.0.1:9090"`) | Prometheus `/metrics` listener bind address (`OBS-1`, `SEC-9`). |
| `LLMROLLER_TRUSTED_PROXIES` | Comma-separated CIDR list (`""`) | When the TCP peer IP is inside this list, client IP is extracted as the right-most `X-Forwarded-For` entry not in the list, and `x-forwarded-proto: https` is honored (`SEC-5`, `SEC-12`). |
| `LLMROLLER_REQUIRE_TLS` | Boolean (`false`) | When `true`, refuses to start (exit `2`) if `LLMROLLER_LISTEN` is non-loopback without TLS or `LLMROLLER_TRUSTED_PROXIES`. When `false`, logs an error every 10 minutes and sets `llmroller_plaintext_data_plane = 1`. |
| `LLMROLLER_SETUP_TOKEN` | ASCII string (auto-generated to `${LLMROLLER_DATA_DIR}/setup-token`) | One-time bootstrap secret checked by `POST /admin/api/setup` (`AUTH-2`). |
| `LLMROLLER_SETUP_TOKEN_REQUIRED` | Boolean (`true`) | When `false`, skips the setup token check during initial first-admin bootstrap (`AUTH-2`). |
| `LLMROLLER_MAX_BODY_MB` | Integer `1..=128` (`32`) | Maximum inbound HTTP request body size in MiB enforced at the proxy edge; must be `<= LLMROLLER_PREFLIGHT_BODY_BUDGET_MB / 4` (exit `2` otherwise) (`SEC-8`). |
| `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` | Integer (`min(1024, floor(0.40 * memory_limit_mb))`, `819` on 2 GB host) | Total MiB of buffered request bodies allowed simultaneously in the pre-first-valid-frame phase (`PERF-3`, `SEC-8`). |
| `LLMROLLER_SQLITE_SYNCHRONOUS` | Enum `NORMAL \| FULL` (`"NORMAL"`) | Sets `PRAGMA synchronous` on the writer connection (`NORMAL` for standard WAL speed; `FULL` for power-loss durability of every transaction, `REL-2`). |
| `LLMROLLER_TLS_CERT` / `LLMROLLER_TLS_KEY` | Filesystem paths (`None`) | Optional PEM certificate chain and private key enabling native `rustls` HTTPS on the listeners (`A4`). |
| `LLMROLLER_DEFAULT_ROUTING_STRATEGY` | Enum `smooth_weighted_rr \| naive_rr \| weighted_rr \| uniform_random \| weighted_random \| power_of_two_choices` (`"smooth_weighted_rr"`) | Sets the initial instance-wide `default_routing_strategy` in `settings` on first boot (or overrides it at startup when set); invalid strategy strings exit with code `2` (`RTE-4`, `RTE-14`). |
| `LLMROLLER_MEMORY_LIMIT_MB` | Integer `256..=65536` (auto-detected from cgroup v2 `memory.max` or `2048`) | Explicit container memory ceiling in MiB used to size `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` and the `256 MiB` trace accumulator cap (`PERF-3`, `Section 22.3`). |
| `LLMROLLER_DEFAULT_ROUTING_STRATEGY` | Enum `smooth_weighted_rr \| naive_rr \| weighted_rr \| uniform_random \| weighted_random \| power_of_two_choices` (`"smooth_weighted_rr"`) | Sets the initial instance-wide `default_routing_strategy` in `settings` on first boot (or overrides it at startup when set); invalid strategy strings exit with code `2` (`RTE-4`, `RTE-14`). |
| `LLMROLLER_MEMORY_LIMIT_MB` | Integer `256..=65536` (auto-detected from cgroup v2 `memory.max` or `2048`) | Explicit container memory ceiling in MiB used to size `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` and the `256 MiB` trace accumulator cap (`PERF-3`, `Section 22.3`). |
| `LLMROLLER_LOG` | Filter string (`"info,llmroller=info"`) | Structured JSON log level filter passed to `tracing_subscriber::EnvFilter` (`OBS-1`). |

## 6. Functional requirements

This section specifies all functional requirements for LLMRoller version 1.0. Every requirement has a unique ID, a priority (`P0`, `P1`, or `P2`), and a clear pass or fail condition linked to Section 16 and `requirements.toml`.

### 6.1 Setup, accounts, and authentication (AUTH)

The authentication subsystem controls initial instance bootstrap, password and two-factor verification, and web session lifecycles.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| AUTH-1 | P0 | When zero rows exist in the `users` table, the control plane must serve a setup flow (`GET /setup`, `POST /admin/api/setup`) that creates the first `admin` account inside a serialized writer transaction so 20 concurrent submissions yield exactly one HTTP 201 and `SELECT COUNT(*) FROM users == 1`. |
| AUTH-2 | P0 | Initial setup must validate a one-time setup token read from `LLMROLLER_SETUP_TOKEN` or auto-generated to `${LLMROLLER_DATA_DIR}/setup-token` with `0600` permissions (`O_EXCL`, reused across restarts until setup completes and deleted upon completion, printing only the file path to `stderr`), unless `LLMROLLER_SETUP_TOKEN_REQUIRED=false`. |
| AUTH-3 | P0 | Once at least one user row exists in the database, all `/setup` routes must return HTTP 404 permanently, with no configuration flag able to re-enable them. |
| AUTH-4 | P0 | An admin must be able to create, disable, enable, and delete user accounts with role `admin` or `user`, and set a temporary password that forces a password change at first login. Deleting a user (`DELETE /admin/api/users/{id}?confirm=<username>`) must run in one transaction that disables all shares granted by that user, deletes their client keys, personal providers, keys, pins, traces, and web sessions, retains `usage_events` and rollups, and records a `deleted_users(user_id, username, deleted_at)` row. |
| AUTH-5 | P0 | The writer transaction for any user role change, disable, or delete must verify `SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0` inside `BEGIN IMMEDIATE` and roll back with HTTP 409 (`last_admin`) if the count would reach zero. Offline recovery is provided by `llmroller reset-admin --username <name>`. |
| AUTH-6 | P0 | Passwords must be at least 12 characters long and hashed with Argon2id (`m >= 19456 KiB`, `t >= 2`, `p = 1`), with concurrent verifications bounded by a semaphore (`max(2, available_parallelism)` permits, 5 s queue timeout returning `503 login_busy`). |
| AUTH-7 | P0 | Web login (`POST /api/auth/login`) must issue a 256-bit random session token in an `HttpOnly`, `SameSite=Strict` cookie (stored as a SHA-256 hash in `web_sessions`) plus a random 256-bit CSRF token (stored as `csrf_hash = SHA-256(token)`), and logout (`POST /api/auth/logout`) must delete the server-side session row immediately. |
| AUTH-8 | P0 | Login and setup endpoints must enforce rate limiting keyed by client IP (or `/64` for IPv6: 20 failures per 15 minutes -> HTTP 429) and by `(username, IP)` pair (5 failures per 15 minutes -> HTTP 429). Across all IPs for one username, exceeding 50 failures per hour must enforce TOTP (if enrolled) or add a 2 s constant delay rather than a hard username lockout so attackers cannot lock out an admin from a new IP. |
| AUTH-9 | P1 | Both `admin` and `user` accounts should support TOTP two-factor authentication (`RFC 6238`) with recovery codes, and admins should be able to reset a user's password and revoke their active web sessions. |

### 6.2 User roles and access control (USR)

The user role subsystem enforces multi-tenant isolation between standard users and admins across the control plane and data plane.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| USR-1 | P0 | The system must support two human user roles (`admin` and `user`) and one internal system actor variant (`ActorScope::System { task: SystemTask }`). Every control-plane request must resolve an `ActorScope::User { user_id, role }`. |
| USR-2 | P0 | A standard `user` must be able to create, view, edit, test, calibrate, and delete their own `personal` providers and upstream keys, manage outgoing and incoming shares, set `user_route_preferences` on own and shared routes, manage their own client keys (including inspecting recently active sessions from each client key and which provider and upstream key each session is pinned to right now), and view their own usage and active pins. |
| USR-3 | P0 | A standard `user` must receive HTTP 404 (`not_found`) for any resource they are not authorized to read (whether or not the ID exists, preventing existence oracles), and HTTP 403 (`permission_error`) only when trying to modify a resource they are authorized to read (such as a shared key or global catalog entry). |
| USR-4 | P0 | Every user-facing API and template must pass upstream key metadata through `project_key(viewer, key)` so a standard `user` never sees raw secrets, `secret_last4`, key IDs, administrative labels, priorities, or `health_until` of `global` upstream keys (which render as `"global pool"`), nor `secret_last4` or raw `/usage` JSON of shared keys. |
| USR-5 | P0 | An `admin` must be able to set `can_use_global_providers` (boolean, default true) and `global_budget_micro_usd_month` (optional integer micro-USD cap enforced via the UTC monthly accumulator) on each `user` account. |

### 6.3 Global and personal providers and upstream keys (PRV)

The provider subsystem manages both admin-owned global providers and user-owned personal providers, along with their upstream API keys.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| PRV-1 | P0 | Every provider record must have an `ownership_scope` of `global` (creatable and editable only by `admin`) or `personal` (creatable and editable by its `owner_user_id` or an `admin` via `/admin/api/*`), with unique names enforced by partial unique indexes `uq_providers_global_name` and `uq_providers_personal_name`. |
| PRV-2 | P0 | Users and admins must be able to add providers from built-in presets (`opencode_go`, `command_code`, `open_design_amr`, `openrouter`) or as a `custom` OpenAI or Anthropic compatible provider with a configurable base URL. |
| PRV-3 | P0 | Each provider record must store editable settings in `ProviderConfigJson`: base URL, protocol paths (`chat`, `responses`, `messages`, `models`), `aux_endpoints` (`count_tokens`, `responses_compact`), auth scheme per protocol, `usage_reporting.chat_stream` (`inject`, `native`, `none`), `/usage` profile, `strip_headers`, `strip_header_tokens`, `add_headers`, `response_header_allow`, `ua_policy`, `session_profile`, `timeouts`, and five-class `error_classifiers`. |
| PRV-4 | P0 | A provider must support multiple upstream keys. Each key has a label, encrypted secret, last-four suffix, enabled flag, default priority weight (`1` to `1000`), optional strict tier (`1` to `10`), and versioned health state (`healthy`, `cooling_down`, `exhausted`, `invalid` with `health_version`). |
| PRV-5 | P0 | Upstream key secrets must be encrypted at rest with XChaCha20-Poly1305 using canonical length-prefixed `AAD` before database insertion and never returned in any API or UI response after creation. |
| PRV-6 | P0 | A Test action on an upstream key must call the provider's `/models` endpoint (and `/usage` endpoint if configured, subject to a 10 s per-key minimum interval) using that key and report HTTP status, discovered model count, and current upstream usage summary. |
| PRV-7 | P0 | Custom provider base URLs and usage URLs must use `https://`, reject non-standard IP encodings at save time, disable HTTP redirects, and validate every connected socket IP (both DNS-resolved names and IP literals) through `GuardedConnector` (`SEC-7`) unless an admin enables `allow_private_network = 1`. Cross-origin `usage_url` on a personal provider needs `usage_url_cross_origin_ack = true` and forbids sharing that provider. |
| PRV-8 | P0 | Disabling a provider, disabling an upstream key, or disabling the owner account (`users.disabled = 1`) must immediately exclude affected routes from new sessions and purge or re-pin active sessions via `CacheInvalidationEvent`. |

### 6.4 Personal provider and key sharing (SHR)

The sharing subsystem lets any user share their personal providers or personal upstream keys with specific peers on the same instance.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| SHR-1 | P0 | The owner of a `personal` provider (or an `admin`) must be able to create a share grant (`provider_shares`) granting a specific `grantee_user_id` access to either the entire personal provider (`upstream_key_id IS NULL`) or one specific personal upstream key. Creating a key-level share when an active provider-level share exists for the same grantee and provider (or vice versa) must be rejected with HTTP 409 `share_overlap`. |
| SHR-2 | P0 | Each share grant must support an optional list of `allowed_models` (`NULL` means all models served) and an optional monthly spend cap (`max_monthly_micro_usd`). |
| SHR-3 | P0 | A share grant must remain inactive until the grantee accepts it (`POST /api/user/shares/{id}/accept`) echoing the provider's current `share_config_fingerprint` (and until an admin approves it via `POST /admin/api/shares/{id}/approve` when `provider.kind == 'custom'`). Any change to the provider's routing-relevant configuration updates `share_config_fingerprint` and suspends active shares until re-accepted. |
| SHR-4 | P0 | A grantee must not be able to re-share a shared key with a third user, edit the shared key's settings, view its secret or `secret_last4`, receive account-identifying upstream response headers, or view other users' usage on that key. |
| SHR-5 | P0 | When a share grant is revoked, suspended by a config change, or reaches its monthly spend cap, the in-memory state must update within 1 s, deleting the grantee's `user_route_preferences` on revocation and forcing any session pinned via that share to re-pin on its next request. |
| SHR-6 | P0 | The personal key owner must see a per-grantee breakdown of requests, tokens, and cost (including charged failed tries) consumed via each share grant (`effective_share(U, K)`) on their key. |

### 6.5 Client keys (CLK)

Client keys authenticate coding agents to the data plane and carry user-defined routing preferences and safety caps.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| CLK-1 | P0 | Any `admin` or `user` must be able to generate client keys (`llr_` followed by 32 base62-encoded random bytes) scoped to their account, with a label, optional expiry timestamp, and optional `allowed_models` list. |
| CLK-2 | P0 | The full client key string must be displayed once at creation or rotation. The database stores `key_hash`, `prefix`, and optional rotation grace columns `prev_key_hash` and `prev_key_expires_at` (with disjoint uniqueness enforced by `uq_client_keys_prev_hash` and `trg_client_keys_hash_disjoint`). Rotating a client key (`POST /api/user/client-keys/{id}/rotate`) updates `key_hash` in place on the same `client_key.id` (preserving active `pins` and `response_links`) and keeps the previous hash valid for `grace_period_secs` (`0..=86400`, default `300 s`) unless revoked or disabled. |
| CLK-3 | P0 | Each client key must have a configurable `routing_scope` policy (`inherit`, `personal_first`, `blended`, `personal_only`, `own_only`, or `global_only`, defaulting to `inherit` which resolves to `users.default_routing_scope`, default `personal_first`) that governs how the router orders the `Own`, `Shared`, and `Global` upstream key tiers. |
| CLK-4 | P0 | Revoking or disabling a client key must clear `prev_key_hash`, update `ArcSwap<ConfigSnapshot>` via the reloader before the HTTP response returns (within 1 s), purge its pins, and reject later data-plane calls with HTTP 401. |
| CLK-5 | P1 | Each client key should support an optional request-per-minute rate limit (`G5`, returning 429), a monthly micro-USD budget cap (`G7`, returning 429 `client_key_budget_exhausted` with `Retry-After` until next UTC month), and an optional CIDR IP allow list (`G4`, returning 403). |

### 6.6 Model catalog and discovery (MDL)

The model catalog normalizes model identifiers across providers and determines which models are exposed to each user.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| MDL-1 | P0 | LLMRoller must discover available models for each enabled upstream key by calling the provider's `/models` endpoint on key creation, on manual refresh, and every 6 hours. Upstream model IDs not matching `^[!-~]{1,256}$` are skipped with a warning. Responses exceeding 2 MiB or 1,000 models are marked `partial` (adding/reactivating routes without marking absent routes `missing`). Complete responses transition absent routes to `status = 'missing'` (`missing_since = now`), and after 7 days to `retired`. |
| MDL-2 | P0 | Discovered upstream model IDs must be normalized using the deterministic algorithm in Section 10.6 (preserving every dot between two ASCII digits, keeping `:free`, `:thinking`, `:extended`, and `:beta` as distinct suffixes, and resolving within-key collisions by keeping the vendor prefix with `--`). Cross-key merges with differing `vendor_prefix` values enter `merge_state = 'pending_review'` until confirmed by an admin. |
| MDL-3 | P0 | For the global pool, a model with `exposed = NULL` is exposed by default when at least one enabled global provider with an enabled, non-`invalid` key serves it (`global_exposure_mode = "union"`, configurable to `"intersection"` in `settings`, where global providers with zero usable keys are ignored so they do not veto). |
| MDL-4 | P0 | Each canonical model has an immutable surrogate primary key `model_pk` and a unique `canonical_id` string. An admin can force-enable (`exposed = 1`) or force-disable (`exposed = 0`, which acts as an instance-wide kill switch across `Own`, `Shared`, and `Global` tiers) any model, and can transactionally merge or split models over `model_pk` and `model_redirects` without cascading deletion of pins, windows, preferences, or response links. |
| MDL-5 | P0 | Personal provider discovery creates `models` rows with `origin = 'personal'` (capped at 2,000 personal-origin models per user; upgraded to `origin = 'global'` if later discovered by a global provider). Models on enabled personal or shared keys are exposed to authorized users unless `exposed = 0` or disabled by the user. |
| MDL-6 | P0 | Confirmed `model_aliases` (which cannot shadow an existing `canonical_id`) apply both during discovery route attachment and during request resolution. Client model strings are normalized first, then resolved via confirmed `model_aliases` -> `model_redirects` -> `models.canonical_id` -> unambiguous exact `upstream_model_id` (returning 409 if multiple canonicals share that raw ID). |
| MDL-7 | P0 | `GET /v1/models` must return canonical models passing checks `C1` through `C5` for the caller (including `exposed = 1` models with no active route marked `"llmroller":{"available":false}`) plus confirmed aliases (`"llmroller":{"alias_of":"<canonical>"}`) in OpenAI list format, or in Anthropic list format when `anthropic-version` is present. |

### 6.7 Routing and session pinning (RTE)

The routing engine pins existing conversations to their upstream key and distributes new conversations by weighted round-robin.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| RTE-1 | P0 | Inbound session IDs must be resolved by the eight-step precedence order in Section 11.1 (with canonical byte-sliced opening-message fingerprinting at Step 8 and the 21-slot exact `ConstantSessionDetector` for Steps 4 through 7) after checking Response-link session inheritance (Section 11.1.2), and hashed with HMAC-SHA256 (`session_hash`). |
| RTE-2 | P0 | A session pin in `PinStore` binds `PinKey = (client_key_id, canonical_id, session_hash)` (persisted with `model_pk`) to one `upstream_key_id`, `pool_tier`, and `generation`. While the pinned route passes checks `C1` through `C8`, every turn in that session must route to that `upstream_key_id` unless a recovery repin (`repin_on_personal_recovery`) fires after the pin has been idle for at least `recovery_repin_min_idle_secs` (default 300 s, `pin_outcome = 'repin_recovery'`). |
| RTE-3 | P0 | Every request must first pass ordered request gates `G1` through `G7` (Section 10.0), and a candidate route `R = (K, M)` is eligible only when all eight candidate checks `C1` through `C8` (Section 10.1) hold. |
| RTE-4 | P0 | When no usable pin exists, the router must select a route from the first non-empty tier group under `routing_scope` through the pluggable `RouteSelector` trait (`Section 10.3`), defaulting to per-group smooth weighted round-robin (`smooth_weighted_rr`, `WrrState` keyed by `(user_id, canonical_id, protocol, pool_tier)` under a mutex, evicting members absent for `>= 300 s` and preserving the zero-sum weight invariant without clamping). |
| RTE-5 | P0 | A route's effective priority `W(U, r)` is resolved in three steps: (1) for `Own` and `Shared` routes only, `user_route_preferences.priority_override` (`1` to `1000`) if set, (2) `model_routes.priority_override` if set, and (3) `upstream_keys.priority`. Standard users cannot override weights on `Global` routes. |
| RTE-6 | P0 | Each canonical model must support two distribution modes: `weighted` (applies the active `RoutingStrategy` across all eligible routes in the active tier group) and `strict` (filters candidates in the active tier group to the lowest numeric `strict_tier` value, where `1` is highest priority and `10` is lowest, before applying the active `RoutingStrategy` within that tier). |
| RTE-7 | P0 | Concurrent requests for the same `PinKey` must synchronize through the per-entry `parking_lot::Mutex<PinEntry>` in `PinStore` (Section 10.4): the first request runs the selector (`< 5 us`) and inserts `Provisional { K, gen = 1, expires = now + 50_000 }`, while followers share `(K, gen)`. On pre-first-byte failure, the first task to report failure advances `gen` and reselects excluding `tried_this_generation`, while tasks with stale `ticket.generation < gen` adopt the new `(K, gen)` instead of scattering across keys. |
| RTE-8 | P0 | A provisional pin is promoted to `Confirmed` under the entry mutex only after the upstream returns HTTP 200 and its first non-comment SSE `data:` frame (or JSON body) is verified free of an embedded error object. Provisional pins exist only in memory; every row persisted to the SQLite `pins` table is confirmed. |
| RTE-9 | P0 | If an upstream try fails with a failover-eligible outcome (Section 10.5.2) before the first valid response frame, LLMRoller must retry on up to two other eligible routes while the cumulative `failover_wall_clock_budget_ms` (`60 s`) is not exhausted. Wait-timeout outcomes (`first_activity_timeout_ms = 60 s`) are failover-eligible only for streaming requests on providers with `timeout_failover = true`. After the first valid frame is flushed to the client, or after any non-streaming request is accepted, LLMRoller must never retry. |
| RTE-10 | P0 | Confirmed pins in `PinStore` (capped at `max_active_pins = 250,000`; overflow routes unpinned with `pin_outcome = 'unpinned_capacity'`) must expire after `pin_ttl_secs` (default 24 hours, checked via the `Clock` seam so `FakeClock` tests run deterministically) and persist to SQLite via the non-dropping `durable_state` queue. |
| RTE-11 | P0 | When a confirmed pin fails any check `C1` through `C8` (or is expired), `PinStore::acquire` reselects under the entry mutex so concurrent turns of the same session coalesce onto one new key, increments `repin_count`, and records `pin_outcome = 'repin'`. |
| RTE-12 | P0 | For OpenAI Responses requests, LLMRoller must create a durable `response_links` row (Section 9.1) for every upstream response ID at the moment the ID is first observed (streaming `response.created` or non-streaming JSON body) via the `durable_state` queue. Follow-up calls carrying `previous_response_id`, and `GET`, `DELETE`, `/input_items`, and `/cancel` calls on `/v1/responses/{id}`, must inherit the link's `session_hash`, route only to the link's `upstream_key_id` (`RoutingConstraint::HardKey`) under the decision table in Section 10.4, and must never fail over to another key. |
| RTE-13 | P1 | Headroom mode should optionally scale each route's effective weight by `bps = clamp(min_w floor(10_000 * max(0, w.limit - w.used - w.reserved_in_flight) / w.limit), 500, 10_000)` across all key and route windows (`W_eff = max(1, W(U, r) * bps / 10_000)`) for all weighted strategies (`smooth_weighted_rr`, `weighted_rr`, `weighted_random`, `power_of_two_choices`). |
| RTE-14 | P0 | The routing engine must support six configurable `RoutingStrategy` algorithms selectable globally (`settings.default_routing_strategy` and `LLMROLLER_DEFAULT_ROUTING_STRATEGY`, default `smooth_weighted_rr`) and overridable per canonical model (`models.routing_strategy`): `smooth_weighted_rr` (Nginx SWRR), `naive_rr` (lock-free `AtomicU64` cyclic `idx = (cursor.fetch_add(1) % N)` ignoring weights), `weighted_rr` (classical GCD block-weighted cycle), `uniform_random` (alias `random`, lock-free thread-local PRNG seeded from `EntropySource` ignoring weights), `weighted_random` (`O(log N)` cumulative-weight binary search over `SmallVec<[i64; 16]>`), and `power_of_two_choices` (alias `p2c`, sampling two distinct weighted candidates and picking minimal `((in_flight + 1) * ewma_ttfb_ms * 1000) / W_eff`). Switching strategies via API or settings must hot-reload within 1 s via `Reloader`, preserve stored weights when unweighted strategies are active, and keep existing confirmed pins intact. |

### 6.8 Transparent proxying (API)

The data plane forwards inference calls across all three supported protocols with minimal mutation and zero response buffering after the first valid frame.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| API-1 | P0 | The data plane must serve `POST /v1/chat/completions`, `POST /v1/responses`, `GET /v1/responses/{id}`, `DELETE /v1/responses/{id}`, `GET /v1/responses/{id}/input_items`, `POST /v1/responses/{id}/cancel`, `POST /v1/responses/compact`, `POST /v1/messages`, `POST /v1/messages/count_tokens`, `GET /v1/models`, `GET /v1/usage`, `GET /v1/usage/raw`, `GET /v1/key`, `GET /v1/auth/key`, `GET /healthz`, and `GET /readyz`. |
| API-2 | P0 | Client authentication (`Authorization: Bearer llr_...` or `x-api-key: llr_...`) and hop-by-hop headers must be stripped and replaced with the chosen upstream key's credential header. |
| API-3 | P0 | Non-credential headers (`User-Agent`, `anthropic-version`, `anthropic-beta`, `openai-beta`, `originator`, `x-claude-code-*`, `x-codex-*`, `x-opencode-*`, `x-session-*`, `x-stainless-*`, `x-cmd-zdr`) must pass through with preserved values and per-name value order (lowercase header names) unless removed by `strip_headers`, `strip_header_tokens`, or Section 11.2 session-carrier stripping on `Shared` and `Global` keys. |
| API-4 | P0 | Request JSON bodies must be validated as UTF-8 objects with no duplicate top-level keys (`400` on duplicate keys) and forwarded with byte-identical message and tool ordering, changing only `model` (JSON-escaped `upstream_model_id`), the provider's configured session field (preserving `Own` or overwriting with the salted token on `Shared`/`Global`), and `stream_options.include_usage: true` when `usage_reporting.chat_stream == 'inject'`. |
| API-5 | P0 | When LLMRoller injects or flips `stream_options.include_usage: true` on a streaming `chat` request (`strip_injected_usage_chunk = true`), the forwarder's 64 KiB frame filter must feed any frame containing `"choices":[]` and an object `"usage"` to `SseUsageTap` and suppress that frame from the client stream. |
| API-6 | P0 | Upstream status and headers are held (up to 64 KiB) until the first valid frame is verified, then filtered through the response-header pipeline (Section 12.1.1, stripping `content-length`, `content-encoding`, `set-cookie`, and on `Shared`/`Global` tiers stripping org and rate-limit headers while adding `x-llmroller-request-id` and `x-llmroller-upstream-scope`), after which chunks stream without buffering. |
| API-7 | P0 | Every inference stream must be wrapped in an RAII `RequestLifecycle` guard: if the client disconnects, `hyper` drops the stream, aborting the upstream request within 1 s and finalizing usage (`termination = 'client_gone'`), reservations, preflight permits, probe slots, and traces in `Drop`. |
| API-8 | P0 | Upstream errors must be classified by the five-class ordered classifier (Section 10.5): `QuotaExhausted`, `RequestRejected` (including content-moderation 403s, passed through verbatim with zero health effect), `PlanRestricted` (`upgrade_required`, blocking `(K, M, protocol)` until next `/models` refresh), `CredentialInvalid` (401 or credential 403, marking a key `invalid` only after strikes from `>= 2` distinct client keys within 10 minutes or a failing `GET /models` verification probe), and `Unclassified403` (passed through verbatim). When all failover tries fail, the terminal error selector (Section 10.5.3) returns 429, 504, or 502. |

### 6.9 Usage, budgets, and source provider `/usage` pass-through (USG & LIM)

The usage and budget subsystem combines local token and cost accounting with live source provider `/usage` polling, decaying observation reconciliation, and client or UI pass-through.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| USG-1 | P0 | Every request that passes authentication gates `G1` through `G3` and completes or fails on an inference or state-bound endpoint must emit a `usage_events` row (`client_key_id NOT NULL`, `cost_micro_usd NOT NULL`, `endpoint`, nullable `protocol`, and `termination`) plus a `usage_attempts` row for each upstream try (`attempt_no = 1..3`, charging `estimated_input` on post-write 5xx, wait timeouts, and initial error frames). Failed `G1`..`G3` checks are recorded in `auth_failure_counters`. |
| USG-2 | P0 | Token extractor logic must normalize cache accounting across all three protocols using Section 12.5: subtracting both `cache_read` and `cache_write` from `prompt_tokens` or `input_tokens` on OpenAI `chat` and `responses` (or using `prompt_cache_miss_tokens` when present on `chat`), and taking the last observed `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, and `output_tokens` across `message_start` and `message_delta` on Anthropic `messages`. |
| USG-3 | P0 | Request cost must use finite provider-reported `usage.cost` in `[0.0, 1000.0]` when present (`cost_source = 'provider'`), else compute integer micro-USD from `prices` (`cost_source = 'price_book'`) using non-overlapping context tiers and half-open `[start, end)` UTC schedules active at request start, else apply `unpriced_fallback_rates` (`$3/$15/$0.30/$3.75` per MTok, `cost_source = 'fallback_rate'`). Aborted streams (`incomplete = 1`) must estimate missing input tokens via the binary-aware JSON string scanner (`1,600` tokens per binary/base64 block `> 1 KiB` plus `ceil(text_bytes / 4)`) and missing output tokens from accumulated decoded text deltas (`ceil(decoded_text_bytes / 4)`, `cost_source = 'estimated_incomplete'`). |
| USG-4 | P0 | For every provider with a supported or custom `/usage` endpoint, LLMRoller must poll the endpoint with per-key single-flight deduplication (`poll_in_flight: AtomicBool`), a 30 s per-key minimum automatic poll interval (10 s for owner/admin manual refresh), and a global 8-permit concurrency semaphore with `±20%` schedule jitter. |
| USG-5 | P0 | Each upstream `/usage` poll must capture `local_at_observation` for each window at poll start `t0` and store an `upstream_usage_snapshots` row containing AEAD-sealed `raw_json_enc`, allow-list `sanitized_json`, and normalized window fields. |
| USG-6 | P0 | Each normalized upstream window (`5h`, `7d`, `1mo`) updates the matching local key-scoped window (`upstream_window_kind`) with an `UpstreamObservation { used_micro_usd, observed_at_ms: t0, local_at_observation, resets_at_ms }`. Effective window usage is `max(local, o.used_micro_usd + max(0, local - o.local_at_observation))` while `now < min(resets_at_ms, observed_at_ms + 1_800_000)` (30 minutes), decaying automatically to the local ring if polling stalls. Exhausted snapshots set `health = exhausted` until `coalesce(resets_at_ms, now + 300_000)`. |
| USG-7 | P0 | The data plane must serve `GET /v1/usage`, `GET /v1/usage/raw`, `GET /v1/key`, and `GET /v1/auth/key` authenticated by a client key, resolving session context exclusively from request headers (`x-llmroller-session-id` or precedence rows 1 through 6; `?session_id=` query strings are rejected with HTTP 400). `?fresh=1` triggers a live poll only when the caller is the key owner (`Own` tier) or an admin; `/v1/usage/raw` always serves the latest stored snapshot. |
| USG-8 | P0 | To protect multi-tenant privacy on `GET /v1/usage` and `/v1/usage/raw`, a standard user's client key receives decrypted `raw_json` only for the user's own `personal` upstream keys; for `Shared` and `Global` keys, responses return only allow-list `sanitized_json` (`SEC-18`). |
| LIM-1 | P0 | Admins (for global keys) and users (for own personal keys) must be able to configure `rolling` (`1 h` to `31 d` in 360-bucket rings), `anchored_fixed`, `anchored_calendar_month` (with day-of-month clamping across short months), and `anchored_provider` budget windows in micro-USD on each upstream key and route, with a configurable admission reserve (`0..=50%`, default `2%`), cache-aware in-flight reservations (`try_reserve`), and window-instance-bound manual calibration adjustments (`budget_adjustments`). |

### 6.10 Content traceability mode and session chat preview (TRC)

The content traceability subsystem lets an admin record full request and response contents using content-addressed block deduplication. It also renders a read-only multi-turn chat preview per pinned session.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| TRC-1 | P0 | Content traceability mode must be disabled (`traceability_config.enabled = false`) by default and evaluated once per request at request start (`match: { user_ids, client_key_ids, models }`, where a request is traced iff every non-empty list matches, and `auto_disable_at` is respected and persisted by the 60 s sweeper). Every change via `PUT /admin/api/settings/traceability` writes an `audit_log` entry. |
| TRC-2 | P0 | When traceability is active for a request, `TraceTap` receives chunk clones via a bounded per-request channel (`256` slots, `try_send`) under a `64 MiB` global `trace_inflight_bytes` budget and a `256 MiB` global output accumulator budget (1 MiB per stream), replacing binary/base64 blocks (`> 1 KiB`) with SHA-256 placeholders and capping new request blocks at 4 MiB (keeping newest) and outputs at 1 MiB (keeping first 512 KiB and last 512 KiB). |
| TRC-3 | P0 | For streaming SSE responses across `chat`, `responses`, and `messages`, `TraceTap` reassembles text, reasoning, and tool-call deltas into a `NormalizedTurnOutput` and submits a `SealJob` at turn end to the fixed 2-thread OS sealer pool (`crossbeam_channel::bounded(64)` with reusable `zstd::bulk::Compressor`), never allocating per-stream zstd encoders or delaying client chunk forwarding (`PERF-2`, `PERF-5`). |
| TRC-4 | P0 | Each turn is stored as self-contained content-addressed blocks in `trace_blocks` (`PRIMARY KEY (owner_user_id, block_hash)`) linked via `trace_record_blocks` and a `trace_records` row (`StoredTurnTrace v2` containing `request_skeleton` with `{"$blk":"<hash>"}` references, `block_refs`, and `response`), ordered by immutable `(ts, request_id)` and tagged with `thread_id` (`subagent_id` or `'main'`). |
| TRC-5 | P0 | The read-only chat preview endpoints (`GET /admin/api/sessions/{session_hash}/chat` and `GET /api/user/sessions/{session_hash}/chat`) must paginate by cursor (`?after=<ts>:<request_id>&limit=50`, loading one predecessor row per page and capping decrypted bytes at 8 MiB per page) and compute Longest Common Prefix (`LCP`) directly over `block_refs` hash arrays without decrypting unchanged prefix blocks. |
| TRC-6 | P0 | The Session Chat Preview response and UI must carry `canonical_model` and `protocol` per turn, `current_pins` (live `PinStore` provider and `project_key`-redacted upstream key bindings right now) per session, and `system_instructions` and `tools` per thread (with `system_changed`), pass key labels through `project_key`, render assistant Markdown via `pulldown-cmark` (raw HTML disabled) + `ammonia`, and render reasoning, user text, tool arguments, tool results, and raw JSON as auto-escaped text inside `<pre>`. |
| TRC-7 | P0 | Admins must be able to configure `traceability_config.retention_days` (`1..=90`, default 14 days; lowering `retention_days` returns HTTP 409 with the affected row count unless `"confirm_purge": true` is sent) and purge traces globally or per session (`DELETE /admin/api/traces`, `DELETE /admin/api/sessions/{session_hash}/trace`), deleting unreferenced `trace_blocks` while leaving `usage_events` intact. |
| TRC-8 | P0 | LLMRoller never exposes a grantee's recorded traces or `trace_blocks` to a share grantor through any LLMRoller interface (`owner_user_id` isolation). Grantees must explicitly accept each share after viewing the destination endpoint disclosure (`SHR-3`), as LLMRoller cannot prevent the operator of the external endpoint or upstream account from observing traffic sent to it. |
| TRC-9 | P0 | For any client key (`llr_...`), the control plane (`GET /api/user/client-keys/{id}/sessions` for the key owner and `GET /admin/api/client-keys/{id}/sessions` for admins, cursor-paginated up to 50 sessions per read transaction) must list recently active sessions (`session_hash IS NOT NULL`, default trailing 24 hours) ordered by `last_active_at DESC`, joining logged request telemetry from `usage_events` and `trace_records` (`request_count`, `error_count`, tokens, `cost_micro_usd`, `traced_turn_count`, `has_traces`, `first_active_at`, `last_active_at`, and `last_served_route`) with live `PinStore` state (`current_pins`: per-model `pin_state`, `pool_tier`, `provider_id`, `provider_name`, `provider_kind`, `provider_scope`, `project_key`-redacted `upstream_key_id`/`upstream_key_label`, `expires_ms`, and `repin_count`, or empty when unpinned/expired). |

### 6.11 Web UI and audit logging (UI & AUD)

The web UI and audit subsystem provide full self-service and administrative visibility without external JavaScript dependencies.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| UI-1 | P0 | The embedded web UI must provide role-aware navigation: admins see both the Admin Console (`/admin/*`) and Personal Workspace (`/app/*`), while standard users see the Personal Workspace (`/app/*`) covering Overview, Personal Providers & Keys, Shared With Me (with share acceptance disclosure), Share Grants, Routing Overrides (with aggregated `"Global pool"` column), Client Keys, Active Sessions, and Usage Explorer. |
| UI-2 | P0 | The UI must display both local window ring spend and reconciled `UpstreamObservation` gauges side-by-side on each upstream key card, projected disk usage (`A6`), live key health from `RuntimeState`, a "Refresh `/usage`" button, and a modal to inspect decrypted raw `/usage` JSON (for authorized personal owners and admins). |
| UI-3 | P0 | All HTML templates, CSS, HTMX (`includeIndicatorStyles = false`, `allowEval = false`, `allowScriptTags = false`), and chart scripts (`uPlot`) must be compiled into the Rust binary with zero external CDN requests and zero `hx-on:*` or `js:` attributes. |
| UI-4 | P0 | When content traceability mode is active (or historical trace rows exist for a session), the Client Keys (Recently Active Sessions drawer), Active Sessions, and Usage Explorer screens must display each session's live pinned provider and upstream key (`project_key`-redacted) right now, a "View Requests" filter link, and a "Preview Chat" action that opens the read-only Session Chat Preview for that `session_hash`, plus a persistent global banner whenever traceability is enabled. |
| AUD-1 | P0 | Every state-changing action by an `admin`, `user`, or `SystemTask` (plus admin chat-trace views, deduplicated to one `trace.viewed` row per `(admin, session_hash)` per 30 minutes) must append an HMAC-SHA256 hash-chained row (`seq`, `prev_hash`, `hash`) to `audit_log` inside the same transaction without logging secrets, and emit an `audit_anchor` structured log line to `stdout` every 100 rows and at least hourly. |

## 7. Non-functional requirements

LLMRoller must add less than 5 ms of proxy overhead at p99 before the first upstream byte. It must forward every complete streamed frame within 1 ms and preserve pins, response links, and committed usage rollups across restarts.

### 7.1 Latency and capacity (PERF)

Speed and capacity targets apply to a reference host with 2 vCPU and 2 GB RAM running Linux `x86_64` against a local stub upstream. The benchmark harness in Section 16 verifies each target against both a zero-delay stub and a log-normal `ttff` stub (median `2 s`, p95 `8 s`). Benchmarks run across request body sizes of `64 KiB`, `512 KiB`, and `2 MiB`.

| ID | Pri | Requirement specification | Verification method |
| --- | --- | --- | --- |
| PERF-1 | P0 | Added proxy latency before the first upstream byte must stay at or below 1 ms at p50 and 5 ms at p99 at 500 req/s (for bodies `<= 256 KiB` against a zero-delay stub, and sustaining `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB / (mean_body * ttff)` req/s with zero `503 gateway_busy` against the log-normal TTFF stub). | Load benchmark (`benches/proxy_latency.rs`) with both zero-delay and log-normal TTFF stubs |
| PERF-2 | P0 | Each complete SSE frame received from an upstream stream must reach the client socket write buffer within 1 ms of its final byte arriving at p99, with zero full-response buffering and zero delay when `TraceTap` is active or artificially stalled. | Timestamped frame-forwarding test in `crates/proxy` |
| PERF-3 | P0 | With 2,000 open SSE streams past their first valid frame, 250,000 active pins in `PinStore` (`~72 MB`), and 64 requests in the pre-first-byte phase with 1 MiB bodies, resident memory (`RSS`) must stay at or below 300 MB plus the pre-first-byte bodies in flight (`64 MiB`). Pre-first-byte request bodies are bounded by `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` (`min(1024, floor(0.40 * memory_limit_mb))`, `819 MiB` on a 2 GB host) with a `2,000 ms` admission queue and released the instant the first valid frame commits. | Soak test with 2,000 active stub streams and 250,000 seeded pins |
| PERF-4 | P0 | Idle resident memory must stay below 50 MB (for standard deployments up to 10,000 routes), and cold start time to readiness (`/readyz` HTTP 200) must stay below 2 s against a database containing 7 days of `usage_rollups_5min` for 500 upstream keys and 50,000,000 raw `usage_events` rows by rebuilding all window rings and monthly accumulators exclusively from `usage_rollups_5min`. | Startup benchmark in CI against a 50M-row seeded database |
| PERF-5 | P0 | Enabling content traceability mode for all traffic at 2,000 open streams must add at most 0.5 ms to p99 TTFB, zero delay to SSE chunk forwarding (`PERF-2`), and keep RSS at or below `PERF-3` plus the global trace accumulator budget (`min(256 MiB, 0.10 * memory_limit)` and `64 MiB` `trace_inflight_bytes`). Compression and AEAD sealing run only at turn completion on a fixed pool of `trace_sealer_threads` (default 2) OS threads reusing `zstd::bulk::Compressor` contexts (`~7 MiB` total across both threads), never allocating per-stream `zstd::stream::Encoder` contexts. | Load and memory soak benchmark with `traceability_config.enabled = true` |

### 7.2 Reliability and availability (REL)

Reliability requirements govern crash recovery, graceful shutdown, fault isolation, and non-blocking usage persistence.

| ID | Pri | Requirement specification | Verification method |
| --- | --- | --- | --- |
| REL-1 | P0 | On `SIGTERM` or `SIGINT`, the server must run the eight-step shutdown sequence (Section 5.5.3): flip `/readyz` to 503 for `drain_grace_secs` (`5 s`), stop accepting new connections, allow open SSE streams up to `drain_timeout_secs` (`60 s`) to finish, cancel remaining streams with `Termination::ShutdownCut`, flush sealer and writer queues, run `PRAGMA wal_checkpoint(TRUNCATE)`, and exit `0` (or `11` on queue timeout). | Integration test sending `SIGTERM` mid-stream |
| REL-2 | P0 | (a) Every confirmed pin, `response_links` row, persisted health transition (`exhausted`/`invalid`), and 5-minute or hourly rollup delta must be committed to SQLite within 1 s of the event under normal load. (b) An unexpected process kill (`SIGKILL`) loses zero committed transactions. (c) Under default `LLMROLLER_SQLITE_SYNCHRONOUS=NORMAL`, an OS crash or host power loss may lose transactions committed since the last WAL sync; operators needing power-loss durability set `LLMROLLER_SQLITE_SYNCHRONOUS=FULL`. | Crash-recovery integration test sending `SIGKILL` 1.2 s after pin confirmation, `response.created`, and request completion |
| REL-3 | P0 | A slow or locked SQLite writer must never block or fail an inference request, and data-plane batches must never starve control-plane transactions (the writer drains `Control` commands between every `4 MiB` / 200-item data batch). Data-plane writes use three separate queues: (1) `durable_state` (`Mutex<IndexMap<DurableKey, DurableOp>>`, capped at 500,000 keys; on full, stops confirming new pins to disk and flips `/readyz` to 503 without dropping queued ops), (2) `rollups` (`Mutex<HashMap<RollupKey, RollupDelta>>` for additive 5-minute and hourly rollup coalescing), and (3) `raw_events` (`Mutex<VecDeque<RawItem>>` capped at 10,000 items and `64 MiB`, evicting the oldest raw event, `usage_attempts`, or trace item after its cost is already folded into `rollups` and incrementing `llmroller_usage_dropped_total{kind}`). | Fault-injection test stalling the SQLite writer task while firing 10,500 requests and concurrent share revocations |
| REL-4 | P0 | All control-plane changes (new keys, shares, priority overrides, disabled routes, budget rules) must be published by the single `Reloader` task in monotonic `instance_meta.config_version` order (applying `CacheInvalidationEvent` purges after the `ArcSwap` store) and take effect within 1 s without restarting the process. | Concurrent mutation and revocation ordering test |
| REL-5 | P0 | One failing provider or upstream key must not slow down requests routed to healthy providers. Outbound HTTP connection pools and `OriginBreaker` state machines (Section 10.5.1) are isolated per upstream origin `OriginKey { scheme, host, port, allow_private_network }`. | Fault-injection test blackholing one upstream origin |
| REL-6 | P1 | The CLI command `llmroller backup --out <path> [--overwrite]` must write a consistent online snapshot to `<path>.tmp`, `fsync` the file, verify `PRAGMA integrity_check`, atomically rename to `<path>`, `fsync` the parent directory, and output JSON including `master_key_id`. | CLI integration test during concurrent write load |

### 7.3 Transparency and compatibility (TRN & COM)

Transparency requirements verify that neither the coding agent nor the upstream provider experiences broken wire behavior when talking through LLMRoller.

| ID | Pri | Requirement specification | Verification method |
| --- | --- | --- | --- |
| TRN-1 | P0 | For any request routed to an `Own`-tier personal key where `model` equals `upstream_model_id` and no session body field or `include_usage` flag needs injection, the outbound request body `Bytes` buffer must be byte-for-byte identical (same pointer) to the inbound body bytes. On `Shared` and `Global` tiers, recognized client session carriers are stripped and replaced with the tenant-salted affinity token per `SEC-16`. | Byte-identity property test in `crates/proto` and `crates/proxy` |
| TRN-2 | P0 | When LLMRoller edits the top-level JSON body (`model`, `prompt_cache_key`, `session_id`, or `stream_options.include_usage`), all other JSON fields (`messages`, `input`, `tools`, `system`, `metadata`) must retain their exact byte sequence and key order, and inserted string values must be JSON-escaped via `serde_json`. | Golden byte-diff and property tests on real agent payloads |
| TRN-3 | P0 | SSE event order, event names (`event:`), data lines (`data:`), and blank-line delimiters must reach the client unchanged (except for suppressing an injected empty-choices usage frame and decoding unexpected upstream compression when a provider ignores `accept-encoding: identity`). | Streaming wire-fidelity test |
| COM-1 | P0 | [Claude Code](https://www.google.com/search?q=https%3A%2F%2Fcode.claude.com%2Fdocs%2Fen%2Fllm-gateway), [Codex CLI](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2Fopenai%2Fcodex%2Fissues%2F24973), [opencode](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2Fanomalyco%2Fopencode%2Fissues%2F55243), the official OpenAI Python and TypeScript SDKs, and the official Anthropic Python and TypeScript SDKs must complete multi-turn streaming tool-call workflows (including stateful `previous_response_id` chains) through LLMRoller. | End-to-end compatibility test suite |

### 7.4 Portability, maintainability, observability, and privacy (POR, MNT, OBS, PRIV)

Operational quality attributes keep the binary portable, auditable, observable, and private by default.

| ID | Pri | Requirement specification | Verification method |
| --- | --- | --- | --- |
| POR-1 | P0 | LLMRoller must compile and pass unit tests in CI from Sub-phase 1.1 onward on Linux (`x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`) and macOS (`aarch64-apple-darwin`), ship a multi-stage distroless production container image under 50 MB (`Section 22.1`) with a native `llmroller healthcheck` probe, and provide complete `docker-compose.yml` orchestration for both production (`caddy` + `llmroller-init` + `llmroller` + `prometheus`) and development (`mock-upstream` + `llmroller-dev`) profiles (`Section 22.2`). | CI build matrix, `docker compose` smoke gate, and image size gate |
| MNT-1 | P0 | All workspace crates must declare `#![forbid(unsafe_code)]` and pass `cargo clippy --all-targets -- -D warnings`. | CI lint gate |
| MNT-2 | P0 | Provider-specific endpoint paths, headers, error patterns, and `/usage` parsers must live inside `ProviderAdapter` implementations and preset TOML files; `crates/core` must contain zero provider-specific branches and zero I/O dependencies (`Clock` in `crates/core` is synchronous). | Architectural dependency check (`tests/workspace_boundaries.rs`) |
| OBS-1 | P0 | Every request must receive a unique ULID in `x-llmroller-request-id`, emitted in structured JSON logs and recorded in `usage_events`. Prometheus metrics at `/metrics` must enforce a closed label allow-list (`provider_kind`, `provider_id`, `protocol`, `endpoint`, `status_class`, `outcome`, `strategy`, `job`, `reason`, and a bounded top-50 `model_bucket` with others grouped as `"other"`), strictly forbidding high-cardinality or tenant-identifying labels (`client_key_id`, `user_id`, `username`, key labels, `session_hash`, or unbounded model IDs). | Integration and metric-registry label allow-list tests |
| PRIV-1 | P0 | LLMRoller must emit zero telemetry, analytics, or crash reports to third parties. Outbound network connections may go only to configured provider hosts and, when explicitly enabled by an admin, the configured price-book catalog URL. | Network sandbox egress test |

## 8. Security and privacy

Warning: Do not expose the control-plane listener (`LLMROLLER_ADMIN_LISTEN`) to an untrusted network without TLS. Bind it to `127.0.0.1` or place it behind an authenticated reverse proxy that handles TLS and is listed in `LLMROLLER_TRUSTED_PROXIES`.

LLMRoller holds high-value upstream credentials across global and personal accounts. The security design separates stable instance secrets (`audit_hmac_key` and `session_hmac_secret`) from the rotatable master key. It seals all six sensitive column families with canonical length-prefixed `AAD`, filters every outbound socket at connect time, and enforces tenant boundaries.

### 8.1 Threat model

Fifteen concrete threats drive the security controls in LLMRoller.

| ID | Threat description | Primary mitigations |
| --- | --- | --- |
| T1 | An attacker reads `llmroller.db` or a backup file from disk to steal upstream API keys or account billing details from `/usage` snapshots. | `SEC-1`, `SEC-2`, `SEC-14`, `SEC-18` |
| T2 | An attacker races a fresh installation or reads persisted stdout logs to steal the bootstrap setup token and register the first admin account. | `AUTH-1`, `AUTH-2`, `AUTH-3`, `SEC-9` |
| T3 | A malicious user or compromised admin configures a custom provider URL (or IP-literal / IPv6-mapped / 6to4 URL) pointing to `127.0.0.1`, `169.254.169.254`, or RFC 1918 internal services (SSRF). | `PRV-7`, `SEC-7` |
| T4 | An upstream API key, client key, session cookie, or user prompt leaks into application logs, panic traces, or error responses. | `SEC-6`, `SEC-14` |
| T5 | HTTP request smuggling, duplicate top-level JSON keys (`{"model":"cheap","model":"expensive"}`), or hop-by-hop header injection tricks the proxy into bypassing model restrictions or poisoning connections. | `API-4`, `SEC-15` |
| T6 | An attacker brute-forces web logins, TOTP codes, or client bearer keys, or floods the admin username to lock out the legitimate admin. | `AUTH-8`, `SEC-3`, `SEC-4`, `SEC-12` |
| T7 | Cross-Site Request Forgery (CSRF) or Cross-Site Scripting (XSS) hijacks an admin or standard user session in the browser. | `SEC-5`, `SEC-19` |
| T8 | A compromised third-party crate in the dependency tree exfiltrates secrets (as occurred in the [March 2026 LiteLLM incident](https://www.google.com/search?q=https%3A%2F%2Fdocs.litellm.ai%2Fblog%2Fsecurity-update-march-2026)). | `SEC-10`, `SEC-11`, `PRIV-1` |
| T9 | A leaked client key drains all global or shared upstream budgets, or a caller sends content-flagged prompts to trigger 403s and disable shared/global keys. | `CLK-4`, `CLK-5`, `USR-5`, `SHR-2`, `LIM-1`, `API-8` |
| T10 | Two users sharing a global or shared upstream key collide on sticky replica routing or infer prompt prefixes via client-supplied session headers. | `SEC-16` |
| T11 | A malicious upstream server returns a 301, 302, 307, or 308 redirect to exfiltrate the `Authorization` header to an attacker host. | `PRV-7`, `SEC-7` |
| T12 | Standard User A crafts control-plane API calls (IDOR) to read, edit, share, or probe existence of User B's personal resources. | `USR-3`, `USR-4`, `SEC-17` |
| T13 | Grantee B abuses a shared personal key from Grantor A to extract A's raw API key, read A's account headers, or read A's raw `/usage` metadata; or Grantor A pushes an unconsented share pointing to an attacker-controlled relay to capture B's prompts. | `SHR-3`, `SHR-4`, `USG-8`, `SEC-16`, `SEC-18` |
| T14 | An attacker tampers with or truncates the tail of SQLite `audit_log` rows to hide unauthorized configuration or share changes. | `AUD-1`, `SEC-13` |
| T15 | An attacker with disk access, a Grantor on a shared key, or a crafted model response reads sensitive prompts in `trace_records`/`trace_blocks` or triggers XSS in the chat preview UI. | `TRC-8`, `SEC-19`, `SEC-5` |

### 8.2 Security requirements (SEC)

Every security requirement is mandatory (`P0`) for version 1.0.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| SEC-1 | P0 | All sealed blobs must use `XChaCha20-Poly1305` in the versioned envelope `0x01 \|\| nonce (24 bytes) \|\| ciphertext \|\| tag (16 bytes)` with canonical length-prefixed Associated Data `aad(domain, fields) = "llmroller.aad.v1 " \|\| u32_be(len(domain)) \|\| domain \|\| (for f in fields: u32_be(len(f)) \|\| f)` (Section 9.2.2) under 256-bit subkeys derived via `HKDF-SHA256` from `LLMROLLER_MASTER_KEY` and salted with the 16-byte `instance_meta.instance_id` (info labels: `"llmroller.v1.upstream_key"`, `"llmroller.v1.totp_secret"`, `"llmroller.v1.trace_record"`, and `"llmroller.v1.instance_secret"`). Two random 256-bit secrets (`session_hmac_secret` and `audit_hmac_key`) are generated once at bootstrap and stored sealed in `instance_secrets` so offline resumable master-key rotation (`llmroller rotate-master-key`, Section 5.5) re-seals all six encrypted tables without ever invalidating `audit_log` hashes, `session_hash` pins, or traces. |
| SEC-2 | P0 | The master key must be supplied via `LLMROLLER_MASTER_KEY_FILE` (preferred; rejected with exit `3` if group or other permission bits are set) or `LLMROLLER_MASTER_KEY`, must decode from base64 to **exactly** 32 raw bytes, and must never be written to SQLite or logs. Startup must exit `3` if the key is missing, not 32 bytes, or disagrees with `instance_meta.active_master_key_id`, or if `instance_meta.rotation_state == 'in_progress'`. |
| SEC-3 | P0 | Client keys (`llr_...`) must contain 256 bits of cryptographic randomness (`OsRng`), be stored solely as SHA-256 digests (`key_hash` and optional grace `prev_key_hash`), and be verified in constant time (`subtle::ConstantTimeEq`). |
| SEC-4 | P0 | User and admin passwords must be hashed with `Argon2id` with parameters `m >= 19456 KiB`, `t >= 2`, `p = 1`, and a 128-bit random salt stored in the PHC string so stronger parameters remain verifiable ([OWASP Password Storage Cheat Sheet](https://www.google.com/search?q=https%3A%2F%2Fcheatsheetseries.owasp.org%2Fcheatsheets%2FPassword_Storage_Cheat_Sheet.html)). |
| SEC-5 | P0 | The control plane must refuse to start on a non-loopback plain-HTTP address unless `LLMROLLER_TRUSTED_PROXIES` is configured (exit `2`). Web session cookies must use `__Host-llmroller_session` (or `llmroller_session` only when `--dev-insecure-cookies` is set), `HttpOnly`, `Secure`, `SameSite=Strict`, 12-hour idle expiry, and 7-day absolute expiry. Each login generates a random 256-bit CSRF token stored as `web_sessions.csrf_hash = SHA-256(token)`, delivered in `<meta name="csrf-token">` and validated in constant time against `x-csrf-token` on every `POST`, `PUT`, `PATCH`, or `DELETE` request (`GET /login` sets a 10-minute `__Host-llmroller_login_csrf` cookie paired with `x-csrf-token` and an `Origin` header check on `POST /api/auth/login`). All HTML responses must set `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'`, `X-Content-Type-Options: nosniff`, and `X-Frame-Options: DENY`, with `htmx.config.includeIndicatorStyles = false`, `allowEval = false`, and `allowScriptTags = false`. |
| SEC-6 | P0 | Application logs, error bodies, and panic hooks must redact `Authorization`, `x-api-key`, `Cookie`, `Set-Cookie`, `secret`, and request or response payloads (truncating and scrubbing any echoed `sk-*`/`llr_*` tokens in upstream error strings), and CSV exports must prefix cells starting with `=`, `+`, `-`, `@`, `	`, or `
` with `'` to prevent formula injection. Automated canary tests must verify that a planted secret string never appears in stdout, stderr, or database error columns. |
| SEC-7 | P0 | Outbound connections must use `GuardedConnector` (`crates/proxy::ssrf`), which disables environment proxies, enforces TLS 1.2+ with `rustls` (permitting `http://` only when an admin sets `allow_private_network = 1`), disables HTTP redirects (`redirect::Policy::none()`), sends an upstream key only to its provider's `base_url` origin (or preset `allowed_usage_origins`, or personal `usage_url_cross_origin_ack` which forbids sharing), and validates **every connected socket IP** (for both DNS hostnames and IP-literal URLs, unwrapping IPv4-mapped IPv6 `::ffff:0:0/96` and 6to4 `2002::/16`) against all special-purpose ranges (`0.0.0.0/8`, `10.0.0.0/8`, `100.64.0.0/10`, `127.0.0.0/8`, `169.254.0.0/16`, `172.16.0.0/12`, `192.0.0.0/24`, `192.0.2.0/24`, `192.88.99.0/24`, `192.168.0.0/16`, `198.18.0.0/15`, `198.51.100.0/24`, `203.0.113.0/24`, `224.0.0.0/4`, `240.0.0.0/4`, `255.255.255.255/32`, `::/128`, `::1/128`, `64:ff9b::/96`, `64:ff9b:1::/48`, `100::/64`, `2001::/23`, `2001:db8::/32`, `fc00::/7`, `fe80::/10`, `fec0::/10`, `ff00::/8`) unless `allow_private_network = 1`. |
| SEC-8 | P0 | The server must enforce configurable resource limits: max request body size (`LLMROLLER_MAX_BODY_MB`, default `32 MiB`, `<= budget / 4`), global pre-first-byte body budget (`LLMROLLER_PREFLIGHT_BODY_BUDGET_MB`, default `min(1024, floor(0.40 * memory_limit_mb))`, `2,000 ms` queue wait), max `/models` or `/usage` response size (`2 MiB`, max `1,000` models per provider refresh), per-user personal resource quotas (`20` providers, `50` upstream keys, `2,000` personal-origin canonical models), max header block (`64 KiB`), and the normative timeout table in Section 10.5.2. |
| SEC-9 | P0 | The control plane (`LLMROLLER_ADMIN_LISTEN`) and metrics listener (`LLMROLLER_METRICS_LISTEN`) must bind to `127.0.0.1` by default, separate from the data-plane listener (`0.0.0.0:8080`). When `LLMROLLER_LISTEN` is non-loopback without TLS or `LLMROLLER_TRUSTED_PROXIES`, startup must log an error every 10 minutes and set `llmroller_plaintext_data_plane = 1` (or exit `2` when `LLMROLLER_REQUIRE_TLS=true`). |
| SEC-10 | P0 | `Cargo.lock` must be committed. CI must run `cargo audit`, `cargo deny check advisories bans licenses sources`, and `cargo vet`. GitHub Actions workflows must pin third-party actions by full 40-character commit SHA. |
| SEC-11 | P0 | Release artifacts must include SHA-256 checksums, a CycloneDX SBOM, and a `cosign` signature. Container images must use a distroless base, run as UID `65532` (non-root), drop all Linux capabilities, and mount the root filesystem read-only. |
| SEC-12 | P0 | Client IP extraction must read the right-most `X-Forwarded-For` entry not in `LLMROLLER_TRUSTED_PROXIES` only when the TCP peer matches `LLMROLLER_TRUSTED_PROXIES`. Failed client-key authentication requests (`G1`..`G3`) must be rate-limited per client IP (20 failures per minute -> HTTP 429) and aggregated into `auth_failure_counters` (never `usage_events`), while failed logins use the three-tier limiter in `AUTH-8`. |
| SEC-13 | P0 | Each `audit_log` row must be written in the same SQLite transaction as the state change it records and compute `hash = hex_lower(HMAC_SHA256(audit_hmac_key, canonical_frame(seq, prev_hash, id, ts, actor_user_id, ip_addr, action, target_type, target_id, detail_json)))` using `audit_hmac_key` from `instance_secrets` (Section 9.2.1). Every 100 rows and at least hourly, the writer must emit `{"event":"audit_anchor","seq":N,"hash":"H","ts":T}` to `stdout` so `llmroller verify-audit-log --anchor-seq N --anchor-hash H` detects both internal tampering (exit `8`) and tail truncation (exit `9`). |
| SEC-14 | P0 | All in-memory secret strings (master key, `instance_secrets`, decrypted upstream API keys, raw client tokens, CSRF tokens) must be wrapped in `secrecy::SecretString` or `SecretBox<[u8]>` with `ZeroizeOnDrop`. |
| SEC-15 | P0 | The HTTP edge must reject requests containing both `Content-Length` and `Transfer-Encoding`, multiple `Host` headers, non-ASCII control characters in header values, or duplicate top-level JSON keys with HTTP 400, and must strip all hop-by-hop headers and `Set-Cookie` headers in both directions (Section 12.1 and Section 12.1.1). |
| SEC-16 | P0 | When routing through a `Shared` or `Global` upstream key, LLMRoller must strip every recognized client session header and body field (except `x-claude-code-agent-id` and `x-claude-code-parent-agent-id`) and write only the provider's configured affinity fields with the tenant-salted token `llr_<base32(HMAC(session_hmac_secret, "affinity.v1 " \|\| owner_user_id \|\| " " \|\| raw_session_id))[0..26]>`. Shares need explicit grantee acceptance matching `providers.share_config_fingerprint` (and admin approval for `custom` providers) so any change to a personal provider's routing-relevant config suspends active shares until re-accepted. |
| SEC-17 | P0 | Every repository method in `crates/store` and handler in `crates/admin` that accesses providers, upstream keys, shares, routes, client keys, pins, or usage must take an `&ActorScope` parameter and enforce SQL-level `WHERE` predicates scoping rows to `owner_user_id` or `grantee_user_id` whenever `actor` is `ActorScope::User { role: Role::User, .. }`. |
| SEC-18 | P0 | `upstream_usage_snapshots.raw_json_enc` must be sealed at rest with `XChaCha20-Poly1305` (domain `usage_raw`). Upstream `/usage` pass-through responses (`GET /v1/usage`, `GET /v1/usage/raw`, `GET /v1/key`, and `/api/user/upstream-keys/{id}/usage`) and inference response headers (Section 12.1.1) must strip all provider account identifiers whenever the caller is not the personal key owner or an `admin`. |
| SEC-19 | P0 | All recorded `trace_blocks.payload_enc` (domain `trace_block`) and `trace_records.payload_enc` (domain `trace_record`) blobs must be compressed with `zstd` and encrypted at rest using `XChaCha20-Poly1305` with canonical length-prefixed `AAD` (Section 9.2.2). In the Session Chat Preview, assistant Markdown must be rendered with `pulldown-cmark` (raw HTML disabled) and sanitized with `ammonia` (`rel="noopener noreferrer nofollow"`, `https:`/`http:` links only), while reasoning, user text, tool arguments, tool results, and raw JSON must be rendered as Askama auto-escaped text inside `<pre>`. |

### 8.3 Privacy and operator compliance

LLMRoller is built for data minimization. It never stores prompt text, system instructions, tool arguments, or model completions unless an admin explicitly enables content traceability mode (`TRC-1`). When enabled, recorded payloads are deduplicated per user in `trace_blocks`, encrypted at rest (`SEC-19`), and isolated to the client key owner (`TRC-8`). They are purged automatically after `traceability_config.retention_days` (`TRC-7`). It never stores raw client session IDs in SQLite or accepts them in URL query strings (`GET /v1/usage` accepts session IDs only via HTTP headers); it hashes every session ID with `session_hmac_secret` before persisting pins, usage rows, or traces.

Aside from stripping session carriers on pooled keys, LLMRoller forwards non-credential headers unchanged. Privacy headers such as Command Code's `x-cmd-zdr: 1` zero-data-retention header therefore reach the upstream provider intact. Operators and users who share keys must review each provider's terms of service on pooled access (tracked as Risk `R1` in Section 18).

## 9. Data model

LLMRoller stores all persistent state in a single SQLite database running in Write-Ahead Logging (`WAL`) mode with incremental auto-vacuum (`PRAGMA auto_vacuum = INCREMENTAL`) and foreign-key enforcement (`PRAGMA foreign_keys = ON`). All monetary amounts are stored as 64-bit signed integers representing micro-USD (`1 USD = 1,000,000 micro_usd`) to avoid floating-point rounding errors. All timestamps are 64-bit integers in UTC epoch milliseconds, and primary keys use 26-character ULIDs or composite keys.

### 9.1 Per-connection pragma builder and complete 31-table SQLite DDL schema

SQLite pragmas such as `foreign_keys`, `busy_timeout`, `query_only`, `synchronous`, and `temp_store` apply per connection rather than in database DDL. Every connection across the writer, the 8 readers, and all CLI commands is therefore built through `crates/store::pool::connect_options(role: Role)`.

| Pragma | Server writer (`Role::Writer`) | Server readers (`Role::Reader`, 8 connections) | CLI subcommands (`Role::Cli`) |
| --- | --- | --- | --- |
| `journal_mode` | `WAL` (asserted equal to `"wal"` on open) | `WAL` (asserted) | `WAL` (asserted) |
| `synchronous` | `NORMAL` or `FULL` (`LLMROLLER_SQLITE_SYNCHRONOUS`) | Not modified | `FULL` |
| `foreign_keys` | `ON` | `ON` | `ON` |
| `busy_timeout` | `5000` (5 s) | `5000` (5 s) | `30000` (30 s) |
| `query_only` | `OFF` | `ON` | `OFF` for `rotate-master-key` and `reset-admin`; `ON` for `verify-audit-log` and `backup` |
| `temp_store` | `MEMORY` | `MEMORY` | `MEMORY` |
| `journal_size_limit` | `67108864` (64 MiB) | Not modified | `67108864` |

Migration `0001` runs `PRAGMA auto_vacuum = INCREMENTAL;` first on an empty database (plus `VACUUM;` if needed). At startup, `serve` asserts `PRAGMA auto_vacuum == 2` and exits with code `13` otherwise.

The SQLite schema has 31 tables (plus triggers and partial unique indexes) across five logical domains. These domains cover instance metadata and accounts, providers and sharing, models and pricing, session pins and response links, and usage, traces, and operations.

```sql
-- 0. Schema Version, Instance Metadata, and Sealed Instance Secrets
CREATE TABLE _llmroller_schema (
  version INTEGER PRIMARY KEY
);

CREATE TABLE instance_meta (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  instance_id BLOB NOT NULL CHECK (length(instance_id) = 16),
  active_master_key_id TEXT NOT NULL,
  config_version INTEGER NOT NULL DEFAULT 0,
  rotation_state TEXT NOT NULL DEFAULT 'idle' CHECK (rotation_state IN ('idle', 'in_progress')),
  rotation_from_key_id TEXT,
  rotation_to_key_id TEXT,
  rotation_cursor_table TEXT,
  rotation_cursor_id TEXT,
  updated_at INTEGER NOT NULL
);

CREATE TABLE instance_secrets (
  name TEXT PRIMARY KEY CHECK (name IN ('session_hmac_secret', 'audit_hmac_key')),
  sealed BLOB NOT NULL, -- 0x01 || 24-byte nonce || ciphertext || 16-byte tag
  master_key_id TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

-- 1. Accounts, Deleted-User History, Web Sessions, and Client Keys
CREATE TABLE users (
  id TEXT PRIMARY KEY,
  username TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('admin', 'user')),
  must_change_password INTEGER NOT NULL DEFAULT 0,
  totp_secret_enc BLOB,
  totp_master_key_id TEXT,
  totp_enabled INTEGER NOT NULL DEFAULT 0,
  totp_last_step INTEGER,
  totp_recovery_hashes_json TEXT,
  can_use_global_providers INTEGER NOT NULL DEFAULT 1,
  global_budget_micro_usd_month INTEGER,
  default_routing_scope TEXT NOT NULL DEFAULT 'personal_first'
    CHECK (default_routing_scope IN ('personal_first', 'blended', 'personal_only', 'own_only', 'global_only')),
  disabled INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  last_login_at INTEGER
);

CREATE TABLE deleted_users (
  user_id TEXT PRIMARY KEY,
  username TEXT NOT NULL,
  deleted_at INTEGER NOT NULL
);

CREATE TABLE web_sessions (
  id_hash TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  csrf_hash TEXT NOT NULL, -- SHA-256 of per-session random 256-bit CSRF token
  ip_addr TEXT,
  user_agent TEXT,
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX idx_web_sessions_user ON web_sessions(user_id);
CREATE INDEX idx_web_sessions_expires ON web_sessions(expires_at);

CREATE TABLE client_keys (
  id TEXT PRIMARY KEY,
  owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  prefix TEXT NOT NULL,
  key_hash TEXT NOT NULL UNIQUE,
  prev_key_hash TEXT,
  prev_key_expires_at INTEGER,
  routing_scope TEXT NOT NULL DEFAULT 'inherit'
    CHECK (routing_scope IN ('inherit', 'personal_first', 'blended', 'personal_only', 'own_only', 'global_only')),
  allowed_models TEXT, -- JSON array of canonical_id strings, or NULL for all accessible models
  rate_limit_rpm INTEGER,
  budget_micro_usd_month INTEGER,
  allowed_cidrs TEXT, -- JSON array of CIDR strings, or NULL
  constant_session_flagged_at INTEGER,
  constant_session_mode TEXT NOT NULL DEFAULT 'flag'
    CHECK (constant_session_mode IN ('flag', 'fingerprint_generic_ids')),
  disabled INTEGER NOT NULL DEFAULT 0,
  revoked_at INTEGER,
  expires_at INTEGER,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER
);
CREATE INDEX idx_client_keys_owner ON client_keys(owner_user_id);
CREATE UNIQUE INDEX uq_client_keys_prev_hash ON client_keys(prev_key_hash)
  WHERE prev_key_hash IS NOT NULL;

CREATE TRIGGER trg_client_keys_hash_disjoint_ins BEFORE INSERT ON client_keys
WHEN EXISTS (
  SELECT 1 FROM client_keys c
  WHERE c.key_hash = NEW.prev_key_hash OR c.prev_key_hash = NEW.key_hash
)
BEGIN SELECT RAISE(ABORT, 'client key hash collision'); END;

CREATE TRIGGER trg_client_keys_hash_disjoint_upd BEFORE UPDATE OF key_hash, prev_key_hash ON client_keys
WHEN EXISTS (
  SELECT 1 FROM client_keys c
  WHERE c.id <> NEW.id AND (c.key_hash = NEW.prev_key_hash OR c.prev_key_hash = NEW.key_hash)
)
BEGIN SELECT RAISE(ABORT, 'client key hash collision'); END;

-- 2. Global and Personal Providers, Upstream Keys, Shares, Budgets, and Upstream /usage Snapshots
CREATE TABLE providers (
  id TEXT PRIMARY KEY,
  ownership_scope TEXT NOT NULL CHECK (ownership_scope IN ('global', 'personal')),
  owner_user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('opencode_go', 'command_code', 'open_design_amr', 'openrouter', 'custom')),
  name TEXT NOT NULL,
  base_url TEXT NOT NULL,
  config_json TEXT NOT NULL,
  share_config_fingerprint TEXT NOT NULL DEFAULT '',
  usage_url_cross_origin_ack INTEGER NOT NULL DEFAULT 0 CHECK (usage_url_cross_origin_ack IN (0, 1)),
  allow_private_network INTEGER NOT NULL DEFAULT 0,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  CHECK (
    (ownership_scope = 'global' AND owner_user_id IS NULL) OR
    (ownership_scope = 'personal' AND owner_user_id IS NOT NULL)
  )
);
CREATE INDEX idx_providers_owner ON providers(owner_user_id);
CREATE UNIQUE INDEX uq_providers_global_name ON providers(name)
  WHERE ownership_scope = 'global';
CREATE UNIQUE INDEX uq_providers_personal_name ON providers(owner_user_id, name)
  WHERE ownership_scope = 'personal';

CREATE TABLE upstream_keys (
  id TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  owner_user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  secret_enc BLOB NOT NULL,
  master_key_id TEXT NOT NULL,
  secret_last4 TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  priority INTEGER NOT NULL DEFAULT 100 CHECK (priority BETWEEN 1 AND 1000),
  strict_tier INTEGER NOT NULL DEFAULT 1 CHECK (strict_tier BETWEEN 1 AND 10),
  health TEXT NOT NULL DEFAULT 'healthy' CHECK (health IN ('healthy', 'cooling_down', 'exhausted', 'invalid')),
  health_until INTEGER,
  health_version INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_upstream_keys_provider ON upstream_keys(provider_id);
CREATE INDEX idx_upstream_keys_owner ON upstream_keys(owner_user_id);

CREATE TRIGGER trg_upstream_keys_owner_ins BEFORE INSERT ON upstream_keys
WHEN NEW.owner_user_id IS NOT (SELECT owner_user_id FROM providers WHERE id = NEW.provider_id)
BEGIN SELECT RAISE(ABORT, 'upstream_keys.owner_user_id must equal providers.owner_user_id'); END;

CREATE TRIGGER trg_upstream_keys_owner_upd BEFORE UPDATE OF owner_user_id, provider_id ON upstream_keys
WHEN NEW.owner_user_id IS NOT (SELECT owner_user_id FROM providers WHERE id = NEW.provider_id)
BEGIN SELECT RAISE(ABORT, 'upstream_keys.owner_user_id must equal providers.owner_user_id'); END;

CREATE TABLE provider_shares (
  id TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  upstream_key_id TEXT REFERENCES upstream_keys(id) ON DELETE CASCADE,
  owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  grantee_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  allowed_models TEXT, -- JSON array of canonical_id strings, or NULL for all models served
  max_monthly_micro_usd INTEGER,
  accepted_at INTEGER,
  accepted_config_fingerprint TEXT,
  admin_approved_at INTEGER,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER,
  CHECK (owner_user_id != grantee_user_id)
);
CREATE INDEX idx_provider_shares_grantee ON provider_shares(grantee_user_id, enabled);
CREATE INDEX idx_provider_shares_owner ON provider_shares(owner_user_id);
CREATE UNIQUE INDEX uq_share_key_active ON provider_shares(grantee_user_id, upstream_key_id)
  WHERE upstream_key_id IS NOT NULL AND revoked_at IS NULL;
CREATE UNIQUE INDEX uq_share_provider_active ON provider_shares(grantee_user_id, provider_id)
  WHERE upstream_key_id IS NULL AND revoked_at IS NULL;

CREATE TRIGGER trg_shares_integrity_ins BEFORE INSERT ON provider_shares
WHEN (SELECT ownership_scope FROM providers WHERE id = NEW.provider_id) <> 'personal'
  OR (SELECT usage_url_cross_origin_ack FROM providers WHERE id = NEW.provider_id) <> 0
  OR NEW.owner_user_id <> (SELECT owner_user_id FROM providers WHERE id = NEW.provider_id)
  OR (NEW.upstream_key_id IS NOT NULL AND (SELECT provider_id FROM upstream_keys WHERE id = NEW.upstream_key_id) <> NEW.provider_id)
BEGIN SELECT RAISE(ABORT, 'invalid provider share'); END;

CREATE TRIGGER trg_shares_integrity_upd BEFORE UPDATE OF provider_id, upstream_key_id, owner_user_id ON provider_shares
WHEN (SELECT ownership_scope FROM providers WHERE id = NEW.provider_id) <> 'personal'
  OR (SELECT usage_url_cross_origin_ack FROM providers WHERE id = NEW.provider_id) <> 0
  OR NEW.owner_user_id <> (SELECT owner_user_id FROM providers WHERE id = NEW.provider_id)
  OR (NEW.upstream_key_id IS NOT NULL AND (SELECT provider_id FROM upstream_keys WHERE id = NEW.upstream_key_id) <> NEW.provider_id)
BEGIN SELECT RAISE(ABORT, 'invalid provider share'); END;

-- 3. Model Catalog (Surrogate model_pk), Redirects, Aliases, Routes, Preferences, Budgets, and Price Book
CREATE TABLE models (
  model_pk TEXT PRIMARY KEY, -- Immutable 26-char ULID surrogate key
  canonical_id TEXT NOT NULL UNIQUE,
  display_name TEXT,
  exposed INTEGER CHECK (exposed IN (0, 1)), -- NULL = automatic global exposure rule, 0 = forced off, 1 = forced on
  distribution TEXT NOT NULL DEFAULT 'weighted' CHECK (distribution IN ('weighted', 'strict')),
  routing_strategy TEXT CHECK (routing_strategy IN ('smooth_weighted_rr', 'naive_rr', 'weighted_rr', 'uniform_random', 'weighted_random', 'power_of_two_choices')), -- NULL = inherit settings.default_routing_strategy
  merge_state TEXT NOT NULL DEFAULT 'confirmed' CHECK (merge_state IN ('confirmed', 'pending_review')),
  origin TEXT NOT NULL DEFAULT 'global' CHECK (origin IN ('global', 'preset', 'personal')),
  created_by_user_id TEXT REFERENCES users(id) ON DELETE SET NULL
);

CREATE TABLE model_redirects (
  old_canonical_id TEXT PRIMARY KEY,
  model_pk TEXT NOT NULL REFERENCES models(model_pk) ON DELETE RESTRICT,
  created_at INTEGER NOT NULL
);

CREATE TABLE model_aliases (
  alias TEXT PRIMARY KEY,
  model_pk TEXT NOT NULL REFERENCES models(model_pk) ON DELETE RESTRICT,
  confirmed INTEGER NOT NULL DEFAULT 1
);

CREATE TRIGGER trg_alias_not_canonical BEFORE INSERT ON model_aliases
WHEN EXISTS (SELECT 1 FROM models WHERE canonical_id = NEW.alias)
BEGIN SELECT RAISE(ABORT, 'alias collides with canonical_id'); END;

CREATE TABLE model_routes (
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  model_pk TEXT NOT NULL REFERENCES models(model_pk) ON DELETE RESTRICT,
  upstream_model_id TEXT NOT NULL,
  vendor_prefix TEXT, -- Substring before the last '/', NULL when absent
  protocols TEXT NOT NULL, -- JSON array: ["chat","responses","messages"]
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'missing', 'retired')),
  enabled INTEGER NOT NULL DEFAULT 1,
  priority_override INTEGER CHECK (priority_override BETWEEN 1 AND 1000),
  route_health TEXT NOT NULL DEFAULT 'healthy'
    CHECK (route_health IN ('healthy', 'cooling_down', 'exhausted', 'unavailable')),
  route_health_until INTEGER,
  health_version INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  missing_since INTEGER,
  discovered_at INTEGER NOT NULL,
  PRIMARY KEY (upstream_key_id, model_pk)
);

CREATE TABLE route_protocol_blocks (
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  model_pk TEXT NOT NULL REFERENCES models(model_pk) ON DELETE RESTRICT,
  protocol TEXT NOT NULL CHECK (protocol IN ('chat', 'responses', 'messages')),
  reason TEXT NOT NULL,
  until_ms INTEGER NOT NULL,
  PRIMARY KEY (upstream_key_id, model_pk, protocol)
);

CREATE TABLE user_route_preferences (
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  model_pk TEXT NOT NULL REFERENCES models(model_pk) ON DELETE RESTRICT,
  priority_override INTEGER NOT NULL CHECK (priority_override BETWEEN 1 AND 1000),
  enabled INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (user_id, upstream_key_id, model_pk)
);

CREATE TABLE budget_windows (
  id TEXT PRIMARY KEY,
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('upstream_key', 'route')),
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  model_pk TEXT REFERENCES models(model_pk) ON DELETE RESTRICT,
  window_kind TEXT NOT NULL
    CHECK (window_kind IN ('rolling', 'anchored_fixed', 'anchored_calendar_month', 'anchored_provider')),
  period_ms INTEGER CHECK (period_ms BETWEEN 3600000 AND 2678400000), -- 1 h .. 31 d; NULL for calendar month
  anchor_at INTEGER,
  upstream_window_kind TEXT CHECK (upstream_window_kind IN ('5h', '7d', '1mo')),
  limit_micro_usd INTEGER NOT NULL CHECK (limit_micro_usd > 0),
  reserve_pct INTEGER NOT NULL DEFAULT 2 CHECK (reserve_pct BETWEEN 0 AND 50),
  CHECK (
    (scope_kind = 'upstream_key' AND model_pk IS NULL) OR
    (scope_kind = 'route' AND model_pk IS NOT NULL)
  ),
  CHECK (
    (window_kind = 'anchored_calendar_month' AND period_ms IS NULL) OR
    (window_kind <> 'anchored_calendar_month' AND period_ms IS NOT NULL)
  )
);
CREATE INDEX idx_budget_windows_key ON budget_windows(upstream_key_id);

CREATE TABLE budget_adjustments (
  id TEXT PRIMARY KEY,
  budget_window_id TEXT NOT NULL REFERENCES budget_windows(id) ON DELETE CASCADE,
  actor_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
  delta_micro_usd INTEGER NOT NULL,
  effective_at INTEGER NOT NULL,
  window_instance_start INTEGER, -- Anchored windows: start of instance it applies to; NULL for rolling
  source TEXT NOT NULL CHECK (source = 'manual_calibration'),
  note TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_budget_adjustments_window ON budget_adjustments(budget_window_id, effective_at);

CREATE TABLE upstream_usage_snapshots (
  id TEXT PRIMARY KEY,
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  fetched_at INTEGER NOT NULL,
  http_status INTEGER NOT NULL,
  plan_name TEXT,
  windows_json TEXT NOT NULL, -- [{"kind":"5h","status":"ok","used_pct":12.5,"used_micro_usd":null,"limit_micro_usd":null,"resets_at":1792400000000}]
  balance_micro_usd INTEGER,
  raw_json_enc BLOB NOT NULL, -- Sealed XChaCha20-Poly1305 blob (domain 'usage_raw')
  master_key_id TEXT NOT NULL,
  sanitized_json TEXT NOT NULL
);
CREATE INDEX idx_upstream_usage_key_ts ON upstream_usage_snapshots(upstream_key_id, fetched_at DESC);

CREATE TABLE prices (
  provider_id TEXT NOT NULL DEFAULT '',
  provider_kind TEXT NOT NULL,
  upstream_model_id TEXT NOT NULL,
  input_per_mtok_micro_usd INTEGER NOT NULL,
  output_per_mtok_micro_usd INTEGER NOT NULL,
  cache_read_per_mtok_micro_usd INTEGER NOT NULL DEFAULT 0,
  cache_write_per_mtok_micro_usd INTEGER NOT NULL DEFAULT 0,
  tiers_json TEXT,
  schedule_json TEXT,
  source TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (provider_id, provider_kind, upstream_model_id)
);

-- 4. Confirmed Session Pins and Durable Stateful Response Links
CREATE TABLE pins (
  client_key_id TEXT NOT NULL REFERENCES client_keys(id) ON DELETE CASCADE,
  session_hash TEXT NOT NULL,
  model_pk TEXT NOT NULL REFERENCES models(model_pk) ON DELETE RESTRICT,
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  pool_tier INTEGER NOT NULL DEFAULT 0 CHECK (pool_tier BETWEEN 0 AND 3),
  created_at INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  repin_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (client_key_id, session_hash, model_pk)
);
CREATE INDEX idx_pins_expires ON pins(expires_at);
CREATE INDEX idx_pins_upstream_key ON pins(upstream_key_id);

CREATE TABLE response_links (
  response_id TEXT PRIMARY KEY,
  owner_user_id TEXT NOT NULL,
  client_key_id TEXT NOT NULL REFERENCES client_keys(id) ON DELETE CASCADE,
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  model_pk TEXT NOT NULL REFERENCES models(model_pk) ON DELETE RESTRICT,
  session_hash TEXT NOT NULL,
  protocol TEXT NOT NULL DEFAULT 'responses' CHECK (protocol = 'responses'),
  status TEXT NOT NULL CHECK (status IN ('in_progress', 'completed', 'incomplete', 'failed')),
  store_flag INTEGER NOT NULL CHECK (store_flag IN (0, 1)),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX idx_response_links_expires ON response_links(expires_at);
CREATE INDEX idx_response_links_session ON response_links(owner_user_id, session_hash);

-- 5. Usage Events, Attempts, 5-Min & Hourly Rollups, Auth Counters, Content-Addressed Traces, Audit Log, and Settings
CREATE TABLE usage_events (
  id TEXT PRIMARY KEY,
  ts INTEGER NOT NULL,
  request_id TEXT NOT NULL UNIQUE,
  client_key_id TEXT NOT NULL,
  owner_user_id TEXT NOT NULL,
  upstream_key_id TEXT,
  provider_id TEXT,
  provider_scope TEXT CHECK (provider_scope IN ('global', 'personal', 'shared', 'none')),
  share_id TEXT,
  canonical_id TEXT,
  upstream_model_id TEXT,
  endpoint TEXT NOT NULL
    CHECK (endpoint IN ('chat', 'responses', 'messages', 'count_tokens', 'responses_get', 'responses_delete', 'responses_input_items', 'responses_cancel', 'responses_compact')),
  protocol TEXT CHECK (protocol IN ('chat', 'responses', 'messages')),
  session_hash TEXT,
  status INTEGER NOT NULL,
  stream INTEGER NOT NULL,
  incomplete INTEGER NOT NULL DEFAULT 0,
  termination TEXT NOT NULL DEFAULT 'completed'
    CHECK (termination IN ('completed', 'upstream_truncated', 'client_gone', 'idle_timeout', 'shutdown_cut')),
  trace_dropped INTEGER NOT NULL DEFAULT 0,
  input_uncached_tokens INTEGER,
  cache_read_tokens INTEGER,
  cache_write_tokens INTEGER,
  output_tokens INTEGER,
  reasoning_tokens INTEGER,
  cost_micro_usd INTEGER NOT NULL,
  cost_source TEXT NOT NULL
    CHECK (cost_source IN ('provider', 'price_book', 'fallback_rate', 'estimated_incomplete', 'estimated_attempt')),
  latency_ms INTEGER,
  ttfb_ms INTEGER,
  pin_outcome TEXT
    CHECK (pin_outcome IN ('new_pin', 'followed', 'repin', 'repin_recovery', 'unpinned_stateless', 'unpinned_capacity', 'response_link')),
  failovers INTEGER NOT NULL DEFAULT 0,
  error_code TEXT
);
CREATE INDEX idx_usage_ts ON usage_events(ts);
CREATE INDEX idx_usage_owner_ts ON usage_events(owner_user_id, ts);
CREATE INDEX idx_usage_client_key_ts ON usage_events(client_key_id, ts);
CREATE INDEX idx_usage_client_key_session_ts ON usage_events(client_key_id, session_hash, ts DESC)
  WHERE session_hash IS NOT NULL;

CREATE TABLE usage_attempts (
  request_id TEXT NOT NULL,
  attempt_no INTEGER NOT NULL CHECK (attempt_no BETWEEN 1 AND 3),
  ts INTEGER NOT NULL,
  upstream_key_id TEXT NOT NULL,
  share_id TEXT,
  outcome TEXT NOT NULL
    CHECK (outcome IN ('served', 'connect_error', 'write_stalled', 'http_429', 'http_402', 'http_401', 'http_403', 'http_5xx', 'wait_timeout', 'error_frame')),
  http_status INTEGER,
  bytes_sent INTEGER NOT NULL,
  charged_micro_usd INTEGER NOT NULL DEFAULT 0,
  charge_basis TEXT NOT NULL CHECK (charge_basis IN ('none', 'actual', 'estimated_input')),
  PRIMARY KEY (request_id, attempt_no)
);
CREATE INDEX idx_usage_attempts_key_ts ON usage_attempts(upstream_key_id, ts);

CREATE TABLE usage_rollups_5min (
  bucket_ms INTEGER NOT NULL, -- floor(ts / 300000) * 300000
  upstream_key_id TEXT NOT NULL, -- '' for pre-route rejections
  canonical_id TEXT NOT NULL,
  client_key_id TEXT NOT NULL,
  owner_user_id TEXT NOT NULL,
  provider_scope TEXT NOT NULL CHECK (provider_scope IN ('global', 'personal', 'shared', 'none')),
  share_id TEXT NOT NULL DEFAULT '',
  cost_micro_usd INTEGER NOT NULL,
  requests INTEGER NOT NULL,
  PRIMARY KEY (bucket_ms, upstream_key_id, canonical_id, client_key_id, share_id)
);
CREATE INDEX idx_rollups_5min_key ON usage_rollups_5min(upstream_key_id, bucket_ms);

CREATE TABLE usage_rollups_hourly (
  hour_ms INTEGER NOT NULL,
  owner_user_id TEXT NOT NULL,
  client_key_id TEXT NOT NULL,
  upstream_key_id TEXT NOT NULL, -- '' for pre-route rejections
  provider_id TEXT NOT NULL, -- '' for pre-route rejections
  provider_scope TEXT NOT NULL CHECK (provider_scope IN ('global', 'personal', 'shared', 'none')),
  share_id TEXT NOT NULL DEFAULT '',
  canonical_id TEXT NOT NULL,
  requests INTEGER NOT NULL,
  errors INTEGER NOT NULL,
  input_uncached_tokens INTEGER NOT NULL,
  cache_read_tokens INTEGER NOT NULL,
  cache_write_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  reasoning_tokens INTEGER NOT NULL,
  cost_micro_usd INTEGER NOT NULL,
  PRIMARY KEY (hour_ms, client_key_id, upstream_key_id, canonical_id, share_id)
);
CREATE INDEX idx_rollups_owner_hour ON usage_rollups_hourly(owner_user_id, hour_ms);
CREATE INDEX idx_rollups_share_hour ON usage_rollups_hourly(share_id, hour_ms);

CREATE TABLE auth_failure_counters (
  minute_ms INTEGER NOT NULL,
  reason TEXT NOT NULL CHECK (reason IN ('unknown_key', 'disabled', 'expired', 'revoked', 'owner_disabled')),
  ip_prefix TEXT NOT NULL, -- /24 for IPv4, /56 for IPv6
  count INTEGER NOT NULL,
  PRIMARY KEY (minute_ms, reason, ip_prefix)
);

CREATE TABLE trace_blocks (
  block_hash TEXT NOT NULL, -- hex SHA-256 of canonical block bytes (cache_control and citations removed)
  owner_user_id TEXT NOT NULL,
  payload_enc BLOB NOT NULL, -- Sealed zstd(canonical block JSON), domain 'trace_block'
  master_key_id TEXT NOT NULL,
  byte_len INTEGER NOT NULL,
  last_referenced_at INTEGER NOT NULL,
  PRIMARY KEY (owner_user_id, block_hash)
);
CREATE INDEX idx_trace_blocks_lastref ON trace_blocks(last_referenced_at);

CREATE TABLE trace_records (
  usage_event_id TEXT PRIMARY KEY REFERENCES usage_events(id) ON DELETE CASCADE,
  request_id TEXT NOT NULL UNIQUE,
  ts INTEGER NOT NULL,
  owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  client_key_id TEXT NOT NULL REFERENCES client_keys(id) ON DELETE CASCADE,
  session_hash TEXT NOT NULL,
  thread_id TEXT NOT NULL DEFAULT 'main', -- subagent_id or 'main'
  canonical_id TEXT NOT NULL,
  protocol TEXT NOT NULL CHECK (protocol IN ('chat', 'responses', 'messages')),
  truncated_content INTEGER NOT NULL DEFAULT 0,
  payload_enc BLOB NOT NULL, -- Sealed zstd(StoredTurnTrace v2), domain 'trace_record'
  master_key_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX idx_trace_session_order ON trace_records(owner_user_id, session_hash, ts ASC, request_id ASC);
CREATE INDEX idx_trace_owner_ts ON trace_records(owner_user_id, ts DESC);
CREATE INDEX idx_trace_client_key_session ON trace_records(client_key_id, session_hash, ts DESC);
CREATE INDEX idx_trace_expires ON trace_records(expires_at);

CREATE TABLE trace_record_blocks (
  usage_event_id TEXT NOT NULL REFERENCES trace_records(usage_event_id) ON DELETE CASCADE,
  owner_user_id TEXT NOT NULL,
  block_hash TEXT NOT NULL,
  PRIMARY KEY (usage_event_id, block_hash)
);
CREATE INDEX idx_trace_record_blocks_ref ON trace_record_blocks(owner_user_id, block_hash);

CREATE TABLE audit_log (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  ts INTEGER NOT NULL,
  actor_user_id TEXT,
  ip_addr TEXT,
  action TEXT NOT NULL,
  target_type TEXT NOT NULL,
  target_id TEXT,
  detail_json TEXT,
  prev_hash TEXT NOT NULL,
  hash TEXT NOT NULL
);
CREATE INDEX idx_audit_ts ON audit_log(ts DESC);
CREATE INDEX idx_audit_actor ON audit_log(actor_user_id, ts DESC);

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
```

### 9.2 Storage invariants, dual-queue write batching, and retention

Six storage rules keep SQLite fast, bounded, and crash-safe under concurrent streaming load.

| Rule | Implementation mechanism |
| --- | --- |
| Dual-queue writer (`Control` + `Data`) | Read queries run across eight read-only SQLite connections (`PRAGMA query_only = ON`) in pages of at most 50 turns or 5,000 rows per read transaction so no reader pins the WAL snapshot. All writes serialize through one dedicated writer actor fed by a high-priority `Control` channel (control-plane mutations + `audit_log` + `instance_meta.config_version` increment, awaited with a `2,000 ms` handler timeout) and three `Data` queues (`durable_state`, `rollups`, and `raw_events`). The writer drains all pending `Control` transactions before and after every data batch. Each data batch commits when `200` items accumulate, `4 MiB` of serialized row bytes accumulate, or `250 ms` elapse inside `BEGIN IMMEDIATE` ordered as: (1) `durable_state` (`pins`, `response_links`, `health_version` updates), (2) `rollups` (`usage_rollups_5min`, `usage_rollups_hourly`, `auth_failure_counters`), and (3) `raw_events` (`usage_events`, `usage_attempts`, `trace_blocks`, `trace_records`, `trace_record_blocks`). Any trace record exceeding `1 MiB` commits in its own standalone transaction. Each item runs in a nested `SAVEPOINT item_i` so a foreign-key violation rolls back only that item (`llmroller_writer_skipped_fk_total`). |
| Three-tier data queue backpressure | (1) `durable_state` (`Mutex<IndexMap<DurableKey, DurableOp>>`, coalescing by key, capped at `500,000` keys): on full, stops confirming new pins to disk and flips `/readyz` to 503 without dropping queued ops. (2) `rollups` (`Mutex<HashMap<RollupKey, RollupDelta>>`): additive coalescing for 5-minute and hourly rollups, never drops. (3) `raw_events` (`Mutex<VecDeque<RawItem>>` capped at `10,000` items and `64 MiB`): on overflow, evicts the oldest raw item (whose cost is already in `rollups`) and increments `llmroller_usage_dropped_total{kind}`. |
| Canonical AEAD encryption (`aad`) | Every encrypted blob uses the versioned format `0x01 \|\| nonce (24 bytes) \|\| ciphertext \|\| tag (16 bytes)` and the canonical length-prefixed AAD encoder in Section 9.2.2 so ciphertexts cannot be swapped across rows, tables, or master-key versions. |
| Content-addressed trace deduplication and truncation | Request blocks are hashed without `cache_control` or `citations` and stored once per `(owner_user_id, block_hash)` in `trace_blocks` (guarded by a 100,000-entry in-memory LRU cache to avoid redundant upserts). Binary/base64 blocks (`> 1 KiB`) are replaced before hashing with `{"type":"<orig>","llmroller_omitted":"binary","sha256":"<hex>","bytes":<int>}`. New request blocks per turn are capped at `4 MiB` (keeping newest blocks) and response outputs at `1 MiB` (keeping the first `512 KiB` and last `512 KiB` with `[llmroller: N bytes omitted]`). |
| WAL checkpointing and `SQLITE_FULL` handling | `PRAGMA wal_autocheckpoint = 1000` is paired with a 30 s background task running `PRAGMA wal_checkpoint(PASSIVE)`. If the `-wal` file exceeds `256 MiB`, the writer issues `PRAGMA wal_checkpoint(RESTART)` between batches; if it exceeds `1 GiB`, `/readyz` flips to 503 and `llmroller_wal_bytes` alerts. On `SQLITE_FULL`, the writer drops incoming `raw_events`, retries `durable_state` and `rollups` every `5 s`, flips `/readyz` to 503, and sets `llmroller_storage_full = 1`. |
| Retention policies | Raw `usage_events` and `usage_attempts` are kept for `usage_events_retention_days` (default `30` days) and capped at `usage_events_max_rows` (default `50,000,000` rows); `usage_rollups_5min` and `auth_failure_counters` for `35` days; `trace_records` and unreferenced `trace_blocks` (`last_referenced_at < cutoff`) for `traceability_config.retention_days` (default `14` days); `response_links` for `30` days (`1 h` when `store_flag = 0`); `retired` `model_routes` for `90` days; expired `pins` and `web_sessions` every `60 s`; and `usage_rollups_hourly` and `audit_log` indefinitely. Sweeps delete in portable chunks (`DELETE FROM usage_events WHERE id IN (SELECT id FROM usage_events WHERE id < :cutoff_ulid ORDER BY id LIMIT 1000)`) and bound trace deletions to `64 MiB` of blob bytes per transaction followed by `PRAGMA incremental_vacuum(500)`. |

#### 9.2.1 Canonical `audit_log` hash-chain serialization

Nullable columns (`actor_user_id`, `ip_addr`, `target_id`, `detail_json`) can be `NULL` or contain delimiter characters. To prevent canonicalization collisions, `crates/store::audit` computes `audit_log.hash` (`SEC-13`) over a length-prefixed byte buffer using `audit_hmac_key` from `instance_secrets`. The genesis `prev_hash` for the first row (`seq = 1`) is 64 ASCII zeros (`"0000000000000000000000000000000000000000000000000000000000000000"`).

```text
opt_field(NULL)    = 0x00
opt_field(Some(s)) = 0x01 || u32_be(len(s)) || utf8(s)
req_field(s)       = u32_be(len(s)) || utf8(s)

canonical_frame =
    "llmroller.audit.v1 "
 || i64_be(seq)
 || req_field(prev_hash)
 || req_field(id)
 || i64_be(ts)
 || opt_field(actor_user_id)
 || opt_field(ip_addr)
 || req_field(action)
 || req_field(target_type)
 || opt_field(target_id)
 || opt_field(detail_json)

hash = hex_lower(HMAC_SHA256(audit_hmac_key, canonical_frame))
```

#### 9.2.2 Canonical AEAD Associated Data (`aad`) specification

All six encrypted column families in SQLite use a single canonical Associated Data encoder in `crates/store::crypto`:

```text
aad(domain: &str, fields: &[&[u8]]) =
    "llmroller.aad.v1 "
 || u32_be(len(domain)) || utf8(domain)
 || (for f in fields: u32_be(len(f)) || f)
```

| Table and column | HKDF subkey info label | `domain` | `fields` (in exact order) |
| --- | --- | --- | --- |
| `instance_secrets.sealed` | `"llmroller.v1.instance_secret"` | `"instance_secret"` | `[master_key_id, name]` |
| `users.totp_secret_enc` | `"llmroller.v1.totp_secret"` | `"totp_secret"` | `[totp_master_key_id, users.id]` |
| `upstream_keys.secret_enc` | `"llmroller.v1.upstream_key"` | `"upstream_key"` | `[master_key_id, upstream_keys.id, provider_id]` |
| `upstream_usage_snapshots.raw_json_enc` | `"llmroller.v1.instance_secret"` | `"usage_raw"` | `[master_key_id, upstream_usage_snapshots.id]` |
| `trace_blocks.payload_enc` | `"llmroller.v1.trace_record"` | `"trace_block"` | `[master_key_id, owner_user_id, block_hash]` |
| `trace_records.payload_enc` | `"llmroller.v1.trace_record"` | `"trace_record"` | `[master_key_id, request_id, owner_user_id, session_hash]` |

### 9.3 JSON column schemas and closed runtime `settings` registry

Four SQLite columns (`providers.config_json`, `prices.tiers_json`, `prices.schedule_json`, and `settings.value_json`) store structured JSON documents validated with `serde` before persistence and compiled into `ConfigSnapshot`.

#### 9.3.1 `providers.config_json` (`ProviderConfigJson`)

Every provider row stores a serialized `ProviderConfigJson` object. Modifying any routing-relevant field recomputes `providers.share_config_fingerprint` in the same transaction.

```json
{
  "schema_version": 1,
  "inherit_preset_defaults": true,
  "paths": {
    "chat": "/chat/completions",
    "responses": "/responses",
    "messages": "/messages",
    "models": "/models"
  },
  "aux_endpoints": {
    "count_tokens": "probe",
    "responses_compact": "probe"
  },
  "auth": {
    "chat": "bearer",
    "responses": "bearer",
    "messages": "x_api_key_or_bearer"
  },
  "usage_reporting": {
    "chat_stream": "inject"
  },
  "strip_headers": ["x-experimental-*"],
  "strip_header_tokens": {
    "anthropic-beta": []
  },
  "add_headers": {},
  "response_header_allow": [],
  "ua_policy": "label",
  "session_profile": {
    "headers_if_absent": ["x-opencode-session"],
    "body_field_if_absent": null,
    "max_len": 256
  },
  "usage_profile": {
    "enabled": true,
    "endpoints": ["https://opencode.ai/zen/go/v1/usage"],
    "parser_kind": "opencode_go"
  },
  "timeouts": {
    "timeout_failover": false,
    "first_activity_timeout_ms": 60000,
    "first_valid_frame_timeout_ms": 180000,
    "nonstream_response_timeout_ms": 600000
  },
  "error_classifiers": {
    "quota_exhausted_substrings": ["quota_exceeded", "window_limit_reached", "insufficient_credits"],
    "request_rejected_substrings": ["flagged", "moderation"],
    "plan_restricted_substrings": ["upgrade_required", "not available on your plan"],
    "credential_invalid_substrings": ["invalid_api_key", "invalid x-api-key", "unauthorized"],
    "model_not_found_substrings": ["model_not_found", "does not exist", "unknown model"]
  }
}
```

#### 9.3.2 `prices.tiers_json` and `prices.schedule_json`

Context-length pricing tiers (`tiers_json`) apply when `total_input_tokens >= min_total_input_tokens` and (`max_total_input_tokens` is `null` or `total_input_tokens <= max_total_input_tokens`); overlapping tiers are rejected on write with HTTP 422. Time-of-day schedules (`schedule_json`) use half-open `[start, end)` UTC minute ranges (`0..1440`) evaluated at request start (`ts` of first byte sent upstream); overlapping schedules on one price row are rejected with HTTP 422.

```json
{
  "tiers": [
    {
      "min_total_input_tokens": 200001,
      "max_total_input_tokens": null,
      "input_per_mtok_micro_usd": 4000000,
      "output_per_mtok_micro_usd": 16000000,
      "cache_read_per_mtok_micro_usd": 400000,
      "cache_write_per_mtok_micro_usd": 5000000
    }
  ],
  "schedules": [
    {
      "name": "deepseek_weekday_peak",
      "iso_weekdays": [1, 2, 3, 4, 5],
      "utc_minute_ranges": [[60, 240], [360, 600]],
      "multiplier_num": 2,
      "multiplier_den": 1
    }
  ]
}
```

#### 9.3.3 Closed runtime `settings` registry

The `settings` table enforces a closed 14-key registry. Any unknown key or out-of-range value submitted to `PATCH /admin/api/settings` is rejected with HTTP 422.

| `settings.key` | Type & validation range | Default `value_json` | Purpose and hot-reload target in `ConfigSnapshot` |
| --- | --- | --- | --- |
| `pin_ttl_secs` | Integer `300..=2_592_000` | `86400` | Sliding idle expiration window in seconds for confirmed session pins (`RTE-10`). |
| `max_active_pins` | Integer `1_000..=1_000_000` | `250000` | Maximum entries in `PinStore` (`~72 MB` at 250k); overflow routes unpinned (`unpinned_capacity`, `RTE-10`). |
| `usage_poll_interval_secs` | Integer `60..=3600` | `300` | Periodic polling interval in seconds for active upstream keys' `/usage` endpoints (`USG-4`). |
| `default_routing_strategy` | Enum `"smooth_weighted_rr" \| "naive_rr" \| "weighted_rr" \| "uniform_random" \| "weighted_random" \| "power_of_two_choices"` (aliases `"random" -> "uniform_random"`, `"p2c" -> "power_of_two_choices"`) | `"smooth_weighted_rr"` | Instance-wide default routing algorithm used when `models.routing_strategy IS NULL` (`RTE-4`, `RTE-14`). |
| `headroom_scaling_enabled` | Boolean | `false` | Scales weighted route weights by remaining budget basis points `bps in 500..=10_000` (`RTE-13`). |
| `repin_on_personal_recovery` | Boolean | `true` | Enables recovery repin from a fallback tier back to a higher priority tier once idle (`RTE-2`). |
| `recovery_repin_min_idle_secs` | Integer `0..=86400` | `300` | Minimum idle seconds since the pin's previous served turn before a recovery repin is permitted (`RTE-2`). |
| `global_exposure_mode` | Enum `"union" \| "intersection"` | `"union"` | Controls automatic global exposure when `models.exposed IS NULL` (`MDL-3`). |
| `usage_events_retention_days` | Integer `1..=365` | `30` | Retention window in days for raw `usage_events` and `usage_attempts` rows (`REL-3`). |
| `usage_events_max_rows` | Integer `>= 1_000_000` | `50000000` | Maximum raw `usage_events` row count before the sweeper prunes oldest rows (`A6`). |
| `drain_timeout_secs` | Integer `0..=3600` | `60` | Maximum seconds to wait for open SSE streams to finish after `SIGTERM` (`REL-1`). |
| `price_sync_catalog_url` | Object `{enabled: bool, url: https}` | `{"enabled":false,"url":"https://models.dev/api.json"}` | Opt-in external price-book synchronization configuration (`PRIV-1`, `USG-3`). |
| `unpriced_fallback_rates` | Object `{input, output, cache_read, cache_write}` | `{"input":3000000,"output":15000000,"cache_read":300000,"cache_write":3750000}` | Fallback micro-USD per MTok rates charged whenever no `prices` row matches (`USG-3`). |
| `traceability_config` | Object `{enabled, match, retention_days 1..=90, auto_disable_at}` | `{"enabled":false,"match":{"user_ids":[],"client_key_ids":[],"models":[]},"retention_days":14,"auto_disable_at":null}` | Admin opt-in content traceability mode configuration (`TRC-1`, `TRC-7`). |

## 10. Routing design

The routing engine in `crates/core` is a pure, deterministic state machine with zero network or disk I/O on the critical path. It evaluates request gates (`G1` through `G7`), resolves the session and routing constraint, and checks candidate eligibility (`C1` through `C8`). It then selects routes through the pluggable `RouteSelector` strategy engine (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, or `power_of_two_choices`) and manages single-flight pinning and pre-first-valid-frame failover.

### 10.0 Ordered request gates (`G1` through `G7`)

Before inspecting candidate routes, every request on `/v1/*` (except `/healthz` and `/readyz`) is evaluated against seven ordered request gates. The first failing gate returns its deterministic HTTP status and protocol error envelope immediately.

| Gate | Predicate (must hold) | Failure response |
| --- | --- | --- |
| `G1` | Bearer or `x-api-key` token hashes (`SHA-256`) in constant time to a known `client_keys` row in `ConfigSnapshot` via either `key_hash` (`HashRole::Current`) or `prev_key_hash` with `now_ms < prev_key_expires_at` (`HashRole::Previous`) | HTTP 401 `authentication_error` (increments `auth_failure_counters`) |
| `G2` | `client_key.disabled == 0 AND client_key.revoked_at IS NULL AND (client_key.expires_at IS NULL OR client_key.expires_at > now_ms)` | HTTP 401 `authentication_error` (increments `auth_failure_counters`) |
| `G3` | Owner account `users[client_key.owner_user_id].disabled == 0` | HTTP 401 `authentication_error` (increments `auth_failure_counters`) |
| `G4` | `client_key.allowed_cidrs IS NULL` or the resolved client IP (after `LLMROLLER_TRUSTED_PROXIES` extraction) falls inside at least one allowed CIDR | HTTP 403 `permission_error` |
| `G5` | `client_key.rate_limit_rpm IS NULL` or the client key's token bucket admits one token at `now_ms` | HTTP 429 `rate_limit_error` with `Retry-After` set to seconds until next token |
| `G6` | For endpoints carrying a `model`, the model resolves to a known `model_pk` (`canonical_id = M`, returning 409 on ambiguous raw ID) and `client_key.allowed_models IS NULL OR M in client_key.allowed_models` | HTTP 404 `model_not_found` if `M` is unknown, else HTTP 403 `permission_error` |
| `G7` | For `EndpointKind::Inference`, `client_key.budget_micro_usd_month IS NULL` or the client key's UTC monthly accumulator admits the request reservation | HTTP 429 `rate_limit_error` with `code = "client_key_budget_exhausted"` and `Retry-After` set to seconds until next UTC month |

### 10.1 Candidate eligibility rules (`C1` through `C8`)

Consider a request from `client_key` (owner `U`) for model `M` (`model_pk`) and protocol `P` that passed gates `G1` through `G7`. A route `R = (upstream_key K, model_pk)` is a valid candidate only when all eight checks below hold.

```text
fn effective_share(U: UserId, K: UpstreamKeyId) -> Option<&ShareGrant>:
    // Key-level active share takes strict precedence over provider-level active share.
    if let Some(s) = key_level_shares.get(&(U, K))
        where s.enabled && s.revoked_at.is_none() && s.accepted_at.is_some()
          && s.accepted_config_fingerprint == providers[K.provider_id].share_config_fingerprint
          && (providers[K.provider_id].kind != Custom || s.admin_approved_at.is_some()) {
        return Some(s);
    }
    if let Some(s) = provider_level_shares.get(&(U, K.provider_id))
        where s.enabled && s.revoked_at.is_none() && s.accepted_at.is_some()
          && s.accepted_config_fingerprint == providers[K.provider_id].share_config_fingerprint
          && (providers[K.provider_id].kind != Custom || s.admin_approved_at.is_some()) {
        return Some(s);
    }
    None
```

| Check | Condition that must hold for route `R = (K, M)` |
| --- | --- |
| `C1` | `provider.enabled == 1 AND K.enabled == 1 AND R.enabled == 1 AND R.status == 'active'` |
| `C2` | Authorization tier membership under `client_key.routing_scope`: `K` belongs to tier `Own` (`provider.ownership_scope == 'personal' AND provider.owner_user_id == U`), tier `Shared` (`effective_share(U, K)` returns `Some(s)` with `s.allowed_models IS NULL OR M in s.allowed_models`), or tier `Global` (`provider.ownership_scope == 'global' AND U.can_use_global_providers == 1`). |
| `C3` | For `personal` providers (tiers `Own` and `Shared`): the provider owner account has `users[provider.owner_user_id].disabled == 0`. |
| `C4` | Model exposure and merge review: `models[M].exposed IS DISTINCT FROM 0` (admin `exposed = 0` is an instance-wide kill switch across all tiers), and if `models[M].merge_state == 'pending_review'`, `R.vendor_prefix` matches the majority vendor prefix of `M`. For tier `Global`, `models[M].exposed == 1` or `M` satisfies `global_exposure_mode` (`union` or `intersection` over enabled global providers with at least one enabled, non-`invalid` key). For tiers `Own` and `Shared`, `user_route_preferences(U, K, M).enabled` is `NULL` or `1`. |
| `C5` | `R.protocols` contains the inbound protocol `P` and `(K, M, P)` is not blocked in `route_protocol_blocks` (`now_ms >= until_ms`). |
| `C6` | Both key health `K.health` and route health `R.route_health` in `RuntimeState` are `healthy`, or are `cooling_down` / `exhausted` with `now_ms >= health_until`. |
| `C7` | The `OriginBreaker` for `K`'s upstream origin `OriginKey { scheme, host, port, allow_private_network }` is `Closed`, or is `HalfOpen` with a free probe slot. |
| `C8` | For `EndpointKind::Inference` (skipped for `Auxiliary` and `StateBound`), `try_reserve` succeeds across the ordered counter set for `(K, M, U, client_key)`: every key window on `K`, every route window on `(K, M)`, the share UTC monthly accumulator for `effective_share(U, K)` (when tier is `Shared`), and `U`'s global UTC monthly accumulator (when tier is `Global`). |

### 10.2 Three-tier pool resolution by `routing_scope`

Routes passing `C1` through `C8` are partitioned into `Tier_Own`, `Tier_Shared`, and `Tier_Global`. The router evaluates the ordered tier groups for the effective `routing_scope` and selects the first non-empty tier group `C`.

| Routing scope mode | Ordered tier groups evaluated | Pool tier recorded on pin (`PoolTier`) |
| --- | --- | --- |
| `personal_first` (default) | `1. [Tier_Own]`, `2. [Tier_Shared]`, `3. [Tier_Global]` | `Own (0)`, `Shared (1)`, or `Global (2)` |
| `blended` | `1. [Tier_Own U Tier_Shared U Tier_Global]` | `Blended (3)` |
| `personal_only` | `1. [Tier_Own]`, `2. [Tier_Shared]` | `Own (0)` or `Shared (1)` |
| `own_only` | `1. [Tier_Own]` | `Own (0)` |
| `global_only` | `1. [Tier_Global]` | `Global (2)` |

When all allowed tier groups are empty because of share or user monthly caps, the router returns HTTP `429 rate_limit_error` (`code = "monthly_cap_exhausted"`). Otherwise it returns HTTP `503 no_eligible_route` with `Retry-After` set to the earliest positive recovery delay.

### 10.3 Pluggable routing strategy engine, effective priority, and headroom scaling

For each candidate route `r = (K, M)` in the active tier group `C`, the base integer weight `W(U, r)` in `1..=1000` is resolved in three steps:
1. For `Own` or `Shared` routes, use `user_route_preferences(U, K, M).priority_override` when set.
2. Else if `R.priority_override` is set on `model_routes`, use `R.priority_override`.
3. Otherwise use `upstream_keys.priority`.

When `headroom_scaling_enabled` is active (`RTE-13`), let `windows` be the union of key windows on `K` (`scope_kind = 'upstream_key'`) and route windows on `(K, M)` (`scope_kind = 'route'`). The remaining budget basis points `bps` and effective weight `W_eff(U, r)` are computed as:

```text
if windows.is_empty() {
    bps = 10_000;
} else {
    bps = clamp(
        min over w in windows of floor(
            10_000 * max(0, w.limit - w.effective_used(now_ms) - w.reserved_in_flight) / w.limit
        ),
        500,
        10_000
    );
}
W_eff(U, r) = max(1, (W(U, r) * bps) / 10_000);
```

When `models.distribution == 'strict'` (`RTE-6`), `C` is filtered to candidates with the minimum `strict_tier` (`1` is highest, `10` is lowest) before invoking the configured routing strategy. Candidate slices passed into the selector are always sorted in ascending `RouteId` (`(upstream_key_id, model_pk)`) order so index arithmetic and tie-breaking are deterministic across threads.

#### 10.3.1 Common `RouteSelector` trait and `RoutingStrategy` enum (`RTE-4`, `RTE-14`)

Target selection is decoupled from candidate filtering via the thread-safe `RouteSelector` trait in `crates/core::strategy`. Each selection group is keyed by `WrrGroupKey { user_id, model_pk, protocol, pool_tier }` so one user's route overrides or selection cursor never perturbs another user's distribution.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStrategy {
    SmoothWeightedRr,      // "smooth_weighted_rr" (default)
    NaiveRr,               // "naive_rr"
    WeightedRr,            // "weighted_rr"
    UniformRandom,         // "uniform_random" (deserializes alias "random")
    WeightedRandom,        // "weighted_random"
    PowerOfTwoChoices,     // "power_of_two_choices" (deserializes alias "p2c")
}

pub trait RouteSelector: Send + Sync {
    fn strategy(&self) -> RoutingStrategy;
    fn select_next(
        &self,
        group_key: &WrrGroupKey,
        candidates: &[(RouteId, i64)],
        now_ms: i64,
        entropy: &dyn EntropySource,
        load_tracker: &RouteLoadTracker,
    ) -> Option<RouteId>;
    fn on_weights_updated(&self, group_key: &WrrGroupKey);
    fn reset_group(&self, group_key: &WrrGroupKey);
    fn evict_idle_groups(&self, now_ms: i64, max_idle_ms: i64);
}
```

#### 10.3.2 Supported routing algorithms and mathematical invariants

LLMRoller ships six built-in `RouteSelector` implementations dispatched by `RoutingStrategyEngine`. All six algorithms return `None` immediately when `candidates` is empty.

| Strategy enum (`RoutingStrategy`) | Uses `W_eff` weights | State per `WrrGroupKey` | Concurrency primitive | Mathematical invariant and selection complexity |
| --- | --- | --- | --- | --- |
| `smooth_weighted_rr` (default) | Yes (`1..=1000`) | `WrrGroup` (`members: Vec<WrrMember>`, `last_used_ms`) | `parking_lot::Mutex<WrrGroup>` (`< 1 us`) | Nginx SWRR: `m.current += W_eff`, pick `argmax(m.current, r.id_bytes)`, `winner.current -= sum(W_eff)`. Preserves `sum(current) == 0` without clamping; evicts members absent `>= 300,000 ms`. Max consecutive run `<= ceil(max(W) / min(W))`. `O(N)`. |
| `naive_rr` | No (treats each healthy candidate equally as weight `1`) | `NaiveRrGroup { cursor: AtomicU64, last_used_ms: AtomicI64 }` | Lock-free `AtomicU64::fetch_add(1, Ordering::Relaxed)` | Cyclic unweighted round-robin: `idx = (cursor.fetch_add(1, Relaxed) % (N as u64)) as usize`, returning `candidates[idx].0`. Unsigned `u64` wrap-around at `u64::MAX` is well-defined (`wrapping_add`). Over `K * N` picks with fixed `N`, each route receives exactly `K` picks. `O(1)`. |
| `weighted_rr` | Yes (`1..=1000`, reduced by `gcd`) | `ClassicalWrrGroup { index: usize, current_weight: i64, last_used_ms: i64 }` | `parking_lot::Mutex<ClassicalWrrGroup>` (`< 1 us`) | Superset/LVS sequential block-weighted cycle: steps `index = (index + 1) % N`; whenever `index == 0`, sets `cw -= gcd(W_eff)` and wraps `cw = max(W_eff)` when `cw <= 0`, picking the first candidate with `W_eff >= cw`. Exact share `W_eff_i / sum(W_eff)` every `sum(W_eff) / gcd(W_eff)` picks. `O(N)`. |
| `uniform_random` (alias `random`) | No (uniform `1 / N` across healthy candidates) | Stateless | Lock-free `thread_local!` `ChaCha12Rng` seeded from `EntropySource` | Pseudo-random uniform pick: draws unbiased `idx in 0..N` using Lemire's divisionless rejection method (zero global RNG mutex contention; deterministic in tests when driven by `DeterministicRng`). `P(route_i) = 1 / N`. `O(1)`. |
| `weighted_random` | Yes (`1..=1000`) | Stateless | Lock-free `thread_local!` `ChaCha12Rng` seeded from `EntropySource` | Builds cumulative prefix weights `prefix: SmallVec<[i64; 16]>` (`prefix[i] = sum_{k=0..=i} W_eff_k`, `total_w = prefix[N - 1]`), draws unbiased `draw in 0..total_w`, and binary-searches `idx = prefix.partition_point(\|&c\| c <= draw)`. `P(route_i) = W_eff_i / total_w`. `O(N)` prefix build (`<= 16` inline on stack) + `O(log N)` binary search. |
| `power_of_two_choices` (alias `p2c`) | Yes (`1..=1000` combined with live load) | Per-route `RouteLoadAtomics` in `RuntimeState` (`in_flight: AtomicU32`, `ewma_ttfb_ms: AtomicU64`) | Lock-free atomic loads + thread-local `ChaCha12Rng` | Samples two distinct candidates `(a, b)` without replacement via `weighted_random` (or returns `candidates[0]` when `N == 1`) and picks `argmin_{r in {a, b}} (load_score(r), r.id_bytes)` where `load_score(r) = ((in_flight(r) + 1) * ewma_ttfb_ms(r) * 1000) / W_eff(r)`. Mitigates herd effects on slow upstreams. `O(log N)`. |

##### 10.3.2.1 Reference implementation of the pluggable algorithms

The pseudocode below specifies the six selection algorithms in `crates/core::strategy`.

```text
const WRR_MEMBER_EVICT_MS: i64 = 300_000; // 5 minutes

// 1. Smooth Weighted Round Robin (smooth_weighted_rr)
fn select_smooth_wrr(group_key: &WrrGroupKey, candidates: &[(RouteId, i64)], now_ms: i64) -> RouteId {
    let mut group = wrr_state.get_or_insert(group_key).lock();
    for (route, _) in candidates {
        if !group.members.iter().any(|m| m.route == *route) {
            group.members.push(WrrMember { route: *route, current: 0, last_present_ms: now_ms });
        }
    }
    for m in &mut group.members {
        if candidates.iter().any(|(r, _)| *r == m.route) {
            m.last_present_ms = now_ms;
        }
    }
    // Evict members absent for >= 5 minutes and rebalance their residual weight so sum(current) stays 0.
    group.members.retain(|m| {
        candidates.iter().any(|(r, _)| *r == m.route) || now_ms - m.last_present_ms < WRR_MEMBER_EVICT_MS
    });
    let total_w: i64 = candidates.iter().map(|(_, w)| *w).sum();
    for m in &mut group.members {
        if let Some((_, w)) = candidates.iter().find(|(r, _)| *r == m.route) {
            m.current += *w;
        }
    }
    let winner = group.members.iter_mut()
        .filter(|m| candidates.iter().any(|(r, _)| *r == m.route))
        .max_by(|a, b| a.current.cmp(&b.current).then_with(|| b.route.cmp(&a.route)))
        .expect("non-empty candidates");
    winner.current -= total_w;
    group.last_used_ms = now_ms;
    winner.route
}

// 2. Naive Unweighted Round Robin (naive_rr)
fn select_naive_rr(group_key: &WrrGroupKey, candidates: &[(RouteId, i64)], now_ms: i64) -> RouteId {
    let entry = naive_rr_state.get_or_insert(group_key);
    entry.last_used_ms.store(now_ms, Ordering::Relaxed);
    let seq = entry.cursor.fetch_add(1, Ordering::Relaxed);
    let idx = (seq % (candidates.len() as u64)) as usize;
    candidates[idx].0
}

// 3. Uniform Random (uniform_random) and 4. Weighted Random (weighted_random)
fn select_weighted_random(candidates: &[(RouteId, i64)], entropy: &dyn EntropySource) -> RouteId {
    let mut prefix: SmallVec<[i64; 16]> = SmallVec::with_capacity(candidates.len());
    let mut acc: i64 = 0;
    for (_, w) in candidates {
        acc += (*w).max(1);
        prefix.push(acc);
    }
    let draw = entropy.next_u64_bounded(acc as u64) as i64;
    let idx = prefix.partition_point(|&cum| cum <= draw);
    candidates[idx].0
}

// 5. Power of Two Choices (power_of_two_choices / p2c)
fn select_p2c(
    candidates: &[(RouteId, i64)],
    entropy: &dyn EntropySource,
    load: &RouteLoadTracker,
) -> RouteId {
    if candidates.len() == 1 {
        return candidates[0].0;
    }
    let first = select_weighted_random_index(candidates, entropy);
    let second = select_weighted_random_index_excluding(candidates, first, entropy);
    let score = |idx: usize| -> (u64, RouteId) {
        let (route, w) = candidates[idx];
        let in_flight = load.in_flight(route) as u64;
        let ttfb_ms = load.ewma_ttfb_ms(route).max(1);
        (((in_flight + 1) * ttfb_ms * 1000) / (w.max(1) as u64), route)
    };
    score(first).min(score(second)).1
}
```

The 60 s background sweeper evicts idle state entries (`WrrGroup`, `NaiveRrGroup`, `ClassicalWrrGroup`) where `now_ms - last_used_ms >= 86_400_000` (24 h).

#### 10.3.3 Strategy configuration schema, precedence, and weight validation rules

The effective `RoutingStrategy` for a request to canonical model `M` is resolved in three steps:
1. If `models[M].routing_strategy` is non-null, use `models[M].routing_strategy`.
2. Else use `ConfigSnapshot.settings.default_routing_strategy` (persisted in `settings` under key `"default_routing_strategy"`).
3. At startup, validate `LLMROLLER_DEFAULT_ROUTING_STRATEGY` against the six enum identifiers (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random` / `random`, `weighted_random`, `power_of_two_choices` / `p2c`). An invalid environment variable value aborts startup immediately with exit code `2`.

Weight and fallback rules across strategies:
1. Priority weights (`upstream_keys.priority`, `model_routes.priority_override`, `user_route_preferences.priority_override`) are always validated (`1..=1000`) and stored in SQLite even when an unweighted strategy (`naive_rr` or `uniform_random`) is currently selected.
2. When `naive_rr` or `uniform_random` is active on model `M`, the UI and route APIs return `"effective_strategy"` and display each route share as `100% / N`. Stored priority weights stay intact so switching back to a weighted strategy restores them without re-entry.
3. Updating `models.routing_strategy` or `settings.default_routing_strategy` increments `instance_meta.config_version` and emits `CacheInvalidationEvent::StrategyChanged`. It resets in-memory selector counters for the affected models while keeping confirmed pins in `PinStore` intact.
4. Every route selection increments `llmroller_routing_selections_total{provider_kind,provider_id,protocol,outcome,strategy,model_bucket}` where `strategy` is one of the six canonical snake_case strategy names (`OBS-1`).

#### 10.3.4 Worked routing example

Consider three upstream keys with default priorities matching their general plan tiers: Open Design AMR (`100`), OpenCode Go (`60`), and Command Code GOAT (`30`). The table below uses illustrative per-model monthly allowances to show how per-model `priority_override` values adjust session shares.

| Canonical model | Upstream key | Illustrative per-model monthly allowance | Key default priority | Route priority override | Effective weight `W` | Share of new sessions |
| --- | --- | --- | --- | --- | --- | --- |
| `deepseek-v4.1-flash` | Open Design AMR | $120 | 100 | None | 100 | 52.6% (100 / 190) |
| `deepseek-v4.1-flash` | OpenCode Go | $80 | 60 | None | 60 | 31.6% (60 / 190) |
| `deepseek-v4.1-flash` | Command Code GOAT | $60 | 30 | None | 30 | 15.8% (30 / 190) |
| `mimo-v2.6-pro` | Open Design AMR | $60 | 100 | 60 | 60 | 63.2% (60 / 95) |
| `mimo-v2.6-pro` | Command Code GOAT | $20 | 30 | 20 | 20 | 21.1% (20 / 95) |
| `mimo-v2.6-pro` | OpenCode Go | $15 | 60 | 15 | 15 | 15.8% (15 / 95) |

### 10.4 Session pin state machine (`PinStore`) and stateful response links

Session pins live in `PinStore` (`DashMap<PinKey, Arc<parking_lot::Mutex<PinEntry>>>`, where `PinKey = (client_key_id, canonical_id, session_hash)`), using the `Clock` seam for all timestamps so `FakeClock` tests run deterministically without sleeping. `PinStore` is capped at `max_active_pins` (default `250,000`, `~72 MB`); when `len() >= max_active_pins` on a miss, the request routes via WRR without inserting a pin (`pin_outcome = 'unpinned_capacity'`, incrementing `llmroller_pin_capacity_overflow_total`).

```rust
pub enum PinState { Provisional, Confirmed }

pub struct PinEntry {
    pub state: PinState,
    pub upstream_key_id: UpstreamKeyId,
    pub generation: u64,
    pub pool_tier: PoolTier,
    pub created_ms: i64,
    pub last_used_ms: i64,
    pub expires_ms: i64, // Confirmed: last_used + pin_ttl; Provisional: created + 50_000
    pub repin_count: u32,
    pub tried_this_generation: SmallVec<[UpstreamKeyId; 3]>,
}
```

All transitions on a `PinEntry` run synchronously (`< 5 us`) under the entry mutex and follow the table below. The entry mutex is never held across an `.await`.

| Event | Current state | Guard condition | Action taken under entry mutex | Next state |
| --- | --- | --- | --- | --- |
| `Acquire(req)` | Absent | `PinStore.len() < max_active_pins` | Run selector `select(C)` -> `(K, tier)`; insert `Provisional { K, gen: 1, pool_tier: tier, expires_ms: now + 50_000 }`; return `PinTicket { K, gen: 1 }` (`pin_outcome = 'new_pin'`) | `Provisional` |
| `Acquire(req)` | `Provisional` | `now < expires_ms` and `K` passes `C1..C8` | Return follower `PinTicket { K, gen }` | `Provisional` |
| `Acquire(req)` | `Provisional` | `now >= expires_ms` or `K` fails `C1..C8` | Reselect excluding `K`; `gen += 1`; reset `expires_ms = now + 50_000` | `Provisional` |
| `Acquire(req)` | `Confirmed` | `now < expires_ms`, `K` passes `C1..C8`, and recovery repin does not fire | `last_used_ms = now`; return `PinTicket { K, gen }` (`pin_outcome = 'followed'`) | `Confirmed` |
| `Acquire(req)` | `Confirmed` | `now < expires_ms`, `K` passes `C1..C8`, `repin_on_personal_recovery == true`, `routing_scope in {personal_first, personal_only}`, `pin.pool_tier > best_tier(req)`, and `now - last_used_ms >= recovery_repin_min_idle_secs * 1000` | Select `(K_new, best_tier)` from `best_tier`; `K = K_new`; `pool_tier = best_tier`; `gen += 1`; `repin_count += 1`; return ticket (`pin_outcome = 'repin_recovery'`) | `Provisional` |
| `Acquire(req)` | `Confirmed` | `now >= expires_ms` | Enqueue durable delete of expired row; reselect as new `Provisional` (`gen += 1`, `pin_outcome = 'new_pin'`) | `Provisional` |
| `Acquire(req)` | `Confirmed` | `K` fails `C1..C8` | Reselect excluding `K`; `gen += 1`; `repin_count += 1`; return ticket (`pin_outcome = 'repin'`) | `Provisional` |
| `FirstValidFrame(ticket)` | Any | `ticket.generation == gen && ticket.upstream_key_id == K` | `state = Confirmed`; `last_used_ms = now`; `expires_ms = now + pin_ttl_ms`; enqueue durable `pins` upsert on `durable_state` (coalesced to once per 60 s for sliding TTL updates) | `Confirmed` |
| `FirstValidFrame(ticket)` | Any | `ticket.generation != gen` | No pin change (stream already committed upstream); increment `llmroller_pin_stale_confirm_total` | Unchanged |
| `PreFirstByteFailure(ticket, K_failed)` | Any | `ticket.generation == gen` | Push `K_failed` to `tried_this_generation`; reselect excluding `tried_this_generation`; `gen += 1`; `expires_ms = now + 50_000`; return new ticket | `Provisional` |
| `PreFirstByteFailure(ticket, K_failed)` | Any | `ticket.generation < gen` | **Adopt winner:** if current `K` is not in caller's tried set and passes `C1..C8`, return current `(K, gen)`; else reselect excluding union of tried sets and bump `gen += 1` | Unchanged or `Provisional` |
| `AdminMove(K_new)` | `Confirmed` | `K_new` passes `C1..C8` | `K = K_new`; `gen += 1`; `last_used_ms = now`; enqueue durable `pins` upsert | `Confirmed` |
| `Invalidate(target)` | Any | `K` or `client_key_id` matches `target` | Remove entry from `PinStore`; enqueue durable `pins` delete | Absent |
| `Sweep` (every 60 s) | `Provisional` or `Confirmed` | `now >= expires_ms` | Remove entry from `PinStore` | Absent |

#### 10.4.1 Stateful Responses hard-route decision table (`RTE-12`)

When `resolve_session_and_constraint` (Section 11.1.2) resolves a `response_links` row for `upstream_key_id = K`, the request uses `RoutingConstraint::HardKey(K)`. The router then applies the decision table below.

| Condition on linked key `K` | Router action | `pin_outcome` | Side effect on `PinStore` |
| --- | --- | --- | --- |
| Passes `C1`..`C5`, and (`EndpointKind::StateBound` or passes `C6`..`C8`) | Route to `K` with failover **disabled** (`HardKey(K)`) | `response_link` | Upsert pin `(client_key_id, canonical_id, inherited_session_hash) -> K` as `Confirmed` (bumping `gen` and `repin_count` only if prior pin target differed). |
| Passes `C1`..`C5`, but fails `C6` (`cooling_down` / `exhausted`), `C7` (breaker `Open`), or `C8` (window/cap full) | Return HTTP 429 `rate_limit_error` with `Retry-After` set to seconds until `health_until`, breaker probe, or window reset; **never** route to another key because the response state exists only on `K` | `response_link` | None |
| Fails `C2` (share revoked, config fingerprint changed, or global access revoked), `C3` (owner disabled), or `C1` (key/provider disabled) | Return HTTP 403 `permission_error` (`"the upstream key that holds previous_response_id is no longer available to this client key"`) | `response_link` | Remove pin if it points to `K`. |
| Fails `C4` (model force-disabled) or `C5` (protocol blocked) | Return HTTP 403 `permission_error` | None | None |

### 10.5 Error classification, endpoint kinds, key health, and pre-first-byte failover

Every `/v1/*` routed request has a `RoutingConstraint` (`Free`, `PinnedPreferred(K)`, or `HardKey(K)`) and an `EndpointKind`:

| Endpoint path | `EndpointKind` | Budget check (`C8`) | Failover allowed | 404 behavior |
| --- | --- | --- | --- | --- |
| `POST /v1/chat/completions`, `/v1/responses`, `/v1/messages` | `Inference` | Yes | Per Section 10.5.2 (disabled if `HardKey`) | Model-missing 404 -> `route_health = unavailable` for `10 min` + trigger `/models` refresh for `K`; other 404 -> pass through unchanged |
| `POST /v1/responses/compact` | `Inference` | Yes | `HardKey` if `previous_response_id` present, else `PinnedPreferred` / `Free` | As `Inference` |
| `POST /v1/messages/count_tokens` | `Auxiliary` | No | Yes (any eligible key, pin preferred) | Pass through; set `aux_unsupported(K, count_tokens)` for `6 h` (when unsupported on all eligible keys, answer locally with HTTP 404 `not_found_error`) |
| `GET`, `DELETE /v1/responses/{id}`, `/input_items`, `/cancel` | `StateBound` | No | Never (`HardKey`; unknown link returns local 404 `response_not_found`) | Pass through unchanged, zero health effect |

Before commit, `crates/proto::adapters::classify` inspects any upstream HTTP error or initial SSE error frame. It matches the first `8 KiB` of the body case-insensitively against five ordered classes:

| Order | Classifier class | Match rule | Health and `OriginBreaker` effect | Failover | Client sees |
| --- | --- | --- | --- | --- | --- |
| 1 | `QuotaExhausted` | Status in `{402, 403, 429}` and body matches `quota_exhausted_substrings` | Set `route_health` (if body names model) or `key.health` to `exhausted` (`health_version += 1`, persisted to SQLite) with `health_until = coalesce(resets_at_ms, now + 300_000)`; trigger single-flight `/usage` poll | Yes (unless `HardKey`) | Terminal error selector if all tries fail |
| 2 | `RequestRejected` | Status `403` matching `request_rejected_substrings` (`"flagged"`, `"moderation"`, or `"permission_error"` with `"content"`), or status in `{400, 404 (not model-missing), 408, 409, 413, 422, 499}` | None (key, route, and breaker stay healthy) | No | Upstream status and scrubbed body verbatim |
| 3 | `PlanRestricted` | Status `403` matching `plan_restricted_substrings` (`"upgrade_required"`, `"not available on your plan"`) | Insert `route_protocol_blocks(K, model_pk, protocol, until_ms = next_discovery)` | Yes (unless `HardKey`) | Terminal error selector if all tries fail |
| 4 | `CredentialInvalid` | Status `401`, or status `403` matching `credential_invalid_substrings` (`"invalid_api_key"`, `"invalid x-api-key"`, `"unauthorized"`) | Record strike in `strikes[K]` for `client_key_id` (10-minute sliding window): if `strikes[K].len() >= 2` (two distinct client keys), mark `key.health = invalid` (`health_version += 1`, persisted, 15-min auto-recovery probe); else spawn a single-flight `GET /models` verification probe (`5 s` timeout) that marks `K` `invalid` on 401/403 or clears `strikes[K]` on 2xx | Yes (unless `HardKey`) | `502 upstream_unreachable` if all tries fail (never forwards upstream 401/403) |
| 5 | `Unclassified403` | Any other status `403` | None; increment `llmroller_upstream_unclassified_403_total{provider_id}` | No | Upstream status and scrubbed body verbatim |

For status `429` not matching `QuotaExhausted`: if `Retry-After <= 2 s`, sleep `Retry-After` once (counted against `failover_wall_clock_budget_ms`) and retry the **same** key to preserve prompt cache; otherwise set `route_health` or `key.health` to `cooling_down` (in memory only) for `Retry-After` (or `15 s..300 s` backoff) and fail over. For status `529`, set `route_health = cooling_down` and fail over. For status `500`, set `route_health = cooling_down` (`5 s..60 s`) and fail over without counting a host failure on `OriginBreaker`. For status `502`, `503`, `504`, count a host failure on `OriginBreaker`, set `key.health = cooling_down`, and fail over.

`RuntimeState` is authoritative for health while running. Transitions to `exhausted`, `invalid`, or `healthy` increment `health_version` and enqueue a durable `HealthWrite` guarded by `WHERE id = ? AND health_version < ?`. Stale data-plane writes therefore never overwrite a control-plane `reset-health`.

#### 10.5.1 Per-origin circuit breaker state machine (`OriginBreaker`)

Each upstream origin `OriginKey { scheme, host, port, allow_private_network }` maintains an `OriginBreaker` in `RuntimeState` with 6 rolling 10-second buckets (`attempts: u32, host_failures: u32`), `consecutive_host_failures: u32`, and `open_streak: u32`. Only `TransportError::{Dns, Connect, Tls, WriteStalled}` and pre-first-frame HTTP `502`, `503`, and `504` count as host failures.

| Current state | Event or condition | Next state | Action taken |
| --- | --- | --- | --- |
| `Closed` | Host failure where `consecutive_host_failures + 1 >= 5` OR (`window_attempts >= 20` and `window_host_failures / window_attempts >= 0.5`) | `Open` | Set `until_ms = now_ms + min(120_000, 15_000 * 2^open_streak)` |
| `Closed` | Host failure below both thresholds | `Closed` | Increment `consecutive_host_failures` and current 10 s bucket counters |
| `Closed` | Any upstream HTTP response received (any HTTP status other than 502/503/504) | `Closed` | Set `consecutive_host_failures = 0`; increment bucket `attempts` |
| `Open` | Request evaluates `C7` while `now_ms < until_ms` | `Open` | Route fails `C7` with zero network I/O |
| `Open` | Request evaluates `C7` when `now_ms >= until_ms` and CAS acquires `probe_in_flight` (`ProbeSlotGuard`) | `HalfOpen` | Allow this single request through as the origin probe |
| `HalfOpen` | Probe receives any HTTP response (any status except 502/503/504) | `Closed` | Set `open_streak = 0`, `consecutive_host_failures = 0`, clear 10 s buckets, release probe slot |
| `HalfOpen` | Probe fails with a host failure (`Dns`, `Connect`, `Tls`, `WriteStalled`, or 502/503/504) | `Open` | Increment `open_streak`, set `until_ms = now_ms + min(120_000, 15_000 * 2^open_streak)`, release probe slot |
| `HalfOpen` | Probe cancelled by client disconnect before outcome (`ProbeSlotGuard::drop`) | `HalfOpen` | Release `probe_in_flight = false` so the next request can probe |

#### 10.5.2 Normative timeout and failover eligibility tables

To prevent duplicate billing on slow prefill or non-streaming calls while keeping connection failures fast to recover, LLMRoller enforces seven distinct timers:

| Timer | Default | Scope | Behavior on expiry |
| --- | --- | --- | --- |
| `connect_timeout_ms` | `5,000` | TCP + TLS handshake per try | `TransportError::Connect` -> failover allowed (upstream never accepted the request). |
| `request_write_timeout_ms` | `30,000` | Writing request headers and body to upstream | `TransportError::WriteStalled` -> failover allowed only if zero response bytes have been received. |
| `first_activity_timeout_ms` (streaming) | `60,000` | From end of request write until the first response byte of any kind (status line, header, SSE comment, or `data:`) | Increment `llmroller_upstream_slow_total{provider_id}`; if provider `timeouts.timeout_failover == true`, fail over (charging `estimated_input` in `usage_attempts`); else return HTTP 504 `upstream_timeout`. |
| `first_valid_frame_timeout_ms` (streaming) | `180,000` | From end of request write until the first valid non-comment frame (SSE comments `: ...` and `event: ping` keep `stream_idle_timeout_ms` alive) | Return HTTP 504 `upstream_timeout`; **never** fail over (upstream accepted and is computing). |
| `stream_idle_timeout_ms` | `300,000` | After first valid frame, maximum gap between any two received bytes | Close stream with `Termination::IdleTimeout` (`incomplete = 1`). |
| `nonstream_response_timeout_ms` | `600,000` | Non-streaming (`stream: false`): from end of request write until full JSON body received | Return HTTP 504 `upstream_timeout`; **never** fail over. |
| `failover_wall_clock_budget_ms` | `60,000` | Cumulative wall-clock time spent across tries that ended with a failover-eligible outcome | Stop failing over and return the terminal error from Section 10.5.3. |

#### 10.5.3 Pre-first-frame holding buffer and terminal error selection

Until the first valid non-comment frame is verified, the forwarder holds the upstream status, headers, and initial bytes in a `64 KiB` buffer. If the buffer reaches `64 KiB` without encountering an error object, the response commits immediately (status and filtered headers sent, held bytes flushed).

When no try commits, LLMRoller selects the client-facing terminal error from the ordered `outcomes` of the tries:
1. If any try ended with `RequestRejected`, `Unclassified403`, or a non-retryable 4xx passthrough, return that upstream response verbatim.
2. Else if all tries ended in `{http_429, QuotaExhausted, PlanRestricted, breaker_open}`, return HTTP 429 (`upstream_quota_exhausted` or `upstream_rate_limited`) with `Retry-After` floored at `1 s`.
3. Else if any try ended in `wait_timeout`, return HTTP 504 (`code = "upstream_timeout"`).
4. Otherwise return HTTP 502 (`code = "upstream_unreachable"`).

### 10.6 Deterministic canonical model ID normalization, discovery lifecycle, and merges

The pure function `crates/core::catalog::normalize_model_id(raw_id: &str) -> Result<String, NormalizeError>` normalizes raw upstream model IDs without ever merging distinct version numbers:

```text
fn normalize_model_id(raw: &str) -> Result<String, NormalizeError> {
    let s = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    if s.is_empty() || s.len() > 256 || !s.bytes().all(|b| (0x21..=0x7E).contains(&b) || b == b' ') {
        return Err(NormalizeError::InvalidCharacters);
    }
    let mut s = s.to_ascii_lowercase();
    if let Some(idx) = s.rfind('/') {
        s = s[idx + 1..].to_string();
    }
    if let Some(idx) = s.rfind(':') {
        let (base, tag) = (&s[..idx], &s[idx + 1..]);
        s = match tag {
            "free" => format!("{base}-free"),
            "thinking" => format!("{base}-thinking"),
            "nitro" | "floor" => base.to_string(),
            "extended" => format!("{base}-extended"),
            "beta" => format!("{base}-beta"),
            other => format!("{base}-{other}"),
        };
    }
    let replaced: String = s.chars().map(|c| if c == '_' || c == ' ' { '-' } else { c }).collect();
    // Preserve '.' iff immediately preceded and followed by an ASCII digit; otherwise replace '.' with '-'.
    let bytes = replaced.as_bytes();
    let mut dotted = String::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'.' {
            let prev_digit = i > 0 && bytes[i - 1].is_ascii_digit();
            let next_digit = i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit();
            dotted.push(if prev_digit && next_digit { '.' } else { '-' });
        } else {
            dotted.push(b as char);
        }
    }
    // Collapse consecutive '-' and trim leading/trailing '-'.
    let collapsed = collapse_hyphens(&dotted);
    Ok(collapsed)
}
```

When one key's `/models` list has multiple raw IDs with the same normalized slug, discovery runs `normalize_keep_vendor(raw)` (replacing `/` with `--`) and logs a warning. Discovery then applies confirmed `model_aliases` (`alias -> model_pk`) so aliased dated IDs attach their routes directly to the target `model_pk`.

| Raw upstream `id` from provider `/models` | Provider kind | Normalized `canonical_id` (`MDL-2`) | Preserved `upstream_model_id` (`model_routes`) |
| --- | --- | --- | --- |
| `deepseek-v4.1-flash` | `open_design_amr` | `deepseek-v4.1-flash` | `deepseek-v4.1-flash` |
| `deepseek/deepseek-v4.1-flash` | `command_code` | `deepseek-v4.1-flash` | `deepseek/deepseek-v4.1-flash` |
| `deepseek-v4-flash` | `opencode_go` | `deepseek-v4-flash` (kept distinct per `ADR-7`) | `deepseek-v4-flash` |
| `anthropic/claude-sonnet-4.6` | `openrouter` | `claude-sonnet-4.6` (preset alias `claude-sonnet-4-6` -> `claude-sonnet-4.6`) | `anthropic/claude-sonnet-4.6` |
| `claude-sonnet-4-6-20260514` | `command_code` | Attaches to `claude-sonnet-4.6` via preset alias | `claude-sonnet-4-6-20260514` |
| `openai/gpt-5.4-pro:free` | `openrouter` | `gpt-5.4-pro-free` | `openai/gpt-5.4-pro:free` |
| `mimo/mimo_v2.6_pro` | `command_code` | `mimo-v2.6-pro` | `mimo/mimo_v2.6_pro` |
| `qwen/qwen2.5-coder-32b-instruct` | `openrouter` | `qwen2.5-coder-32b-instruct` | `qwen/qwen2.5-coder-32b-instruct` |

When an admin merges model `a` into `b` (`POST /admin/api/models/merge`), the writer runs one transaction over `model_pk`. It repoints `model_routes`, `user_route_preferences`, `budget_windows`, `pins`, `response_links`, and `model_aliases`, deletes `a` from `models`, and inserts `model_redirects(a.canonical_id -> b.model_pk)`.

## 11. Session identification and hints

Coding agents and upstream providers use different session headers and body fields. LLMRoller therefore separates inbound session extraction and response-link inheritance from outbound affinity hint injection.

### 11.1 Eight-step inbound session ID resolution order

The session resolver checks each inbound request using the eight-step precedence order below (after evaluating Response-link session inheritance in Section 11.1.2). Note that `previous_response_id` is **not** a session-ID precedence step because its value changes on every turn of a stateful Responses conversation; instead, `previous_response_id` inherits the parent turn's `session_hash` via `response_links` (Section 11.1.2). For steps 1 through 7, the resolver ignores empty values, values over 256 bytes, or non-printable ASCII bytes, falling through to the next step.

| Precedence | Source location | Header or JSON field name | Target client or scenario |
| --- | --- | --- | --- |
| 1 | HTTP header | `x-llmroller-session-id` | Explicit override for custom scripts, `/v1/usage` queries, and tests |
| 2 | HTTP header | `x-claude-code-session-id` | [Claude Code](https://www.google.com/search?q=https%3A%2F%2Fcode.claude.com%2Fdocs%2Fen%2Fllm-gateway). Note: `x-claude-code-agent-id` and `x-claude-code-parent-agent-id` are ignored for pin lookup so subagents share the parent conversation's pin and prompt cache. |
| 3 | HTTP header | `x-parent-session-id` | [opencode](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2Fanomalyco%2Fopencode%2Fissues%2F55243) child or forked sessions, keeping child turns on the parent's warm cache |
| 4 | HTTP header | `x-opencode-session` | Clients already configured with OpenCode Go's session header |
| 5 | HTTP header | `x-session-affinity` | [opencode](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2Fanomalyco%2Fopencode%2Fpull%2F35982), Fireworks, and Vercel AI Gateway clients |
| 6 | HTTP header | `x-session-id`, `session-id`, `session_id`, `conversation-id`, `x-bf-session-id` | [Codex CLI](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2Fopenai%2Fcodex%2Fissues%2F24973), [OpenRouter](https://www.google.com/search?q=https%3A%2F%2Fopenrouter.ai%2Fdocs%2Fguides%2Fbest-practices%2Fprompt-caching), [Bifrost](https://www.google.com/search?q=https%3A%2F%2Fdocs.getbifrost.ai%2Fproviders%2Fsession-affinity.md), and generic gateways |
| 7 | JSON body | `session_id`, `prompt_cache_key`, `promptCacheKey`, or `metadata.session_id` | OpenAI, OpenRouter, and Claude Code metadata hints |
| 8 | Derived fallback | `"fp:" \|\| hex(SHA-256(fp_input))[0..32]` computed over canonical byte-sliced `SYS[0..min(len, 4096)]` and `USER1[0..min(len, 4096)]` (Section 11.1.3) | Stateless SDK callers that send no session header or body field (or keys with `constant_session_mode = 'fingerprint_generic_ids'` on steps 4..7) |

#### 11.1.1 Privacy hashing and 21-slot exact constant-session detector

LLMRoller never writes the raw inbound session ID to SQLite. It computes a tenant-scoped `session_hash = hex_lower(HMAC-SHA256(session_hmac_secret, owner_user_id || " " || raw_session_id)[0..16])` (128 bits, 32 hex characters) using the persistent `session_hmac_secret` from `instance_secrets` so two different users never collide on `session_hash`.

A misconfigured client that sends a static constant session ID across all conversations defeats load balancing ([vscode#334186](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2Fmicrosoft%2Fvscode%2Fissues%2F334186)). Claude Code subagents (`x-claude-code-agent-id`) and opencode child sessions (Step 3) intentionally share a parent session ID across distinct opening prompts. `ConstantSessionDetector` therefore evaluates only generic session sources (Steps 4 through 7) when `x-claude-code-agent-id` is absent.

The detector is keyed by `(ClientKeyId, SessionHash)` independent of model (`DashMap<(ClientKeyId, SessionHash), DetectorEntry>`, swept after 60 minutes of inactivity). Each `DetectorEntry` (`~262 bytes`) holds an exact 21-slot array of `OpeningSlot { fingerprint: u64, seen_minute: u32 }` where `fingerprint` is the first 8 bytes of `SHA-256(sys || 0x00 || usr)` (Section 11.1.3):
1. On each qualifying request, drop slots with `now_minute - seen_minute >= 60`.
2. If `fingerprint` is already present, update its `seen_minute` and return.
3. Insert `(fingerprint, now_minute)` into `slots`. When `len == 21` and `flagged_at_ms` is `None`, set `flagged_at_ms = Some(now_ms)`, increment `llmroller_constant_session_suspected_total{client_key_id}`, enqueue a system audit row (`session.constant_id_suspected`), and enqueue a durable update setting `client_keys.constant_session_flagged_at = now_ms`.
4. By default (`client_keys.constant_session_mode == 'flag'`), flagging surfaces a warning badge in the UI without altering routing. When `constant_session_mode = 'fingerprint_generic_ids'`, Steps 4 through 7 switch to the Step 8 fingerprint while Steps 1 through 3 still use their native header.

#### 11.1.2 Response-link session inheritance (`previous_response_id`)

Stateful OpenAI Responses conversations are chained via `previous_response_id`. To keep a stateful conversation on one `session_hash` and one key, `resolve_session_and_constraint` runs before pin lookup:

```text
fn resolve_session_and_constraint(req, caller) -> Result<SessionResolution, ProxyError> {
    let raw_id_1_to_7 = extract_steps_1_to_7(req, caller.client_key.constant_session_mode);
    if req.protocol == Protocol::Responses {
        if let Some(prev_id) = req.body_previous_response_id_or_path_id() {
            if let Some(link) = link_cache.get(prev_id).or_else(|| read_pool.select_response_link(prev_id)) {
                if link.owner_user_id != caller.owner_user_id {
                    return Err(ProxyError::NotFound("response_not_found"));
                }
                return Ok(SessionResolution {
                    session_hash: link.session_hash, // Inherit parent session_hash, overriding raw_id_1_to_7
                    constraint: RoutingConstraint::HardKey(link.upstream_key_id),
                    source: SessionSource::ResponseLink,
                });
            } else if req.endpoint_kind == EndpointKind::StateBound {
                return Err(ProxyError::NotFound("response_not_found"));
            }
        }
    }
    let raw_id = raw_id_1_to_7.unwrap_or_else(|| step8_fingerprint(req, caller.client_key_id));
    Ok(SessionResolution {
        session_hash: hmac_session_hash(caller.owner_user_id, &raw_id),
        constraint: RoutingConstraint::Free,
        source: SessionSource::Normal,
    })
}
```

#### 11.1.3 Canonical byte-sliced Step 8 fallback fingerprint extraction

Step 8 extracts `SYS` and `USER1` into canonical `Vec<u8>` byte buffers so multi-byte UTF-8 characters, moving `cache_control` markers, and base64 images never break pinning:

| Protocol | `SYS` (concatenated in document order with `0x1E` separator) | `USER1` |
| --- | --- | --- |
| `chat` | `content` of every `messages[i]` with `role in {"system", "developer"}` appearing before the first `role == "user"` message | `content` of the first `messages[i]` with `role == "user"` |
| `messages` | Top-level `system` string or content-block array | `content` of the first `messages[i]` with `role == "user"` |
| `responses` | Top-level `instructions` plus `content` of every `input` item with `role in {"system", "developer"}` appearing before the first `role == "user"` item | If `input` is a string, that string; else `content` of the first item with `role == "user"` |

Content canonicalization `canon(content) -> Vec<u8>`:
1. If `content` is a JSON string, return its decoded UTF-8 bytes.
2. If `content` is an array of blocks, for each block in order (joined by `0x1E`): if `type in {"text", "input_text"}`, emit `b"T" || decoded_text_bytes`; otherwise emit `b"B" || type_bytes || 0x1F || SHA-256(canonical_json_without_cache_control_and_citations)`.
3. Truncate as raw byte slices (never `&str` slicing): `sys = &canon(SYS)[0..min(len, 4096)]` and `usr = &canon(USER1)[0..min(len, 4096)]`.
4. Compute `fp_input = "llmroller.fp.v1 " || u32_be(len(client_key_id)) || client_key_id || u32_be(len(canonical_id)) || canonical_id || u8(protocol_code) || u32_be(len(sys)) || sys || u32_be(len(usr)) || usr`, and return `raw_session_id = "fp:" || hex_lower(SHA-256(fp_input))[0..32]`.

### 11.2 Outbound provider session hint profiles and tier isolation

Each provider has a configurable `session_profile` in `providers.config_json` that tells the adapter which headers or body fields to write into the outbound request. Outbound session handling depends strictly on the authorization tier (`Own` vs. `Shared` / `Global`) of the chosen upstream key (`SEC-16`):

| Tier of chosen upstream key | Recognized inbound session carriers (headers in Precedence Steps 1..6 and body fields in Step 7) | Provider's configured `session_profile` affinity fields |
| --- | --- | --- |
| `Own` (caller's own personal key) | Forwarded verbatim | Written only if absent (`send_if_absent`), using the client's raw session ID (truncated at the largest UTF-8 character boundary `<= 256` bytes) or `llr_<26-char-base32>` when resolved via Step 8 |
| `Shared` or `Global` (pooled across users) | Every recognized client session header and body field is **removed** from the outbound request (headers stripped; body fields removed by span deletion), except `x-claude-code-agent-id` and `x-claude-code-parent-agent-id` | Always written (overwriting any existing value) with the tenant-salted token `llr_<base32(HMAC-SHA256(session_hmac_secret, "affinity.v1 " \|\| owner_user_id \|\| " " \|\| raw_session_id))[0..26]>` |

The table below lists the default session hint profile for each built-in provider preset.

| Provider preset | Outbound headers configured | Outbound body field configured | Special rules and verification source |
| --- | --- | --- | --- |
| `opencode_go` | `x-opencode-session: <affinity_id>` | None | Mandatory on [OpenCode Go](https://www.google.com/search?q=https%3A%2F%2Fopencode.ai%2Fdocs%2Fgo); prevents `400 MissingSessionID`. On `Own` keys, native `x-claude-code-session-id` and `session_id` pass through; on `Shared`/`Global` keys, they are stripped and replaced by the salted `x-opencode-session`. |
| `openrouter` | `x-session-id: <affinity_id>` | `session_id: "<affinity_id>"` (truncated to UTF-8 boundary `<= 256` bytes) | Enables sticky provider routing from turn 1 ([OpenRouter prompt caching](https://www.google.com/search?q=https%3A%2F%2Fopenrouter.ai%2Fdocs%2Fguides%2Fbest-practices%2Fprompt-caching)). Never injects `provider.order`. |
| `command_code` | `x-session-affinity: <affinity_id>`, `x-session-id: <affinity_id>` | `prompt_cache_key: "<affinity_id>"` on `chat` and `responses` | [Command Code](https://www.google.com/search?q=https%3A%2F%2Fcommandcode.ai%2Fdocs%2Fprovider) documents no custom header; forwarding standard headers and `prompt_cache_key` preserves upstream cache hints. |
| `open_design_amr` | `x-session-affinity: <affinity_id>`, `x-session-id: <affinity_id>` | `prompt_cache_key: "<affinity_id>"` on `chat` and `responses` | Same default as Command Code. |
| `custom` | Configurable header list (default `x-session-id`, `x-session-affinity`) | Configurable (`off`, `prompt_cache_key`, or `session_id`) | Allows operators and users to match Fireworks, DeepInfra, vLLM Router, or LiteLLM upstream requirements. |

## 12. Protocol handling and transparent proxying

LLMRoller acts as a transparent wire proxy for OpenAI Chat Completions (`POST /v1/chat/completions`), OpenAI Responses (`POST /v1/responses`), and Anthropic Messages (`POST /v1/messages`). It buffers the inbound request body once in a shared `Bytes` buffer to read `model` and support pre-first-byte failover. It edits only top-level routing and usage fields, and streams responses chunk-by-chunk after the first valid frame.

### 12.1 Request and response header policies

The proxy applies a deterministic four-rule request header pipeline to every outbound upstream request.

| Rule | Header names | Action taken |
| --- | --- | --- |
| 1. Strip hop-by-hop, org-scoping, and proxy internal | `connection` (plus any header named in `connection`), `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `te`, `trailer`, `transfer-encoding`, `upgrade`, `host`, `content-length`, `cookie`, `set-cookie`, `openai-organization`, `openai-project`, `forwarded`, `x-forwarded-for`, `x-forwarded-host`, `x-forwarded-proto`, `x-real-ip`, `x-llmroller-*`, plus (when routing to `Shared` or `Global` tiers) all recognized session headers in Precedence Steps 1..6 | Removed unconditionally before provider `add_headers` (which cannot override `host`, `authorization`, or `content-length`). Outbound `accept-encoding` is forced to `identity` on inference and `/usage` calls. |
| 2. Strip provider deny-list and token filters | Any header matching a glob in `provider.config.strip_headers`, plus individual comma-separated tokens listed in `provider.config.strip_header_tokens` (for example stripping an unsupported beta token from `anthropic-beta` and dropping the header if empty) | Removed when configured on that provider. |
| 3. Replace credentials | `authorization`, `x-api-key` | Stripped of the client's `llr_...` key and replaced with the decrypted upstream key in the provider's configured auth scheme (`Authorization: Bearer <secret>` or `x-api-key: <secret>`). |
| 4. Forward remaining headers | `user-agent`, `accept`, `content-type`, `anthropic-version`, `anthropic-beta`, `openai-beta`, `originator`, `x-claude-code-*` (or agent-only headers on `Shared`/`Global`), `x-stainless-*`, `x-cmd-zdr`, and any future agent headers | Forwarded unchanged in value; multiple values of the same header name preserve their relative order, and header names are sent in lowercase (RFC 9110 Section 5.1 / RFC 9113). The relative order of distinct header names is not preserved. |

#### 12.1.1 Response header policy (`R1` through `R5`)

When an upstream response commits at the first valid frame, LLMRoller filters response headers through five rules. These rules prevent `content-length` mismatches and strip upstream account identifiers on pooled keys (`SEC-18`):

| Step | Response header rule |
| --- | --- |
| `R1` | Remove all hop-by-hop headers: `connection`, `keep-alive`, `proxy-authenticate`, `proxy-connection`, `te`, `trailer`, `transfer-encoding`, `upgrade`, plus every header name listed inside a `connection` header value. |
| `R2` | Remove `content-length` and `content-encoding` unconditionally (LLMRoller re-frames responses; `hyper` sets chunked transfer encoding for streams and exact length for buffered non-streaming bodies). Remove `set-cookie` and `set-cookie2`. |
| `R3` | Retain only allow-listed headers: `content-type`, `cache-control`, `x-request-id`, `request-id`, `retry-after`, `openai-processing-ms`, `anthropic-version`, any additional lowercase header names in `provider.config.response_header_allow`, and (only when the caller is the personal key owner on tier `Own` or an `admin`) `x-ratelimit-*` and `anthropic-ratelimit-*`. |
| `R4` | When the chosen key is on tier `Shared` or `Global` (and the caller is not an `admin`), strip `openai-organization`, `openai-project`, `anthropic-organization-id`, every `x-ratelimit-*`, every `anthropic-ratelimit-*`, `cf-ray`, `server`, and `via` (`R4` overrides `response_header_allow`). `x-request-id` and `request-id` are kept for per-request supportability. |
| `R5` | Add `x-llmroller-request-id: <ULID>`. When the chosen key is on tier `Shared` or `Global`, add `x-llmroller-upstream-scope: shared` or `global` (never exposing the key label). |

### 12.2 User-Agent policy

Each provider configures `ua_policy` to one of three modes shown below.

| Mode | Policy behavior |
| --- | --- |
| `pass` (default for Command Code, Open Design AMR, OpenRouter) | Forwards the client's `User-Agent` header unchanged, or sends `llmroller/1.0` when the client sent no `User-Agent`. |
| `label` (default for OpenCode Go) | Forwards coding-agent `User-Agent` values (`claude-cli/*`, `codex-cli/*`, `opencode/*`, `hermes/*`) unchanged, and replaces generic HTTP library strings (`curl/*`, `python-requests/*`, `node-fetch/*`) with `llmroller/1.0` to satisfy [OpenCode Go's client identification guidelines](https://www.google.com/search?q=https%3A%2F%2Fopencode.ai%2Fdocs%2Fgo) without impersonating another vendor. |
| `reject` | Returns HTTP 400 if the client sends an empty or generic library `User-Agent`. |

### 12.3 Surgical JSON body validation and edits

LLM providers compute prompt cache hashes over the exact serialized sequence of `system`, `tools`, and `messages` (or `input`). Re-serializing the entire JSON payload through a DOM map would permute object keys and destroy prompt cache hits.

`crates/proto::body_edit::scan_top_level(bytes)` scans top-level fields in a single pass and enforces the normative validation and editing contract below before routing:

| Condition or operation | Normative behavior |
| --- | --- |
| Body is not valid UTF-8, begins with a UTF-8 BOM (`EF BB BF`), top-level value is not a JSON object (`{...}`), or non-whitespace bytes follow the closing `}` | Reject immediately with HTTP 400 `invalid_request_error`. |
| Duplicate top-level JSON key (after JSON string unescaping of object member names) | Reject immediately with HTTP 400 `invalid_request_error` (`"duplicate top-level JSON key: <key>"`, key truncated to 64 bytes with control characters stripped) so parser differentials cannot bypass `allowed_models`, pricing, or `stream` detection. |
| Type validation of top-level routing fields | `model` must be a JSON string; `stream` (when present) must be a JSON boolean; `previous_response_id` (when present) must be a JSON string; otherwise return HTTP 400 `invalid_request_error`. |
| String escaping on insertion or replacement | Every inserted or replaced string value (`upstream_model_id` or session affinity token) is serialized via `serde_json::to_writer` as a JSON string so quotes or backslashes can never break JSON syntax or inject keys. |
| Session string truncation | Truncated to the largest UTF-8 character boundary at or below `session_profile.max_len` (`256` bytes, computed via `s.char_indices()`), then JSON-encoded. |
| Member insertion position | New top-level members are inserted immediately after the opening `{` as `"key":value,` (or `"key":value` if the object was `{}`), avoiding trailing-comma edge cases. |
| Session body-field handling by tier | On `Own` tier, insert the configured session body field only if absent. On `Shared` and `Global` tiers, delete any client-supplied spans for `session_id`, `prompt_cache_key`, `promptCacheKey`, and `metadata.session_id`, and write the provider's configured session body field with the tenant-salted token. |

For streaming `chat` requests (`stream: true`), `usage_reporting.chat_stream` (`inject`, `native`, or `none`) governs `stream_options.include_usage` editing:

| Client request body state | Action when `chat_stream == "inject"` | `strip_injected_usage_chunk` |
| --- | --- | --- |
| `stream` is absent or `false`, or `chat_stream != "inject"` | None | `false` |
| No `stream_options` field present | Insert `"stream_options":{"include_usage":true}` after `{` | `true` |
| `stream_options` value is `null` | Replace the `null` span with `{"include_usage":true}` | `true` |
| `stream_options` is an object without `include_usage` | Insert `"include_usage":true` as the first member of `stream_options` | `true` |
| `stream_options.include_usage` is `false` | Replace the `false` span with `true` (never creating a duplicate key) | `true` |
| `stream_options.include_usage` is already `true` | None | `false` |

When the chosen key is in tier `Own` and no edit applies, LLMRoller forwards the original `bytes::Bytes` buffer with identical pointer identity (`TRN-1`).

### 12.4 Pre-first-frame holding, SSE streaming tee, and `RequestLifecycle` guard

Outbound `accept-encoding` is forced to `identity`. If an upstream still returns `gzip`, `deflate`, `br`, or `zstd`, the forwarder decodes the stream via `async-compression` and strips `content-encoding`. Any other encoding before the first byte triggers a failover-eligible `TransportError::UnsupportedEncoding`.

#### 12.4.1 Frame filter, inline `SseUsageTap`, and bounded `TraceTap` execution model

LLMRoller decouples the usage tap, the trace tap, and the client stream so chunk forwarding never blocks and usage frames are never lost:

| Component | Runtime context | Input and memory bound | Backpressure / overflow behavior |
| --- | --- | --- | --- |
| Outbound client stream and usage-chunk filter (`crates/proxy::forward`) | Inline in the response body `Stream::poll_next` | When `strip_injected_usage_chunk == false`, forwards every `Bytes` chunk immediately after first-frame commit. When `strip_injected_usage_chunk == true`, buffers only the incomplete tail of the current SSE frame in `partial: BytesMut` (max `64 KiB`): each complete frame (`

` or `

`) whose single `data:` JSON has `"choices":[]` and an object `"usage"` is fed to `SseUsageTap` and suppressed from the client; all other complete frames (and any partial frame exceeding `64 KiB` or remaining at stream end) are flushed immediately. | Forwarding delay is bounded by single-frame completion (`<= 1 ms` per complete frame, `PERF-2`). |
| `SseUsageTap` (`crates/proto::sse_tap`) | Inline synchronous call inside `poll_next` | Maintains a 3-byte `utf8_carryover` and a `64 KiB` line accumulator; JSON-parses **only** frames whose event or data can carry usage or response IDs: `chat` frames containing `"usage"`, `responses` events `response.created`, `response.completed`, `response.incomplete`, `response.failed`, and `messages` events `message_start`, `message_delta`, `message_stop` (plus accumulating decoded output text byte counts for abort estimation). | Zero channel queue; work per frame is `O(frame_len)` capped at `64 KiB`, so usage frames can never be dropped by channel backpressure. |
| `TraceTap` collector (`crates/proto::trace_tap`) | Dedicated async task per traced request | Receives `Bytes::clone` chunks via `tokio::sync::mpsc::channel(256)` using non-blocking `try_send`, bounded by a global `trace_inflight_bytes` semaphore (`64 MiB`) and a per-stream `1 MiB` (`256 MiB` global) output accumulator. | On `try_send` failure or budget exhaustion, sets `trace_dropped = true`, stops sending chunks for that request, records `truncated_content = 1`, and increments `llmroller_trace_input_dropped_total`. Client streaming is never delayed. |
| Trace sealer pool (`crates/store::traces`) | Fixed pool of `trace_sealer_threads` (default `2`) OS threads (`std::thread::Builder`) | Fed by `crossbeam_channel::bounded::<SealJob>(64)` using `try_send`. Each thread owns one reusable `zstd::bulk::Compressor` (level 3, `~3.5 MiB` context per thread = `~7 MiB` total). | On channel `Full`, drops the turn's trace, sets `usage_events.trace_dropped = 1`, and increments `llmroller_trace_dropped_total{reason="sealer_backlog"}`. |

#### 12.4.2 Deterministic completion and abort guard (`RequestLifecycle`)

Every routed request is owned by a `RequestLifecycle` struct wrapped inside the outbound response stream. `RequestLifecycle::drop` runs synchronously once (`finalized` flag) with zero I/O across all five `Termination` modes (`Completed`, `UpstreamTruncated`, `ClientGone`, `IdleTimeout`, `ShutdownCut`):
1. Computes token counts and `cost_micro_usd` from `SseUsageTap` (using observed usage if received, or Section 13.1 abort estimation with `incomplete = 1` otherwise).
2. Posts the actual or estimated cost to window rings and monthly accumulators before subtracting `est` from `reserved`.
3. Releases the `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` permit (if not already released at first valid frame) and any `ProbeSlotGuard`.
4. Enqueues the `UsageEvent` (with `termination` and `incomplete`), `usage_attempts` rows, and 5-minute/hourly rollup deltas, and finalizes any active `TraceTap` collector as truncated.

### 12.5 Token and cost extraction across protocols

`SseUsageTap` and `JsonUsageExtractor` normalize token counts across all three wire protocols into five mutually exclusive non-negative integer fields using the normative formulas below.

| Normalized field | OpenAI `chat` formula | OpenAI `responses` formula | Anthropic `messages` formula |
| --- | --- | --- | --- |
| `cache_read_tokens` | `coalesce(usage.prompt_tokens_details.cached_tokens, usage.prompt_cache_hit_tokens, 0)` | `coalesce(usage.input_tokens_details.cached_tokens, 0)` | Last observed `cache_read_input_tokens` across `message_start` and `message_delta` (default `0`) |
| `cache_write_tokens` | `coalesce(usage.prompt_tokens_details.cache_write_tokens, 0)` | `coalesce(usage.input_tokens_details.cache_write_tokens, 0)` | Last observed `cache_creation_input_tokens` across `message_start` and `message_delta` (default `0`) |
| `input_uncached_tokens` | If `usage.prompt_cache_miss_tokens` is present: that value; else `max(0, usage.prompt_tokens - cache_read_tokens - cache_write_tokens)` | `max(0, usage.input_tokens - cache_read_tokens - cache_write_tokens)` | Last observed `input_tokens` across `message_start` and `message_delta` (already excludes cache reads and writes) |
| `output_tokens` | `usage.completion_tokens` (includes reasoning) | `usage.output_tokens` (includes reasoning) | Last observed `output_tokens` across `message_start` and `message_delta` |
| `reasoning_tokens` | `coalesce(usage.completion_tokens_details.reasoning_tokens, 0)` | `coalesce(usage.output_tokens_details.reasoning_tokens, 0)` | Upstream numeric reasoning token field when present, else `0` (included in `output_tokens`) |
| `provider_cost_micro_usd` | `(usage.cost * 1_000_000.0).round() as i64` (half away from zero) when `usage.cost` is a finite `f64` in `[0.0, 1000.0]`; out-of-range values are ignored and logged | Same validation and rounding rule | Same validation and rounding rule |

### 12.6 Content-addressed trace storage and Session Chat Preview reconstruction

When a request is traced, request blocks are extracted synchronously from the buffered request `Bytes` before the preflight permit is released. Each block is canonicalized (`cache_control` and `citations` removed; binary/base64 assets `> 1 KiB` replaced with SHA-256 placeholders) and hashed (`block_hash = hex_lower(SHA-256(canonical_block_bytes))`). New blocks not in the 100,000-entry in-memory block-hash LRU are sealed into `trace_blocks`, while `trace_records.payload_enc` seals `StoredTurnTrace` v2:

```json
{
  "v": 2,
  "request_skeleton": {
    "model": "claude-sonnet-4.6",
    "system": [{"$blk": "a1b2..."}],
    "tools": [{"$blk": "c3d4..."}],
    "messages": [{"$blk": "e5f6..."}]
  },
  "block_refs": ["a1b2...", "c3d4...", "e5f6..."],
  "response": {
    "assistant_reasoning": "Let us run cargo test -p llmroller-core first.",
    "assistant_tool_calls": [
      {"id": "toolu_01A", "name": "bash", "arguments_json": "{"cmd":"cargo test -p llmroller-core"}"}
    ],
    "assistant_text": ""
  },
  "response_raw": null,
  "truncation": null
}
```

Because each turn record is self-contained via `block_refs`, dropping, truncating, or purging an earlier turn never corrupts later turns. During read-time reconstruction (`crates/core::chat_preview`):
1. The handler queries `trace_records` ordered by `(ts ASC, request_id ASC)` using cursor pagination (`?after=<ts>:<request_id>&limit=50`). Each page query fetches `limit + 1` rows starting one row **before** the cursor (the predecessor turn on that `thread_id`) in a short read transaction.
2. For consecutive stateless turns on the same `thread_id`, Longest Common Prefix (`LCP`) is computed directly over the `block_refs` hash arrays **without decrypting** unchanged prefix blocks. Only the new suffix blocks `turn_k.message_block_refs[LCP..]` (capped at `8 MiB` of decrypted content per page; excess blocks render as `"load more"` placeholders fetched via `GET /api/user/traces/{request_id}`) are fetched from `trace_blocks` and decrypted.
3. If a referenced block is absent from `trace_blocks` (expired), it renders as a `"content expired"` placeholder card without affecting any other block or turn.

#### 12.6.1 Session Chat Preview JSON contract (`SessionChatPreviewResponse`)

Because one session can span multiple models and subagent threads, `canonical_model` and `protocol` live on each turn. Similarly, `system_instructions` and `tools` live per thread. The response also includes `current_pins` from `PinStore` so the viewer sees which provider and upstream key (`project_key`-redacted) the session is pinned to right now.

```json
{
  "session_hash": "9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c",
  "owner_user_id": "01J9X0USER...",
  "client_key_id": "01J9X0CLK...",
  "client_key_prefix": "llr_7f9a",
  "client_key_label": "laptop-claude-code",
  "current_pins": [
    {
      "canonical_model": "claude-sonnet-4.6",
      "pin_state": "confirmed",
      "pool_tier": "own",
      "provider_id": "01J9X1PROV...",
      "provider_name": "my-opencode-go",
      "provider_kind": "opencode_go",
      "provider_scope": "personal",
      "upstream_key_id": "01J9X1B...",
      "upstream_key_label": "my-opencode-go-1",
      "pinned_since_ms": 1792401000000,
      "last_used_ms": 1792401050000,
      "expires_ms": 1792487450000,
      "repin_count": 0
    }
  ],
  "total_turns": 2,
  "pagination": {
    "after": null,
    "limit": 50,
    "next_after": null
  },
  "threads": {
    "main": {
      "system_instructions": "You are an expert Rust systems engineer.",
      "tools": [
        { "name": "bash", "description": "Run a shell command", "parameters": { "type": "object" } }
      ]
    }
  },
  "timeline": [
    {
      "ordinal": 1,
      "request_id": "01J9X5REQ1...",
      "ts": 1792401000000,
      "thread_id": "main",
      "pin_outcome": "new_pin",
      "canonical_model": "claude-sonnet-4.6",
      "protocol": "messages",
      "upstream_key_id": "01J9X1B...",
      "upstream_key_label": "my-opencode-go-1",
      "provider_scope": "personal",
      "system_changed": false,
      "new_system_instructions": null,
      "context_compacted_or_branched": false,
      "truncated_content": false,
      "new_input_messages": [
        { "role": "user", "content": "Check why cargo test fails in crates/core." }
      ],
      "assistant_reasoning": "Let us run cargo test -p llmroller-core first.",
      "assistant_tool_calls": [
        { "id": "toolu_01A", "name": "bash", "arguments_json": "{"cmd":"cargo test -p llmroller-core"}" }
      ],
      "assistant_text": "",
      "metrics": {
        "status": 200,
        "ttfb_ms": 410,
        "latency_ms": 1250,
        "input_uncached_tokens": 1850,
        "cache_read_tokens": 0,
        "cache_write_tokens": 1500,
        "output_tokens": 95,
        "reasoning_tokens": 42,
        "cost_micro_usd": 7820
      }
    }
  ]
}
```
#### 12.6.2 Recently active sessions per client key (`ClientKeySessionsResponse`)

Operators and standard users often start troubleshooting from an LLMRoller client key (`llr_...`). They need to see which sessions recently used that key and which provider and key each session is pinned to right now. They also need to see which sessions have logged requests or recorded traces (`TRC-9`).

`GET /api/user/client-keys/{id}/sessions` (scoped to `actor.user_id`) and `GET /admin/api/client-keys/{id}/sessions` run a paged read query (`<= 50` sessions per read transaction, default `since_ms = now_ms - 86_400_000`) against `usage_events` (`idx_usage_client_key_session_ts`) and `trace_records` (`idx_trace_client_key_session`). For each discovered `(session_hash, canonical_id)`, the handler looks up `PinKey = (client_key_id, canonical_id, session_hash)` in `PinStore` and merges any active `PinStore` entries for that `client_key_id`. Every upstream key in `current_pins` and `last_served_route` passes through `project_key(viewer, key)`. Standard users therefore see the provider name and `"global pool"` on global pins.

```json
{
  "client_key_id": "01J9X0CLK...",
  "client_key_prefix": "llr_7f9a",
  "client_key_label": "laptop-claude-code",
  "since_ms": 1792314650000,
  "pagination": {
    "after": null,
    "limit": 50,
    "next_after": null
  },
  "sessions": [
    {
      "session_hash": "9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c",
      "first_active_at": 1792401000000,
      "last_active_at": 1792401050000,
      "request_count": 14,
      "error_count": 0,
      "traced_turn_count": 14,
      "has_traces": true,
      "input_uncached_tokens": 24100,
      "cache_read_tokens": 182000,
      "cache_write_tokens": 15400,
      "output_tokens": 6200,
      "cost_micro_usd": 148900,
      "models_used": ["claude-sonnet-4.6"],
      "current_pins": [
        {
          "canonical_model": "claude-sonnet-4.6",
          "pin_state": "confirmed",
          "pool_tier": "own",
          "provider_id": "01J9X1PROV...",
          "provider_name": "my-opencode-go",
          "provider_kind": "opencode_go",
          "provider_scope": "personal",
          "upstream_key_id": "01J9X1B...",
          "upstream_key_label": "my-opencode-go-1",
          "pinned_since_ms": 1792401000000,
          "last_used_ms": 1792401050000,
          "expires_ms": 1792487450000,
          "repin_count": 0
        }
      ],
      "last_served_route": {
        "request_id": "01J9X5REQ14...",
        "ts": 1792401050000,
        "canonical_model": "claude-sonnet-4.6",
        "provider_id": "01J9X1PROV...",
        "provider_name": "my-opencode-go",
        "provider_kind": "opencode_go",
        "provider_scope": "personal",
        "upstream_key_id": "01J9X1B...",
        "upstream_key_label": "my-opencode-go-1",
        "pin_outcome": "followed"
      }
    }
  ]
}
```

## 13. Pricing, limits, and source provider usage engine

LLMRoller combines local micro-USD pricing and window budgets with single-flight `/usage` polling, time-bounded observation projections, and sanitized `/usage` pass-through.

### 13.1 Local price book, fallback rates, and token estimation

Every `(provider_id, provider_kind, upstream_model_id)` row in `prices` (`provider_id = ""` for preset/kind defaults, or a specific `provider.id` for custom/personal overrides; lookup checks `(provider.id, kind, model)` first, then `("", kind, model)`) sets micro-USD rates per 1,000,000 tokens. It supports two validated modifiers:
1. Context tiers (`tiers_json`) apply when `total_input_tokens = input_uncached + cache_read + cache_write` satisfies `total_input_tokens >= min_total_input_tokens` and (`max_total_input_tokens` is `null` or `total_input_tokens <= max_total_input_tokens`). Tiers for a price row must not overlap (validated on write with HTTP 422).
2. Time-of-day schedules (`schedule_json`) define half-open `[start, end)` UTC minute ranges (`0..1440`) and ISO weekdays evaluated at request start (`ts` of first byte sent upstream). Schedules for a price row must not overlap (HTTP 422).

Every `usage_events` row has a non-null `cost_micro_usd`:
1. If the upstream response includes a finite `usage.cost` in `[0.0, 1000.0]`, `cost_micro_usd = (usage.cost * 1_000_000.0).round() as i64` (half away from zero) and `cost_source = 'provider'`.
2. Otherwise LLMRoller uses the matching `prices` row (or `settings.unpriced_fallback_rates` with `cost_source = 'fallback_rate'`) and computes `cost_micro_usd` with `i128` ceiling division:

```text
cost_micro_usd = div_ceil(
    input_uncached_tokens * rate_in
  + cache_read_tokens     * rate_cache_read
  + cache_write_tokens    * rate_cache_write
  + output_tokens         * rate_out,
  1_000_000
)
```

#### 13.1.1 Binary-aware input token estimator and aborted output estimation

Pre-flight reservations and aborted-stream costing (`cost_source = 'estimated_incomplete'`) share `crates/core::pricing::estimate_input_tokens(body: &[u8]) -> i64` so base64 media are never miscounted as text:

```text
fn estimate_input_tokens(body: &[u8]) -> i64 {
    // Walk JSON string values without building a DOM:
    // If a string is the value of a key in {"data", "image_url", "file_data", "b64_json"}
    // (or "url" whose value starts with "data:") and its byte length > 1_024:
    //     binary_blocks += 1;
    // else:
    //     text_bytes += decoded_utf8_byte_len;
    div_ceil(text_bytes, 4) + binary_blocks * 1_600
}
```

For an aborted stream (`incomplete = 1`), observed token fields from `SseUsageTap` are kept. Missing input tokens use `estimate_input_tokens(body)` and missing output tokens use `div_ceil(decoded_text_bytes, 4)` over decoded text deltas. In `usage_attempts`, failed tries with outcome in `{'http_5xx', 'wait_timeout', 'error_frame'}` charge `estimated_input = div_ceil(estimate_input_tokens(body) * rate_in, 1_000_000)` against that key's windows and caps.

### 13.2 Generalized budget windows, decaying observations, and startup rebuild

Each `budget_windows` rule attaches either to an `upstream_key_id` (`scope_kind = 'upstream_key'`) or to a specific `(upstream_key_id, model_pk)` route (`scope_kind = 'route'`), and specifies one of four `window_kind` values:

| `window_kind` | `period_ms` / `anchor_at` | In-memory structure and reset semantics |
| --- | --- | --- |
| `rolling` | `period_ms` in `3_600_000..=2_678_400_000` (`1 h` to `31 d`) | Generalized ring buffer of at most 360 buckets: bucket width `b = max(60_000, div_ceil(period_ms, 360 * 60_000) * 60_000)` and bucket count `n = div_ceil(period_ms, b)` (`5 h` -> `300 * 1 min`, `7 d` -> `360 * 28 min`, `30 d` -> `360 * 120 min`). Local spend is the sliding sum of the trailing `n` buckets plus `manual_offset.value_at(now_ms)` (manual calibration deltas inserted into the ring bucket for `effective_at` so they age out naturally after `period_ms`). |
| `anchored_fixed` | `period_ms` (`1 h..31 d`) and `anchor_at` | Accumulator `used` plus `window_start_ms` and `resets_at_ms`; when `now_ms >= resets_at_ms`, advances `resets_at_ms += div_ceil(now_ms - resets_at_ms + 1, period_ms) * period_ms` and resets `used = 0`. Manual calibration deltas apply iff `window_instance_start == window_start_ms`. |
| `anchored_calendar_month` | `period_ms IS NULL`; `anchor_at` stores the billing anchor timestamp (UTC day-of-month `1..=31` and time-of-day) | Advances across calendar months with day-of-month clamping (`day = min(anchor_day, days_in_month(candidate_month))`, so a 31 January anchor resets on 28/29 February and 31 March). |
| `anchored_provider` | `period_ms` (`5 h`, `7 d`, or `30 d`) with `upstream_window_kind in {'5h','7d','1mo'}` | Used for provider windows that report an explicit `resets_at` timestamp (OpenCode Go and Command Code `5h`, `7d`, and `1mo` windows). Before the first upstream observation arrives, behaves as a sliding ring; when an observation provides a new `resets_at_ms`, sets `window_start_ms = resets_at_ms - period_ms` and sums local ring events with `ts >= window_start_ms`. |

#### 13.2.1 Time-bounded `UpstreamObservation` projection (`MAX_OBSERVATION_AGE_MS`)

Upstream `/usage` snapshots never overwrite the local ring and never insert rows into `budget_adjustments`. A key therefore never stays stuck at a stale upstream percentage when `/usage` polling fails. Instead, each window stores an optional `UpstreamObservation`:

```rust
pub struct UpstreamObservation {
    pub used_micro_usd: i64,       // From snapshot used_micro_usd, or round(used_pct / 100.0 * limit)
    pub observed_at_ms: i64,       // Poll start timestamp t0
    pub local_at_observation: i64, // Local ring + manual_offset captured at t0 when the poll was issued
    pub resets_at_ms: Option<i64>,
}
```

The effective usage of window `w` at `now_ms` is:

```text
const MAX_OBSERVATION_AGE_MS: i64 = 1_800_000; // 30 minutes

fn effective_used(w: &WindowState, now_ms: i64) -> i64 {
    let local = w.ring.sum_at(now_ms) + w.manual_offset.value_at(now_ms);
    match &w.observation {
        None => local,
        Some(o) => {
            let expiry = o.resets_at_ms.unwrap_or(i64::MAX).min(o.observed_at_ms + MAX_OBSERVATION_AGE_MS);
            if now_ms >= expiry {
                return local; // Observation aged out or window reset; local ring governs alone
            }
            let projected = o.used_micro_usd + (local - o.local_at_observation).max(0);
            local.max(projected)
        }
    }
}
```

#### 13.2.2 Per-candidate cache-aware reservation (`try_reserve`) and UTC month accumulators

During check `C8` (for both new selections and pinned turns), LLMRoller computes a per-candidate estimate `est` using `observed_cache_read_ratio_ewma` (`alpha = 0.1`, initial `0.0`):

```text
input_tokens_est   = estimate_input_tokens(body)
cache_ratio        = route.observed_cache_read_ratio_ewma
in_cost            = input_tokens_est * ((1.0 - cache_ratio) * rate_in + cache_ratio * rate_cache_read)
out_tokens_est     = min(coalesce(req.max_tokens, model.max_output_tokens, 8_192), 8_192)
out_cost           = out_tokens_est * rate_out
estimate_micro_usd = clamp(div_ceil(in_cost + out_cost, 1_000_000), 1_000, 2_000_000) // $0.001 .. $2.00
```

`try_reserve(counters, est)` visits the candidate's counters in fixed global order and runs `reserved.compare_exchange` checking `effective_used(now_ms) + reserved + est + reserve_amount <= limit`. Any failure rolls back the reservation on earlier counters. On failover to a new candidate, the previous reservation is dropped and a new reservation is acquired for the new candidate.

UTC monthly accumulators are keyed by `(UtcYearMonth_of_reservation, Id)`. A request straddling midnight at month end therefore releases its reservation and posts its cost to the month it started in. On the first 60 s sweep at least 1 hour after month `M + 1` begins, entries for months `< M + 1` with `reserved == 0` are pruned.

#### 13.2.3 Fast startup reconstruction from `usage_rollups_5min` (`< 2 s`)

Startup reconstruction never scans `usage_events`. Instead, `serve` scans `usage_rollups_5min` (`WHERE bucket_ms >= now_ms - max_window_period - 300_000`), places each bucket at `bucket_ms + 150_000`, and applies `budget_adjustments` and the latest `upstream_usage_snapshots` row.

### 13.3 Source provider `/usage` polling, single-flight concurrency, and reconciliation

The `/usage` poller enforces strict concurrency and rate bounds so client traffic or 429 bursts cannot flood upstream endpoints:
1. Per-key single-flight: each key has `poll_in_flight: AtomicBool`; concurrent triggers attach to the in-flight poll result.
2. Per-key minimum start interval: `30 s` between automatic poll starts and `10 s` between owner or admin manual refreshes.
3. Global concurrency cap: an 8-permit semaphore (`usage_poll_concurrency = 8`) with scheduled polls (`300 s` active, `3600 s` idle) jittered by `±20%`.
4. At poll start `t0`, the poller records `local_at_observation = w.ring.sum_at(t0) + w.manual_offset.value_at(t0)` for each window on the key. It then fetches `/usage` (`10 s` timeout), seals `raw_json_enc`, builds `sanitized_json`, and updates each matching `upstream_window_kind` window.
5. If a snapshot window reports `status != "ok"`, `used_percent >= (100 - w.reserve_pct)`, or `limit_remaining <= 0`, key health transitions to `exhausted` (`health_version += 1`) with `health_until = resets_at_ms` (if present and `> now_ms`) else `now_ms + 300_000`. When `now_ms >= health_until`, Check `C6` admits the key immediately and the observation has also expired (`now_ms >= resets_at_ms`), so a failing `/usage` poll never extends exclusion.

### 13.4 Source provider `/usage` pass-through API (`GET /v1/usage`, `/v1/key`, `/v1/usage/raw`)

Coding agents and IDE status bars query LLMRoller for quota status using their `llr_...` client key. To keep raw session IDs out of access logs (`SEC-9`), `/v1/usage` and `/v1/usage/raw` reject `?session_id=` query parameters with HTTP 400. They read the session ID only from request headers (`x-llmroller-session-id` or precedence rows 1 through 6).

| Endpoint and parameters | Returned payload and pass-through behavior |
| --- | --- |
| `GET /v1/usage` (optional `?model=<id>`, `?upstream_key_id=<id>`, `?fresh=0\|1`, plus session headers) | Validates that any requested `?upstream_key_id` is accessible to the caller (returning 404 otherwise). Honors `?fresh=1` (10 s min interval, 3 s wait timeout) **only** when the caller is the personal key owner (`Own` tier) or an `admin`; for `Shared` or `Global` callers, `?fresh=1` is ignored and the cached snapshot is returned with header `x-llmroller-usage-stale-ms`. Returns `client_key` usage, `pinned_route` (projected via `project_key`), `usage` window gauges, and `source_provider_usage` (decrypted `raw_json` for `Own` personal keys and admins; `sanitized_json` for `Shared` and `Global` keys). |
| `GET /v1/key`, `GET /v1/auth/key` | Returns an [OpenRouter-compatible](https://www.google.com/search?q=https%3A%2F%2Fopenrouter.ai%2Fdocs%2Fapi_reference%2Flimits) `{ "data": { "label", "usage", "usage_daily", "usage_weekly", "usage_monthly", "limit", "limit_remaining", "limit_reset", "is_free_tier", "llmroller": { ... } } }` payload. |
| `GET /v1/usage/raw` (optional `?upstream_key_id=<id>` plus session headers) | Always serves the latest **stored** snapshot from `upstream_usage_snapshots` (never triggering a live upstream fetch), returning its stored `http_status` and decrypted `raw_json` (for `Own` personal keys and admins) or `sanitized_json` (for `Shared` and `Global` keys). |

## 14. API and web UI specification

LLMRoller exposes three HTTP surfaces. The data plane (`/v1/*`) serves coding agents. The user control plane (`/app/*`, `/api/user/*`) serves personal resources, and the admin control plane (`/admin/*`, `/admin/api/*`) serves instance administration.

### 14.1 Data plane API (`/v1`)

All `/v1/*` routes except `/healthz` and `/readyz` authenticate using a client key (`llr_...`) passed in `Authorization: Bearer <key>` or `x-api-key: <key>`.

| Method and path | `EndpointKind` | Protocol or purpose | Behavior summary |
| --- | --- | --- | --- |
| `POST /v1/chat/completions` | `Inference` | OpenAI Chat Completions (`chat`) | Routes to an eligible upstream key supporting `chat`. Supports JSON and SSE (`stream: true`). |
| `POST /v1/responses` | `Inference` | OpenAI Responses (`responses`) | Routes to an eligible upstream key supporting `responses`. Inherits `session_hash` and sets `HardKey` when `previous_response_id` matches `response_links` (`RTE-12`). |
| `GET /v1/responses/{id}`, `DELETE /v1/responses/{id}`, `GET /v1/responses/{id}/input_items`, `POST /v1/responses/{id}/cancel` | `StateBound` | OpenAI Responses stateful operations | Looks up `response_links(id)` (returning local 404 `response_not_found` if absent or owned by another user), verifies `C1`..`C5`, and forwards to `link.upstream_key_id` with no failover (`HardKey`). |
| `POST /v1/responses/compact` | `Inference` | OpenAI Responses compaction | Routes via `HardKey` when `previous_response_id` is present, else via `PinnedPreferred` or `Free`. |
| `POST /v1/messages` | `Inference` | Anthropic Messages (`messages`) | Routes to an eligible upstream key supporting `messages`. Supports JSON and SSE (`stream: true`). |
| `POST /v1/messages/count_tokens` | `Auxiliary` | Anthropic token counting | Skips budget check `C8`; routes to the pinned or eligible `messages` key (or returns local 404 `not_found_error` if unsupported on all eligible keys). |
| `GET /v1/models` | Metadata | Model discovery | Returns canonical models passing `C1`..`C5` (plus confirmed aliases) in OpenAI list shape, or in Anthropic list shape when `anthropic-version` is sent.Produces metrics only (no `usage_events` row). |
| `GET /v1/usage` | Metadata | Unified and pass-through usage | Extracts session from headers only (rejects `?session_id=` with 400); returns client key usage, window gauges, and role-scoped `source_provider_usage` (`USG-7`). |
| `GET /v1/key`, `GET /v1/auth/key` | Metadata | OpenRouter-compatible key usage | Returns OpenRouter key usage format plus LLMRoller pool metadata (`USG-7`). |
| `GET /v1/usage/raw` | Metadata | Stored upstream `/usage` snapshot | Serves the latest stored `upstream_usage_snapshots` payload (`raw_json` for `Own`/admin; `sanitized_json` for `Shared`/`Global`). |
| `GET /healthz`, `GET /readyz` | Probe | Process liveness and readiness | `/healthz` returns 200 when alive; `/readyz` returns 200 when startup preload is complete, not draining, WAL `< 1 GiB`, `durable_state` not full, and not `SQLITE_FULL` (503 otherwise). |

#### 14.1.1 Client configuration examples

Standard coding agents connect to LLMRoller by setting their base URL and API key environment variables, following the patterns documented by [Claude Code](https://www.google.com/search?q=https%3A%2F%2Fcode.claude.com%2Fdocs%2Fen%2Fllm-gateway-connect) and [OpenCode](https://www.google.com/search?q=https%3A%2F%2Fopencode.ai%2Fdocs%2Fgo):

```bash
# 1. Claude Code (Anthropic Messages wire, base URL without /v1)
export ANTHROPIC_BASE_URL="https://llmroller.internal.example.com"
export ANTHROPIC_AUTH_TOKEN="llr_7f9a..."

# 2. Codex CLI (OpenAI Responses or Chat Completions wire, base URL with /v1)
# In ~/.codex/config.toml:
# base_url = "https://llmroller.internal.example.com/v1"
export OPENAI_API_KEY="llr_7f9a..."

# 3. opencode (@ai-sdk/openai-compatible provider)
# baseURL: "https://llmroller.internal.example.com/v1"
# apiKey:  "llr_7f9a..."
```

#### 14.1.2 Proxy error envelopes and status codes

Errors generated by LLMRoller itself use the caller's protocol envelope: `{"error":{"message": "...", "type": "...", "code": "..."}}` on OpenAI paths and `{"type":"error","error":{"type": "...", "message": "..."}}` on `/v1/messages`. Quota and monthly-cap exhaustions with a known reset time always return HTTP `429` with `Retry-After` so coding agents back off cleanly.

| HTTP status | Envelope `type` / `code` | Trigger condition | `Retry-After` header |
| --- | --- | --- | --- |
| 400 | `invalid_request_error` / `invalid_request` | Malformed JSON, non-UTF-8 or BOM prefix, non-object top level, trailing bytes, duplicate top-level JSON key, type-confused `model`/`stream`, `?session_id=` query parameter on `/v1/usage*`, invalid header bytes (`SEC-15`), or rejected `User-Agent` (`ua_policy = reject`). | Omitted |
| 401 | `authentication_error` / `invalid_client_key` | Missing, malformed, disabled, expired, or revoked client key (`G1`, `G2`), or disabled owner account (`G3`). | Omitted |
| 403 | `permission_error` / `permission_denied` | Client IP outside `client_key.allowed_cidrs` (`G4`), requested model excluded by `client_key.allowed_models` (`G6`), or `HardKey` (`previous_response_id`) references a key whose share was revoked or disabled. | Omitted |
| 404 | `not_found_error` / `model_not_found` or `response_not_found` | Unknown model (`model_not_found`), or unknown / cross-tenant `response_id` (`response_not_found`). | Omitted |
| 405 / 415 | `invalid_request_error` / `method_not_allowed` or `unsupported_media_type` | Unsupported HTTP method (`405`) or missing/non-JSON `Content-Type` on POST routes (`415`). | Omitted |
| 409 | `invalid_request_error` / `ambiguous_model_id` | Client requested a raw `upstream_model_id` that maps to more than one accessible canonical model. | Omitted |
| 413 | `invalid_request_error` / `request_too_large` | Request body exceeds `LLMROLLER_MAX_BODY_MB` (default 32 MiB). | Omitted |
| 429 | `rate_limit_error` / `rate_limit_exceeded`, `client_key_budget_exhausted`, `monthly_cap_exhausted`, `upstream_quota_exhausted`, or `upstream_rate_limited` | Client key RPM exceeded (`G5`), IP auth failure limit exceeded (`SEC-12`), client-key monthly budget exhausted (`G7`), all candidates excluded solely by share or user global monthly caps, `HardKey` target is cooling/exhausted/breaker-open, or all failover tries failed with 429 / `QuotaExhausted` / `PlanRestricted` / breaker open. | Seconds to next token, next UTC month, or earliest `health_until` / `Retry-After` (min `1 s`) |
| 502 | `server_error` (`api_error` on Anthropic) / `upstream_unreachable` | All allowed failover tries failed before the first valid frame with connect/TLS errors, 5xx, or `CredentialInvalid`. | Omitted |
| 503 | `server_error` (`overloaded_error` on Anthropic) / `no_eligible_route` or `gateway_busy` | Every candidate route for the model is cooling down, window-exhausted, or circuit-open (`no_eligible_route`), or the global `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` semaphore queue timed out after `2,000 ms` (`gateway_busy`). | Earliest positive recovery seconds (`1 s` for `gateway_busy`) |
| 504 | `server_error` (`api_error` on Anthropic) / `upstream_timeout` | `first_activity_timeout_ms` expired with `timeout_failover == false`, `first_valid_frame_timeout_ms` expired, or `nonstream_response_timeout_ms` expired. | Omitted |

### 14.2 Control plane JSON API (`/api/user` and `/admin/api`)

All control-plane endpoints authenticate via the `__Host-llmroller_session` cookie and validate the per-session random `x-csrf-token` on mutating methods (`POST /api/auth/login` validates `__Host-llmroller_login_csrf` against `x-csrf-token` plus `Origin`). Identifiers that may contain `/` (`upstream_model_id` and `alias`) travel in JSON bodies or query parameters so `%2F`-decoding proxies never break routing.

#### 14.2.1 Self-service user API (`/api/user/*` for `user` and `admin` roles)

Every `/api/user/*` endpoint scopes queries to `actor.user_id`, returns HTTP 404 on other users' resources (`USR-3`), and applies `project_key`.

| Resource group | Endpoints and methods |
| --- | --- |
| Authentication and profile | `POST /api/auth/login` (validates `__Host-llmroller_login_csrf` + `Origin`), `POST /api/auth/logout`, `GET /api/auth/me`, `POST /api/user/password`, `POST /api/user/totp/enroll`, `POST /api/user/totp/verify`, `POST /api/user/totp/disable`, `DELETE /api/user/web-sessions/{id_hash}` |
| Personal providers & pricing | `GET /api/user/providers`, `POST /api/user/providers` (`ownership_scope = 'personal'`), `PATCH /api/user/providers/{id}` (recomputes `share_config_fingerprint`), `DELETE /api/user/providers/{id}`, `PUT /api/user/providers/{id}/prices` (body carries `upstream_model_id`, rates, `tiers_json`, `schedule_json`) |
| Personal upstream keys | `GET /api/user/providers/{id}/keys`, `POST /api/user/providers/{id}/keys`, `PATCH /api/user/upstream-keys/{id}`, `DELETE /api/user/upstream-keys/{id}`, `POST /api/user/upstream-keys/{id}/test`, `POST /api/user/upstream-keys/{id}/reset-health` (bumps `health_version` in `RuntimeState` first), `PUT /api/user/upstream-keys/{id}/windows`, `POST /api/user/upstream-keys/{id}/calibrate` |
| Source `/usage` inspection | `GET /api/user/upstream-keys/{id}/usage` (returns latest snapshot + history, decrypted `raw_json` for own keys, `sanitized_json` for shared keys), `POST /api/user/upstream-keys/{id}/usage/refresh` (owner only, 10 s min interval) |
| Personal provider sharing | `GET /api/user/shares/outgoing`, `POST /api/user/shares` (returns 409 `share_overlap` on provider/key overlap or if `usage_url_cross_origin_ack = 1`), `PATCH /api/user/shares/{id}`, `DELETE /api/user/shares/{id}` (also deletes grantee `user_route_preferences`), `GET /api/user/shares/incoming`, `POST /api/user/shares/{id}/accept` (grantee only; body must echo `config_fingerprint`) |
| Personal route overrides | `GET /api/user/routes?model=`, `PUT /api/user/routes/{upstream_key_id}/{canonical_id}` (sets `user_route_preferences.priority_override` and `enabled` on own or accepted shared routes) |
| Client keys & recent sessions | `GET /api/user/client-keys`, `POST /api/user/client-keys` (returns raw `llr_...` once), `PATCH /api/user/client-keys/{id}` (including `routing_scope` and `constant_session_mode`), `POST /api/user/client-keys/{id}/rotate` (accepts `grace_period_secs` `0..=86400`, default `300`), `DELETE /api/user/client-keys/{id}`, `GET /api/user/client-keys/{id}/sessions?since_ms=&after=&limit=50` (lists recently active sessions from that client key with logged request stats, trace counts, and live `PinStore` provider and `project_key`-redacted upstream key bindings right now, `TRC-9`) |
| Active session pins | `GET /api/user/pins?client_key_id=` (returns live `PinStore` pins with provider name/kind and `project_key`-redacted upstream key), `DELETE /api/user/pins/{client_key_id}/{session_hash}/{canonical_id}`, `POST /api/user/pins/move` (validates `C1`..`C8` on target key before calling `PinStore::admin_move`) |
| Session chat preview (read-only) | `GET /api/user/sessions/{session_hash}/chat?after=<ts>:<request_id>&limit=50` (reconstructed timeline for caller's own session), `GET /api/user/traces/{request_id}` (single turn trace and reconstructed raw JSON) |
| Scoped usage analytics | `GET /api/user/usage/summary`, `GET /api/user/usage/timeseries`, `GET /api/user/usage/breakdown`, `GET /api/user/usage/events` (paged `<= 5,000` rows per read tx), `GET /api/user/usage/export.csv` |
| Scoped audit log | `GET /api/user/audit` (events where `actor_user_id == me` or target belongs to `me`) |

#### 14.2.2 Instance administration API (`/admin/api/*` for `admin` role only)

Every `/admin/api/*` endpoint rejects non-admin callers with HTTP 403.

| Resource group | Endpoints and methods |
| --- | --- |
| First-run setup (`users == 0`) | `GET /setup`, `GET /admin/api/setup/status`, `POST /admin/api/setup` (all three return HTTP 404 permanently once any user exists per `AUTH-3`) |
| User, key, and session management | `GET /admin/api/users`, `POST /admin/api/users`, `PATCH /admin/api/users/{id}` (enforces in-tx `last_admin` guard; disabling a user emits `CacheInvalidationEvent::OwnerDisabled`), `DELETE /admin/api/users/{id}?confirm=<username>` (enforces `last_admin` guard and records `deleted_users`), `POST /admin/api/users/{id}/reset-password`, `POST /admin/api/users/{id}/totp/reset`, `DELETE /admin/api/users/{id}/web-sessions`, `GET /admin/api/users/{id}/client-keys`, `POST /admin/api/users/{id}/client-keys`, `DELETE /admin/api/client-keys/{id}`, `GET /admin/api/client-keys/{id}/sessions?since_ms=&after=&limit=50` (lists recently active sessions for any client key with logged request stats, trace counts, and live pinned provider and upstream key right now, `TRC-9`) |
| Admin override on user resources | `GET /admin/api/users/{id}/providers`, `PATCH /admin/api/providers/{id}`, `DELETE /admin/api/providers/{id}`, `GET /admin/api/users/{id}/shares`, `DELETE /admin/api/shares/{id}`, `POST /admin/api/shares/{id}/approve` (approves `custom` provider share), `GET /admin/api/users/{id}/pins` (every action logs `target_type` and owner `user_id` in `audit_log`) |
| Global providers and keys | `GET /admin/api/providers`, `POST /admin/api/providers` (`ownership_scope = 'global'`), `POST /admin/api/providers/{id}/keys`, `PATCH /admin/api/upstream-keys/{id}`, `DELETE /admin/api/upstream-keys/{id}`, `POST /admin/api/upstream-keys/{id}/test`, `POST /admin/api/upstream-keys/{id}/reset-health`, `GET /admin/api/upstream-keys/{id}/usage`, `POST /admin/api/upstream-keys/{id}/usage/refresh`, `PUT /admin/api/upstream-keys/{id}/windows`, `POST /admin/api/upstream-keys/{id}/calibrate` |
| Model catalog and price book | `GET /admin/api/models` (includes `merge_state`, `origin`, and `routing_strategy` filters), `PATCH /admin/api/models/{canonical_id}` (`exposed`, `distribution`, `routing_strategy`), `POST /admin/api/models/{canonical_id}/confirm-merge`, `POST /admin/api/models/refresh`, `POST /admin/api/models/merge`, `POST /admin/api/models/split`, `PUT /admin/api/model-aliases` (body `{alias, canonical_id}`), `DELETE /admin/api/model-aliases?alias=<urlencoded>`, `GET /admin/api/prices`, `PUT /admin/api/prices` (body carries `{provider_id, provider_kind, upstream_model_id, ...}`), `POST /admin/api/prices/sync` |
| Global routing and pins | `GET /admin/api/routes`, `PATCH /admin/api/routes/{upstream_key_id}/{canonical_id}`, `GET /admin/api/pins`, `DELETE /admin/api/pins` (takes a filter or `confirm_all=true`), `POST /admin/api/pins/move` |
| Content traceability & chat preview | `GET /admin/api/settings/traceability`, `PUT /admin/api/settings/traceability` (lowering `retention_days` returns 409 with row count unless `"confirm_purge": true`), `GET /admin/api/sessions/{session_hash}/chat?after=<ts>:<request_id>&limit=50` (logs deduplicated `trace.viewed` audit row once per 30 m), `GET /admin/api/traces/{request_id}`, `DELETE /admin/api/sessions/{session_hash}/trace`, `DELETE /admin/api/traces` |
| Instance usage, settings, and audit | `GET /admin/api/usage/summary`, `GET /admin/api/usage/timeseries`, `GET /admin/api/usage/breakdown`, `GET /admin/api/audit`, `GET /admin/api/settings`, `PATCH /admin/api/settings` (validates against closed 14-key registry including `default_routing_strategy`) |

### 14.3 Web UI screens, CSP configuration, and metric definitions

The web interface is server-rendered with Askama templates and HTMX partial updates, using zero external CDN assets. `/static/app.js` sets `htmx.config.includeIndicatorStyles = false`, `htmx.config.allowEval = false`, and `htmx.config.allowScriptTags = false` before HTMX initializes; loading-spinner CSS ships inside `/static/app.css`, and templates never use `hx-on:*` or `js:` attributes so `script-src 'self'; style-src 'self'` holds without exceptions.

| Screen | Audience | Primary elements and actions |
| --- | --- | --- |
| Setup & Login | Public (unauthenticated) | First-admin setup form (with setup token) when zero users exist; username, password, and TOTP login form protected by `__Host-llmroller_login_csrf` thereafter. |
| Overview Dashboard | `user` (scoped) & `admin` (global + personal) | Range selector (24 h, 7 d, 30 d, custom); cards for total requests, tokens, micro-USD cost, cache hit rate, pin follow rate, and repin rate; projected disk usage meter (`A6`) and `"Unpriced models"` badge; stacked daily cost chart (`uPlot`); upstream key cards showing live health from `RuntimeState`, local window ring spend, and `UpstreamObservation` gauges (`5h`, `7d`, `1mo`) with reset countdowns. |
| Personal Providers & Keys | `user` & `admin` | Add personal provider from preset or custom URL; add personal upstream keys; Test button; Refresh `/usage` button; modal showing decrypted raw source provider `/usage` JSON; configure budget windows and manual calibration. |
| Sharing & Shared With Me | `user` & `admin` | **Outgoing Shares tab:** grant access to a personal provider or key (preventing overlapping provider/key shares), set allowed models and monthly USD cap, view per-grantee token and dollar consumption (including charged failed tries), revoke share. **Shared With Me tab:** inspect pending and accepted shares, view provider kind, `base_url` host, key label, and the explicit disclosure `"Requests you send through this share travel to the endpoint above and are visible to its operator"`, and click **Accept Share** (`POST /api/user/shares/{id}/accept`). |
| Routing Matrix | `user` (`Own` + `Shared` + aggregated `"Global pool"` via `project_key`) & `admin` (full catalog) | Matrix of canonical models (rows) by upstream keys (columns). For standard users, all global keys are aggregated into a single `"Global pool"` column showing the combined projected share without leaking global key labels or priorities. Displays protocol chips (`chat`, `responses`, `messages`), active `routing_strategy` badge (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, `power_of_two_choices`, showing `100% / N` equal shares when an unweighted strategy is active), inline editable per-model `priority_override` on `Own` and `Shared` routes, effective weight, and projected percentage share. |
| Client Keys | `user` (own) & `admin` (all) | Generate `llr_...` key (shown once in a confirmation modal), select `routing_scope` (`inherit`, `personal_first`, `blended`, `personal_only`, `own_only`, `global_only`), configure `constant_session_mode` (`flag` or `fingerprint_generic_ids`), restrict models, set RPM or monthly USD cap, rotate with configurable `grace_period_secs`, or revoke. Includes a **Recently Active Sessions** drawer per client key (`TRC-9`) showing each recent `session_hash`, which provider and upstream key (`project_key`-redacted) it is pinned to right now (or `"unpinned / expired"` with last-served route), logged request and traced-turn counts, **View Requests** filter link, and **Preview Chat** button. |
| Active Sessions | `user` (own) & `admin` (all) | Live table (10 s HTMX poll) of active pins in `PinStore` filterable by `client_key_id` (with a toggle to include recently active sessions from `usage_events` whose pins expired): session hash prefix, client key, model, pinned provider name/kind and upstream key (projected via `project_key`), tier badge (`Own`, `Shared`, `Global`, `Blended`), age, logged request count, `repin_count`, constant-session warning badge, **View Requests** link, and **Preview Chat** button. Supports Unpin and Move Pin. |
| Session Chat Preview (Read-Only) | `user` (own sessions) & `admin` (all traced sessions) | Read-only conversation replay paginated by `(ts, request_id)` cursor: header displays the originating `client_key_prefix`/`label` and live `current_pins` badges showing which provider and upstream key (`project_key`-redacted) the session is pinned to right now; timeline renders per-thread system instructions and tool schemas, user prompts, collapsible reasoning/thinking blocks, formatted tool calls and tool outputs (auto-escaped in `<pre>`), assistant Markdown sanitized via `pulldown-cmark` (raw HTML off) + `ammonia`, context-compaction dividers, per-turn telemetry chips (with `project_key` redaction), and a raw JSON inspector drawer. |
| Usage Explorer | `user` (own) & `admin` (all) | Filter by time range, user, client key, provider scope, upstream key, model, or `cost_source` (`provider`, `price_book`, `fallback_rate`, `estimated_incomplete`, `estimated_attempt`). Paged event table (`<= 5,000` rows per read tx) with `usage_attempts` breakdown and formula-sanitized CSV or JSON export. |
| Admin: Global Providers & Catalog | `admin` only | Manage global providers and keys, review `pending_review` cross-vendor model merges, force-expose or disable models, merge or split models over `model_pk`, edit aliases and prices, and approve `custom` provider shares. |
| Admin: Users, Settings & Audit | `admin` only | Create, disable, or delete user accounts (`?confirm=<username>`), inspect `deleted_users`, set per-user global pool access and monthly global caps, edit the 14 validated runtime settings (including `default_routing_strategy`), and inspect the hash-chained `audit_log`. |

## 15. Technology choices and workspace layout

LLMRoller is implemented in safe Rust (Rust 2024 edition) with a pinned stable toolchain and a small, audited dependency tree.

### 15.1 Core technology stack

Every dependency is chosen for predictable speed, memory safety, zero external runtime services, and supply-chain auditability.

| Layer | Selected crate or tool | Rationale | Rejected alternative |
| --- | --- | --- | --- |
| Language & toolchain | Rust (2024 edition, stable toolchain, `#![forbid(unsafe_code)]`) | Predictable tail latency on 2,000 concurrent SSE streams with no garbage-collector pauses and a single static binary. | Go (GC pauses under high stream counts) or Python (packaging and supply-chain exposure). |
| HTTP server | `axum 0.8`, `hyper 1.x`, `hyper-util`, `tower` on `tokio 1.x` | Standard async Rust HTTP stack with native streaming body support and composable middleware. | `actix-web` (separate actor runtime model). |
| HTTP client & SSRF guard | `reqwest 0.13` (`rustls-tls`, `stream`, `http2`, `default-features = false`) + `hickory-resolver` inside `GuardedConnector` | Connection pooling per upstream origin (`OriginKey`), custom `tower::Service<Uri>` socket connector validating both DNS-resolved and IP-literal socket addresses against special-purpose ranges (`SEC-7`), and unbuffered chunk streaming. | `ureq` (blocking I/O) or `reqwest::dns::Resolve` alone (which `hyper` skips when the URL host is an IP literal). |
| Database | SQLite in WAL mode via `sqlx 0.9` (`connect_options(role)`) | Zero-configuration single-file persistence with per-role pragma enforcement and migration-tested SQL queries. | PostgreSQL (needs an external database server for single-node installs). |
| Cryptography | `chacha20poly1305`, `hkdf`, `sha2`, `hmac`, `argon2`, `totp-rs`, `subtle`, `secrecy`, `zeroize` | Pure-Rust audited RustCrypto implementations for XChaCha20-Poly1305 AEAD with canonical length-prefixed AAD, Argon2id, HMAC-SHA256, and memory zeroization. | OpenSSL (`openssl-sys` C bindings complicate static musl builds). |
| In-memory state & caches | `arc-swap` (`ConfigSnapshot`), `dashmap` + `parking_lot` + `smallvec` (`PinStore`, `WrrState`, `OriginBreaker`, window rings, UTC month accumulators), `moka` (`response_links` cache), `indexmap` (`durable_state` queue) | Lock-free atomic snapshot reads on the hot path, `Clock`-driven deterministic pin expiry in `PinStore` (`max_active_pins = 250,000`), and `moka` read-through caching for `response_links`. | `moka` for session pins (whose internal `quanta` timer cannot be driven by `FakeClock` and whose TinyLFU eviction would silently drop confirmed pins). |
| Web UI & safe chat rendering | `askama`, `rust-embed`, `pulldown-cmark`, `ammonia`, vendored `htmx 2.x` and `uPlot` | Type-checked server templates and strict HTML/Markdown sanitization (`ammonia` with raw HTML disabled in `pulldown-cmark`) for the read-only Session Chat Preview with zero CDN requests. | React or Next.js SPA (adds a large `node_modules` supply chain). |
| Compression & sealer pool | `zstd` (`zstd::bulk::Compressor` on 2 OS threads via `crossbeam-channel`) + `async-compression` | Reusable bulk zstd contexts on a fixed 2-thread sealer pool (`~7 MiB` total RAM) for `trace_blocks` and `trace_records`, plus fallback `async-compression` decoders (`gzip`, `deflate`, `br`, `zstd`) only for upstreams that ignore `accept-encoding: identity`. | Per-stream `zstd::stream::Encoder` (which allocates `~3.5 MiB` per stream = `~7 GB` at 2,000 streams). |
| Observability | `tracing`, `tracing-subscriber` (JSON formatter), `metrics`, `metrics-exporter-prometheus` | Structured JSON request logs with `request_id` correlation, `audit_anchor` emission, and bounded-cardinality Prometheus metrics. | Heavy OpenTelemetry gRPC exporters in v1.0 core. |
| Workspace automation & testing | `xtask` (Rust workspace binary), `cargo-nextest`, `proptest`, `wiremock`, `insta`, `cargo-llvm-cov`, `cargo-mutants`, `cargo-fuzz`, `cargo-deny`, `cargo-audit`, `cargo-vet` | Pure-Rust `cargo xtask` commands (`traceability`, `doc-consistency`, `docs-bundle`) with zero Python supply-chain dependencies, plus property testing and automated mutation testing ([cargo-mutants](https://mutants.rs/)). | External Python or Node CI scripts. |

### 15.2 Cargo workspace layout and architectural boundaries

The repository is a seven-member Cargo workspace (`crates/{core,proto,store,proxy,admin,server}` plus `xtask`) that enforces dependency direction and keeps `crates/core` free of I/O.

```
llmroller/
  Cargo.toml                  # Workspace manifest, shared lints (#![forbid(unsafe_code)])
  Cargo.lock                  # Committed lockfile for reproducible builds
  Dockerfile                  # Multi-stage build (chef, builder, runtime-distroless, runtime-dev)
  .dockerignore               # Deterministic build context exclusion rules
  docker-compose.yml          # Production/staging and dev/test Compose orchestration
  .env.example                # Documented bootstrap environment variable template
  deny.toml                   # cargo-deny license, ban, and advisory rules
  .cargo/mutants.toml         # cargo-mutants configuration and timeout tuning
  deploy/
    Caddyfile                 # Streaming-safe TLS reverse proxy config (flush_interval -1)
    prometheus.yml            # Internal Prometheus scrape config for :9090/metrics
  docs/
    srdd/
      0. SRDD - Introduction.md .. 22. SRDD - Deployment and Containers.md
      deployment-and-containers.md # Alias copy of Section 22 container & deployment spec
      requirements.toml       # Authoritative requirement -> milestone & sub-phase registry
    bigbang/
      big-bang-phases-0.md .. big-bang-phases-7.md (including big-bang-phases-4b.md)
  presets/
    opencode_go.toml          # Built-in OpenCode Go preset (endpoints, aliases, protocol_defaults, /usage, prices)
    command_code.toml         # Built-in Command Code preset
    open_design_amr.toml      # Built-in Open Design AMR preset
    openrouter.toml           # Built-in OpenRouter preset
  fuzz/                       # cargo-fuzz targets for body_edit, sse_frame, sse_tap, trace_tap, headers, step8_fingerprint
  benches/                    # Criterion and load benchmarks (PERF-1..PERF-5 with log-normal TTFF stub)
  fixtures/
    crypto/
      aad_vectors.json        # Committed XChaCha20-Poly1305 canonical AAD test vectors
    sse/                      # Recorded SSE streams (chat, responses, messages, truncated)
    usage/                    # Recorded source provider /usage JSON responses
    harnesses/                # Recorded Claude Code, Codex CLI, and opencode request traces
  xtask/                      # Pure-Rust workspace automation binary:
                              #   cargo xtask traceability --milestone M<N>
                              #   cargo xtask doc-consistency
                              #   cargo xtask docs-bundle
  crates/
    core/                     # Pure domain logic (ZERO network or database dependencies):
                              #   models (model_pk), ActorScope, RBAC rules, G1..G7 & C1..C8,
                              #   pluggable RouteSelector (SWRR, NaiveRR, WeightedRR, Random,
                              #   WeightedRandom, P2C), PinStore state machine, OriginBreaker,
                              #   pricing & binary-aware estimator, WindowState & UpstreamObservation,
                              #   UTC month accumulators, synchronous Clock seam
    proto/                    # Wire protocols and provider adapters:
                              #   duplicate-rejecting surgical JSON span editor, request & response
                              #   header pipelines, inline SseUsageTap, bounded TraceTap,
                              #   ProviderAdapter trait, 5-class error classifier, /models & /usage parsers
    store/                    # SQLite persistence, sealer pool, and cryptography:
                              #   connect_options(role), 31-table migrations & triggers, dual-queue
                              #   batching writer (Control + durable_state, rollups, raw_events),
                              #   2-thread zstd::bulk::Compressor sealer pool, canonical AAD sealer,
                              #   resumable rotate-master-key, Argon2id, HMAC audit_log chain
    proxy/                    # Data plane HTTP service (:8080):
                              #   preflight body budget semaphore, GuardedConnector (socket SSRF filter),
                              #   64 KiB pre-first-frame hold buffer, RequestLifecycle RAII drop guard,
                              #   pre-first-byte failover loop, AsyncClock, /v1/usage pass-through
    admin/                    # Control plane HTTP service (127.0.0.1:8081):
                              #   /api/user/* and /admin/api/* handlers, project_key redaction,
                              #   CSRF & session middleware, Askama templates, embedded static assets
    server/                   # Binary entrypoint and background supervisor:
                              #   CLI subcommands (exit codes 0..14), ordered startup & 8-step SIGTERM drain,
                              #   Reloader task, supervised discovery, single-flight /usage poller,
                              #   30s WAL checkpointer and retention sweeper
```

`crates/core` depends only on pure-Rust in-memory crates (`serde`, `serde_json`, `time`, `ulid`, `sha2`, `hmac`, `subtle`, `secrecy`, `zeroize`, `ipnet`, `arc-swap`, `dashmap`, `parking_lot`, `smallvec`, `rand`, `rand_chacha`). Its `Clock` seam (`fn now_ms(&self) -> i64`, `fn monotonic_ms(&self) -> i64`) is strictly synchronous; `crates/proxy::AsyncClock: Clock` extends it with `async fn sleep(&self, d: Duration)` outside `crates/core`. It cannot import `tokio`, `reqwest`, `hyper`, `axum`, or `sqlx`. This boundary guarantees that every routing, sharing, eligibility, pinning, breaker, and budget rule can be unit-tested and mutation-tested in milliseconds without sockets or disk files.

## 16. Testing plan and TDD governance

Testing in LLMRoller is a primary architectural constraint, never an afterthought written after code lands. Every subsystem is built under strict Test-Driven Development (TDD) with bidirectional traceability between requirements (`docs/srdd/requirements.toml`), automated tests, and source modules verified by `cargo xtask traceability`. The test suite combines component mocking, property-based testing, integration tests, regression tests, end-to-end tests, and mutation testing with [cargo-mutants](https://mutants.rs/).

### 16.1 Testing boundary discipline: what we test vs. what we do not test

A disciplined test suite tests the logic that the engineering team controls and owns. Do not write tests that merely check the Rust standard library, the Tokio scheduler, SQLite's internal B-tree pager, `hyper` framing, or static TOML files. Over-testing external runtimes wastes CI time and creates brittle noise.

| Inside our testing boundary (MUST test thoroughly) | Outside our testing boundary (DO NOT over-test) |
| --- | --- |
| Request gates (`G1`..`G7`), candidate checks (`C1`..`C8`), `effective_share` precedence, and three-tier pool resolution (`Own`, `Shared`, `Global`, `Blended`) (`RTE-3`, `SHR-3`, `USR-3`) | Whether `tokio::spawn` schedules tasks onto OS threads or whether `dashmap` implements concurrent hash shards internally |
| Pluggable `RouteSelector` strategies (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, `power_of_two_choices`), zero-sum SWRR weight invariant across subset churn, `u64::MAX` wrap-around in `naive_rr`, seeded PRNG determinism and chi-squared distribution bounds, `p2c` load scoring, per-model priority overrides, strict tier fallback, and headroom `bps` scaling (`RTE-4`, `RTE-5`, `RTE-6`, `RTE-13`, `RTE-14`) | Whether `reqwest` or `rustls` implements the TLS 1.3 handshake state machine according to RFC 8446 |
| Eight-step inbound session precedence, `response_links` session inheritance (`Section 11.1.2`), canonical byte-sliced Step 8 fingerprinting, 21-slot `ConstantSessionDetector`, and `Shared`/`Global` carrier stripping (`RTE-1`, `RTE-12`, `SEC-16`) | Whether `sha2`, `chacha20poly1305`, or `argon2` implements its underlying mathematical primitives correctly |
| Duplicate-key rejection and surgical top-level JSON span editing (`model`, `prompt_cache_key`, `stream_options.include_usage`) without altering message byte order (`TRN-1`, `TRN-2`, `API-4`, `API-5`) | Whether `serde_json` parses valid RFC 8259 JSON strings in general |
| Inline `SseUsageTap`, bounded `TraceTap`, `RequestLifecycle` drop finalization, unified cache-read and cache-write subtraction, content-addressed `trace_blocks` LCP reconstruction, and binary-aware token estimation (`USG-1`..`USG-3`, `TRC-2`..`TRC-5`) | Whether SQLite's `PRAGMA journal_mode = WAL` writes frames to the `-wal` file on the OS filesystem |
| Source provider `/usage` response parsing, time-bounded `UpstreamObservation` projection and expiry, single-flight polling, and allow-list `sanitized_json` (`USG-4`..`USG-8`, `SEC-18`) | Live third-party provider uptime during automated CI runs (live upstream checks run only in an opt-in manual harness) |
| Our SQL query predicates (`ActorScope`), triggers, `model_pk` merges, canonical length-prefixed AAD binding (`fixtures/crypto/aad_vectors.json`), resumable 500-row rotation, and `audit_log` HMAC verification (`SEC-1`, `SEC-13`, `SEC-17`) | Static constant values in preset files unless validating that every preset parses into a valid `ProviderConfigJson` |

### 16.2 Failing tests taxonomy: expected failures vs. unexpected failures

Failing tests come in two distinct kinds. Confusing them turns a test suite into friction instead of a safety net. Every engineer and automated agent working on LLMRoller must follow the taxonomy and protocol below.

#### 16.2.1 Expected failures (part of building or changing behavior intentionally)

Expected failures happen when building something new or intentionally replacing a specified behavior. The table below defines the three rules for expected failures.

| Rule | Engineering protocol for expected failures |
| --- | --- |
| 1. Red-Green-Refactor for new specs | When building a new requirement, write the failing test guard first and cite `#[doc = "Spec: <ID>"]` (or `#[doc = "Partial: <ID>"]` when testing a foundational building block before the final integration sub-phase). Feature branches may hold a red test commit while developing locally, but every commit merged to `main` must be green (test and implementation committed together). |
| 2. Replace retired behavior in the same change | When a product brief or architectural decision deliberately replaces an existing behavior, any test that pinned the old behavior will turn red. Update or retire that test in the exact same commit or pull request, naming the requirement ID or decision ID (`ADR-*`) that sanctioned the change. |
| 3. Test observable contracts, not internals | A test that pins private struct fields, internal helper counts, or exact SQL whitespace is a test waiting to fail during routine refactoring. Write tests against observable inputs, outputs, state transitions, and wire effects so redesigning internal code underneath does not break them. |

#### 16.2.2 Unexpected failures (the real defect signal: stop and investigate)

Unexpected failures happen when changing one component breaks a test in another component. They also happen when a change breaks unintended behavior.

| Rule | Engineering protocol for unexpected failures |
| --- | --- |
| 1. Leaking boundary or unintended side effect | If changing `/usage` observation projection in `crates/core` breaks a session-pinning test or a personal-share authorization test, an architectural boundary is leaking. |
| 2. Zero tolerance for loosening assertions | When an unexpected failure occurs, never loosen the assertion, add `#[ignore]`, delete the test, or tweak mock data just to make CI green. |
| 3. The red-test rule of thumb | A red test is acceptable only when the change was meant to make that test red. Every other red test must be root-caused before work continues. |

### 16.3 Strict TDD workflow

Every feature, bug fix, or schema change follows a five-step loop enforced by pull-request review and CI gates.

| Step | Phase | Mandatory action |
| --- | --- | --- |
| 1 | Link requirement | Identify the exact requirement ID(s) in Section 6, 7, or 8 and `docs/srdd/requirements.toml`, or add a new requirement ID to the SRDD and `requirements.toml` first. |
| 2 | Write failing test (Red) | Write the unit, property, or integration test asserting the observable contract and linking `#[doc = "Spec: <ID>"]` (or `#[doc = "Partial: <ID>"]`). Confirm it fails as expected with `cargo nextest run`. |
| 3 | Write minimal code (Green) | Implement the minimal production logic in `core`, `proto`, `store`, `proxy`, `admin`, or `server` to pass the new test and all existing tests. |
| 4 | Refactor cleanly | Remove duplication and tighten types while keeping `cargo clippy --all-targets -- -D warnings` and `cargo nextest run` green. |
| 5 | Kill surviving mutants | Stage the changes (`git add -A`), generate the staged diff (`git diff --cached > target/commit.diff`), and run `cargo mutants --in-diff target/commit.diff` locally (enforced as a blocking gate on every PR against `origin/main...HEAD` and nightly across the full workspace). |

### 16.4 Component mocking and dependency injection architecture

To test our controlled logic thoroughly without flakiness from wall-clock time, OS entropy, network sockets, or slow disk I/O, LLMRoller defines six core seam traits. Note that `crates/core::Clock` is strictly synchronous (`now_ms`, `monotonic_ms`) so `crates/core` stays 100% free of async runtime dependencies, while `crates/proxy::AsyncClock: Clock` adds `async fn sleep(&self, d: Duration)` for proxy retry delays.

| Seam trait | Production implementation | Test fake or mock implementation | Controlled logic verified by mocking this seam |
| --- | --- | --- | --- |
| `Clock` (`fn now_ms(&self) -> i64`, `fn monotonic_ms(&self) -> i64`) and `AsyncClock: Clock` (`async fn sleep(&self, d: Duration)`) | `SystemClock` (`time::OffsetDateTime::now_utc`, `std::time::Instant`, `tokio::time::sleep`) | `FakeClock` (atomic millisecond counter advanced manually by tests) | `PinStore` provisional (50 s) and confirmed (`pin_ttl_secs`) expiry (`RTE-10`), `recovery_repin_min_idle_secs` (`300 s`), 360-bucket rolling rings and day-clamped calendar months (`LIM-1`), 30-minute `UpstreamObservation` decay, `prev_key_expires_at` rotation grace, and `Retry-After <= 2 s` same-key retry (`RTE-9`). |
| `EntropySource` (`fn fill_bytes(&self, dest: &mut [u8])`, `fn next_u64_bounded(&self, bound: u64) -> u64`) | `OsEntropy` (`rand::rngs::OsRng` + thread-local `ChaCha12Rng` for lock-free routing picks) | `DeterministicRng` (seeded ChaCha20/ChaCha12 RNG for reproducible test vectors and routing sequences) | Client key generation (`llr_...`, `SEC-3`), XChaCha20 24-byte nonces (`SEC-1`, `fixtures/crypto/aad_vectors.json`), web session and CSRF token creation (`AUTH-7`, `SEC-5`), and deterministic `uniform_random`, `weighted_random`, and `power_of_two_choices` route selection (`RTE-14`). |
| `UpstreamTransport` (`async fn send_inference(&self, req: OutboundRequest) -> Result<UpstreamResponseStream, TransportError>`) | `ReqwestUpstreamTransport` (with `GuardedConnector` socket SSRF filter and `rustls`) | `MockUpstreamTransport` (programmable per-route response queue: SSE chunk streams, comment keep-alives, delays, mid-stream drops, 429/401/403/503 responses, and outbound request capture) | Pre-first-valid-frame failover and 64 KiB holding (`RTE-9`), `HardKey` no-failover (`RTE-12`), five-class error classification (`API-8`), request/response header policies (`API-2`, `API-3`, `API-6`), surgical body edits (`API-4`), and `RequestLifecycle` disconnect finalization (`API-7`). |
| `UsageEndpointClient` (`async fn fetch_usage(&self, key: &UpstreamKey) -> Result<RawUsageResponse, UsagePollError>`) | `HttpUsageEndpointClient` | `StubUsageEndpointClient` (returns canned or sequence-scripted OpenCode Go, Command Code, OpenRouter, or 404/500 responses) | Single-flight polling, `local_at_observation` capture at `t0`, `UpstreamObservation` projection and 30-minute decay, key exhaustion transitions (`USG-4`..`USG-6`), and `/v1/usage` pass-through sanitization (`USG-7`, `USG-8`, `SEC-18`). |
| `StorageRepository` (`UserRepo`, `ProviderRepo`, `ShareRepo`, `CatalogRepo`, `PinRepo`, `UsageRepo`, `AuditRepo`) | `SqliteRepository` (`sqlx::SqlitePool` via `connect_options(role)`) | `InMemoryRepository` (for fast handler unit tests) plus ephemeral temp-file SQLite WAL instances (for `crates/store` SQL integration tests) | Handler RBAC enforcement (`ActorScope`, `USR-1`..`USR-5`), share acceptance and `share_config_fingerprint` suspension (`SHR-3`, `SHR-5`), atomic `/setup` (`AUTH-1`), in-transaction `last_admin` guard (`AUTH-5`), and three-queue writer backpressure (`REL-3`). |
| `SecretSealer` (`fn seal(&self, domain: &str, fields: &[&[u8]], plain: &[u8]) -> Vec<u8>`, `fn open(...)`) | `XChaCha20Poly1305Sealer` | `TestSealer` (real XChaCha20-Poly1305 initialized with a fixed 32-byte test master key and `instance_id`) | Canonical length-prefixed AAD tampering rejection across all 6 sealed tables and resumable 500-row master-key rotation (`SEC-1`, `SEC-2`). |

### 16.5 Test levels and suites

The test pyramid consists of six automated suites run via `cargo nextest` and dedicated verification tools.

#### 16.5.1 Level 1: Deterministic unit and property-based tests (`crates/core`, `crates/proto`)

Unit and property tests verify pure domain and protocol rules in milliseconds without network or disk I/O.

| Test category | Invariants and behaviors verified |
| --- | --- |
| Table-driven unit tests | Eight-step session resolution plus `response_links` inheritance, canonical byte-sliced Step 8 fingerprinting (including multi-byte UTF-8 characters straddling offset 4096 and moving `cache_control` markers), 21-slot `ConstantSessionDetector` (skipping Steps 1..3 and `x-claude-code-agent-id`), digit-preserving model normalization and within-key `--` collision resolution, `ActorScope` RBAC checks, `G1`..`G7` and `C1`..`C8` across all five `routing_scope` modes, and five-class error classification. |
| Pluggable routing strategy `proptest` and statistical suite (`RTE-4`, `RTE-14`) | (1) `smooth_weighted_rr` & `weighted_rr`: for 1 to 50 routes with weights in `1..=1000`, over 100,000 picks, each route's share stays within `0.5%` of `W_eff_i / sum(W_eff)`, SWRR consecutive runs never exceed `ceil(max(W) / min(W))`, and `sum(current) == 0` holds across subset churn (`1.0%` subset-relative bound). (2) `naive_rr`: exact cyclic sequence `0, 1, ..., N-1` over `K * N` picks regardless of configured route weights, plus `AtomicU64` wrap-around from `u64::MAX - 5` across 8 concurrent threads. (3) `uniform_random` & `weighted_random`: seeded determinism with `DeterministicRng(42)` produces identical pick sequences across runs, `weighted_random` binary search (`partition_point`) matches exact cumulative intervals, and over 100,000 picks empirical frequencies stay within `1.0%` of target probabilities and pass a Pearson chi-squared goodness-of-fit test (`p > 0.001`). (4) `power_of_two_choices`: routes with higher `in_flight` or `ewma_ttfb_ms` receive proportionally fewer selections than idle low-latency routes of equal weight. (5) Two-user isolation: user B's `priority_override = 1000` on a shared key never alters user C's selection shares. |
| Surgical JSON span `proptest` suite | For any generated JSON payload, duplicate top-level keys always return 400; random `upstream_model_id` strings containing quotes and backslashes produce valid JSON whose parsed `model` equals the input; when `Own` tier and no edit applies, output `Bytes` pointer is identical to input (`TRN-1`); when edited, untouched spans keep exact byte equality (`TRN-2`). |
| Micro-USD cost, estimator, and window `proptest` suite | Integer micro-USD cost math never overflows up to `10^9` tokens; `estimate_input_tokens` counts a 4 MB base64 image as `1,600` tokens plus text; generalized 360-bucket rolling rings, day-clamped calendar months (31 Jan -> 28/29 Feb -> 31 Mar), and 30-minute decaying `UpstreamObservation` projections match exact reference sums. |

#### 16.5.2 Level 2: Golden fixture and wire-contract tests (`fixtures/`)

Recorded real-world payloads in `fixtures/` are replayed through `crates/proto` and `crates/store` to lock wire and cryptographic compatibility.

| Fixture set | Recorded scenarios verified |
| --- | --- |
| `fixtures/crypto/aad_vectors.json` | Fixed-key, fixed-nonce test vectors for all 6 `aad(domain, fields)` domains (`instance_secret`, `totp_secret`, `upstream_key`, `usage_raw`, `trace_block`, `trace_record`). |
| `fixtures/sse/` | Multi-chunk SSE streams for OpenAI `chat` (with and without `include_usage`), OpenAI `responses` (`response.created`, `response.completed`, `reasoning_tokens`), Anthropic `messages` (`message_start`, `content_block_delta`, `message_delta`, `message_stop`), OpenRouter `: OPENROUTER PROCESSING` keep-alives and `usage.cost`, and truncated streams ([command-code#785](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2FCommandCodeAI%2Fcommand-code%2Fissues%2F785)). |
| `fixtures/usage/` | Recorded JSON payloads from OpenCode Go (`/zen/go/v1/usage`), Command Code (`/alpha/billing/credits` and `/alpha/usage/summary`), OpenRouter (`/api/v1/key`), and Open Design AMR (404 and proposed [#8674](https://github.com/nexu-io/open-design/issues/8674) schema), verifying normalization and allow-list `sanitized_json`. |

#### 16.5.3 Level 3: Component and SQLite integration tests (`crates/store`, `crates/proxy`, `crates/admin`)

Integration tests run against temporary SQLite WAL databases and local `wiremock` or custom `hyper` fault-injection servers.

| Integration test suite | Scenario and assertions |
| --- | --- |
| Concurrency and single-flight pinning (`RTE-7`) | (1) Spawn 100 concurrent turn-1 tasks when stub returns 200 -> all 100 calls hit one `upstream_key_id` and write one `pins` row. (2) Spawn 10 concurrent turn-1 tasks when `K1` returns 429 (no `Retry-After`) and others return 200 -> all 10 tasks adopt the same failover winner `K2` (`MockUpstreamTransport` records zero calls to any third key). |
| Failover before first valid frame vs. post-commit no-retry (`RTE-9`, `RTE-12`) | Verify that a connect error, 503, 429, or HTTP 200 whose first `data:` frame is an error object fails over to `K2` while holding status/headers until `K2`'s first valid frame commits (client sees one 200 status line), whereas a stream drop after the first valid frame is committed never retries and marks `incomplete = 1`, and a `HardKey` (`previous_response_id`) request on a cooling/503 key returns 429/502 with zero calls to other keys. |
| Consented personal sharing and tier isolation (`SHR-1`..`SHR-6`, `SEC-16`..`SEC-18`) | User A creates a personal provider, adds a key, and shares it with User B ($5 monthly cap, `allowed_models = ["deepseek-v4.1-flash"]`). Verify the share is inert until B accepts `share_config_fingerprint`; once accepted, with B's client key set to `personal_only`, B routes `deepseek-v4.1-flash` (spending B's own key first under `personal_first` if B has one), gets HTTP 404 `model_not_found` on unserved models, has client session headers stripped and replaced with the salted token, never receives `openai-organization` or `x-ratelimit-*` headers, and is suspended within 1 s if A edits `paths.chat` or revokes the share. |
| Security, SSRF connector, and canary redaction (`SEC-1`..`SEC-19`) | Plant canary secret `sk-canary-9988776655443322`, trigger upstream 401/500 errors, invalid JSON, and panics, and assert zero canary occurrences across logs, HTTP responses, SQLite tables, and `/metrics`. Verify `GuardedConnector` blocks IP-literal `https://127.0.0.1`, `https://[::ffff:127.0.0.1]`, 6to4 `https://[2002:a9fe:a9fe::1]`, decimal `https://2130706433` (at save), DNS rebinding to `10.0.0.5`, and HTTP 302 redirects. |

#### 16.5.4 Level 4: Regression test suite

Every defect discovered during development, QA, or production operation follows the four-step Regression Lock Protocol:
1. Before changing production code, add a deterministic test in `crates/<crate>/tests/regressions.rs` named `regression_<issue_id>_<short_description>` with `#[doc = "Regression: #<issue_id> | Spec: <REQ-ID>"]`.
2. Run `cargo nextest` and confirm the new regression test fails (`Red`) on `HEAD`.
3. Apply the fix and confirm the regression test passes (`Green`).
4. Keep regression tests permanently unless the underlying requirement is formally retired under Section 16.2.1.

#### 16.5.5 Level 5: Full end-to-end (E2E) and agent compatibility tests

The E2E suite compiles the release `llmroller` binary and launches it as a child process against a temporary SQLite directory and a mock upstream cluster.

| E2E suite | End-to-end verification flow |
| --- | --- |
| Full multi-role lifecycle (`tests/e2e_lifecycle.rs`) | Complete `/setup` with the `setup-token` file, confirm `/setup` locks to 404, add global providers as `admin`, create standard users Alice and Bob, log in as Alice, add a personal OpenCode Go key, share it with Bob, accept the share as Bob, issue `llr_...` keys, stream multi-turn chats and chained `previous_response_id` turns, verify `/v1/usage` and `/v1/key`, verify `project_key` redaction across `/api/user/*` and HTMX pages, send `SIGTERM` mid-stream to verify the 8-step drain (`REL-1`), and restart to verify `< 2 s` rebuild from `usage_rollups_5min` (`PERF-4`, `REL-2`). |
| Real CLI harness compatibility (`tests/e2e_cli_harnesses.rs`, `COM-1`) | Run headless invocations of `claude` (Claude Code), `codex`, `opencode`, and Python and TypeScript scripts using the official `openai` and `anthropic` SDKs against LLMRoller, completing multi-turn tool-call loops and stateful `previous_response_id` chains and asserting pin stickiness, single `session_hash` inheritance, and exact usage rows. |

#### 16.5.6 Level 6: Mutation testing (`cargo-mutants`)

Line and branch coverage (`cargo llvm-cov`, minimum gate `90%` workspace-wide and `95%` in `crates/core` and `crates/proto`) only proves that a test ran a line of code. It does not prove that any assertion checked the result ([cargo-mutants vs. coverage](https://www.google.com/search?q=https%3A%2F%2Fmutants.rs%2Fvs-coverage.html)).

LLMRoller enforces automated mutation testing using [`cargo-mutants`](https://mutants.rs/).

| Mutation aspect | Specification and CI gate |
| --- | --- |
| Injected fault types | `cargo-mutants` parses the Rust AST and injects behavioral faults one by one: replacing functions with `Ok(())` or `Default::default()`, flipping `<` to `<=` or `>` in budget reserve and cooldown checks, inverting `&&` and `||` in route eligibility (`C1`..`C8`) and RBAC guards (`SEC-17`), deleting match arms in the five-class error classifier, and changing `+`, `-`, or `*` in WRR and micro-USD cost math. |
| Pull-request gate (`--in-diff`) | Every pull request generates a unified diff (`git diff origin/main...HEAD > target/pr.diff`) and runs `cargo mutants --in-diff target/pr.diff -j 4` across modified Rust files ([diff testing](https://www.google.com/search?q=https%3A%2F%2Fmutants.rs%2Fin-diff.html)). Any surviving mutant fails the PR build unless annotated with `#[mutants::skip] // equivalent: <reason>` (CI fails if any `#[mutants::skip]` lacks an `equivalent:` reason comment or CODEOWNERS review). |
| Nightly full-workspace gate | Nightly CI runs `cargo mutants --workspace` across `crates/core`, `crates/proto`, `crates/store`, `crates/proxy`, and `crates/admin`. |
| Target mutation score | `100%` killed mutants (zero unannotated survivors) in `crates/core`, `crates/proto`, and all RBAC, share, and cryptographic guards in `crates/store`, `crates/proxy`, and `crates/admin`. |

### 16.6 Code-test-spec traceability matrix (`cargo xtask traceability`)

Every requirement ID in this SRDD is registered in `docs/srdd/requirements.toml` with its `priority`, `milestone`, `delivering_subphase`, and `partial_subphases`. On every commit and PR, `cargo xtask traceability --milestone M<N>` and `cargo xtask doc-consistency` scan `requirements.toml`, the SRDD and Big-Bang tables, and Rust test attributes (`#[doc = "Spec: <ID>"]` and `#[doc = "Partial: <ID>"]`). A milestone gate fails if any requirement assigned to that milestone in `requirements.toml` has only `Partial` links or zero full `Spec` links.

| Requirement ID(s) | Milestone & delivering sub-phase | Source crate & module | Unit / property test target | Integration / E2E / mutation target |
| --- | --- | --- | --- | --- |
| `AUTH-1`..`AUTH-9` | `M0` (`1.4`) | `crates/admin::auth`, `crates/store::users` | `admin::auth::tests::*`, `store::users::tests::*` | `tests/e2e_setup_and_auth.rs` (20-way setup race `COUNT(*) == 1`, `last_admin` tx guard, `reset-admin` CLI, 3-tier login limiter, TOTP, CSRF) |
| `USR-1`..`USR-5` | `M0` (`1.2`), `M1` (`2.0`), `M2` (`3.0c`), `M5` (`6.2`, `6.3`) | `crates/core::rbac`, `crates/admin::projection`, `crates/store::*` | `core::rbac::tests::actor_scope_matrix` | `tests/integration_rbac_isolation.rs` (`ActorScope::System` boundary, 404 IDOR checks, `project_key` global label redaction) + `cargo-mutants` |
| `PRV-1`..`PRV-8` | `M1` (`2.0`, `2.1`, `2.6`), `M2` (`3.6b`), `M4` (`4b.2`) | `crates/core::provider`, `crates/proto::adapters`, `crates/proxy::ssrf`, `crates/store::providers` | `core::provider::tests::*`, `proto::adapters::tests::*`, `proxy::ssrf::tests::ip_ranges` | `tests/integration_providers_and_keys.rs` (`GuardedConnector` IP-literal / 6to4 / DNS-rebinding SSRF tests, key disable invalidation) |
| `SHR-1`..`SHR-6` | `M2` (`3.2`), `M3` (`4.3`) | `crates/core::shares`, `crates/store::shares`, `crates/admin::shares` | `core::shares::tests::eligibility_and_caps` | `tests/integration_personal_sharing.rs` (409 `share_overlap`, `share_config_fingerprint` acceptance & suspension, `custom` admin approval, monthly cap reservation) |
| `CLK-1`..`CLK-5` | `M1` (`2.2`), `M2` (`3.0c`, `3.4`, `3.6b`) | `crates/core::client_key`, `crates/proxy::auth` | `core::client_key::tests::*` | `tests/integration_client_keys.rs` (`prev_key_hash` rotation grace, 5 `routing_scope` modes, <1 s revocation, G1..G7 gates) |
| `MDL-1`..`MDL-7` | `M2` (`3.1`) | `crates/core::catalog`, `crates/proto::discovery`, `crates/store::models` | `core::catalog::tests::normalization_and_exposure` | `tests/integration_model_catalog.rs` (digit-dot preservation, `--` collision, `merge_state`, surrogate `model_pk` merge/split, `union`/`intersection`, 2,000 personal cap) |
| `RTE-1`..`RTE-14` | `M2` (`3.3`, `3.4`, `3.5`, `3.6a`, `3.6b`, `3.7`) | `crates/core::router`, `crates/core::strategy`, `crates/core::wrr`, `crates/core::pins`, `crates/core::session` | `core::strategy::proptests::*`, `core::wrr::proptests::*`, `core::session::tests::*`, `core::pins::tests::*` | `tests/integration_routing_and_failover.rs` (all 6 `RoutingStrategy` variants, hot-reload strategy switching, `PinStore` 10-way 429 loser adoption, durable `response_links` session inheritance, `HardKey` no-failover, `OriginBreaker`) + `cargo-mutants` |
| `API-1`..`API-8`, `TRN-1`..`TRN-3` | `M1` (`2.3`..`2.7`), `M2` (`3.6a`, `3.6b`), `M3` (`4.1`), `M4` (`4b.2`) | `crates/proto::body_edit`, `crates/proto::headers`, `crates/proxy::forward` | `proto::body_edit::proptests::*`, `proto::headers::tests::*` | `tests/integration_wire_transparency.rs` (duplicate-key 400, response header `R1`..`R5`, 64 KiB frame filter, `RequestLifecycle` disconnect guard, 5-class error classifier) |
| `USG-1`..`USG-8`, `LIM-1` | `M2` (`3.0a`, `3.0b`), `M3` (`4.1`, `4.2`, `4.3`, `4.6`), `M4` (`4b.1`, `4b.2`) | `crates/proto::sse_tap`, `crates/core::pricing`, `crates/core::windows`, `crates/proto::usage_poller` | `proto::sse_tap::golden_tests::*`, `core::pricing::proptests::*`, `core::windows::tests::*` | `tests/integration_usage_and_passthrough.rs` (`usage_attempts`, `usage_rollups_5min` `< 2 s` rebuild, 30-min `UpstreamObservation` decay, single-flight polling, `raw_json_enc`) |
| `TRC-1`..`TRC-9`, `PERF-5`, `SEC-19` | `M3b` (`5.1`..`5.4`), `M5` (`6.2`, `6.4`, `6.5`) | `crates/proto::trace_tap`, `crates/core::chat_preview`, `crates/store::traces`, `crates/admin::traces` | `proto::trace_tap::tests::*`, `core::chat_preview::tests::dedup_and_branching` | `tests/integration_content_traceability.rs` (`trace_blocks` dedup, 2-thread `zstd::bulk::Compressor` pool, block-hash LCP, `(ts, request_id)` cursor pagination, per-client-key recently active sessions with live `PinStore` provider/key projection, `ammonia` + `<pre>` XSS tests) |
| `UI-1`..`UI-4`, `AUD-1` | `M0` (`1.5`), `M5` (`6.1`..`6.5`) | `crates/admin::ui`, `crates/store::audit` | `store::audit::tests::hash_chain_verification` | `tests/e2e_dashboard_htmx.rs` (`audit_anchor` truncation detection, HTMX CSP lint, share acceptance UI, `"Global pool"` matrix column, preview drawer) |
| `PERF-1`..`PERF-5`, `REL-1`..`REL-6`, `COM-1`, `POR-1`, `MNT-1`..`MNT-2`, `OBS-1`, `PRIV-1` | `M0` (`1.1`, `1.6`), `M1` (`2.2`, `2.5`, `2.7`), `M3` (`4.3`, `4.7`), `M4` (`4b.1`), `M6` (`7.1`..`7.5`) | `crates/server`, workspace-wide | `tests/workspace_boundaries.rs`, metric label allow-list test | `benches/proxy_latency.rs` (log-normal TTFF stub), `tests/e2e_sigterm_drain.rs`, `tests/e2e_kill9_recovery.rs`, `tests/e2e_cli_harnesses.rs` |
| `SEC-1`..`SEC-19` | `M0` (`1.1`..`1.5`), `M1` (`2.1`..`2.4`), `M2` (`3.2`, `3.3`), `M3b` (`5.3`, `5.4`), `M4` (`4b.2`), `M5` (`6.1`), `M6` (`7.1`, `7.5`) | `crates/store::crypto`, `crates/proxy::ssrf`, `crates/admin::csrf`, `crates/admin::traces` | `store::crypto::tests::aead_aad_and_rotation`, `proxy::ssrf::tests::ip_ranges` | `tests/security_canary_and_ssrf.rs`, `tests/security_csrf_and_headers.rs` + `cargo-mutants` on all security modules |

## 17. Roadmap and milestones

Implementation follows eight dependency-ordered milestones (`M0`, `M1`, `M2`, `M3`, `M4`, `M3b`, `M5`, `M6`) that map one-to-one to the Big-Bang epics. A milestone is complete when `cargo xtask traceability --milestone <M>` verifies 100% `Spec:` coverage and `cargo-mutants` reports zero unannotated survivors.

### 17.1 Milestone delivery plan

The table below defines the scope, deliverables, and exit criteria for each milestone (generated from `docs/srdd/requirements.toml`).

| Milestone (Big-Bang Epic) | Scope and deliverables | Exit criteria (must pass in `cargo xtask traceability --milestone <M>` before starting next milestone) |
| --- | --- | --- |
| `M0`: Foundation, Dual-Queue Writer, Reloader, RBAC, and Auth (`Epic 1`: `1.1`..`1.6`) | Six-crate + `xtask` workspace, foundational Docker scaffolding (`Dockerfile`, `.dockerignore`, `docker-compose.yml`, `.env.example`), `POR-1` multi-arch CI matrix, `connect_options(role)` + 31-table SQLite WAL migrations (`auto_vacuum = INCREMENTAL`), `instance_meta` & `instance_secrets`, canonical AAD `XChaCha20-Poly1305` sealer, offline resumable 500-row `rotate-master-key`, `ActorScope::{User, System}`, `Argon2id` + TOTP + `setup-token` file (`0600`) + `reset-admin` CLI + 3-tier login limiter, HMAC `audit_log` with `audit_anchor`, dual-queue writer (`Control` + `Data` `4 MiB` cap), closed 14-key settings registry (including `default_routing_strategy`), and single `Reloader` task (`config_version` + `RuntimeState`). | `AUTH-1`..`AUTH-9`, `USR-1`, `AUD-1`, `MNT-1`, `MNT-2`, `POR-1` (build matrix + base container build), `REL-4`, `SEC-1`, `SEC-2`, `SEC-4`, `SEC-10`, `SEC-13`, `SEC-14`, `SEC-17` tests pass; zero surviving mutants in `crates/store`. |
| `M1`: Transparent Pass-Through Proxy & Orchestrator v1 (`Epic 2`: `2.0`..`2.7`) | Minimal control-plane JSON CRUD (`2.0`), `GuardedConnector` socket-level SSRF filter (`2.1`), client key issuance and `prev_key_hash` grace rotation + gates `G1`..`G6` + `auth_failure_counters` + `/healthz`, `/readyz`, `/metrics` with closed label allow-list (`2.2`), request & response header pipelines (`R1`..`R5`) (`2.3`), duplicate-rejecting surgical JSON span editor (`2.4`), 64 KiB pre-first-frame holding buffer + 64 KiB usage-chunk filter + `RequestLifecycle` drop guard (`2.5`), `ProviderAdapter` presets (`2.6`), and `/v1/*` orchestrator v1 + `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` admission queue + 8-step `SIGTERM` drain + parser fuzz targets + initial TTFF load benchmark (`2.7`). | `PRV-1`..`PRV-5`, `PRV-7`, `CLK-1`, `CLK-4`, `API-2`..`API-4`, `API-6`, `API-7`, `TRN-1`..`TRN-3`, `PERF-1`, `PERF-2`, `REL-1`, `OBS-1`, `SEC-3`, `SEC-6`..`SEC-9`, `SEC-12`, `SEC-15` tests pass. |
| `M2`: Pure Pricing/Windows Core, Catalog, Consented Sharing, Pluggable Routing Engine, and Pinning (`Epic 3`: `3.0a`..`3.7`) | Pure `crates/core::pricing` + binary-aware estimator (`3.0a`), pure `WindowState` (360-bucket rings, day-clamped calendar months, decaying `UpstreamObservation`, cache-aware `try_reserve`) (`3.0b`), pure UTC monthly accumulators + gate `G7` (`3.0c`), `/models` discovery + digit-preserving normalization + surrogate `model_pk` merge/split (`3.1`), consented `provider_shares` (`effective_share`, `share_config_fingerprint`, `custom` approval) (`3.2`), 8-step session resolver + 21-slot detector + `Shared`/`Global` carrier stripping (`3.3`), `C1`..`C8` + 3-tier pool selector (`3.4`), pluggable `RouteSelector` engine (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, `power_of_two_choices`) (`3.5`), pure health/5-class classifier/`OriginBreaker` (`3.6a`), `PinStore` mutex state machine + durable `response_links` inheritance + failover (`3.6b`), and orchestrator v2 (`3.7`). | `USR-2`..`USR-5`, `PRV-8`, `SHR-1`..`SHR-5`, `CLK-2`, `CLK-3`, `CLK-5`, `MDL-1`..`MDL-7`, `RTE-1`..`RTE-14`, `API-8`, `LIM-1`, `REL-5`, `SEC-16` tests pass; zero surviving mutants in `crates/core`. |
| `M3`: Usage Pipeline, 5-Min & Hourly Rollups, Orchestrator v3, and Retention (`Epic 4`: `4.1`..`4.3`, `4.6`, `4.7`) | Inline `SseUsageTap` and `JsonUsageExtractor` with unified `cache_read` and `cache_write` subtraction (`4.1`), `models.dev` price sync and body-based price APIs (`4.2`), three-queue writer (`durable_state`, `rollups`, `raw_events` `VecDeque` dropping oldest) + `usage_events` + `usage_attempts` + `usage_rollups_5min` + `usage_rollups_hourly` + `SIGKILL` recovery (`4.3`), orchestrator v3 wiring live usage and reservations (`4.6`), and general retention sweeper + 30 s/256 MiB WAL checkpointer + `SQLITE_FULL` handling (`4.7`). | `SHR-6`, `API-5`, `USG-1`..`USG-3`, `REL-2`, `REL-3`, `PRIV-1` tests and all golden SSE fixtures in `fixtures/sse/` pass. |
| `M4`: Window Persistence, `< 2 s` Startup Reconstruction, and Source Provider `/usage` Engine (`Epic 4b`: `4b.1`, `4b.2`) | Startup reconstruction (`< 2 s`) of all window rings and UTC monthly accumulators from `usage_rollups_5min`, window-instance-bound `budget_adjustments`, and `upstream_usage_snapshots` (`4b.1`), plus single-flight `/usage` poller (8-permit concurrency semaphore, `local_at_observation` at `t0`, `raw_json_enc` sealing, allow-list `sanitized_json`, decaying `UpstreamObservation` sync, and header-scoped `/v1/usage`, `/v1/key`, and stored `/v1/usage/raw`) (`4b.2`). | `PRV-6`, `API-1`, `USG-4`..`USG-8`, `PERF-4`, `SEC-18` tests and all `/usage` fixtures in `fixtures/usage/` pass. |
| `M3b`: Opt-In Content Traceability and Session Chat Preview API (`Epic 5`: `5.1`..`5.4`) | Request-start `traceability_config` matcher + `"confirm_purge": true` guard (`5.1`), bounded `TraceTap` (`mpsc(256)` + `64 MiB` in-flight + `1 MiB` per-stream output cap + 2,000-stream memory soak) (`5.2`), content-addressed `trace_blocks` + 2-thread OS sealer pool (`crossbeam::bounded(64)` + `zstd::bulk::Compressor`) (`5.3`), and block-hash LCP reconstructor + `(ts, request_id)` cursor pagination + per-client-key recently active sessions with live `PinStore` provider/key lookup + 30-min deduplicated `trace.viewed` audit logging (`5.4`). | `TRC-1`..`TRC-5`, `TRC-7`..`TRC-9`, `PERF-5`, `SEC-19` tests pass. |
| `M5`: Dual-Workspace HTMX Web UI and Session Chat Preview UI (`Epic 6`: `6.1`..`6.5`) | Embedded Askama + HTMX shell (`includeIndicatorStyles = false`, `allowEval = false`, `allowScriptTags = false`) + CSRF middleware (`6.1`), Personal Workspace `/app/*` and `/api/user/*` with share-acceptance modal and `project_key` verification (`6.2`), Admin Console `/admin/*` and `/admin/api/*` with user-resource override routes and `model_pk` merge/split (`6.3`), Overview dashboard (`uPlot`, dual gauges, disk budget `A6`) + Active Sessions + paged Usage Explorer (`6.4`), and read-only Session Chat Preview UI (`6.5`). | `UI-1`..`UI-4`, `TRC-6`, `SEC-5` E2E browser and HTMX tests pass. |
| `M6`: Extended Fuzzing, Full Soak Verification, Live Harnesses, Container Smoke Gate, and v1.0.0 Release (`Epic 7`: `7.1`..`7.5`) | Extended 24-hour fuzzing across all 6 fuzz targets + full-stack canary redaction test (`7.1`), full-workspace `cargo mutants --workspace` + `cargo xtask traceability --milestone M6` + `cargo xtask doc-consistency` (`7.2`), 500 req/s and 2,000-stream soak benchmark against 50M-row DB + `llmroller backup` `fsync` + exit codes `0..14` (`7.3`), headless CLI harness compatibility (`COM-1`) (`7.4`), and multi-arch static release + `< 50 MB` distroless container + `docker compose` smoke & routing-strategy integration gate + SBOM + `cosign` + `llmroller --version` (`7.5`). | All `PERF-1`..`PERF-5`, `REL-1`..`REL-6`, `POR-1`, `COM-1`, `SEC-11` checks pass; `cargo xtask traceability --milestone M6` reports 100% coverage. |

### 17.2 Post-1.0 roadmap (`P2` items)

Capabilities scheduled after version 1.0 ship behind clean extension traits without breaking schema compatibility:
1. Opt-in cross-protocol request and stream translation (`chat` <-> `messages` <-> `responses`) with explicit capability warnings when an upstream drops extended thinking blocks.
2. WebSocket pass-through transport for the OpenAI Realtime and stateful WebSocket Responses endpoints.
3. Scoped automation API tokens for `/admin/api/*` and `/api/user/*` CI/CD provisioning.
4. Optional PostgreSQL storage backend implementing the `StorageRepository` traits for multi-node active-active deployments.

## 18. Risks and mitigations

Fifteen technical, vendor, security, and operational risks are tracked below alongside their concrete architectural mitigations.

### 18.1 Risk register

Each risk lists its likelihood, impact, and the specific design mechanisms and requirement IDs that mitigate it.

| ID | Risk description | Likelihood | Impact | Mitigation in LLMRoller design |
| --- | --- | --- | --- | --- |
| R1 | An upstream provider suspends an account for pooling keys across multiple users or violating subscription terms. | Medium | High | Support personal providers (`PRV-1`) with three-tier `personal_first` or `own_only` routing so each user routes through their own `Own` key before any `Shared` or `Global` key, enforce grantee consent (`SHR-3`), forward authentic client headers on `Own` keys (`API-3`), and pin each session to a single key (`RTE-2`). |
| R2 | Source provider `/usage` endpoints change schema, return 5xx, or are undocumented (such as Command Code `/alpha/billing/credits` or Open Design AMR [#8674](https://github.com/nexu-io/open-design/issues/8674)). | High | Medium | Isolate `/usage` parsing inside `ProviderAdapter` (`MNT-2`), seal raw JSON in `raw_json_enc` (`USG-5`), and model upstream figures as time-bounded `UpstreamObservation` projections that expire after 30 minutes (`MAX_OBSERVATION_AGE_MS`) or at `resets_at_ms` (`USG-6`) so a failing `/usage` endpoint never leaves a key permanently excluded. |
| R3 | Local micro-USD cost estimates drift from a provider's internal billing due to external key usage, promotional multipliers, or failed tries. | High | Medium | Poll upstream `/usage` endpoints with single-flight deduplication and capture `local_at_observation` at poll start `t0` (`USG-4`, `USG-6`), record and charge failed tries that consumed compute in `usage_attempts` (`USG-1`), enforce cache-aware in-flight reservations (`LIM-1`), and support window-instance-bound manual calibration. |
| R4 | Command Code or Open Design AMR changes which session affinity header or body field its backend honors. | Medium | Medium | Make `session_profile` editable per provider in `ProviderConfigJson` (`PRV-3`) and surface the per-route cache hit ratio (`observed_cache_read_ratio_ewma`) on the dashboard so operators can verify cache locality empirically. |
| R5 | A provider enables automatic pay-as-you-go wallet fallback when subscription windows run out (such as OpenCode Go "Use balance" or Command Code extra credits). | Medium | High | Warn operators in the UI setup guide to disable automatic wallet fallback in provider consoles, or set LLMRoller window limits and reserve percentages (`reserve_pct = 5%`) below the plan cap so LLMRoller stops routing before spillover starts. |
| R6 | A grantee on a shared personal key drains the owner's quota, inspects the owner's account details, or has their prompts redirected to a grantor-controlled relay. | Medium | High | Separate `Own` and `Shared` tiers so grantees use their own keys first (`CLK-3`), enforce per-share `max_monthly_micro_usd` and `allowed_models` (`SHR-2`), enforce explicit grantee acceptance of `share_config_fingerprint` (`SHR-3`, `SEC-16`) plus admin approval for `custom` providers, and strip account headers and raw `/usage` PII on shared keys (`SEC-18`). |
| R7 | An upstream provider drops SSE streams mid-generation without sending `[DONE]`, `response.completed`, or `message_stop` ([command-code#785](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2FCommandCodeAI%2Fcommand-code%2Fissues%2F785)), or a slow generation times out. | Medium | High | Disable wait-timeout failover by default (`timeout_failover = false`) to prevent duplicate billing (`RTE-9`), never retry after the first valid frame commits, finalize every stream through `RequestLifecycle::drop` (`API-7`), and charge binary-aware token estimates on aborted streams (`USG-3`). |
| R8 | A misconfigured client sends a static constant session ID for every conversation, funneling all traffic onto one key ([vscode#334186](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2Fmicrosoft%2Fvscode%2Fissues%2F334186)). | Medium | Medium | Track opening-message fingerprint cardinality in the 21-slot exact `ConstantSessionDetector` per `(ClientKeyId, SessionHash)` for generic session sources (Steps 4..7, excluding Claude Code subagents and opencode child sessions, Section 11.1.1), flag any session exceeding 20 distinct conversations per hour in the Sessions UI, and let the owner or admin enable `constant_session_mode = 'fingerprint_generic_ids'`. |
| R9 | Long-running agent sessions skew actual token spend away from the session-count weights configured in smooth WRR. | Medium | Low | Provide `headroom` weight scaling (`RTE-13`) that scales WRR weights by minimum remaining window basis points `bps`, plus automatic repinning when a key's window reaches its limit (`RTE-11`). |
| R10 | Upstream providers change model availability, endpoint support (`chat` vs. `messages`), or pricing without notice. | High | Low | Run scheduled `/models` discovery every 6 hours (`MDL-1`) with a `missing` -> `retired` lifecycle that preserves overrides during transient provider glitches, use surrogate `model_pk` keys (`MDL-4`), and charge `unpriced_fallback_rates` when a newly discovered model lacks a price entry (`USG-3`). |
| R11 | A compromised crate in the Rust dependency ecosystem tries to exfiltrate decrypted upstream keys ([March 2026 LiteLLM incident](https://www.google.com/search?q=https%3A%2F%2Fdocs.litellm.ai%2Fblog%2Fsecurity-update-march-2026)). | Low | High | Keep the dependency list small, use pure-Rust `xtask` instead of external scripting runtimes, commit `Cargo.lock`, enforce `cargo audit`, `cargo deny`, and `cargo vet` in CI (`SEC-10`), run inside a read-only distroless container (`SEC-11`), and filter every socket connect through `GuardedConnector` (`PRIV-1`, `SEC-7`). |
| R12 | Losing `LLMROLLER_MASTER_KEY` or crashing mid-rotation leaves encrypted rows unreadable or invalidates the audit log. | Low | High | Keep `audit_hmac_key` and `session_hmac_secret` in `instance_secrets` so rotation never alters their values, track rotation progress in `instance_meta` across 500-row resumable batches (`SEC-1`), refuse `serve` startup while `rotation_state == 'in_progress'` (`SEC-2`), and include `master_key_id` in `llmroller backup` JSON output (`REL-6`). |
| R13 | Tests become brittle or allow subtle logic regressions in routing, budget math, or RBAC checks to slip into production. | Medium | High | Enforce strict TDD contract testing, boundary discipline, the Expected vs. Unexpected failure protocol (`Section 16.2`), `cargo xtask traceability`, and automated mutation testing with `cargo-mutants` (`Section 16.5.6`). |
| R14 | A single-node SQLite instance experiences disk growth, WAL starvation, or hardware failure. | Medium | Medium | Cap raw `usage_events` at 30 days and 50,000,000 rows (`A6`), rebuild startup state from narrow `usage_rollups_5min` rows in `< 2 s` (`PERF-4`), page all read queries so readers never pin the WAL, run 30 s `PASSIVE` and 256 MiB `RESTART` WAL checkpoints, handle `SQLITE_FULL` gracefully, and provide `fsync`-verified `llmroller backup` (`REL-6`). |
| R15 | Enabling content traceability mode exhausts RAM with per-stream compressors or exposes sensitive prompts across shared personal keys. | Medium | High | Keep traceability off by default (`TRC-1`), accumulate turn outputs in memory (`1 MiB` per stream, `256 MiB` global) and compress at turn end on a fixed 2-thread `zstd::bulk::Compressor` pool (`~7 MiB` total RAM, `PERF-5`), deduplicate request blocks per user in `trace_blocks` (`TRC-4`), and isolate traces strictly by `owner_user_id` (`TRC-8`, `SEC-19`). |

## 19. Architectural decisions and open questions

This section records the architectural decisions resolved during specification refinement and lists the remaining operational configuration choices for deployment.

### 19.1 Resolved architectural decisions (ADRs)

Twelve core design decisions govern version 1.0 of LLMRoller.

| ADR ID | Topic | Decision for version 1.0 | Rationale |
| --- | --- | --- | --- |
| `ADR-1` | Provider ownership scopes and consented sharing | Support `global` (admin-owned) and `personal` (user-owned) provider scopes. Personal shares need explicit grantee acceptance of `share_config_fingerprint` (and admin approval when `kind == 'custom'`), and forbid overlapping provider-level and key-level grants. | Lets individuals safely share spare capacity while preventing unconsented prompt redirection or double-cap accounting bugs. |
| `ADR-2` | Standard non-admin user class (`user`) and `ActorScope` | Ship the standard `user` role in version 1.0 (`P0`) with full self-service UI and API (`/app/*`, `/api/user/*`), enforced by a closed `ActorScope::{User, System}` enum on every repository method and `project_key` redaction on every user response. | Eliminates admin bottlenecks, keeps background jobs from masquerading as admins, and guarantees SQL-level and wire-level tenant isolation. |
| `ADR-3` | Three-tier pool ordering (`Own`, `Shared`, `Global`) | Partition routes into three ordered tiers (`Own`, `Shared`, `Global`) across five selectable `routing_scope` modes: `personal_first` (`[Own] -> [Shared] -> [Global]`), `blended`, `personal_only` (`[Own] -> [Shared]`), `own_only` (`[Own]`), and `global_only` (`[Global]`). | Guarantees that grantees spend their own personal subscription quota before consuming a teammate's shared quota under `personal_first`, and gates recovery repins on `recovery_repin_min_idle_secs` (`300 s`). |
| `ADR-4` | Source provider `/usage` observations and pass-through | Poll source provider `/usage` endpoints with per-key single-flight deduplication and capture `local_at_observation` at poll start `t0` to update a time-bounded `UpstreamObservation` (expiring after 30 minutes or at `resets_at_ms`), and expose header-scoped `/v1/usage`, `/v1/key`, and stored `/v1/usage/raw` with allow-list PII sanitization. | Prevents permanent key lockouts when `/usage` polling fails and eliminates timing jitter between poll start and completion. |
| `ADR-5` | Pluggable `RouteSelector` strategy engine vs. hardcoded SWRR | Abstract target selection behind `RouteSelector` (`smooth_weighted_rr` default, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, `power_of_two_choices`) scoped per `(user_id, model_pk, protocol, pool_tier)` with `strict` tier pre-filtering and lock-free thread-local `ChaCha12Rng` seeded from `EntropySource`. | Preserves deterministic Nginx SWRR interleaving by default while letting operators select unweighted cyclic, randomized, or latency-aware P2C load balancing per model or instance-wide (`RTE-4`, `RTE-14`). |
| `ADR-6` | Cross-protocol translation | Route requests in version 1.0 only to candidates that natively support the inbound protocol (`chat`, `responses`, or `messages`); defer opt-in cross-protocol translation to post-1.0 (`P2`). | Prevents silent loss of reasoning blocks ([open-design#8687](https://www.google.com/search?q=https%3A%2F%2Fgithub.com%2Fnexu-io%2Fopen-design%2Fissues%2F8687)) and tool-call schema mismatches. |
| `ADR-7` | Digit-preserving model normalization and surrogate `model_pk` | Preserve every dot between two ASCII digits during normalization (`normalize_model_id`), resolve within-key collisions by keeping the vendor prefix with `--`, and identify canonical models by an immutable ULID `model_pk` (`ON DELETE RESTRICT` with `model_redirects`). | Prevents merging distinct model versions (`deepseek-v4-flash` != `deepseek-v4.1-flash`) and guarantees that admin merges and splits never cascade-delete pins, windows, preferences, or response links. |
| `ADR-8` | Test governance, `xtask`, and mutation testing | Mandate Red-Green-Refactor TDD, pure-Rust `cargo xtask` (`traceability`, `doc-consistency`, `docs-bundle`) driven by `docs/srdd/requirements.toml`, and zero unannotated surviving mutants in `cargo-mutants` (`git diff --cached` locally, `origin/main...HEAD` per PR). | Guarantees that tests verify actual behavioral invariants and eliminates specification-to-phase drift. |
| `ADR-9` | Content-addressed traceability and 2-thread sealer pool | Store opt-in traced prompts as per-user content-addressed blocks (`trace_blocks`) referenced by `StoredTurnTrace v2` (`trace_records`), ordered by `(ts, request_id)`, and compressed at turn end on a fixed 2-thread `zstd::bulk::Compressor` pool (`~7 MiB` total RAM). | Eliminates quadratic multi-turn storage growth, prevents broken delta chains when turns drop or expire, and avoids the `~7 GB` RAM cost of 2,000 per-stream zstd encoders. |
| `ADR-10` | Pre-flight body budgeting and opt-in wait-timeout failover | Buffer request bodies once in shared `Bytes` under `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` (`819 MiB` on a 2 GB host, `2,000 ms` queue wait), and fail over automatically on connection/write errors and explicit HTTP rejections while keeping wait-timeout failover opt-in (`timeout_failover = false`). | Sustains high concurrency for realistic megabyte-scale coding prompts and prevents duplicate billing on slow prefills or non-streaming calls. |
| `ADR-11` | Separation of instance secrets from the rotatable master key | Store `session_hmac_secret` and `audit_hmac_key` as random 256-bit secrets sealed in `instance_secrets`, and run `llmroller rotate-master-key` as an offline resumable 500-row state machine tracked in `instance_meta`. | Guarantees that rotating `LLMROLLER_MASTER_KEY` never invalidates the `audit_log` HMAC chain or existing `session_hash` pins and recovers cleanly from mid-rotation crashes. |
| `ADR-12` | Distroless production container with built-in `healthcheck` subcommand and split Compose networks | Ship a multi-stage `Dockerfile` targeting `gcr.io/distroless/static-debian12:nonroot` (UID `65532`, read-only rootfs, `< 50 MB`) with a native `llmroller healthcheck` subcommand and segmented `edge_net` / `internal_obs_net` networks in `docker-compose.yml` (`Section 22`). | Eliminates shell and package-manager attack surfaces in production containers while supporting native Docker healthchecks, `llmroller-init` volume ownership setup, and a `runtime-dev` profile for CI integration testing (`POR-1`, `SEC-11`). |
| `ADR-12` | Distroless production container with built-in `healthcheck` subcommand and split Compose networks | Ship a multi-stage `Dockerfile` targeting `gcr.io/distroless/static-debian12:nonroot` (UID `65532`, read-only rootfs, `< 50 MB`) with a native `llmroller healthcheck` subcommand and segmented `edge_net` / `internal_obs_net` networks in `docker-compose.yml` (`Section 22`). | Eliminates shell and package-manager attack surfaces in production containers while supporting native Docker healthchecks, `llmroller-init` volume ownership setup, and a `runtime-dev` profile for CI integration testing (`POR-1`, `SEC-11`). |

### 19.2 Operational verification items for Milestone 1 (`M1`)

Four empirical checks are scheduled during Milestone 1 implementation against live upstream accounts.

| Item | Target provider or client | Verification procedure |
| --- | --- | --- |
| 1. Cache-affinity field benchmark | Command Code and Open Design AMR | Run `scripts/verify_cache_affinity.sh` sending identical 8,192-token prefixes twice under candidate headers (`x-session-affinity`, `x-session-id`) and body `prompt_cache_key`, and inspect `cached_tokens` on turn 2. |
| 2. Upstream `/v1/usage` readiness | Open Design AMR | Monitor [nexu-io/open-design#8674](https://github.com/nexu-io/open-design/issues/8674) and enable the `open_design_amr` `/usage` poller URL in `presets/open_design_amr.toml` once the upstream service endpoint ships. |
| 3. Alpha billing schema lock | Command Code (`GOAT` and `Pro`) | Capture live JSON responses from `/alpha/billing/credits` and `/alpha/usage/summary` across `GOAT` and `Pro` keys and lock them into `fixtures/usage/command_code_*.json`. |
| 4. Responses transport check | Codex CLI | Verify that current Codex CLI builds use HTTP SSE when configured with a custom `base_url` on `/v1/responses` and inherit `session_hash` across `previous_response_id` turns. |

## 20. Sources

All factual claims, endpoint paths, header names, quota windows, and tool capabilities in this specification were verified against primary sources on 19 October 2026.

### 20.1 Target model providers and upstream `/usage` APIs

The links below document the endpoints, quota windows, pricing rules, and `/usage` APIs of the four target providers.

| Source topic | Verified URL |
| --- | --- |
| OpenCode Go documentation (endpoints, models, dollar windows, `x-opencode-session`) | https://opencode.ai/docs/go |
| OpenCode Go live `/zen/go/v1/usage` quota endpoint verification and response schema | https://github.com/anomalyco/opencode/issues/43983 |
| OpenCode Go `x-opencode-session` requirement discussion | https://github.com/microsoft/vscode/issues/334186 |
| Command Code Provider API documentation (`/provider/v1/chat/completions`, `/responses`, `/messages`, `/models`) | https://commandcode.ai/docs/provider |
| Command Code GOAT plan documentation (5-hour, weekly, monthly limits, and boosted model credits) | https://commandcode.ai/docs/plans/goat |
| Command Code Pro plan documentation | https://commandcode.ai/docs/plans/pro |
| Command Code OpenAI Responses API support announcement | https://commandcode.ai/blog/command-code-now-supports-openai-responses-api |
| Command Code usage endpoints (`/alpha/billing/credits`, `/alpha/usage/summary`, `/internal/billing/credits`) and DeepSeek UTC peak schedule in `pi-commandcode-provider` | https://pi.dev/packages/pi-commandcode-provider |
| CommandCode GOAT Usagebar extension (status bar quota endpoints and 5h, 7d, monthly windows) | https://open-vsx.org/extension/berkeduruu/commandcode-goat-usagebar |
| Command Code stream truncation issue report | https://github.com/CommandCodeAI/command-code/issues/785 |
| Open Design pricing and Coding Plan model credits | https://open-design.ai/pricing/ |
| Open Design AMR API key usage endpoint feature request and maintainer confirmation (`GET /v1/usage` on `amr-link.open-design.ai`) | https://github.com/nexu-io/open-design/issues/8674 |
| Open Design AMR `/v1/messages` reasoning field report | https://github.com/nexu-io/open-design/issues/8687 |
| Open Design AMR host and `sk-amr-` key structure | https://github.com/magpie-community/plugins/pull/84 |
| Open Design terms of service | https://download.open-design.ai/terms/ |
| OpenRouter API credit and rate limits (`GET https://openrouter.ai/api/v1/key`) | https://openrouter.ai/docs/api_reference/limits |
| OpenRouter usage accounting (`usage.cost` and `cost_details` in responses) | https://openrouter.ai/docs/cookbook/administration/usage-accounting |

### 20.2 Session pinning, prompt caching, and coding agents

The links below document session affinity mechanisms across model APIs, gateways, and client coding harnesses.

| Source topic | Verified URL |
| --- | --- |
| OpenAI prompt caching and `prompt_cache_key` routing hint | https://developers.openai.com/api/docs/guides/prompt-caching |
| OpenRouter prompt caching and sticky routing (`session_id` and `x-session-id`) | https://openrouter.ai/docs/guides/best-practices/prompt-caching |
| Fireworks AI prompt caching and `x-session-affinity` | https://docs.fireworks.ai/guides/prompt-caching |
| Vercel AI Gateway session affinity headers | https://vercel.com/docs/ai-gateway/models-and-providers/session-affinity |
| LLM Gateway deterministic session routing | https://docs.llmgateway.io/features/sessions |
| Bifrost session affinity architecture | https://docs.getbifrost.ai/providers/session-affinity.md |
| Eden AI prompt caching router keys | https://www.edenai.co/docs/v3/llms/prompt-caching |
| Merge Gateway prompt caching and tracing headers | https://docs.merge.dev/merge-gateway/features/prompt-caching |
| LiteLLM Claude Code prompt cache routing | https://docs.litellm.ai/docs/tutorials/claude_code_prompt_cache_routing |
| LiteLLM `previous_response_id` routing affinity commit | https://git.kvant.cloud/phoenix/litellm-mirror/commit/043055c7b3a893e358b2e592d140f2be83823104 |
| Claude Code LLM gateway configuration guide | https://code.claude.com/docs/en/llm-gateway |
| Claude Code gateway connection guide | https://code.claude.com/docs/en/llm-gateway-connect |
| Claude Code organization gateway rollout (`x-claude-code-session-id`) | https://code.claude.com/docs/en/llm-gateway-rollout |
| NVIDIA Dynamo coding agent session ID reference (`x-claude-code-session-id`, `session-id`) | https://docs.nvidia.com/dynamo/user-guides/agents/session-i-ds |
| Codex CLI request and session capture issue | https://github.com/openai/codex/issues/24973 |
| opencode session affinity headers (`x-session-affinity`, `X-Session-Id`, `x-parent-session-id`) | https://github.com/anomalyco/opencode/issues/55243 |
| opencode prompt caching pull request | https://github.com/anomalyco/opencode/pull/35982 |
| OpenAI Streaming Chat Completions `stream_options.include_usage` cookbook | https://nbviewer.org/format/script/github/openai/openai-cookbook/blob/main/examples/How_to_stream_completions.ipynb |
| OpenAI Responses API streaming `response.completed` usage reference | https://community.openai.com/t/openai-api-get-usage-tokens-in-response-when-set-stream-true/141866/35 |
| Anthropic Messages streaming `message_start` and `message_delta` reference | https://abacus.ai/help/developer-platform/route-llm/anthropic-messages |

### 20.3 Security, testing, and prior art references

The links below cover Rust mutation testing, supply-chain security advisories, cryptographic storage guidelines, and prior gateway implementations.

| Source topic | Verified URL |
| --- | --- |
| `cargo-mutants` Rust mutation testing documentation | https://mutants.rs/ |
| `cargo-mutants` mutations vs. code coverage guide | https://mutants.rs/vs-coverage.html |
| `cargo-mutants` diff-scoped CI testing (`--in-diff`) | https://mutants.rs/in-diff.html |
| `cargo-mutants` source repository | https://github.com/sourcefrog/cargo-mutants |
| LiteLLM March 2026 PyPI supply-chain security incident advisory | https://docs.litellm.ai/blog/security-update-march-2026 |
| OWASP Password Storage Cheat Sheet (`Argon2id` parameters) | https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html |
| `prodex` Rust Codex profile pooling agent documentation | https://docs.rs/crate/prodex/0.2.133/source/AGENTS.md |
| `shift-proxy` Rust streaming body forwarding pattern | https://docs.rs/crate/shift-proxy/latest/source/src/forward.rs |
| `models.dev` public model and pricing catalog JSON | https://models.dev/api.json |
| IANA IPv4 Special-Purpose Address Registry (`SEC-7` SSRF filter ranges) | https://www.iana.org/assignments/iana-ipv4-special-registry/iana-ipv4-special-registry.xhtml |
| IANA IPv6 Special-Purpose Address Registry (`SEC-7` SSRF filter ranges) | https://www.iana.org/assignments/iana-ipv6-special-registry/iana-ipv6-special-registry.xhtml |
| Facebook `zstd` C manual (`ZSTD_estimateCStreamSize` memory sizing) | https://facebook.github.io/zstd/zstd_manual.html |

## 21. Concurrency, Lifecycle, and Failure-Recovery Model

LLMRoller coordinates high-concurrency streaming inference (`2,000` open SSE streams and `500 req/s`) and lock-free configuration reads. It also coordinates in-memory state machines and asynchronous SQLite WAL persistence in one process. This section specifies the cross-cutting concurrency invariants, lock hierarchies, RAII lifecycle guards, state-machine synchronization rules, and failure-recovery protocols that govern every crate in the workspace.

### 21.1 Per-request RAII completion guard (`RequestLifecycle`)

When a downstream client disconnects mid-stream or a Tokio task is cancelled before end-of-stream, hyper drops the response body stream immediately without polling `poll_next` again. No in-flight reservation, pre-flight body permit, probe slot, provisional pin ticket, or usage/trace record may leak or bypass accounting. To guarantee this, `crates/proxy` wraps every routed request in a `RequestLifecycle` RAII struct owned by the response stream wrapper (`API-7`, `USG-1`, `USG-3`).

```rust
pub struct RequestLifecycle {
    pub request_id: Ulid,
    pub started_ms: i64,
    pub first_frame_ms: Option<i64>,
    pub usage: UsageAccumulator,                 // observed token counts + decoded_text_bytes
    pub reservation: Option<Reservation>,        // per-candidate budget reservation guard
    pub preflight_permit: Option<OwnedSemaphorePermit>,
    pub probe_slot: Option<ProbeSlotGuard>,
    pub pin_ticket: Option<PinTicket>,
    pub trace: Option<TraceCollectorHandle>,
    pub termination: Termination,                // defaults to Termination::ClientGone
    pub emitter: UsageEmitter,                   // non-blocking queue sender
    pub finalized: bool,
}

pub enum Termination {
    Completed,
    UpstreamTruncated,
    ClientGone,
    IdleTimeout,
    ShutdownCut,
}
```

| Lifecycle event | Deterministic action taken by `RequestLifecycle` |
| --- | --- |
| First valid response frame committed (or non-streaming response accepted) | Sets `first_frame_ms = Some(now_ms)`, drops `preflight_permit` immediately to free global pre-first-byte body memory (`PERF-3`), promotes `pin_ticket` via `FirstValidFrame(ticket)` (`RTE-8`), and closes any `HalfOpen` `OriginBreaker` via `probe_slot`. |
| Normal stream completion (`[DONE]`, `response.completed`, or `message_stop`) | Sets `termination = Termination::Completed` and calls `finalize()` once before yielding the final stream poll. |
| Upstream stream closes without terminal event | Sets `termination = Termination::UpstreamTruncated` (`incomplete = 1`) and calls `finalize()`. |
| `stream_idle_timeout_ms` (`300,000 ms`) expires | Sets `termination = Termination::IdleTimeout` (`incomplete = 1`) and drops the stream, invoking `Drop::drop`. |
| Graceful shutdown drain timer (`drain_timeout_secs`) expires | Cancels the stream with `termination = Termination::ShutdownCut` (`incomplete = 1`). |
| Client TCP disconnect or future cancellation at any point | `Drop::drop` runs with `termination = Termination::ClientGone` (`incomplete = 1`). |

The `finalize()` method (invoked either explicitly or from `Drop::drop` when `!self.finalized`) runs with zero blocking I/O:
1. Sets `self.finalized = true`.
2. Computes request cost from `self.usage` using observed token counts if a usage frame arrived. Otherwise, it estimates input via `estimate_input_tokens(body)` and output via `ceil(decoded_text_bytes / 4)` (`cost_source = 'estimated_incomplete'`).
3. Posts the actual or estimated cost into the window rings and monthly accumulators **before** subtracting the estimate from `reserved_in_flight`. Thus `effective_used + reserved_in_flight` never dips below truth.
4. Enqueues the `UsageEvent` (with `termination` and `incomplete = (termination != Completed) as i32`) and any `usage_attempts` rows to the batching writer's non-blocking queues.
5. Marks any active `TraceCollectorHandle` as finished (setting `truncated_content = 1` if `termination != Completed`) and releases any remaining `preflight_permit` or `probe_slot`.

### 21.2 Single-flight `PinStore` synchronization and loser adoption

Session pins live in `crates/core::pins::PinStore` (`DashMap<PinKey, Arc<parking_lot::Mutex<PinEntry>>>`) driven by the synchronous `Clock` seam (`RTE-7`, `RTE-8`, `RTE-10`, `RTE-11`). Each `PinEntry` mutex is held only across synchronous in-memory operations (`< 5 us`, never across `.await`).

| Concurrency hazard | Synchronization invariant in `PinStore` |
| --- | --- |
| Parallel turn-1 requests for the same new session (`RTE-7`) | The first caller acquires the `PinEntry` mutex on the absent slot, runs `WrrState::select` inside the mutex, and inserts `PinState::Provisional { generation: 1, expires_ms: now + 50_000 }`. Concurrent turn-1 callers acquiring the mutex observe the non-expired `Provisional` entry and receive follower tickets `(K, generation: 1)` without re-running selection. |
| Concurrent pre-first-byte failover scatter (`PreFirstByteFailure`) | When `N` parallel turn-1 holders fail before the first valid frame on key `K1`, the first caller to acquire the `PinEntry` mutex observes `ticket.generation == entry.generation`, pushes `K1` to `tried_this_generation`, selects `K2` excluding all tried keys, and increments `entry.generation = 2`. The remaining `N - 1` callers acquire the mutex with `ticket.generation < entry.generation` and **adopt** the winner's `(K2, generation: 2)` (provided `K2` is not in their own tried set) rather than running independent WRR selections. |
| Stale confirmation after failover or admin move (`FirstValidFrame`) | `FirstValidFrame(ticket)` promotes the pin to `Confirmed` and refreshes `expires_ms = now + pin_ttl_ms` only when `ticket.generation == entry.generation` and `ticket.upstream_key_id == entry.upstream_key_id`. On generation mismatch, the pin is left untouched (`llmroller_pin_stale_confirm_total`), preventing a slow loser or pre-move request from reverting a failover winner or admin pin move. |
| Leaked provisional pins on client abort | If all holders of a provisional pin disconnect before any upstream responds, `expires_ms = created_ms + 50_000` expires after 50 s. Both `Acquire(req)` and the 60 s sweeper treat expired provisional entries as absent and increment `generation`. |
| Capacity bound (`max_active_pins = 250,000`) | Confirmed pins are never evicted by TinyLFU capacity pressure. When `PinStore.len() >= max_active_pins`, new sessions bypass insertion, route via WRR with `pin_outcome = 'unpinned_capacity'`, and increment `llmroller_pin_capacity_overflow_total`. |

### 21.3 Concurrency invariants across pluggable `RouteSelector` strategies

Routing state in `crates/core::strategy` and `crates/core::wrr` is partitioned into independent selection groups keyed by `WrrGroupKey = (user_id, model_pk, protocol, pool_tier)` (`RTE-4`, `RTE-5`, `RTE-14`, `G2`).

| Invariant | Enforcement rule |
| --- | --- |
| Tenant weight and cursor isolation | Keying by `user_id` and `pool_tier` guarantees that User B's `user_route_preferences.priority_override` or selection cursor on a shared route never alters the weights or round-robin position seen by User C. |
| Single critical section for `smooth_weighted_rr` and `weighted_rr` | Each `WrrGroup` and `ClassicalWrrGroup` is protected by one `parking_lot::Mutex` (never held across `.await`). Adding `W_eff(r)` to active candidates, picking `argmax(m.current, r.id_bytes)`, and subtracting `total = sum(W_eff)` run as one atomic critical section. |
| Unclamped zero-sum convergence (`smooth_weighted_rr`) | For every `WrrGroup`, `sum(m.current for m in group.members)` before selection equals `sum(m.current)` after selection (`0`). No artificial clamp is applied when the candidate subset shrinks during transient cooldowns. Members absent for `>= WRR_MEMBER_EVICT_MS` (`300,000 ms`) are evicted and rebalanced lazily. |
| Lock-free atomic wrap-around (`naive_rr`) | `NaiveRrGroup` increments `cursor: AtomicU64` via `fetch_add(1, Ordering::Relaxed)` and computes `(seq % (candidates.len() as u64)) as usize` over candidates sorted by `RouteId`, wrapping cleanly across `u64::MAX` with zero mutex contention. |
| Contention-free thread-local PRNG (`uniform_random`, `weighted_random`, `p2c`) | Random strategies draw from a thread-local `ChaCha12Rng` seeded from `EntropySource` (or directly from `DeterministicRng` in unit tests), avoiding any global RNG mutex across Tokio worker threads. |
| Lock-free load tracking (`power_of_two_choices`) | `RouteLoadTracker` maintains per-route `in_flight: AtomicU32` (RAII-decremented in `RequestLifecycle::drop`) and `ewma_ttfb_ms: AtomicU64` (updated via `compare_exchange_weak` on first valid frame). |
| Bounded memory and hot-reload reset | The 60 s background sweeper removes any strategy group with `now_ms - last_used_ms >= 86_400_000` (24 h), and `CacheInvalidationEvent::StrategyChanged` clears group state immediately after `ArcSwap` publication. |

### 21.4 Ordered budget reservation (`Reservation`) and decaying observations

Admission check `C8` and gate `G7` reserve estimated spend across up to seven counters (`LIM-1`, `SHR-2`, `USR-5`, `CLK-5`). These counters cover key windows, route windows, and the share, user-global, and client-key monthly accumulators.

1. Every candidate's counter list is sorted in a fixed canonical order. That order places key windows (`budget_windows.id`), route windows (`budget_windows.id`), the share accumulator, the user-global accumulator, and the client-key accumulator.
2. `try_reserve(counters, est, reservation_month)` iterates the sorted counters and runs `AtomicI64::compare_exchange` on each counter's `reserved_in_flight`, checking `effective_used(now) + reserved + est + reserve_amount <= limit`. If any counter fails, `rollback(taken)` subtracts `est` from all previously acquired counters in reverse order and returns `None`.
3. Every `Reservation` stores the `UtcYearMonth` of the reservation timestamp (`reservation_month`). A stream starting at `23:59:59.900 UTC` in month `M` and finishing in `M + 1` posts its cost and releases `reserved_in_flight` in month `M`.
4. Upstream `/usage` polls record `UpstreamObservation { used_micro_usd, observed_at_ms: t0, local_at_observation, resets_at_ms }` where `t0` and `local_at_observation` are captured **before** sending the HTTP request. `effective_used(w, now)` returns `max(local, o.used_micro_usd + max(0, local - o.local_at_observation))` while `now < min(o.resets_at_ms.unwrap_or(i64::MAX), o.observed_at_ms + 1_800_000)`, and falls back automatically to `local` once the observation expires (30 minutes or `resets_at_ms`).

### 21.5 Versioned health synchronization and origin circuit breakers

Runtime health (`upstream_keys.health` and `model_routes.route_health`) is authoritative in `RuntimeState` (`DashMap`) while the server runs and persisted write-behind to SQLite for startup preload (`REL-5`, `Section 10.5`).

| Component | State synchronization contract |
| --- | --- |
| Monotonic `health_version` | Every key and route in `RuntimeState` carries a monotonic `health_version: u64`. Transitions to `exhausted`, `invalid`, or back to `healthy` increment `health_version` in memory and enqueue `HealthWrite { id, state, until_ms, version }` on the `durable_state` queue. The SQLite writer runs `UPDATE ... WHERE id = ? AND health_version < ?`, guaranteeing that a control-plane `reset-health` (which bumps `health_version` in memory immediately) can never be overwritten by an older queued data-plane write. Transient `cooling_down` transitions remain in memory only. |
| Two-key / probe credential invalidation | `CredentialInvalid` responses (`401` or `403` matching `credential_invalid_substrings`) record `(client_key_id, now_ms)` in a 10-minute sliding strike map per upstream key. A key transitions to `invalid` only when `strikes[K].len() >= 2` (two distinct client keys within 10 minutes) or when a single-flight verification probe (`GET {base_url}{models_path}`, 5 s timeout) returns `401`/`403` matching `CredentialInvalid`. Any `2xx` response clears `strikes[K]`. |
| Per-origin `OriginBreaker` | Circuit breakers are keyed by `OriginKey = (scheme, host, port, allow_private_network)` (`REL-5`). Only `TransportError::{Dns, Connect, Tls, WriteStalled}` and HTTP `502`/`503`/`504` before the first valid frame count as host failures. `Closed -> Open` triggers when `consecutive_host_failures >= 5` or when 60 s sliding window requests `>= 20` and `host_failures / requests >= 0.5`. In `HalfOpen`, a `ProbeSlotGuard` (RAII) releases `probe_in_flight = false` if the probe request disconnects before receiving an outcome; any HTTP response from the origin closes the breaker. |

### 21.6 Dual-priority SQLite writer, three-queue overflow, and single `Reloader`

All SQLite mutations flow through a single writer task (`crates/store::writer`) paired with a single snapshot reloader task (`crates/server::reloader`) (`REL-2`, `REL-3`, `REL-4`).

#### 21.6.1 Writer queue priority and byte bounds

The writer polls two command channels and three data-plane queues:
1. `control_rx` (`WriterCommand::Control { tx, reply }`): high-priority channel for control-plane transactions. The writer drains **all** pending `control_rx` commands before every data-plane batch so user/admin actions (`CLK-4`, `SHR-5`) never block behind data-plane load. Control handlers await `reply` with a `2,000 ms` timeout (`503 writer_busy`).
2. `durable_state` (`Mutex<IndexMap<DurableKey, DurableOp>>`, max `500,000` keys): non-dropping coalescing queue for confirmed pins, `response_links`, and `HealthWrite` items. If `len() >= 500,000`, new pin confirmations remain in memory only (`llmroller_durable_queue_full_total`) and `/readyz` returns HTTP `503`.
3. `rollups` (`Mutex<HashMap<RollupKey, RollupDelta>>`): non-dropping additive coalescing map for `usage_rollups_5min` and `usage_rollups_hourly`.
4. `raw_events` (`Mutex<VecDeque<RawItem>>`, max `10,000` items and `64 MiB`): bounded ring for `usage_events`, `usage_attempts`, `trace_blocks`, and `trace_records`. When full, the oldest item is popped and dropped (its token/cost totals are already folded into `rollups`) and `llmroller_usage_dropped_total{kind}` increments.
5. Each data-plane batch flushes at `200` items, `4 MiB` of serialized row bytes (`writer_batch_max_bytes`), or `250 ms` (`writer_batch_max_age_ms`). Any `trace_records` item `> 1 MiB` commits in its own dedicated transaction.

#### 21.6.2 Versioned `ConfigSnapshot` publication (`Reloader`)

To prevent concurrent snapshot reloads from publishing an older snapshot over a newer one or purging pins before the new configuration is visible:
1. Every `Control` transaction that modifies configuration tables increments `instance_meta.config_version = config_version + 1` inside the same `BEGIN IMMEDIATE` transaction.
2. After `COMMIT`, the writer sends `ReloadRequest { min_version, changed_domains, invalidations }` to the single `Reloader` task.
3. The `Reloader` drains and coalesces all pending `ReloadRequest` messages (`max(min_version)` and union of `changed_domains` and `invalidations`). It then opens one read transaction, reads `v = SELECT config_version FROM instance_meta`, and rebuilds only the `changed_domains` slices.
4. If `v > published.version`, the `Reloader` calls `arc_swap.store(Arc::new(snapshot))` and **then** applies all `CacheInvalidationEvent`s (`PinStore` purges, `WrrState` resets, `RuntimeState` updates) before notifying waiting control handlers (`timeout 1 s`).

#### 21.6.3 WAL checkpointing, paged reads, and `SQLITE_FULL` recovery

1. Migration `0001` sets `PRAGMA auto_vacuum = INCREMENTAL` before creating any table (verified at startup: `PRAGMA auto_vacuum == 2`).
2. A background checkpoint task runs `PRAGMA wal_autocheckpoint = 1000` and issues `PRAGMA wal_checkpoint(PASSIVE)` every `30 s`. If the `-wal` file exceeds `256 MiB`, the writer issues `PRAGMA wal_checkpoint(RESTART)` between batches; if it exceeds `1 GiB`, `/readyz` transitions to HTTP `503` and `llmroller_wal_bytes` alerts fire.
3. Every read connection sets `PRAGMA query_only = ON`. Chat preview (`<= 50` turns) and usage export (`<= 5,000` rows) read each page in a short transaction and decrypt after closing it.
4. On `SQLITE_FULL`, the writer stops accepting `raw_events` (dropping with metric), retries `durable_state` and `rollups` commits every `5 s`, sets `/readyz` to HTTP `503`, and sets `llmroller_storage_full = 1`.

### 21.7 `ActorScope` isolation for human and system actors

Every repository method in `crates/store` takes `&ActorScope` (`SEC-17`). To represent background tasks and CLI commands without fabricating a synthetic admin user ID, `ActorScope` is a two-variant enum:

```rust
pub enum ActorScope {
    User { user_id: UserId, role: Role },
    System { task: SystemTask },
}

pub enum SystemTask {
    Writer,
    Reloader,
    Discovery,
    UsagePoller,
    RetentionSweeper,
    DataPlaneUsage,
    Cli(CliCommand),
}
```

`ActorScope::System` can only be constructed inside `crates/server` and `crates/store` (`pub(crate)` builder in `crates/core`, verified by `tests/workspace_boundaries.rs` so `crates/admin` handlers can never construct a `System` scope). Audit rows written under `ActorScope::System` record `actor_user_id = NULL` and `detail_json.system_task`.

### 21.8 Ordered startup, graceful shutdown, and background supervision

Process startup, shutdown draining, and background job supervision follow strict deterministic sequences.

#### 21.8.1 Ordered startup sequence (`llmroller serve`)

`llmroller serve` runs ten ordered steps before setting `/readyz` to HTTP `200`:
1. Acquire `flock(LOCK_EX | LOCK_NB)` on `${LLMROLLER_DATA_DIR}/llmroller.lock` (exit `4` on contention).
2. Load and validate the 32-byte master key from `LLMROLLER_MASTER_KEY_FILE` (rejecting group/other permissions with exit `3`) or `LLMROLLER_MASTER_KEY` (logging a one-time `/proc/<pid>/environ` warning).
3. Validate listener TLS rules: exit `2` if the control-plane listener is non-loopback without TLS or `LLMROLLER_TRUSTED_PROXIES`; exit `2` (when `LLMROLLER_REQUIRE_TLS=true`) or log a recurring 10-minute `error` (`llmroller_plaintext_data_plane = 1`) if the data-plane listener is non-loopback plaintext.
4. Open SQLite via `connect_options`, verify `_llmroller_schema(version)` (exit `14` if newer than binary), run forward-only migrations (exit `13` on failure), and assert `PRAGMA auto_vacuum == 2`.
5. Verify `instance_meta.rotation_state == 'idle'`, `LLMROLLER_MASTER_KEY_ID == active_master_key_id`, and unseal `instance_secrets` (`session_hmac_secret` and `audit_hmac_key`) (exit `3` on any mismatch or AEAD failure).
6. Load the initial `ConfigSnapshot` into `ArcSwap`.
7. Rebuild all rolling and anchored budget windows and UTC calendar-month accumulators from `usage_rollups_5min`, `budget_adjustments`, and `upstream_usage_snapshots` (`< 2 s`, `PERF-4`).
8. Preload up to `max_active_pins` (`250,000`) non-expired confirmed `pins` (`ORDER BY last_used_at DESC`), `response_links`, and `upstream_keys.health` (`cooling_down` normalized to `healthy`) into memory.
9. Spawn the writer actor, `Reloader` task, and the 2-thread OS trace sealer pool, then bind the three TCP listeners with `/readyz` returning HTTP `503`.
10. Spawn supervised background jobs (`Discovery`, `UsagePoller`, `RetentionSweeper`, `WalCheckpointer`) and switch `/readyz` to HTTP `200`.

#### 21.8.2 Eight-step graceful shutdown sequence (`SIGTERM` / `SIGINT`)

Upon receiving `SIGTERM` or `SIGINT`, the server drains in eight time-bounded steps.

| Step | Action taken | Time bound |
| --- | --- | --- |
| 1 | Switch `/readyz` to HTTP `503` while continuing to accept connections for `drain_grace_secs` (default `5 s`) so load balancers observe readiness withdrawal. | `5 s` |
| 2 | Stop accepting new TCP connections on all listeners. | Immediate |
| 3 | Wait for in-flight HTTP requests and SSE streams to complete naturally. | `drain_timeout_secs` (`60 s`) |
| 4 | Cancel any remaining SSE streams with `Termination::ShutdownCut` so `RequestLifecycle::drop` finalizes their usage and releases reservations. | Immediate |
| 5 | Stop background jobs (`UsagePoller`, `Discovery`, `RetentionSweeper`, `WalCheckpointer`). | `5 s` |
| 6 | Close the `crossbeam_channel` sealer input queue and wait for the 2 sealer OS threads to finish in-flight jobs (dropping any remaining backlog with metrics on timeout). | `min(10 s, remaining_drain)` |
| 7 | Drain `Control`, `durable_state`, `rollups`, and `raw_events` queues in the SQLite writer in strict priority order. | `15 s` |
| 8 | Run `PRAGMA wal_checkpoint(TRUNCATE)`, close SQLite connections, and release `llmroller.lock`. | Exit `0` (or `11` if any queue timed out in Step 7) |

#### 21.8.3 Background job supervision and writer panic isolation

Stateless background jobs (`Discovery`, `UsagePoller`, `RetentionSweeper`, `WalCheckpointer`) run inside tasks wrapped with `FutureExt::catch_unwind`. On panic, the supervisor logs the panic payload (redacted per `SEC-6`) and restarts the job with exponential backoff (`1 s, 2 s, 4 s, ...` capped at `300 s`). Three panics within 10 minutes mark the job `failed`, set `llmroller_job_failed{job} = 1`, and leave the job stopped until operator intervention.

The **SQLite writer actor is never restarted** after a panic because an interrupted batch could desynchronize in-memory queues. Instead, a writer panic sets `/readyz` to HTTP `503` and triggers controlled shutdown with exit code `12`.

## 22. Deployment and containerization specification

This section defines the containerization architecture, orchestration topologies, volume and network boundaries, and environment contracts for LLMRoller (`POR-1`, `SEC-9`, `SEC-11`, `REL-1`). Every port, path, timeout, and environment variable matches Sections 5, 7, 8, 9, 10, and 21 with zero drift.

### 22.1 Multi-stage `Dockerfile` and `.dockerignore` architecture

LLMRoller builds from a single multi-stage `Dockerfile` using BuildKit (`# syntax=docker/dockerfile:1.7`). The build pipeline uses four stages: `planner`, `builder`, `runtime-distroless` (production), and `runtime-dev` (development and CI).

#### 22.1.1 Build stage responsibilities and security invariants

The table below defines the four stages in the `Dockerfile` and the security controls enforced on the final image (`POR-1`, `SEC-10`, `SEC-11`).

| Stage name | Base image (pinned tag & digest) | Purpose and key invariants |
| --- | --- | --- |
| `planner` | `rust:1.85.0-bookworm@sha256:632f37c2d09c9b6447a9d35b17e64940f97a461b91e04b815b7556795f2e90b2` | Installs `cargo-chef` (`0.1.71`) and computes `recipe.json` from `Cargo.toml` and `Cargo.lock` so source edits in `crates/*` never invalidate cached third-party crate compilations. |
| `builder` | `rust:1.85.0-bookworm@sha256:632f37c2d09c9b6447a9d35b17e64940f97a461b91e04b815b7556795f2e90b2` | Compiles cached dependencies via `cargo chef cook --release --locked --target <musl_target>`, verifies `cargo xtask doc-consistency`, compiles the static `llmroller` binary (`--locked`, stripped of debug symbols), and prepares empty `/var/lib/llmroller` and `/etc/llmroller/presets` directories owned by `65532:65532`. |
| `runtime-distroless` (default production target) | `gcr.io/distroless/static-debian12:nonroot@sha256:e8a4044e0b4ae4257efa45fc026c0bc30ad320d43bd4c1a7d5271bd241e386d0` | Copies only `/usr/local/bin/llmroller` and `/etc/llmroller/presets/*.toml`. Runs as non-root `65532:65532` (`nonroot`), contains zero shell (`/bin/sh` absent) and zero package manager, keeps total uncompressed image size `< 50 MB` (`POR-1`), and uses `llmroller healthcheck` for native container probes. |
| `runtime-dev` (development & CI target) | `debian:bookworm-slim@sha256:f06537653ac770703bc45b4b113475bd402f451e85223f0f2837acbf89ab020a` | Installs `tini`, `ca-certificates`, `curl`, and `sqlite3`, creates non-root user `llmroller` (`65532:65532`), and runs under `/usr/bin/tini -- /usr/local/bin/llmroller` for interactive debugging and Compose smoke testing. |

#### 22.1.2 Complete `.dockerignore` reference manifest

The `.dockerignore` file excludes build artifacts, local SQLite databases, git history, and secret files (`SEC-11`). This keeps the build context under 2 MiB and prevents credential leaks into layers.

```dockerignore
# Build outputs and caches
target/
**/target/
*.profraw
lcov.info

# Local runtime state, SQLite WAL files, and secrets
/var/
*.db
*.db-wal
*.db-shm
*.lock
setup-token
.env
.env.*
!.env.example
*.pem
*.key
secrets/

# Version control, IDE state, and test artifacts
.git/
.github/
.vscode/
.idea/
artifacts/
tmp/
*.log
```

#### 22.1.3 Complete multi-stage `Dockerfile` reference manifest

The `Dockerfile` below is the normative container build manifest for `x86_64` (`x86_64-unknown-linux-musl`) and `aarch64` (`aarch64-unknown-linux-musl`) builds.

```dockerfile
# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=rust:1.85.0-bookworm@sha256:632f37c2d09c9b6447a9d35b17e64940f97a461b91e04b815b7556795f2e90b2
ARG DISTROLESS_IMAGE=gcr.io/distroless/static-debian12:nonroot@sha256:e8a4044e0b4ae4257efa45fc026c0bc30ad320d43bd4c1a7d5271bd241e386d0
ARG DEBIAN_SLIM_IMAGE=debian:bookworm-slim@sha256:f06537653ac770703bc45b4b113475bd402f451e85223f0f2837acbf89ab020a

# -----------------------------------------------------------------------------
# Stage 1: planner - compute deterministic cargo-chef dependency recipe
# -----------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS planner
WORKDIR /src
RUN cargo install cargo-chef --locked --version 0.1.71
COPY Cargo.toml Cargo.lock deny.toml ./
COPY xtask/Cargo.toml xtask/Cargo.toml
COPY crates/core/Cargo.toml crates/core/Cargo.toml
COPY crates/proto/Cargo.toml crates/proto/Cargo.toml
COPY crates/store/Cargo.toml crates/store/Cargo.toml
COPY crates/proxy/Cargo.toml crates/proxy/Cargo.toml
COPY crates/admin/Cargo.toml crates/admin/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
RUN mkdir -p xtask/src crates/core/src crates/proto/src crates/store/src              crates/proxy/src crates/admin/src crates/server/src     && touch xtask/src/main.rs crates/core/src/lib.rs crates/proto/src/lib.rs              crates/store/src/lib.rs crates/proxy/src/lib.rs              crates/admin/src/lib.rs crates/server/src/main.rs     && cargo chef prepare --recipe-path recipe.json

# -----------------------------------------------------------------------------
# Stage 2: builder - compile static musl release binary with embedded UI assets
# -----------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS builder
ARG TARGETARCH
ARG GIT_SHA=unknown
WORKDIR /src

RUN apt-get update && apt-get install -y --no-install-recommends         musl-tools pkg-config clang llvm lld     && rm -rf /var/lib/apt/lists/*     && cargo install cargo-chef --locked --version 0.1.71

RUN case "${TARGETARCH:-amd64}" in       amd64) echo "x86_64-unknown-linux-musl" > /tmp/rust_target ;;       arm64) echo "aarch64-unknown-linux-musl" > /tmp/rust_target ;;       *) echo "Unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;;     esac     && rustup target add "$(cat /tmp/rust_target)"

COPY --from=planner /src/recipe.json recipe.json
RUN TARGET_TRIPLE="$(cat /tmp/rust_target)"     && cargo chef cook --release --locked --target "${TARGET_TRIPLE}" --recipe-path recipe.json

COPY Cargo.toml Cargo.lock deny.toml ./
COPY presets ./presets
COPY docs ./docs
COPY fixtures ./fixtures
COPY xtask ./xtask
COPY crates ./crates

ENV LLMROLLER_BUILD_GIT_SHA=${GIT_SHA}
RUN TARGET_TRIPLE="$(cat /tmp/rust_target)"     && cargo build --release --locked --target "${TARGET_TRIPLE}" --bin llmroller     && cp "target/${TARGET_TRIPLE}/release/llmroller" /usr/local/bin/llmroller     && strip --strip-debug /usr/local/bin/llmroller     && install -d -m 0750 -o 65532 -g 65532 /staging/var/lib/llmroller     && install -d -m 0755 -o 65532 -g 65532 /staging/etc/llmroller/presets     && install -m 0644 -o 65532 -g 65532 presets/*.toml /staging/etc/llmroller/presets/

# -----------------------------------------------------------------------------
# Stage 3: runtime-dev - instrumented image for local Compose & CI smoke tests
# -----------------------------------------------------------------------------
FROM ${DEBIAN_SLIM_IMAGE} AS runtime-dev
RUN apt-get update && apt-get install -y --no-install-recommends         ca-certificates curl sqlite3 tini     && rm -rf /var/lib/apt/lists/*     && groupadd -g 65532 llmroller     && useradd -u 65532 -g 65532 -d /var/lib/llmroller -s /usr/sbin/nologin -M llmroller

COPY --from=builder --chown=65532:65532 /staging/var/lib/llmroller /var/lib/llmroller
COPY --from=builder --chown=65532:65532 /staging/etc/llmroller /etc/llmroller
COPY --from=builder /usr/local/bin/llmroller /usr/local/bin/llmroller

ENV LLMROLLER_DATA_DIR=/var/lib/llmroller     LLMROLLER_LISTEN=0.0.0.0:8080     LLMROLLER_ADMIN_LISTEN=0.0.0.0:8081     LLMROLLER_METRICS_LISTEN=0.0.0.0:9090     LLMROLLER_DEFAULT_ROUTING_STRATEGY=smooth_weighted_rr     LLMROLLER_LOG=info,llmroller=info

EXPOSE 8080 8081 9090
USER 65532:65532
WORKDIR /var/lib/llmroller

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3   CMD ["/usr/local/bin/llmroller", "healthcheck", "--url", "http://127.0.0.1:8080/readyz"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/llmroller"]
CMD ["serve"]

# -----------------------------------------------------------------------------
# Stage 4: runtime-distroless - default hardened production image (< 50 MB)
# -----------------------------------------------------------------------------
FROM ${DISTROLESS_IMAGE} AS runtime-distroless

COPY --from=builder --chown=65532:65532 /staging/var/lib/llmroller /var/lib/llmroller
COPY --from=builder --chown=65532:65532 /staging/etc/llmroller /etc/llmroller
COPY --from=builder /usr/local/bin/llmroller /usr/local/bin/llmroller

ENV LLMROLLER_DATA_DIR=/var/lib/llmroller     LLMROLLER_LISTEN=0.0.0.0:8080     LLMROLLER_ADMIN_LISTEN=0.0.0.0:8081     LLMROLLER_METRICS_LISTEN=0.0.0.0:9090     LLMROLLER_DEFAULT_ROUTING_STRATEGY=smooth_weighted_rr     LLMROLLER_LOG=info,llmroller=info

EXPOSE 8080 8081 9090
USER 65532:65532
WORKDIR /var/lib/llmroller

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3   CMD ["/usr/local/bin/llmroller", "healthcheck", "--url", "http://127.0.0.1:8080/readyz"]

ENTRYPOINT ["/usr/local/bin/llmroller"]
CMD ["serve"]
```

### 22.2 Orchestration and `docker-compose.yml` topology

LLMRoller runs as a stateful service with a persistent SQLite WAL volume, a volume initializer (`llmroller-init`), and a `caddy` TLS proxy. Optional Compose profiles add Prometheus scraping (`--profile observability`) and a local development harness (`--profile dev`).

#### 22.2.1 Network segmentation, volumes, and security constraints

The Compose topology enforces four isolation boundaries:
1. Network segmentation: `edge_net` (`172.28.1.0/24`) connects `caddy` (`172.28.1.10`) to `llmroller` (`172.28.1.20`) on ports `8080` (data plane) and `8081` (control plane), with `LLMROLLER_TRUSTED_PROXIES=172.28.1.10/32` satisfying `SEC-5` and `SEC-9`. `internal_obs_net` (`172.28.2.0/24`, `internal: true`) connects `prometheus` to `llmroller:9090` with zero external ingress or egress.
2. Persistent volumes and `tmpfs`: `llmroller_data` mounts at `/var/lib/llmroller` and `llmroller_backups` mounts at `/var/backups/llmroller`. Because SQLite WAL uses `mmap` in `llmroller.db-shm`, `llmroller_data` must reside on a local POSIX filesystem (ext4, XFS, or ZFS). A `64 MiB` `tmpfs` (`noexec,nosuid,nodev`) mounts at `/tmp`.
3. Non-root volume initialization (`llmroller-init`): Fresh Docker volumes default to `root:root` (`0755`), blocking UID `65532` from creating `llmroller.lock`. The one-shot `llmroller-init` container runs before `llmroller` to set `65532:65532` and `0750` on `/var/lib/llmroller` and `/var/backups/llmroller`.
4. Hardened container runtime (`SEC-11`): `llmroller` sets `user: "65532:65532"`, `read_only: true`, `cap_drop: ["ALL"]`, `security_opt: ["no-new-privileges:true"]`, `pids_limit: 512`, and `mem_limit: 2048m`. It sets `stop_grace_period: 90s`, which covers the 85 s drain budget in Section 21.8.2.

#### 22.2.2 Complete `docker-compose.yml` reference manifest

The `docker-compose.yml` manifest below defines the production, observability, and local development topologies.

```yaml
name: llmroller

services:
  llmroller-init:
    image: debian:bookworm-slim@sha256:f06537653ac770703bc45b4b113475bd402f451e85223f0f2837acbf89ab020a
    user: "0:0"
    read_only: true
    security_opt:
      - no-new-privileges:true
    cap_drop:
      - ALL
    cap_add:
      - CHOWN
      - FOWNER
      - DAC_OVERRIDE
    volumes:
      - llmroller_data:/var/lib/llmroller
      - llmroller_backups:/var/backups/llmroller
    entrypoint:
      - /bin/sh
      - -ec
      - |
        chown 65532:65532 /var/lib/llmroller /var/backups/llmroller
        chmod 0750 /var/lib/llmroller /var/backups/llmroller
        if [ -f /var/lib/llmroller/llmroller.db ]; then
          chown 65532:65532 /var/lib/llmroller/llmroller.db*
        fi
    restart: "no"

  llmroller:
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime-distroless
      args:
        GIT_SHA: ${GIT_SHA:-v1.0.0}
    image: llmroller:1.0.0-distroless
    user: "65532:65532"
    read_only: true
    init: false
    stop_signal: SIGTERM
    stop_grace_period: 90s
    security_opt:
      - no-new-privileges:true
    cap_drop:
      - ALL
    cpus: 2.0
    mem_limit: 2048m
    mem_reservation: 512m
    memswap_limit: 2048m
    pids_limit: 512
    ulimits:
      nofile:
        soft: 65536
        hard: 65536
    depends_on:
      llmroller-init:
        condition: service_completed_successfully
    environment:
      LLMROLLER_MASTER_KEY_FILE: /run/secrets/llmroller_master_key
      LLMROLLER_MASTER_KEY_ID: ${LLMROLLER_MASTER_KEY_ID:-mk_2026_10}
      LLMROLLER_DATA_DIR: /var/lib/llmroller
      LLMROLLER_LISTEN: 0.0.0.0:8080
      LLMROLLER_ADMIN_LISTEN: 0.0.0.0:8081
      LLMROLLER_METRICS_LISTEN: 0.0.0.0:9090
      LLMROLLER_TRUSTED_PROXIES: 172.28.1.10/32
      LLMROLLER_REQUIRE_TLS: ${LLMROLLER_REQUIRE_TLS:-true}
      LLMROLLER_SETUP_TOKEN_REQUIRED: ${LLMROLLER_SETUP_TOKEN_REQUIRED:-true}
      LLMROLLER_MAX_BODY_MB: ${LLMROLLER_MAX_BODY_MB:-32}
      LLMROLLER_PREFLIGHT_BODY_BUDGET_MB: ${LLMROLLER_PREFLIGHT_BODY_BUDGET_MB:-819}
      LLMROLLER_MEMORY_LIMIT_MB: 2048
      LLMROLLER_SQLITE_SYNCHRONOUS: ${LLMROLLER_SQLITE_SYNCHRONOUS:-NORMAL}
      LLMROLLER_DEFAULT_ROUTING_STRATEGY: ${LLMROLLER_DEFAULT_ROUTING_STRATEGY:-smooth_weighted_rr}
      LLMROLLER_LOG: ${LLMROLLER_LOG:-info,llmroller=info}
    secrets:
      - source: llmroller_master_key
        target: /run/secrets/llmroller_master_key
        uid: "65532"
        gid: "65532"
        mode: 0400
    volumes:
      - llmroller_data:/var/lib/llmroller
      - llmroller_backups:/var/backups/llmroller
      - type: tmpfs
        target: /tmp
        tmpfs:
          size: 67108864
          mode: 01777
    networks:
      edge_net:
        ipv4_address: 172.28.1.20
      internal_obs_net:
        ipv4_address: 172.28.2.20
    healthcheck:
      test: ["CMD", "/usr/local/bin/llmroller", "healthcheck", "--url", "http://127.0.0.1:8080/readyz"]
      interval: 10s
      timeout: 3s
      retries: 3
      start_period: 10s
    logging:
      driver: json-file
      options:
        max-size: "50m"
        max-file: "5"
    restart: unless-stopped

  caddy:
    image: caddy:2.9.1-alpine@sha256:663c83823d28073681e2c5f9b86c718193d689b8d9a22325f206f4e4596f7b0b
    depends_on:
      llmroller:
        condition: service_healthy
    security_opt:
      - no-new-privileges:true
    cap_drop:
      - ALL
    cap_add:
      - NET_BIND_SERVICE
    ports:
      - "${LLMROLLER_PUBLIC_HTTPS_PORT:-443}:443"
      - "${LLMROLLER_ADMIN_HTTPS_PORT:-8443}:8443"
    environment:
      LLMROLLER_DOMAIN: ${LLMROLLER_DOMAIN:-llmroller.internal.example.com}
    volumes:
      - ./deploy/Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy_data:/data
      - caddy_config:/config
    networks:
      edge_net:
        ipv4_address: 172.28.1.10
    healthcheck:
      test: ["CMD", "caddy", "version"]
      interval: 15s
      timeout: 3s
      retries: 3
    logging:
      driver: json-file
      options:
        max-size: "25m"
        max-file: "3"
    restart: unless-stopped

  prometheus:
    profiles: ["observability"]
    image: prom/prometheus:v3.1.0@sha256:36259373f5b247829e3a1e4736c81f53e7b6a15c26149b176c6371a79d1e04b7
    user: "65534:65534"
    read_only: true
    security_opt:
      - no-new-privileges:true
    cap_drop:
      - ALL
    depends_on:
      llmroller:
        condition: service_healthy
    command:
      - "--config.file=/etc/prometheus/prometheus.yml"
      - "--storage.tsdb.path=/prometheus"
      - "--storage.tsdb.retention.time=15d"
    volumes:
      - ./deploy/prometheus.yml:/etc/prometheus/prometheus.yml:ro
      - prometheus_data:/prometheus
    networks:
      internal_obs_net:
        ipv4_address: 172.28.2.30
    restart: unless-stopped

  mock-upstream:
    profiles: ["dev"]
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime-dev
    command: ["serve", "--listen", "0.0.0.0:18080"]
    networks:
      edge_net:
        ipv4_address: 172.28.1.50

  llmroller-dev:
    profiles: ["dev"]
    build:
      context: .
      dockerfile: Dockerfile
      target: runtime-dev
    user: "65532:65532"
    stop_signal: SIGTERM
    stop_grace_period: 30s
    depends_on:
      llmroller-init:
        condition: service_completed_successfully
    environment:
      LLMROLLER_MASTER_KEY: ${LLMROLLER_DEV_MASTER_KEY:-MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=}
      LLMROLLER_MASTER_KEY_ID: mk_dev
      LLMROLLER_DATA_DIR: /var/lib/llmroller
      LLMROLLER_LISTEN: 0.0.0.0:8080
      LLMROLLER_ADMIN_LISTEN: 0.0.0.0:8081
      LLMROLLER_METRICS_LISTEN: 0.0.0.0:9090
      LLMROLLER_TRUSTED_PROXIES: 127.0.0.1/32,172.28.1.0/24
      LLMROLLER_REQUIRE_TLS: "false"
      LLMROLLER_SETUP_TOKEN_REQUIRED: "false"
      LLMROLLER_DEFAULT_ROUTING_STRATEGY: ${LLMROLLER_DEFAULT_ROUTING_STRATEGY:-smooth_weighted_rr}
      LLMROLLER_LOG: debug,llmroller=trace
    command: ["serve", "--dev-insecure-cookies"]
    ports:
      - "127.0.0.1:8080:8080"
      - "127.0.0.1:8081:8081"
      - "127.0.0.1:9090:9090"
    volumes:
      - llmroller_data:/var/lib/llmroller
      - type: tmpfs
        target: /tmp
    networks:
      - edge_net
      - internal_obs_net

networks:
  edge_net:
    driver: bridge
    ipam:
      config:
        - subnet: 172.28.1.0/24
  internal_obs_net:
    driver: bridge
    internal: true
    ipam:
      config:
        - subnet: 172.28.2.0/24

volumes:
  llmroller_data:
  llmroller_backups:
  caddy_data:
  caddy_config:
  prometheus_data:

secrets:
  llmroller_master_key:
    file: ${LLMROLLER_MASTER_KEY_SECRET_PATH:-./secrets/master.key}
```

#### 22.2.3 Companion reverse-proxy (`deploy/Caddyfile`) and Prometheus (`deploy/prometheus.yml`) manifests

Upstream reverse proxies must disable response buffering (`flush_interval -1`) so SSE token chunks reach coding agents within `1 ms` (`PERF-2`).

```caddyfile
# deploy/Caddyfile
{$LLMROLLER_DOMAIN:llmroller.internal.example.com}:443 {
    reverse_proxy 172.28.1.20:8080 {
        flush_interval -1
        header_up X-Forwarded-Proto {scheme}
    }
}

{$LLMROLLER_DOMAIN:llmroller.internal.example.com}:8443 {
    reverse_proxy 172.28.1.20:8081 {
        flush_interval -1
        header_up X-Forwarded-Proto {scheme}
    }
}
```

```yaml
# deploy/prometheus.yml
global:
  scrape_interval: 15s
  evaluation_interval: 15s

scrape_configs:
  - job_name: "llmroller"
    metrics_path: "/metrics"
    static_configs:
      - targets: ["172.28.2.20:9090"]
```

### 22.3 Environment, secrets, and entrypoint contract

LLMRoller validates all environment variables at startup before opening network listeners. Invalid values cause an immediate exit with code `2` (or code `3` for master-key errors).

#### 22.3.1 Complete environment variable schema

The table below lists all supported environment variables, their defaults, and their validation rules.

| Variable name | Type | Default | Mandatory / optional | Description and validation rule |
| --- | --- | --- | --- | --- |
| `LLMROLLER_MASTER_KEY_FILE` | Path | `None` | Mandatory (unless `LLMROLLER_MASTER_KEY` is set) | Path to a `0400`/`0600` file containing the base64-encoded 32-byte master key (`/run/secrets/llmroller_master_key` in Docker). Group or world permission bits cause exit `3` (`SEC-2`). |
| `LLMROLLER_MASTER_KEY` | Base64 string | `None` | Optional fallback | Raw base64 master key; must decode to **exactly** 32 bytes (exit `3` otherwise). Logs a one-time `/proc/<pid>/environ` warning at startup (`SEC-2`). |
| `LLMROLLER_MASTER_KEY_ID` | ASCII string | `"mk_default"` | Optional | Key identifier `^[A-Za-z0-9_.-]{1,64}$` verified against `instance_meta.active_master_key_id` at startup (exit `3` on mismatch, `SEC-1`). |
| `LLMROLLER_DATA_DIR` | Path | `"/var/lib/llmroller"` | Optional | Writable directory holding `llmroller.db`, `-wal`, `-shm`, `llmroller.lock`, and `setup-token`. |
| `LLMROLLER_LISTEN` | SocketAddr | `"0.0.0.0:8080"` | Optional | Data-plane bind address serving `/v1/*`, `/healthz`, and `/readyz` (exit `5` on bind error). |
| `LLMROLLER_ADMIN_LISTEN` | SocketAddr | `"127.0.0.1:8081"` | Optional | Control-plane bind address serving `/admin/*`, `/app/*`, `/api/*`, `/setup`, and `/login`. Non-loopback without TLS or `LLMROLLER_TRUSTED_PROXIES` exits with code `2` (`SEC-5`). |
| `LLMROLLER_METRICS_LISTEN` | SocketAddr | `"127.0.0.1:9090"` | Optional | Prometheus `/metrics` listener bind address (`OBS-1`, `SEC-9`). |
| `LLMROLLER_TRUSTED_PROXIES` | CIDR CSV | `""` | Optional | Comma-separated IPv4/IPv6 CIDRs trusted to supply `X-Forwarded-For` and `X-Forwarded-Proto` (`172.28.1.10/32` in Compose, `SEC-12`). |
| `LLMROLLER_REQUIRE_TLS` | Bool | `false` (`true` in prod Compose) | Optional | When `true`, refuses non-loopback `LLMROLLER_LISTEN` without TLS or `LLMROLLER_TRUSTED_PROXIES` (exit `2`, `SEC-9`). |
| `LLMROLLER_SETUP_TOKEN` | ASCII string | Auto-generated file | Optional | Explicit bootstrap setup token (`>= 16` chars); otherwise generated to `${LLMROLLER_DATA_DIR}/setup-token` (`0600`, `AUTH-2`). |
| `LLMROLLER_SETUP_TOKEN_REQUIRED` | Bool | `true` | Optional | When `false`, skips setup-token verification on `POST /admin/api/setup` (`AUTH-2`). |
| `LLMROLLER_MAX_BODY_MB` | Integer | `32` | Optional | Maximum inbound request body in MiB (`1..=128` and `<= LLMROLLER_PREFLIGHT_BODY_BUDGET_MB / 4`, exit `2` otherwise, `SEC-8`). |
| `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` | Integer | `min(1024, floor(0.40 * mem_mb))` | Optional | Total MiB of buffered request bodies permitted concurrently before first valid frame (`819` on 2 GB host, `PERF-3`). |
| `LLMROLLER_MEMORY_LIMIT_MB` | Integer | Auto (`cgroup v2` or `2048`) | Optional | Explicit container memory ceiling in MiB (`256..=65536`) used when calculating memory budgets (`PERF-3`, `PERF-5`). |
| `LLMROLLER_SQLITE_SYNCHRONOUS` | Enum | `"NORMAL"` | Optional | `"NORMAL"` or `"FULL"` writer WAL sync mode (`REL-2`). |
| `LLMROLLER_DEFAULT_ROUTING_STRATEGY` | Enum | `"smooth_weighted_rr"` | Optional | Initial/override default routing strategy: `smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random` (`random`), `weighted_random`, or `power_of_two_choices` (`p2c`); invalid values exit with code `2` (`RTE-14`). |
| `LLMROLLER_TLS_CERT` / `LLMROLLER_TLS_KEY` | Path | `None` | Optional | PEM certificate chain and private key for native `rustls` HTTPS listeners (`A4`). |
| `LLMROLLER_LOG` | EnvFilter | `"info,llmroller=info"` | Optional | Structured JSON log filter string for `tracing_subscriber` (`OBS-1`). |

#### 22.3.2 Complete `.env.example` reference manifest

Operators copy `.env.example` to `.env` to configure ports, routing strategy defaults, and resource limits.

```dotenv
# LLMRoller Production & Development Environment Template (.env.example)
# Copy to .env and generate ./secrets/master.key with:
#   mkdir -p secrets && chmod 0700 secrets
#   docker run --rm llmroller:1.0.0-distroless generate-master-key > secrets/master.key
#   chmod 0400 secrets/master.key

GIT_SHA=v1.0.0
LLMROLLER_DOMAIN=llmroller.internal.example.com
LLMROLLER_PUBLIC_HTTPS_PORT=443
LLMROLLER_ADMIN_HTTPS_PORT=8443

# Master Key Configuration (file secret preferred in production)
LLMROLLER_MASTER_KEY_SECRET_PATH=./secrets/master.key
LLMROLLER_MASTER_KEY_ID=mk_2026_10

# Security & TLS Enforcement
LLMROLLER_REQUIRE_TLS=true
LLMROLLER_SETUP_TOKEN_REQUIRED=true

# Memory, Body Budgets & SQLite Durability
LLMROLLER_MEMORY_LIMIT_MB=2048
LLMROLLER_PREFLIGHT_BODY_BUDGET_MB=819
LLMROLLER_MAX_BODY_MB=32
LLMROLLER_SQLITE_SYNCHRONOUS=NORMAL

# Pluggable Routing Strategy Default
# Allowed: smooth_weighted_rr | naive_rr | weighted_rr | uniform_random | weighted_random | power_of_two_choices
LLMROLLER_DEFAULT_ROUTING_STRATEGY=smooth_weighted_rr

# Observability
LLMROLLER_LOG=info,llmroller=info
```

#### 22.3.3 Entrypoint signal handling, migrations, and healthcheck contract

In `runtime-distroless`, `/usr/local/bin/llmroller` runs directly as PID 1 using `ENTRYPOINT ["/usr/local/bin/llmroller"]`. Because `serve` spawns no child processes, `crates/server` handles `SIGTERM` and `SIGINT` natively via `tokio::signal::unix::signal`.

Database migrations run automatically inside Startup Step 3 (`Section 5.5.2`) while holding the exclusive `flock` on `${LLMROLLER_DATA_DIR}/llmroller.lock`. During Startup Steps 1 through 9, `/healthz` returns HTTP `200` and `/readyz` returns HTTP `503`. Once Step 10 completes, `/readyz` flips to HTTP `200` and `llmroller healthcheck --url http://127.0.0.1:8080/readyz` exits `0`.

### 22.4 Observability and logging in containers

All logs write to `stdout` and `stderr` as single-line JSON objects (`ts`, `level`, `target`, `request_id`, `msg`, `fields`). Secrets are scrubbed per `SEC-6`, and `audit_anchor` lines emit per `AUD-1`. Container log rotation is bounded by Docker's `json-file` driver (`max-size: "50m"`, `max-file: "5"`).

Prometheus scrapes `http://172.28.2.20:9090/metrics` over `internal_obs_net`. Every routing selection increments `llmroller_routing_selections_total{provider_kind,provider_id,protocol,outcome,strategy,model_bucket}` where `strategy` identifies the active `RoutingStrategy` (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, or `power_of_two_choices`).
