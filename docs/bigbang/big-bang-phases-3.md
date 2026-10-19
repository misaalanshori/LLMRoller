# Epic 3: Model catalog, personal sharing, smooth WRR routing, and session pinning

Epic 3 builds the model catalog, personal provider sharing (`provider_shares`), the nine-step session resolver, and the eight-check eligibility filter. It also implements smooth weighted round-robin (`smooth_wrr`), single-flight session pinning, and pre-first-byte failover. It corresponds to Milestone `M2` (`SRDD Sections 4.3–4.6, 6.4, 6.6, 6.7, 10, 11`).

## 3.1 Sub-phase 3.1: `/models` discovery, canonical ID normalization, and exposure rules

This sub-phase implements `/models` polling (`MDL-1`), canonical normalization (`MDL-2`), global auto-intersection (`MDL-3`), and admin force-exposure (`MDL-4`). It also implements personal model exposure (`MDL-5`), aliases (`MDL-6`), and `GET /v1/models` (`MDL-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.6`, `Section 10.6`, `Section 14.2.2`, `Section 19.1 (ADR-7)`; Requirements `MDL-1`..`MDL-7` |
| Crates touched | `crates/core::catalog`, `crates/proto::discovery`, `crates/store::models`, `crates/proxy::models` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`core::catalog::tests::normalization_and_intersection`, `#[doc = "Spec: MDL-1, MDL-2, MDL-3, MDL-4, MDL-5, MDL-6, MDL-7"]`) asserting: (1) `deepseek/deepseek-v4.1-flash` and `deepseek-v4.1-flash` normalize to `deepseek-v4.1-flash`, while `deepseek-v4-flash` stays a distinct canonical model (`ADR-7`), (2) a model with `exposed = NULL` is exposed in the global pool if and only if every enabled global provider with at least one enabled key serves it (`MDL-3`), (3) a standard user with a personal key serving `claude-sonnet-4-6` sees that model on `GET /v1/models` even if global providers lack it (`MDL-5`), and (4) `GET /v1/models` with `anthropic-version` returns the Anthropic list schema (`MDL-7`). |
| Atomic commits | 1. `feat(core/3.1): implement 6-step canonical model normalization, model merge, alias CRUD, and exposure rules [Spec: MDL-2, MDL-3, MDL-4, MDL-5, MDL-6]` 2. `feat(server/3.1): implement scheduled /models discovery job and GET /v1/models handler [Spec: MDL-1, MDL-7]` |
| Risks and resolutions | **Risk:** One provider's `/models` endpoint is temporarily unreachable during startup or periodic refresh, which could shrink the global intersection set if treated as an empty catalog. **Resolution:** Keep the last successful `model_routes` rows for a key when a `/models` refresh fails with a network or 5xx error, only removing routes when `/models` returns HTTP 200 without that model or when the key is disabled. |

## 3.2 Sub-phase 3.2: Personal provider and key sharing (`provider_shares`)

This sub-phase implements user-to-user sharing grants on `personal` providers and keys, optional `allowed_models` filters, monthly micro-USD caps (`max_monthly_micro_usd`), non-transitive share enforcement, and instant revocation (`SRDD Section 4.4, Section 6.4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.4`, `Section 4.6`, `Section 6.4`, `Section 8.2`; Requirements `SHR-1`..`SHR-6`, `SEC-17` |
| Crates touched | `crates/core::shares`, `crates/store::shares` |
| TDD step (`Red -> Green`) | Write unit and SQLite integration tests (`tests/integration_personal_sharing.rs`, `#[doc = "Spec: SHR-1, SHR-2, SHR-3, SHR-4, SHR-5, SHR-6, SEC-17"]`) verifying: (1) Alice (`user`) can share her personal provider or key with Bob (`user`) with `allowed_models = ["deepseek-v4.1-flash"]` and `max_monthly_micro_usd = 5_000_000`, (2) Bob cannot re-share Alice's key with Carol (`SHR-4`), (3) Bob cannot read Alice's secret or `secret_last4`, and (4) when Bob's monthly spend on the share reaches `$5.00` (minus reserve) or Alice revokes the share, the route becomes immediately ineligible for Bob (`SHR-5`). |
| Atomic commits | 1. `feat(core/3.2): implement personal share grant validation, model filtering, and monthly cap checks [Spec: SHR-1, SHR-2, SHR-3, SHR-4]` 2. `feat(store/3.2): implement provider_shares repository and per-grantee usage aggregation [Spec: SHR-5, SHR-6, SEC-17]` |
| Risks and resolutions | **Risk:** Two concurrent requests from Grantee Bob slightly overshoot `max_monthly_micro_usd`. **Resolution:** Apply the same configurable admission reserve (`default 2%`) to share caps that `LIM-1` applies to upstream key budget windows. |

## 3.3 Sub-phase 3.3: Nine-step inbound session ID resolver and tenant-salted affinity hints

This sub-phase implements the nine-step session ID precedence (`SRDD Section 11.1`) and 128-bit `HMAC-SHA256` privacy hashing. It also adds HyperLogLog constant-session detection (`Section 11.1.1`) and tenant-salted outbound affinity hints (`SEC-16`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 11.1`, `Section 11.2`, `Section 6.7`, `Section 8.2`; Requirements `RTE-1`, `SEC-16` |
| Crates touched | `crates/core::session`, `crates/proto::affinity` |
| TDD step (`Red -> Green`) | Write table-driven tests (`core::session::tests`, `#[doc = "Spec: RTE-1, SEC-16"]`) testing all nine precedence levels, verifying that `x-claude-code-agent-id` does not split a parent `x-claude-code-session-id` pin, that values `> 256` bytes or containing control characters are rejected/hashed safely, that two different users sending the same `session_id = "abc"` on a `global` or `shared` key emit distinct outbound affinity tokens (`SEC-16`), and that 21 distinct opening prompts on one `session_hash` set `suspected_constant_session_id = true`. |
| Atomic commits | 1. `feat(core/3.3): implement nine-step session ID resolver, HMAC hashing, and constant-session detector [Spec: RTE-1]` 2. `sec(proto/3.3): implement tenant-salted outbound affinity hint generator [Spec: SEC-16]` |
| Risks and resolutions | **Risk:** Stateless SDK callers send no session header, and their system prompt changes slightly on turn 2, breaking the Step 9 fallback hash. **Resolution:** Hash only the first 4 KiB of the static system prompt plus the **first** user message (`messages[0..=1]`), which remains constant across multi-turn stateless history arrays. |

## 3.4 Sub-phase 3.4: Eight-check candidate eligibility engine and `routing_scope` pool selector

This sub-phase implements the eight-check candidate eligibility filter (`SRDD Section 10.1`) and the four `routing_scope` pool combinators (`personal_first`, `blended`, `personal_only`, `global_only` in `SRDD Section 10.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.5`, `Section 6.5`, `Section 6.7`, `Section 10.1`, `Section 10.2`; Requirements `CLK-3`, `RTE-3` |
| Crates touched | `crates/core::eligibility` |
| TDD step (`Red -> Green`) | Write exhaustive unit tests (`core::eligibility::tests`, `#[doc = "Spec: CLK-3, RTE-3"]`) toggling each of the eight eligibility conditions one by one (client key model allow-list, user ownership/share/global quota authorization, model exposure, enabled flags, protocol support, key health/cooldown expiry, circuit breaker state, and window headroom) and testing all four `routing_scope` modes. Run `cargo mutants` on `crates/core::eligibility` to confirm every boolean condition in the 8-check filter is covered by a dedicated negative test. |
| Atomic commits | 1. `feat(core/3.4): implement eight-check route eligibility filter and routing_scope pool selector [Spec: CLK-3, RTE-3]` |
| Risks and resolutions | **Risk:** A user sets `routing_scope = 'personal_first'`, their personal key serves `deepseek-v4.1-flash`, and they request `claude-sonnet-4-6` which only a global key serves. **Resolution:** Because `Pool_Personal_And_Shared` is filtered per requested model `M` first, it is empty for `claude-sonnet-4-6`, so `personal_first` cleanly falls back to `Pool_Global` for that model while keeping `deepseek-v4.1-flash` on the personal key. |

## 3.5 Sub-phase 3.5: Smooth weighted round-robin (`smooth_wrr`), per-model overrides, and headroom scaling

This sub-phase implements nginx's smooth weighted round-robin algorithm (`SRDD Section 10.3`) and three-level effective weight resolution. It also supports `strict` tier filtering and `headroom` budget scaling (`RTE-4`..`RTE-6`, `RTE-13`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.7`, `Section 10.3`; Requirements `RTE-4`, `RTE-5`, `RTE-6`, `RTE-13` |
| Crates touched | `crates/core::wrr` |
| TDD step (`Red -> Green`) | Write both the worked-example unit test from `SRDD Section 10.3.1` (`deepseek-v4.1-flash` at `100:60:30` and `mimo-v2.6-pro` at `60:20:15`) and the `proptest` convergence and burstiness suite (`core::wrr::proptests`, `#[doc = "Spec: RTE-4, RTE-5, RTE-6, RTE-13"]`). Verify that over 100,000 picks across random weight vectors, empirical shares stay within `0.5%` of exact weights and consecutive runs never exceed `ceil(max(W) / min(W))`. |
| Atomic commits | 1. `feat(core/3.5): implement smooth weighted round-robin, per-model priority overrides, strict tiers, and headroom scaling [Spec: RTE-4, RTE-5, RTE-6, RTE-13]` |
| Risks and resolutions | **Risk:** When a route temporarily leaves the candidate set (e.g., enters `cooling_down`) and later returns, stale `current_weight` counters could cause a sudden burst. **Resolution:** Reset a route's `current_weight` to `0` whenever the active candidate set membership changes for that `(pool_scope, canonical_model_id)`, preserving the invariant `sum(current_weight) == 0`. |

## 3.6 Sub-phase 3.6: Single-flight session pinning, `previous_response_id` links, and pre-first-byte failover

This sub-phase implements `moka` single-flight provisional pinning (`RTE-7`), first-byte confirmation (`RTE-8`), sliding TTL persistence (`RTE-10`), and repin tracking (`RTE-11`). It also implements `previous_response_id` affinity (`RTE-12`) and pre-first-byte failover (`RTE-9`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.7`, `Section 10.4`, `Section 10.5`; Requirements `RTE-2`, `RTE-7`..`RTE-12`, `API-8` |
| Crates touched | `crates/core::pins`, `crates/core::router`, `crates/store::pins`, `crates/proxy::failover` |
| TDD step (`Red -> Green`) | Write the 100-way concurrency race test (`tests/integration_routing_and_failover.rs`, `#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-9, RTE-10, RTE-11, RTE-12"]`): (1) 100 parallel first-turn requests for one new session ID select the exact same `upstream_key_id`, (2) upstream 503/429/401 before the first byte retries up to 2 alternative candidates, updates key health (`cooling_down`, `exhausted`, `invalid`), and confirms the pin on the candidate that yields the first byte, (3) `429` with `Retry-After: 1` sleeps 1 s on `FakeClock` and retries the **same** key once to preserve cache, and (4) stream failure after 1 byte never retries. |
| Atomic commits | 1. `feat(core/3.6): implement single-flight pin cache, routing_scope pin re-validation, and response_links [Spec: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, RTE-12]` 2. `feat(proxy/3.6): implement pre-first-byte failover, key/route health transitions, provider circuit breaker, and GET/DELETE/compact /v1/responses/{id} [Spec: RTE-9, RTE-12, API-8, REL-5]` |
| Risks and resolutions | **Risk:** If the winning task inside `moka::try_get_with` panics or is cancelled by client disconnect before upstream responds, waiting tasks could hang. **Resolution:** Keep `try_get_with` strictly synchronous in `crates/core` (inserting the `provisional` pin in `< 10 µs`), and if the first upstream call fails before the first byte, atomically CAS-update the provisional pin to the failover candidate. |
