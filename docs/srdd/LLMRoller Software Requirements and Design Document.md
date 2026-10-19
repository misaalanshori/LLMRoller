# LLMRoller: Software Requirements and Design Document

Version 1.1 · 19 October 2026 · Complete software requirements, architecture, routing, `/usage` pass-through, opt-in content traceability and chat preview, multi-role RBAC, and TDD specification.


## 0. Introduction

LLMRoller is a self-hosted Rust gateway that pools upstream coding-plan and aggregator API keys behind one client key per user. It pins each conversation session to the upstream key that started it. It also tracks token usage, dollar budgets, and upstream quota windows.

### 0.1 Purpose and scope

This document specifies the software requirements and system design for LLMRoller version 1.0 (dated 19 October 2026). Engineers use this specification to build, test, and run the proxy, router, model catalog, usage pipeline, and web interface.

Sections 1 through 8 define what the system must do, who uses it, and how security boundaries work. Sections 9 through 19 define the data model, internal architecture, algorithms, interfaces, and test governance. Section 20 lists the primary sources verified on 19 October 2026.

| In scope for version 1.0 | Out of scope for version 1.0 |
| --- | --- |
| Proxy for OpenAI Chat Completions, OpenAI Responses, and Anthropic Messages | Image generation, audio transcription, and vector embedding endpoints |
| Providers: OpenCode Go, Command Code, Open Design AMR, OpenRouter, and custom OpenAI or Anthropic compatible endpoints | OAuth consumer browser logins for ChatGPT Plus or Claude Pro web accounts |
| Global providers managed by admins and personal providers managed and shared by any user | Commercial billing, customer invoicing, or public resale to external third parties |
| Admin and standard user roles with full role-based access control | Multi-node clustering and distributed consensus across hosts |
| Session pinning, smooth weighted round-robin, and per-model key priority overrides | Automatic cross-protocol request translation in the base release |
| Local cost accounting plus source provider `/usage` polling, reconciliation, and pass-through | Unconditional or silent prompt logging (content recording is off by default and strictly opt-in) |
| Admin opt-in content traceability mode with encrypted turn storage and read-only session chat preview | Automatic cross-tenant prompt inspection on shared personal keys |
| Single static Rust binary, SQLite in WAL mode, and an embedded HTMX web interface | External single-page JavaScript build pipelines or CDN asset dependencies |

### 0.2 Requirement priorities and conventions

Every requirement in this specification carries a unique identifier and a release priority level. Every requirement is written so an automated test can pass or fail it.

| Priority | Keyword | Meaning |
| --- | --- | --- |
| P0 | must | Mandatory for version 1.0. The release cannot ship until all linked tests pass. |
| P1 | should | Expected in version 1.0 unless a documented engineering trade-off defers it to a point release. |
| P2 | may | Planned for a future release. Schema hooks and trait boundaries accommodate it now. |

Requirement identifiers use a short prefix for each subsystem. Functional prefixes include `AUTH`, `USR`, `PRV`, `SHR`, `CLK`, `MDL`, `RTE`, `API`, `USG`, `LIM`, `TRC`, `UI`, and `AUD`. Non-functional requirements use `PERF`, `REL`, `TRN`, `COM`, `POR`, `MNT`, `OBS`, and `PRIV`, while security requirements use `SEC`.

### 0.3 Definitions and glossary

The terms in the table below have one fixed meaning throughout all twenty-one sections of this specification.

| Term | Definition |
| --- | --- |
| Provider | An upstream inference service with defined endpoints, authentication rules, and session behavior, such as OpenCode Go or OpenRouter. |
| Global provider | A provider record created by an admin. Its upstream keys are available to all authorized users on the instance. |
| Personal provider | A provider record created by an individual user or admin for their own account. Only the owner and explicitly selected grantees can route through it. |
| Provider share | A permission grant from a personal provider owner that allows another specific user on the instance to route through that personal provider or key. |
| Upstream key | One API credential for one provider account. Each upstream key carries a default priority weight, a health state, and budget windows. |
| Client | A coding agent or SDK that calls LLMRoller, such as Claude Code, Codex CLI, opencode, or Hermes. |
| Client key | A credential (`llr_...`) that LLMRoller issues to an admin or standard user. Clients send it to LLMRoller; LLMRoller never forwards it upstream. |
| Session | One multi-turn conversation or agent task identified by a client-supplied session header, body field, or derived message fingerprint. |
| Pin | The stored binding from `(client_key_id, canonical_model_id, session_hash)` to one `upstream_key_id`. |
| Canonical model | The normalized model identifier that LLMRoller exposes to clients across providers, such as `deepseek-v4.1-flash`. |
| Route | The pairing of one `upstream_key_id` and one `canonical_model_id`, holding the raw upstream model name, supported protocols, and optional priority override. |
| Candidate | A route that passes all ownership, share, protocol, health, circuit breaker, and budget checks for the current request. |
| Protocol | One of three wire formats: `chat` (OpenAI Chat Completions), `responses` (OpenAI Responses), or `messages` (Anthropic Messages). |
| Affinity hint | The session identifier field or header that LLMRoller writes into an outbound request according to the target provider's session profile. |
| Upstream usage snapshot | The quota, credit, and reset-time record retrieved from a source provider's native usage or key endpoint. |
| Content traceability mode | An admin-controlled opt-in setting that records compressed, encrypted request and response contents (prompts, reasoning, tool calls, and completions) per turn. |
| Session chat preview | A read-only dashboard view that reconstructs and renders the multi-turn conversation timeline across all traced requests sharing a pinned `session_hash`. |

### 0.4 Document map

The specification is split into twenty-one numbered Markdown files (`0` through `20`) so engineers can review and trace individual subsystems.

| File | Title | Focus |
| --- | --- | --- |
| `0. SRDD - Introduction.md` | Introduction | Scope, priority conventions, glossary, and document map |
| `1. SRDD - Summary.md` | Summary | Problem statement, product vision, and core architectural choices |
| `2. SRDD - Research Findings.md` | Research findings | Empirical data on session headers, provider limits, and `/usage` endpoints |
| `3. SRDD - Goals, Non-Goals, and Assumptions.md` | Goals, non-goals, and assumptions | Targets, boundaries, and sizing assumptions |
| `4. SRDD - Users, Roles, and Provider Ownership.md` | Users, roles, and provider ownership | Admin and standard user roles, global and personal providers, and user sharing |
| `5. SRDD - System Architecture.md` | System architecture | Component layout, request flow, hot-path state, and listeners |
| `6. SRDD - Functional Requirements.md` | Functional requirements | Testable P0, P1, and P2 functional specifications |
| `7. SRDD - Non-Functional Requirements.md` | Non-functional requirements | Latency, memory, streaming, compatibility, and operational targets |
| `8. SRDD - Security and Privacy.md` | Security and privacy | Threat model, cryptography, isolation, and supply-chain controls |
| `9. SRDD - Data Model.md` | Data model | SQLite WAL schema, encryption rules, and retention policies |
| `10. SRDD - Routing Design.md` | Routing design | Eligibility, smooth weighted round-robin, pinning, and failover |
| `11. SRDD - Session Identification and Hints.md` | Session identification and hints | Inbound session extraction, hashing, and outbound provider hints |
| `12. SRDD - Protocol Handling and Transparent Proxying.md` | Protocol handling and transparent proxying | Header policy, body edits, streaming tee, and token extraction |
| `13. SRDD - Pricing, Limits, and Source Provider Usage Engine.md` | Pricing, limits, and source provider usage engine | Price book math, rolling windows, and upstream `/usage` sync and pass-through |
| `14. SRDD - API and Web UI Specification.md` | API and web UI specification | Proxy routes, user and admin JSON APIs, and HTMX dashboard screens |
| `15. SRDD - Technology Choices and Workspace Layout.md` | Technology choices and workspace layout | Rust crates, versions, and multi-crate repository structure |
| `16. SRDD - Testing Plan and TDD Governance.md` | Testing plan and TDD governance | Strict TDD, boundary rules, failure taxonomy, mocking, mutation tests, and traceability |
| `17. SRDD - Roadmap and Milestones.md` | Roadmap and milestones | Dependency-ordered delivery milestones and exit criteria |
| `18. SRDD - Risks and Mitigations.md` | Risks and mitigations | Technical, vendor, and operational risk register |
| `19. SRDD - Architectural Decisions and Open Questions.md` | Architectural decisions and open questions | Resolved design trade-offs and operator configuration choices |
| `20. SRDD - Sources.md` | Sources | Verified primary links for providers, clients, gateways, and standards |

## 1. Summary

LLMRoller is a self-hosted LLM proxy and API key router written in Rust. It sits between coding agents and multiple paid upstream subscriptions or aggregator accounts. Each user authenticates to LLMRoller with a single client key, and LLMRoller distributes conversations across a pool of upstream keys.

### 1.1 The problem

Subscription coding plans such as [OpenCode Go](https://opencode.ai/docs/go), [Command Code GOAT](https://commandcode.ai/docs/plans/goat), and [Open Design](https://open-design.ai/pricing/) sell monthly tiers with rolling five-hour, seven-day, and monthly dollar caps. Heavy users and small engineering teams often hold several keys across these providers alongside pay-as-you-go aggregators like [OpenRouter](https://openrouter.ai/docs/api_reference/limits).

Managing these keys by hand causes two failures. First, switching keys or providers mid-conversation destroys prompt cache locality, which multiplies input token costs up to tenfold and slows responses. Second, different providers give different monthly dollar limits to the same model, so static key rotation wastes high-allowance tiers on the wrong models.

### 1.2 The solution

LLMRoller solves both problems in one single-binary gateway. It reads the session identifier that coding agents already send and pins each conversation to one upstream key for its entire lifetime. Recognized fields include `x-claude-code-session-id`, `session_id`, `x-session-affinity`, `x-opencode-session`, and `prompt_cache_key`.

When a new session starts, LLMRoller selects an eligible upstream key using smooth weighted round-robin. Each upstream key has a default priority weight. Operators and users can override that weight per model to match each provider's per-model dollar allowance.

### 1.3 Core architectural pillars

The system design rests on nine engineering decisions that govern every module in the workspace.

| Pillar | Design decision |
| --- | --- |
| Transparent forwarding | Forward inbound requests and streamed SSE chunks without buffering. Rewrite only authentication headers, the model identifier, provider session fields, and usage-reporting flags. |
| Native session pinning | Extract the client's existing session identifier, bind `(client_key, model, session)` to one upstream key, and emit the exact affinity field that the chosen provider expects. |
| Per-model priority overrides | Use the upstream key's default priority weight unless the route defines a per-model override. Route new sessions with nginx-style smooth weighted round-robin. |
| Dual provider scopes | Support both admin-managed `global` providers (usable by all authorized users) and user-managed `personal` providers (usable by the owner and explicitly shared grantees). |
| First-class standard users | Ship full role-based access control in version 1.0 for both `admin` and `user` accounts, giving standard users self-service personal providers, sharing controls, client keys, and scoped analytics. |
| Hybrid usage and `/usage` pass-through | Compute request cost locally in integer micro-USD while polling and reconciling source provider `/usage` endpoints where available, and expose upstream usage via pass-through APIs. |
| Opt-in content traceability | Record prompts, reasoning, tool calls, and responses when enabled by an admin, and render a read-only session chat preview in the dashboard. |
| Zero-dependency operations | Ship one static Rust binary with an embedded HTMX web interface and one SQLite database in WAL mode. |
| Strict TDD and mutation testing | Develop all controlled logic under Red-Green-Refactor discipline with full requirement-to-test traceability and automated mutation testing via [cargo-mutants](https://mutants.rs/). |

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
| Command Code | `GET https://api.commandcode.ai/alpha/billing/credits`, `/alpha/usage/summary`, `/alpha/billing/subscriptions`, `/alpha/whoami` (plus `/internal/billing/credits` fallback) | `Authorization: Bearer <key>` | Verified in [pi-commandcode-provider](https://pi.dev/packages/pi-commandcode-provider) and [commandcode-goat-usagebar](https://open-vsx.org/extension/berkeduruu/commandcode-goat-usagebar). Returns 5 h, 7 d, and monthly quota percentages, credits used and remaining, plan tier, and credit sources. |
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
| G1 | Preserve upstream prompt cache locality across multi-turn coding sessions. | Requests carrying a valid session ID stay on their pinned upstream key while that route remains eligible. |
| G2 | Pool global, personal, and shared upstream keys with predictable session distribution. | Over 100,000 new sessions, smooth weighted round-robin matches configured route weights within 1%. |
| G3 | Support both admin-managed global providers and user-managed personal providers with granular user-to-user sharing. | Standard users can add personal providers, share them with specific peers, and route traffic without admin intervention. |
| G4 | Stay transparent to coding agents and upstream providers. | Streaming chunks incur under 1 ms of forwarding delay, and request bodies keep their exact key and message order. |
| G5 | Provide accurate cost, token, and quota visibility by combining local price-book math with source provider `/usage` endpoints. | Local window counters reconcile automatically with upstream `/usage` snapshots, and clients can query `/v1/usage` directly. |
| G6 | Protect upstream secrets, client keys, and user privacy at rest, in memory, and in logs. | Zero secret leaks in canary log tests, XChaCha20-Poly1305 encryption at rest, and zero prompt storage unless content traceability is enabled. |
| G7 | Operate as a single, low-overhead static Rust binary with an embedded web interface. | Idle memory stays under 50 MB, and p99 proxy overhead before first upstream byte stays under 5 ms at 500 req/s. |
| G8 | Enforce strict Test-Driven Development and mutation testing across all controlled logic. | Every requirement maps to automated tests, and `cargo-mutants` reports zero surviving mutants in core routing and protocol crates. |
| G9 | Support admin opt-in content traceability and a read-only session chat preview. | When enabled by an admin, traced sessions render prompts, reasoning, tool calls, and responses in chronological order with zero XSS risk. |

### 3.2 Non-goals for version 1.0

Eight capabilities are intentionally excluded from version 1.0 to keep the attack surface and complexity small.

| ID | Non-goal | Rationale |
| --- | --- | --- |
| NG1 | Client or TLS fingerprint impersonation | LLMRoller never spoofs another vendor's `User-Agent` or JA3/JA4 TLS fingerprint. It forwards the caller's real headers or a neutral default. |
| NG2 | Public commercial resale or multi-organization billing | Subscription coding plans forbid public reselling. LLMRoller targets a single person or a trusted internal team. |
| NG3 | Automatic cross-protocol translation in Milestone 1 | Translating between `chat`, `responses`, and `messages` can drop reasoning blocks ([open-design#8687](https://github.com/nexu-io/open-design/issues/8687)). Base routing matches native support; opt-in translation is deferred to a later milestone. |
| NG4 | Multi-node active-active clustering | A single Rust process with SQLite WAL handles 500 req/s and 2,000 open streams, which exceeds single-team coding-agent load. |
| NG5 | Payment processing, invoicing, or Stripe integration | Budget tracking exists to prevent quota exhaustion and monitor spend, not to bill external customers. |
| NG6 | Unconditional or silent prompt logging | Prompt and response recording is strictly off by default and activates only when an admin enables content traceability mode (`TRC-1`). |
| NG7 | Consumer OAuth browser session scraping | Version 1.0 authenticates upstream using standard API keys only. |
| NG8 | Non-inference media endpoints | Image, audio, and embedding endpoints do not use multi-turn prompt caching and are outside version 1.0 scope. |

### 3.3 Assumptions and constraints

The design relies on five operational assumptions listed in the table below.

| ID | Assumption | Operational impact |
| --- | --- | --- |
| A1 | Terms of use compliance | The instance operator and individual users hold valid accounts with each configured provider and comply with each provider's terms ([Open Design Terms](https://download.open-design.ai/terms/), [OpenCode Go Docs](https://opencode.ai/docs/go)). |
| A2 | Sizing envelope | One instance supports up to 200 users, 500 upstream keys, 1,000 client keys, 500 req/s at peak, and 2,000 open SSE streams on 2 vCPU and 2 GB RAM. |
| A3 | Configurable client base URL | Every target client tool (Claude Code, Codex CLI, opencode, Hermes, and standard OpenAI or Anthropic SDKs) lets the user configure a custom base URL and API key. |
| A4 | TLS endpoint handling | Production deployments handle HTTPS either at an upstream reverse proxy (Caddy, nginx, or Traefik) with response buffering off, or via LLMRoller's built-in `rustls` listener. |
| A5 | Rust toolchain and runtime | The project compiles with a pinned stable Rust toolchain (Rust 2024 edition) and runs on Linux (`x86_64` and `aarch64`) and macOS without external runtime dependencies. |

## 4. Users, roles, and provider ownership

LLMRoller is a multi-user system in version 1.0. It defines two human user classes (`admin` and `user`) and one machine caller class (`client`). It also defines two provider ownership scopes (`global` and `personal`) with explicit user-to-user sharing.

### 4.1 User classes

Every human account has a `role` column set to either `admin` or `user`. Machine callers authenticate to the data plane with a client key owned by a human account.

| User class | Version 1.0 status | Summary of capabilities |
| --- | --- | --- |
| Admin (`admin`) | P0 (full support) | Manages instance settings, user accounts, global providers, global upstream keys, canonical model catalog, global price book, all personal providers, all pins, and system-wide usage analytics. |
| Standard user (`user`) | P0 (full support) | Manages own account credentials, own client keys, own personal providers and upstream keys, sharing grants to other users, per-model priority overrides on own keys, own active pins, and own usage analytics. |
| Client agent (`client`) | P0 (full support) | Calls `/v1/chat/completions`, `/v1/responses`, `/v1/messages`, `/v1/models`, `/v1/usage`, and `/v1/key` using a client key (`llr_...`). Has no web UI or control-plane access. |

### 4.2 Role-based access control matrix

The control plane enforces strict authorization checks on every HTTP handler and database query using an explicit `ActorScope` passed from the authentication middleware.

| Resource or action | Admin (`admin`) | Standard user (`user`) | Client agent (`client`) |
| --- | --- | --- | --- |
| Complete initial first-admin setup | Yes (when zero users exist) | No | No |
| Create, disable, delete users, or reset passwords | Yes (cannot delete last active admin) | No (can change own password and TOTP only) | No |
| Create, edit, test, or delete `global` providers and keys | Yes | No | No |
| View `global` provider availability and exposed models | Yes (full details and last-4 of secrets) | Yes (provider name and exposed models only; no key secrets or global key labels) | Via `GET /v1/models` |
| Create, edit, test, calibrate, or delete `personal` providers and keys | Yes (own personal keys, plus admin override on any user's keys) | Yes (own personal providers and keys only) | No |
| Share a `personal` provider or key with another user | Yes (for own personal keys or any key) | Yes (for own personal providers and keys only) | No |
| View a `personal` provider or key shared by another user | Yes | Yes (grantee sees provider type, key label, health, allowed models, and own share usage or cap; never the secret) | Routes through it if eligible |
| Poll or view source provider `/usage` for an upstream key | Yes (all global and personal keys) | Yes (own personal keys, plus summary window status on keys shared with them) | Via `GET /v1/usage` and `GET /v1/key` (scoped) |
| Manage canonical model exposure (`auto`, `forced_on`, `forced_off`) and aliases | Yes | Read-only view of catalog; can enable or disable models on own personal routes | No |
| Set per-model priority overrides on routes | Yes (global routes and own personal routes) | Yes (own personal routes and personal routing weights across eligible routes) | No |
| Create, rotate, restrict, or revoke client keys (`llr_...`) | Yes (own keys and any user's keys) | Yes (own client keys only) | Uses key for bearer auth |
| View usage dashboard, time series, and CSV or JSON exports | Yes (instance-wide and filtered by any user, key, provider, or model) | Yes (strictly scoped to own client keys and traffic served by own personal keys) | Via `GET /v1/usage` for own client key |
| View, unpin, or move active session pins | Yes (all pins) | Yes (pins belonging to own client keys only) | Implicit via requests |
| Enable, scope, or purge content traceability mode | Yes (instance-wide and scoped targets) | No (read-only visibility into whether traceability is active on own traffic) | No |
| View read-only Session Chat Preview of recorded turns | Yes (all traced sessions, logged in `audit_log`) | Yes (traced sessions belonging to own client keys only; never sees grantee transcripts on shared keys) | No |
| View audit log | Yes (all instance events) | Yes (events where actor or target belongs to the user) | No |

### 4.3 Provider ownership scopes: global and personal

Every provider record in LLMRoller has an `ownership_scope` field set to either `global` or `personal`.

#### 4.3.1 Global providers

Only an admin can create, modify, or delete a `global` provider and its upstream keys. Global providers hold shared team or organization subscriptions. Every active user with `can_use_global_providers = 1` can route requests through eligible global upstream keys. Admins can also set an optional monthly global spend cap (`global_budget_micro_usd_month`) on any user account.

#### 4.3.2 Personal providers

Any authenticated user (`user` or `admin`) can create a `personal` provider and attach one or more personal upstream keys to it. A personal provider has a non-null `owner_user_id` referencing the user who created it.

Personal providers let users bring their own OpenCode Go, Command Code, Open Design AMR, OpenRouter, or custom API keys into LLMRoller. Users get session pinning and per-model priority overrides across their own keys, and can combine personal keys with global keys.

### 4.4 Sharing personal providers and keys

A personal provider owner can share a personal provider or a specific personal upstream key with other registered users on the same instance.

| Share attribute | Behavior and enforcement |
| --- | --- |
| Grantor (`owner_user_id`) | Must be the owner of the personal provider (or an admin). Cannot re-share a key that was only shared with them (no transitive sharing). |
| Grantee (`grantee_user_id`) | Any active `user` or `admin` account on the instance, selected by username. |
| Target scope | Either an entire `personal` provider (all enabled keys under it) or one specific `personal` upstream key. |
| Model restriction (`allowed_models`) | Optional JSON list of canonical models that the grantee may invoke on the shared key. Defaults to all models the key serves. |
| Monthly spend cap (`max_monthly_micro_usd`) | Optional micro-USD cap on how much the grantee's client keys can spend on this share in a calendar month. |
| Permission level | Usage only. The grantee can route inference requests through the shared key and see its health badge and remaining share allowance, but cannot view the key's last four characters, edit its priority or settings, trigger calibration, or delete it. |
| Revocation | The owner (or an admin) can revoke a share at any time. Revocation takes effect within 1 s on the in-memory config snapshot and immediately invalidates any active session pins where the grantee's client key was pinned to the revoked upstream key. |

### 4.5 Routing interaction between personal, shared, and global providers

When a client key owned by user `U` sends a request for canonical model `M`, the router builds candidates from three pools.

| Pool name | Candidate membership criteria |
| --- | --- |
| Own personal pool | Enabled upstream keys on personal providers where `owner_user_id = U`. |
| Shared personal pool | Enabled upstream keys on personal providers shared with `U` via an active share grant whose model filter includes `M` and whose monthly share spend is below `max_monthly_micro_usd`. |
| Global pool | Enabled upstream keys on `global` providers, provided `U.can_use_global_providers = 1` and `U`'s monthly spend on global keys is below `U.global_budget_micro_usd_month` (when set). |

Each client key (and user account default) defines a `routing_scope` policy that controls how these three pools combine for new sessions.

| Routing scope mode | Candidate selection behavior |
| --- | --- |
| `personal_first` (default) | Selects among eligible routes in the user's own personal pool and shared personal pool using smooth weighted round-robin. Falls back to the global pool only when no personal or shared route is eligible for model `M`. |
| `blended` | Combines eligible routes from the own personal, shared personal, and global pools into a single smooth weighted round-robin pool. |
| `personal_only` | Restricts routing strictly to the user's own personal pool and shared personal pool. Returns `503 no_eligible_route` if neither can serve the request, never using global keys. |
| `global_only` | Restricts routing strictly to the global pool, ignoring personal and shared keys. |

### 4.6 Visibility and accounting for shared personal keys

When Grantee `B` routes a request through a personal key owned by Grantor `A`, the usage row records both `client_key_id` and `upstream_key_id`.

| Actor | Visibility and privacy boundaries |
| --- | --- |
| Grantor `A` (key owner) | Sees the total tokens and cost consumed on their personal upstream key, plus a breakdown showing how much Grantee `B` consumed against the share grant. Grantor `A` never sees Grantee `B`'s client key labels, session hashes, or recorded chat transcripts (`TRC-8`). |
| Grantee `B` (share recipient) | Sees all requests made by `B`'s own client keys, including which requests routed through the shared key (`shared: <owner_username>/<key_label>`), along with `B`'s remaining monthly share allowance. Grantee `B` never sees Grantor `A`'s own usage or other grantees' usage on that key. |

## 5. System architecture

LLMRoller runs as one Rust process made of six internal subsystems. These subsystems share an atomic in-memory configuration snapshot and one SQLite database in Write-Ahead Logging (WAL) mode.

### 5.1 High-level architecture diagram

The diagram below shows how client agents, standard users, and admins interact with LLMRoller's data plane and control plane.

```
Client agents (Claude Code, Codex CLI, opencode, Hermes, SDKs)
   |  HTTPS + Client Key (llr_...)
   v
+============================= LLMRoller Process =============================+
|                                                                             |
|  +----------------------- Data Plane Listener (:8080) -------------------+  |
|  | Proxy Edge -> Normalizer -> Model & Scope Resolver -> Session Resolver|  |
|  |     -> Router (Pin Lookup / Smooth WRR / Failover)                    |  |
|  |     -> Provider Adapter -> Stream Forwarder, Usage & Trace Tap        |  |
|  |     -> /v1/usage & /v1/key Pass-Through Handler                       |  |
|  +-----------------------------------------------------------------------+  |
|                     |                                     ^                 |
|                     v (lock-free read)                    | (atomic swap)   |
|  +-----------------------------------------------------------------------+  |
|  | Hot-Path In-Memory State                                              |  |
|  |  - ArcSwap<ConfigSnapshot> (users, keys, shares, models, routes)      |  |
|  |  - Moka Pin Cache & Response-Link Cache (single-flight + sliding TTL) |  |
|  |  - DashMap WRR Counters, Circuit Breakers, Window Ring Counters       |  |
|  |  - Cached Upstream Usage Snapshots                                    |  |
|  +-----------------------------------------------------------------------+  |
|                     |                                     ^                 |
|                     v (async mpsc batch)                  | (reload)        |
|  +----------------------- Storage & Background Jobs ---------------------+  |
|  |  - SQLite (WAL mode): 20 tables, single batching writer task          |  |
|  |  - Jobs: Model Discovery, Upstream /usage Poller, Price Sync,         |  |
|  |          Hourly Rollups, Retention Sweeper                            |  |
|  +-----------------------------------------------------------------------+  |
|                                       ^                                     |
|  +------------------- Control Plane Listener (127.0.0.1:8081) -----------+  |
|  |  - Admin UI & API (/admin/*): Setup, Users, Global Providers, Catalog |  |
|  |  - User UI & API (/app/*, /api/user/*): Personal Providers, Shares,   |  |
|  |    Client Keys, Personal Routing Overrides, Scoped Usage & Pins       |  |
|  |  - Metrics Listener (127.0.0.1:9090): Prometheus /metrics             |  |
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
| Proxy edge (`proxy`) | Authenticates client keys (`llr_...`), enforces body size and per-key rate limits, dispatches `/v1/*` inference and `/v1/usage` routes, and streams responses without buffering. | `axum`, `hyper-util`, `tower`, `reqwest` (`rustls`) |
| Routing core (`core`) | Pure deterministic logic with zero I/O: resolves session IDs, filters candidate routes by ownership, share grants, protocol, health, and budgets, runs smooth weighted round-robin, and manages the pin state machine. | `arc-swap`, `moka`, `dashmap` |
| Provider adapters (`proto`) | Implements `ProviderAdapter` per provider type: builds upstream URLs, headers, auth, and session hints, parses `/models` and source `/usage` responses, and classifies upstream HTTP errors. | `serde`, `serde_json`, `bytes` |
| Usage and trace tap (`proto`, `store`) | Tees raw SSE chunks to a non-blocking parser, extracts token counts and costs, and (when content traceability is on) reassembles prompts, reasoning, tool calls, and completions into compressed, encrypted `trace_records`. | `tokio::sync::mpsc`, `async-compression`, `zstd`, `sqlx` |
| Catalog, health, and `/usage` service (`server`) | Runs background tasks for `/models` discovery, upstream `/usage` endpoint polling and budget reconciliation, key cooldown timers, and circuit breaker state transitions. | `tokio`, `time` |
| Control plane (`admin`) | Serves the JSON APIs and server-rendered HTMX web interface for both `admin` and `user` roles, handles cookie sessions, Argon2id logins, TOTP, CSRF verification, and hash-chained audit logging. | `askama`, `rust-embed`, `argon2`, `chacha20poly1305` |

### 5.3 Ten-step inference request lifecycle

Every inference request on `/v1/chat/completions`, `/v1/responses`, or `/v1/messages` follows ten sequential steps.

| Step | Stage | Action taken |
| --- | --- | --- |
| 1 | Authenticate client key | Hash the bearer or `x-api-key` token with SHA-256, look up the `ClientKey` in `ConfigSnapshot` in constant time, and verify rate limits. |
| 2 | Parse envelope | Identify the wire protocol (`chat`, `responses`, or `messages`) from the URL path and read `model`, `stream`, and session fields without altering field order. |
| 3 | Resolve canonical model | Map the requested model string to a `CanonicalModel` and verify that the model is exposed and permitted by `client_key.allowed_models`. |
| 4 | Resolve session identity | Extract the session ID using the nine-level precedence order (Section 11.1) and compute the 128-bit `session_hash` using HMAC-SHA256. |
| 5 | Check existing pin | Check `moka` for a pin matching `(client_key_id, canonical_model_id, session_hash)` (or `previous_response_id`). If confirmed and still eligible, select that route. |
| 6 | Select new route on miss | Filter candidate routes across the owner's personal, shared, and global pools under `routing_scope`, run smooth WRR, and write a `provisional` single-flight pin. |
| 7 | Build and send request | Build the outbound request via the provider adapter, inject the decrypted upstream key and session affinity hint, apply surgical body edits, and send upstream. |
| 8 | Failover before first byte | If the upstream call fails before yielding the first body byte, update key health, delete or move the provisional pin, and retry on up to two other routes. |
| 9 | Confirm pin and stream | When the first response byte arrives, promote the pin to `confirmed`, refresh its sliding TTL, and stream every chunk to the client while teeing the usage tap. |
| 10 | Record usage, trace, and reconcile | Emit a `UsageEvent` (and an encrypted `TraceRecord` when content traceability is active) to the batch writer, update window counters, and trigger `/usage` polls when needed. |

### 5.4 Hot-path state and listener isolation

To keep p99 routing latency under 5 ms, the data plane never queries SQLite on the request path. All configuration tables are compiled into an immutable `ConfigSnapshot` struct held inside an `ArcSwap`. When an admin or standard user changes configuration in the control plane, the store commits the SQLite transaction. It then builds a new `ConfigSnapshot` and swaps the pointer atomically.

LLMRoller binds three independent TCP listeners by default, as listed in the table below.

| Listener | Default bind address | Served routes and purpose |
| --- | --- | --- |
| Data plane listener | `0.0.0.0:8080` | Serves `/v1/*`, `/healthz`, and `/readyz` for coding agents. |
| Control plane listener | `127.0.0.1:8081` | Serves `/admin/*`, `/app/*`, `/api/*`, `/setup`, and `/login`. Loopback binding prevents accidental internet exposure. |
| Metrics listener | `127.0.0.1:9090` | Serves `/metrics` in Prometheus text format on loopback. |

### 5.5 Command-line interface (`llmroller`) contract and exit codes

The `llmroller` binary in `crates/server` uses `clap` derive macros to expose one daemon command (`serve`) and four maintenance subcommands. Every subcommand returns a deterministic POSIX exit code so systemd units, Kubernetes probes, and cron scripts can automate recovery.

| Subcommand and arguments | Operational behavior | Exit codes |
| --- | --- | --- |
| `llmroller serve [--listen <addr>] [--admin-listen <addr>] [--metrics-listen <addr>] [--data-dir <path>] [--dev-insecure-cookies]` | Validates `LLMROLLER_MASTER_KEY`, runs pending SQLite WAL migrations, loads `ArcSwap<ConfigSnapshot>`, rebuilds rolling window rings from `usage_events`, spawns background jobs, and binds the three TCP listeners until `SIGTERM` or `SIGINT`. | `0` clean drain; `2` invalid CLI flag or env var; `3` missing or invalid master key (`SEC-2`); `4` SQLite migration or lock error; `5` listener bind error |
| `llmroller backup --out <dest_path> [--data-dir <path>]` | Opens the active SQLite WAL database in shared mode and runs the SQLite Online Backup API (`sqlite3_backup_init` / `step` / `finish`) into `<dest_path>.tmp`, verifies `PRAGMA integrity_check` on the snapshot, and atomically renames it to `<dest_path>` (`REL-6`). | `0` verified backup written; `4` source DB locked or corrupt; `6` destination I/O error |
| `llmroller rotate-master-key --old-key-env <VAR> --new-key-env <VAR> --new-key-id <id> [--data-dir <path>]` | Opens a `BEGIN IMMEDIATE` transaction on the writer connection, decrypts every `upstream_keys.secret_enc`, `users.totp_secret_enc`, and `trace_records.payload_enc` row using the old master key and row-specific `AAD`, re-encrypts each row with a fresh 24-byte random nonce under the new HKDF subkey, updates `master_key_id`, and appends a hash-chained `audit_log` entry (`SEC-1`). | `0` all rows rotated; `3` key decode or length error; `7` AEAD authentication failure on any existing row (transaction rolled back cleanly) |
| `llmroller verify-audit-log [--data-dir <path>] [--json]` | Streams all rows from `audit_log` ordered by `ts ASC, id ASC`, recomputes the canonical length-prefixed SHA-256 chain from the genesis seed (`0x00 * 32`), and reports total verified rows or the exact first tampered `audit_log.id` (`SEC-13`). | `0` chain intact; `8` hash mismatch or missing predecessor detected |
| `llmroller generate-master-key` | Reads 32 bytes from `OsRng`, base64-encodes the key, writes it to `stdout` with a trailing newline, and zeroizes the buffer before exiting. | `0` success |

### 5.6 Environment variables and runtime `settings` registry

LLMRoller separates static bootstrap parameters from dynamic runtime policies. Static bootstrap parameters come from environment variables or CLI flags at startup. Runtime policies live in the SQLite `settings` table and hot-reload into `ArcSwap<ConfigSnapshot>` within 1 s (`REL-4`).

| Environment variable | Type & default | Validation rule and architectural effect |
| --- | --- | --- |
| `LLMROLLER_MASTER_KEY` | Base64 string (mandatory unless `_FILE` set) | Must decode to at least 32 raw bytes (`SEC-2`). Used as the root IKM for `HKDF-SHA256` subkey derivation. |
| `LLMROLLER_MASTER_KEY_FILE` | Filesystem path (`None`) | Path to a file (`0400` or `0600` permissions) containing the base64 master key; mutually exclusive with `LLMROLLER_MASTER_KEY`. |
| `LLMROLLER_MASTER_KEY_ID` | ASCII string (`"mk_default"`) | Identifier recorded in `master_key_id` columns on newly sealed rows (`SEC-1`). |
| `LLMROLLER_DATA_DIR` | Filesystem path (`"/var/lib/llmroller"`) | Directory holding `llmroller.db`, `llmroller.db-wal`, and `llmroller.db-shm`. |
| `LLMROLLER_LISTEN` | Socket address (`"0.0.0.0:8080"`) | Data-plane HTTP/HTTPS bind address serving `/v1/*`, `/healthz`, and `/readyz`. |
| `LLMROLLER_ADMIN_LISTEN` | Socket address (`"127.0.0.1:8081"`) | Control-plane HTTP/HTTPS bind address serving `/admin/*`, `/app/*`, `/api/*`, `/setup`, and `/login` (`SEC-9`). |
| `LLMROLLER_METRICS_LISTEN` | Socket address (`"127.0.0.1:9090"`) | Prometheus `/metrics` listener bind address (`OBS-1`, `SEC-9`). |
| `LLMROLLER_SETUP_TOKEN` | ASCII string (auto-generated 32-char base62) | One-time bootstrap secret checked by `POST /admin/api/setup` (`AUTH-2`). |
| `LLMROLLER_SETUP_TOKEN_REQUIRED` | Boolean (`true`) | When `false`, skips the setup token check during initial first-admin bootstrap (`AUTH-2`). |
| `LLMROLLER_MAX_BODY_MB` | Integer `1..=128` (`32`) | Maximum inbound HTTP request body size in MiB enforced at the proxy edge (`SEC-8`). |
| `LLMROLLER_TLS_CERT` / `LLMROLLER_TLS_KEY` | Filesystem paths (`None`) | Optional PEM certificate chain and private key enabling native `rustls` HTTPS on the listeners (`A4`). |
| `LLMROLLER_LOG` | Filter string (`"info,llmroller=info"`) | Structured JSON log level filter passed to `tracing_subscriber::EnvFilter` (`OBS-1`). |

## 6. Functional requirements

This section specifies all functional requirements for LLMRoller version 1.0. Every requirement has a unique ID, a priority (`P0`, `P1`, or `P2`), and a clear pass or fail condition linked to Section 16.

### 6.1 Setup, accounts, and authentication (AUTH)

The authentication subsystem controls initial instance bootstrap, password and two-factor verification, and web session lifecycles.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| AUTH-1 | P0 | When zero rows exist in the `users` table, the control plane must serve a setup flow (`GET /setup`, `POST /admin/api/setup`) that creates the first `admin` account inside a serializable transaction so concurrent submissions cannot create two admins. |
| AUTH-2 | P0 | Initial setup must validate a one-time setup token read from `LLMROLLER_SETUP_TOKEN` or generated and printed to standard output at startup (unless `LLMROLLER_SETUP_TOKEN_REQUIRED=false`). |
| AUTH-3 | P0 | Once at least one user row exists in the database, all `/setup` routes must return HTTP 404 permanently, with no configuration flag able to re-enable them. |
| AUTH-4 | P0 | An admin must be able to create, disable, enable, and delete user accounts with role `admin` or `user`, and set a temporary password that forces a password change at first login. |
| AUTH-5 | P0 | The system must reject any request to disable, demote, or delete the last active `admin` account with HTTP 409. |
| AUTH-6 | P0 | Passwords must be at least 12 characters long and hashed with Argon2id (`m >= 19456 KiB`, `t >= 2`, `p = 1`). |
| AUTH-7 | P0 | Web login (`POST /api/auth/login`) must issue a 256-bit random session token in an `HttpOnly`, `SameSite=Strict` cookie (stored as a SHA-256 hash in `web_sessions`), and logout (`POST /api/auth/logout`) must delete the server-side session row immediately. |
| AUTH-8 | P0 | Login and setup endpoints must enforce rate limiting per IP address and per username (default: five failed logins per 15 minutes). |
| AUTH-9 | P1 | Both `admin` and `user` accounts should support TOTP two-factor authentication (`RFC 6238`) with recovery codes, and admins should be able to reset a user's password and revoke their active web sessions. |

### 6.2 User roles and access control (USR)

The user role subsystem enforces multi-tenant isolation between standard users and admins across the control plane and data plane.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| USR-1 | P0 | The system must support two human user roles in version 1.0: `admin` and `user`. Every control-plane request must resolve an `ActorScope` containing `user_id` and `role`. |
| USR-2 | P0 | A standard `user` must be able to create, view, edit, test, calibrate, and delete their own `personal` providers and upstream keys, manage shares on their own personal keys, manage their own client keys, and view their own usage and active pins. |
| USR-3 | P0 | A standard `user` must receive HTTP 403 or 404 when requesting or mutating another user's client keys, personal providers (unless shared, in which case only read-only grantee metadata is visible), usage events, pins, or admin-only endpoints. |
| USR-4 | P0 | A standard `user` must never see raw secrets, last-four secret suffixes, or administrative labels of `global` upstream keys or keys shared with them by another user. |
| USR-5 | P0 | An `admin` must be able to set `can_use_global_providers` (boolean, default true) and `global_budget_micro_usd_month` (optional integer micro-USD cap) on each `user` account. |

### 6.3 Global and personal providers and upstream keys (PRV)

The provider subsystem manages both admin-owned global providers and user-owned personal providers, along with their upstream API keys.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| PRV-1 | P0 | Every provider record must have an `ownership_scope` of `global` (creatable and editable only by `admin`) or `personal` (creatable and editable by its `owner_user_id`, whether `user` or `admin`). |
| PRV-2 | P0 | Users and admins must be able to add providers from built-in presets (`opencode_go`, `command_code`, `open_design_amr`, `openrouter`) or as a `custom` OpenAI or Anthropic compatible provider with a configurable base URL. |
| PRV-3 | P0 | Each provider record must store editable settings: base URL, protocol paths (`chat`, `responses`, `messages`), auth scheme per protocol (`Bearer` or `x-api-key`), `/models` path, `/usage` profile, header strip and add lists, fallback `User-Agent` policy, and session hint profile. |
| PRV-4 | P0 | A provider must support multiple upstream keys. Each key has a label, encrypted secret, last-four suffix, enabled flag, default priority weight (`1` to `1000`), optional strict tier, and health state (`healthy`, `cooling_down`, `exhausted`, `invalid`). |
| PRV-5 | P0 | Upstream key secrets must be encrypted at rest with XChaCha20-Poly1305 before database insertion and never returned in any API or UI response after creation. |
| PRV-6 | P0 | A Test action on an upstream key must call the provider's `/models` endpoint (and `/usage` endpoint if configured) using that key and report HTTP status, discovered model count, and current upstream usage summary. |
| PRV-7 | P0 | Custom provider base URLs must use `https://`, reject HTTP redirects, and block loopback, link-local, and RFC 1918 private IP ranges unless an admin explicitly enables `allow_private_network` on that provider. |
| PRV-8 | P0 | Disabling an upstream key must immediately exclude it from new sessions and force sessions pinned to it to re-pin on their next request. |

### 6.4 Personal provider and key sharing (SHR)

The sharing subsystem lets any user share their personal providers or personal upstream keys with specific peers on the same instance.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| SHR-1 | P0 | The owner of a `personal` provider (or an `admin`) must be able to create a share grant (`provider_shares`) granting a specific `grantee_user_id` access to either the entire personal provider or one specific personal upstream key. |
| SHR-2 | P0 | Each share grant must support an optional list of `allowed_models` and an optional monthly spend cap (`max_monthly_micro_usd`). |
| SHR-3 | P0 | A grantee must be able to route inference requests through an active shared key (when eligible under `allowed_models` and `max_monthly_micro_usd`) and view the shared key's label, provider type, health state, and remaining share cap in the UI. |
| SHR-4 | P0 | A grantee must not be able to re-share a shared key with a third user, edit the shared key's settings, view its secret or last-four characters, or view other users' usage on that key. |
| SHR-5 | P0 | When a share grant is revoked or its monthly spend cap is reached, the in-memory `ConfigSnapshot` must update within 1 s, and any active session pin binding the grantee's client key to that shared upstream key must re-pin on the next request. |
| SHR-6 | P0 | The personal key owner must see a per-grantee breakdown of requests, tokens, and cost consumed via each share grant on their key. |

### 6.5 Client keys (CLK)

Client keys authenticate coding agents to the data plane and carry user-defined routing preferences and safety caps.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| CLK-1 | P0 | Any `admin` or `user` must be able to generate client keys (`llr_` followed by 32 base62-encoded random bytes) scoped to their account, with a label, optional expiry timestamp, and optional `allowed_models` list. |
| CLK-2 | P0 | The full client key string must be displayed once at creation or rotation. The database stores only its SHA-256 hash (`key_hash`) and an 8-character prefix (`llr_xxxx`). |
| CLK-3 | P0 | Each client key must have a configurable `routing_scope` policy (`personal_first`, `blended`, `personal_only`, or `global_only`, defaulting to `personal_first`) that governs how the router combines the owner's personal, shared, and global upstream key pools. |
| CLK-4 | P0 | Revoking or disabling a client key must take effect within 1 s via the atomic `ConfigSnapshot` swap and reject later data-plane calls with HTTP 401. |
| CLK-5 | P1 | Each client key should support an optional request-per-minute rate limit, a monthly micro-USD budget cap, and an optional CIDR IP allow list. |

### 6.6 Model catalog and discovery (MDL)

The model catalog normalizes model identifiers across providers and determines which models are exposed to each user.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| MDL-1 | P0 | LLMRoller must discover available models for each enabled upstream key by calling the provider's `/models` endpoint on key creation, on manual refresh, and every 6 hours. |
| MDL-2 | P0 | Discovered upstream model IDs must be normalized to a `canonical_id` using the deterministic six-step algorithm in Section 10.6 (lowercase, vendor prefix and routing tags stripped, version separators unified without merging distinct version numbers) while preserving the exact `upstream_model_id` on the `model_routes` row for outbound requests. |
| MDL-3 | P0 | For the global pool, a model with `exposed = NULL` (automatic mode) is exposed by default when every enabled global provider with at least one enabled key serves it. |
| MDL-4 | P0 | An admin can force-enable (`exposed = 1`) any canonical model across the global catalog even if only a subset of global providers serve it, or force-disable (`exposed = 0`) any model, and can merge, split, or alias canonical model IDs. |
| MDL-5 | P0 | For a user with personal or shared keys, any model served by an enabled personal or shared key available to that user is also exposed to that user's client keys (unless disabled on that personal route by the user). |
| MDL-6 | P0 | Clients may request a canonical ID, a configured alias, or a raw upstream model ID; all three resolve to the same canonical model and route to the chosen upstream key's `upstream_model_id`. |
| MDL-7 | P0 | `GET /v1/models` must return the models accessible to the caller's client key (under its `allowed_models` and `routing_scope`) in OpenAI list format, or in Anthropic list format when the `anthropic-version` header is present. |

### 6.7 Routing and session pinning (RTE)

The routing engine pins existing conversations to their upstream key and distributes new conversations by weighted round-robin.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| RTE-1 | P0 | Inbound session IDs must be resolved by the nine-step precedence order in Section 11.1 and hashed with HMAC-SHA256 (`session_hash`). |
| RTE-2 | P0 | A session pin binds `(client_key_id, canonical_model_id, session_hash)` to one `upstream_key_id`. While the pinned route remains eligible for that client key, every turn in that session must route to that `upstream_key_id`. |
| RTE-3 | P0 | A route is eligible for a request only when: (a) the client key's owner is authorized to use the upstream key under the active `routing_scope` (via own personal ownership, active within-cap share grant, or allowed global pool access), (b) the provider, upstream key, and route are enabled, (c) both `upstream_keys.health` and `model_routes.route_health` are `healthy` (or their cooldown `health_until` has expired), (d) no budget window on the key or route is exhausted, (e) the provider circuit breaker is closed or half-open for a probe, and (f) the route natively supports the inbound wire protocol (`chat`, `responses`, or `messages`). |
| RTE-4 | P0 | When no usable pin exists, the router must select a route from the eligible pool (governed by `routing_scope`) using smooth weighted round-robin (`smooth_wrr`) weighted by effective priority. |
| RTE-5 | P0 | A route's effective priority is the user's or admin's per-model `priority_override` (`1` to `1000`) for that `(upstream_key_id, canonical_id)` pair if set, and otherwise the upstream key's default `priority`. |
| RTE-6 | P0 | Each canonical model (and personal routing profile) must support two distribution modes: `weighted` (default smooth WRR across all eligible routes) and `strict` (routes only to the highest-priority eligible tier, falling back to lower tiers only when higher tiers are ineligible). |
| RTE-7 | P0 | Concurrent initial requests for the same `(client_key_id, canonical_model_id, session_hash)` must coalesce onto a single provisional pin (`single-flight`) so parallel startup requests do not scatter across different upstream keys. |
| RTE-8 | P0 | A provisional pin is promoted to `confirmed` upon receiving the first valid upstream response byte, or deleted if the upstream call fails before the first byte. |
| RTE-9 | P0 | If an upstream call fails before the first response byte (connect error, timeout, 5xx, 429, auth error, or quota error), LLMRoller must fail over to up to two other eligible routes and pin the session to the route that succeeds. After the first byte is sent to the client, LLMRoller must never retry. |
| RTE-10 | P0 | Confirmed pins must expire after a configurable sliding idle TTL (default 24 hours, refreshed on each served turn) and persist to SQLite so restarts do not drop active pins. |
| RTE-11 | P0 | When a previously confirmed pin becomes ineligible, the next request must select a new eligible route, update the pin, increment `repin_count`, and mark the usage event with `pin_outcome = 'repin'`. |
| RTE-12 | P0 | For stateful OpenAI Responses requests, LLMRoller must record `response.id -> upstream_key_id` on completed responses and route any follow-up call carrying `previous_response_id` (or `GET` and `DELETE /v1/responses/{id}`) to that exact upstream key. |
| RTE-13 | P1 | Headroom mode should optionally scale each route's effective WRR weight by `max(0.05, remaining_budget_ratio)` so new sessions shift away from keys nearing a window cap. |

### 6.8 Transparent proxying (API)

The data plane forwards inference calls across all three supported protocols with minimal mutation and zero response buffering.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| API-1 | P0 | The data plane must serve `POST /v1/chat/completions`, `POST /v1/responses`, `POST /v1/messages`, `GET /v1/models`, `GET /v1/usage`, `GET /v1/key`, `GET /healthz`, and `GET /readyz`. |
| API-2 | P0 | Client authentication (`Authorization: Bearer llr_...` or `x-api-key: llr_...`) and hop-by-hop headers must be stripped and replaced with the chosen upstream key's credential header. |
| API-3 | P0 | All other client headers (`User-Agent`, `anthropic-version`, `anthropic-beta`, `openai-beta`, `originator`, `x-claude-code-*`, `x-codex-*`, `x-opencode-*`, `x-session-*`, `x-stainless-*`, `x-cmd-zdr`) must pass through unchanged unless listed in the provider's header strip list. |
| API-4 | P0 | Request JSON bodies must be forwarded with byte-identical message and tool ordering, changing only `model` (to `upstream_model_id`), the provider's configured session field (if absent), and `stream_options.include_usage: true` on streaming `chat` requests when needed. |
| API-5 | P0 | When LLMRoller injects `stream_options.include_usage: true` into a streaming `chat` request where the client did not set it, LLMRoller must consume the extra usage-only SSE chunk in the usage tap and omit it from the client's outbound stream. |
| API-6 | P0 | Responses must stream chunk-by-chunk without buffering, preserving upstream status codes, SSE framing, and headers while adding `x-llmroller-request-id`. |
| API-7 | P0 | If the downstream client disconnects during a request or stream, LLMRoller must abort the upstream HTTP request within 1 s. |
| API-8 | P0 | Client-caused 4xx upstream errors (400, 413, 422) must pass through unchanged without retry. Upstream 401 or 403 authentication errors on an upstream key must never return 401 to the client; they must mark the upstream key `invalid` (after two consecutive hits), trigger failover before the first byte, and return `502 upstream_unreachable` if all routes fail. |

### 6.9 Usage, budgets, and source provider `/usage` pass-through (USG & LIM)

The usage and budget subsystem combines local token and cost accounting with live source provider `/usage` polling, reconciliation, and client or UI pass-through.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| USG-1 | P0 | Every completed or failed request must emit a `usage_events` row recording timestamps, request ID, `client_key_id`, `owner_user_id`, `upstream_key_id`, `canonical_id`, `upstream_model_id`, protocol, `session_hash`, HTTP status, stream flag, `incomplete` flag, normalized token counts (`input_uncached`, `cache_read`, `cache_write`, `output`, `reasoning`), `cost_micro_usd`, `cost_source`, latency, TTFB, `pin_outcome`, and failover count. |
| USG-2 | P0 | Token extractor logic must normalize cache accounting differences: subtracting `cached_tokens` from `prompt_tokens` or `input_tokens` on OpenAI `chat` and `responses` so `input_uncached`, `cache_read`, and `cache_write` are mutually exclusive across all three protocols. |
| USG-3 | P0 | Request cost must use the provider-reported cost (`usage.cost` on OpenRouter) when present (`cost_source = 'provider'`), and otherwise compute integer micro-USD from the price book (`cost_source = 'price_book'`), applying context-length tiers and UTC peak or off-peak schedules. |
| USG-4 | P0 | For every provider with a supported or custom `/usage` endpoint (`GET /zen/go/v1/usage` on OpenCode Go, `GET /alpha/billing/credits` and `/alpha/usage/summary` on Command Code, `GET /api/v1/key` on OpenRouter, or a custom URL), LLMRoller must poll the endpoint on key test, on manual refresh, on a periodic schedule (default every 5 minutes for active keys), and upon 80% local window spend or a 429 or quota response. |
| USG-5 | P0 | Each upstream `/usage` poll must store an `upstream_usage_snapshots` record containing both the raw upstream JSON payload and normalized fields (`window_kind`, `used_percent`, `used_micro_usd`, `limit_micro_usd`, `remaining_micro_usd`, `resets_at`, `plan_name`, `fetched_at`). |
| USG-6 | P0 | When an upstream `/usage` snapshot reports authoritative window usage (`used_percent` or `used_micro_usd`) or an exact `resets_at` timestamp, the budget engine must reconcile the key's in-memory window counter and reset boundary with the provider's values, automatically marking the key `exhausted` at 100% (or >=98% admission threshold) until `resets_at`. |
| USG-7 | P0 | The data plane must serve `GET /v1/usage` and `GET /v1/key` authenticated by a client key. The response must include: (a) the client key's own usage and remaining limits, (b) normalized window status for the caller's eligible pool or pinned session key, and (c) a `source_provider_usage` pass-through object containing the raw and normalized upstream `/usage` payload when the request targets a pinned session (`?session_id=...` or session header) or a specific personal key owned by the caller (`?upstream_key_id=...`), with an optional `?fresh=1` live fetch parameter. |
| USG-8 | P0 | To protect multi-tenant privacy on `GET /v1/usage`, a standard user's client key must receive raw `source_provider_usage` payloads only for the user's own personal upstream keys; for shared personal keys and global keys, `GET /v1/usage` must return sanitized window availability (`status`, `used_percent`, `resets_at`) without exposing account-identifying fields (`whoami`, email, workspace ID, or billing details). |
| LIM-1 | P0 | Admins (for global keys) and users (for own personal keys) must be able to configure rolling (`5h`, `7d`, custom hours or days) and anchored (`1mo`, billing cycle anchor) budget windows in micro-USD on each upstream key and route, with a configurable admission reserve (default 2%) and manual calibration adjustments. |

### 6.10 Content traceability mode and session chat preview (TRC)

The content traceability subsystem lets an admin record full request and response contents. It also renders a read-only multi-turn chat preview per pinned session.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| TRC-1 | P0 | Content traceability mode must be disabled (`off`) by default. Only an `admin` can enable, scope (`all`, `users`, `client_keys`, or `models`), or disable content traceability mode (`PUT /admin/api/settings/traceability`), and every change must write an `audit_log` entry. |
| TRC-2 | P0 | When content traceability mode is active for a request, LLMRoller must asynchronously capture the inbound request and outbound response contents, extracting: system instructions, input messages, tool definitions, model parameters, assistant reasoning or thinking blocks, assistant tool calls (`id`, `name`, `arguments`), tool results, final assistant output text, and error payloads. |
| TRC-3 | P0 | For streaming SSE responses across `chat`, `responses`, and `messages`, the trace tap must reassemble chunked deltas (`delta.content`, `delta.reasoning_content`, `delta.tool_calls`, `response.output_text.delta`, `response.reasoning_summary_text.delta`, and `content_block_delta`) into a complete turn record without buffering or delaying the client stream (`PERF-2`). |
| TRC-4 | P0 | Each `trace_records` row must link to `usage_events.id`, `request_id`, `session_hash`, `client_key_id`, `owner_user_id`, and `turn_index` (ordered by timestamp `ts` within `session_hash`) so all turns of a pinned session form a deterministic sequence. |
| TRC-5 | P0 | The control plane must expose read-only session chat preview endpoints (`GET /admin/api/sessions/{session_hash}/chat` for admins and `GET /api/user/sessions/{session_hash}/chat` for a user's own sessions) that reconstruct the full multi-turn conversation by deduplicating repeated message prefixes across stateless turns and chaining `previous_response_id` turns. |
| TRC-6 | P0 | The dashboard must render a read-only Session Chat Preview screen and drawer displaying chronological turns with collapsible sections for system prompts, tool schemas, user prompts, reasoning blocks, tool call arguments, tool outputs, assistant responses, raw JSON toggle, and per-turn metrics (model, upstream key, TTFB, latency, tokens, cache hit rate, and cost). |
| TRC-7 | P0 | Admins must be able to set a dedicated retention window (`trace_retention_days`, default 14 days) and purge recorded trace contents globally or for a single `session_hash` (`DELETE /admin/api/traces`, `DELETE /admin/api/sessions/{session_hash}/trace`) without deleting the underlying token and cost rows in `usage_events`. |
| TRC-8 | P0 | Multi-tenant trace isolation: A standard `user` may view recorded traces and chat previews only for sessions created by their own `client_keys`. When Grantee B routes through a personal key shared by Grantor A, Grantor A must never have access to Grantee B's recorded prompts, reasoning, tool calls, or responses. |

### 6.11 Web UI and audit logging (UI & AUD)

The web UI and audit subsystem provide full self-service and administrative visibility without external JavaScript dependencies.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| UI-1 | P0 | The embedded web UI must provide role-aware navigation: admins see both the Admin Console (`/admin/*`) and Personal Workspace (`/app/*`), while standard users see the Personal Workspace (`/app/*`) covering Overview, Personal Providers & Keys, Shared With Me, Share Grants, Routing Overrides, Client Keys, Active Sessions, and Usage Explorer. |
| UI-2 | P0 | The UI must display both local estimated window spend and source provider `/usage` gauges side-by-side on each upstream key card, with a "Refresh `/usage`" button and a modal to inspect the raw source provider `/usage` JSON (for authorized owners and admins). |
| UI-3 | P0 | All HTML templates, CSS, HTMX, and chart scripts (`uPlot`) must be compiled into the Rust binary with zero external CDN requests. |
| UI-4 | P0 | When content traceability mode is active (or historical trace rows exist for a session), the Active Sessions and Usage Explorer screens must display a "Preview Chat" action that opens the read-only Session Chat Preview for that `session_hash`, plus a global banner showing when content traceability mode is active. |
| AUD-1 | P0 | Every state-changing action by an `admin` or `user` (login, account changes, provider or key creation, share grant or revocation, route override, calibration, pin move, client key creation or revocation, content traceability toggle, or admin inspection of another user's chat trace) must append a tamper-evident hash-chained row (`prev_hash`, `hash`) to `audit_log` without logging secrets. |

## 7. Non-functional requirements

LLMRoller must add less than 5 ms of proxy overhead at p99 before the first upstream byte. It must forward every streamed chunk without buffering and preserve pins and committed usage rows across restarts.

### 7.1 Latency and capacity (PERF)

Speed and capacity targets apply to a reference host with 2 vCPU and 2 GB RAM running Linux `x86_64` against a local stub upstream. The benchmark harness in Section 16 verifies each target before release.

| ID | Pri | Requirement specification | Verification method |
| --- | --- | --- | --- |
| PERF-1 | P0 | Added proxy latency before the first upstream byte must stay at or below 1 ms at p50 and 5 ms at p99 at 500 req/s across 100 concurrent streams. | Load test (`oha` or `k6`) against a zero-delay local stub upstream |
| PERF-2 | P0 | Each SSE chunk received from an upstream stream must reach the client socket write buffer within 1 ms at p99, with zero full-response buffering. | Timestamped chunk-forwarding test in `crates/proxy` |
| PERF-3 | P0 | The process must sustain 2,000 open SSE streams and 500 req/s within 300 MB of resident memory (`RSS`). | Soak test with 2,000 slow-dripping stub streams |
| PERF-4 | P0 | Idle resident memory must stay below 50 MB, and cold start time to readiness (`/readyz` HTTP 200) must stay below 2 s with 100,000 stored usage rows. | Startup benchmark in CI |
| PERF-5 | P0 | Enabling content traceability mode must add at most 0.5 ms to p99 TTFB and zero delay to SSE chunk forwarding (`PERF-2`). Compression (`zstd`) and AEAD encryption must run on a background worker with a 4 MiB cap per turn. | Load benchmark with `content_traceability_mode` enabled vs. disabled |

### 7.2 Reliability and availability (REL)

Reliability requirements govern crash recovery, graceful shutdown, fault isolation, and non-blocking usage persistence.

| ID | Pri | Requirement specification | Verification method |
| --- | --- | --- | --- |
| REL-1 | P0 | On `SIGTERM` or `SIGINT`, the server must stop accepting new connections and allow in-flight SSE streams up to 60 s (configurable) to finish before exiting. | Integration test sending `SIGTERM` mid-stream |
| REL-2 | P0 | An unexpected process kill (`SIGKILL`) must lose zero confirmed pins and zero usage rows committed more than 2 s earlier. | Crash-recovery integration test against SQLite WAL |
| REL-3 | P0 | A slow or locked usage writer must never block or fail an inference request. The usage tap uses a bounded channel (10,000 events) that drops the oldest event and increments `llmroller_usage_dropped_total` when full. | Fault-injection test stalling the SQLite writer task |
| REL-4 | P0 | All control-plane changes (new keys, shares, priority overrides, disabled routes, budget rules) must apply to new requests within 1 s without restarting the process. | Integration test verifying `ArcSwap<ConfigSnapshot>` propagation |
| REL-5 | P0 | One failing provider or upstream key must not slow down requests routed to healthy providers. Each provider has its own connection pool and circuit breaker. | Fault-injection test blackholing one upstream host |
| REL-6 | P1 | The CLI command `llmroller backup --out <path>` should write a consistent online snapshot using the SQLite Online Backup API while the server runs. | CLI integration test during concurrent write load |

### 7.3 Transparency and compatibility (TRN & COM)

Transparency requirements verify that neither the coding agent nor the upstream provider experiences broken wire behavior when talking through LLMRoller.

| ID | Pri | Requirement specification | Verification method |
| --- | --- | --- | --- |
| TRN-1 | P0 | For any request where `model` equals `upstream_model_id` and no session body field or `include_usage` flag needs injection, the outbound request body bytes must be byte-for-byte identical to the inbound body bytes. | Byte-identity property test in `crates/proxy` |
| TRN-2 | P0 | When LLMRoller edits the top-level JSON body (`model`, `prompt_cache_key`, `session_id`, or `stream_options.include_usage`), all other JSON fields (`messages`, `input`, `tools`, `system`, `metadata`) must retain their exact byte sequence and key order. | Golden byte-diff test on real agent payloads |
| TRN-3 | P0 | SSE event order, event names (`event:`), data lines (`data:`), and blank-line delimiters must reach the client unchanged (except for stripping an injected `include_usage` final chunk). | Streaming wire-fidelity test |
| COM-1 | P0 | [Claude Code](https://code.claude.com/docs/en/llm-gateway), [Codex CLI](https://github.com/openai/codex/issues/24973), [opencode](https://github.com/anomalyco/opencode/issues/55243), the official OpenAI Python and TypeScript SDKs, and the official Anthropic Python and TypeScript SDKs must complete multi-turn streaming tool-call workflows through LLMRoller. | End-to-end compatibility test suite |

### 7.4 Portability, maintainability, observability, and privacy (POR, MNT, OBS, PRIV)

Operational quality attributes keep the binary portable, auditable, observable, and private by default.

| ID | Pri | Requirement specification | Verification method |
| --- | --- | --- | --- |
| POR-1 | P0 | LLMRoller must compile and pass all tests on Linux (`x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`) and macOS (`aarch64-apple-darwin`), and ship a container image under 50 MB. | CI build matrix and image size gate |
| MNT-1 | P0 | All workspace crates must declare `#![forbid(unsafe_code)]` and pass `cargo clippy --all-targets -- -D warnings`. | CI lint gate |
| MNT-2 | P0 | Provider-specific endpoint paths, headers, error patterns, and `/usage` parsers must live inside `ProviderAdapter` implementations and preset TOML files; `crates/core` must contain zero provider-specific branches. | Architectural dependency check (`cargo-depgraph`) |
| OBS-1 | P0 | Every request must receive a unique ULID in `x-llmroller-request-id`, emitted in structured JSON logs and recorded in `usage_events`. Prometheus metrics must be served at `/metrics`. | Integration test inspecting headers, logs, and metrics |
| PRIV-1 | P0 | LLMRoller must emit zero telemetry, analytics, or crash reports to third parties. Outbound network connections may go only to configured provider hosts and, when explicitly enabled by an admin, the configured price-book catalog URL. | Network sandbox egress test |

## 8. Security and privacy

Warning: Do not expose the control-plane listener (`LLMROLLER_ADMIN_LISTEN`) to an untrusted network without TLS. Bind it to `127.0.0.1` or place it behind an authenticated VPN or reverse proxy.

LLMRoller holds high-value upstream credentials across global and personal accounts. The security design encrypts all upstream secrets and opt-in trace payloads at rest, hashes client keys and session IDs, and enforces strict tenant boundaries.

### 8.1 Threat model

Fifteen concrete threats drive the security controls in LLMRoller.

| ID | Threat description | Primary mitigations |
| --- | --- | --- |
| T1 | An attacker reads `llmroller.db` or a backup file from disk to steal upstream API keys. | `SEC-1`, `SEC-2`, `SEC-14` |
| T2 | An attacker races a fresh internet-facing installation to register the first admin account. | `AUTH-1`, `AUTH-2`, `AUTH-3`, `SEC-9` |
| T3 | A malicious user or compromised admin configures a custom provider URL pointing to `127.0.0.1`, `169.254.169.254`, or RFC 1918 internal services (SSRF). | `PRV-7`, `SEC-7` |
| T4 | An upstream API key, client key, session cookie, or user prompt leaks into application logs, panic traces, or error responses. | `SEC-6`, `SEC-14` |
| T5 | HTTP request smuggling or hop-by-hop header injection tricks the proxy into misrouting traffic or poisoning connections. | `SEC-15` |
| T6 | An attacker brute-forces web logins, TOTP codes, or client bearer keys. | `AUTH-8`, `SEC-3`, `SEC-4`, `SEC-12` |
| T7 | Cross-Site Request Forgery (CSRF) or Cross-Site Scripting (XSS) hijacks an admin or standard user session in the browser. | `SEC-5` |
| T8 | A compromised third-party crate in the dependency tree exfiltrates secrets (as occurred in the [March 2026 LiteLLM incident](https://docs.litellm.ai/blog/security-update-march-2026)). | `SEC-10`, `SEC-11`, `PRIV-1` |
| T9 | A leaked client key drains all global or shared upstream budgets. | `CLK-4`, `CLK-5`, `USR-5`, `SHR-2`, `LIM-1` |
| T10 | Two users sharing a global or shared upstream key infer prompt prefixes via cache-hit timing side channels. | `SEC-16` |
| T11 | A malicious upstream server returns a 301, 302, 307, or 308 redirect to exfiltrate the `Authorization` header to an attacker host. | `PRV-7`, `SEC-7` |
| T12 | Standard User A crafts control-plane API calls (IDOR) to read, edit, share, or route through User B's unshared personal provider or inspect User B's usage. | `USR-3`, `USR-4`, `SEC-17` |
| T13 | Grantee B abuses a shared personal key from Grantor A to extract A's raw API key or read A's personal account metadata via `GET /v1/usage`. | `SHR-4`, `USG-8`, `SEC-18` |
| T14 | An attacker tampers with SQLite `audit_log` rows to hide unauthorized configuration or share changes. | `AUD-1`, `SEC-13` |
| T15 | An attacker with disk access, a Grantor on a shared key, or a crafted model response reads sensitive prompts in `trace_records` or triggers XSS in the chat preview UI. | `TRC-8`, `SEC-19`, `SEC-5` |

### 8.2 Security requirements (SEC)

Every security requirement is mandatory (`P0`) for version 1.0 unless marked `P1`.

| ID | Pri | Requirement specification |
| --- | --- | --- |
| SEC-1 | P0 | All upstream key secrets must be encrypted with `XChaCha20-Poly1305` (192-bit random nonce per encryption, authenticated associated data `AAD = upstream_key.id || "|" || provider_id`) using a 256-bit domain-separated subkey derived via `HKDF-SHA256` from `LLMROLLER_MASTER_KEY` (using distinct HKDF `info` labels: `"llmroller.v1.upstream_key"`, `"llmroller.v1.totp_secret"`, `"llmroller.v1.trace_record"`, and `"llmroller.v1.session_hmac"`). Each row stores `master_key_id` to support atomic master key rotation (`llmroller rotate-master-key`). |
| SEC-2 | P0 | The master key must be supplied via `LLMROLLER_MASTER_KEY` or `LLMROLLER_MASTER_KEY_FILE` (32 raw bytes encoded in base64) and must never be written to SQLite or logs. The binary must refuse to start if the master key is missing or has fewer than 32 decoded bytes. |
| SEC-3 | P0 | Client keys (`llr_...`) must contain 256 bits of cryptographic randomness (`OsRng`), be stored solely as a SHA-256 digest, and be verified in constant time (`subtle::ConstantTimeEq`). |
| SEC-4 | P0 | User and admin passwords must be hashed with `Argon2id` (`m = 19456 KiB`, `t = 2`, `p = 1`, 128-bit random salt) following the [OWASP Password Storage Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html). |
| SEC-5 | P0 | Web session cookies must use the `__Host-llmroller_session` name (when TLS is active), `HttpOnly`, `Secure` (outside explicit `--dev-insecure-cookies`), `SameSite=Strict`, 12-hour idle expiry, and 7-day absolute expiry. Every state-changing request (`POST`, `PUT`, `PATCH`, `DELETE`) on cookie-authenticated routes must validate a per-session `x-csrf-token` header or form field. All HTML responses must set `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'`, `X-Content-Type-Options: nosniff`, and `X-Frame-Options: DENY`. |
| SEC-6 | P0 | Application logs, error bodies, and panic hooks must redact `Authorization`, `x-api-key`, `Cookie`, `Set-Cookie`, `secret`, and request or response payloads. Automated canary tests must verify that a planted secret string never appears in stdout, stderr, or database error columns. |
| SEC-7 | P0 | Outbound `reqwest` clients must enforce TLS 1.2+ with `rustls` certificate verification, disable automatic HTTP redirects (`redirect::Policy::none()`), send an upstream key only to the exact origin `(scheme, host, port)` of its provider's `base_url`, and resolve DNS through an SSRF filter that rejects loopback (`127.0.0.0/8`, `::1`), link-local (`169.254.0.0/16`, `fe80::/10`), CGNAT (`100.64.0.0/10`), and RFC 1918 ranges unless an admin explicitly enabled `allow_private_network` on that provider. |
| SEC-8 | P0 | The server must enforce configurable resource limits: max request body size (`32 MiB` default), max header block (`64 KiB`), max headers (`128`), upstream TCP connect timeout (`5 s`), upstream time-to-first-byte timeout (`300 s`), and stream idle timeout (`120 s`). |
| SEC-9 | P0 | The control plane (`LLMROLLER_ADMIN_LISTEN`) and metrics listener (`LLMROLLER_METRICS_LISTEN`) must bind to `127.0.0.1` by default, separate from the data-plane listener (`0.0.0.0:8080`). |
| SEC-10 | P0 | `Cargo.lock` must be committed. CI must run `cargo audit`, `cargo deny check advisories bans licenses sources`, and `cargo vet`. GitHub Actions workflows must pin third-party actions by full 40-character commit SHA. |
| SEC-11 | P0 | Release artifacts must include SHA-256 checksums, a CycloneDX SBOM, and a `cosign` signature. Container images must use a distroless base, run as UID `65532` (non-root), drop all Linux capabilities, and mount the root filesystem read-only. |
| SEC-12 | P0 | Failed client-key authentication requests must be rate-limited per source IP (default 20 failures per minute), and failed login or setup requests must be rate-limited per IP and username (default five failures per 15 minutes). |
| SEC-13 | P0 | Each `audit_log` row must compute `hash = hex(SHA256(canonical_frame(prev_hash, id, ts, actor_user_id, ip_addr, action, target_type, target_id, detail_json)))` using the length-prefixed framing in Section 9.2.1 so any deletion or edit of any column in historical audit records breaks the chain (`llmroller verify-audit-log`). |
| SEC-14 | P0 | All in-memory secret strings (master key, decrypted upstream API keys, raw client tokens, CSRF secrets) must be wrapped in `secrecy::SecretString` or `SecretBox<[u8]>` with `ZeroizeOnDrop`. |
| SEC-15 | P0 | The HTTP edge must reject requests containing both `Content-Length` and `Transfer-Encoding`, multiple `Host` headers, or non-ASCII control characters in header values, and must strip all hop-by-hop headers (`Connection`, `Keep-Alive`, `Proxy-Authenticate`, `Proxy-Authorization`, `TE`, `Trailer`, `Transfer-Encoding`, `Upgrade`) and `Set-Cookie` headers in both directions. |
| SEC-16 | P1 | When routing through a shared or global upstream key on a provider that uses a body or header cache key, LLMRoller should mix the caller's `owner_user_id` into the outbound synthetic affinity key (`HMAC(server_secret, owner_user_id || session_id)`) so two different users cannot collide on or probe each other's prompt cache. |
| SEC-17 | P0 | Every repository method in `crates/store` and handler in `crates/admin` that accesses providers, upstream keys, shares, routes, client keys, pins, or usage must take an `&ActorScope` parameter and enforce SQL-level `WHERE` predicates scoping rows to `owner_user_id` or `grantee_user_id` whenever `actor.role != Role::Admin`. |
| SEC-18 | P0 | Upstream `/usage` pass-through responses (`GET /v1/usage`, `GET /v1/key`, and `/api/user/upstream-keys/{id}/usage`) must strip raw provider account identifiers (email, organization ID, payment method, billing address, raw key prefix) whenever the caller is not the owner of a `personal` key or an `admin`. |
| SEC-19 | P0 | All recorded prompt, reasoning, tool-call, and completion payloads in `trace_records` must be compressed with `zstd` and encrypted at rest using `XChaCha20-Poly1305` (with a subkey derived via `HKDF-SHA256` from `LLMROLLER_MASTER_KEY` and `AAD` binding `request_id || owner_user_id || session_hash`). Rendering the read-only Session Chat Preview in the dashboard must sanitize all HTML and Markdown via `ammonia` (and strict DOM escaping under CSP) so prompt-injected script tags or event handlers never run in the browser. |

### 8.3 Privacy and operator compliance

LLMRoller is built for data minimization. It never stores prompt text, system instructions, tool arguments, or model completions unless an admin explicitly enables content traceability mode (`TRC-1`). When enabled, all recorded payloads are encrypted at rest (`SEC-19`), isolated to the client key owner (`TRC-8`), and purged after `trace_retention_days` (`TRC-7`). It never stores raw client session IDs; it hashes every session ID with a server-side HMAC secret before persisting pins or usage rows.

Because LLMRoller forwards non-credential headers unchanged, privacy headers sent by agents (such as Command Code's `x-cmd-zdr: 1` zero-data-retention header) reach the upstream provider intact. Operators and users who share keys must review each provider's terms of service on pooled access (tracked as Risk `R1` in Section 18).

## 9. Data model

LLMRoller stores all persistent state in a single SQLite database running in Write-Ahead Logging (`WAL`) mode with `NORMAL` synchronous commits and `FOREIGN_KEYS = ON`. All monetary amounts are stored as 64-bit signed integers representing micro-USD (`1 USD = 1,000,000 micro_usd`) to avoid floating-point rounding errors. All timestamps are stored as 64-bit integers representing Unix epoch milliseconds in UTC, and all primary keys use sortable 26-character ULIDs (or composite natural keys).

### 9.1 Complete SQLite DDL schema

The SQLite schema has 20 tables across five logical domains. These domains cover accounts, providers and sharing, models and pricing, session pins, and usage, traces, and operations.

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

-- 1. Accounts, Web Sessions, and Client Keys
CREATE TABLE users (
  id TEXT PRIMARY KEY,
  username TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('admin', 'user')),
  must_change_password INTEGER NOT NULL DEFAULT 0,
  totp_secret_enc BLOB,
  can_use_global_providers INTEGER NOT NULL DEFAULT 1,
  global_budget_micro_usd_month INTEGER,
  default_routing_scope TEXT NOT NULL DEFAULT 'personal_first'
    CHECK (default_routing_scope IN ('personal_first', 'blended', 'personal_only', 'global_only')),
  disabled INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  last_login_at INTEGER
);

CREATE TABLE web_sessions (
  id_hash TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  csrf_hash TEXT NOT NULL,
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
  routing_scope TEXT NOT NULL DEFAULT 'personal_first'
    CHECK (routing_scope IN ('personal_first', 'blended', 'personal_only', 'global_only')),
  allowed_models TEXT, -- JSON array of canonical_id strings, or NULL for all accessible models
  rate_limit_rpm INTEGER,
  budget_micro_usd_month INTEGER,
  allowed_cidrs TEXT, -- JSON array of CIDR strings, or NULL
  disabled INTEGER NOT NULL DEFAULT 0,
  revoked_at INTEGER,
  expires_at INTEGER,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER
);
CREATE INDEX idx_client_keys_owner ON client_keys(owner_user_id);

-- 2. Global and Personal Providers, Upstream Keys, Shares, Budgets, and Upstream /usage Snapshots
CREATE TABLE providers (
  id TEXT PRIMARY KEY,
  ownership_scope TEXT NOT NULL CHECK (ownership_scope IN ('global', 'personal')),
  owner_user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('opencode_go', 'command_code', 'open_design_amr', 'openrouter', 'custom')),
  name TEXT NOT NULL,
  base_url TEXT NOT NULL,
  config_json TEXT NOT NULL, -- protocol paths, auth style, header strip/add, UA policy, session profile, usage profile, error classifiers
  allow_private_network INTEGER NOT NULL DEFAULT 0,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  CHECK (
    (ownership_scope = 'global' AND owner_user_id IS NULL) OR
    (ownership_scope = 'personal' AND owner_user_id IS NOT NULL)
  ),
  UNIQUE (ownership_scope, owner_user_id, name)
);
CREATE INDEX idx_providers_owner ON providers(owner_user_id);

CREATE TABLE upstream_keys (
  id TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  owner_user_id TEXT REFERENCES users(id) ON DELETE CASCADE, -- NULL for global provider keys, matches provider.owner_user_id for personal keys
  label TEXT NOT NULL,
  secret_enc BLOB NOT NULL,
  master_key_id TEXT NOT NULL,
  secret_last4 TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  priority INTEGER NOT NULL DEFAULT 100 CHECK (priority BETWEEN 1 AND 1000),
  strict_tier INTEGER NOT NULL DEFAULT 1 CHECK (strict_tier BETWEEN 1 AND 10),
  health TEXT NOT NULL DEFAULT 'healthy' CHECK (health IN ('healthy', 'cooling_down', 'exhausted', 'invalid')),
  health_until INTEGER,
  last_error TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_upstream_keys_provider ON upstream_keys(provider_id);
CREATE INDEX idx_upstream_keys_owner ON upstream_keys(owner_user_id);

CREATE TABLE provider_shares (
  id TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  upstream_key_id TEXT REFERENCES upstream_keys(id) ON DELETE CASCADE, -- NULL means all enabled keys in provider_id are shared
  owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  grantee_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  allowed_models TEXT, -- JSON array of canonical_id strings, or NULL for all models served
  max_monthly_micro_usd INTEGER,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER,
  CHECK (owner_user_id != grantee_user_id)
);
CREATE INDEX idx_provider_shares_grantee ON provider_shares(grantee_user_id, enabled);
CREATE INDEX idx_provider_shares_owner ON provider_shares(owner_user_id);

CREATE TABLE budget_windows (
  id TEXT PRIMARY KEY,
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('upstream_key', 'route')),
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  canonical_id TEXT REFERENCES models(canonical_id) ON DELETE CASCADE,
  window_kind TEXT NOT NULL CHECK (window_kind IN ('rolling', 'anchored')),
  period TEXT NOT NULL, -- '5h', '7d', '1mo', or integer milliseconds as text
  anchor_at INTEGER,
  limit_micro_usd INTEGER NOT NULL,
  reserve_pct INTEGER NOT NULL DEFAULT 2 CHECK (reserve_pct BETWEEN 0 AND 50),
  CHECK (
    (scope_kind = 'upstream_key' AND canonical_id IS NULL) OR
    (scope_kind = 'route' AND canonical_id IS NOT NULL)
  )
);
CREATE INDEX idx_budget_windows_key ON budget_windows(upstream_key_id);

CREATE TABLE budget_adjustments (
  id TEXT PRIMARY KEY,
  budget_window_id TEXT NOT NULL REFERENCES budget_windows(id) ON DELETE CASCADE,
  actor_user_id TEXT NOT NULL REFERENCES users(id),
  delta_micro_usd INTEGER NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('manual_calibration', 'upstream_usage_sync')),
  note TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_budget_adjustments_window ON budget_adjustments(budget_window_id, created_at);

CREATE TABLE upstream_usage_snapshots (
  id TEXT PRIMARY KEY,
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  fetched_at INTEGER NOT NULL,
  http_status INTEGER NOT NULL,
  plan_name TEXT,
  windows_json TEXT NOT NULL, -- Normalized array: [{"kind":"5h","used_pct":12.5,"used_micro_usd":null,"limit_micro_usd":null,"resets_at":1792400000000}]
  balance_micro_usd INTEGER,
  raw_json TEXT NOT NULL, -- Exact JSON returned by source provider /usage endpoint
  sanitized_json TEXT NOT NULL -- Sanitized JSON stripped of PII and account IDs for grantee or shared inspection
);
CREATE INDEX idx_upstream_usage_key_ts ON upstream_usage_snapshots(upstream_key_id, fetched_at DESC);

-- 3. Model Catalog, Routes, User Route Overrides, and Price Book
CREATE TABLE models (
  canonical_id TEXT PRIMARY KEY,
  display_name TEXT,
  exposed INTEGER CHECK (exposed IN (0, 1)), -- NULL = auto intersection rule for global pool, 0 = forced off, 1 = forced on
  distribution TEXT NOT NULL DEFAULT 'weighted' CHECK (distribution IN ('weighted', 'strict'))
);

CREATE TABLE model_aliases (
  alias TEXT PRIMARY KEY,
  canonical_id TEXT NOT NULL REFERENCES models(canonical_id) ON DELETE CASCADE,
  confirmed INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE model_routes (
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  canonical_id TEXT NOT NULL REFERENCES models(canonical_id) ON DELETE CASCADE,
  upstream_model_id TEXT NOT NULL,
  protocols TEXT NOT NULL, -- JSON array: ["chat","responses","messages"]
  enabled INTEGER NOT NULL DEFAULT 1,
  priority_override INTEGER CHECK (priority_override BETWEEN 1 AND 1000),
  route_health TEXT NOT NULL DEFAULT 'healthy'
    CHECK (route_health IN ('healthy', 'cooling_down', 'exhausted', 'unavailable')),
  route_health_until INTEGER,
  last_error TEXT,
  discovered_at INTEGER NOT NULL,
  PRIMARY KEY (upstream_key_id, canonical_id)
);

CREATE TABLE user_route_preferences (
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  canonical_id TEXT NOT NULL REFERENCES models(canonical_id) ON DELETE CASCADE,
  priority_override INTEGER NOT NULL CHECK (priority_override BETWEEN 1 AND 1000),
  enabled INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (user_id, upstream_key_id, canonical_id)
);

CREATE TABLE prices (
  provider_kind TEXT NOT NULL,
  upstream_model_id TEXT NOT NULL,
  input_per_mtok_micro_usd INTEGER NOT NULL,
  output_per_mtok_micro_usd INTEGER NOT NULL,
  cache_read_per_mtok_micro_usd INTEGER NOT NULL DEFAULT 0,
  cache_write_per_mtok_micro_usd INTEGER NOT NULL DEFAULT 0,
  tiers_json TEXT, -- Optional prompt token threshold overrides
  schedule_json TEXT, -- Optional UTC peak and off-peak multipliers
  source TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (provider_kind, upstream_model_id)
);

-- 4. Session Pins and Stateful Response Links
CREATE TABLE pins (
  client_key_id TEXT NOT NULL REFERENCES client_keys(id) ON DELETE CASCADE,
  session_hash TEXT NOT NULL,
  canonical_id TEXT NOT NULL REFERENCES models(canonical_id) ON DELETE CASCADE,
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  status TEXT NOT NULL CHECK (status IN ('provisional', 'confirmed')),
  created_at INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  repin_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (client_key_id, session_hash, canonical_id)
);
CREATE INDEX idx_pins_expires ON pins(expires_at);
CREATE INDEX idx_pins_upstream_key ON pins(upstream_key_id);

CREATE TABLE response_links (
  response_id TEXT PRIMARY KEY,
  client_key_id TEXT NOT NULL REFERENCES client_keys(id) ON DELETE CASCADE,
  upstream_key_id TEXT NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX idx_response_links_expires ON response_links(expires_at);

-- 5. Usage Events, Hourly Rollups, Audit Log, and Settings
CREATE TABLE usage_events (
  id TEXT PRIMARY KEY,
  ts INTEGER NOT NULL,
  request_id TEXT NOT NULL,
  client_key_id TEXT NOT NULL,
  owner_user_id TEXT NOT NULL,
  upstream_key_id TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  provider_scope TEXT NOT NULL CHECK (provider_scope IN ('global', 'personal', 'shared')),
  share_id TEXT,
  canonical_id TEXT NOT NULL,
  upstream_model_id TEXT NOT NULL,
  protocol TEXT NOT NULL CHECK (protocol IN ('chat', 'responses', 'messages')),
  session_hash TEXT,
  status INTEGER NOT NULL,
  stream INTEGER NOT NULL,
  incomplete INTEGER NOT NULL DEFAULT 0,
  input_uncached_tokens INTEGER,
  cache_read_tokens INTEGER,
  cache_write_tokens INTEGER,
  output_tokens INTEGER,
  reasoning_tokens INTEGER,
  cost_micro_usd INTEGER,
  cost_source TEXT CHECK (cost_source IN ('provider', 'price_book', 'unpriced')),
  latency_ms INTEGER,
  ttfb_ms INTEGER,
  pin_outcome TEXT CHECK (pin_outcome IN ('new_pin', 'followed', 'repin', 'unpinned_stateless', 'response_link')),
  failovers INTEGER NOT NULL DEFAULT 0,
  error_code TEXT
);
CREATE INDEX idx_usage_ts ON usage_events(ts);
CREATE INDEX idx_usage_owner_ts ON usage_events(owner_user_id, ts);
CREATE INDEX idx_usage_client_key_ts ON usage_events(client_key_id, ts);
CREATE INDEX idx_usage_upstream_key_ts ON usage_events(upstream_key_id, ts);
CREATE INDEX idx_usage_share_ts ON usage_events(share_id, ts);

CREATE TABLE usage_rollups_hourly (
  hour_ms INTEGER NOT NULL,
  owner_user_id TEXT NOT NULL,
  client_key_id TEXT NOT NULL,
  upstream_key_id TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  provider_scope TEXT NOT NULL CHECK (provider_scope IN ('global', 'personal', 'shared')),
  share_id TEXT NOT NULL DEFAULT '', -- Empty string '' when not routed via provider_shares
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

CREATE TABLE trace_records (
  usage_event_id TEXT PRIMARY KEY REFERENCES usage_events(id) ON DELETE CASCADE,
  request_id TEXT NOT NULL UNIQUE,
  ts INTEGER NOT NULL,
  owner_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  client_key_id TEXT NOT NULL REFERENCES client_keys(id) ON DELETE CASCADE,
  session_hash TEXT NOT NULL,
  turn_index INTEGER NOT NULL,
  canonical_id TEXT NOT NULL,
  protocol TEXT NOT NULL CHECK (protocol IN ('chat', 'responses', 'messages')),
  truncated_content INTEGER NOT NULL DEFAULT 0,
  payload_enc BLOB NOT NULL, -- XChaCha20-Poly1305(zstd(NormalizedTurnTrace + raw request and response JSON))
  master_key_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX idx_trace_session_ts ON trace_records(session_hash, ts ASC);
CREATE INDEX idx_trace_owner_ts ON trace_records(owner_user_id, ts DESC);
CREATE INDEX idx_trace_expires ON trace_records(expires_at);

CREATE TABLE audit_log (
  id TEXT PRIMARY KEY,
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

### 9.2 Storage invariants and write batching

Six storage rules keep SQLite fast and crash-safe under concurrent streaming load.

| Rule | Implementation mechanism |
| --- | --- |
| Single batching writer | Read queries run across a pool of eight read-only SQLite connections. All high-frequency writes go through one dedicated writer task that commits batches of up to 200 items or every 250 ms in one `BEGIN IMMEDIATE` transaction ordered strictly as: (1) `usage_events`, (2) `usage_rollups_hourly`, (3) `trace_records` (satisfying `FOREIGN KEY (usage_event_id)`), and (4) `pins`, `response_links`, and `client_keys.last_used_at`. |
| Envelope encryption | `upstream_keys.secret_enc` and `users.totp_secret_enc` store a 24-byte `XChaCha20` nonce plus `Poly1305` ciphertext and tag, with `upstream_key.id` (or `user.id`) bound in the AEAD Associated Data (`AAD`) so a ciphertext blob cannot be copied between rows. |
| Provider scope recording | Each `usage_events` row records `provider_scope` (`global`, `personal`, or `shared`) and `share_id` (when routed via `provider_shares`) at request time. Historical share accounting and monthly share caps stay exact even if a share is later modified. |
| Snapshot history cap | The `upstream_usage_snapshots` table keeps the most recent 100 snapshots per upstream key. Older snapshots are pruned by the retention task, allowing the dashboard and `/v1/usage` endpoint to read the latest upstream quota state in O(1) time. |
| Compressed and encrypted traces | When content traceability mode is active, `trace_records.payload_enc` stores `24-byte nonce || XChaCha20-Poly1305(zstd(json_bytes))` with `request_id || owner_user_id || session_hash` bound in the AEAD `AAD`. Compression happens on a background worker thread before the batch writer transaction. |
| Retention policies | Raw `usage_events` rows are kept for 400 days, `trace_records` for 14 days (configurable via `trace_retention_days`), `response_links` for 30 days, expired `pins` and `web_sessions` are swept every 60 s, and `usage_rollups_hourly` and `audit_log` rows are kept indefinitely. |

#### 9.2.1 Canonical `audit_log` hash-chain serialization

Nullable columns (`actor_user_id`, `ip_addr`, `target_id`, `detail_json`) can be `NULL` or contain delimiter characters. To prevent canonicalization collisions, `crates/store::audit` computes `audit_log.hash` (`SEC-13`) over a length-prefixed byte buffer. The genesis `prev_hash` for the first row is 64 ASCII zeros (`"0000000000000000000000000000000000000000000000000000000000000000"`).

```text
canonical_frame =
    "llmroller.audit.v1\0"
 || u32_be(len(prev_hash))       || utf8(prev_hash)
 || u32_be(len(id))              || utf8(id)
 || i64_be(ts)
 || u32_be(len(actor_user_id))   || utf8(coalesce(actor_user_id, ""))
 || u32_be(len(ip_addr))         || utf8(coalesce(ip_addr, ""))
 || u32_be(len(action))          || utf8(action)
 || u32_be(len(target_type))     || utf8(target_type)
 || u32_be(len(target_id))       || utf8(coalesce(target_id, ""))
 || u32_be(len(detail_json))     || utf8(coalesce(detail_json, ""))

hash = hex_lower(SHA256(canonical_frame))
```

### 9.3 JSON column schemas and runtime `settings` structures

Four SQLite columns (`providers.config_json`, `prices.tiers_json`, `prices.schedule_json`, and `settings.value_json`) store structured JSON documents. `crates/core` validates these documents with `serde` before persistence and compiles them into `ConfigSnapshot`.

#### 9.3.1 `providers.config_json` (`ProviderConfigJson`)

Every provider row stores a serialized `ProviderConfigJson` object. This object parameterizes `ProviderAdapter` without hard-coding vendor branches in `crates/core`.

```json
{
  "paths": {
    "chat": "/chat/completions",
    "responses": "/responses",
    "messages": "/messages",
    "models": "/models"
  },
  "auth": {
    "chat": "bearer",
    "responses": "bearer",
    "messages": "x_api_key_or_bearer"
  },
  "strip_headers": ["x-experimental-*"],
  "add_headers": {},
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
  "error_classifiers": {
    "quota_exhausted_substrings": ["quota_exceeded", "window_limit_reached", "insufficient_credits"],
    "plan_invalid_substrings": ["upgrade_required", "invalid_api_key"]
  }
}
```

#### 9.3.2 `prices.tiers_json` and `prices.schedule_json`

Context-length pricing tiers (`tiers_json`) and UTC time-of-day multipliers (`schedule_json`) use the following deterministic schemas:

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

#### 9.3.3 Runtime `settings` table keys

The `settings` table stores instance-wide runtime configuration keys. Admins can update these keys via `PATCH /admin/api/settings` or `PUT /admin/api/settings/traceability`.

| `settings.key` | Default `value_json` | Purpose and hot-reload target in `ConfigSnapshot` |
| --- | --- | --- |
| `pin_ttl_secs` | `86400` | Sliding idle expiration window in seconds for confirmed session pins (`RTE-10`). |
| `usage_poll_interval_secs` | `300` | Periodic polling interval in seconds for active upstream keys' `/usage` endpoints (`USG-4`). |
| `headroom_scaling_enabled` | `false` | Enables scaling WRR route weights by `max(0.05, remaining_budget_ratio)` (`RTE-13`). |
| `repin_on_personal_recovery` | `true` | When `true`, a `personal_first` session that fell back to a `global` key repins back to the owner's `personal` key once its cooldown or window resets. |
| `price_sync_catalog_url` | `{"enabled":false,"url":"https://models.dev/api.json"}` | Opt-in external price-book synchronization configuration (`PRIV-1`, `MDL-4`). |
| `drain_timeout_secs` | `60` | Maximum seconds to wait for open SSE streams to finish after `SIGTERM` (`REL-1`). |
| `traceability_config` | `{"enabled":false,"scope":"all","user_ids":[],"client_key_ids":[],"models":[],"retention_days":14,"auto_disable_at":null}` | Admin opt-in content traceability mode configuration (`TRC-1`, `TRC-7`). |

## 10. Routing design

The routing engine in `crates/core` is a pure, deterministic state machine with zero network or disk I/O on the critical path. It resolves the caller's session, checks pin eligibility, selects new routes by smooth weighted round-robin, and manages failovers before the first byte.

### 10.1 Candidate eligibility rules

Consider a request authenticated by `client_key` (owned by `user_id = U`) for canonical model `M` over protocol `P`. A route `R = (upstream_key K, canonical_model M)` is a valid candidate only when all eight conditions below hold.

| Check # | Condition | Source in `ConfigSnapshot` and runtime state |
| --- | --- | --- |
| 1 | Model `M` is permitted by `client_key.allowed_models` (or `allowed_models` is `NULL`). | `client_keys.allowed_models` |
| 2 | User `U` has authorization to use key `K` under `client_key.routing_scope`: (a) `K` belongs to a `personal` provider with `owner_user_id == U`, (b) `K` is covered by an enabled `provider_shares` row with `grantee_user_id == U`, `M` in `share.allowed_models`, and current-month share spend below `share.max_monthly_micro_usd`, or (c) `K` belongs to a `global` provider, `U.can_use_global_providers == 1`, and `U`'s current-month global spend is below `U.global_budget_micro_usd_month`. | `providers`, `upstream_keys`, `provider_shares`, `users`, window counters |
| 3 | Model `M` is exposed in the pool being checked (for global routes, `M` satisfies the global auto-intersection or `exposed == 1` rule; for personal or shared routes, `R.enabled == 1`). | `models.exposed`, `model_routes.enabled`, `user_route_preferences.enabled` |
| 4 | Provider, upstream key `K`, and route `R` all have `enabled == 1`. | `providers`, `upstream_keys`, `model_routes` |
| 5 | Route `R.protocols` includes the inbound request protocol `P` (`chat`, `responses`, or `messages`). | `model_routes.protocols` |
| 6 | Both key health `K.health` and route health `R.route_health` are `healthy` (or `cooling_down` where `now >= health_until`). | In-memory key and route health tables |
| 7 | The provider's circuit breaker is `Closed` (or `HalfOpen` with a free probe slot). | In-memory circuit breaker |
| 8 | Every budget window attached to key `K` or route `(K, M)` has `used_micro_usd + reserve_micro_usd < limit_micro_usd` AND the latest reconciled `upstream_usage_snapshot` has `used_percent < (100 - reserve_pct)`. | In-memory window counters and `upstream_usage_snapshots` |

### 10.2 Pool resolution by `routing_scope`

After filtering routes that pass Checks 1 through 8, the router splits candidates into `Pool_Personal_And_Shared` and `Pool_Global`. It then applies `client_key.routing_scope` according to the table below.

| Routing scope mode | Pool selection rule |
| --- | --- |
| `personal_first` (default) | If `Pool_Personal_And_Shared` is non-empty, use `Pool_Personal_And_Shared`. Otherwise use `Pool_Global`. |
| `blended` | Use the union of `Pool_Personal_And_Shared` and `Pool_Global`. |
| `personal_only` | Use `Pool_Personal_And_Shared` only. |
| `global_only` | Use `Pool_Global` only. |

If the resulting candidate set `C` is empty, the router returns HTTP `503 Service Unavailable` (`no_eligible_route`). The response includes `Retry-After` set to the earliest recovery timestamp among excluded routes.

### 10.3 Effective priority and smooth weighted round-robin

For each candidate route `r` in the active candidate set `C`, the effective weight `W(U, r)` is resolved in three steps:
1. If user `U` configured a personal override in `user_route_preferences(U, r.upstream_key_id, M)`, use `user_route_preferences.priority_override`.
2. Else if route `r` has a `model_routes.priority_override`, use `model_routes.priority_override`.
3. Otherwise use `upstream_keys.priority`.

When `models.distribution == 'strict'` (or when keys in `C` use different `strict_tier` values), the router filters `C` first. It keeps only the candidates in the highest priority tier before running weighted selection.

For a new session, the router runs nginx's deterministic smooth weighted round-robin (`smooth_wrr`) scoped to `(pool_scope_key, canonical_model_id)`:
1. For each candidate `r` in `C`, increment its running counter: `current_weight[r] += W(U, r)`.
2. Select the candidate `r_win` with the maximum `current_weight[r]` (breaking ties deterministically by `upstream_key_id`).
3. Subtract the sum of weights of all candidates in `C` from the winner: `current_weight[r_win] -= sum(W(U, r) for r in C)`.
4. Return `r_win`.

#### 10.3.1 Worked routing example

Consider three upstream keys with default priorities matching their general plan tiers: Open Design AMR (`100`), OpenCode Go (`60`), and Command Code GOAT (`30`). For `deepseek-v4.1-flash`, the monthly allowances follow the same order ($120, $80, $60), so no override is needed. For `mimo-v2.6-pro`, Command Code GOAT gives $20 while OpenCode Go gives $15, so the admin or user sets per-model overrides (`60`, `20`, `15`).

| Canonical model | Upstream key | Monthly allowance | Key default priority | Route priority override | Effective weight `W` | Share of new sessions |
| --- | --- | --- | --- | --- | --- | --- |
| `deepseek-v4.1-flash` | Open Design AMR | $120 | 100 | None | 100 | 52.6% (100 / 190) |
| `deepseek-v4.1-flash` | OpenCode Go | $80 | 60 | None | 60 | 31.6% (60 / 190) |
| `deepseek-v4.1-flash` | Command Code GOAT | $60 | 30 | None | 30 | 15.8% (30 / 190) |
| `mimo-v2.6-pro` | Open Design AMR | $60 | 100 | 60 | 60 | 63.2% (60 / 95) |
| `mimo-v2.6-pro` | Command Code GOAT | $20 | 30 | 20 | 20 | 21.1% (20 / 95) |
| `mimo-v2.6-pro` | OpenCode Go | $15 | 60 | 15 | 15 | 15.8% (15 / 95) |

### 10.4 Session pin lifecycle and single-flight concurrency

Session pins live in an in-memory `moka::sync::Cache` keyed by `PinKey = (client_key_id, canonical_id, session_hash)` and backed by asynchronous write-behind to the SQLite `pins` table.

| Lifecycle stage | Behavior and concurrency rule |
| --- | --- |
| Single-flight provisional pin (`RTE-7`) | When two or more parallel requests arrive simultaneously for a new session, `moka::Cache::try_get_with` runs the route selector once and inserts a `provisional` pin so all concurrent turn-1 requests route to the same upstream key. |
| Confirmation on first byte (`RTE-8`) | When the upstream responds with HTTP 200 and yields its first body byte, the proxy marks the pin `confirmed` and updates `last_used_at` and `expires_at = now + pin_ttl_ms` (default 24 h). |
| Repin on ineligibility (`RTE-11`) | If a later turn finds that the pinned route `(upstream_key_id, canonical_id)` fails any eligibility check in Section 10.1 (including an exhausted window, a cooling key or route, a revoked share, or a changed `client_key.routing_scope` that now forbids the pinned key's `ownership_scope`), the router picks a new candidate via `smooth_wrr`, replaces the pin, increments `repin_count`, and records `pin_outcome = 'repin'`. |
| Stateful Responses override (`RTE-12`) | If a `POST /v1/responses` request includes `previous_response_id`, the router looks up `response_links(previous_response_id)`. If found and owned by the same `client_key_id`, the router routes directly to that `upstream_key_id` (`pin_outcome = 'response_link'`), bypassing `smooth_wrr`. |

### 10.5 Key health state machine and pre-first-byte failover

Each upstream key maintains an atomic health state and transitions according to upstream responses.

| Upstream outcome (before first byte) | Router failover action | Key health transition and circuit breaker effect |
| --- | --- | --- |
| HTTP 200 (first byte received) | Confirm pin and stream to client. | Key stays `healthy`; circuit breaker resets consecutive failure counter to 0. |
| TCP connect error, TLS error, or TTFB timeout | Delete provisional pin; retry on next eligible candidate (up to two retries). | Increment provider breaker failure count; set key to `cooling_down` for 30 s. |
| HTTP 500, 502, 503, 504, or 529 | Retry on next eligible candidate (up to two retries). | Increment provider breaker failure count; set key to `cooling_down` for `Retry-After` or exponential backoff (`5 s` to `300 s`). |
| HTTP 429 with `Retry-After <= 2 s` | Sleep `Retry-After` once and retry on the **same** pinned key to preserve warm prompt cache. | No state change if the retry succeeds; otherwise transition to `cooling_down`. |
| HTTP 429 (rate limit, `Retry-After > 2 s` or absent) | Retry on next eligible candidate (up to two retries). | Set key to `cooling_down` for `Retry-After` (or backoff starting at `15 s`). |
| HTTP 429, 402, or 403 matching provider quota-exhausted pattern | Retry on next eligible candidate; trigger immediate async upstream `/usage` poll. | Set key to `exhausted` until the earliest window `resets_at` (from `/usage` snapshot or local window). |
| HTTP 401 or 403 authentication failure (or Command Code `403 upgrade_required`) | Retry on next eligible candidate; never forward upstream 401 to client. | After two consecutive auth failures, set key to `invalid` until manual reset and raise a dashboard alert. |
| HTTP 400, 413, or 422 (invalid client request or context length exceeded) | Do not retry. Forward status and body directly to client. | Key stays `healthy`; no breaker failure counted. |
| Mid-stream disconnect after first byte | Never retry. Close client stream cleanly and mark usage event `incomplete = 1`. | Count soft breaker failure if upstream reset the connection. |

#### 10.5.1 Provider circuit breaker state machine and failure thresholds

Per-key health (`upstream_keys.health`) and per-route health (`model_routes.route_health`) isolate failures on a single credential or model. In addition, each `provider_id` maintains an atomic `ProviderCircuitBreaker` in `crates/core` to guard against host-wide outages (`REL-5`, `RTE-3`). Client errors (`400`, `413`, `422`), single-key rate limits (`429`), and single-key auth errors (`401`, `402`, `403`) never trip the provider breaker. Only transport errors and upstream `500`/`502`/`503`/`504` server errors count toward the provider breaker.

| Current state | Event or condition | Next state | Action taken |
| --- | --- | --- | --- |
| `Closed` | Upstream returns HTTP 200..=499 (any status other than transport timeout/error or 500/502/503/504) | `Closed` | Reset `consecutive_host_failures = 0`. |
| `Closed` | `consecutive_host_failures` reaches `5` within a rolling `60 s` window across the provider's keys | `Open` | Record `opened_at = now_ms` and `open_duration_ms = min(120_000, 15_000 * 2^open_streak)`. Exclude all routes on this provider from Check 7 (`RTE-3`). |
| `Open` | Request arrives while `now_ms < opened_at + open_duration_ms` | `Open` | Route immediately fails Check 7 with zero network I/O. |
| `Open` | Request arrives when `now_ms >= opened_at + open_duration_ms` and `probe_in_flight == false` | `HalfOpen` | Atomically set `probe_in_flight = true` (`compare_exchange`) and allow exactly one probe request through to the provider. Concurrent requests see `HalfOpen` with no free probe slot and route elsewhere. |
| `HalfOpen` | Probe request receives first valid response byte (HTTP 200) | `Closed` | Reset `consecutive_host_failures = 0`, `open_streak = 0`, and `probe_in_flight = false`. |
| `HalfOpen` | Probe request fails with transport error or HTTP 5xx before first byte | `Open` | Increment `open_streak`, set `probe_in_flight = false`, and re-open breaker for `min(120_000, 15_000 * 2^open_streak)`. |

### 10.6 Deterministic canonical model ID normalization algorithm

The function `crates/core::catalog::normalize_model_id(raw_id: &str) -> String` implements `MDL-2` and `ADR-7` in six deterministic steps:
1. Trim leading and trailing ASCII whitespace and convert all ASCII uppercase bytes (`A..=Z`) to lowercase (`a..=z`).
2. Strip any vendor prefix up to and including the last `/` character (for example, `deepseek/deepseek-v4.1-flash` becomes `deepseek-v4.1-flash`).
3. Strip aggregator routing tags (`:free`, `:extended`, `:nitro`, `:floor`), and convert `:thinking` into `-thinking` so reasoning variants stay distinct.
4. Replace all `_` characters and spaces with `-`, and collapse consecutive hyphens (`--`) into one hyphen (`-`).
5. Strip trailing date stamps (`-202[4-9][0-1][0-9][0-3][0-9]`) when a base family exists, while keeping distinct version numbers (`v4` vs. `v4.1`) separate (`ADR-7`).
6. Look up the resulting slug in `model_aliases`; return `model_aliases.canonical_id` when `confirmed = 1`, or the slug itself otherwise.

| Raw upstream `id` from provider `/models` | Provider kind | Normalized `canonical_id` (`MDL-2`) | Preserved `upstream_model_id` (`model_routes`) |
| --- | --- | --- | --- |
| `deepseek-v4.1-flash` | `open_design_amr` | `deepseek-v4.1-flash` | `deepseek-v4.1-flash` |
| `deepseek/deepseek-v4.1-flash` | `command_code` | `deepseek-v4.1-flash` | `deepseek/deepseek-v4.1-flash` |
| `deepseek-v4-flash` | `opencode_go` | `deepseek-v4-flash` (kept distinct per `ADR-7`) | `deepseek-v4-flash` |
| `anthropic/claude-sonnet-4.6` | `openrouter` | `claude-sonnet-4.6` (aliased with `claude-sonnet-4-6`) | `anthropic/claude-sonnet-4.6` |
| `claude-sonnet-4-6-20260514` | `command_code` | `claude-sonnet-4-6` | `claude-sonnet-4-6-20260514` |
| `openai/gpt-5.4-pro:free` | `openrouter` | `gpt-5.4-pro` | `openai/gpt-5.4-pro:free` |
| `mimo/mimo_v2.6_pro` | `command_code` | `mimo-v2.6-pro` | `mimo/mimo_v2.6_pro` |

## 11. Session identification and hints

Because coding agents and upstream providers use different session headers and body fields, LLMRoller separates inbound session extraction from outbound affinity hint injection.

### 11.1 Inbound session ID resolution order

The session resolver checks each inbound request using the nine-step precedence order below. It stops at the first non-empty printable ASCII value (`0x21..=0x7E`, up to 256 bytes).

| Precedence | Source location | Header or JSON field name | Target client or scenario |
| --- | --- | --- | --- |
| 1 | HTTP header | `x-llmroller-session-id` | Explicit override for custom scripts and tests |
| 2 | HTTP header | `x-claude-code-session-id` | [Claude Code](https://code.claude.com/docs/en/llm-gateway). Note: `x-claude-code-agent-id` and `x-claude-code-parent-agent-id` are ignored for pin lookup so subagents share the parent conversation's pin and prompt cache. |
| 3 | HTTP header | `x-parent-session-id` | [opencode](https://github.com/anomalyco/opencode/issues/55243) child or forked sessions, keeping child turns on the parent's warm cache |
| 4 | HTTP header | `x-opencode-session` | Clients already configured with OpenCode Go's session header |
| 5 | HTTP header | `x-session-affinity` | [opencode](https://github.com/anomalyco/opencode/pull/35982), Fireworks, and Vercel AI Gateway clients |
| 6 | HTTP header | `x-session-id`, `session-id`, `session_id`, `conversation-id`, `x-bf-session-id` | [Codex CLI](https://github.com/openai/codex/issues/24973), [OpenRouter](https://openrouter.ai/docs/guides/best-practices/prompt-caching), [Bifrost](https://docs.getbifrost.ai/providers/session-affinity.md), and generic gateways |
| 7 | JSON body | `session_id`, `prompt_cache_key`, `promptCacheKey`, or `metadata.session_id` | OpenAI, OpenRouter, and Claude Code metadata hints |
| 8 | JSON body | `previous_response_id` | Stateful OpenAI Responses continuation calls (resolves to the upstream key in `response_links`) |
| 9 | Derived fallback | `SHA-256(client_key_id || canonical_id || protocol || system_prompt[..4096] || first_user_message[..4096])` | Stateless SDK callers that send no session header or body field |

#### 11.1.1 Privacy hashing and anomaly detection

LLMRoller never writes the raw inbound session ID to SQLite. It computes `session_hash = hex(HMAC-SHA256(server_session_secret, raw_session_id)[0..16])` (128 bits, 32 hex characters).

A misconfigured client that sends a static constant session ID across all conversations defeats load balancing ([vscode#334186](https://github.com/microsoft/vscode/issues/334186)). To catch this, LLMRoller tracks a HyperLogLog sketch of distinct opening-message fingerprints per `session_hash`. If one `session_hash` appears across more than 20 distinct opening-message fingerprints within one hour, the Sessions UI flags the pin with a `suspected_constant_session_id` warning.

### 11.2 Outbound provider session hint profiles

Each provider has a configurable `session_profile` in `providers.config_json` that tells the adapter which headers or body fields to write into the outbound request.

| Rule | Outbound affinity behavior |
| --- | --- |
| Personal key passthrough | When routing through a user's own `personal` key, if the client sent a raw session ID, LLMRoller uses that ID (or a deterministic `llr_<26-char-base32>` token when using the Step 9 fallback fingerprint). |
| Tenant-salted shared and global IDs (`SEC-16`) | When routing through a `global` key or a `shared` personal key, LLMRoller derives the outbound affinity string as `llr_` plus 26 base32 characters of `HMAC-SHA256(server_session_secret, owner_user_id || raw_session_id)` so two different users on the same upstream key never share an affinity token while every turn from the same user and session sends an identical token. |
| Preserve existing headers (`send_if_absent`) | If the inbound request already carries the target header (and tenant salting is not active), LLMRoller leaves it untouched. |

The table below lists the default session hint profile for each built-in provider preset.

| Provider preset | Outbound headers written (`if_absent`) | Outbound body field written (`if_absent`) | Special rules and verification source |
| --- | --- | --- | --- |
| `opencode_go` | `x-opencode-session: <affinity_id>` | None | Mandatory on [OpenCode Go](https://opencode.ai/docs/go); prevents `400 MissingSessionID`. Also passes through `x-claude-code-session-id` and `session_id`. |
| `openrouter` | `x-session-id: <affinity_id>` | `session_id: "<affinity_id>"` (truncated to 256 chars) | Enables sticky provider routing from turn 1 ([OpenRouter prompt caching](https://openrouter.ai/docs/guides/best-practices/prompt-caching)). Never injects `provider.order`. |
| `command_code` | `x-session-affinity: <affinity_id>`, `x-session-id: <affinity_id>` | `prompt_cache_key: "<affinity_id>"` on `chat` and `responses` | [Command Code](https://commandcode.ai/docs/provider) documents no custom header; forwarding standard headers and `prompt_cache_key` preserves upstream cache hints. |
| `open_design_amr` | `x-session-affinity: <affinity_id>`, `x-session-id: <affinity_id>` | `prompt_cache_key: "<affinity_id>"` on `chat` and `responses` | Same safe default as Command Code. |
| `custom` | Configurable header list (default `x-session-id`, `x-session-affinity`) | Configurable (`off`, `prompt_cache_key`, or `session_id`) | Allows operators and users to match Fireworks, DeepInfra, vLLM Router, or LiteLLM upstream requirements. |

## 12. Protocol handling and transparent proxying

LLMRoller acts as a transparent wire proxy for OpenAI Chat Completions (`POST /v1/chat/completions`), OpenAI Responses (`POST /v1/responses`), and Anthropic Messages (`POST /v1/messages`). It modifies only what routing and usage accounting need.

### 12.1 Inbound and outbound header policy

The proxy applies a deterministic four-rule header pipeline to every request.

| Rule | Header names | Action taken |
| --- | --- | --- |
| 1. Strip hop-by-hop and proxy internal | `connection`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`, `te`, `trailer`, `transfer-encoding`, `upgrade`, `host`, `content-length`, `cookie`, `forwarded`, `x-forwarded-for`, `x-forwarded-host`, `x-forwarded-proto`, `x-real-ip`, `x-llmroller-*` | Removed unconditionally from the outbound request. `host` and `content-length` are set by `reqwest` for the target upstream. |
| 2. Strip provider deny-list | Any header matching a pattern in `provider.config.strip_headers` (for example, experimental `anthropic-beta` flags rejected by a specific upstream) | Removed when configured on that provider. |
| 3. Replace credentials | `authorization`, `x-api-key` | Stripped of the client's `llr_...` key and replaced with the decrypted upstream key in the provider's configured auth scheme (`Authorization: Bearer <secret>` or `x-api-key: <secret>`). |
| 4. Forward all other headers verbatim | `user-agent`, `accept`, `accept-encoding`, `content-type`, `anthropic-version`, `anthropic-beta`, `openai-beta`, `openai-organization`, `originator`, `x-claude-code-*`, `x-codex-*`, `x-opencode-*`, `x-session-*`, `x-stainless-*`, `x-cmd-zdr`, and any future agent headers | Forwarded unchanged in their original casing and order ([Claude Code Gateway Guide](https://code.claude.com/docs/en/llm-gateway)). |

### 12.2 User-Agent policy

Each provider configures `ua_policy` to one of three modes shown below.

| Mode | Policy behavior |
| --- | --- |
| `pass` (default for Command Code, Open Design AMR, OpenRouter) | Forwards the client's `User-Agent` header unchanged, or sends `llmroller/1.0` when the client sent no `User-Agent`. |
| `label` (default for OpenCode Go) | Forwards coding-agent `User-Agent` values (`claude-cli/*`, `codex-cli/*`, `opencode/*`, `hermes/*`) unchanged, and replaces generic HTTP library strings (`curl/*`, `python-requests/*`, `node-fetch/*`) with `llmroller/1.0` to satisfy [OpenCode Go's client identification guidelines](https://opencode.ai/docs/go) without impersonating another vendor. |
| `reject` | Returns HTTP 400 if the client sends an empty or generic library `User-Agent`. |

### 12.3 Surgical JSON body edits and prefix cache preservation

LLM providers compute prompt cache hashes over the exact serialized sequence of `system`, `tools`, and `messages` (or `input`). Re-serializing the entire JSON payload through an unordered `HashMap` would permute object keys and destroy prompt cache hits.

LLMRoller parses only top-level fields using a zero-copy span scanner (`serde_json::value::RawValue` over an ordered slice of `(Key, &RawValue)` pairs) and makes at most three surgical edits:
1. If `upstream_model_id` differs from the client's `model` string, replace the `model` string slice with `upstream_model_id`.
2. If the provider's `session_profile` specifies a body field (`prompt_cache_key` or `session_id`) with `if_absent` and the top-level keys do not contain it, append the field.
3. On a streaming `chat` request without `stream_options.include_usage: true`, inject that flag if the provider needs opt-in usage, and set `strip_injected_usage_chunk = true`.

When none of these three edits apply, LLMRoller forwards the original `bytes::Bytes` buffer untouched (`TRN-1`).

### 12.4 Zero-buffering SSE streaming tee and disconnect cancellation

When an upstream response starts, the forwarder wraps the `reqwest` byte stream in a custom `Pin<Box<dyn Stream<Item = Result<Bytes, Error>> + Send>>`:
1. Every `Bytes` chunk read from the upstream socket is yielded immediately to `axum::body::Body::from_stream` so hyper writes it to the client socket within 1 ms (`PERF-2`).
2. Before yielding a chunk, the stream adapter sends a reference-counted clone (`Bytes::clone` is an O(1) atomic increment) to the per-request `SseUsageTap` state machine.
3. If `strip_injected_usage_chunk` is `true` on a `chat` stream, the stream adapter inspects SSE `data:` frames that contain `"usage"` with `"choices":[]`; it feeds that frame to `SseUsageTap` and suppresses it from the client output so strict clients do not fail on an unexpected empty-choices chunk (`API-5`).
4. If the client closes its TCP connection mid-stream, `hyper` drops the response stream. This drops the `reqwest` future and closes the upstream request within 1 s (`API-7`).

#### 12.4.1 SSE chunk framing, partial-line holdback, and UTF-8 carryover state machine

TCP packets and HTTP/2 `DATA` frames can split an SSE stream at any byte offset. They can split a `data: {...}\n\n` line or a multi-byte UTF-8 character (`0xC2..=0xF4`). LLMRoller handles chunk boundaries with two decoupled state machines so client forwarding delay stays under 1 ms (`PERF-2`).

| Component | State machine behavior across arbitrary `Bytes` chunk splits |
| --- | --- |
| Outbound client stream (`crates/proxy::forward`) | **Case A (`strip_injected_usage_chunk == false`):** Forwards every `Bytes` chunk to the client socket immediately with zero line buffering. **Case B (`strip_injected_usage_chunk == true`):** Streams all chunks immediately while `finish_reason` has not yet been seen. Once an SSE frame containing a non-null `finish_reason` is forwarded, enters `PostFinishReasonHoldback` (holding at most 16 KiB until `\n\n`) to inspect the final `data:` frame: if it matches `"choices":[]` with a `"usage"` object, suppresses that frame from the client stream and forwards `data: [DONE]\n\n` (`API-5`). |
| Asynchronous tap (`SseUsageTap` and `TraceTap` in `crates/proto`) | Receives `Bytes::clone` chunks over an in-memory ring buffer (64 KiB max line buffer). Maintains a 1-to-3 byte `utf8_carryover: [u8; 3]` buffer for trailing incomplete UTF-8 codepoints across chunk boundaries, splits complete lines on `\n` (stripping optional `\r`), ignores SSE comment lines starting with `:`, and parses `event:` and `data:` lines into token counts and `NormalizedTurnTrace` deltas. |

### 12.5 Token and cost extraction across protocols

The `SseUsageTap` (for `text/event-stream`) and `JsonUsageExtractor` (for `application/json`) normalize token counts across the three wire protocols into five mutually exclusive integer fields.

| Normalized field | OpenAI `chat` formula | OpenAI `responses` formula | Anthropic `messages` formula |
| --- | --- | --- | --- |
| `input_uncached_tokens` | `max(0, usage.prompt_tokens - coalesce(usage.prompt_tokens_details.cached_tokens, 0))` | `max(0, usage.input_tokens - coalesce(usage.input_tokens_details.cached_tokens, 0))` | `usage.input_tokens` (already excludes cache reads and writes) |
| `cache_read_tokens` | `coalesce(usage.prompt_tokens_details.cached_tokens, 0)` | `coalesce(usage.input_tokens_details.cached_tokens, 0)` | `coalesce(usage.cache_read_input_tokens, 0)` |
| `cache_write_tokens` | `0` (not billed separately in OpenAI format) | `0` | `coalesce(usage.cache_creation_input_tokens, 0)` |
| `output_tokens` | `usage.completion_tokens` (includes reasoning) | `usage.output_tokens` (includes reasoning) | `usage.output_tokens` (from final `message_delta`) |
| `reasoning_tokens` | `coalesce(usage.completion_tokens_details.reasoning_tokens, 0)` | `coalesce(usage.output_tokens_details.reasoning_tokens, 0)` | Extracted when upstream includes extended thinking token metadata, else `0` |
| `provider_cost_micro_usd` | `round(usage.cost * 1_000_000)` when `usage.cost` is present | `round(usage.cost * 1_000_000)` when `usage.cost` is present | `round(usage.cost * 1_000_000)` when present |

Warning: OpenAI `prompt_tokens` and `input_tokens` include `cached_tokens`, whereas Anthropic `input_tokens` excludes `cache_read_input_tokens` and `cache_creation_input_tokens`. Subtracting `cached_tokens` on `chat` and `responses` before multiplying by `input_per_mtok_micro_usd` prevents double-charging cached tokens.

### 12.6 SSE delta reassembly and session chat reconstruction for content traceability

When content traceability mode is active (`TRC-2`, `TRC-3`), `SseUsageTap` and `JsonUsageExtractor` build a `NormalizedTurnTrace`. They also keep the raw request and response JSON.

| Protocol | Inbound fields parsed | Streamed SSE deltas reassembled into `NormalizedTurnTrace` |
| --- | --- | --- |
| OpenAI `chat` | `messages[]` (`system`, `developer`, `user`, `assistant`, `tool`), `tools[]`, `temperature`, `reasoning_effort` | Concatenates `choices[0].delta.content` into `assistant_text`, `choices[0].delta.reasoning_content` into `reasoning_text`, and merges `choices[0].delta.tool_calls[i]` by `index` into complete `ToolCall { id, name, arguments_json }` objects. |
| OpenAI `responses` | `instructions`, `input` (string or item array), `tools[]`, `previous_response_id`, `reasoning` | Accumulates `response.output_text.delta` events into `assistant_text`, `response.reasoning_summary_text.delta` into `reasoning_text`, and `response.function_call_arguments.delta` (or completed `function_call` items in `response.completed`) into `ToolCall` records. |
| Anthropic `messages` | `system` (string or block array), `messages[]` (`user` text/tool_result and `assistant` text/thinking/tool_use blocks), `tools[]`, `thinking` | Tracks active content blocks from `content_block_start` (`text`, `thinking`, `tool_use`) and appends `content_block_delta` (`text_delta`, `thinking_delta`, `input_json_delta`) until `content_block_stop`. |

All turns in one conversation share a pinned `session_hash`. The reconstructor (`crates/core::chat_preview`) builds a clean read-only chat timeline from the ordered `trace_records` rows (`turn_index = 1..N`).

| Turn pattern | Timeline reconstruction rule |
| --- | --- |
| Stateless turns (`chat` and `messages`) | Turn `k` resends prior messages `[m_1..m_p]` plus new user or `tool_result` blocks. The reconstructor hashes each block `(role, content_hash, tool_call_id)`, emits only new input blocks on turn `k`, and appends turn `k`'s `reasoning`, `tool_calls`, `assistant_text`, and telemetry badge. |
| Stateful turns (`responses` with `previous_response_id`) | Each turn already holds only new delta inputs and new assistant outputs, so the reconstructor links them directly in `turn_index` order. |
| Context compaction or forked turns | When the input prefix on turn `k` diverges from turn `k - 1`, the reconstructor inserts a `context_compacted_or_branched` divider card in the timeline. |

#### 12.6.1 `NormalizedTurnTrace` and Session Chat Preview JSON contract

Each `trace_records.payload_enc` blob decrypts and decompresses (`zstd`) to a `StoredTurnTrace` JSON document. The endpoints `GET /api/user/sessions/{session_hash}/chat` and `GET /admin/api/sessions/{session_hash}/chat` return a `SessionChatPreviewResponse` matching the schema below (`TRC-4`, `TRC-5`, `TRC-6`).

```json
{
  "session_hash": "9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c",
  "owner_user_id": "01J9X0USER...",
  "client_key_prefix": "llr_7f9a",
  "canonical_model": "claude-sonnet-4-6",
  "protocol": "messages",
  "total_turns": 2,
  "pagination": { "from_turn": 1, "limit": 50, "next_turn": null },
  "system_instructions": "You are an expert Rust systems engineer.",
  "tools": [
    { "name": "bash", "description": "Run a shell command", "parameters": { "type": "object" } }
  ],
  "timeline": [
    {
      "turn_index": 1,
      "request_id": "01J9X5REQ1...",
      "ts": 1792401000000,
      "pin_outcome": "new_pin",
      "upstream_key_label": "my-opencode-go-1",
      "provider_scope": "personal",
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

## 13. Pricing, limits, and source provider usage engine

LLMRoller combines a deterministic local micro-USD pricing and window-budget engine with live polling, reconciliation, and pass-through of each source provider's `/usage` API.

### 13.1 Local price book and integer micro-USD cost calculation

Every `(provider_kind, upstream_model_id)` pair in the `prices` table defines rates in micro-USD per 1,000,000 tokens (`input_per_mtok_micro_usd`, `output_per_mtok_micro_usd`, `cache_read_per_mtok_micro_usd`, and `cache_write_per_mtok_micro_usd`), plus two optional modifiers:
1. Context tiers (`tiers_json`) define replacement rates when `total_input_tokens = input_uncached + cache_read + cache_write` exceeds a threshold (such as the 200,000-token tier on OpenCode Go and Command Code).
2. Time-of-day schedules (`schedule_json`) define UTC weekday and hour ranges that apply a rate multiplier. For example, [OpenCode Go](https://opencode.ai/docs/go) and [Command Code](https://pi.dev/packages/pi-commandcode-provider) apply a 2x peak rate for DeepSeek models on weekdays from `01:00–04:00 UTC` and `06:00–10:00 UTC`.

For each request at start timestamp `ts_utc`, if the upstream response includes an explicit dollar cost (`usage.cost` on [OpenRouter](https://openrouter.ai/docs/cookbook/administration/usage-accounting)), LLMRoller records `cost_micro_usd = round(usage.cost * 1_000_000)` and `cost_source = 'provider'`. Otherwise, LLMRoller selects the active tier and schedule rate for `ts_utc` and computes the cost in integer micro-USD:

```
cost_micro_usd = div_ceil(
    input_uncached_tokens * rate_in
  + cache_read_tokens     * rate_cache_read
  + cache_write_tokens    * rate_cache_write
  + output_tokens         * rate_out,
  1_000_000
)
```

Using 128-bit intermediate integer arithmetic (`i128`) and ceiling division prevents integer overflow and floating-point drift.

### 13.2 Rolling and anchored budget window ring counters

Each `budget_windows` rule attaches either to an entire `upstream_key_id` (base plan cap) or to a specific `(upstream_key_id, canonical_id)` route (per-model allowance).

| Window kind | Supported periods | In-memory data structure | Reset semantics |
| --- | --- | --- | --- |
| `rolling` | `5h` (300 min), `7d` (168 h), or custom duration | Ring buffer of 300 one-minute buckets (for `<= 6h`) or 336 half-hour buckets (for `<= 7d`) | Sliding sum over the active buckets at timestamp `now`. Rebuilt from `usage_events` and `budget_adjustments` at startup. |
| `anchored` | `5h`, `7d`, `1mo` (calendar or billing anchor timestamp) | Single accumulator `used_micro_usd` plus `window_start_ms` and `resets_at_ms` | Resets to `0` when `now >= resets_at_ms` and advances `resets_at_ms` by one period. |

Built-in presets seed the documented plan windows automatically when a user or admin selects a plan template:
1. The OpenCode Go preset seeds rolling 5 h ($12), rolling 7 d ($30), and anchored 1 mo ($60).
2. The [Command Code GOAT](https://commandcode.ai/docs/plans/goat) preset seeds rolling 5 h ($14), rolling 7 d ($35), anchored 1 mo ($70), and per-model boosted route allowances.
3. The Open Design Go preset seeds rolling 5 h ($10), rolling 7 d ($30), and anchored 1 mo ($120).

#### 13.2.1 RAII in-flight budget reservation guard (`InFlightBudgetGuard`)

Concurrent requests arriving at 97.9% window spend could otherwise pass Check 8 (`Section 10.1`) together and overshoot a cap. To prevent this, `crates/core::windows` enforces an optimistic in-flight reservation using an RAII guard (`InFlightBudgetGuard`):
1. Check 8 evaluates `used_micro_usd + in_flight_reserved_micro_usd + reserve_micro_usd < limit_micro_usd` on every attached window and share grant.
2. Selecting a route atomically adds `default_in_flight_reservation_micro_usd` (`50_000` micro-USD = `$0.05`) to `in_flight_reserved_micro_usd` (`AtomicI64::fetch_add`) and returns an `InFlightBudgetGuard`.
3. When the stream finishes, fails over, or aborts, `InFlightBudgetGuard::drop` atomically subtracts `50_000` micro-USD (`fetch_sub`) and records the actual `cost_micro_usd`.

### 13.3 Source provider `/usage` polling, parsing, and auto-reconciliation

Local token accounting can drift from a provider's ledger when keys are used outside LLMRoller, when promotions apply, or when window boundaries shift. To remove drift, LLMRoller runs an upstream `/usage` poller and reconciler in each `ProviderAdapter`.

#### 13.3.1 Provider `/usage` adapter profiles

Each provider preset configures its upstream usage endpoints and normalizer rules.

| Provider kind | Polled endpoint(s) | Normalized fields extracted | Fallback when unavailable |
| --- | --- | --- | --- |
| `opencode_go` | `GET https://opencode.ai/zen/go/v1/usage` ([opencode#43983](https://github.com/anomalyco/opencode/issues/43983)) | Extracts `usage.rolling` (`5h`), `usage.weekly` (`7d`), and `usage.monthly` (`1mo`): `status`, `percent` (`0..=100`), and `resetsAt` (parsed to Unix ms). | If HTTP 404 or 5xx, keep local window counter and log warning. |
| `command_code` | `GET https://api.commandcode.ai/alpha/billing/credits` and `GET https://api.commandcode.ai/alpha/usage/summary` (with `/internal/billing/credits` fallback, [commandcode-goat-usagebar](https://open-vsx.org/extension/berkeduruu/commandcode-goat-usagebar)) | Extracts `5h`, `7d`, and `1mo` window percentages and credit balances, `plan` name, and billing period `resets_at`. | If alpha endpoints return 401 or 404, rely on local window counters and stream usage. |
| `openrouter` | `GET https://openrouter.ai/api/v1/key` ([OpenRouter Limits](https://openrouter.ai/docs/api_reference/limits)) | Extracts `data.usage` (total USD), `usage_daily`, `usage_weekly`, `usage_monthly`, `limit`, `limit_remaining`, and `limit_reset`. | Uses per-response `usage.cost` plus local window counters. |
| `open_design_amr` | `GET https://amr-link.open-design.ai/v1/usage` (configurable, off until [open-design#8674](https://github.com/nexu-io/open-design/issues/8674) ships) | Ready to parse `plan`, `windows[]` (`label`, `used_percent`, `resets_at`), and `wallet_balance_usd`. | Uses local window counters and manual calibration while endpoint returns 404. |
| `custom` | Optional `usage_url` and JSONPath mapping in `provider.config_json` | Extracts custom percentage, remaining USD, and reset timestamp fields. | Local window counters. |

#### 13.3.2 Polling triggers and reconciliation math

The `/usage` poller runs for an enabled upstream key under four triggers:
1. Immediately when the key is created or when the user clicks **Test** or **Refresh `/usage`**.
2. Every 5 minutes (configurable via `usage_poll_interval_secs`) for keys that served at least one request since the previous poll, and every 60 minutes for idle keys.
3. Asynchronously (once per 30 s max) when window spend crosses 80%, 90%, or 95%, or on HTTP 429 or 402.
4. On demand when a client calls `GET /v1/usage?fresh=1` (rate-limited to six live refreshes per minute per key).

When a valid snapshot arrives from the source provider, the budget engine applies three reconciliation rules:

| Reconciliation step | Action taken |
| --- | --- |
| Reset boundary sync | If the snapshot provides `resets_at` for a window (`5h`, `7d`, `1mo`), LLMRoller updates that window's `resets_at_ms` to match the provider's clock. |
| Usage reconciliation | If the snapshot reports `used_micro_usd` (or `used_percent`, converted via `round(used_percent / 100.0 * limit_micro_usd)`), LLMRoller compares `upstream_used` with `local_used`. If `abs(upstream_used - local_used) > 0.5% of limit`, LLMRoller inserts an automatic `budget_adjustments` row (`source = 'upstream_usage_sync'`, `delta_micro_usd = upstream_used - local_used`) so the window gauge and admission check match the provider's ledger. |
| Exhaustion state transition | If the snapshot reports `status != "ok"`, `used_percent >= (100 - reserve_pct)`, or `limit_remaining <= 0`, LLMRoller marks the key (or window) `exhausted` until `resets_at_ms`. If a poll after `resets_at_ms` reports `used_percent < (100 - reserve_pct)`, the key returns to `healthy`. |

#### 13.3.3 Strict allow-list PII sanitizer for `sanitized_json` (`USG-8`, `SEC-18`)

When saving an `upstream_usage_snapshots` row, `crates/proto::usage_poller` builds `sanitized_json` using a strict allow-list projection. This prevents newly added upstream fields from leaking account PII on shared or global keys:
1. Only the normalized quota structure (`{"provider_kind", "fetched_at", "plan_tier", "is_free_tier", "windows": [{"kind", "status", "used_percent", "resets_at"}]}`) is copied into `sanitized_json`.
2. Any raw key-value pair whose field name matches the case-insensitive regex `(email|user|whoami|name|org|workspace|account|billing|card|payment|invoice|ip|key|token|secret|id|customer)` is unconditionally stripped before constructing `sanitized_json`.

### 13.4 Source provider `/usage` pass-through API (`GET /v1/usage` and `GET /v1/key`)

Coding agents, IDE status bars ([CommandCode GOAT Usagebar](https://open-vsx.org/extension/berkeduruu/commandcode-goat-usagebar), [opencode#43983](https://github.com/anomalyco/opencode/issues/43983)), and CLI scripts can query LLMRoller for quota status using their `llr_...` client key.

| Endpoint and query parameters | Returned payload and pass-through behavior |
| --- | --- |
| `GET /v1/usage` (optional `?session_id=<id>`, `?model=<id>`, `?upstream_key_id=<id>`, `?fresh=0\|1`, or inbound session headers) | Returns a unified JSON object containing: (1) `client_key`: the caller's client key usage (24 h, 7 d, 30 d tokens and cost, plus remaining monthly budget), (2) `usage`: OpenCode-Go-compatible `rolling`, `weekly`, and `monthly` window objects (`status`, `percent`, `resetsAt`) representing the pinned upstream key (if a session matches an active pin) or the weighted aggregate of the caller's eligible pool, and (3) `source_provider_usage`: the latest `upstream_usage_snapshot` for the pinned or requested upstream key. |
| `GET /v1/key` (and `GET /v1/auth/key`) | Returns an [OpenRouter-compatible](https://openrouter.ai/docs/api_reference/limits) `{ "data": { "label", "usage", "usage_daily", "usage_weekly", "usage_monthly", "limit", "limit_remaining", "limit_reset", "is_free_tier", "llmroller": { ... } } }` payload so tools built for OpenRouter's key endpoint work unmodified against LLMRoller. |
| `GET /v1/usage/raw` (needs `?session_id=<id>` or `?upstream_key_id=<id>`) | Passes through the exact HTTP status and raw JSON body (`raw_json` for personal keys owned by the caller or admins; `sanitized_json` for shared or global keys) from the upstream provider's `/usage` endpoint. |

Example `GET /v1/usage?session_id=ses_99a` response when pinned to an OpenCode Go personal key:

```json
{
  "object": "llmroller.usage",
  "client_key": {
    "id": "01J9X2A...",
    "prefix": "llr_7f9a",
    "routing_scope": "personal_first",
    "usage_24h_micro_usd": 4120000,
    "usage_30d_micro_usd": 28450000,
    "budget_month_micro_usd": null
  },
  "pinned_route": {
    "upstream_key_id": "01J9X1B...",
    "key_label": "my-opencode-go-1",
    "provider_kind": "opencode_go",
    "ownership_scope": "personal",
    "canonical_model": "deepseek-v4.1-flash"
  },
  "usage": {
    "rolling": { "status": "ok", "percent": 18, "resetsAt": "2026-10-19T18:15:45.084Z" },
    "weekly": { "status": "ok", "percent": 42, "resetsAt": "2026-10-25T00:00:00.000Z" },
    "monthly": { "status": "ok", "percent": 31, "resetsAt": "2026-11-12T07:45:46.000Z" }
  },
  "source_provider_usage": {
    "provider_kind": "opencode_go",
    "fetched_at": 1792401200000,
    "sanitized": false,
    "raw": {
      "usage": {
        "rolling": { "status": "ok", "percent": 18, "resetsAt": "2026-10-19T18:15:45.084Z" },
        "weekly": { "status": "ok", "percent": 42, "resetsAt": "2026-10-25T00:00:00.000Z" },
        "monthly": { "status": "ok", "percent": 31, "resetsAt": "2026-11-12T07:45:46.000Z" }
      }
    }
  }
}
```

## 14. API and web UI specification

LLMRoller exposes three HTTP surfaces. The data plane (`/v1/*`) serves coding agents. The user control plane (`/app/*`, `/api/user/*`) serves personal resources, and the admin control plane (`/admin/*`, `/admin/api/*`) serves instance administration.

### 14.1 Data plane API (`/v1`)

All `/v1/*` routes except `/healthz` and `/readyz` authenticate using a client key (`llr_...`) passed in `Authorization: Bearer <key>` or `x-api-key: <key>`.

| Method and path | Protocol or purpose | Behavior summary |
| --- | --- | --- |
| `POST /v1/chat/completions` | OpenAI Chat Completions (`chat`) | Routes to an eligible upstream key supporting `chat`. Supports JSON and SSE (`stream: true`). |
| `POST /v1/responses` | OpenAI Responses (`responses`) | Routes to an eligible upstream key supporting `responses`. Honors `previous_response_id` affinity (`RTE-12`). |
| `GET /v1/responses/{id}`, `DELETE /v1/responses/{id}`, `POST /v1/responses/compact` | OpenAI Responses stateful operations | Looks up `response_links(id)` and forwards directly to the upstream key that created the response. |
| `POST /v1/messages` | Anthropic Messages (`messages`) | Routes to an eligible upstream key supporting `messages`. Supports JSON and SSE (`stream: true`). |
| `POST /v1/messages/count_tokens` | Anthropic token counting | Routes to the pinned or eligible `messages` upstream key. |
| `GET /v1/models` | Model discovery | Returns models accessible to the client key in OpenAI list shape, or in Anthropic list shape when `anthropic-version` is sent. |
| `GET /v1/usage` | Unified and pass-through usage | Returns client key usage, normalized window percentages (`rolling`, `weekly`, `monthly`), and `source_provider_usage` pass-through (`USG-7`). |
| `GET /v1/key`, `GET /v1/auth/key` | OpenRouter-compatible key usage | Returns OpenRouter key usage format plus LLMRoller pool metadata (`USG-7`). |
| `GET /v1/usage/raw` | Raw upstream `/usage` pass-through | Returns the upstream provider's raw `/usage` JSON (or sanitized JSON on shared and global keys, `USG-8`). |
| `GET /healthz`, `GET /readyz` | Process liveness and readiness | Returns HTTP 200 when the process is alive (`/healthz`) and when SQLite WAL and `ConfigSnapshot` are ready (`/readyz`). |

#### 14.1.1 Client configuration examples

Standard coding agents connect to LLMRoller by setting their base URL and API key environment variables, following the patterns documented by [Claude Code](https://code.claude.com/docs/en/llm-gateway-connect) and [OpenCode](https://opencode.ai/docs/go):

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

#### 14.1.2 Proxy error envelopes

Errors generated by LLMRoller itself use the caller's protocol envelope: `{"error":{"message": "...", "type": "...", "code": "..."}}` on OpenAI paths and `{"type":"error","error":{"type": "...", "message": "..."}}` on `/v1/messages`.

| HTTP status | Error code | Trigger condition |
| --- | --- | --- |
| 401 | `authentication_error` | Missing, malformed, disabled, expired, or revoked client key (`llr_...`). |
| 403 | `permission_error` | Requested model is excluded by `client_key.allowed_models`, CIDR filter, or client key monthly budget cap. |
| 404 | `model_not_found` | Requested model or alias does not exist or is not exposed to the caller's personal, shared, or global pool. |
| 413 | `request_too_large` | Request body exceeds `LLMROLLER_MAX_BODY_MB` (default 32 MiB). |
| 429 | `rate_limit_error` | Client key exceeded its configured `rate_limit_rpm`. |
| 502 | `upstream_unreachable` | All allowed failover retries failed before the first response byte due to network, TLS, or upstream auth errors. |
| 503 | `no_eligible_route` | Every route for the model is cooling down, exhausted, or disabled. Includes `Retry-After` in seconds. |

### 14.2 Control plane JSON API (`/api/user` and `/admin/api`)

All control plane endpoints authenticate via the `__Host-llmroller_session` cookie and validate `x-csrf-token` on mutating methods. Each handler supports content negotiation (`Accept: application/json` returns JSON; `HX-Request: true` returns an Askama HTML fragment). OpenAPI 3.1 specifications are served at `/api/openapi.json`.

#### 14.2.1 Self-service user API (`/api/user/*` for `user` and `admin` roles)

Every `/api/user/*` endpoint scopes queries and mutations to `actor.user_id`.

| Resource group | Endpoints and methods |
| --- | --- |
| Authentication and profile | `POST /api/auth/login`, `POST /api/auth/logout`, `GET /api/auth/me`, `POST /api/user/password`, `POST /api/user/totp/enroll`, `POST /api/user/totp/verify`, `DELETE /api/user/web-sessions/{id_hash}` |
| Personal providers | `GET /api/user/providers`, `POST /api/user/providers` (`ownership_scope = 'personal'`), `PATCH /api/user/providers/{id}`, `DELETE /api/user/providers/{id}` |
| Personal upstream keys | `GET /api/user/providers/{id}/keys`, `POST /api/user/providers/{id}/keys`, `PATCH /api/user/upstream-keys/{id}`, `DELETE /api/user/upstream-keys/{id}`, `POST /api/user/upstream-keys/{id}/test`, `POST /api/user/upstream-keys/{id}/reset-health`, `PUT /api/user/upstream-keys/{id}/windows`, `POST /api/user/upstream-keys/{id}/calibrate` |
| Source `/usage` inspection | `GET /api/user/upstream-keys/{id}/usage` (returns latest snapshot + history), `POST /api/user/upstream-keys/{id}/usage/refresh` (triggers live upstream `/usage` poll) |
| Personal provider sharing | `GET /api/user/shares/outgoing`, `POST /api/user/shares` (share personal provider or key with `grantee_username`), `PATCH /api/user/shares/{id}`, `DELETE /api/user/shares/{id}`, `GET /api/user/shares/incoming` (keys shared with the caller) |
| Personal route overrides | `GET /api/user/routes?model=`, `PUT /api/user/routes/{upstream_key_id}/{canonical_id}` (sets `user_route_preferences.priority_override` and `enabled`) |
| Client keys | `GET /api/user/client-keys`, `POST /api/user/client-keys` (returns raw `llr_...` once), `PATCH /api/user/client-keys/{id}`, `POST /api/user/client-keys/{id}/rotate`, `DELETE /api/user/client-keys/{id}` |
| Active session pins | `GET /api/user/pins` (caller's client keys only), `DELETE /api/user/pins/{client_key_id}/{session_hash}/{canonical_id}`, `POST /api/user/pins/move` |
| Session chat preview (read-only) | `GET /api/user/sessions/{session_hash}/chat` (reconstructed multi-turn chat timeline for caller's own session), `GET /api/user/traces/{request_id}` (single turn trace and raw JSON) |
| Scoped usage analytics | `GET /api/user/usage/summary`, `GET /api/user/usage/timeseries`, `GET /api/user/usage/breakdown`, `GET /api/user/usage/events`, `GET /api/user/usage/export.csv` |
| Scoped audit log | `GET /api/user/audit` (events where `actor_user_id == me` or target belongs to `me`) |

#### 14.2.2 Instance administration API (`/admin/api/*` for `admin` role only)

Every `/admin/api/*` endpoint rejects non-admin callers with HTTP 403.

| Resource group | Endpoints and methods |
| --- | --- |
| First-run setup | `GET /admin/api/setup/status`, `POST /admin/api/setup` (404 once any user exists) |
| User management | `GET /admin/api/users`, `POST /admin/api/users` (role `admin` or `user`, global pool permissions), `PATCH /admin/api/users/{id}`, `DELETE /admin/api/users/{id}`, `POST /admin/api/users/{id}/reset-password` |
| Global providers and keys | `GET /admin/api/providers`, `POST /admin/api/providers` (`ownership_scope = 'global'`), `PATCH /admin/api/providers/{id}`, `DELETE /admin/api/providers/{id}`, `POST /admin/api/providers/{id}/keys`, `PATCH /admin/api/upstream-keys/{id}`, `DELETE /admin/api/upstream-keys/{id}`, `POST /admin/api/upstream-keys/{id}/test`, `POST /admin/api/upstream-keys/{id}/usage/refresh`, `PUT /admin/api/upstream-keys/{id}/windows`, `POST /admin/api/upstream-keys/{id}/calibrate` |
| Model catalog and price book | `GET /admin/api/models`, `PATCH /admin/api/models/{canonical_id}` (`exposed`, `distribution`), `POST /admin/api/models/refresh`, `POST /admin/api/models/merge`, `PUT /admin/api/model-aliases/{alias}`, `DELETE /admin/api/model-aliases/{alias}`, `GET /admin/api/prices`, `PUT /admin/api/prices/{provider_kind}/{upstream_model_id}`, `POST /admin/api/prices/sync` |
| Global routing and pins | `GET /admin/api/routes`, `PATCH /admin/api/routes/{upstream_key_id}/{canonical_id}`, `GET /admin/api/pins` (all users), `DELETE /admin/api/pins`, `POST /admin/api/pins/move` |
| Content traceability & chat preview | `GET /admin/api/settings/traceability`, `PUT /admin/api/settings/traceability` (enable, scope, set retention TTL), `GET /admin/api/sessions/{session_hash}/chat`, `GET /admin/api/traces/{request_id}`, `DELETE /admin/api/sessions/{session_hash}/trace`, `DELETE /admin/api/traces` |
| Instance usage and audit | `GET /admin/api/usage/summary`, `GET /admin/api/usage/timeseries`, `GET /admin/api/usage/breakdown` (group by `user`, `client_key`, `upstream_key`, `provider`, `provider_scope`, `model`), `GET /admin/api/audit`, `PATCH /admin/api/settings` |

### 14.3 Web UI screens and metric definitions

The web interface is server-rendered with Askama templates and HTMX partial updates, using zero external CDN assets. Standard users see the **Personal Workspace** (`/app/*`); admins also see the **Admin Console** (`/admin/*`).

| Screen | Audience | Primary elements and actions |
| --- | --- | --- |
| Setup & Login | Public (unauthenticated) | First-admin setup form (with setup token) when zero users exist; username, password, and TOTP login form thereafter. |
| Overview Dashboard | `user` (scoped) & `admin` (global + personal) | Range selector (24 h, 7 d, 30 d, custom); cards for total requests, tokens, micro-USD cost, cache hit rate, pin follow rate, and repin rate; stacked daily cost chart (`uPlot`); upstream key cards showing both local estimated budget gauges and source provider `/usage` window gauges (`5h`, `7d`, `1mo`) with reset countdowns; health alerts. |
| Personal Providers & Keys | `user` & `admin` | Add personal provider from preset or custom URL; add personal upstream keys; Test button; Refresh `/usage` button; modal showing raw source provider `/usage` JSON; configure budget windows and manual calibration. |
| Sharing & Shared With Me | `user` & `admin` | **Outgoing Shares tab:** grant access to a personal provider or key to another username, set allowed models and monthly USD cap, view per-grantee token and dollar consumption, revoke share. **Shared With Me tab:** inspect keys shared by teammates, health badge, allowed models, and remaining monthly share cap. |
| Routing Matrix | `user` (personal + shared + global weights) & `admin` (global catalog) | Matrix of canonical models (rows) by upstream keys (columns). Displays protocol chips (`chat`, `responses`, `messages`), key default priority, inline editable per-model `priority_override`, effective weight, and projected percentage share of new sessions. |
| Client Keys | `user` (own) & `admin` (all) | Generate `llr_...` key (shown once with copy button), select `routing_scope` (`personal_first`, `blended`, `personal_only`, `global_only`), restrict models, set RPM or monthly USD cap, rotate, or revoke. |
| Active Sessions | `user` (own) & `admin` (all) | Live table (10 s HTMX poll) of active pins: session hash prefix, client key, model, pinned upstream key and scope badge (`personal`, `shared`, `global`), age, turn count, `repin_count`, constant-session warning badge, and **Preview Chat** button (when trace rows exist). Supports Unpin and Move Pin. |
| Session Chat Preview (Read-Only) | `user` (own sessions) & `admin` (all traced sessions) | Read-only conversation replay for a `session_hash`: renders system instructions, tool schemas, user prompts, collapsible reasoning/thinking blocks, formatted tool calls and tool outputs, assistant Markdown/code blocks (sanitized via `ammonia`), context-compaction markers, per-turn telemetry badges (TTFB, latency, tokens, cache hit ratio, cost, upstream key), and a raw request/response JSON inspector drawer. |
| Usage Explorer | `user` (own) & `admin` (all) | Filter by time range, user, client key, provider scope, upstream key, or model. Paged event table with status, TTFB, total latency, token breakdown, `cost_source` badge (`provider` vs. `price_book`), and CSV or JSON export. |
| Admin: Global Providers & Catalog | `admin` only | Manage global providers and keys, force-expose or hide models, edit canonical aliases, edit price book rates, and trigger catalog discovery. |
| Admin: Users, Settings & Audit | `admin` only | Create or disable `admin` and `user` accounts, set per-user global pool access and monthly global budget caps, configure instance settings (pin TTL, `/usage` poll interval, retention), and inspect the hash-chained audit log. |

## 15. Technology choices and workspace layout

LLMRoller is implemented in safe Rust (Rust 2024 edition) with a pinned stable toolchain and a small, audited dependency tree.

### 15.1 Core technology stack

Every dependency is chosen for predictable speed, memory safety, zero external runtime services, and supply-chain auditability.

| Layer | Selected crate or tool | Rationale | Rejected alternative |
| --- | --- | --- | --- |
| Language & toolchain | Rust (2024 edition, stable toolchain, `#![forbid(unsafe_code)]`) | Predictable tail latency on 2,000 concurrent SSE streams with no garbage-collector pauses and a single static binary. | Go (GC pauses under high stream counts) or Python (packaging and supply-chain exposure). |
| HTTP server | `axum 0.8`, `hyper 1.x`, `hyper-util`, `tower` on `tokio 1.x` | Standard async Rust HTTP stack with native streaming body support and composable middleware. | `actix-web` (separate actor runtime model). |
| HTTP client | `reqwest 0.13` (`rustls-tls`, `stream`, `http2`, `default-features = false`) | Connection pooling per provider, custom DNS resolver hook for SSRF filtering, and raw unbuffered chunk streaming. | `ureq` (blocking I/O unsuitable for 2,000 streams). |
| Database | SQLite in WAL mode via `sqlx 0.9` | Zero-configuration single-file persistence with compile-time and migration-tested SQL queries. | PostgreSQL (needs an external database server for single-node installs). |
| Cryptography | `chacha20poly1305`, `hkdf`, `sha2`, `hmac`, `argon2`, `totp-rs`, `subtle`, `secrecy`, `zeroize` | Pure-Rust audited RustCrypto implementations for XChaCha20-Poly1305 AEAD, Argon2id, HMAC-SHA256, and memory zeroization. | OpenSSL (`openssl-sys` C bindings complicate static musl builds). |
| In-memory state & caches | `arc-swap` (`ConfigSnapshot`), `moka` (`sync::Cache` for pins and response links), `dashmap` (WRR and window ring counters) | Lock-free atomic snapshot reads on the hot path and single-flight cache loaders for concurrent turn-1 pinning (`RTE-7`). | Global `RwLock<Config>` (writer starvation under 500 req/s). |
| Web UI & safe chat rendering | `askama`, `rust-embed`, `pulldown-cmark`, `ammonia`, vendored `htmx 2.x` and `uPlot` | Type-checked server templates and strict HTML/Markdown sanitization (`ammonia`) for the read-only Session Chat Preview with zero CDN requests. | React or Next.js SPA (adds a large `node_modules` supply chain). |
| Compression | `zstd` (`async-compression`) | High-ratio fast compression for recorded `trace_records` payloads before AEAD encryption, plus gzip/br stream decoding in the usage tap. | Uncompressed JSON blobs in SQLite. |
| Observability | `tracing`, `tracing-subscriber` (JSON formatter), `metrics`, `metrics-exporter-prometheus` | Structured JSON request logs with `request_id` correlation and low-overhead Prometheus counters and histograms. | Heavy OpenTelemetry gRPC exporters in v1.0 core. |
| Testing & verification | `cargo-nextest`, `proptest`, `wiremock`, `insta`, `cargo-llvm-cov`, `cargo-mutants`, `cargo-fuzz`, `cargo-deny`, `cargo-audit`, `cargo-vet` | Enforces fast parallel tests, property testing, wire stubbing, line coverage, and automated mutation testing ([cargo-mutants](https://mutants.rs/)). | Standard `cargo test` alone without mutation verification. |

### 15.2 Cargo workspace layout and architectural boundaries

The repository is structured as a six-crate Cargo workspace so the compiler enforces strict dependency direction and keeps the routing core free of I/O.

```
llmroller/
  Cargo.toml                  # Workspace manifest, shared lints (#![forbid(unsafe_code)])
  Cargo.lock                  # Committed lockfile for reproducible builds
  deny.toml                   # cargo-deny license, ban, and advisory rules
  .cargo/mutants.toml         # cargo-mutants configuration and timeout tuning
  presets/
    opencode_go.toml          # Built-in OpenCode Go preset (endpoints, session hint, /usage, prices)
    command_code.toml         # Built-in Command Code preset
    open_design_amr.toml      # Built-in Open Design AMR preset
    openrouter.toml           # Built-in OpenRouter preset
  fixtures/
    sse/                      # Recorded SSE streams (chat, responses, messages, truncated)
    usage/                    # Recorded source provider /usage JSON responses
  crates/
    core/                     # Pure domain logic (ZERO network or database dependencies):
                              #   models, ActorScope, RBAC rules, routing eligibility,
                              #   smooth WRR, pin state machine, price math, window rings
    proto/                    # Wire protocols and provider adapters:
                              #   surgical JSON span editor, header policy, SSE usage tap,
                              #   ProviderAdapter trait, /models & /usage response parsers
    store/                    # SQLite persistence and cryptography:
                              #   migrations, Repository traits, batching writer task,
                              #   XChaCha20-Poly1305 sealer, Argon2id, audit hash chain
    proxy/                    # Data plane HTTP service (:8080):
                              #   client key auth, SSRF DNS connector, stream forwarder,
                              #   pre-first-byte failover loop, /v1/usage pass-through
    admin/                    # Control plane HTTP service (127.0.0.1:8081):
                              #   /api/user/* and /admin/api/* handlers, CSRF & session
                              #   middleware, Askama templates, embedded static assets
    server/                   # Binary entrypoint and background supervisor:
                              #   CLI args, config loader, listener lifecycle, SIGTERM drain,
                              #   discovery job, upstream /usage poller, retention sweeper
```

`crates/core` depends only on `serde`, `time`, `ulid`, `sha2`, `hmac`, `moka`, and `dashmap`. It cannot import `tokio::net`, `reqwest`, or `sqlx`. This boundary guarantees that every routing, sharing, eligibility, pinning, and budget rule can be unit-tested and mutation-tested in milliseconds without sockets or disk files.

## 16. Testing plan and TDD governance

Testing in LLMRoller is a primary architectural constraint, never an afterthought written after code lands. Every subsystem is built under strict Test-Driven Development (TDD) with bidirectional traceability between requirements, automated tests, and source modules. The test suite combines component mocking, integration tests, regression tests, end-to-end tests, and mutation testing with [cargo-mutants](https://mutants.rs/).

### 16.1 Testing boundary discipline: what we test vs. what we do not test

A disciplined test suite tests the logic that the engineering team controls and owns. Do not write tests that merely check the Rust standard library, the Tokio scheduler, SQLite's internal B-tree pager, `hyper` framing, or static TOML files. Over-testing external runtimes wastes CI time and creates brittle noise.

| Inside our testing boundary (MUST test thoroughly) | Outside our testing boundary (DO NOT over-test) |
| --- | --- |
| Route eligibility rules across `global`, `personal`, and `shared` providers (`RTE-3`, `SHR-3`, `USR-3`) | Whether `tokio::spawn` schedules tasks onto OS threads or whether `moka` implements concurrent hash buckets internally |
| Smooth weighted round-robin counter math, per-model priority overrides, and strict tier fallback (`RTE-4`, `RTE-5`, `RTE-6`) | Whether `reqwest` or `rustls` implements the TLS 1.3 handshake state machine according to RFC 8446 |
| Inbound session precedence, HMAC privacy hashing, and tenant-salted outbound affinity hints (`RTE-1`, `SEC-16`) | Whether `sha2`, `chacha20poly1305`, or `argon2` implements its underlying mathematical primitives correctly |
| Surgical top-level JSON span editing (`model`, `prompt_cache_key`, `include_usage`) without altering message byte order (`TRN-1`, `TRN-2`, `API-4`, `API-5`) | Whether `serde_json` parses valid RFC 8259 JSON strings in general |
| SSE usage and trace tap state machines, OpenAI vs. Anthropic cached token subtraction, SSE delta reassembly (`text`, `reasoning`, `tool_calls`), session chat deduplication, and integer micro-USD cost math (`USG-1`..`USG-3`, `TRC-2`..`TRC-5`) | Whether SQLite's `PRAGMA journal_mode = WAL` writes frames to the `-wal` file on the OS filesystem |
| Source provider `/usage` response parsing, window reconciliation math, and PII sanitization on shared and global pass-through (`USG-4`..`USG-8`, `SEC-18`) | Live third-party provider uptime during automated CI runs (live upstream checks run only in an opt-in manual harness) |
| Our SQL query predicates (`WHERE owner_user_id = ?`), migration invariants, AEAD associated-data binding, and audit hash chaining in `crates/store` (`SEC-1`, `SEC-13`, `SEC-17`) | Static constant values in preset files unless validating that every preset parses into a valid `ProviderConfig` |

### 16.2 Failing tests taxonomy: expected failures vs. unexpected failures

Failing tests come in two distinct kinds. Confusing them turns a test suite into friction instead of a safety net. Every engineer and automated agent working on LLMRoller must follow the taxonomy and protocol below.

#### 16.2.1 Expected failures (part of building or changing behavior intentionally)

Expected failures happen when building something new or intentionally replacing a specified behavior. The table below defines the three rules for expected failures.

| Rule | Engineering protocol for expected failures |
| --- | --- |
| 1. Red-Green-Refactor for new specs | When building a new requirement (`AUTH-*`, `PRV-*`, `SHR-*`, `RTE-*`, `USG-*`, `SEC-*`), write the failing test guard first and cite the requirement ID. That test stays red until the implementation lands in the same branch. |
| 2. Replace retired behavior in the same change | When a product brief or architectural decision deliberately replaces an existing behavior, any test that pinned the old behavior will turn red. Update or retire that test in the exact same commit or pull request, naming the requirement ID or decision ID (`ADR-*`) that sanctioned the change. |
| 3. Test observable contracts, not internals | A test that pins private struct fields, internal helper counts, or exact SQL whitespace is a test waiting to fail during routine refactoring. Write tests against observable inputs, outputs, state transitions, and wire effects so redesigning internal code underneath does not break them. |

#### 16.2.2 Unexpected failures (the real defect signal: stop and investigate)

Unexpected failures happen when changing one component breaks a test in another component. They also happen when a change breaks unintended behavior.

| Rule | Engineering protocol for unexpected failures |
| --- | --- |
| 1. Leaking boundary or unintended side effect | If changing `/usage` reconciliation in `crates/core` breaks a session-pinning test or a personal-share authorization test, an architectural boundary is leaking. |
| 2. Zero tolerance for loosening assertions | When an unexpected failure occurs, never loosen the assertion, add `#[ignore]`, delete the test, or tweak mock data just to make CI green. |
| 3. The red-test rule of thumb | A red test is acceptable only when the change was meant to make that test red. Every other red test must be root-caused before work continues. |

### 16.3 Strict TDD workflow

Every feature, bug fix, or schema change follows a five-step loop enforced by pull-request review and CI gates.

| Step | Phase | Mandatory action |
| --- | --- | --- |
| 1 | Link requirement | Identify the exact requirement ID(s) in Section 6, 7, or 8, or add a new requirement ID to the SRDD first. |
| 2 | Write failing test (Red) | Write the unit, property, or integration test asserting the observable contract and linking `#[doc = "Spec: <ID>"]`. Confirm it fails as expected. |
| 3 | Write minimal code (Green) | Implement the minimal production logic in `core`, `proto`, `store`, `proxy`, `admin`, or `server` to pass the new test and all existing tests. |
| 4 | Refactor cleanly | Remove duplication and tighten types while keeping the entire test suite green. |
| 5 | Kill surviving mutants | Run `cargo mutants --in-diff` on the changed files and add boundary or negative assertions until zero mutants survive. |

### 16.4 Component mocking and dependency injection architecture

To test our controlled logic thoroughly without flakiness from wall-clock time, OS entropy, network sockets, or slow disk I/O, LLMRoller defines six core seam traits. Production code wires the real implementations; unit and component tests inject deterministic fakes or mocks.

| Seam trait | Production implementation | Test fake or mock implementation | Controlled logic verified by mocking this seam |
| --- | --- | --- | --- |
| `Clock` (`fn now_ms(&self) -> i64`, `async fn sleep(&self, d: Duration)`) | `SystemClock` (`time::OffsetDateTime::now_utc`, `tokio::time::sleep`) | `FakeClock` (atomic millisecond counter advanced manually by tests) | Pin sliding TTL expiration (`RTE-10`), 5 h and 7 d rolling ring bucket rotation (`LIM-1`), cooldown timer expiry, `Retry-After <= 2 s` same-key retry (`RTE-9`), and DeepSeek UTC peak-hour pricing schedules (`USG-3`). |
| `EntropySource` (`fn fill_bytes(&self, dest: &mut [u8])`) | `OsEntropy` (`rand::rngs::OsRng`) | `DeterministicRng` (seeded ChaCha20 RNG for reproducible test vectors) | Client key generation (`llr_...`, `SEC-3`), XChaCha20 nonce generation (`SEC-1`), and web session token creation (`AUTH-7`). |
| `UpstreamTransport` (`async fn send_inference(&self, req: OutboundRequest) -> Result<UpstreamResponseStream, TransportError>`) | `ReqwestUpstreamTransport` (with SSRF DNS filter and `rustls`) | `MockUpstreamTransport` (programmable per-route response queue: SSE chunk streams, delays, mid-stream drops, 429, 401, or 503 responses, and recorded outbound request capture) | Pre-first-byte failover (`RTE-9`), non-retry after first byte, header strip and passthrough policy (`API-2`, `API-3`), surgical body rewrites (`API-4`), and disconnect cancellation (`API-7`). |
| `UsageEndpointClient` (`async fn fetch_usage(&self, key: &UpstreamKey) -> Result<RawUsageResponse, UsagePollError>`) | `HttpUsageEndpointClient` | `StubUsageEndpointClient` (returns canned or sequence-scripted OpenCode Go, Command Code, OpenRouter, or 404 responses) | Polling triggers, reset-time synchronization, automatic `budget_adjustments` reconciliation, key exhaustion transitions (`USG-4`..`USG-6`), and `/v1/usage` pass-through sanitization (`USG-7`, `USG-8`, `SEC-18`). |
| `StorageRepository` (`UserRepo`, `ProviderRepo`, `ShareRepo`, `CatalogRepo`, `PinRepo`, `UsageRepo`, `AuditRepo`) | `SqliteRepository` (`sqlx::SqlitePool`) | `InMemoryRepository` (for fast `admin` and `proxy` handler unit tests) plus ephemeral temp-file SQLite WAL instances (for `crates/store` SQL integration tests) | Handler RBAC enforcement (`USR-1`..`USR-5`), share revocation effects (`SHR-5`), atomic first-admin setup (`AUTH-1`), and non-blocking usage queue overflow (`REL-3`). |
| `SecretSealer` (`fn seal(&self, aad: &[u8], plain: &[u8]) -> Vec<u8>`, `fn open(...)`) | `XChaCha20Poly1305Sealer` | `TestSealer` (real XChaCha20-Poly1305 initialized with a fixed 32-byte test master key) | Cross-row AAD tampering rejection and master-key rotation (`SEC-1`, `SEC-2`). |

### 16.5 Test levels and suites

The test pyramid consists of six automated suites run via `cargo nextest` and dedicated verification tools.

#### 16.5.1 Level 1: Deterministic unit and property-based tests (`crates/core`, `crates/proto`)

Unit and property tests verify pure domain and protocol rules in milliseconds without network or disk I/O.

| Test category | Invariants and behaviors verified |
| --- | --- |
| Table-driven unit tests | All nine session-resolution steps, canonical model normalization (`deepseek/deepseek-v4.1-flash` to `deepseek-v4.1-flash` while keeping `deepseek-v4-flash` distinct), RBAC checks for `admin` vs. `user`, personal, shared, and global candidate filtering across all four `routing_scope` modes, and error classification per provider. |
| Smooth WRR `proptest` suite | For any generated vector of 1 to 50 routes with weights in `1..=1000`, over 100,000 picks, each route's selection share stays within `0.5%` of `W_i / sum(W)`, and the longest consecutive run of one route stays at or below `ceil(max(W) / min(W))`. |
| Surgical JSON span `proptest` suite | For any generated JSON payload with nested `messages`, `tools`, and `system` arrays and arbitrary key order, editing `model` or injecting `prompt_cache_key` keeps exact byte equality on every untouched field span (`TRN-2`). |
| Micro-USD cost and window ring `proptest` suite | For any sequence of token counts in `0..=10_000_000` and rates in `1..=100_000_000`, integer micro-USD cost math never overflows, never produces negative numbers, and rolling ring counters equal the exact sum of events inside `[now - window_ms, now]`. |

#### 16.5.2 Level 2: Golden fixture and wire-contract tests (`fixtures/`)

Recorded real-world payloads in `fixtures/` are replayed through `crates/proto` to lock wire compatibility.

| Fixture set | Recorded scenarios verified |
| --- | --- |
| `fixtures/sse/` | Multi-chunk SSE streams for OpenAI `chat` (with and without `include_usage`), OpenAI `responses` (`response.completed` and `reasoning_tokens`), Anthropic `messages` (`message_start`, `content_block_delta`, `message_delta`, `message_stop`), OpenRouter `usage.cost`, and truncated streams ([command-code#785](https://github.com/CommandCodeAI/command-code/issues/785)). |
| `fixtures/usage/` | Recorded JSON payloads from OpenCode Go (`/zen/go/v1/usage`), Command Code (`/alpha/billing/credits` and `/alpha/usage/summary`), OpenRouter (`/api/v1/key`), and Open Design AMR (404 and proposed [#8674](https://github.com/nexu-io/open-design/issues/8674) schema), verifying normalization and PII stripping (`sanitized_json`). |

#### 16.5.3 Level 3: Component and SQLite integration tests (`crates/store`, `crates/proxy`, `crates/admin`)

Integration tests run against temporary SQLite WAL databases and local `wiremock` or custom `hyper` fault-injection servers.

| Integration test suite | Scenario and assertions |
| --- | --- |
| Concurrency and single-flight pinning (`RTE-7`) | Spawn 100 concurrent Tokio tasks sending turn 1 of the same session ID simultaneously; assert that `MockUpstreamTransport` receives all 100 calls on one `upstream_key_id` and creates one pin row. |
| Failover before first byte vs. post-first-byte no-retry (`RTE-9`) | Verify that a connect timeout, 503, 429, or 401 before the first byte fails over to the next candidate and re-pins, while a stream drop after yielding 1 byte never retries and marks `incomplete = 1`. |
| Personal provider sharing and revocation isolation (`SHR-1`..`SHR-6`, `SEC-17`, `SEC-18`) | User A creates a personal provider, adds a key, and shares it with User B with a $5 monthly cap and `allowed_models = ["deepseek-v4.1-flash"]`. Verify that User B routes `deepseek-v4.1-flash`, gets HTTP 403 on other models, never sees User A's secret or raw `/usage` email, re-pins to global when the $5 cap hits or User A revokes the share, and cannot re-share with User C. |
| Security and canary redaction (`SEC-1`..`SEC-19`) | Plant canary secret `sk-canary-9988776655443322` as an upstream key, trigger upstream 401 and 500 errors, invalid JSON, and panics, and assert the canary never appears in logs, HTTP responses, SQLite error columns, or `/metrics`. Verify SSRF rejection on `127.0.0.1`, `169.254.169.254`, and HTTP 302 redirects. |

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
| Full multi-role lifecycle (`tests/e2e_lifecycle.rs`) | Complete `/setup` with the one-time token, confirm `/setup` locks to 404, add global providers as `admin`, create standard users Alice and Bob, log in as Alice, add a personal OpenCode Go key, share it with Bob, issue `llr_...` keys, stream multi-turn chats, verify `/v1/usage` and `/v1/key` pass-through, verify Alice's and Bob's HTMX pages and CSV exports, send `SIGTERM` mid-stream to verify graceful drain (`REL-1`), and restart to verify pin persistence (`REL-2`). |
| Real CLI harness compatibility (`tests/e2e_cli_harnesses.rs`, `COM-1`) | Run headless invocations of `claude` (Claude Code), `codex`, `opencode`, and Python and TypeScript scripts using the official `openai` and `anthropic` SDKs against LLMRoller, completing a multi-turn tool-call loop and asserting pin stickiness and exact usage rows. |

#### 16.5.6 Level 6: Mutation testing (`cargo-mutants`)

Line and branch coverage (`cargo llvm-cov`, minimum gate `90%` workspace-wide and `95%` in `crates/core` and `crates/proto`) only proves that a test ran a line of code. It does not prove that any assertion checked the result ([cargo-mutants vs. coverage](https://mutants.rs/vs-coverage.html)).

LLMRoller enforces automated mutation testing using [`cargo-mutants`](https://mutants.rs/).

| Mutation aspect | Specification and CI gate |
| --- | --- |
| Injected fault types | `cargo-mutants` parses the Rust AST and injects behavioral faults one by one: replacing functions with `Ok(())` or `Default::default()`, flipping `<` to `<=` or `>` in budget reserve and cooldown checks, inverting `&&` and `||` in route eligibility and RBAC guards (`SEC-17`), deleting match arms in error classifiers, and changing `+`, `-`, or `*` in WRR and micro-USD cost math. |
| Pull-request gate (`--in-diff`) | Every pull request runs `cargo mutants --in-diff git_base..HEAD -j 4` across all modified Rust files ([diff testing](https://mutants.rs/in-diff.html)). Any surviving mutant fails the PR build unless annotated with `#[mutants::skip]` on a non-behavioral log line with reviewer sign-off. |
| Nightly full-workspace gate | Nightly CI runs `cargo mutants --workspace` across `crates/core`, `crates/proto`, `crates/store`, `crates/proxy`, and `crates/admin`. |
| Target mutation score | `100%` killed mutants (zero unannotated survivors) in `crates/core`, `crates/proto`, and all RBAC, share, and cryptographic guards in `crates/store`, `crates/proxy`, and `crates/admin`. |

### 16.6 Code-test-spec traceability matrix

Every requirement ID in this SRDD maps to concrete source modules and automated test targets. A CI script (`scripts/verify_traceability.py`) scans the SRDD tables and Rust test attributes (`#[doc = "Spec: ..."]`) on every commit. The script fails the build if any requirement lacks a linked test or if any test cites an unknown requirement ID.

| Requirement ID(s) | Source crate & module | Unit / property test target | Integration / E2E / mutation target |
| --- | --- | --- | --- |
| `AUTH-1`..`AUTH-9` | `crates/admin::auth`, `crates/store::users` | `admin::auth::tests::*`, `store::users::tests::*` | `tests/e2e_setup_and_auth.rs` (concurrent setup race, lockout, TOTP, cookie flags) |
| `USR-1`..`USR-5` | `crates/core::rbac`, `crates/admin::middleware`, `crates/store::*` | `core::rbac::tests::actor_scope_matrix` | `tests/integration_rbac_isolation.rs` (IDOR checks across all `/api/user/*` and `/admin/api/*` routes) + `cargo-mutants` on `core::rbac` |
| `PRV-1`..`PRV-8` | `crates/core::provider`, `crates/proto::adapters`, `crates/store::providers` | `core::provider::tests::*`, `proto::adapters::tests::*` | `tests/integration_providers_and_keys.rs` (global vs. personal CRUD, key test, SSRF check) |
| `SHR-1`..`SHR-6` | `crates/core::shares`, `crates/store::shares`, `crates/admin::shares` | `core::shares::tests::eligibility_and_caps` | `tests/integration_personal_sharing.rs` (grant, model filter, monthly cap, revocation repin, no transitive share) |
| `CLK-1`..`CLK-5` | `crates/core::client_key`, `crates/proxy::auth` | `core::client_key::tests::*` | `tests/integration_client_keys.rs` (creation, `routing_scope`, rotation, 1 s revocation, RPM & CIDR caps) |
| `MDL-1`..`MDL-7` | `crates/core::catalog`, `crates/proto::discovery`, `crates/server::jobs` | `core::catalog::tests::normalization_and_intersection` | `tests/integration_model_catalog.rs` (auto intersection vs. forced on or off, personal model exposure, `/v1/models` shapes) |
| `RTE-1`..`RTE-13` | `crates/core::router`, `crates/core::wrr`, `crates/core::pins` | `core::wrr::proptests::*`, `core::router::tests::*`, `core::pins::tests::*` | `tests/integration_routing_and_failover.rs` (100-way single-flight race, pre-first-byte failover, post-first-byte no-retry, `previous_response_id`) + `cargo-mutants` |
| `API-1`..`API-8`, `TRN-1`..`TRN-3` | `crates/proto::body_edit`, `crates/proto::headers`, `crates/proxy::forward` | `proto::body_edit::proptests::*`, `proto::headers::tests::*` | `tests/integration_wire_transparency.rs` (byte-identical pass-through, injected `include_usage` chunk stripping, 1 s disconnect abort) |
| `USG-1`..`USG-8`, `LIM-1` | `crates/proto::sse_tap`, `crates/core::pricing`, `crates/core::windows`, `crates/proto::usage_poller` | `proto::sse_tap::golden_tests::*`, `core::pricing::proptests::*`, `core::windows::tests::*` | `tests/integration_usage_and_passthrough.rs` (OpenCode Go, Command Code, OpenRouter `/usage` sync, auto-adjustment, `/v1/usage` & `/v1/key` sanitization) |
| `TRC-1`..`TRC-8`, `PERF-5`, `SEC-19` | `crates/proto::trace_tap`, `crates/core::chat_preview`, `crates/store::traces`, `crates/admin::traces` | `proto::trace_tap::tests::*`, `core::chat_preview::tests::dedup_and_branching` | `tests/integration_content_traceability.rs` (default-off check, streaming delta reassembly, Grantor A blocked from Grantee B trace, XSS sanitization in chat preview, purge & TTL) + `cargo-mutants` |
| `UI-1`..`UI-4`, `AUD-1` | `crates/admin::ui`, `crates/store::audit` | `store::audit::tests::hash_chain_verification` | `tests/e2e_dashboard_htmx.rs` (admin and standard user UI flows, read-only session chat preview drawer, zero external asset check, audit tamper detection) |
| `PERF-1`..`PERF-5`, `REL-1`..`REL-6`, `COM-1`, `POR-1`, `MNT-1`..`MNT-2`, `OBS-1`, `PRIV-1` | `crates/server`, workspace-wide | `crates/core` zero-I/O dependency test | `benches/proxy_latency.rs`, `tests/e2e_sigterm_drain.rs`, `tests/e2e_kill9_recovery.rs`, `tests/e2e_cli_harnesses.rs` |
| `SEC-1`..`SEC-19` | `crates/store::crypto`, `crates/proxy::ssrf`, `crates/admin::csrf`, `crates/admin::traces` | `store::crypto::tests::aead_aad_and_rotation`, `proxy::ssrf::tests::ip_ranges` | `tests/security_canary_and_ssrf.rs`, `tests/security_csrf_and_headers.rs` + `cargo-mutants` on all security modules |

## 17. Roadmap and milestones

Implementation follows seven dependency-ordered milestones (`M0` through `M6`). Under the TDD rules in Section 16, a milestone is complete only when all linked requirement tests pass and `cargo-mutants` reports zero surviving mutants.

### 17.1 Milestone delivery plan

The table below defines the scope, deliverables, and exit criteria for each milestone.

| Milestone | Scope and deliverables | Exit criteria (must pass before starting next milestone) |
| --- | --- | --- |
| `M0`: Foundation, RBAC, and Auth | Cargo workspace setup, SQLite WAL migrations (20 tables), XChaCha20-Poly1305 secret sealer, Argon2id + TOTP auth, atomic first-admin `/setup`, `admin` and `user` account management, `ActorScope` RBAC middleware, hash-chained `audit_log`, and CI gates (`clippy`, `cargo-deny`, `cargo-audit`, `cargo-mutants`). | `AUTH-1`..`AUTH-9`, `USR-1`, `AUD-1`, `SEC-1`..`SEC-5`, `SEC-10`, `SEC-12`..`SEC-14`, `SEC-17` tests pass; zero surviving mutants in `crates/store`. |
| `M1`: Transparent Pass-Through Proxy | Global and personal provider records, upstream key encryption and `/models` test endpoint, client key issuance (`llr_...`), SSRF-safe `reqwest` transport, header policy, surgical JSON span editor, and unbuffered SSE streaming for `chat`, `responses`, and `messages`. | `PRV-1`..`PRV-8`, `CLK-1`..`CLK-5`, `API-1`..`API-8`, `TRN-1`..`TRN-3`, `SEC-6`..`SEC-8`, `SEC-15` tests pass; `COM-1` passes against a single provider. |
| `M2`: Routing, Pinning, and Sharing | Model catalog discovery and normalization, global auto-intersection and force exposure, personal provider and key sharing (`provider_shares`), `routing_scope` pool resolution (`personal_first`, `blended`, `personal_only`, `global_only`), smooth WRR with per-model overrides, single-flight session pinning, `previous_response_id` links, and pre-first-byte failover. | `SHR-1`..`SHR-6`, `MDL-1`..`MDL-7`, `RTE-1`..`RTE-13`, `USR-2`..`USR-5`, `SEC-16` tests pass; 100-way concurrency race test and `proptest` WRR suite pass with zero surviving mutants in `crates/core`. |
| `M3`: Usage Pipeline, Price Book, and Content Traceability | `SseUsageTap`, `JsonUsageExtractor`, and opt-in `TraceTap` delta reassembler for all three protocols; OpenAI vs. Anthropic cache token normalization; integer micro-USD price book; `zstd` + `XChaCha20-Poly1305` `trace_records` storage; session chat timeline reconstructor; batching SQLite writer and hourly rollups. | `USG-1`..`USG-3`, `TRC-1`..`TRC-8`, `PERF-5`, `SEC-19`, `REL-2`, `REL-3` tests and all golden SSE fixtures in `fixtures/sse/` pass. |
| `M4`: Budget Windows and Source Provider `/usage` Engine | Rolling (`5h`, `7d`) and anchored (`1mo`) budget window ring counters, source provider `/usage` poller and parsers for OpenCode Go (`/zen/go/v1/usage`), Command Code (`/alpha/billing/credits`), OpenRouter (`/api/v1/key`), and custom endpoints, automatic window reconciliation, and `/v1/usage` + `/v1/key` pass-through with PII sanitization. | `USG-4`..`USG-8`, `LIM-1`, `SEC-18` tests and all `/usage` fixtures in `fixtures/usage/` pass. |
| `M5`: Embedded Web UI and Read-Only Session Chat Preview | Server-rendered Askama + HTMX screens for both the Personal Workspace (`/app/*`) and Admin Console (`/admin/*`), vendored `uPlot` charts, inline routing matrix editor, share management UI, raw `/usage` inspection modal, active session pin controls, XSS-safe read-only Session Chat Preview screen, and CSV or JSON exports. | `UI-1`..`UI-4`, `TRC-5`, `TRC-6` E2E browser and HTMX tests pass; CSP and zero-external-asset checks pass. |
| `M6`: Hardening, Load Verification, and Release Packaging | `cargo-fuzz` targets on JSON span scanner, SSE parser, and header normalizer; 500 req/s and 2,000-stream benchmark verification (`PERF-1`..`PERF-4`); `SIGTERM` drain and `SIGKILL` recovery verification; live CLI harness verification (`Claude Code`, `Codex CLI`, `opencode`); signed static binaries, SBOM, and distroless container image under 50 MB. | All `PERF-*`, `REL-*`, `POR-1`, `COM-1`, `SEC-11` checks pass; `scripts/verify_traceability.py` reports 100% requirement test coverage. |

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
| R1 | An upstream provider suspends an account for pooling keys across multiple users or violating subscription terms. | Medium | High | Support personal providers (`PRV-1`) with `personal_first` or `personal_only` routing so each user routes primarily through their own subscription, and scope shares to trusted teammates (`SHR-1`). Forward authentic client headers (`API-3`) and pin each session to a single key (`RTE-2`) so traffic matches normal single-agent patterns. Document operator responsibility (`A1`). |
| R2 | Source provider `/usage` endpoints change schema or are undocumented (such as Command Code `/alpha/billing/credits` or Open Design AMR [#8674](https://github.com/nexu-io/open-design/issues/8674)). | High | Medium | Isolate `/usage` parsing inside `ProviderAdapter` (`MNT-2`), store raw JSON alongside normalized fields (`USG-5`), and fall back automatically to local micro-USD window ring counters and manual calibration (`LIM-1`) whenever an upstream `/usage` endpoint returns 404 or an unparseable shape. |
| R3 | Local micro-USD cost estimates drift from a provider's internal billing due to external key usage, promotional multipliers, or rounding. | High | Medium | Poll upstream `/usage` endpoints every 5 minutes and at 80% spend to auto-reconcile window counters and reset timestamps (`USG-4`, `USG-6`), enforce a 2% admission reserve (`LIM-1`), and provide one-click manual calibration. |
| R4 | Command Code or Open Design AMR changes which session affinity header or body field its backend honors. | Medium | Medium | Make `session_profile` editable per provider in JSON configuration (`PRV-3`) and surface the per-route cache hit ratio on the dashboard so operators can verify cache locality empirically. |
| R5 | A provider enables automatic pay-as-you-go wallet fallback when subscription windows run out (such as OpenCode Go "Use balance" or Command Code extra credits). | Medium | High | Warn operators in the UI setup guide to disable automatic wallet fallback in provider consoles, or set LLMRoller window limits and reserve percentages (`reserve_pct = 5%`) below the plan cap so LLMRoller stops routing before spillover starts. |
| R6 | A grantee on a shared personal key consumes the owner's entire monthly quota or tries to inspect the owner's private account details. | Medium | High | Enforce per-share `max_monthly_micro_usd` and `allowed_models` (`SHR-2`), allow instant share revocation (`SHR-5`), and strip all PII and account identifiers from `/v1/usage` responses on shared keys (`USG-8`, `SEC-18`). |
| R7 | An upstream provider drops SSE streams mid-generation without sending `[DONE]`, `response.completed`, or `message_stop` ([command-code#785](https://github.com/CommandCodeAI/command-code/issues/785)). | Medium | Medium | Never retry after the first byte (`RTE-9`), detect missing terminal events in `SseUsageTap`, set `incomplete = 1` on the usage row (`USG-1`), and count soft circuit-breaker failures. |
| R8 | A misconfigured client sends a static constant session ID for every conversation, funneling all traffic onto one key ([vscode#334186](https://github.com/microsoft/vscode/issues/334186)). | Medium | Medium | Track opening-message fingerprint cardinality per `session_hash` (`Section 11.1.1`) and flag any session hash exceeding 20 distinct conversations per hour in the Sessions UI. |
| R9 | Long-running agent sessions skew actual token spend away from the session-count weights configured in smooth WRR. | Medium | Low | Provide `headroom` weight scaling (`RTE-13`) that scales WRR weights by remaining budget fraction, plus automatic repinning when a key's window reaches its limit (`RTE-11`). |
| R10 | Upstream providers change model availability, endpoint support (`chat` vs. `messages`), or pricing without notice. | High | Low | Run scheduled `/models` discovery every 6 hours (`MDL-1`), read `supported_endpoints` dynamically, and keep prices and window rules in SQLite tables rather than compiled constants. |
| R11 | A compromised crate in the Rust dependency ecosystem tries to exfiltrate decrypted upstream keys ([March 2026 LiteLLM incident](https://docs.litellm.ai/blog/security-update-march-2026)). | Low | High | Keep the dependency list small, commit `Cargo.lock`, enforce `cargo audit`, `cargo deny`, and `cargo vet` in CI (`SEC-10`), run inside a read-only distroless container (`SEC-11`), and restrict outbound connections to configured provider origins (`PRIV-1`, `SEC-7`). |
| R12 | Losing `LLMROLLER_MASTER_KEY` renders all encrypted `upstream_keys.secret_enc` blobs unrecoverable. | Low | High | Validate the master key at startup (`SEC-2`), document master-key backup procedures in the operator guide, and provide `llmroller rotate-master-key` (`SEC-1`). |
| R13 | Tests become brittle or allow subtle logic regressions in routing, budget math, or RBAC checks to slip into production. | Medium | High | Enforce strict TDD contract testing, boundary discipline, the Expected vs. Unexpected failure protocol (`Section 16.2`), and automated mutation testing with `cargo-mutants` (`Section 16.5.6`). |
| R14 | A single-node SQLite instance experiences hardware failure. | Low | Medium | Persist pins and usage in WAL mode (`REL-2`), keep startup recovery under 2 s (`PERF-4`), and provide `llmroller backup --out <path>` for online cron backups (`REL-6`). |
| R15 | Enabling content traceability mode increases SQLite disk usage or exposes sensitive prompts across shared personal keys. | Medium | High | Keep content traceability strictly off by default (`TRC-1`), compress with `zstd` and encrypt with `XChaCha20-Poly1305` (`SEC-19`), cap captured turns at 4 MiB (`PERF-5`), enforce a 14-day default TTL (`TRC-7`), and block shared-key grantors from reading grantee traces (`TRC-8`). |

## 19. Architectural decisions and open questions

This section records the architectural decisions resolved during specification refinement and lists the remaining operational configuration choices for deployment.

### 19.1 Resolved architectural decisions (ADRs)

Nine core design questions from the initial draft and product brief are formally resolved below for version 1.0.

| ADR ID | Topic | Decision for version 1.0 | Rationale |
| --- | --- | --- | --- |
| `ADR-1` | Provider ownership scopes | Support two first-class provider scopes: `global` (created by `admin`, usable by any authorized user) and `personal` (created by any `user` or `admin`, usable by the owner and explicitly shared grantees). | Lets teams pool shared corporate keys while individuals safely bring their own coding-plan subscriptions and share spare capacity with specific teammates. |
| `ADR-2` | Standard non-admin user class (`user`) | Ship the standard `user` role in version 1.0 (`P0`) with full self-service UI and API (`/app/*`, `/api/user/*`) for personal providers, key sharing, client keys, personal route overrides, pins, and scoped usage. | Eliminates admin bottlenecks and enforces strict SQL-level tenant isolation (`ActorScope`) from day one. |
| `ADR-3` | Combining personal, shared, and global keys | Provide four selectable `routing_scope` modes on each client key: `personal_first` (default), `blended`, `personal_only`, and `global_only`. | Gives users predictable control over whether personal and shared keys are spent before global keys or blended together. |
| `ADR-4` | Source provider `/usage` tracking and pass-through | Poll source provider `/usage` endpoints (`OpenCode Go`, `Command Code`, `OpenRouter`, and custom URLs) to auto-reconcile local window counters, and expose `/v1/usage`, `/v1/key`, and `/v1/usage/raw` with role-aware sanitization. | Combines the immediate responsiveness of local micro-USD accounting with the authoritative accuracy of upstream quota ledgers, while powering IDE status bars directly. |
| `ADR-5` | Weighted round-robin vs. strict priority tiers | Ship both modes in version 1.0: `weighted` (smooth WRR using key priority or per-model `priority_override`) as the default, plus `strict` tier fallback per model or key. | Satisfies both proportional dollar-allowance balancing ($120, $80, $60) and primary-backup failover setups. |
| `ADR-6` | Cross-protocol translation | Route requests in version 1.0 only to candidates that natively support the inbound protocol (`chat`, `responses`, or `messages`); defer opt-in cross-protocol translation to post-1.0 (`P2`). | Prevents silent loss of reasoning blocks ([open-design#8687](https://github.com/nexu-io/open-design/issues/8687)) and tool-call schema mismatches. |
| `ADR-7` | Canonical model normalization | Normalize vendor prefixes and casing (`deepseek/deepseek-v4.1-flash` to `deepseek-v4.1-flash`) but never merge distinct version numbers (`deepseek-v4-flash` != `deepseek-v4.1-flash`). | Prevents routing a client request to a different model generation while still matching identical models across providers. |
| `ADR-8` | Test governance and mutation testing | Mandate Red-Green-Refactor TDD, contract-level testing within our controlled boundary, strict Expected vs. Unexpected failure handling, and zero surviving mutants in `cargo-mutants`. | Guarantees that tests verify actual behavioral invariants rather than superficial line coverage. |
| `ADR-9` | Opt-in content traceability and read-only session chat preview | Provide an admin opt-in `content_traceability_mode` that records compressed, AEAD-encrypted turn payloads (`trace_records`) and reconstructs a deduplicated read-only chat preview per pinned `session_hash` in the dashboard. | Enables deep debugging and session replay when needed while preserving zero-prompt-storage privacy by default and strict multi-tenant isolation (`TRC-1`..`TRC-8`, `SEC-19`). |

### 19.2 Operational verification items for Milestone 1 (`M1`)

Four empirical checks are scheduled during Milestone 1 implementation against live upstream accounts.

| Item | Target provider or client | Verification procedure |
| --- | --- | --- |
| 1. Cache-affinity field benchmark | Command Code and Open Design AMR | Run `scripts/verify_cache_affinity.sh` sending identical 8,192-token prefixes twice under candidate headers (`x-session-affinity`, `x-session-id`) and body `prompt_cache_key`, and inspect `cached_tokens` on turn 2. |
| 2. Upstream `/v1/usage` readiness | Open Design AMR | Monitor [nexu-io/open-design#8674](https://github.com/nexu-io/open-design/issues/8674) and enable the `open_design_amr` `/usage` poller URL in `presets/open_design_amr.toml` once the upstream service endpoint ships. |
| 3. Alpha billing schema lock | Command Code (`GOAT` and `Pro`) | Capture live JSON responses from `/alpha/billing/credits` and `/alpha/usage/summary` across `GOAT` and `Pro` keys and lock them into `fixtures/usage/command_code_*.json`. |
| 4. Responses transport check | Codex CLI | Verify that current Codex CLI builds use HTTP SSE when configured with a custom `base_url` on `/v1/responses`. |

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
