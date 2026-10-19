# LLMRoller: Big-Bang Development Phases and TDD Execution Guide

Version 1.1 · 19 October 2026 · Adaptive ground-up engineering guide from `git init` to `v1.0.0` with full traceability to the LLMRoller SRDD.


## Epic 0: Engineering governance, TDD workflow, atomic commits, and adaptive playbook

This guide defines how engineers and coding agents build LLMRoller from an empty repository (`git init`) to version 1.0.0. It pairs every phase and sub-phase with exact references to the Software Requirements and Design Document (SRDD Sections 0 through 20).

### 0.1 How to use this guide as an adaptive playbook

This document is an adaptive engineering playbook, not a rigid waterfall script. Software development uncovers upstream API quirks, latency bottlenecks, and schema refinements as real code meets real traffic.

The eight epics (`big-bang-phases-0.md` through `big-bang-phases-7.md`) follow the dependency graph of the six Cargo crates (`core`, `proto`, `store`, `proxy`, `admin`, `server`). Engineers may reorder sibling sub-phases or split a sub-phase when an upstream quirk appears. They may also merge two sub-phases when a shared trait simplifies both.

| Invariant that never bends | Flexible aspect that adapts as needed |
| --- | --- |
| Every requirement ID (`AUTH-*`, `USR-*`, `PRV-*`, `SHR-*`, `CLK-*`, `MDL-*`, `RTE-*`, `API-*`, `USG-*`, `LIM-*`, `TRC-*`, `UI-*`, `AUD-*`, `PERF-*`, `REL-*`, `TRN-*`, `COM-*`, `POR-*`, `MNT-*`, `OBS-*`, `PRIV-*`, `SEC-*`) must map to passing tests (`SRDD Section 16.6`). | The internal struct layout, helper functions, and private module organization inside each crate can change freely during refactoring. |
| `crates/core` must remain 100% free of network and disk I/O (`SRDD Section 15.2`, `MNT-2`). | The order in which provider adapters (`opencode_go`, `command_code`, `open_design_amr`, `openrouter`) are implemented can shift based on test key availability. |
| Red-Green-Refactor TDD, the Expected vs. Unexpected failure rules, and zero surviving mutants in [cargo-mutants](https://mutants.rs/) apply to every commit (`SRDD Section 16.2`, `16.3`). | Batch sizes, channel capacities, cache TTL defaults, and SQLite connection pool sizes can be tuned empirically during benchmark phases. |
| Every architectural or contract change cascades in order: `SRDD -> Seam Trait / Contract Test -> Code -> Traceability Check`. | UI layout details and HTMX partial boundaries can evolve as user workflows are tested in Epic 6. |

### 0.2 The seven-step Red-Green-Refactor-Mutate TDD loop

Every sub-phase in Epics 1 through 7 is built through a strict seven-step loop. No production logic lands before its failing contract test exists (`SRDD Section 16.3`).

| Step | Loop stage | Developer action and verification command |
| --- | --- | --- |
| 1 | Trace to SRDD | Read the target SRDD section and requirement IDs listed in the sub-phase header. If a new edge case changes a contract, update the SRDD requirement table first. |
| 2 | Define seam or type | Add or update the domain structs and seam traits (`Clock`, `EntropySource`, `UpstreamTransport`, `UsageEndpointClient`, `StorageRepository`, `SecretSealer` from `SRDD Section 16.4`) with `todo!()` bodies so the crate compiles. |
| 3 | Write failing test (`Red`) | Write unit, property (`proptest`), or integration tests against the observable contract. Annotate each test with `#[doc = "Spec: <REQ-ID>"]` and run `cargo nextest run` to confirm it fails for the expected reason. |
| 4 | Write minimal code (`Green`) | Write the smallest, clearest implementation in safe Rust (`#![forbid(unsafe_code)]`) that makes the new test pass while keeping all prior tests green. |
| 5 | Refactor under green | Remove duplication, simplify control flow, and run `cargo clippy --all-targets -- -D warnings` plus `cargo nextest run`. |
| 6 | Kill surviving mutants | Run `cargo mutants --in-diff HEAD~1..HEAD` (or against the working tree diff). If any mutant survives, add a boundary or negative test until zero mutants survive (`SRDD Section 16.5.6`). |
| 7 | Commit atomically | Run `python3 scripts/verify_traceability.py`, stage the test and implementation together, and create one atomic commit following Section 0.4. |

### 0.3 Failing test triage: expected vs. unexpected failures

As defined in `SRDD Section 16.2`, failing tests divide into two categories. Treating an unexpected regression like an expected red test destroys the safety net of the suite.

| Failure type | When it occurs | Mandatory engineering response |
| --- | --- | --- |
| Expected failure | (a) A newly written test guard in Step 3 (`Red`) before its implementation lands, or (b) an existing test that pinned an old behavior being intentionally replaced by a new requirement or `ADR-*` decision. | For (a), write the implementation in Step 4 until green. For (b), update or retire the old test in the **same atomic commit** as the implementation change, and name the sanctioning requirement ID or `ADR-*` in the commit message. |
| Unexpected failure | Changing component X causes an existing test in component Y (or an unrelated contract in X) to fail. | **Stop immediately.** Do not loosen assertions, do not add `#[ignore]`, and do not tweak mock return values to force green. Diagnose whether an architectural boundary leaked or an invariant broke, fix the root cause, and add a regression test if a hidden bug was exposed. |

### 0.4 Atomic commit and branch discipline

Work progresses in small, self-contained atomic commits. Each sub-phase typically produces between two and six atomic commits.

| Rule | Atomic commit standard |
| --- | --- |
| One logical change per commit | Never mix pure structural refactoring (renaming modules, extracting helpers) with behavioral changes in the same commit. |
| Test and code travel together | Every behavioral commit contains both the contract test (`#[doc = "Spec: ..."]`) and the production code that makes it pass, plus any test updated under the Expected Failure rule. |
| Always green on `main` | Every commit must compile with zero warnings (`cargo clippy -- -D warnings`), pass `cargo nextest run`, and pass `cargo mutants --in-diff`. |
| Traceable commit message format | Use the header format `<type>(<crate>/<subphase>): <imperative summary>` followed by a body line `Spec: <REQ-IDs>` and `Tests: <test_module_paths>`. Allowed types: `feat`, `fix`, `refactor`, `test`, `sec`, `perf`, `docs`, `ci`. |

Example atomic commit message for Sub-Phase 3.2:

```text
feat(core/3.2): enforce monthly spend cap and model filter on personal shares

Spec: SHR-2, SHR-3, SHR-5, RTE-3, SEC-17
Decision: ADR-1, ADR-3
Tests: crates/core/src/shares/tests.rs::share_eligibility_and_cap_exhaustion
Mutants: 14 generated, 14 killed, 0 survived
```

### 0.5 Adaptive change-management and pivot protocol

When real-world conditions change during development, engineers follow the table below to adjust the plan cleanly without accumulating technical debt.

| Trigger event during development | How development changes | Cascade adjustment steps |
| --- | --- | --- |
| An upstream provider changes its `/models` or `/usage` JSON schema or auth header | Only the specific `ProviderAdapter` in `crates/proto` and its preset TOML change (`MNT-2`). `crates/core` and `crates/proxy` stay untouched. | 1. Capture the new live payload into `fixtures/usage/` or `fixtures/sse/`. 2. Update the golden contract test (Expected Failure citing the upstream change). 3. Update the parser in `crates/proto`. |
| A client harness (Claude Code, Codex CLI, opencode) introduces a new session header | Add the new header name to the precedence table in `SRDD Section 11.1` and `crates/core::session`. | 1. Update `SRDD Section 11.1`. 2. Add a table-test row in `core::session::tests`. 3. Add the header constant to the resolver slice. |
| SQLite write latency spikes when content traceability mode (`TRC-2`) is active at 500 req/s | Decouple trace payload persistence from the `usage_events` batch transaction using a dedicated trace ring buffer and separate `zstd` worker pool. | 1. Check `PERF-1` and `PERF-5` benchmarks. 2. Adjust batch interval or worker count in `crates/store::traces`. 3. Verify `REL-3` non-blocking overflow test still passes. |
| A sub-phase reveals a missing edge case in route eligibility or RBAC | Pause feature work in the current sub-phase and lock the boundary in `crates/core` first. | 1. Add the edge case to `SRDD Section 6` or `Section 8`. 2. Write the red unit test in `crates/core`. 3. Fix the pure logic and run `cargo mutants` on `crates/core`. |

### 0.6 Epic overview map (zero to v1.0.0)

The table below maps the seven implementation epics (`big-bang-phases-1.md` through `big-bang-phases-7.md`) to SRDD milestones and requirement families.

| Epic file | Epic title | SRDD milestone | Requirement families covered |
| --- | --- | --- | --- |
| `big-bang-phases-1.md` | Foundation, storage engine, cryptography, and multi-role auth | `M0` | `AUTH-1`..`AUTH-9`, `USR-1`..`USR-5`, `AUD-1`, `SEC-1`..`SEC-5`, `SEC-10`, `SEC-12`..`SEC-14`, `SEC-17`, `MNT-1`, `MNT-2` |
| `big-bang-phases-2.md` | Transparent wire proxy, protocol normalizer, and provider adapters | `M1` | `PRV-1`..`PRV-8`, `CLK-1`..`CLK-5`, `API-1`..`API-8`, `TRN-1`..`TRN-3`, `SEC-6`..`SEC-8`, `SEC-15`, `PERF-1`, `PERF-2` |
| `big-bang-phases-3.md` | Model catalog, personal sharing, smooth WRR routing, and session pinning | `M2` | `SHR-1`..`SHR-6`, `MDL-1`..`MDL-7`, `RTE-1`..`RTE-13`, `SEC-16` |
| `big-bang-phases-4.md` | Usage pipeline, price book, budget windows, and source `/usage` engine | `M3`, `M4` | `USG-1`..`USG-8`, `LIM-1`, `REL-2`, `REL-3`, `SEC-18` |
| `big-bang-phases-5.md` | Opt-in content traceability mode and read-only session chat preview | `M3`, `M5` | `TRC-1`..`TRC-8`, `PERF-5`, `SEC-19` |
| `big-bang-phases-6.md` | Dual-workspace HTMX web UI and control plane APIs | `M5` | `UI-1`..`UI-4`, `USR-1`..`USR-5`, `SEC-5`, `SEC-9` |
| `big-bang-phases-7.md` | System hardening, fuzzing, load verification, live harness E2E, and v1.0.0 release | `M6` | `PERF-1`..`PERF-5`, `REL-1`..`REL-6`, `COM-1`, `POR-1`, `OBS-1`, `PRIV-1`, `SEC-11` |

## Epic 1: Foundation, storage engine, cryptography, and multi-role auth

Epic 1 builds the Cargo workspace, the six seam traits, the 20-table SQLite WAL store, and the `XChaCha20-Poly1305` secret sealer. It also implements atomic `/setup` and role-based access control for `admin` and `user` accounts. It corresponds to Milestone `M0` (`SRDD Sections 4, 5, 6.1, 6.2, 8, 9, 15, 16`).

### 1.1 Sub-phase 1.1: Cargo workspace, lint gates, and traceability harness

This sub-phase initializes the six-crate repository layout (`SRDD Section 15.2`) and enforces compile-time safety and supply-chain checks (`MNT-1`, `MNT-2`, `SEC-10`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 15.1`, `Section 15.2`, `Section 16.6`; Requirements `MNT-1`, `MNT-2`, `SEC-10` |
| Crates touched | Root `Cargo.toml`, `deny.toml`, `.cargo/mutants.toml`, `scripts/verify_traceability.py`, skeleton `crates/{core,proto,store,proxy,admin,server}` |
| TDD step (`Red -> Green`) | Write an architectural dependency test (`tests/workspace_boundaries.rs`, `#[doc = "Spec: MNT-1, MNT-2"]`) that parses `cargo metadata` and asserts: (1) every crate has `#![forbid(unsafe_code)]`, and (2) `crates/core` has zero dependency edges to `sqlx`, `reqwest`, `hyper`, or `axum`. |
| Atomic commits | 1. `ci(workspace/1.1): initialize six-crate workspace with forbid(unsafe_code) and deny.toml [Spec: MNT-1, SEC-10]` 2. `test(workspace/1.1): add cargo-metadata boundary test and verify_traceability.py [Spec: MNT-2]` |
| Risks and resolutions | **Risk:** Transitive dependencies pull in duplicate TLS stacks (`openssl-sys`) or unmaintained crates. **Resolution:** Set `default-features = false` on `reqwest` and `sqlx` immediately and run `cargo deny check bans licenses advisories` on the very first commit. |

### 1.2 Sub-phase 1.2: Core domain identifiers, `ActorScope`, and seam traits

This sub-phase defines the fundamental domain primitives in `crates/core` (`ULID` wrappers, `Role`, `ActorScope`, `OwnershipScope`, `RoutingScope`) and the six dependency-injection seam traits (`SRDD Section 16.4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.1`, `Section 4.2`, `Section 8.2`, `Section 16.4`; Requirements `USR-1`..`USR-5`, `SEC-14`, `SEC-17` |
| Crates touched | `crates/core::ids`, `crates/core::rbac`, `crates/core::seams` |
| TDD step (`Red -> Green`) | Write table-driven RBAC contract tests (`core::rbac::tests::actor_scope_matrix`, `#[doc = "Spec: USR-1, USR-2, USR-3, USR-4, SEC-17"]`) testing every `(Role::Admin, Role::User)` action against `global` resources, own `personal` resources, other users' `personal` resources, and `shared` resources. Implement `ActorScope::can_view_provider`, `can_edit_provider`, `can_share_key`, and `can_view_trace` until green, then run `cargo mutants` on `crates/core::rbac`. |
| Atomic commits | 1. `feat(core/1.2): define ULID types, SecretString wrappers, Clock and EntropySource seams [Spec: SEC-14]` 2. `feat(core/1.2): implement ActorScope RBAC guards for admin and standard user roles [Spec: USR-1, USR-2, USR-3, USR-4, SEC-17]` |
| Risks and resolutions | **Risk:** A future handler forgets to check `ActorScope` before running a query. **Resolution:** Make `&ActorScope` a mandatory parameter on every `StorageRepository` trait method so code that omits the scope fails to compile (`SEC-17`). |

### 1.3 Sub-phase 1.3: Cryptographic sealer (`XChaCha20-Poly1305`) and SQLite WAL store

This sub-phase implements master-key derivation (`HKDF-SHA256`), AEAD encryption with row-bound Associated Data (`AAD`), key rotation, and the 20-table SQLite WAL migration (`SRDD Section 8.2, Section 9.1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 8.2`, `Section 9.1`, `Section 9.2`; Requirements `PRV-5`, `SEC-1`, `SEC-2`, `SEC-3`, `SEC-14` (and HKDF subkey prerequisite for `SEC-19`) |
| Crates touched | `crates/store::crypto`, `crates/store::migrations`, `crates/store::pool` |
| TDD step (`Red -> Green`) | 1. Write unit tests (`store::crypto::tests`, `#[doc = "Spec: SEC-1, SEC-2, PRV-5"]`) verifying that: (a) missing or `< 32-byte` `LLMROLLER_MASTER_KEY` returns `StartupError::InvalidMasterKey`, (b) sealing the same secret twice yields distinct ciphertexts (random 24-byte nonce), (c) swapping ciphertext blobs between two `upstream_key.id` rows fails AEAD authentication (`AAD` mismatch), and (d) `rotate_master_key` re-seals all rows under a new `master_key_id`. 2. Write SQLite WAL migration tests verifying all 20 tables and foreign-key constraints. |
| Atomic commits | 1. `sec(store/1.3): implement HKDF-SHA256 domain-separated subkeys, XChaCha20-Poly1305 sealer, and rotate-master-key CLI [Spec: SEC-1, SEC-2, PRV-5]` 2. `feat(store/1.3): add 20-table SQLite WAL migration and connection pool [Spec: REL-2, SEC-17]` |
| Risks and resolutions | **Risk:** `SQLITE_BUSY` errors under concurrent read and write load. **Resolution:** Configure `SqliteConnectOptions` with `journal_mode(Wal)`, `synchronous(Normal)`, `busy_timeout(5s)`, an 8-connection read-only pool, and a single serialized writer connection (`SRDD Section 9.2`). |

### 1.4 Sub-phase 1.4: Atomic first-admin `/setup` and `Argon2id` + `TOTP` authentication

This sub-phase implements the one-time setup token, serializable first-admin creation, permanent 404 lockout after setup, `Argon2id` password hashing, and `TOTP` two-factor verification (`SRDD Section 6.1, Section 8.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.1`, `Section 8.2`; Requirements `AUTH-1`..`AUTH-9`, `USR-5`, `SEC-4`, `SEC-5`, `SEC-12` |
| Crates touched | `crates/store::users`, `crates/admin::setup`, `crates/admin::auth` |
| TDD step (`Red -> Green`) | Write an integration race test (`tests/e2e_setup_and_auth.rs`, `#[doc = "Spec: AUTH-1, AUTH-2, AUTH-3, SEC-4, SEC-5"]`) that fires 20 concurrent `POST /admin/api/setup` requests with a valid setup token against an empty database. Assert that exactly one returns HTTP 201, the other 19 return HTTP 404 or 409, and subsequent `GET /setup` calls return HTTP 404 permanently. Also test `AUTH-5` (last active admin cannot be deleted or disabled) and IP/username rate limiting (`AUTH-8`, `SEC-12`). |
| Atomic commits | 1. `feat(store/1.4): implement user repository with Argon2id, TOTP, and last-admin guard [Spec: AUTH-4, AUTH-5, AUTH-6, AUTH-9, USR-5, SEC-4]` 2. `feat(admin/1.4): implement atomic first-admin setup, session cookies, and login rate limiter [Spec: AUTH-1, AUTH-2, AUTH-3, AUTH-7, AUTH-8, SEC-5, SEC-12]` |
| Risks and resolutions | **Risk:** `Argon2id` (`19 MiB` memory per hash) can exhaust RAM if 100 concurrent login requests run at once. **Resolution:** Bound concurrent `Argon2id` verifications with a `tokio::sync::Semaphore` (4 permits) and apply the per-IP rate limiter (`SEC-12`) before hashing. |

### 1.5 Sub-phase 1.5: Tamper-evident hash-chained `audit_log` and `ArcSwap<ConfigSnapshot>`

This sub-phase implements the cryptographic hash chain on `audit_log` (`AUD-1`, `SEC-13`) and the lock-free `ArcSwap<ConfigSnapshot>` loader (`REL-4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.4`, `Section 6.11`, `Section 8.2`; Requirements `AUD-1`, `REL-4`, `SEC-13` |
| Crates touched | `crates/store::audit`, `crates/core::snapshot`, `crates/server::snapshot_sync` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`store::audit::tests::hash_chain_verification`, `#[doc = "Spec: AUD-1, SEC-13"]`) that insert 10 audit rows, verify `verify_audit_chain()` returns `Ok(())`, mutate one character in row 4 via raw SQL, and assert `verify_audit_chain()` detects the exact tampered row ID. Write a snapshot test (`#[doc = "Spec: REL-4"]`) proving that mutating a user or key swaps `ArcSwap<ConfigSnapshot>` within 10 ms. |
| Atomic commits | 1. `sec(store/1.5): implement SHA-256 hash-chained audit_log and verifier CLI [Spec: AUD-1, SEC-13]` 2. `feat(core/1.5): implement lock-free ArcSwap<ConfigSnapshot> reload pipeline [Spec: REL-4]` |
| Risks and resolutions | **Risk:** Concurrent admin actions race on `prev_hash` when appending to `audit_log`. **Resolution:** Route all `audit_log` inserts through the serialized SQLite writer transaction so `prev_hash` is read and written atomically. |

#### 1.5.1 Incremental `ConfigSnapshot` evolution across Epics 1 through 5

Sub-Phase 1.5 introduces `ArcSwap<ConfigSnapshot>` while Epics 1 through 5 populate its domain tables. Engineers extend `ConfigSnapshot` additively at each sub-phase boundary without breaking earlier tests.

| Sub-phase | New domain slices added to `ConfigSnapshot` | Triggering control-plane mutations that call `reload_snapshot()` |
| --- | --- | --- |
| `1.5` | `users: HashMap<UserId, UserRecord>`, `settings: RuntimeSettings` | User creation, role/budget update, disable/delete, settings update |
| `2.1` | `providers: HashMap<ProviderId, ProviderRecord>`, `upstream_keys: HashMap<UpstreamKeyId, UpstreamKeyRecord>` | Provider or upstream key CRUD, enable/disable (`PRV-8`) |
| `2.2` | `client_keys_by_hash: HashMap<[u8; 32], ClientKeyRecord>` | Client key creation, rotation, revocation (`CLK-4`), CIDR/RPM edit |
| `3.1` | `models: HashMap<CanonicalModelId, ModelRecord>`, `aliases: HashMap<String, CanonicalModelId>`, `routes: HashMap<CanonicalModelId, Vec<RouteRecord>>` | `/models` discovery refresh, force-expose toggle, model merge, alias CRUD |
| `3.2` | `shares_by_grantee: HashMap<UserId, Vec<ShareGrant>>`, `user_route_prefs: HashMap<(UserId, UpstreamKeyId, CanonicalModelId), RoutePref>` | Share grant creation, cap edit, or revocation (`SHR-5`), personal route priority override |
| `4.2` & `4.4` | `prices: HashMap<(ProviderKind, String), PriceEntry>`, `budget_windows: HashMap<UpstreamKeyId, Vec<BudgetWindowSpec>>` | Price book edit or `models.dev` sync, budget window rule update |
| `5.1` | `traceability: TraceabilityConfig` | `PUT /admin/api/settings/traceability` (`TRC-1`) |

## Epic 2: Transparent wire proxy, protocol normalizer, and provider adapters

Epic 2 builds the data-plane HTTP proxy (`:8080`), client key authentication, and SSRF-safe outbound transport. It also adds the surgical JSON editor, the unbuffered SSE tee, and the provider adapters. It corresponds to Milestone `M1` (`SRDD Sections 2, 5, 6.3, 6.5, 6.8, 7.1, 7.3, 8.2, 12`).

### 2.1 Sub-phase 2.1: Global and personal provider records and SSRF-safe DNS connector

This sub-phase implements storage for `global` and `personal` providers (`PRV-1`..`PRV-5`, `PRV-8`). It also adds the custom `reqwest` DNS resolver that blocks private, loopback, and link-local IPs (`PRV-7`, `SEC-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.3`, `Section 6.3`, `Section 8.2`; Requirements `PRV-1`..`PRV-8`, `SEC-7` |
| Crates touched | `crates/store::providers`, `crates/proxy::ssrf`, `crates/proxy::transport` |
| TDD step (`Red -> Green`) | Write SSRF unit and integration tests (`proxy::ssrf::tests::ip_ranges`, `#[doc = "Spec: PRV-7, SEC-7"]`) testing IPv4 and IPv6 loopback (`127.0.0.1`, `::1`), cloud metadata (`169.254.169.254`), RFC 1918 (`10.0.0.1`, `172.16.0.1`, `192.168.1.1`), CGNAT (`100.64.0.1`), IPv4-mapped IPv6 (`::ffff:127.0.0.1`), HTTP (`http://`) rejection, and HTTP 301/302/307/308 redirect rejection. Verify that `allow_private_network = 1` (settable only by `admin`) permits local model servers when explicitly enabled. |
| Atomic commits | 1. `sec(proxy/2.1): implement SSRF-filtering DNS resolver and no-redirect reqwest client [Spec: PRV-7, SEC-7]` 2. `feat(store/2.1): implement global and personal provider and upstream_key repositories [Spec: PRV-1, PRV-2, PRV-3, PRV-4, PRV-8]` |
| Risks and resolutions | **Risk:** DNS rebinding attacks where a hostname resolves to a public IP during config validation and `127.0.0.1` at request time. **Resolution:** Enforce the IP-range check inside `reqwest`'s `Resolve` trait on **every socket connect**, not just when the provider URL is saved in the UI. |

### 2.2 Sub-phase 2.2: Client key (`llr_...`) issuance, constant-time auth, and rate/CIDR guards

This sub-phase implements client key generation (`llr_` + 32 base62 bytes), SHA-256 hashing, and constant-time lookup in `ConfigSnapshot`. It also enforces per-IP failure throttling, RPM token buckets, and CIDR allow lists (`SRDD Section 6.5, Section 8.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.5`, `Section 8.2`; Requirements `CLK-1`..`CLK-5`, `API-2`, `SEC-3`, `SEC-12` |
| Crates touched | `crates/core::client_key`, `crates/store::client_keys`, `crates/proxy::auth` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`tests/integration_client_keys.rs`, `#[doc = "Spec: CLK-1, CLK-2, CLK-3, CLK-4, CLK-5, SEC-3, SEC-12"]`) verifying: (1) raw token is returned only on creation/rotation, (2) both `Authorization: Bearer llr_...` and `x-api-key: llr_...` authenticate, (3) revoking a key rejects calls within 1 s (`CLK-4`), (4) CIDR mismatches return 403, and (5) 21 failed bearer requests from one IP within 1 minute return HTTP 429 (`SEC-12`). |
| Atomic commits | 1. `feat(core/2.2): implement client key generation, constant-time verification, CIDR and RPM guards [Spec: CLK-1, CLK-2, CLK-3, CLK-5, SEC-3]` 2. `feat(proxy/2.2): wire client key auth middleware and IP failure rate limiter [Spec: CLK-4, API-2, SEC-12]` |
| Risks and resolutions | **Risk:** Updating `client_keys.last_used_at` synchronously on every request would hit SQLite on the hot path. **Resolution:** Record `last_used_at` in an in-memory `DashMap<ClientKeyId, i64>` and flush dirty timestamps to SQLite inside the 250 ms batch writer task. |

### 2.3 Sub-phase 2.3: Four-rule header pipeline, smuggling guards, and `User-Agent` policy

This sub-phase implements request smuggling rejection (`SEC-15`), hop-by-hop and credential stripping (`API-2`), and agent header passthrough (`API-3`). It also implements the `pass`, `label`, and `reject` `User-Agent` policies (`SRDD Section 12.1, Section 12.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 12.1`, `Section 12.2`, `Section 8.2`; Requirements `API-2`, `API-3`, `SEC-6`, `SEC-8`, `SEC-15` |
| Crates touched | `crates/proto::headers`, `crates/proxy::edge` |
| TDD step (`Red -> Green`) | Write table-driven tests (`proto::headers::tests`, `#[doc = "Spec: API-2, API-3, SEC-8, SEC-15"]`) asserting that `x-claude-code-session-id`, `x-claude-code-agent-id`, `anthropic-beta`, `openai-beta`, `x-stainless-*`, and `x-cmd-zdr` pass through untouched, while `authorization`, `x-api-key`, `cookie`, `x-forwarded-for`, and `x-llmroller-*` are stripped and replaced with the upstream credential. Verify that requests with both `Content-Length` and `Transfer-Encoding` or duplicate `Host` headers are rejected with HTTP 400 (`SEC-15`). |
| Atomic commits | 1. `sec(proto/2.3): implement HTTP smuggling checks and four-rule header pipeline [Spec: API-2, API-3, SEC-8, SEC-15]` 2. `feat(proto/2.3): implement pass, label, and reject User-Agent policies [Spec: API-3]` |
| Risks and resolutions | **Risk:** An upstream provider rejects a new experimental `anthropic-beta` flag sent by Claude Code (`SRDD Section 2.6 F7`). **Resolution:** Support glob patterns in `provider.config.strip_headers` (or comma-separated token filtering inside `anthropic-beta`) so operators can strip one beta token without losing other headers. |

### 2.4 Sub-phase 2.4: Zero-copy surgical JSON span editor

This sub-phase implements the top-level JSON span scanner in `crates/proto::body_edit` (`SRDD Section 12.3`). It rewrites `model` and injects session or `include_usage` fields while keeping all other bytes identical.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 12.3`, `Section 7.3`; Requirements `API-4`, `TRN-1`, `TRN-2` |
| Crates touched | `crates/proto::body_edit` |
| TDD step (`Red -> Green`) | Write property-based tests (`proto::body_edit::proptests`, `#[doc = "Spec: API-4, TRN-1, TRN-2"]`) generating arbitrary nested JSON requests with weird whitespace, escaped Unicode, and specific key orders in `tools` and `messages`. Assert that: (1) when no edit is needed, the output `Bytes` pointer is identical to the input `Bytes` (`TRN-1`), and (2) when `model` or `include_usage` is edited, slicing out the `messages`, `tools`, and `system` byte ranges from the output yields exact byte equality with the input (`TRN-2`). |
| Atomic commits | 1. `feat(proto/2.4): implement zero-copy top-level JSON span scanner and surgical editor [Spec: API-4, TRN-1, TRN-2]` |
| Risks and resolutions | **Risk:** Full DOM deserialization (`serde_json::Value`) would both permute object keys (breaking upstream prefix caches) and allocate tens of megabytes on 32 MiB prompts. **Resolution:** Use a custom streaming lexer or `serde_json::value::RawValue` top-level slice so nested arrays and objects are never parsed into DOM nodes on the forwarding path. |

### 2.5 Sub-phase 2.5: Unbuffered SSE streaming tee, `include_usage` suppression, and disconnect abort

This sub-phase implements the zero-buffering chunk forwarder (`PERF-2`, `API-6`) and suppresses injected empty-choices usage chunks (`API-5`). It also handles compressed streams and aborts upstream calls within 1 s on client disconnect (`API-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 12.4`, `Section 7.1`, `Section 7.3`; Requirements `API-5`, `API-6`, `API-7`, `PERF-2`, `TRN-3` |
| Crates touched | `crates/proxy::forward`, `crates/proto::sse_frame` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/integration_wire_transparency.rs`, `#[doc = "Spec: API-5, API-6, API-7, PERF-2, TRN-3"]`) using a local stub server that emits 5 SSE chunks spaced 50 ms apart: (1) assert each chunk arrives at the client within 1 ms of emission (`PERF-2`), (2) assert that an injected `include_usage` chunk (`"choices":[]`) is captured by the tap and omitted from the client stream when the client did not ask for it (`API-5`), and (3) drop the client TCP connection after chunk 2 and assert the stub server observes socket closure within 100 ms (`API-7`). |
| Atomic commits | 1. `feat(proxy/2.5): implement unbuffered SSE stream forwarder and injected usage chunk filter [Spec: API-5, API-6, PERF-2, TRN-3]` 2. `feat(proxy/2.5): wire client disconnect drop guard to abort upstream request within 1s [Spec: API-7]` |
| Risks and resolutions | **Risk:** An SSE frame (`data: ...\n\n`) splits across two TCP `Bytes` chunks right in the middle of the final usage event or mid-UTF-8 codepoint. **Resolution:** Follow the two-state machine in `SRDD Section 12.4.1`: when `strip_injected_usage_chunk == false`, forward raw `Bytes` immediately without waiting for `\n\n` while feeding the 64 KiB line-reassembling ring buffer (with 1-to-3 byte UTF-8 carryover) inside `SseUsageTap`. When `strip_injected_usage_chunk == true`, enter `PostFinishReasonHoldback` only after `finish_reason` has already been flushed to the client. |

### 2.6 Sub-phase 2.6: Built-in `ProviderAdapter` implementations and preset TOML files

This sub-phase implements the `ProviderAdapter` trait (`MNT-2`) and ships the four built-in provider presets (`opencode_go.toml`, `command_code.toml`, `open_design_amr.toml`, `openrouter.toml`) plus the `custom` adapter (`SRDD Section 2.3, Section 11.2, Section 15.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.3`, `Section 6.3`, `Section 7.4`, `Section 11.2`; Requirements `PRV-2`, `PRV-3`, `PRV-6`, `API-1`, `API-8`, `MNT-2` |
| Crates touched | `crates/proto::adapters`, `presets/*.toml` |
| TDD step (`Red -> Green`) | Write adapter contract tests (`proto::adapters::tests`, `#[doc = "Spec: PRV-2, PRV-3, PRV-6, API-8, MNT-2"]`) verifying URL construction (including `POST /v1/messages/count_tokens`), auth header injection (`Bearer` vs. `x-api-key`), outbound session hint injection (`x-opencode-session`, `x-session-id`, `x-session-affinity`, `prompt_cache_key`), Stage-1 key testing against `/models` (`PRV-6`, with `/usage` probe added in Sub-Phase 4.5), and upstream error classification (401/403 auth vs. 403 `upgrade_required` on Command Code vs. 429 rate limit vs. 400 client error). |
| Atomic commits | 1. `feat(proto/2.6): implement ProviderAdapter trait and built-in presets for OpenCode Go, Command Code, Open Design AMR, and OpenRouter [Spec: PRV-2, PRV-3, PRV-6, API-8, MNT-2]` |
| Risks and resolutions | **Risk:** OpenCode Go changes which models sit behind `/chat/completions` vs. `/messages` (`SRDD Section 2.3`). **Resolution:** Store supported protocols per route in `model_routes.protocols` discovered from `/models` or configured in the preset, never hard-coded in Rust branches. |

## Epic 3: Model catalog, personal sharing, smooth WRR routing, and session pinning

Epic 3 builds the model catalog, personal provider sharing (`provider_shares`), the nine-step session resolver, and the eight-check eligibility filter. It also implements smooth weighted round-robin (`smooth_wrr`), single-flight session pinning, and pre-first-byte failover. It corresponds to Milestone `M2` (`SRDD Sections 4.3–4.6, 6.4, 6.6, 6.7, 10, 11`).

### 3.1 Sub-phase 3.1: `/models` discovery, canonical ID normalization, and exposure rules

This sub-phase implements `/models` polling (`MDL-1`), canonical normalization (`MDL-2`), global auto-intersection (`MDL-3`), and admin force-exposure (`MDL-4`). It also implements personal model exposure (`MDL-5`), aliases (`MDL-6`), and `GET /v1/models` (`MDL-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.6`, `Section 10.6`, `Section 14.2.2`, `Section 19.1 (ADR-7)`; Requirements `MDL-1`..`MDL-7` |
| Crates touched | `crates/core::catalog`, `crates/proto::discovery`, `crates/store::models`, `crates/proxy::models` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`core::catalog::tests::normalization_and_intersection`, `#[doc = "Spec: MDL-1, MDL-2, MDL-3, MDL-4, MDL-5, MDL-6, MDL-7"]`) asserting: (1) `deepseek/deepseek-v4.1-flash` and `deepseek-v4.1-flash` normalize to `deepseek-v4.1-flash`, while `deepseek-v4-flash` stays a distinct canonical model (`ADR-7`), (2) a model with `exposed = NULL` is exposed in the global pool if and only if every enabled global provider with at least one enabled key serves it (`MDL-3`), (3) a standard user with a personal key serving `claude-sonnet-4-6` sees that model on `GET /v1/models` even if global providers lack it (`MDL-5`), and (4) `GET /v1/models` with `anthropic-version` returns the Anthropic list schema (`MDL-7`). |
| Atomic commits | 1. `feat(core/3.1): implement 6-step canonical model normalization, model merge, alias CRUD, and exposure rules [Spec: MDL-2, MDL-3, MDL-4, MDL-5, MDL-6]` 2. `feat(server/3.1): implement scheduled /models discovery job and GET /v1/models handler [Spec: MDL-1, MDL-7]` |
| Risks and resolutions | **Risk:** One provider's `/models` endpoint is temporarily unreachable during startup or periodic refresh, which could shrink the global intersection set if treated as an empty catalog. **Resolution:** Keep the last successful `model_routes` rows for a key when a `/models` refresh fails with a network or 5xx error, only removing routes when `/models` returns HTTP 200 without that model or when the key is disabled. |

### 3.2 Sub-phase 3.2: Personal provider and key sharing (`provider_shares`)

This sub-phase implements user-to-user sharing grants on `personal` providers and keys, optional `allowed_models` filters, monthly micro-USD caps (`max_monthly_micro_usd`), non-transitive share enforcement, and instant revocation (`SRDD Section 4.4, Section 6.4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.4`, `Section 4.6`, `Section 6.4`, `Section 8.2`; Requirements `SHR-1`..`SHR-6`, `SEC-17` |
| Crates touched | `crates/core::shares`, `crates/store::shares` |
| TDD step (`Red -> Green`) | Write unit and SQLite integration tests (`tests/integration_personal_sharing.rs`, `#[doc = "Spec: SHR-1, SHR-2, SHR-3, SHR-4, SHR-5, SHR-6, SEC-17"]`) verifying: (1) Alice (`user`) can share her personal provider or key with Bob (`user`) with `allowed_models = ["deepseek-v4.1-flash"]` and `max_monthly_micro_usd = 5_000_000`, (2) Bob cannot re-share Alice's key with Carol (`SHR-4`), (3) Bob cannot read Alice's secret or `secret_last4`, and (4) when Bob's monthly spend on the share reaches `$5.00` (minus reserve) or Alice revokes the share, the route becomes immediately ineligible for Bob (`SHR-5`). |
| Atomic commits | 1. `feat(core/3.2): implement personal share grant validation, model filtering, and monthly cap checks [Spec: SHR-1, SHR-2, SHR-3, SHR-4]` 2. `feat(store/3.2): implement provider_shares repository and per-grantee usage aggregation [Spec: SHR-5, SHR-6, SEC-17]` |
| Risks and resolutions | **Risk:** Two concurrent requests from Grantee Bob slightly overshoot `max_monthly_micro_usd`. **Resolution:** Apply the same configurable admission reserve (`default 2%`) to share caps that `LIM-1` applies to upstream key budget windows. |

### 3.3 Sub-phase 3.3: Nine-step inbound session ID resolver and tenant-salted affinity hints

This sub-phase implements the nine-step session ID precedence (`SRDD Section 11.1`) and 128-bit `HMAC-SHA256` privacy hashing. It also adds HyperLogLog constant-session detection (`Section 11.1.1`) and tenant-salted outbound affinity hints (`SEC-16`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 11.1`, `Section 11.2`, `Section 6.7`, `Section 8.2`; Requirements `RTE-1`, `SEC-16` |
| Crates touched | `crates/core::session`, `crates/proto::affinity` |
| TDD step (`Red -> Green`) | Write table-driven tests (`core::session::tests`, `#[doc = "Spec: RTE-1, SEC-16"]`) testing all nine precedence levels, verifying that `x-claude-code-agent-id` does not split a parent `x-claude-code-session-id` pin, that values `> 256` bytes or containing control characters are rejected/hashed safely, that two different users sending the same `session_id = "abc"` on a `global` or `shared` key emit distinct outbound affinity tokens (`SEC-16`), and that 21 distinct opening prompts on one `session_hash` set `suspected_constant_session_id = true`. |
| Atomic commits | 1. `feat(core/3.3): implement nine-step session ID resolver, HMAC hashing, and constant-session detector [Spec: RTE-1]` 2. `sec(proto/3.3): implement tenant-salted outbound affinity hint generator [Spec: SEC-16]` |
| Risks and resolutions | **Risk:** Stateless SDK callers send no session header, and their system prompt changes slightly on turn 2, breaking the Step 9 fallback hash. **Resolution:** Hash only the first 4 KiB of the static system prompt plus the **first** user message (`messages[0..=1]`), which remains constant across multi-turn stateless history arrays. |

### 3.4 Sub-phase 3.4: Eight-check candidate eligibility engine and `routing_scope` pool selector

This sub-phase implements the eight-check candidate eligibility filter (`SRDD Section 10.1`) and the four `routing_scope` pool combinators (`personal_first`, `blended`, `personal_only`, `global_only` in `SRDD Section 10.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.5`, `Section 6.5`, `Section 6.7`, `Section 10.1`, `Section 10.2`; Requirements `CLK-3`, `RTE-3` |
| Crates touched | `crates/core::eligibility` |
| TDD step (`Red -> Green`) | Write exhaustive unit tests (`core::eligibility::tests`, `#[doc = "Spec: CLK-3, RTE-3"]`) toggling each of the eight eligibility conditions one by one (client key model allow-list, user ownership/share/global quota authorization, model exposure, enabled flags, protocol support, key health/cooldown expiry, circuit breaker state, and window headroom) and testing all four `routing_scope` modes. Run `cargo mutants` on `crates/core::eligibility` to confirm every boolean condition in the 8-check filter is covered by a dedicated negative test. |
| Atomic commits | 1. `feat(core/3.4): implement eight-check route eligibility filter and routing_scope pool selector [Spec: CLK-3, RTE-3]` |
| Risks and resolutions | **Risk:** A user sets `routing_scope = 'personal_first'`, their personal key serves `deepseek-v4.1-flash`, and they request `claude-sonnet-4-6` which only a global key serves. **Resolution:** Because `Pool_Personal_And_Shared` is filtered per requested model `M` first, it is empty for `claude-sonnet-4-6`, so `personal_first` cleanly falls back to `Pool_Global` for that model while keeping `deepseek-v4.1-flash` on the personal key. |

### 3.5 Sub-phase 3.5: Smooth weighted round-robin (`smooth_wrr`), per-model overrides, and headroom scaling

This sub-phase implements nginx's smooth weighted round-robin algorithm (`SRDD Section 10.3`) and three-level effective weight resolution. It also supports `strict` tier filtering and `headroom` budget scaling (`RTE-4`..`RTE-6`, `RTE-13`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.7`, `Section 10.3`; Requirements `RTE-4`, `RTE-5`, `RTE-6`, `RTE-13` |
| Crates touched | `crates/core::wrr` |
| TDD step (`Red -> Green`) | Write both the worked-example unit test from `SRDD Section 10.3.1` (`deepseek-v4.1-flash` at `100:60:30` and `mimo-v2.6-pro` at `60:20:15`) and the `proptest` convergence and burstiness suite (`core::wrr::proptests`, `#[doc = "Spec: RTE-4, RTE-5, RTE-6, RTE-13"]`). Verify that over 100,000 picks across random weight vectors, empirical shares stay within `0.5%` of exact weights and consecutive runs never exceed `ceil(max(W) / min(W))`. |
| Atomic commits | 1. `feat(core/3.5): implement smooth weighted round-robin, per-model priority overrides, strict tiers, and headroom scaling [Spec: RTE-4, RTE-5, RTE-6, RTE-13]` |
| Risks and resolutions | **Risk:** When a route temporarily leaves the candidate set (e.g., enters `cooling_down`) and later returns, stale `current_weight` counters could cause a sudden burst. **Resolution:** Reset a route's `current_weight` to `0` whenever the active candidate set membership changes for that `(pool_scope, canonical_model_id)`, preserving the invariant `sum(current_weight) == 0`. |

### 3.6 Sub-phase 3.6: Single-flight session pinning, `previous_response_id` links, and pre-first-byte failover

This sub-phase implements `moka` single-flight provisional pinning (`RTE-7`), first-byte confirmation (`RTE-8`), sliding TTL persistence (`RTE-10`), and repin tracking (`RTE-11`). It also implements `previous_response_id` affinity (`RTE-12`) and pre-first-byte failover (`RTE-9`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.7`, `Section 10.4`, `Section 10.5`; Requirements `RTE-2`, `RTE-7`..`RTE-12`, `API-8` |
| Crates touched | `crates/core::pins`, `crates/core::router`, `crates/store::pins`, `crates/proxy::failover` |
| TDD step (`Red -> Green`) | Write the 100-way concurrency race test (`tests/integration_routing_and_failover.rs`, `#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-9, RTE-10, RTE-11, RTE-12"]`): (1) 100 parallel first-turn requests for one new session ID select the exact same `upstream_key_id`, (2) upstream 503/429/401 before the first byte retries up to 2 alternative candidates, updates key health (`cooling_down`, `exhausted`, `invalid`), and confirms the pin on the candidate that yields the first byte, (3) `429` with `Retry-After: 1` sleeps 1 s on `FakeClock` and retries the **same** key once to preserve cache, and (4) stream failure after 1 byte never retries. |
| Atomic commits | 1. `feat(core/3.6): implement single-flight pin cache, routing_scope pin re-validation, and response_links [Spec: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, RTE-12]` 2. `feat(proxy/3.6): implement pre-first-byte failover, key/route health transitions, provider circuit breaker, and GET/DELETE/compact /v1/responses/{id} [Spec: RTE-9, RTE-12, API-8, REL-5]` |
| Risks and resolutions | **Risk:** If the winning task inside `moka::try_get_with` panics or is cancelled by client disconnect before upstream responds, waiting tasks could hang. **Resolution:** Keep `try_get_with` strictly synchronous in `crates/core` (inserting the `provisional` pin in `< 10 µs`), and if the first upstream call fails before the first byte, atomically CAS-update the provisional pin to the failover candidate. |

## Epic 4: Usage pipeline, price book, budget windows, and source `/usage` engine

Epic 4 builds the SSE and JSON token extractors, the integer micro-USD price book, and the batching SQLite writer. It also implements budget window ring counters and the source provider `/usage` poller, reconciler, and pass-through endpoints. It corresponds to Milestones `M3` and `M4` (`SRDD Sections 2.4, 2.5, 6.9, 7.2, 9, 12.5, 13`).

### 4.1 Sub-phase 4.1: `SseUsageTap`, `JsonUsageExtractor`, and cache token normalization

This sub-phase extracts token counts across `chat`, `responses`, and `messages` (`SRDD Section 12.5`), normalizes OpenAI vs. Anthropic cache token accounting (`USG-2`), and detects truncated streams (`USG-1`, `R7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.5`, `Section 6.9`, `Section 12.5`; Requirements `USG-1`, `USG-2` |
| Crates touched | `crates/proto::sse_tap`, `crates/proto::json_usage`, `fixtures/sse/*` |
| TDD step (`Red -> Green`) | Write golden fixture tests (`proto::sse_tap::golden_tests`, `#[doc = "Spec: USG-1, USG-2"]`) replaying recorded streams for OpenAI `chat`, OpenAI `responses`, Anthropic `messages`, OpenRouter `usage.cost`, and mid-stream drops missing `[DONE]`, `response.completed`, or `message_stop`. Assert that `input_uncached = prompt_tokens - cached_tokens` on `chat` and `responses`, while `input_uncached = input_tokens` on `messages`, and that truncated streams set `incomplete = 1`. |
| Atomic commits | 1. `feat(proto/4.1): implement SseUsageTap and JsonUsageExtractor with cache normalization and truncation detection [Spec: USG-1, USG-2]` |
| Risks and resolutions | **Risk:** A provider emits usage across two separate SSE events (`message_start` and `message_delta` in Anthropic `messages`) and aborts before `message_stop`. **Resolution:** Accumulate `message_start` input/cache counts immediately and update `output_tokens` on each `message_delta`; if `message_stop` never arrives, emit the partial usage with `incomplete = 1`. |

### 4.2 Sub-phase 4.2: Integer micro-USD price book, context tiers, and UTC peak schedules

This sub-phase implements 128-bit integer micro-USD cost calculation (`SRDD Section 13.1`), 200K+ context-length tier rates, and weekday UTC peak-hour multipliers (`USG-3`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.3`, `Section 6.9`, `Section 13.1`; Requirements `USG-3` |
| Crates touched | `crates/core::pricing`, `presets/*.toml` |
| TDD step (`Red -> Green`) | Write property-based and table-driven tests (`core::pricing::proptests`, `#[doc = "Spec: USG-3"]`) verifying: (1) provider-reported `usage.cost` wins (`cost_source = 'provider'`), (2) price-book math uses `i128` ceiling division with zero overflow up to `10^9` tokens, (3) prompts `> 200,000` tokens select the high-context tier rate, and (4) DeepSeek calls at `02:30 UTC` on Tuesday apply the 2x peak multiplier while `02:30 UTC` on Saturday uses the off-peak rate. |
| Atomic commits | 1. `feat(core/4.2): implement i128 micro-USD pricing engine with context tiers and UTC schedules [Spec: USG-3]` 2. `feat(admin/4.2): implement opt-in models.dev price catalog importer and POST /admin/api/prices/sync [Spec: USG-3, PRIV-1]` |
| Risks and resolutions | **Risk:** A newly discovered model has no entry in `prices`. **Resolution:** Record `cost_micro_usd = NULL` and `cost_source = 'unpriced'` (never `$0`), surface an "Unpriced models" badge on the dashboard, and support one-click baseline import from `models.dev`. |

### 4.3 Sub-phase 4.3: Batching SQLite usage writer, bounded overflow queue, and hourly rollups

This sub-phase implements the non-blocking `mpsc` usage queue (10,000 slots) and the batching SQLite writer (`SRDD Section 7.2, Section 9.2`). Each transaction commits up to 200 rows every 250 ms and updates `usage_rollups_hourly`.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.2`, `Section 9.1`, `Section 9.2`; Requirements `USG-1`, `REL-2`, `REL-3` |
| Crates touched | `crates/store::usage_writer`, `crates/store::rollups` |
| TDD step (`Red -> Green`) | Write fault-injection and crash tests (`tests/integration_usage_writer.rs`, `#[doc = "Spec: USG-1, REL-2, REL-3"]`): (1) pause the SQLite writer task and send 10,500 usage events; assert that inference requests complete in `< 1 ms`, the oldest 500 events drop cleanly, and `llmroller_usage_dropped_total == 500` (`REL-3`), and (2) verify that every committed batch updates `usage_rollups_hourly` in the same SQLite transaction. |
| Atomic commits | 1. `feat(store/4.3): implement ordered batching SQLite usage writer, share-aware hourly rollups, and non-blocking overflow ring [Spec: USG-1, SHR-6, REL-2, REL-3]` |
| Risks and resolutions | **Risk:** On `SIGTERM`, buffered events in the `mpsc` channel could be lost if the process exits immediately. **Resolution:** During the `SIGTERM` drain phase (`REL-1`), close the inbound listener first, wait for active streams to finish, and flush all remaining items in the usage channel before closing the SQLite pool. |

### 4.4 Sub-phase 4.4: Rolling (`5h`, `7d`) and anchored (`1mo`) budget window ring counters

This sub-phase implements the in-memory rolling ring counters and anchored monthly accumulators (`SRDD Section 13.2`). It rebuilds window state from `usage_events` and `budget_adjustments` at startup and enforces the 2% admission reserve.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.9`, `Section 13.2`; Requirements `LIM-1`, `RTE-3` |
| Crates touched | `crates/core::windows`, `crates/store::budgets` |
| TDD step (`Red -> Green`) | Write deterministic clock-driven tests (`core::windows::tests`, `#[doc = "Spec: LIM-1, RTE-3"]`) using `FakeClock`: advance time minute-by-minute across a 5-hour rolling window and a 30-day anchored window, asserting that buckets expire at the exact millisecond boundary, that routes are excluded at `98%` spend (`reserve_pct = 2`), and that restarting the store reconstructs identical bucket sums from SQLite. |
| Atomic commits | 1. `feat(core/4.4): implement rolling bucket ring counters, anchored windows, RAII InFlightBudgetGuard, and admission reserve checks [Spec: LIM-1, RTE-3]` |
| Risks and resolutions | **Risk:** High concurrency allows several new requests to pass admission simultaneously when a window is at `97.9%`. **Resolution:** Increment an optimistic in-flight reservation counter (`reserved_in_flight_micro_usd`, default `$0.05` per active stream) while the stream is open and replace it with actual cost when the stream finishes. |

### 4.5 Sub-phase 4.5: Source provider `/usage` poller, auto-reconciliation, and `/v1/usage` pass-through

This sub-phase implements upstream `/usage` polling and automatic `budget_adjustments` reconciliation (`USG-4`..`USG-6`). It also serves `/v1/usage`, `/v1/key`, and `/v1/usage/raw` with PII sanitization (`USG-7`, `USG-8`, `SEC-18`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.4`, `Section 6.9`, `Section 8.2`, `Section 13.3`, `Section 13.4`; Requirements `USG-4`..`USG-8`, `SEC-18` |
| Crates touched | `crates/proto::usage_poller`, `crates/store::usage_snapshots`, `crates/proxy::usage_passthrough`, `fixtures/usage/*` |
| TDD step (`Red -> Green`) | Write golden parser and integration tests (`tests/integration_usage_and_passthrough.rs`, `#[doc = "Spec: USG-4, USG-5, USG-6, USG-7, USG-8, SEC-18"]`): (1) replay `fixtures/usage/opencode_go.json`, `command_code_credits.json`, and `openrouter_key.json`, (2) assert that when OpenCode Go reports `percent: 25` and `resetsAt` while local spend is `10%`, LLMRoller inserts an automatic `upstream_usage_sync` adjustment so the window gauge reads `25%` (`USG-6`), (3) assert `GET /v1/usage` returns raw `source_provider_usage` for a user's own personal key, and (4) assert `GET /v1/usage` on a shared or global key strips `email`, `whoami`, `user_id`, and `workspace_id` (`USG-8`, `SEC-18`). |
| Atomic commits | 1. `feat(proto/4.5): implement source provider /usage parsers and strict allow-list PII sanitizer [Spec: PRV-6, USG-4, USG-5, SEC-18]` 2. `feat(core/4.5): implement automatic window reset and spend reconciliation from /usage snapshots [Spec: USG-6]` 3. `feat(proxy/4.5): implement GET /v1/usage, GET /v1/key, and GET /v1/usage/raw pass-through endpoints [Spec: USG-7, USG-8]` |
| Risks and resolutions | **Risk:** Command Code's `/alpha/billing/credits` endpoint changes a field name or Open Design AMR's `/v1/usage` returns 404 ([open-design#8674](https://github.com/nexu-io/open-design/issues/8674)). **Resolution:** Keep `raw_json` intact in `upstream_usage_snapshots`, treat missing normalized fields as `None` (falling back cleanly to local window counters without failing key health), and surface a non-fatal schema notice in the admin UI (`R2`). |

## Epic 5: Opt-in content traceability mode and read-only session chat preview

Epic 5 builds the admin opt-in content traceability mode (`TRC-1`..`TRC-8`) and non-blocking SSE delta reassembly. It also implements `zstd` + `XChaCha20-Poly1305` trace storage and the multi-turn Session Chat Preview reconstructor (`SRDD Section 6.10, Section 12.6`).

### 5.1 Sub-phase 5.1: Admin opt-in `content_traceability_mode` configuration and scope matching

This sub-phase implements the default-off `content_traceability_mode` setting, scope filters, and the 14-day retention TTL (`TRC-1`, `TRC-7`). Every change to traceability settings writes a hash-chained `audit_log` entry (`AUD-1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 6.10`, `Section 6.11`; Requirements `TRC-1`, `TRC-7`, `AUD-1` |
| Crates touched | `crates/core::trace_config`, `crates/store::settings`, `crates/admin::trace_settings` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`core::trace_config::tests`, `#[doc = "Spec: TRC-1, TRC-7, AUD-1"]`) verifying: (1) fresh instances have `content_traceability_mode = Disabled`, (2) standard `user` callers receive HTTP 403 on `PUT /admin/api/settings/traceability`, (3) scoping traceability to `users: [alice_id]` records traces for Alice's client keys while recording zero trace rows for Bob's client keys, and (4) toggling the setting appends a hash-chained `audit_log` row. |
| Atomic commits | 1. `feat(core/5.1): implement default-off TraceabilityConfig and scope matcher in ConfigSnapshot [Spec: TRC-1, TRC-7]` 2. `feat(admin/5.1): implement PUT /admin/api/settings/traceability with audit logging [Spec: TRC-1, AUD-1]` |
| Risks and resolutions | **Risk:** An admin enables content traceability for debugging and forgets to turn it off. **Resolution:** Support an optional `auto_disable_after_hours` timer in `TraceabilityConfig` and display a persistent top banner in the web UI whenever content traceability mode is active (`UI-4`). |

### 5.2 Sub-phase 5.2: Non-blocking request capture and SSE delta reassembler (`TraceTap`)

This sub-phase extends `crates/proto::trace_tap` to capture inbound prompts and tools (`TRC-2`). It reassembles streamed text, reasoning, and tool-call deltas across `chat`, `responses`, and `messages` into a `NormalizedTurnTrace` without slowing chunk forwarding (`TRC-3`, `PERF-5`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.10`, `Section 7.1`, `Section 12.6`; Requirements `TRC-2`, `TRC-3`, `PERF-2`, `PERF-5` |
| Crates touched | `crates/proto::trace_tap` |
| TDD step (`Red -> Green`) | Write golden SSE reassembly tests (`proto::trace_tap::tests`, `#[doc = "Spec: TRC-2, TRC-3, PERF-5"]`) replaying multi-chunk streams across `chat`, `responses`, and `messages` that interleave extended thinking/reasoning blocks, parallel tool calls with fragmented JSON arguments, and markdown text. Assert that `TraceTap::finish()` produces the exact reconstructed `reasoning_text`, `ToolCall` structs with valid JSON arguments, and `assistant_text`, and truncates turns exceeding `4 MiB` with `truncated_content = 1` (`PERF-5`). |
| Atomic commits | 1. `feat(proto/5.2): implement non-blocking TraceTap and SSE delta reassembler for chat, responses, and messages [Spec: TRC-2, TRC-3, PERF-5]` |
| Risks and resolutions | **Risk:** A streaming tool call emits malformed partial JSON when the client disconnects mid-stream. **Resolution:** Store tool-call argument fragments in a `String` buffer during streaming and parse JSON only at block completion; if the stream aborts early, preserve the raw partial argument string with `incomplete = 1` so the trace preview still shows what arrived before disconnect. |

### 5.3 Sub-phase 5.3: Background `zstd` compression, `XChaCha20-Poly1305` encryption, and TTL purge

This sub-phase implements background `zstd` compression and `XChaCha20-Poly1305` encryption for `trace_records` (`TRC-4`, `SEC-19`). It also adds on-demand trace purge endpoints and the 14-day retention sweeper (`TRC-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.10`, `Section 8.2`, `Section 9.1`, `Section 9.2`; Requirements `TRC-4`, `TRC-7`, `PERF-5`, `SEC-19` |
| Crates touched | `crates/store::traces`, `crates/server::jobs` |
| TDD step (`Red -> Green`) | Write cryptographic and storage tests (`store::traces::tests`, `#[doc = "Spec: TRC-4, TRC-7, SEC-19"]`) verifying: (1) raw SQLite inspection of `trace_records.payload_enc` contains zero plaintext substrings of the prompt or completion, (2) tampering with `owner_user_id` or `session_hash` on a `trace_records` row causes AEAD decryption to fail (`SEC-19`), and (3) `DELETE /admin/api/sessions/{session_hash}/trace` deletes all `trace_records` for that session while leaving `usage_events` token and cost rows intact (`TRC-7`). |
| Atomic commits | 1. `sec(store/5.3): implement zstd compression and XChaCha20-Poly1305 encryption for trace_records [Spec: TRC-4, PERF-5, SEC-19]` 2. `feat(store/5.3): implement trace retention sweeper and selective session trace purge [Spec: TRC-7]` |
| Risks and resolutions | **Risk 1:** `zstd` compression on large prompts competes with Tokio HTTP worker threads. **Resolution 1:** Run `zstd::encode_all` (level 3) and `XChaCha20-Poly1305` sealing on the background `spawn_blocking` pool (2 threads max, `PERF-5`). **Risk 2:** `trace_records.usage_event_id` references `usage_events(id)` under `PRAGMA foreign_keys = ON`, so inserting a trace before its parent usage event commits would fail with `SQLITE_CONSTRAINT_FOREIGNKEY`. **Resolution 2:** Send the sealed `TraceRecord` into the single SQLite batch writer (`SRDD Section 9.2`), which commits `(1) usage_events -> (2) usage_rollups_hourly -> (3) trace_records` in one atomic transaction. |

### 5.4 Sub-phase 5.4: Multi-turn session chat timeline reconstructor and privacy-isolated preview API

This sub-phase implements `crates/core::chat_preview` (`SRDD Section 12.6`) to reconstruct multi-turn conversations per `session_hash`. It deduplicates stateless message prefixes, marks context compaction, sanitizes HTML via `ammonia`, and serves the read-only chat preview APIs (`TRC-5`, `TRC-8`, `SEC-19`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 4.6`, `Section 6.10`, `Section 8.2`, `Section 12.6`, `Section 14.2`; Requirements `TRC-5`, `TRC-6`, `TRC-8`, `SEC-17`, `SEC-19` |
| Crates touched | `crates/core::chat_preview`, `crates/admin::traces` |
| TDD step (`Red -> Green`) | Write unit, privacy, and XSS security tests (`tests/integration_content_traceability.rs`, `#[doc = "Spec: TRC-5, TRC-6, TRC-8, SEC-17, SEC-19"]`): (1) feed a 5-turn stateless coding session where each request resends prior messages plus new tool results; assert the reconstructed timeline emits each user prompt, reasoning block, tool call, tool result, and assistant message **once** in chronological order, (2) simulate mid-session context compaction on turn 4 and assert a `context_compacted_or_branched` marker appears, (3) assert that when Grantee Bob routes through Grantor Alice's shared personal key, Alice receives HTTP 403/404 when querying Bob's `session_hash` chat preview (`TRC-8`), and (4) inject `<script>alert(1)</script><img src=x onerror=alert(1)>` into a recorded prompt and completion and assert the rendered HTML preview strips all active scripts and event handlers (`SEC-19`). |
| Atomic commits | 1. `feat(core/5.4): implement multi-turn session chat timeline reconstructor with prefix deduplication and compaction detection [Spec: TRC-5]` 2. `sec(admin/5.4): implement RBAC-isolated session chat preview endpoints with ammonia XSS sanitization [Spec: TRC-6, TRC-8, SEC-17, SEC-19]` |
| Risks and resolutions | **Risk:** A long coding session has 150 turns, making a single chat preview payload large. **Resolution:** Support cursor pagination (`?from_turn=1&limit=50`) on `/sessions/{session_hash}/chat` and lazy-load raw request/response JSON per turn via `GET /api/user/traces/{request_id}`. |

## Epic 6: Dual-workspace HTMX web UI and control plane APIs

Epic 6 builds the embedded web interface (`askama`, `htmx`, `uPlot`, `rust-embed`) and JSON control-plane APIs. It covers the Personal Workspace (`/app/*`), the Admin Console (`/admin/*`), and the read-only Session Chat Preview UI. It corresponds to Milestone `M5` (`SRDD Sections 4, 6.10, 6.11, 8.2, 14`).

### 6.1 Sub-phase 6.1: Askama + HTMX shell, embedded assets, CSP headers, and role-aware navigation

This sub-phase sets up the Askama templates and embeds vendored `htmx 2.x` and `uPlot` assets via `rust-embed` (`UI-3`). It also enforces `Content-Security-Policy`, `x-csrf-token` checks (`SEC-5`), and role-aware navigation (`UI-1`, `UI-4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.11`, `Section 8.2`, `Section 14.2`, `Section 14.3`; Requirements `UI-1`, `UI-3`, `UI-4`, `SEC-5`, `SEC-9` |
| Crates touched | `crates/admin::ui`, `crates/admin::assets`, `crates/admin::csrf` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/security_csrf_and_headers.rs`, `#[doc = "Spec: UI-1, UI-3, UI-4, SEC-5, SEC-9"]`) asserting: (1) every HTML response includes `Content-Security-Policy: default-src 'self'...`, `X-Frame-Options: DENY`, and `X-Content-Type-Options: nosniff`, (2) zero HTML templates reference external `http://` or `https://` script/style/font URLs (`UI-3`), (3) any `POST`/`PUT`/`PATCH`/`DELETE` without a valid `x-csrf-token` returns HTTP 403 (`SEC-5`), and (4) enabling `content_traceability_mode` renders the global traceability status banner (`UI-4`). |
| Atomic commits | 1. `feat(admin/6.1): embed vendored HTMX and uPlot assets and enforce CSP and CSRF middleware [Spec: UI-3, SEC-5, SEC-9]` 2. `feat(admin/6.1): implement role-aware layout shell and content traceability status banner [Spec: UI-1, UI-4]` |
| Risks and resolutions | **Risk:** Inline `onclick` handlers or inline `<script>` blocks break `script-src 'self'`. **Resolution:** Use standard declarative `hx-get`, `hx-post`, `hx-target`, and `hx-swap` attributes plus a single external `/static/app.js` file served from `rust-embed`. |

### 6.2 Sub-phase 6.2: Standard user Personal Workspace (`/app/*` and `/api/user/*`)

This sub-phase implements the self-service `/api/user/*` endpoints and `/app/*` screens for standard `user` accounts (`SRDD Section 14.2.1`). It covers Personal Providers & Keys, Outgoing and Incoming Shares, the Personal Routing Matrix, and Client Keys.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 6.2`, `Section 6.3`, `Section 6.4`, `Section 6.5`, `Section 14.2.1`, `Section 14.3`; Requirements `USR-2`..`USR-4`, `PRV-1`..`PRV-6`, `SHR-1`..`SHR-6`, `CLK-1`..`CLK-5`, `UI-1`, `UI-2` |
| Crates touched | `crates/admin::user_api`, `crates/admin::templates::app` |
| TDD step (`Red -> Green`) | Write RBAC and UI integration tests (`tests/integration_rbac_isolation.rs`, `#[doc = "Spec: USR-2, USR-3, USR-4, UI-1, UI-2"]`) testing every `/api/user/*` endpoint as Standard User Alice and Standard User Bob: (1) Alice can create a personal provider, add a key, test `/models` and `/usage`, set `user_route_preferences` priority overrides, share the key with Bob, and generate an `llr_...` client key, (2) Bob sees the shared key in `/api/user/shares/incoming` with health and remaining cap but never sees `secret_last4` or Alice's raw `/usage` JSON (`USR-4`, `SEC-18`), and (3) cross-tenant IDOR requests (`PATCH /api/user/upstream-keys/{alice_key_id}` by Bob) return HTTP 404. |
| Atomic commits | 1. `feat(admin/6.2): implement /api/user/* endpoints and HTMX views for personal providers, keys, and /usage modal [Spec: USR-2, USR-3, USR-4, PRV-1, PRV-6, UI-2]` 2. `feat(admin/6.2): implement /api/user/shares, personal routing matrix, and client key screens [Spec: SHR-1, SHR-6, CLK-1, CLK-3, UI-1]` |
| Risks and resolutions | **Risk:** A user loses their newly generated `llr_...` client key if they navigate away accidentally. **Resolution:** Render the one-time token inside a modal that stays open until the user clicks "I have copied this key" (`SRDD Section 14.3`). |

### 6.3 Sub-phase 6.3: Admin Console (`/admin/*` and `/admin/api/*`)

This sub-phase implements the `/admin/api/*` endpoints and `/admin/*` screens for instance administration (`SRDD Section 14.2.2`). It covers Global Providers, the Model Catalog, the Price Book, Users, Content Traceability settings, and the Audit Log.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.1`..`6.3`, `Section 6.6`, `Section 6.10`, `Section 6.11`, `Section 14.2.2`, `Section 14.3`; Requirements `AUTH-4`, `USR-5`, `MDL-3`, `MDL-4`, `TRC-1`, `UI-1`, `AUD-1` |
| Crates touched | `crates/admin::admin_api`, `crates/admin::templates::admin` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/e2e_dashboard_htmx.rs`, `#[doc = "Spec: AUTH-4, USR-5, MDL-3, MDL-4, TRC-1, UI-1, AUD-1"]`) verifying that every `/admin/*` and `/admin/api/*` route returns HTTP 403 for `Role::User`, and that `Role::Admin` can manage global providers, force-expose models, update price-book tiers/schedules, set per-user global budgets, toggle content traceability mode, and filter the audit log. |
| Atomic commits | 1. `feat(admin/6.3): implement /admin/api/* endpoints and screens for global providers, model catalog, and price book [Spec: MDL-3, MDL-4, UI-1]` 2. `feat(admin/6.3): implement user management, traceability settings, and audit log screens [Spec: AUTH-4, USR-5, TRC-1, AUD-1]` |
| Risks and resolutions | **Risk:** Editing global model exposure or route priorities changes the projected WRR session shares in the Routing Matrix UI. **Resolution:** Reuse the exact `crates/core::wrr::projected_shares` function in the template handler so UI percentages always match router behavior. |

### 6.4 Sub-phase 6.4: Overview dashboard, dual `/usage` gauges, Active Sessions, and Usage Explorer

This sub-phase implements the Overview Dashboard with `uPlot` charts and side-by-side local and upstream `/usage` window gauges (`SRDD Section 14.3`). It also implements the Active Sessions pin table and the Usage Explorer with CSV and JSON export.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.9`, `Section 6.11`, `Section 14.3`; Requirements `USG-1`..`USG-8`, `UI-1`, `UI-2` |
| Crates touched | `crates/admin::dashboard`, `crates/admin::sessions`, `crates/admin::usage_explorer` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/e2e_dashboard_htmx.rs`, `#[doc = "Spec: USG-4, USG-6, UI-1, UI-2"]`) asserting that: (1) standard users see only their own client keys' usage and pins, while admins can group by `user`, `client_key`, `upstream_key`, `provider`, `provider_scope`, and `model`, (2) upstream key cards display both local estimated spend and reconciled source provider `/usage` percentages (`5h`, `7d`, `1mo`), and (3) moving an active pin in the Sessions UI routes the very next turn of that session to the newly selected upstream key. |
| Atomic commits | 1. `feat(admin/6.4): implement Overview Dashboard with uPlot charts and dual local/upstream /usage gauges [Spec: UI-1, UI-2]` 2. `feat(admin/6.4): implement Active Sessions pin manager and Usage Explorer with CSV/JSON export [Spec: UI-1]` |
| Risks and resolutions | **Risk:** Range queries over 30 days of raw `usage_events` could be slow in the dashboard. **Resolution:** Route all time-series charts and summary cards to `usage_rollups_hourly` (which is pre-aggregated in O(1) per hour), querying raw `usage_events` only for the paginated event table and CSV export. |

### 6.5 Sub-phase 6.5: Read-only Session Chat Preview UI (`TRC-5`, `TRC-6`, `UI-4`)

This sub-phase builds the client-side (dashboard) read-only Session Chat Preview drawer and full-page view (`/app/sessions/{session_hash}/chat` and `/admin/sessions/{session_hash}/chat`), rendering the reconstructed multi-turn conversation timeline (`SRDD Section 6.10, Section 12.6, Section 14.3`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.10`, `Section 8.2`, `Section 12.6`, `Section 14.3`; Requirements `TRC-5`, `TRC-6`, `TRC-8`, `UI-4`, `SEC-19` |
| Crates touched | `crates/admin::chat_preview_ui`, `crates/admin::templates::chat_preview` |
| TDD step (`Red -> Green`) | Write end-to-end HTMX rendering and security tests (`tests/e2e_dashboard_htmx.rs`, `#[doc = "Spec: TRC-5, TRC-6, TRC-8, UI-4, SEC-19"]`) verifying that clicking **Preview Chat** on a traced session renders: (1) collapsible `<details>` blocks for system instructions, tool schemas, extended thinking/reasoning traces, and formatted tool-call JSON arguments/results, (2) sanitized Markdown and formatted code blocks for assistant responses (`ammonia`), (3) per-turn telemetry chips (`turn_index`, timestamp, model, upstream key, TTFB, latency, tokens, cache hit rate, cost), and (4) a raw request/response JSON drawer per turn. |
| Atomic commits | 1. `feat(admin/6.5): implement read-only Session Chat Preview UI and raw turn JSON inspector [Spec: TRC-5, TRC-6, TRC-8, UI-4, SEC-19]` |
| Risks and resolutions | **Risk:** Rendering large tool outputs (such as a 2,000-line file read by Claude Code or opencode) clutters the chat timeline. **Resolution:** Collapse tool results and reasoning blocks taller than 12 lines inside expandable `<details>` containers by default, showing a one-line summary (`Tool Result: Read 2,048 lines, 64.2 KiB`). |

## Epic 7: System hardening, fuzzing, load verification, live harness E2E, and v1.0.0 release

Epic 7 hardens the gateway through fuzzing, full-workspace mutation testing, and 500 req/s load benchmarks. It also verifies crash recovery, headless CLI agent compatibility (`Claude Code`, `Codex CLI`, `opencode`), and signed release packaging. It corresponds to Milestone `M6` (`SRDD Sections 7, 8, 16, 17, 18, 19`).

### 7.1 Sub-phase 7.1: Structure-aware fuzzing (`cargo-fuzz`) and canary secret verification

This sub-phase runs fuzzing campaigns against all untrusted wire parsers (`body_edit`, `sse_tap`, `trace_tap`, `headers`). It also runs the full-system canary secret redaction test (`SEC-6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 8.2`, `Section 16.5.3`; Requirements `SEC-6`, `SEC-8`, `SEC-15`, `TRN-2` |
| Crates touched | `fuzz/fuzz_targets/*`, `tests/security_canary_and_ssrf.rs` |
| TDD step (`Red -> Green`) | 1. Write four `cargo-fuzz` targets asserting zero panics, zero out-of-bounds slices, and bounded memory allocation on arbitrary byte streams. 2. Run `tests/security_canary_and_ssrf.rs` (`#[doc = "Spec: SEC-6, SEC-14"]`) planting canary secrets in upstream keys, client keys, cookies, and master keys, forcing upstream 401/500 errors, malformed SSE streams, and simulated handler panics, and asserting zero occurrences of any canary substring across stdout/stderr JSON logs, HTTP responses, `audit_log`, and `usage_events`. |
| Atomic commits | 1. `test(fuzz/7.1): add cargo-fuzz targets for JSON span editor, SSE tap, trace reassembler, and header parser [Spec: SEC-8, SEC-15, TRN-2]` 2. `sec(tests/7.1): add full-stack canary secret redaction test across logs, errors, and SQLite tables [Spec: SEC-6, SEC-14]` |
| Risks and resolutions | **Risk:** A fuzzer discovers a multi-byte UTF-8 boundary split across two `Bytes` chunks in `SseUsageTap` or `TraceTap` that causes a slice panic. **Resolution:** Convert any discovered crash input into a permanent regression test under `crates/proto/tests/regressions.rs` (`SRDD Section 16.5.4`) and use `std::str::from_utf8` with incomplete trailing UTF-8 byte carryover across chunk boundaries. |

### 7.2 Sub-phase 7.2: Full-workspace mutation sweep (`cargo-mutants`) and 100% traceability gate

This sub-phase runs a full workspace `cargo mutants --workspace` sweep (`SRDD Section 16.5.6`). It also runs `scripts/verify_traceability.py` (`SRDD Section 16.6`) to verify bidirectional links across all SRDD requirements.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 16.5.6`, `Section 16.6`; Requirements `G8`, `ADR-8`, all requirement IDs in `Sections 6, 7, 8` |
| Crates touched | All test modules across `crates/{core,proto,store,proxy,admin,server}`, `scripts/verify_traceability.py` |
| TDD step (`Red -> Green`) | Run `cargo mutants --workspace` and inspect every surviving mutant. For each survivor, write the missing boundary or negative contract test that kills the mutant. Run `python3 scripts/verify_traceability.py` and confirm 100% of requirement IDs in `6. SRDD - Functional Requirements.md`, `7. SRDD - Non-Functional Requirements.md`, and `8. SRDD - Security and Privacy.md` map to at least one passing test. |
| Atomic commits | 1. `test(workspace/7.2): eliminate remaining surviving mutants across core, proto, store, proxy, and admin [Spec: G8, ADR-8]` 2. `ci(workspace/7.2): enforce 100% SRDD-to-test traceability check in CI [Spec: G8]` |
| Risks and resolutions | **Risk:** Full-workspace mutation testing takes too long on every pull request. **Resolution:** Keep `cargo mutants --in-diff` (typically `< 90 s`) as the blocking PR gate and run full `cargo mutants --workspace` in the nightly and pre-release CI workflow (`SRDD Section 16.5.6`). |

### 7.3 Sub-phase 7.3: Load and soak benchmarks (`PERF-1`..`PERF-5`) and crash/drain resilience (`REL-1`..`REL-6`)

This sub-phase validates the latency and memory targets (`PERF-1`..`PERF-5`) on a 2 vCPU, 2 GB RAM host against a stub upstream cluster. It also verifies graceful `SIGTERM` draining, `SIGKILL` WAL durability, and online SQLite backups (`REL-1`..`REL-6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.1`, `Section 7.2`; Requirements `PERF-1`..`PERF-5`, `REL-1`..`REL-6` |
| Crates touched | `benches/proxy_latency.rs`, `tests/e2e_sigterm_drain.rs`, `tests/e2e_kill9_recovery.rs`, `crates/server::shutdown`, `crates/server::backup` |
| TDD step (`Red -> Green`) | 1. Run the load harness (`500 req/s` and `2,000` concurrent SSE streams, both with `content_traceability_mode` off and on) and assert: p50 overhead `<= 1 ms`, p99 overhead `<= 5 ms` (`PERF-1`), chunk forwarding delay `<= 1 ms` (`PERF-2`), RSS `<= 300 MB` at 2,000 streams (`PERF-3`), idle RSS `<= 50 MB` and startup `<= 2 s` (`PERF-4`), and traceability TTFB delta `<= 0.5 ms` (`PERF-5`). 2. Send `SIGTERM` with 50 active streams and assert all 50 complete cleanly (`REL-1`). 3. Send `SIGKILL` (`kill -9`) 2 s after confirming pins and usage rows and assert zero lost pins or usage rows on restart (`REL-2`). |
| Atomic commits | 1. `feat(server/7.3): implement graceful SIGTERM stream drain and llmroller backup CLI command [Spec: REL-1, REL-6]` 2. `perf(workspace/7.3): add load benchmark harness and verify PERF-1 through PERF-5 and REL-2 [Spec: PERF-1, PERF-2, PERF-3, PERF-4, PERF-5, REL-2]` |
| Risks and resolutions | **Risk:** Under 2,000 open streams with `content_traceability_mode` enabled, per-stream trace buffers could push RSS above `300 MB`. **Resolution:** Stream trace deltas in 16 KiB chunks into a per-stream incremental `zstd::stream::Encoder` (which compresses ~5x in flight) so a 100 KiB transcript occupies `< 20 KiB` of RAM while streaming. |

### 7.4 Sub-phase 7.4: Headless CLI harness compatibility (`COM-1`) and Milestone 1 live checks

This sub-phase runs headless E2E workflows with `Claude Code`, `Codex CLI`, `opencode`, and the OpenAI and Anthropic SDKs (`COM-1`). It also runs the opt-in live provider verification scripts (`SRDD Section 19.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.3`, `Section 16.5.5`, `Section 19.2`; Requirements `COM-1`, `OBS-1`, `PRIV-1` |
| Crates touched | `tests/e2e_cli_harnesses.rs`, `scripts/verify_cache_affinity.sh` |
| TDD step (`Red -> Green`) | Run `tests/e2e_cli_harnesses.rs` (`#[doc = "Spec: COM-1, OBS-1, PRIV-1"]`) pointing each CLI tool and SDK at LLMRoller backed by multi-turn tool-calling stub upstreams. Assert that: (1) every multi-turn tool-call loop completes, (2) all turns in one CLI invocation hit the same pinned `upstream_key_id`, (3) when content traceability mode is enabled, the entire multi-turn tool-call conversation renders accurately in `GET /api/user/sessions/{session_hash}/chat`, and (4) network egress monitoring confirms zero outbound packets to any host outside the configured upstream origins (`PRIV-1`). |
| Atomic commits | 1. `test(e2e/7.4): add headless CLI harness compatibility suite for Claude Code, Codex CLI, opencode, and SDKs [Spec: COM-1, OBS-1, PRIV-1]` 2. `feat(scripts/7.4): add opt-in live cache-affinity and /usage verification script [Spec: RTE-2, USG-4]` |
| Risks and resolutions | **Risk:** A new release of `Claude Code` or `Codex CLI` changes a path or header formatting expectation. **Resolution:** Lock the recorded inbound HTTP requests from each CLI version into `fixtures/harnesses/` so CI runs them deterministically on every PR even when the external CLI binary is not installed in a minimal container. |

### 7.5 Sub-phase 7.5: Multi-arch static builds, distroless container, SBOM, `cosign` signing, and v1.0.0 release

This sub-phase builds static binaries for Linux (`x86_64`, `aarch64`) and macOS (`aarch64-apple-darwin`) and packages the `< 50 MB` distroless container (`POR-1`, `SEC-11`). It then generates the CycloneDX SBOM, signs all release files with `cosign`, and tags `v1.0.0`.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.4`, `Section 8.2`, `Section 12.1`; Requirements `POR-1`, `SEC-10`, `SEC-11` |
| Crates touched | `.github/workflows/release.yml`, `Dockerfile`, `docker-compose.yml` |
| TDD step (`Red -> Green`) | Run the release packaging smoke test (`scripts/verify_release_package.sh`, `#[doc = "Spec: POR-1, SEC-11"]`) asserting: (1) container image compressed/uncompressed size is `< 50 MB`, (2) container runs as UID `65532` with a read-only root filesystem, (3) `llmroller --version`, `llmroller backup`, `llmroller rotate-master-key`, and `llmroller verify-audit-log` succeed on a fresh container start, and (4) SHA-256 checksums and SBOM validate cleanly. |
| Atomic commits | 1. `ci(release/7.5): add multi-arch musl build matrix, distroless Dockerfile (<50MB), SBOM, and cosign signing [Spec: POR-1, SEC-10, SEC-11]` |
| Risks and resolutions | **Risk:** `zstd` C bindings complicate cross-compilation to `aarch64-unknown-linux-musl`. **Resolution:** Compile with `cargo-zigbuild` in CI (which provides a hermetic musl C/C++ cross-toolchain for `x86_64` and `aarch64`) or enable pure-Rust compression fallback where needed. |
