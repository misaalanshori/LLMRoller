# LLMRoller

[![CI](https://github.com/misaalanshori/LLMRoller/actions/workflows/ci.yml/badge.svg)](https://github.com/misaalanshori/LLMRoller/actions/workflows/ci.yml)
[![License: MIT/Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](Cargo.toml)
[![Rust: 1.99.0+](https://img.shields.io/badge/rust-1.99.0%2B-orange.svg)](Cargo.toml)
[![Security: forbid(unsafe_code)](https://img.shields.io/badge/unsafe-forbidden-success.svg)](crates/)
[![Distroless: <50MB](https://img.shields.io/badge/container-%3C50MB%20distroless-blueviolet.svg)](Dockerfile)

**LLMRoller** is an enterprise-grade, high-performance transparent reverse proxy, load balancer, and usage governance engine specifically engineered for autonomous coding agents (Claude Code, OpenAI Codex, OpenCode Go, Cursor, Cline, OpenDesign AMR) and multi-tenant LLM inference clusters.

Built from first principles in **100% safe Rust** (`#![forbid(unsafe_code)]`), LLMRoller delivers wire-transparent proxying, deterministic multi-turn session affinity, pluggable traffic distribution algorithms, cryptographic multi-tenant sharing with strict privacy guarantees, 128-bit micro-USD cost tracking, and crash-resilient SQLite WAL persistence.

---

## Table of Contents

- [Architectural Highlights](#architectural-highlights)
- [System Architecture](#system-architecture)
- [Core Subsystems](#core-subsystems)
  - [1. Pluggable Routing & Load Balancing](#1-pluggable-routing--load-balancing)
  - [2. Multi-Turn Session Affinity & Pinning](#2-multi-turn-session-affinity--pinning)
  - [3. Multi-Tenancy, Consented Sharing & RBAC](#3-multi-tenancy-consented-sharing--rbac)
  - [4. Wire Transparency & Surgical Body Editing](#4-wire-transparency--surgical-body-editing)
  - [5. Dual-Priority SQLite WAL Writer & Rollups](#5-dual-priority-sqlite-wal-writer--rollups)
  - [6. Zero-Trust Cryptography & Resumable Rotation](#6-zero-trust-cryptography--resumable-rotation)
  - [7. Socket-Level SSRF Protection](#7-socket-level-ssrf-protection)
- [Quick Start](#quick-start)
  - [Docker Compose (Production-Ready)](#docker-compose-production-ready)
  - [Local Native Build](#local-native-build)
  - [Initial 1-Click Setup Walkthrough](#initial-1-click-setup-walkthrough)
- [Coding Agent Integration Guides](#coding-agent-integration-guides)
  - [Claude Code](#claude-code)
  - [OpenAI Codex & SDKs](#openai-codex--sdks)
  - [OpenCode Go](#opencode-go)
  - [Cursor / Cline / Windsurf](#cursor--cline--windsurf)
- [API & Wire Reference](#api--wire-reference)
  - [Data Plane (`:8080`)](#data-plane-8080)
  - [Control Plane & Admin Console (`:8081`)](#control-plane--admin-console-8081)
  - [Observability (`:9090`)](#observability-9090)
- [CLI Reference & Operations](#cli-reference--operations)
  - [Daemon Mode (`serve`)](#daemon-mode-serve)
  - [Keygen (`generate-master-key`)](#keygen-generate-master-key)
  - [Resumable Key Rotation (`rotate-master-key`)](#resumable-key-rotation-rotate-master-key)
  - [Audit Chain Verification (`verify-audit-log`)](#audit-chain-verification-verify-audit-log)
  - [Atomic Online Backup (`backup`)](#atomic-online-backup-backup)
  - [Break-Glass Admin Recovery (`reset-admin`)](#break-glass-admin-recovery-reset-admin)
  - [Container Healthcheck (`healthcheck`)](#container-healthcheck-healthcheck)
- [Configuration Reference](#configuration-reference)
  - [Environment Variables](#environment-variables)
  - [Dynamic Runtime Settings](#dynamic-runtime-settings)
- [Development, Verification & TDD Governance](#development-verification--tdd-governance)
- [License](#license)

---

## Architectural Highlights

- **Zero Memory Overhead Proxying:** Unbuffered SSE streaming forwarder with a 64 KiB pre-first-frame holding buffer. Upstream HTTP headers and status lines are held until the first valid chunk arrives, enabling transparent pre-first-byte failover before client commitment.
- **Strict Wire Transparency (`TRN-1` / `TRN-2`):** When routing to personal keys without modification, payload bytes are forwarded with zero-copy pointer identity. Model rewrites preserve surrounding byte formatting and model string escaping without parsing or altering unrelated JSON ASTs.
- **Nginx-Compliant Smooth Weighted Round Robin (`RTE-4`):** Maintains the strict invariant `sum(current_weights) == 0` across continuous subset churn, dynamically scaling weights by headroom basis points (`500..=10,000 bps`) based on real-time budget utilization.
- **Deterministic 8-Step Session Precedence (`RTE-1`):** Native awareness of Claude Code subagent hierarchies (`x-claude-code-agent-id`), upstream session headers, body attributes, and byte-safe Step-8 fallback hashing (<50 ms over 10 MiB payloads).
- **Consented Key Sharing with Cryptographic Fingerprints (`SHR-1`..`SHR-5`):** Grantors can share providers or keys with granular model filters and monthly micro-USD caps. Grantees must cryptographically accept the configuration fingerprint; any upstream path or private network change automatically suspends active shares within 1 second.
- **Dual-Priority SQLite Writer Actor (`REL-2` / `REL-3`):** High-priority control-plane transactions take precedence over data-plane queues. Drains non-dropping 5-minute and hourly additive rollups, while raw usage events operate on a bounded FIFO ring (10,000 items / 64 MiB) dropping oldest raw items under saturation without losing a single token or micro-USD in rollups.
- **Sub-2-Second Cold Start Reconstruction (`PERF-4`):** Reconstructs 500 upstream key rolling rings and UTC monthly accumulators in under 2 seconds directly from `usage_rollups_5min`, bypassing millions of raw usage events.
- **Defense in Depth Security:** Socket-level SSRF connector filtering 16 IPv4 and 11 IPv6 reserved CIDRs with IPv4-mapped, 6to4, and NAT64 unwrapping; Argon2id password hashing; RFC 6238 TOTP; constant-time CSRF verification; XChaCha20-Poly1305 authenticated encryption; and stored XSS sanitization via `pulldown-cmark` and `ammonia`.

---

## System Architecture

```
                  ┌──────────────────────────────────────────────┐
                  │                 Clients                      │
                  │   Claude Code / Codex / Cursor / SDKs / UI   │
                  └──────────────┬──────────────────┬────────────┘
                                 │ :8080 (Data)     │ :8081 (Admin / Web UI)
                                 ▼                  ▼
┌────────────────────────────────────────────────────────────────────────────────────────┐
│                                LLMRoller Service                                       │
│                                                                                        │
│   ┌───────────────────────────────────┐    ┌───────────────────────────────────────┐   │
│   │       Data Plane (:8080)          │    │      Control Plane / Admin (:8081)    │   │
│   │                                   │    │                                       │   │
│   │  • Client Key Auth (Gates G1-G7)  │    │  • Session & Login Auth (Argon2id)    │   │
│   │  • Body Budget Semaphore (819 MB) │    │  • RFC 6238 TOTP + Recovery Codes     │   │
│   │  • 8-Step Session Precedence      │    │  • Anti-CSRF Middleware (ct_eq)       │   │
│   │  • Two-Phase PinStore State       │    │  • Askama SSR + HTMX Web Workspace    │   │
│   │  • Candidate Eligibility (C1-C8)  │    │  • JSON CRUD APIs & Model Catalog     │   │
│   │  • Pluggable Route Selectors      │    │  • Consented Provider Sharing Matrix  │   │
│   │  • Surgical JSON Body Editor      │    │  • Sanitized Session Chat Preview     │   │
│   │  • Request / Response Pipelines   │    │  • Dynamic Settings Registry          │   │
│   └─────────────────┬─────────────────┘    └──────────────────┬────────────────────┘   │
│                     │                                         │                        │
│                     │  Enqueues Raw Events & Rollups          │  Control Transactions  │
│                     ▼                                         ▼                        │
│   ┌────────────────────────────────────────────────────────────────────────────────┐   │
│   │                        Dual-Priority Writer Actor                              │   │
│   │                                                                                │   │
│   │   [Control Channel] (Highest Priority) ──► Instant Config Version Bump         │   │
│   │   [Durable Queue]   (500k Keys)        ──► Pins, Response Links, Health State  │   │
│   │   [Rollups Map]     (Non-Dropping)     ──► 5-Min & Hourly Rollups (SAVEPOINTs) │   │
│   │   [Raw Events FIFO] (10k items / 64MB) ──► Drop Oldest Under Saturation        │   │
│   └────────────────────────────────────────┬───────────────────────────────────────┘   │
│                                            │                                           │
│                         Single Writer TX   ▼   Read Pools (PRAGMA query_only = ON)     │
│                     ┌─────────────────────────────────────────────┐                    │
│                     │       SQLite WAL Database Engine            │                    │
│                     │   (31 Tables, Incremental Auto-Vacuum)      │                    │
│                     └──────────────────────┬──────────────────────┘                    │
│                                            │                                           │
│                                            │ Notifies on version bump                  │
│                                            ▼                                           │
│                     ┌─────────────────────────────────────────────┐                    │
│                     │           Snapshot Reloader Task            │                    │
│                     │    Coalesced Rebuild -> arc_swap Store      │                    │
│                     └─────────────────────────────────────────────┘                    │
│                                            ▲                                           │
│                                            │ Atomic lock-free read                     │
│                     ┌──────────────────────┴──────────────────────┐                    │
│                     │     Outbound Guarded HTTP/SSE Client        │                    │
│                     │    Socket SSRF Guard (16 IPv4 / 11 IPv6)    │                    │
│                     └──────────────────────┬──────────────────────┘                    │
└────────────────────────────────────────────┼───────────────────────────────────────────┘
                                             │
                        ┌────────────────────┼───────────────────┐
                        ▼                    ▼                   ▼
               ┌─────────────────┐  ┌─────────────────┐ ┌─────────────────┐
               │   Anthropic     │  │     OpenAI      │ │   OpenRouter    │
               │ (Claude Models) │  │  (GPT / o-Series)│ │ (DeepSeek/Meta) │
               └─────────────────┘  └─────────────────┘ └─────────────────┘
```

---

## Core Subsystems

### 1. Pluggable Routing & Load Balancing

LLMRoller supports hot-reconfigurable routing strategies selectable globally via runtime settings or per-client-key overrides:

| Strategy ID | Name | Description & Invariants |
| :--- | :--- | :--- |
| `smooth_weighted_rr` *(default)* | Smooth Weighted Round Robin | Nginx SWRR algorithm. Guarantees `sum(current_weights) == 0`. Scales effective weights by headroom basis points (`500..=10,000 bps`) based on window reserve status (`RTE-4`, `RTE-13`). |
| `weighted_rr` | Classical Weighted Round Robin | Block-cyclic GCD distribution sequence (`RTE-5`). |
| `naive_rr` | Unweighted Round Robin | Strict atomic cyclic permutation `0, 1, ..., N-1` with lock-free `u64::MAX` wrap-around (`RTE-14`). |
| `power_of_two_choices` (`p2c`) | Power of Two Choices | Samples two random candidate routes and picks the route with superior composite latency/inflight load score, mitigating herd effects. |
| `weighted_random` | Weighted Random | Seeded determinism via ChaCha, cumulative binary search (`partition_point`) validated against Pearson chi-squared goodness-of-fit (`p > 0.001`). |
| `uniform_random` | Uniform Random | Uniform stochastic selection across all eligible candidate routes. |

### 2. Multi-Turn Session Affinity & Pinning

Coding agents generate dozens of related turns that benefit heavily from prompt cache locality. LLMRoller resolves and pins sessions through a deterministic pipeline:

```
[Inbound Request]
       │
       ▼
[Precedence Level 1] Header: `x-claude-code-agent-id` / `x-claude-code-parent-agent-id`
       │ (if absent)
       ▼
[Precedence Level 2] Header: `x-llmroller-session-id`
       │ (if absent)
       ▼
[Precedence Level 3] Headers: `x-session-id`, `session-id`, `session_id`, `conversation-id`, `x-bf-session-id`
       │ (if absent)
       ▼
[Precedence Level 4] Top-Level JSON Body: `session_id`
       │ (if absent)
       ▼
[Precedence Level 5] Top-Level JSON Body: `prompt_cache_key` / `promptCacheKey`
       │ (if absent)
       ▼
[Precedence Level 6] JSON Body Metadata: `metadata.session_id` / `metadata.user_id`
       │ (if absent)
       ▼
[Precedence Level 7] 21-Slot Exact ConstantSessionDetector (skips generic headers)
       │ (if absent)
       ▼
[Precedence Level 8] Byte-Safe Step-8 Fingerprint: SHA-256(client_key || canon(SYS) || canon(USER1))
```

- **Two-Phase PinStore:** Turn 1 acquires a provisional pin (50s TTL). Confirmation occurs upon the commitment of the first valid byte from upstream.
- **Failover & Loser Adoption (`RTE-8`):** If candidate route $K_1$ returns a pre-first-frame error (429, 502, 503, or connect error), the loser adoption state machine seamlessly transitions concurrent Turn-1 waiters to failover winner $K_2$ with zero extra upstream calls.
- **Durable `response_links` (`RTE-9`):** Stateful chaining for OpenAI `/v1/responses` tracks parent response IDs, strictly enforcing same-key affinity on subsequent continuation turns.

### 3. Multi-Tenancy, Consented Sharing & RBAC

LLMRoller enforces zero-trust multi-tenant isolation across three candidate pool tiers:

```
                    ┌──────────────────────────────────────────┐
                    │               Candidate Pool             │
                    └──────┬─────────────┬─────────────┬───────┘
                           │             │             │
                    ┌──────▼──────┐┌─────▼───────┐┌────▼───────┐
                    │   Own Tier  ││ Shared Tier ││ Global Tier│
                    └─────────────┘└─────────────┘└────────────┘
```

1. **Routing Scopes:**
   - `personal_first`: Routes through own personal keys first; falls back to shared and global pools.
   - `blended`: Equal-weighted distribution across own, shared, and global routes.
   - `personal_only`: Restricts traffic strictly to own and accepted shared keys.
   - `own_only`: Restricts traffic exclusively to keys owned directly by the user.
   - `global_only`: Restricts traffic strictly to administrator-managed global pool keys.
2. **Consented Provider Sharing (`SHR-1`..`SHR-5`):**
   - Personal providers can be shared with specific users (`grantee_user_id`).
   - Generates a SHA-256 `share_config_fingerprint` over `kind`, `base_url`, `allow_private_network`, and `paths`. Grantees must explicitly accept the matching fingerprint.
   - If the grantor changes any routing-relevant setting, active shares are suspended within 1 second.
   - Revoking a share deletes grantee route preferences and forces immediate re-pinning.
3. **IDOR Existence Oracle Prevention (`USR-3`):**
   - Querying or mutating a personal resource owned by another tenant returns **HTTP 404 (`not_found`)**, indistinguishable from a non-existent UUID.
   - **HTTP 403 (`permission_error`)** is returned only when a user attempts to mutate a readable global or shared catalog resource.

### 4. Wire Transparency & Surgical Body Editing

To preserve caching and low latency, LLMRoller modifies request bodies only when necessary:

- **Top-Level Span Scanner:** Scans JSON top-level keys without building an unneeded AST.
- **Zero-Copy Pass-Through (`TRN-1`):** On the `Own` tier when no model rename is required, the input `Bytes` slice is forwarded to the upstream socket unmodified.
- **Adjacent Comma Handling (`TRN-2`):** When stripping session identifiers on `Shared` or `Global` pools, adjacent commas are cleanly stripped to prevent JSON syntax corruption (`{,"model":...}`).
- **Streaming SSE Usage Tap (`API-5`):** Pass-through SSE parser that tracks token counts, extracts usage chunks (`choices:[]` + `usage:{}`), and estimates aborted stream output text at $\lceil \text{bytes} / 4 \rceil$.

### 5. Dual-Priority SQLite WAL Writer & Rollups

All mutations are funneled through a dedicated writer task paired with a read-only connection pool:

- **Transaction Prioritization:** Drains all pending control commands (`control_rx`) before executing data-plane queue flushes, guaranteeing that user and admin mutations never wait behind telemetry loads.
- **Multi-Level Queueing:**
  1. `durable_state`: Coalescing map for confirmed pins, response links, and upstream health updates.
  2. `rollups`: Non-dropping in-memory additive map aggregating metrics into `usage_rollups_5min` and `usage_rollups_hourly` using individual `SAVEPOINT`s.
  3. `raw_events`: Bounded FIFO ring (10,000 items / 64 MiB) for individual request/attempt traces. Drops oldest raw items during sustained bursts while 100% of tokens and costs remain preserved in rollups.
- **WAL Checkpoint Supervisor (`REL-3`):** Executes `PASSIVE` checkpoints every 30 seconds, escalates to `RESTART` when WAL exceeds 256 MiB, and flips `/readyz` to 503 if WAL hits 1 GiB or triggers `SQLITE_FULL`.

### 6. Zero-Trust Cryptography & Resumable Rotation

All sensitive data stored at rest is sealed using `XChaCha20-Poly1305` authenticated encryption:

- **Canonical AAD Binding:** Associated Data strictly binds domain and length-prefixed identifiers:
  $$\text{AAD} = \text{"llmroller.aad.v1 "} \parallel \text{u32\_be}(\text{len}(\text{domain})) \parallel \text{domain} \parallel \prod_{f \in \text{fields}} \left(\text{u32\_be}(\text{len}(f)) \parallel f\right)$$
- **Salt & Key Derivation:** Subkeys are derived from `LLMROLLER_MASTER_KEY` via `HKDF-SHA256` salted with the 16-byte `instance_meta.instance_id`.
- **Offline Resumable Master Key Rotation:** The `llmroller rotate-master-key` CLI executes a 500-row batch state machine across 6 sealed tables (`instance_secrets`, `users`, `upstream_keys`, `upstream_usage_snapshots`, `trace_blocks`, `trace_records`). Safe to interrupt and resume at any point.
- **Audit Log HMAC Chain:** Cryptographically linked hash chain (`HMAC-SHA256`) verified via `llmroller verify-audit-log`. Detects sequence gaps, tampered rows, and tail truncations.

### 7. Socket-Level SSRF Protection

LLMRoller wraps outbound HTTP requests in a custom Tower connector (`GuardedConnector`) that checks IP destinations after DNS resolution:

- **Blocked Ranges:** Blocks 16 IPv4 ranges and 11 IPv6 ranges (including loopback `127.0.0.0/8`, link-local `169.254.0.0/16`, AWS metadata `169.254.169.254`, private subnets `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, IPv4-mapped IPv6 `::ffff:0:0/96`, 6to4 `2002::/16`, and NAT64 `64:ff9b::/96`).
- **DNS Rebinding Defense:** Validates each resolved IP address at socket connection time, preventing attackers from using time-of-check-to-time-of-use DNS rebinding.
- **Save-Time URL Validator:** Enforces `https://` schemes (unless `allow_private_network = true` is explicitly enabled) and rejects non-standard octal, hex, or integer IP encodings.

---

## Quick Start

### Docker Compose (Production-Ready)

Deploy LLMRoller with isolated internal networks, Caddy reverse proxy with automatic TLS, and Prometheus metrics scraping:

```bash
# 1. Clone repository
git clone https://github.com/misaalanshori/LLMRoller.git
cd LLMRoller

# 2. Configure environment
cp .env.example .env

# Generate a cryptographically secure 256-bit master key
MASTER_KEY=$(docker run --rm gcr.io/distroless/static-debian12:nonroot -c "" 2>/dev/null || cargo run --release --bin llmroller -- generate-master-key)
sed -i "s|CHANGE_ME_GENERATE_WITH_LLMROLLER_GENERATE_MASTER_KEY|${MASTER_KEY}|" .env

# 3. Launch cluster
docker compose up -d

# 4. Read setup token from stderr / container logs
docker compose logs llmroller | grep "SETUP TOKEN"
```

### Local Native Build

Prerequisites: Rust 1.99.0 toolchain (pinned by `rust-toolchain.toml`, which also declares the MSRV in `Cargo.toml`) and standard build tools.

```bash
# 1. Build release binary
cargo build --release --bin llmroller

# 2. Generate master key
export LLMROLLER_MASTER_KEY=$(target/release/llmroller generate-master-key)
export LLMROLLER_MASTER_KEY_ID="key-v1"
export LLMROLLER_DATA_DIR="./data"

# 3. Start daemon
target/release/llmroller serve
```

### Initial 1-Click Setup Walkthrough

When LLMRoller starts on an uninitialized database:
1. It generates an ephemeral 32-character base62 setup token and writes it to a file with `0600` permissions (`$LLMROLLER_DATA_DIR/setup-token`).
2. Navigate to `http://localhost:8081/setup` in your browser (or submit `POST /admin/api/setup`).
3. Enter the setup token from the file, specify your initial administrator username and strong password, and capture your 2FA TOTP secret.
4. Once completed, the setup token file is immediately deleted, and all `/setup` routes are permanently locked to **HTTP 404**.

---

## Coding Agent Integration Guides

LLMRoller acts as an in-place drop-in proxy for all major coding assistants.

### Claude Code

Point Claude Code to LLMRoller's data-plane listener (`:8080`):

```bash
# Export proxy base URL and LLMRoller Client Key
export ANTHROPIC_BASE_URL="http://127.0.0.1:8080"
export ANTHROPIC_API_KEY="llr_live_xxxxxxxxxxxxxxxxxxxxxxxxxxxx"

# Launch Claude Code
claude
```

*Subagent sessions (`x-claude-code-agent-id`) and conversation chains are automatically routed with stickiness to the same upstream key without leaking personal tokens.*

### OpenAI Codex & SDKs

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:8080/v1",
    api_key="llr_live_xxxxxxxxxxxxxxxxxxxxxxxxxxxx"
)

response = client.chat.completions.create(
    model="claude-3-7-sonnet", # Canonical catalog ID or confirmed alias
    messages=[{"role": "user", "content": "Refactor this function..."}],
    stream=True
)

for chunk in response:
    if chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="")
```

### OpenCode Go

Configure OpenCode Go in `~/.config/opencode/config.json`:

```json
{
  "api_endpoint": "http://127.0.0.1:8080",
  "api_key": "llr_live_xxxxxxxxxxxxxxxxxxxxxxxxxxxx",
  "model": "deepseek-v4.1-flash"
}
```

### Cursor / Cline / Windsurf

In your IDE settings:
- **OpenAI Compatible Base URL:** `http://127.0.0.1:8080/v1`
- **API Key:** `llr_live_xxxxxxxxxxxxxxxxxxxxxxxxxxxx`
- **Model Name:** Enter any canonical ID served by your enabled providers (e.g. `gpt-4o`, `claude-3-5-sonnet`, `deepseek-v4.1-flash`).

---

## API & Wire Reference

### Data Plane (`:8080`)

Authenticated via Bearer token or `x-api-key: llr_...`. Evaluates Request Gates `G1` through `G7`.

| Method | Endpoint | Description |
| :--- | :--- | :--- |
| `POST` | `/v1/chat/completions` | OpenAI wire-format completions. Supports streaming SSE and usage injection. |
| `POST` | `/v1/responses` | OpenAI Responses API. Handles `previous_response_id` turn chaining. |
| `GET` | `/v1/responses/{id}` | Inspect turn state for a linked response. |
| `DELETE` | `/v1/responses/{id}` | Delete response link and turn state. |
| `POST` | `/v1/responses/{id}/cancel` | Cancel an active chained response turn. |
| `POST` | `/v1/messages` | Anthropic wire-format streaming and non-streaming messages API. |
| `POST` | `/v1/messages/count_tokens` | Token counting pass-through for Anthropic models. |
| `GET` | `/v1/models` | OpenAI / Anthropic format catalog of available models, status, and aliases. |
| `GET` | `/v1/usage` | Authenticated client key budget status and active window usage. |
| `GET` | `/v1/usage/raw` | Decrypted raw source JSON for own personal keys; sanitized JSON for global/shared. |
| `GET` | `/v1/key` | Current client key rate limits, monthly caps, and expiration status. |
| `GET` | `/healthz` | Kubernetes liveness probe (returns 200 OK immediately). |
| `GET` | `/readyz` | Kubernetes readiness probe (returns 503 during startup/drain; 200 when ready). |

### Control Plane & Admin Console (`:8081`)

Authenticated via HTTP-only session cookie (`__Host-llmroller_session`) + anti-CSRF token (`x-csrf-token`).

| Route Group | Endpoints | Access |
| :--- | :--- | :--- |
| **Web UI** | `GET /app`, `GET /admin` | Dual-workspace Askama + HTMX user workspace and admin console. |
| **Auth** | `POST /api/auth/login`, `POST /api/auth/logout` | Session login with 3-tier rate limiter and constant-time CSRF. |
| **Providers** | `/api/user/providers`, `/admin/api/providers` | Manage personal and global upstream providers (Base URL, SSRF settings). |
| **Keys** | `/api/user/providers/{id}/keys` | Add, calibrate, strict-tier, or disable upstream API keys. |
| **Sharing** | `/api/user/shares`, `/api/user/shares/{id}/accept` | Grant and cryptographically accept personal key shares. |
| **Client Keys**| `/api/user/client-keys`, `POST .../rotate` | Issue coding agent tokens, set CIDR filters, RPM limits, and grace rotation. |
| **Catalog** | `/admin/api/models`, `POST .../merge` | Manage models, confirmed aliases, redirects, and kill switches. |
| **Settings** | `GET /admin/api/settings`, `PATCH ...` | Dynamic closed 14-key runtime settings (SWRR strategy, exposure mode). |
| **Traceability**| `GET /api/user/sessions/{hash}/preview` | Sanitized read-only session chat preview with block-hash LCP. |

### Observability (`:9090`)

- `GET /metrics`: Prometheus text-format scraper endpoint with strict label allowlists (`provider_kind`, `provider_id`, `protocol`, `endpoint`, `status_class`, `outcome`, `strategy`, `job`, `reason`, `model_bucket`). Never emits high-cardinality tokens, prompt text, or user IDs.

---

## CLI Reference & Operations

LLMRoller ships as a single static self-contained binary:

```bash
LLMRoller v1.0.0 (commit a778efc, schema 1)
High-performance LLM load-balancer, reverse proxy, and usage governance engine

Usage: llmroller <COMMAND>

Commands:
  serve                 Run the LLMRoller proxy and control plane daemon
  generate-master-key   Generate a random 256-bit base64 master key
  rotate-master-key     Rotate master key across all sealed SQLite tables (offline)
  verify-audit-log      Verify HMAC-SHA256 hash chain integrity on SQLite audit_log
  backup                Perform an online crash-safe backup of the SQLite database
  reset-admin           Emergency break-glass admin password reset
  healthcheck           Probe local data plane readiness endpoint for container checks
  help                  Print help message or the help of the given subcommand
```

### Daemon Mode (`serve`)

```bash
llmroller serve \
  --data-dir /var/lib/llmroller \
  --listen 0.0.0.0:8080 \
  --admin-listen 127.0.0.1:8081 \
  --metrics-listen 127.0.0.1:9090
```

Listener values can also come from `LLMROLLER_LISTEN`, `LLMROLLER_ADMIN_LISTEN`, and
`LLMROLLER_METRICS_LISTEN`; explicit flags take precedence over environment
variables, which take precedence over the built-in defaults.

Executes the formal 10-step startup sequence (lock acquisition, schema migration, instance secret unsealing, writer and reloader task spawning, startup budget reconstruction, and `/readyz` flip). Handles `SIGTERM` / `SIGINT` with an 8-step graceful drain and WAL checkpoint.

### Keygen (`generate-master-key`)

```bash
$ llmroller generate-master-key
2xJ9kL+vB5rN1mP8qT0wY4uI7oA3sD6fG9hJ2kL5nM8=
```

### Resumable Key Rotation (`rotate-master-key`)

Re-encrypts all database secrets when updating master keys:

```bash
export LLMROLLER_MASTER_KEY_OLD="old_base64_key=="
export LLMROLLER_MASTER_KEY_NEW="new_base64_key=="

llmroller rotate-master-key \
  --old-key-env LLMROLLER_MASTER_KEY_OLD \
  --new-key-env LLMROLLER_MASTER_KEY_NEW \
  --new-key-id key-v2 \
  --data-dir /var/lib/llmroller
```

*State machine operates in 500-row transactions (`BEGIN IMMEDIATE`) and records progression in `instance_meta.rotation_state`. If terminated midway, rerun the command to resume.*

### Audit Chain Verification (`verify-audit-log`)

Audits the cryptographic HMAC chain and anchor sequence:

```bash
# Verify integrity across the entire log
llmroller verify-audit-log --data-dir /var/lib/llmroller

# Verify against an emitted anchor log line (detects tail truncation)
llmroller verify-audit-log \
  --data-dir /var/lib/llmroller \
  --anchor-seq 500 \
  --anchor-hash 9f83c12... \
  --json
```

### Atomic Online Backup (`backup`)

Performs an online, non-blocking backup with directory `fsync` and PRAGMA integrity verification:

```bash
llmroller backup \
  --data-dir /var/lib/llmroller \
  --out /var/backups/llmroller-backup.db
```

The JSON summary reports the database's `instance_meta.active_master_key_id`;
`LLMROLLER_MASTER_KEY_ID` does not need to be set for `backup`.

*Exits with code `6` if the backup file fails SQLite integrity verification.*

### Break-Glass Admin Recovery (`reset-admin`)

Reset an admin account password from the host terminal when locked out:

```bash
llmroller reset-admin \
  --data-dir /var/lib/llmroller \
  --username admin
```

*Generates a temporary password, prints it once to `stderr`, sets
`must_change_password = 1`, and appends an `admin.break_glass` audit row to the
cryptographic audit log.*

### Container Healthcheck (`healthcheck`)

Lightweight probe querying the local readiness endpoint:

```bash
llmroller healthcheck --url http://127.0.0.1:8080/readyz
# Exit 0 if ready; Exit 1 if starting, draining, or degraded.
```

---

## Configuration Reference

### Environment Variables

| Variable | Default | Purpose |
| :--- | :--- | :--- |
| `LLMROLLER_MASTER_KEY` | *(None)* | Primary 256-bit base64 AEAD master key. |
| `LLMROLLER_MASTER_KEY_FILE` | *(None)* | Path to file containing master key (trims trailing whitespace). |
| `LLMROLLER_MASTER_KEY_ID` | *(required)* | Identifier for key version tracking and AAD binding; missing or empty exits `3` (`SEC-2`). |
| `LLMROLLER_DATA_DIR` | `"/var/lib/llmroller"` | Directory for SQLite database, WAL, lock file, and setup token. |
| `LLMROLLER_LISTEN` | `"0.0.0.0:8080"` | Address and port for the proxy data plane. |
| `LLMROLLER_ADMIN_LISTEN` | `"127.0.0.1:8081"` | Address and port for the Web UI and admin API. |
| `LLMROLLER_METRICS_LISTEN`| `"127.0.0.1:9090"` | Address and port for the Prometheus scraper. |
| `LLMROLLER_TRUSTED_PROXIES`| `""` | Comma-separated CIDRs of reverse proxies (e.g. `"10.0.0.0/8,172.16.0.0/12"`). |
| `LLMROLLER_REQUIRE_TLS` | `false` | When `true`, rejects a non-loopback data listener without TLS or trusted proxies. |
| `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB`| `min(1024, floor(0.40 * memory_limit_mb))` | Maximum pooled memory for in-flight pre-buffered request bodies. |
| `RUST_LOG` | `"info"` | Tracing log level filter (`error`, `warn`, `info`, `debug`, `trace`). |

### Dynamic Runtime Settings

Administrators can update runtime settings without downtime via `PATCH /admin/api/settings`:

| Key | Default | Validation & Effect |
| :--- | :--- | :--- |
| `default_routing_strategy` | `"smooth_weighted_rr"` | Default algorithm (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `p2c`, etc.). |
| `global_exposure_mode` | `"union"` | `"union"` (exposed if any key serves) or `"intersection"` (requires all keys). |
| `price_sync_catalog_url` | `"https://models.dev/api.json"` | Target URL for automated price-book synchronization. |
| `timeout_failover` | `false` | Whether to trigger failover on upstream request timeouts. |
| `recovery_repin_min_idle_secs`| `300` | Idle seconds before a session pinned away from personal tier returns home. |
| `retention_usage_events_days`| `90` | Retention window for individual raw usage event records. |
| `retention_usage_rollups_days`| `365` | Retention window for aggregated hourly usage rollups. |
| `retention_trace_records_days`| `30` | Retention window for content traceability records. |

---

## Development, Verification & TDD Governance

LLMRoller is built under strict Test-Driven Development governance. Every requirement in `docs/srdd/requirements.toml` is verified through automated unit, property-based, golden-fixture, and integration tests.

### Workspace Structure

```
LLMRoller/
├── crates/
│   ├── core/      # Pure domain logic (routing algorithms, pricing, sessions, pins)
│   ├── proto/     # Protocol parsing, surgical body editing, headers, SSE taps
│   ├── store/     # SQLite persistence, migrations, encryption, and writer actor
│   ├── proxy/     # Hyper/Axum proxy, SSRF GuardedConnector, failover orchestrator
│   ├── admin/     # Askama templates, HTMX UI, admin API endpoints, rate limiter
│   └── server/    # CLI entrypoints, daemon lifecycle, graceful drain supervisor
├── fixtures/      # Golden test vectors (crypto AAD, golden SSE frames, usage docs)
├── presets/       # Seed TOML configurations for supported upstream providers
├── docs/          # Canonical specifications (SRDD) and Big-Bang phased roadmap
└── xtask/         # Continuous architecture governance and traceability verifiers
```

### Running the Verification Pipeline

```bash
# 1. Run all workspace unit, property, and integration tests
cargo test --workspace

# 2. Check strict formatting and Clippy lints
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings

# 3. Audit supply-chain dependencies (licenses, bans, vulnerabilities)
cargo deny check advisories bans licenses sources

# 4. Verify documentation consistency & test path resolution
cargo xtask doc-consistency

# 5. Verify master documentation bundle synchronization
cargo xtask docs-bundle --check

# 6. Verify 100% requirement-to-test traceability
cargo xtask traceability --milestone M6
```

---

## License

LLMRoller is distributed under the terms of both the **MIT License** and the **Apache License (Version 2.0)**. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) for details.
