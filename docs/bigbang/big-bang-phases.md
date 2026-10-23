<!-- Generated file - edit the numbered parts -->
# LLMRoller: Big-Bang Development Phases and TDD Execution Guide

Version 1.3 · 20 October 2026 · Adaptive ground-up engineering guide from `git init` to `v1.0.0` with pluggable routing strategies, Docker deployment lockstep gates, and full traceability to the LLMRoller SRDD.


## Epic 0: Engineering governance, TDD workflow, atomic commits, and adaptive playbook

This guide defines how engineers and coding agents build LLMRoller from an empty repository (`git init`) to version 1.0.0. It pairs every phase and sub-phase with exact references to the Software Requirements and Design Document (SRDD Sections 0 through 22).

### 0.1 How to use this guide as an adaptive playbook

This document is an adaptive engineering playbook, not a rigid waterfall script. Software development uncovers upstream API quirks, latency bottlenecks, and schema refinements as real code meets real traffic.

The split epic files (`big-bang-phases-0.md` through `big-bang-phases-7.md`, including `big-bang-phases-4b.md`) and `docs/srdd/requirements.toml` are authoritative. The combined `big-bang-phases.md` file is a generated bundle built by `cargo xtask docs-bundle` and verified in CI.

The epics follow the dependency graph of the seven workspace members (`core`, `proto`, `store`, `proxy`, `admin`, `server`, `xtask`). Engineers may reorder sibling sub-phases or split a sub-phase when an upstream quirk appears. They may also merge two sub-phases when a shared trait simplifies both.

| Invariant that never bends | Flexible aspect that adapts as needed |
| --- | --- |
| Every requirement ID (`AUTH-*`, `USR-*`, `PRV-*`, `SHR-*`, `CLK-*`, `MDL-*`, `RTE-*`, `API-*`, `USG-*`, `LIM-*`, `TRC-*`, `UI-*`, `AUD-*`, `PERF-*`, `REL-*`, `TRN-*`, `COM-*`, `POR-*`, `MNT-*`, `OBS-*`, `PRIV-*`, `SEC-*`) must map to passing tests via `docs/srdd/requirements.toml` (`SRDD Section 16.6`). | The internal struct layout, helper functions, and private module organization inside each crate can change freely during refactoring. |
| `crates/core` must remain 100% free of network and disk I/O (`SRDD Section 15.2`, `MNT-2`). | The order in which provider adapters (`opencode_go`, `command_code`, `open_design_amr`, `openrouter`) are implemented can shift based on test key availability. |
| Red-Green-Refactor TDD, the Expected vs. Unexpected failure rules, and zero unannotated surviving mutants in [cargo-mutants](https://mutants.rs/) apply to every pull request (`SRDD Section 16.2`, `16.3`). | Batch sizes, channel capacities, cache TTL defaults, and SQLite connection pool sizes can be tuned empirically during benchmark phases. |
| Every architectural or contract change cascades in order: `SRDD + requirements.toml -> Seam Trait / Contract Test -> Code -> xtask traceability & doc-consistency`. | UI layout details and HTMX partial boundaries can evolve as user workflows are tested in Epic 6. |

### 0.2 The seven-step Red-Green-Refactor-Mutate TDD loop

Every sub-phase in Epics 1 through 7 is built through a strict seven-step loop. No production logic lands before its failing contract test exists (`SRDD Section 16.3`).

| Step | Loop stage | Developer action and verification command |
| --- | --- | --- |
| 1 | Trace to SRDD | Read the target SRDD section and requirement IDs listed in the sub-phase header. If a new edge case changes a contract, update the SRDD requirement table and `docs/srdd/requirements.toml` first. |
| 2 | Define seam or type | Add or update the domain structs and seam traits (`Clock`, `AsyncClock`, `EntropySource`, `UpstreamTransport`, `UsageEndpointClient`, `StorageRepository`, `SecretSealer` from `SRDD Section 16.4`) with `todo!()` bodies so the crate compiles. |
| 3 | Write failing test (`Red`) | Write unit, property (`proptest`), or integration tests against the observable contract. Annotate full requirement tests with `#[doc = "Spec: <REQ-ID>"]` and partial prerequisite tests with `#[doc = "Partial: <REQ-ID>"]`, then run `cargo nextest run` to confirm the new test fails. |
| 4 | Write minimal code (`Green`) | Write the smallest, clearest implementation in safe Rust (`#![forbid(unsafe_code)]`) that makes the new test pass while keeping all prior tests green. |
| 5 | Refactor under green | Remove duplication, simplify control flow, and run `cargo clippy --all-targets -- -D warnings` plus `cargo nextest run`. |
| 6 | Kill surviving mutants | Stage the changes (`git add -A`), export the staged diff (`git diff --cached > target/commit.diff`), and run `cargo mutants --in-diff target/commit.diff` (recommended locally per commit; mandatory blocking gate on every PR via `git diff origin/main...HEAD` and nightly). Equivalent mutants must carry `#[mutants::skip] // equivalent: <reason>` with CODEOWNERS review (`SRDD Section 16.5.6`). |
| 7 | Commit atomically | Run `cargo xtask traceability --milestone M<N>` and `cargo xtask doc-consistency`, verify `cargo xtask docs-bundle --check`, and create one atomic commit following Section 0.4. |

### 0.3 Failing test triage: expected vs. unexpected failures

As defined in `SRDD Section 16.2`, failing tests divide into two categories. Treating an unexpected regression like an expected red test destroys the safety net of the suite.

| Failure type | When it occurs | Mandatory engineering response |
| --- | --- | --- |
| Expected failure | (a) A newly written test guard in Step 3 (`Red`) before its implementation lands, or (b) an existing test that pinned an old behavior being intentionally replaced by a new requirement or `ADR-*` decision. | For (a), write the implementation in Step 4 until green; feature branches may contain red commits only if squashed before merge, and `main` never accepts a red commit. For (b), update or retire the old test in the **same atomic commit** as the implementation change, and name the sanctioning requirement ID or `ADR-*` in the commit message. |
| Unexpected failure | Changing component X causes an existing test in component Y (or an unrelated contract in X) to fail. | **Stop immediately.** Do not loosen assertions, do not add `#[ignore]`, and do not tweak mock return values to force green. Diagnose whether an architectural boundary leaked or an invariant broke, fix the root cause, and add a regression test if a hidden bug was exposed. |

### 0.4 Atomic commit and branch discipline

Work progresses in small, self-contained atomic commits. Each sub-phase typically produces between two and six atomic commits.

| Rule | Atomic commit standard |
| --- | --- |
| One logical change per commit | Never mix pure structural refactoring (renaming modules, extracting helpers) with behavioral changes in the same commit. |
| Test and code travel together | Every behavioral commit on `main` contains both the contract test (`#[doc = "Spec: ..."]` or `#[doc = "Partial: ..."]`) and the production code that makes it pass, plus any test updated under the Expected Failure rule. |
| Always green on `main` | Every commit on `main` must compile with zero warnings (`cargo clippy -- -D warnings`) and pass `cargo nextest run`. Every pull request (`origin/main...HEAD`) and nightly run must pass `cargo mutants --in-diff` (or `--workspace`) and `cargo xtask traceability`. |
| Traceable commit message format | Use the header format `<type>(<crate>/<subphase>): <imperative summary>` followed by a body line `Spec: <REQ-IDs>` and `Tests: <test_module_paths>`. Allowed types: `feat`, `fix`, `refactor`, `test`, `sec`, `perf`, `docs`, `ci`. |

Example atomic commit message for Sub-Phase 3.2:

```text
feat(core/3.2): enforce share consent, overlap checks, and monthly cap reservations

Spec: SHR-1, SHR-2, SHR-3, SHR-4, SHR-5, SEC-17
Decision: ADR-1, ADR-3
Tests: crates/core/src/shares/tests.rs::share_eligibility_and_cap_exhaustion
Mutants: 18 generated, 18 killed, 0 survived
```

### 0.5 Adaptive change-management and pivot protocol

When real-world conditions change during development, engineers follow the table below to adjust the plan cleanly without accumulating technical debt.

| Trigger event during development | How development changes | Cascade adjustment steps |
| --- | --- | --- |
| An upstream provider changes its `/models` or `/usage` JSON schema or auth header | Only the specific `ProviderAdapter` in `crates/proto` and its preset TOML change (`MNT-2`). `crates/core` and `crates/proxy` stay untouched. | 1. Capture the new live payload into `fixtures/usage/` or `fixtures/sse/`. 2. Update the golden contract test (Expected Failure citing the upstream change). 3. Update the parser in `crates/proto`. |
| A client harness (Claude Code, Codex CLI, opencode) introduces a new session header | Add the new header name to the eight-step precedence table in `SRDD Section 11.1` and `crates/core::session`. | 1. Update `SRDD Section 11.1`. 2. Add a table-test row in `core::session::tests`. 3. Add the header constant to the resolver slice. |
| SQLite write latency spikes when content traceability mode (`TRC-2`) is active at 500 req/s | Standalone transactions already isolate trace records `> 1 MiB` (`SRDD Section 9.2`, `Section 21.6.1`); tune `trace_sealer_threads` or batch byte caps in `crates/store::traces`. | 1. Check `PERF-1` and `PERF-5` benchmarks. 2. Adjust `writer_batch_max_bytes` or sealer thread count. 3. Verify `REL-3` three-queue overflow test still passes. |
| A sub-phase reveals a missing edge case in route eligibility or RBAC | Pause feature work in the current sub-phase and lock the boundary in `crates/core` first. | 1. Add the edge case to `SRDD Section 6` or `Section 8`. 2. Write the red unit test in `crates/core`. 3. Fix the pure logic and run `cargo mutants` on `crates/core`. |

### 0.6 Epic overview map (zero to v1.0.0)

The table below maps the eight implementation epics (`big-bang-phases-1.md` through `big-bang-phases-7.md`, including `big-bang-phases-4b.md`) one-to-one to SRDD milestones and requirement families, generated from `docs/srdd/requirements.toml`.

| Epic file | Epic title | SRDD milestone | Requirement families completed (`Spec:`) |
| --- | --- | --- | --- |
| `big-bang-phases-1.md` | Foundation, storage engine, cryptography, RBAC, and snapshot reloader | `M0` | `AUTH-1`..`AUTH-9`, `USR-1`, `AUD-1`, `SEC-1`, `SEC-2`, `SEC-4`, `SEC-5`, `SEC-10`, `SEC-13`, `SEC-14`, `SEC-17`, `REL-4`, `MNT-1`, `MNT-2` |
| `big-bang-phases-2.md` | Transparent wire proxy, protocol normalizer, provider adapters, and orchestrator v1 | `M1` | `PRV-1`..`PRV-5`, `PRV-7`, `CLK-1`, `CLK-4`, `API-2`..`API-4`, `API-6`, `API-7`, `TRN-1`..`TRN-3`, `SEC-3`, `SEC-6`..`SEC-9`, `SEC-12`, `SEC-15`, `REL-1`, `OBS-1` |
| `big-bang-phases-3.md` | Core pricing/windows/accumulators, model catalog, sharing, pluggable routing strategies, pinning, and orchestrator v2 | `M2` | `USR-2`..`USR-5`, `PRV-8`, `SHR-1`..`SHR-5`, `CLK-2`, `CLK-3`, `CLK-5`, `MDL-1`..`MDL-7`, `RTE-1`..`RTE-14`, `API-8`, `LIM-1`, `REL-5`, `SEC-16` |
| `big-bang-phases-4.md` | Usage taps, three-queue batching writer, 5-minute & hourly rollups, orchestrator v3, and retention | `M3` | `API-5`, `USG-1`..`USG-3`, `SHR-6`, `REL-2`, `REL-3` |
| `big-bang-phases-4b.md` | Startup 5-minute rollup reconstruction and source provider `/usage` poller, observations, and pass-through | `M4` | `PRV-6`, `API-1`, `USG-4`..`USG-8`, `SEC-18` |
| `big-bang-phases-5.md` | Opt-in content traceability mode, content-addressed blocks, 2-thread sealer pool, and chat reconstructor | `M3b` | `TRC-1`..`TRC-5`, `TRC-7`..`TRC-9`, `PERF-5`, `SEC-19` |
| `big-bang-phases-6.md` | Dual-workspace HTMX web UI, key projection, share consent modals, and Session Chat Preview UI | `M5` | `UI-1`..`UI-4`, `TRC-6` |
| `big-bang-phases-7.md` | System hardening, fuzzing, load verification, live harness E2E, and v1.0.0 release | `M6` | `PERF-1`..`PERF-4`, `REL-6`, `COM-1`, `POR-1`, `PRIV-1`, `SEC-11` |
## Epic 1: Foundation, storage engine, cryptography, RBAC, and snapshot reloader

Epic 1 builds the seven-member Cargo workspace, the seam traits, the 31-table SQLite WAL store, and the `XChaCha20-Poly1305` secret sealer. It also implements atomic `/setup`, `ActorScope` RBAC, the hash-chained `audit_log`, and the `Reloader` pipeline. It corresponds to Milestone `M0` (`SRDD Sections 4, 5, 6.1, 6.2, 8, 9, 15, 16, 21`).

### 1.1 Sub-phase 1.1: Cargo workspace, `xtask` governance tools, foundational Docker scaffolding, and multi-arch CI matrix

This sub-phase initializes the seven-member repository layout (`SRDD Section 15.2`), foundational container scaffolding (`Dockerfile`, `.dockerignore`, `docker-compose.yml`, `.env.example`, `deploy/` per `SRDD Section 22`), and the Rust `xtask` binary. It also sets up compile-time safety, supply-chain checks, and the `POR-1` multi-arch build matrix (`MNT-1`, `MNT-2`, `SEC-10`, `Partial: POR-1`, `Partial: SEC-11`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 15.1`, `Section 15.2`, `Section 16.6`, `Section 17.1`, `Section 22.1`..`22.3`; Requirements `MNT-1`, `MNT-2`, `SEC-10`, `Partial: POR-1`, `Partial: SEC-11` |
| Crates touched | Root `Cargo.toml`, `Dockerfile`, `.dockerignore`, `docker-compose.yml`, `.env.example`, `deploy/{Caddyfile,prometheus.yml}`, `deny.toml`, `.cargo/mutants.toml`, `docs/srdd/requirements.toml`, `xtask`, skeleton `crates/{core,proto,store,proxy,admin,server}` |
| TDD step (`Red -> Green`) | Write an architectural dependency test (`tests/workspace_boundaries.rs`, `#[doc = "Spec: MNT-1, MNT-2, Partial: POR-1, Partial: SEC-11"]`) that inspects root `Cargo.toml` (`[workspace.lints.rust] unsafe_code = "forbid"`) plus each crate's `src/lib.rs` / `src/main.rs` header (`#![forbid(unsafe_code)]`), parses `cargo metadata` to assert `crates/core` has zero dependency edges to `tokio`, `sqlx`, `reqwest`, `hyper`, or `axum`, verifies that `crates/admin` cannot construct `ActorScope::System`, and validates `Dockerfile` (`planner`, `builder`, `runtime-distroless`, `runtime-dev`), `.dockerignore`, and `docker-compose.yml` (`docker compose config --quiet`). Add unit tests for `cargo xtask traceability`, `cargo xtask doc-consistency`, and `cargo xtask docs-bundle --check`. Configure CI to compile and run unit tests across all five `POR-1` targets (`x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`) and build the base `runtime-distroless` and `runtime-dev` images from the first commit. |
| Atomic commits | 1. `ci(workspace/1.1): initialize workspace with forbid(unsafe_code), deny.toml, multi-stage Dockerfile, and POR-1 build matrix [Spec: MNT-1, SEC-10, Partial: POR-1, Partial: SEC-11]` 2. `feat(xtask/1.1): implement xtask traceability, doc-consistency, and docs-bundle with boundary and Compose config tests [Spec: MNT-2]` |
| Risks and resolutions | **Risk:** Transitive dependencies pull in duplicate TLS stacks (`openssl-sys`) or C cross-compilation failures late in the project. **Resolution:** Set `default-features = false` on `reqwest` and `sqlx` and run the `cargo-zigbuild` musl matrix and `cargo deny check bans licenses advisories sources` on every PR starting in Sub-phase 1.1. |

### 1.2 Sub-phase 1.2: Core domain identifiers, `ActorScope` enum, and seam traits

This sub-phase defines the domain primitives in `crates/core` (`ULID` wrappers, `Role`, `ActorScope`, `SystemTask`, `OwnershipScope`, `RoutingScope`, `PoolTier`) and the dependency-injection seam traits (`SRDD Section 4.2, Section 16.4, Section 21.7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.1`, `Section 4.2`, `Section 8.2`, `Section 15.2`, `Section 16.4`, `Section 21.7`; Requirements `USR-1`, `SEC-14`, `SEC-17`, `Partial: USR-2`, `Partial: USR-3`, `Partial: USR-4` |
| Crates touched | `crates/core::ids`, `crates/core::rbac`, `crates/core::seams`, `crates/proxy::clock` |
| TDD step (`Red -> Green`) | Write table-driven RBAC contract tests (`core::rbac::tests::actor_scope_matrix`, `#[doc = "Spec: USR-1, SEC-17, Partial: USR-2, Partial: USR-3, Partial: USR-4"]`) testing `ActorScope::User { user_id, role }` (`Role::Admin`, `Role::User`) and `ActorScope::System { task: SystemTask }` (`Writer`, `Reloader`, `Discovery`, `UsagePoller`, `RetentionSweeper`, `DataPlaneUsage`, `Cli`) against `global`, own `personal`, other users' `personal`, and `shared` resources. Verify that `crates/core::Clock` is purely synchronous (`now_ms`, `monotonic_ms`) while `crates/proxy::AsyncClock: Clock` adds `async fn sleep`. |
| Atomic commits | 1. `feat(core/1.2): define ULID types, SecretString wrappers, synchronous Clock, and EntropySource seams [Spec: SEC-14]` 2. `feat(core/1.2): implement ActorScope User and System variants and RBAC guards [Spec: USR-1, SEC-17, Partial: USR-2, Partial: USR-3, Partial: USR-4]` |
| Risks and resolutions | **Risk:** Background tasks pass a synthetic admin user ID to repository methods, polluting `audit_log` with fake admin actions. **Resolution:** Use `ActorScope::System { task }` with a constructor restricted to `crates/server` and `crates/store`, recording `actor_user_id = NULL` and `detail_json.system_task` on system audit rows (`SRDD Section 21.7`). |

### 1.3 Sub-phase 1.3: Cryptographic sealer (`XChaCha20-Poly1305`), 31-table SQLite WAL store, and resumable key rotation

This sub-phase implements `HKDF-SHA256` subkeys, canonical length-prefixed `aad(domain, fields)`, the `0x01 || nonce || ciphertext || tag` envelope, and `instance_secrets`. It also builds the 31-table SQLite WAL migration, `connect_options(role)`, and offline resumable `rotate-master-key` (`SRDD Section 8.2, Section 9.1, Section 21.6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 8.2`, `Section 9.1`, `Section 9.2`, `Section 21.6`; Requirements `SEC-1`, `SEC-2`, `Partial: PRV-5`, `Partial: SEC-19` |
| Crates touched | `crates/store::crypto`, `crates/store::migrations`, `crates/store::pool`, `fixtures/crypto/aad_vectors.json` |
| TDD step (`Red -> Green`) | 1. Write unit tests (`store::crypto::tests::aead_aad_and_rotation`, `#[doc = "Spec: SEC-1, SEC-2, Partial: PRV-5, Partial: SEC-19"]`) verifying: (a) missing or non-32-byte decoded `LLMROLLER_MASTER_KEY` or group/other-readable `LLMROLLER_MASTER_KEY_FILE` exits with code `3`, (b) deterministic test vectors in `fixtures/crypto/aad_vectors.json` match `aad(domain, fields)` across all six domains (`upstream_key`, `totp_secret`, `usage_raw`, `trace_block`, `trace_record`, `instance_secret`), (c) swapping ciphertext blobs across rows or columns fails AEAD authentication, and (d) `rotate-master-key` re-seals all six encrypted tables in 500-row batches while preserving `instance_secrets` (`session_hmac_secret` and `audit_hmac_key`), resumes cleanly from `rotation_cursor_table`/`rotation_cursor_id` after a simulated mid-rotation kill, and causes `serve` to exit with code `3` if `rotation_state = 'in_progress'` or `LLMROLLER_MASTER_KEY_ID != active_master_key_id`. 2. Write SQLite migration tests verifying `PRAGMA auto_vacuum == 2` (`INCREMENTAL`), `connect_options(role)` pragmas for `Writer`, `Readers`, and `Cli`, `_llmroller_schema(version)` forward-only guard (exit code `14` on newer schema), and all ownership/uniqueness triggers (`trg_client_keys_hash_disjoint`, `trg_upstream_keys_owner_*`, `trg_shares_integrity_*`, `trg_alias_not_canonical`). |
| Atomic commits | 1. `sec(store/1.3): implement HKDF-SHA256 subkeys, canonical length-prefixed AAD sealer, and committed test vectors [Spec: SEC-1, SEC-2, Partial: PRV-5, Partial: SEC-19]` 2. `feat(store/1.3): add connect_options builder, incremental auto_vacuum, and 31-table SQLite schema with triggers [Spec: SEC-17]` 3. `sec(store/1.3): implement instance_meta, instance_secrets, and resumable batched rotate-master-key CLI [Spec: SEC-1, SEC-2]` |
| Risks and resolutions | **Risk:** Per-connection pragmas (`foreign_keys`, `busy_timeout`, `query_only`) placed only in migration SQL do not apply to pool connections or CLI tools. **Resolution:** Enforce all pragmas inside the single `crates/store::pool::connect_options(role: Role)` constructor and assert their values by querying `PRAGMA` on every role in integration tests. |

### 1.4 Sub-phase 1.4: Atomic first-admin `/setup`, `Argon2id` + `TOTP` auth, and `reset-admin` CLI

This sub-phase implements the `${LLMROLLER_DATA_DIR}/setup-token` file (`0600`), serializable first-admin creation, permanent 404 lockout, `Argon2id` hashing, and `TOTP` verification. It also adds in-transaction `last_admin` protection, `llmroller reset-admin`, non-loopback TLS checks, random CSRF tokens, and the three-tier login limiter (`SRDD Section 6.1, Section 8.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 6.1`, `Section 8.2`; Requirements `AUTH-1`..`AUTH-9`, `SEC-4`, `SEC-5`, `Partial: SEC-9`, `Partial: SEC-12` |
| Crates touched | `crates/store::users`, `crates/admin::setup`, `crates/admin::auth`, `crates/server::cli` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/e2e_setup_and_auth.rs`, `#[doc = "Spec: AUTH-1, AUTH-2, AUTH-3, AUTH-4, AUTH-5, AUTH-6, AUTH-7, AUTH-8, AUTH-9, SEC-4, SEC-5, Partial: SEC-9, Partial: SEC-12"]`): (1) auto-generated setup token is written to `${LLMROLLER_DATA_DIR}/setup-token` with mode `0600` (only the path logged to stderr) and deleted on setup completion, (2) 20 concurrent `POST /admin/api/setup` requests result in exactly one HTTP 201, 19 HTTP 409/404 responses, and `SELECT COUNT(*) FROM users == 1`, (3) two admins concurrently demoting or disabling each other inside `BEGIN IMMEDIATE` serialize so the second returns HTTP 409 `last_admin` (`AUTH-5`), (4) `llmroller reset-admin --username <name>` restores admin access offline while holding `llmroller.lock`, (5) 1,000 bad passwords for `admin` across 100 IPs trigger TOTP/2s delay without locking out `admin` from a new IP, while trusted-proxy `X-Forwarded-For` extraction isolates users behind `LLMROLLER_TRUSTED_PROXIES`, and (6) non-loopback control-plane bind without TLS or `LLMROLLER_TRUSTED_PROXIES` exits with code `2`, login sets `__Host-llmroller_login_csrf`, and authenticated sessions use random 256-bit CSRF tokens (`csrf_hash = SHA-256(token)`). |
| Atomic commits | 1. `feat(store/1.4): implement user repository with Argon2id, TOTP, in-transaction last_admin guard, and reset-admin CLI [Spec: AUTH-4, AUTH-5, AUTH-6, AUTH-9, SEC-4]` 2. `feat(admin/1.4): implement setup-token file bootstrap, serializable /setup, CSRF cookies, and three-tier login limiter [Spec: AUTH-1, AUTH-2, AUTH-3, AUTH-7, AUTH-8, SEC-5, Partial: SEC-9, Partial: SEC-12]` |
| Risks and resolutions | **Risk:** `Argon2id` (`>= 19 MiB` memory per hash) can exhaust RAM or stall under login floods. **Resolution:** Bound concurrent `Argon2id` verifications with a semaphore (`max(2, available_parallelism)` permits, 5 s queue timeout returning `503 login_busy`) and evaluate the three-tier limiter before hashing. |

### 1.5 Sub-phase 1.5: Tamper-evident `audit_log` HMAC chain, external anchors, and `verify-audit-log` CLI

This sub-phase implements the `HMAC-SHA256` hash chain on `audit_log` ordered by `seq ASC` using `audit_hmac_key` from `instance_secrets`. It also adds periodic `audit_anchor` stdout log emission and the `llmroller verify-audit-log` CLI (`AUD-1`, `SEC-13`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 6.11`, `Section 8.2`, `Section 9.2.1`; Requirements `AUD-1`, `SEC-13` |
| Crates touched | `crates/store::audit`, `crates/server::cli` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`store::audit::tests::hash_chain_verification`, `#[doc = "Spec: AUD-1, SEC-13"]`) verifying: (1) inserting 10 audit rows (including simulated backward NTP wall-clock steps on `ts`) verifies cleanly over `seq ASC` from the 64-ASCII-zero genesis `prev_hash`, (2) mutating one character in row 4 or deleting row 5 (leaving a `seq` gap) causes `verify-audit-log` to exit with code `8`, (3) every 100 rows (and hourly) emits `{"event":"audit_anchor","seq":...,"hash":...,"ts":...}` to stdout, and (4) deleting the last 3 rows causes `verify-audit-log --anchor-seq N --anchor-hash H` to exit with code `9` (`truncation detected`). |
| Atomic commits | 1. `sec(store/1.5): implement seq-ordered HMAC-SHA256 audit_log chain, stdout anchors, and verify-audit-log CLI [Spec: AUD-1, SEC-13]` |
| Risks and resolutions | **Risk:** Rotating the master key would invalidate the audit chain if the HMAC key were derived directly from the master key. **Resolution:** Sign the chain with the random 256-bit `audit_hmac_key` stored in `instance_secrets` (re-sealed on master-key rotation), keeping historical signatures valid across rotations. |

### 1.6 Sub-phase 1.6: Dual-priority SQLite writer, closed `settings` registry, `Reloader` task, and `RuntimeState`

This sub-phase implements the dual-queue SQLite writer (`Control` priority queue and `4 MiB` `Data` batches) and the closed 14-key `settings` registry (including `default_routing_strategy` and `LLMROLLER_DEFAULT_ROUTING_STRATEGY`). It also adds `RuntimeState` and the single `Reloader` task that publishes versioned per-domain `Arc` slices into `ArcSwap<ConfigSnapshot>` (`REL-4`, `Partial: RTE-14`, `SRDD Section 21.6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.4`, `Section 7.2`, `Section 9.2`, `Section 9.3.3`, `Section 21.6`; Requirements `REL-4` |
| Crates touched | `crates/store::writer`, `crates/store::settings`, `crates/core::snapshot`, `crates/core::runtime_state`, `crates/server::reloader` |
| TDD step (`Red -> Green`) | Write concurrency and latency tests (`tests/integration_snapshot_reloader.rs`, `#[doc = "Spec: REL-4"]`): (1) 50 concurrent alternating control-plane mutations commit through the `Control` queue (incrementing `instance_meta.config_version` in the same transaction) and the single `Reloader` task coalesces notifications, rebuilds only changed domain `Arc` slices in one read transaction, never publishes an older `config_version` over a newer one, and applies `CacheInvalidationEvent`s after `arc_swap.store(...)`, (2) while the `Data` queue is saturated with `4 MiB` batches, a control-plane mutation commits and propagates to `ConfigSnapshot` within `1 s` (`REL-4`), and (3) unknown keys in `settings` are rejected with HTTP 422. |
| Atomic commits | 1. `feat(store/1.6): implement dual-priority Control/Data writer actor and closed 14-key settings registry with default_routing_strategy [Spec: REL-4, Partial: RTE-14]` 2. `feat(server/1.6): implement versioned Reloader task, per-domain ConfigSnapshot slices, and RuntimeState [Spec: REL-4]` |
| Risks and resolutions | **Risk:** Two concurrent handlers calling `reload_snapshot()` directly can publish an older snapshot over a newer revocation. **Resolution:** Only the single `Reloader` task writes to `ArcSwap<ConfigSnapshot>`, gated on `v > published.version`, and handlers await the reloader's version notification (up to 1 s) before returning 2xx (`SRDD Section 21.6.2`). |

#### 1.6.1 Incremental `ConfigSnapshot` domain slices across Epics 1 through 5

Sub-Phase 1.6 introduces `ArcSwap<ConfigSnapshot>` with per-domain `Arc` slices. Epics 1 through 5 populate its slices additively as each domain lands.

| Sub-phase | Domain `Arc` slices in `ConfigSnapshot` | Control-plane mutations that bump `config_version` and notify `Reloader` |
| --- | --- | --- |
| `1.6` | `users: Arc<HashMap<UserId, UserRecord>>`, `settings: Arc<RuntimeSettings>` | User CRUD, role/budget/disable update (`OwnerDisabled`), settings update |
| `2.0` & `2.1` | `providers: Arc<HashMap<ProviderId, ProviderRecord>>`, `upstream_keys: Arc<HashMap<UpstreamKeyId, UpstreamKeyRecord>>` | Provider or upstream key CRUD, enable/disable (`PRV-8`), `share_config_fingerprint` update |
| `2.2` | `client_keys_by_hash: Arc<HashMap<[u8; 32], (ClientKeyRecord, HashRole)>>` | Client key creation, in-place rotation with grace (`CLK-2`), revocation (`CLK-4`), CIDR/RPM/mode edit |
| `3.0a` & `3.0b` | `prices: Arc<HashMap<(ProviderIdOrEmpty, ProviderKind, String), PriceEntry>>`, `budget_windows: Arc<HashMap<UpstreamKeyId, Vec<BudgetWindowSpec>>>` | Price book edit or `models.dev` sync, budget window rule update |
| `3.1` | `models: Arc<HashMap<ModelPk, ModelRecord>>`, `models_by_canonical: Arc<HashMap<CanonicalId, ModelPk>>`, `aliases: Arc<HashMap<String, ModelPk>>`, `redirects: Arc<HashMap<String, ModelPk>>`, `routes: Arc<HashMap<ModelPk, Vec<RouteRecord>>>` | `/models` discovery refresh, force-expose toggle, merge/split (`model_pk`), alias CRUD |
| `3.2` | `shares_by_grantee: Arc<HashMap<UserId, Vec<ShareGrant>>>`, `user_route_prefs: Arc<HashMap<(UserId, UpstreamKeyId, ModelPk), RoutePref>>` | Share creation, grantee acceptance, admin approval, cap edit, or revocation (`SHR-5`), personal route override |
| `5.1` | `traceability: Arc<TraceabilityConfig>` | `PUT /admin/api/settings/traceability` (`TRC-1`) |
## Epic 2: Transparent wire proxy, protocol normalizer, provider adapters, and orchestrator v1

Epic 2 builds control-plane CRUD APIs (`2.0`), the SSRF connector (`2.1`), client key auth (`2.2`), header pipelines (`2.3`), and the JSON editor (`2.4`). It also adds the SSE forwarder and `RequestLifecycle` guard (`2.5`), provider adapters (`2.6`), and orchestrator v1 with `SIGTERM` drain (`2.7`). It corresponds to Milestone `M1` (`SRDD Sections 2, 5, 6.3, 6.5, 6.8, 7.1–7.4, 8.2, 12, 21`).

### 2.0 Sub-phase 2.0: Minimal control-plane JSON CRUD for providers, keys, client keys, and settings

This sub-phase exposes the JSON control-plane endpoints for providers, upstream keys, client keys, and runtime settings before UI work in Epic 6. End-to-end tests in Epics 2 through 5 use these endpoints to provision state through the `Control` writer and `Reloader` pipeline (`SRDD Section 14.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.3`, `Section 6.5`, `Section 14.2.1`, `Section 14.2.2`; Requirements `PRV-1`, `PRV-4`, `PRV-5`, `Partial: CLK-1` |
| Crates touched | `crates/store::providers`, `crates/store::client_keys`, `crates/admin::api` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/integration_providers_and_keys.rs`, `#[doc = "Spec: PRV-1, PRV-4, PRV-5, Partial: CLK-1"]`) verifying: (1) `admin` can create global providers and keys while `user` can create personal providers and keys (up to 20 providers and 50 keys per user, `SEC-8`), (2) secrets are sealed with `XChaCha20-Poly1305` (`domain = "upstream_key"`) and never echoed in API responses (`PRV-5`), (3) `providers.share_config_fingerprint` is computed and persisted on every provider insert/update, and (4) every mutation writes an `audit_log` row and updates `ConfigSnapshot` within `1 s`. |
| Atomic commits | 1. `feat(store/2.0): implement provider and upstream_key repositories with share_config_fingerprint and per-user quotas [Spec: PRV-1, PRV-4, PRV-5]` 2. `feat(admin/2.0): add JSON CRUD handlers for providers, upstream keys, client keys, and settings [Spec: PRV-1, PRV-4, PRV-5, Partial: CLK-1]` |
| Risks and resolutions | **Risk:** E2E tests that seed SQLite directly bypass the `Reloader` and `CacheInvalidationEvent` path used in production. **Resolution:** Provision all providers, keys, client keys, and settings through the Sub-phase 2.0 JSON APIs in every integration and E2E test from Epic 2 onward. |

### 2.1 Sub-phase 2.1: Socket-level SSRF `GuardedConnector` and URL validation

This sub-phase implements save-time URL validation and the custom `GuardedConnector` (`tower::Service<Uri>`) that validates both DNS-resolved addresses and IP-literal hosts on every socket connect (`PRV-7`, `SEC-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.3`, `Section 8.2`, `Section 21.8`; Requirements `PRV-7`, `SEC-7` |
| Crates touched | `crates/proxy::ssrf`, `crates/proxy::transport` |
| TDD step (`Red -> Green`) | Write SSRF unit and integration tests (`proxy::ssrf::tests::ip_ranges`, `#[doc = "Spec: PRV-7, SEC-7"]`) verifying that `GuardedConnector` blocks: IPv4/IPv6 literals (`https://127.0.0.1`, `https://[::1]`, `https://169.254.169.254`), RFC 1918 (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`), CGNAT (`100.64.0.0/10`), multicast/reserved (`224.0.0.0/4`, `240.0.0.0/4`, `255.255.255.255/32`), IPv4-mapped IPv6 (`[::ffff:127.0.0.1]`), NAT64 (`64:ff9b::/96`, `64:ff9b:1::/48`), 6to4 (`[2002:a9fe:a9fe::1]` embedding `169.254.169.254`), Teredo/IETF (`2001::/23`, `2001:db8::/32`), discard (`100::/64`), ULA/link-local/site-local/multicast (`fc00::/7`, `fe80::/10`, `fec0::/10`, `ff00::/8`), non-standard integer IP hosts (`https://2130706433` rejected with 422 at save), `http://` schemes, and HTTP 301/302/307/308 redirects. Verify `allow_private_network = 1` (admin-only) and cross-origin `usage_url` rules (`usage_url_cross_origin_ack = true` on unshared personal providers only). |
| Atomic commits | 1. `sec(proxy/2.1): implement socket-level GuardedConnector SSRF filter covering IP literals, 6to4, NAT64, and mapped IPv6 [Spec: PRV-7, SEC-7]` |
| Risks and resolutions | **Risk:** hyper's default connector skips `reqwest::dns::Resolve` when the URL host is an IP literal (`https://127.0.0.1/`). **Resolution:** Enforce `is_blocked(ip)` inside `GuardedConnector::call(uri)` for both `IpAddr` literals and `hickory-resolver` DNS lookups on every socket connect (`SRDD Section 21.8`). |

### 2.2 Sub-phase 2.2: Client key (`llr_...`) auth, rotation grace model, request gates `G1`–`G6`, and listeners

This sub-phase implements client key generation, in-place rotation with `prev_key_hash` and `prev_key_expires_at`, and ordered request gates `G1`–`G6`. It also adds per-IP auth throttling, `auth_failure_counters`, and the `/v1/*`, `/healthz`, `/readyz`, and `/metrics` listeners (`CLK-1`, `CLK-4`, `SEC-3`, `SEC-9`, `SEC-12`, `OBS-1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.4`, `Section 6.5`, `Section 7.4`, `Section 8.2`, `Section 10.0`; Requirements `CLK-1`, `CLK-4`, `API-2`, `SEC-3`, `SEC-9`, `SEC-12`, `OBS-1`, `Partial: CLK-2`, `Partial: CLK-5` |
| Crates touched | `crates/core::client_key`, `crates/store::client_keys`, `crates/proxy::auth`, `crates/server::listeners` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`tests/integration_client_keys.rs`, `#[doc = "Spec: CLK-1, CLK-4, API-2, SEC-3, SEC-9, SEC-12, OBS-1, Partial: CLK-2, Partial: CLK-5"]`) verifying: (1) raw `llr_...` token is returned once, (2) rotating with `grace_period_secs = 300` keeps `prev_key_hash` valid at `+299 s` and rejects it at `+301 s` on `FakeClock`, rotating twice replaces the predecessor, and revoking during grace rejects both hashes within `1 s` (`CLK-4`), (3) ordered gates `G1`–`G6` return deterministic status codes (including 401 when `owner.disabled == 1`), (4) 100,000 bad-token requests are throttled at 20/min per IP (`SEC-12`) and increment `auth_failure_counters` without inserting any `usage_events` rows, (5) non-loopback plaintext data plane logs an error and sets `llmroller_plaintext_data_plane = 1` (or exits `2` when `LLMROLLER_REQUIRE_TLS=true`), and (6) every registered Prometheus metric uses only the closed label allow-list (`provider_kind`, `provider_id`, `protocol`, `endpoint`, `status_class`, `outcome`, `strategy`, `job`, `reason`, `model_bucket`), and `llmroller healthcheck --url http://127.0.0.1:8080/readyz` returns exit code `0` when ready and `1` when `/readyz` is 503. |
| Atomic commits | 1. `feat(core/2.2): implement client key generation, prev_key_hash grace rotation, and request gates G1-G6 [Spec: CLK-1, SEC-3, Partial: CLK-2, Partial: CLK-5]` 2. `feat(proxy/2.2): wire client key auth middleware, auth_failure_counters, and /healthz, /readyz, /metrics listeners [Spec: CLK-4, API-2, SEC-9, SEC-12, OBS-1]` |
| Risks and resolutions | **Risk:** Recording unauthenticated 401 floods in `usage_events` would evict legitimate usage rows from the 10,000-slot raw ring buffer. **Resolution:** Record `G1`–`G3` failures solely in the per-minute `auth_failure_counters` table and enforce `usage_events.client_key_id NOT NULL`. |

### 2.3 Sub-phase 2.3: Request and response header pipelines, smuggling guards, `User-Agent` policy, and header fuzzer

This sub-phase implements HTTP smuggling rejection (`SEC-15`), the four-rule request header pipeline (`API-2`, `API-3`), and the five-step response header pipeline (`Section 12.1.1`). It also implements the `User-Agent` policies and the header parser fuzz target (`SRDD Section 12.1, Section 12.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 12.1`, `Section 12.1.1`, `Section 12.2`, `Section 8.2`; Requirements `API-2`, `API-3`, `SEC-6`, `SEC-8`, `SEC-15` |
| Crates touched | `crates/proto::headers`, `crates/proxy::edge`, `fuzz/fuzz_targets/headers.rs` |
| TDD step (`Red -> Green`) | Write table-driven tests (`proto::headers::tests`, `#[doc = "Spec: API-2, API-3, SEC-6, SEC-8, SEC-15"]`) asserting: (1) on `Own`-tier keys, `x-claude-code-session-id` passes through verbatim and multiple `anthropic-beta` headers preserve their per-name relative order, while `strip_header_tokens` removes configured comma-separated tokens, (2) on `Shared` and `Global` tiers, response pipeline step `R4` strips `openai-organization`, `openai-project`, `anthropic-organization-id`, `x-ratelimit-*`, `anthropic-ratelimit-*`, `cf-ray`, `server`, and `via` while step `R2` removes `content-length`, `content-encoding`, and `set-cookie*`, and (3) requests with both `Content-Length` and `Transfer-Encoding` or duplicate `Host` headers return HTTP 400 (`SEC-15`). Run `fuzz/fuzz_targets/headers.rs` for 60 s in CI. |
| Atomic commits | 1. `sec(proto/2.3): implement HTTP smuggling checks, request header pipeline with strip_header_tokens, and response header pipeline R1-R5 [Spec: API-2, API-3, SEC-8, SEC-15]` 2. `feat(proto/2.3): implement User-Agent policies, canary log scrubber, and headers cargo-fuzz target [Spec: API-3, SEC-6]` |
| Risks and resolutions | **Risk:** Forwarding upstream `content-length` after suppressing an injected usage chunk hangs HTTP/1.1 clients, and forwarding `openai-organization` or `x-ratelimit-*` on shared keys leaks grantor identity. **Resolution:** Enforce the five-step response header pipeline (`R1`–`R5`) at response commit (`SRDD Section 12.1.1`). |

### 2.4 Sub-phase 2.4: Zero-copy surgical JSON span editor, duplicate-key guard, and body-edit fuzzer

This sub-phase implements the top-level JSON span scanner and surgical editor in `crates/proto::body_edit` (`SRDD Section 12.3`). It rejects duplicate top-level keys (`400`), validates UTF-8/object envelopes, JSON-escapes inserted strings, and handles all six `usage_reporting.chat_stream` cases (`API-4`, `TRN-1`, `TRN-2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.3`, `Section 12.3`; Requirements `API-4`, `TRN-1`, `TRN-2` |
| Crates touched | `crates/proto::body_edit`, `fuzz/fuzz_targets/body_edit.rs` |
| TDD step (`Red -> Green`) | Write property-based and security tests (`proto::body_edit::proptests`, `#[doc = "Spec: API-4, TRN-1, TRN-2"]`): (1) any request body with a duplicate top-level key (after JSON key unescaping, such as two `"model"` or two `"stream"` keys), a UTF-8 BOM, a non-object top level, trailing non-whitespace bytes, or non-string `model` / non-boolean `stream` returns HTTP 400 before routing, (2) when routing on an `Own`-tier key with no edits needed, the output `Bytes` pointer is identical to the input `Bytes` (`TRN-1`), (3) random `upstream_model_id` strings containing `"` and `\` are JSON-escaped via `serde_json::to_writer` and inserted members land immediately after `{`, keeping `messages`, `tools`, and `system` byte-identical (`TRN-2`), and (4) explicit `"stream_options": null` or `{"include_usage": false}` (including objects with sibling fields such as OpenAI's `"include_obfuscation": true` or numeric extensions) is replaced in place without creating duplicate keys, while non-boolean `include_usage` or duplicate keys inside `stream_options` return HTTP 400. Run `fuzz/fuzz_targets/body_edit.rs` in CI. |
| Atomic commits | 1. `feat(proto/2.4): implement surgical JSON span editor with duplicate-key rejection, envelope validation, and body_edit fuzzer [Spec: API-4, TRN-1, TRN-2]` |
| Risks and resolutions | **Risk:** A client key restricted to a cheap model sends `{"model":"cheap","model":"expensive"}` to exploit a first-wins vs. last-wins parser differential. **Resolution:** Reject any JSON body containing duplicate top-level keys with HTTP 400 in `scan_top_level` before model resolution. |

### 2.5 Sub-phase 2.5: Unbuffered SSE forwarder, 64 KiB pre-first-frame holding buffer, single-frame filter, and `RequestLifecycle` guard

This sub-phase implements the 64 KiB pre-first-valid-frame holding buffer (`Section 10.5.3`), the zero-buffering SSE forwarder (`PERF-2`, `API-6`), and the 64 KiB usage-chunk filter (`Partial: API-5`). It also adds `async-compression` fallback decoding and the `RequestLifecycle` RAII drop guard (`API-7`, `SRDD Section 12.4, Section 21.1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.1`, `Section 7.3`, `Section 10.5.3`, `Section 12.4`, `Section 21.1`; Requirements `API-6`, `API-7`, `TRN-3`, `Partial: API-5`, `Partial: PERF-2` |
| Crates touched | `crates/proxy::forward`, `crates/proxy::lifecycle`, `crates/proto::sse_frame`, `fuzz/fuzz_targets/sse_frame.rs` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/integration_wire_transparency.rs`, `#[doc = "Spec: API-6, API-7, TRN-3, Partial: API-5, Partial: PERF-2"]`): (1) upstream status and headers are held in a 64 KiB buffer until the first valid non-comment frame is verified free of a non-null error object (explicitly permitting `"error": null` on OpenAI and OpenRouter `/v1/responses` `response.created` frames), then committed through the response header pipeline, (2) each complete SSE frame after commit reaches the client within 1 ms (`Partial: PERF-2`), (3) when `strip_injected_usage_chunk == true`, only a complete frame whose single `data:` JSON has `"choices":[]` and an object `"usage"` is suppressed (`Partial: API-5`) and any partial bytes at stream end flush verbatim, (4) unexpected `content-encoding: gzip|br|deflate|zstd` is decoded transparently in the forwarder, and (5) dropping the client connection after chunk 2 aborts the upstream stream within 100 ms (`API-7`) and invokes `RequestLifecycle::drop` exactly once with `Termination::ClientGone`. |
| Atomic commits | 1. `feat(proxy/2.5): implement pre-first-frame holding buffer, single-frame SSE usage filter, and compression fallback decoder [Spec: API-6, TRN-3, Partial: API-5, Partial: PERF-2]` 2. `feat(proxy/2.5): implement RequestLifecycle RAII completion guard and sse_frame fuzz target [Spec: API-7]` |
| Risks and resolutions | **Risk:** When a client disconnects mid-stream, hyper simply drops the response stream future, leaking permits and skipping usage accounting if finalization is tied to end-of-stream polling. **Resolution:** Own all per-request resources inside `RequestLifecycle` so its `Drop` implementation always finalizes accounting and releases permits (`SRDD Section 21.1`). |

### 2.6 Sub-phase 2.6: Built-in `ProviderAdapter` implementations, 5-class error classifier, and preset TOML files

This sub-phase implements the `ProviderAdapter` trait (`MNT-2`) and the five-class ordered error classifier (`QuotaExhausted`, `RequestRejected`, `PlanRestricted`, `CredentialInvalid`, `Unclassified403`). It also ships the four built-in preset TOML files plus `custom` (`SRDD Section 2.3, Section 10.5.2, Section 11.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.3`, `Section 6.3`, `Section 7.4`, `Section 10.5.2`, `Section 11.2`; Requirements `PRV-2`, `PRV-3`, `MNT-2`, `Partial: PRV-6`, `Partial: API-8` |
| Crates touched | `crates/proto::adapters`, `presets/*.toml` |
| TDD step (`Red -> Green`) | Write adapter contract tests (`proto::adapters::tests`, `#[doc = "Spec: PRV-2, PRV-3, MNT-2, Partial: PRV-6, Partial: API-8"]`) verifying: (1) URL construction and `aux_endpoints` capability declarations (`count_tokens`, `responses_compact`), (2) preset `[aliases]` (`claude-sonnet-4-6 -> claude-sonnet-4.6`) and `[protocol_defaults]` glob mappings, (3) `usage_reporting.chat_stream` (`inject` vs. `native`), (4) Stage-1 `/models` key testing (`Partial: PRV-6`), and (5) ordered five-class error classification over the first 8 KiB of response bodies: `QuotaExhausted` (402/403/429 matching quota substrings), `RequestRejected` (OpenRouter 403 `"flagged"`/`"moderation"` and 400/413/422 passed through without health change), `PlanRestricted` (Command Code 403 `"upgrade_required"` blocking `(K, M, P)`), `CredentialInvalid` (401 or 403 `"invalid_api_key"`), and `Unclassified403`. |
| Atomic commits | 1. `feat(proto/2.6): implement ProviderAdapter trait, 5-class error classifier, and preset TOML files [Spec: PRV-2, PRV-3, MNT-2, Partial: PRV-6, Partial: API-8]` |
| Risks and resolutions | **Risk:** A content-moderation 403 from an aggregator or a plan-restriction 403 on one protocol marks a shared or global key `invalid` for all users. **Resolution:** Classify `RequestRejected` and `PlanRestricted` before `CredentialInvalid` so content rejections never touch key health and plan restrictions only block `(upstream_key_id, canonical_id, protocol)` (`SRDD Section 10.5.2`). |

### 2.7 Sub-phase 2.7: `/v1/*` orchestrator v1, pre-flight body memory budget, graceful `SIGTERM` drain, and early load harness

This sub-phase composes gates `G1`–`G6`, pre-flight body admission (`LLMROLLER_PREFLIGHT_BODY_BUDGET_MB`), single-route selection, and SSE forwarding into `/v1/*` orchestrator v1. It also implements the 8-step `SIGTERM` drain (`REL-1`) and runs the early load harness with a log-normal TTFF stub (`SRDD Section 7.1, Section 21.8`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.3`, `Section 5.5`, `Section 7.1`, `Section 7.2`, `Section 21.1`, `Section 21.8`; Requirements `REL-1`, `SEC-8`, `Partial: PERF-1`, `Partial: PERF-2`, `Partial: PERF-3` |
| Crates touched | `crates/proxy::orchestrator`, `crates/server::shutdown`, `benches/proxy_latency.rs`, `tests/e2e_sigterm_drain.rs` |
| TDD step (`Red -> Green`) | Write end-to-end pass-through, memory-admission, and drain tests (`tests/e2e_sigterm_drain.rs`, `#[doc = "Spec: REL-1, SEC-8, Partial: PERF-1, Partial: PERF-2, Partial: PERF-3"]`): (1) single-provider pass-through succeeds across `/v1/chat/completions`, `/v1/responses`, and `/v1/messages` (streaming and non-streaming), (2) `budget_semaphore.acquire_many(ceil(len / 4096))` queues excess pre-first-byte requests up to `preflight_queue_timeout_ms` (`2,000 ms`), releases the permit the instant the first valid frame commits, and returns `503 gateway_busy` (`Retry-After: 1`) on queue timeout, (3) `SIGTERM` runs the 8-step shutdown sequence (`/readyz -> 503` for 5 s grace, stop accept, 60 s stream drain, `Termination::ShutdownCut` on stragglers, stop jobs, flush queues, `wal_checkpoint(TRUNCATE)`, release lock), and (4) the load benchmark against a log-normal TTFF stub (median 2 s, p95 8 s) verifies sustained req/s across 64 KiB, 512 KiB, and 2 MiB bodies. |
| Atomic commits | 1. `feat(proxy/2.7): implement /v1/* orchestrator v1 and cgroup-aware pre-flight body budget semaphore [Spec: SEC-8, Partial: PERF-1, Partial: PERF-2, Partial: PERF-3]` 2. `feat(server/2.7): implement 8-step graceful SIGTERM drain, job panic supervisor, and log-normal TTFF load harness [Spec: REL-1]` |
| Risks and resolutions | **Risk:** Holding buffered request bodies in a tiny 128 MiB cap with immediate rejection throttles 1 MiB coding requests with 5 s TTFF to ~25 req/s. **Resolution:** Size `LLMROLLER_PREFLIGHT_BODY_BUDGET_MB` to `min(1024, floor(0.40 * memory_limit_mb))` (819 MiB on a 2 GB host) with a 2,000 ms admission queue (`SRDD Section 7.1`). |
## Epic 3: Core pricing/windows/accumulators, model catalog, sharing, pluggable routing strategies, pinning, and orchestrator v2

Epic 3 builds the pure-core pricing, window, and monthly accumulator engines (`3.0a`–`3.0c`) first so routing and shares use real budget reservations. It then builds the catalog (`3.1`), sharing (`3.2`), session resolution (`3.3`), eligibility (`3.4`), smooth WRR (`3.5`), health/breakers (`3.6a`), `PinStore` (`3.6b`), and orchestrator v2 (`3.7`). It corresponds to Milestone `M2` (`SRDD Sections 4, 6.4–6.8, 10, 11, 13.1, 13.2, 21`).

### 3.0a Sub-phase 3.0a: Pure `crates/core::pricing` engine, fallback rates, and binary-aware input estimator

This sub-phase implements 128-bit integer micro-USD cost calculation (`SRDD Section 13.1`), non-overlapping context tiers, half-open UTC schedules, `unpriced_fallback_rates`, and the streaming `estimate_input_tokens` tokenizer in `crates/core::pricing` (`Partial: USG-3`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.9`, `Section 9.3.2`, `Section 13.1`; Requirements `Partial: USG-3` |
| Crates touched | `crates/core::pricing`, `presets/*.toml` |
| TDD step (`Red -> Green`) | Write property-based and unit tests (`core::pricing::proptests`, `#[doc = "Partial: USG-3"]`) verifying: (1) finite provider-reported `usage.cost` in `[0.0, 1000.0]` converts via `(cost * 1_000_000.0).round() as i64` (`cost_source = 'provider'`), while negative, `NaN`, or `> 1000.0` values fall back to the price book, (2) `i128` ceiling division never overflows up to `10^9` tokens, (3) non-overlapping context tiers (`total_input >= min_total_input_tokens`) and half-open `[start, end)` UTC schedules apply deterministically at request start timestamp `ts`, (4) missing price rows always charge `unpriced_fallback_rates` (`$3/$15/$0.30/$3.75 per MTok`, `cost_source = 'fallback_rate'`, never `NULL` or `$0`), and (5) `estimate_input_tokens(body)` counts `> 1,024`-byte strings under `"data"`, `"image_url"`, `"url"` (`"data:"`), `"file_data"`, or `"b64_json"` as `1,600` tokens per binary block so a 4 MiB screenshot estimates `<= 2,000` tokens plus text. |
| Atomic commits | 1. `feat(core/3.0a): implement i128 micro-USD pricing engine, fallback rates, and binary-aware token estimator [Partial: USG-3]` |
| Risks and resolutions | **Risk:** Estimating input tokens as `body_bytes / 3` treats a 4 MiB base64 screenshot as 1.3M text tokens, exhausting a 5 h budget window on one request. **Resolution:** Use the streaming JSON string tokenizer `estimate_input_tokens` that charges a flat `1,600` tokens per binary block (`SRDD Section 13.1`). |

### 3.0b Sub-phase 3.0b: Pure `crates/core::windows` rings, anchored calendars, decaying observations, and `Reservation` guard

This sub-phase implements generalized 360-bucket rolling rings, `anchored_fixed`, `anchored_calendar_month`, and `anchored_provider` windows in `crates/core::windows`. It also implements decaying `UpstreamObservation` projections, `ManualOffset`, and cache-aware per-candidate `try_reserve` / `Reservation` (`LIM-1`, `SRDD Section 13.2, Section 21.4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.9`, `Section 13.2`, `Section 13.2.1`, `Section 21.4`; Requirements `LIM-1`, `Partial: RTE-3`, `Partial: USG-6` |
| Crates touched | `crates/core::windows` |
| TDD step (`Red -> Green`) | Write deterministic `FakeClock` tests (`core::windows::tests`, `#[doc = "Spec: LIM-1, Partial: RTE-3, Partial: USG-6"]`): (1) rolling rings (`5h` -> 300 x 1-min buckets, `7d` -> 360 x 28-min buckets, `30d` -> 360 x 120-min buckets) and `ManualOffset` age out at exact bucket boundaries, (2) `anchored_calendar_month` advances a 31 January billing anchor to `28/29 February` and then `31 March` via `next_reset`, (3) an `UpstreamObservation` (90% at 12:00) projects `o.used_micro_usd + max(0, local - o.local_at_observation)` and expires after `MAX_OBSERVATION_AGE_MS` (`30 min`) or `resets_at_ms` so a key never stays stuck when `/usage` polling fails, and (4) 50 concurrent `try_reserve` calls at `97.9%` of a `$10` window using cache-aware estimates (`observed_cache_read_ratio_ewma`, clamped to `$0.001..$2.00`) never admit spend exceeding `$10 * 0.98`, rolling back in fixed counter order on failure and posting actual cost before decrementing `reserved_in_flight` on drop. |
| Atomic commits | 1. `feat(core/3.0b): implement generalized rolling rings, calendar-month and provider-anchored windows, and decaying UpstreamObservation [Spec: LIM-1, Partial: USG-6]` 2. `feat(core/3.0b): implement cache-aware per-candidate Reservation CAS ledger with ordered rollback [Spec: LIM-1, Partial: RTE-3]` |
| Risks and resolutions | **Risk:** A rolling baseline synced from `/usage` never ages out when subsequent `/usage` polls fail, permanently excluding a healthy key. **Resolution:** Represent upstream `/usage` data as an `UpstreamObservation` that expires at `min(resets_at_ms, observed_at_ms + 30 min)`, falling back cleanly to the local ring sum (`SRDD Section 13.2`). |

### 3.0c Sub-phase 3.0c: Pure `crates/core::accumulators` UTC calendar-month accumulators

This sub-phase implements the UTC calendar-month accumulators (`DashMap<(UtcYearMonth, Id), MonthlyAccumulator>`) for `provider_shares.max_monthly_micro_usd`, `users.global_budget_micro_usd_month`, and `client_keys.budget_micro_usd_month` (`CLK-5`, `USR-5`, `Partial: SHR-2`, `SRDD Section 13.2.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.2`, `Section 6.4`, `Section 6.5`, `Section 10.0`, `Section 13.2.2`; Requirements `USR-5`, `CLK-5`, `Partial: SHR-2` |
| Crates touched | `crates/core::accumulators` |
| TDD step (`Red -> Green`) | Write unit tests (`core::accumulators::tests`, `#[doc = "Spec: USR-5, CLK-5, Partial: SHR-2"]`) verifying: (1) a request reserved at `23:59:59.900 UTC` on the last day of month `M` and completed at `00:00:04 UTC` in `M+1` posts its actual cost and releases its reservation in month `M`'s accumulator (`reservation_month`), (2) the sweeper prunes entries for months `< M+1` with `reserved == 0` one hour after rollover, (3) gate `G7` returns `429 client_key_budget_exhausted` (`Retry-After` = seconds until next UTC month) when `client_key.budget_micro_usd_month` is exhausted, and (4) lowering a cap emits `CacheInvalidationEvent::CapChanged`. |
| Atomic commits | 1. `feat(core/3.0c): implement reservation-month-keyed UTC monthly accumulators, rollover pruning, and gate G7 [Spec: USR-5, CLK-5, Partial: SHR-2]` |
| Risks and resolutions | **Risk:** Midnight-straddling streams that reserve in month `M` and release in month `M+1` leak `reserved` in month `M` forever. **Resolution:** Store `reservation_month: UtcYearMonth` inside `Reservation` and post both actual cost and reservation release to that exact month key (`SRDD Section 13.2.2`). |

### 3.1 Sub-phase 3.1: `/models` discovery, surrogate `model_pk`, collision-safe normalization, and exposure rules

This sub-phase implements `/models` polling (`MDL-1`), the surrogate `model_pk` catalog with `model_redirects`, and digit-dot-preserving normalization (`MDL-2`). It also adds `normalize_keep_vendor`, `merge_state` review, alias attachment (`MDL-6`), the `active`/`missing`/`retired` lifecycle, `union`/`intersection` exposure (`MDL-3`..`MDL-5`), and `GET /v1/models` (`MDL-7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.6`, `Section 10.6`, `Section 14.2.2`, `Section 19.1 (ADR-7)`; Requirements `MDL-1`..`MDL-7` |
| Crates touched | `crates/core::catalog`, `crates/proto::discovery`, `crates/store::models`, `crates/proxy::models` |
| TDD step (`Red -> Green`) | Write unit, property, and integration tests (`core::catalog::tests::normalization_and_exposure`, `#[doc = "Spec: MDL-1, MDL-2, MDL-3, MDL-4, MDL-5, MDL-6, MDL-7"]`) asserting: (1) `normalize_model_id` is idempotent, preserves every dot between two ASCII digits (`anthropic/claude-sonnet-4.6` -> `claude-sonnet-4.6`, `openai/gpt-5.4-pro:free` -> `gpt-5.4-pro-free`, `mimo/mimo_v2.6_pro` -> `mimo-v2.6-pro`), keeps `:extended` as `-extended`, and rejects IDs outside `^[!-~]{1,256}$`, (2) one key listing both `vendor-a/x` and `vendor-b/x` yields distinct routes `vendor-a--x` and `vendor-b--x`, while two keys listing different `vendor_prefix` values set `merge_state = 'pending_review'`, (3) preset alias `claude-sonnet-4-6-20260514 -> claude-sonnet-4.6` attaches the dated Command Code route directly to `claude-sonnet-4.6`'s `model_pk`, and ambiguous raw `upstream_model_id` requests return HTTP 409, (4) merging `a -> b` repoints `model_pk` across routes, preferences, budget windows, pins, and response links and inserts `model_redirects(a -> b.model_pk)` without cascading deletes, (5) responses exceeding `2 MiB` or `1,000` models mark the refresh `partial` without marking absent routes `missing`, (6) `global_exposure_mode` defaults to `"union"` (and `"intersection"` ignores global providers with zero usable keys), and (7) personal discovery sets `origin = 'personal'` (capped at 2,000 models per user) and upgrades to `'global'` when discovered globally. |
| Atomic commits | 1. `feat(core/3.1): implement digit-dot-preserving normalization, surrogate model_pk merge/split, and alias resolution [Spec: MDL-2, MDL-3, MDL-4, MDL-5, MDL-6]` 2. `feat(server/3.1): implement /models discovery lifecycle (active/missing/retired, partial guard) and GET /v1/models [Spec: MDL-1, MDL-7]` |
| Risks and resolutions | **Risk:** Using mutable `canonical_id` strings with `ON DELETE CASCADE` deletes active pins, response links, and route budget windows during a model merge. **Resolution:** Reference `models(model_pk)` (`ON DELETE RESTRICT`) from all dependent tables and record old names in `model_redirects` (`SRDD Section 9.1, Section 10.6`). |

### 3.2 Sub-phase 3.2: Personal provider and key sharing (`provider_shares`), consent workflow, and overlap prevention

This sub-phase implements personal share creation (`SHR-1`), partial unique indexes preventing share overlap (`409 share_overlap`), and deterministic `effective_share(U, K)`. It also adds grantee acceptance (`accepted_config_fingerprint`), custom-provider admin approval, model/cap filters (`SHR-2`..`SHR-4`), and instant revocation (`SHR-5`, `SRDD Section 4.4, Section 10.1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.4`, `Section 4.6`, `Section 6.4`, `Section 8.2`, `Section 10.1`; Requirements `SHR-1`..`SHR-5`, `USR-2`..`USR-4`, `SEC-17` |
| Crates touched | `crates/core::shares`, `crates/store::shares` |
| TDD step (`Red -> Green`) | Write unit and SQLite integration tests (`tests/integration_personal_sharing.rs`, `#[doc = "Spec: SHR-1, SHR-2, SHR-3, SHR-4, SHR-5, USR-2, USR-3, USR-4, SEC-17"]`) verifying: (1) Alice (`user`) shares her personal key with Bob (`allowed_models = ["deepseek-v4.1-flash"]`, `max_monthly_micro_usd = 5_000_000`), and the share remains ineligible until Bob calls `accept` echoing `providers.share_config_fingerprint` (and an admin calls `approve` when `kind == 'custom'`), (2) creating an overlapping provider-level and key-level share for Bob returns HTTP 409 `share_overlap`, (3) modifying any routing-relevant field in `providers.config_json` (`base_url`, `paths`, `auth`, `add_headers`, `usage_profile`) changes `share_config_fingerprint` and suspends the share within `1 s`, (4) disabling Alice (`users.disabled = 1`) emits `OwnerDisabled(alice_id)` and makes her shared routes ineligible within `1 s`, and (5) with Bob's client key set to `personal_only`, Bob routes `deepseek-v4.1-flash`, gets HTTP 404 `model_not_found` on unshared models, and cannot overshoot the `$5.00` monthly cap under 50 concurrent streams (`try_reserve` via `3.0b`/`3.0c`). |
| Atomic commits | 1. `feat(core/3.2): implement effective_share resolution, share_config_fingerprint consent, and monthly cap reservation [Spec: SHR-1, SHR-2, SHR-3, SHR-4, USR-2, USR-3, USR-4]` 2. `feat(store/3.2): implement provider_shares repository with overlap indexes and revocation invalidation [Spec: SHR-5, SEC-17]` |
| Risks and resolutions | **Risk:** A grantor shares a `custom` provider pointing to a relay they control, silently capturing a grantee's prompts under default `personal_first`. **Resolution:** Place `Own` strictly ahead of `Shared` in `personal_first`, enforce explicit grantee acceptance displaying the destination host (`accepted_config_fingerprint`), and enforce admin approval (`admin_approved_at`) for shares of `custom` providers (`SRDD Section 4.4, Section 8.2 SEC-16`). |

### 3.3 Sub-phase 3.3: Eight-step inbound session ID resolver, canonical Step-8 fingerprint, 21-slot detector, and salted affinity hints

This sub-phase implements the eight-step session ID precedence (`SRDD Section 11.1`) and the canonical byte-sliced Step-8 fallback fingerprint (`canon(SYS)` and `canon(USER1)`). It also adds the 21-slot exact `ConstantSessionDetector` (`Section 11.1.1`) and `Shared`/`Global` carrier stripping with salted affinity hints (`RTE-1`, `SEC-16`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.7`, `Section 8.2`, `Section 11.1`, `Section 11.1.1`, `Section 11.2`; Requirements `RTE-1`, `SEC-16` |
| Crates touched | `crates/core::session`, `crates/proto::affinity`, `fuzz/fuzz_targets/step8_fingerprint.rs` |
| TDD step (`Red -> Green`) | Write table-driven, property, and fuzz tests (`core::session::tests`, `#[doc = "Spec: RTE-1, SEC-16"]`) verifying: (1) all eight precedence levels resolve in order, values `> 256` bytes or containing non-printable ASCII are ignored and fall through to the next step, and a request with only `previous_response_id = "resp_X"` and no `response_links` row resolves to the Step-8 fingerprint hash (never `hmac(owner, "resp_X")`), (2) Step-8 extraction slices `Vec<u8>` bytes at `4096` (never panicking on a 3-byte UTF-8 character straddling bytes `4095..4097`), strips `cache_control` and `citations` so moving `cache_control` between turns yields the exact same `raw_session_id`, and hashes a 10 MiB base64 image in `< 50 ms`, (3) 30 Claude Code subagent openings under `x-claude-code-session-id` (or with `x-claude-code-agent-id`) never trip `ConstantSessionDetector`, whereas 21 distinct openings on `x-session-id` within 59 minutes set `constant_session_flagged_at` (and 21 spread over 61 minutes do not), and (4) routing on `Shared` or `Global` keys strips all inbound session headers/body fields and writes only the salted token `llr_<base32(HMAC(session_hmac_secret, "affinity.v1 " || owner_user_id || " " || raw_session_id))[0..26]>`. |
| Atomic commits | 1. `feat(core/3.3): implement eight-step session resolver, byte-safe canonical Step-8 fingerprint, and 21-slot exact detector [Spec: RTE-1]` 2. `sec(proto/3.3): implement Shared/Global session carrier stripping, salted affinity hints, and step8_fingerprint fuzzer [Spec: SEC-16]` |
| Risks and resolutions | **Risk:** Slicing `&str[..4096]` panics on multi-byte UTF-8 characters, and counting subagent prompts in an approximate HyperLogLog detector falsely disables header pinning for Claude Code users. **Resolution:** Slice canonical `Vec<u8>` byte buffers and run the 21-slot exact `ConstantSessionDetector` only on generic sources (steps 4–7 without `x-claude-code-agent-id`), defaulting to `constant_session_mode = 'flag'` (`SRDD Section 11.1.1`). |

### 3.4 Sub-phase 3.4: Request gates `G1`–`G7`, candidate checks `C1`–`C8`, and three-tier `routing_scope` pool selector

This sub-phase implements the candidate eligibility filter (`C1`–`C8` in `SRDD Section 10.1`). It also implements the five `routing_scope` pool selectors (`personal_first`, `blended`, `personal_only`, `own_only`, `global_only`) over tiers `Own`, `Shared`, `Global`, and `Blended` (`SRDD Section 10.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.5`, `Section 6.5`, `Section 6.7`, `Section 10.0`, `Section 10.1`, `Section 10.2`; Requirements `CLK-3`, `RTE-3` |
| Crates touched | `crates/core::eligibility` |
| TDD step (`Red -> Green`) | Write exhaustive unit tests (`core::eligibility::tests`, `#[doc = "Spec: CLK-3, RTE-3"]`) toggling each of `G1`–`G7` and `C1`–`C8` one by one: `C1` enabled flags, `C2` tier authorization (`Own`, `Shared` via `effective_share` with `allowed_models IS NULL` admitting all models, `Global`), `C3` personal owner `users.disabled == 0`, `C4` admin kill switch `models.exposed IS DISTINCT FROM 0` across all tiers plus `merge_state` check, `C5` protocol support (and `route_protocol_blocks`), `C6` key/route health, `C7` origin breaker (`3.6a`), and `C8` `try_reserve` admission (`3.0b`/`3.0c`). Verify that under `personal_first` (`[Own] -> [Shared] -> [Global]`), 1,000 new sessions for Bob (who has both an eligible own key and an accepted shared key from Alice) all route to Bob's `Own` key until Bob's key is exhausted, then route to Alice's `Shared` key, and then to `Global`. Verify `429 monthly_cap_exhausted` vs. `503 no_eligible_route` status codes. Run `cargo mutants` on `crates/core::eligibility`. |
| Atomic commits | 1. `feat(core/3.4): implement candidate checks C1-C8 and three-tier routing_scope pool selector [Spec: CLK-3, RTE-3]` |
| Risks and resolutions | **Risk:** Merging `Own` and `Shared` keys into a single first-choice pool causes grantees to drain a teammate's shared quota while their own personal subscription sits idle. **Resolution:** Order `personal_first` as `[Own]`, then `[Shared]`, then `[Global]`, and record `pool_tier` (`0..=3`) on every selected pin (`SRDD Section 10.2`). |

### 3.5 Sub-phase 3.5: Pluggable `RouteSelector` strategy engine (`SWRR`, `NaiveRR`, `WeightedRR`, `Random`, `WeightedRandom`, `P2C`), strict tiers, and headroom scaling

This sub-phase implements the `RouteSelector` trait and `RoutingStrategy` engine (`smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, `power_of_two_choices`) scoped per `WrrGroupKey` (`SRDD Section 10.3`). It also implements `WRR_MEMBER_EVICT_MS` (`300,000 ms`), 24-hour group GC, `strict` tier filtering, and `headroom` basis-point scaling (`RTE-4`..`RTE-6`, `RTE-13`, `RTE-14`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 3.1 (G2)`, `Section 6.7`, `Section 10.3`, `Section 21.3`; Requirements `RTE-4`, `RTE-5`, `RTE-6`, `RTE-13`, `RTE-14` |
| Crates touched | `crates/core::strategy`, `crates/core::wrr` |
| TDD step (`Red -> Green`) | Write the worked-example test (`SRDD Section 10.3.4`) and the pluggable routing strategy `proptest`, statistical, and concurrency suites (`core::strategy::proptests` and `core::wrr::proptests`, `#[doc = "Spec: RTE-4, RTE-5, RTE-6, RTE-13, RTE-14"]`): (1) `smooth_weighted_rr` and `weighted_rr`: fixed route set over 100,000 picks stays within `0.5%` of `W_eff_i / sum(W_eff)` with SWRR max consecutive run `<= ceil(max(W)/min(W))`, and varying candidate subsets (dropping up to 50% of routes for `< 300 s`) preserve `sum(current over members) == 0` without clamping and stay within `1.0%` of subset-relative weights (`G2`), (2) `naive_rr`: exact unweighted cyclic sequence `0, 1, ..., N-1` over `K * N` selections regardless of configured route weights, plus `AtomicU64` wrap-around from `u64::MAX - 5` under 8-thread contention, (3) `uniform_random` (`random`) and `weighted_random`: seeded `DeterministicRng(42)` produces identical reproducible sequences, `weighted_random` cumulative-weight binary search (`partition_point`) matches exact weight intervals, and over 100,000 picks empirical frequencies stay within `1.0%` of target probabilities and pass a Pearson chi-squared test (`p > 0.001`), (4) `power_of_two_choices` (`p2c`): routes with higher `in_flight` or `ewma_ttfb_ms` receive proportionally fewer picks than idle low-latency routes of equal weight, (5) User B's `priority_override = 1000` on a shared route never alters User C's empirical shares, and (6) 8 OS threads x 10,000 concurrent picks across all six strategies complete with zero torn reads or global RNG lock contention, and idle groups are evicted after 24 h on `FakeClock`. |
| Atomic commits | 1. `feat(core/3.5): define RouteSelector trait, RoutingStrategy enum, and per-group SmoothWeightedRr and WeightedRr selectors [Spec: RTE-4, RTE-5, RTE-6, RTE-13, RTE-14]` 2. `feat(core/3.5): implement NaiveRr, UniformRandom, WeightedRandom binary search, and PowerOfTwoChoices selectors with statistical proptests [Spec: RTE-14]` |
| Risks and resolutions | **Risk:** Sharing one global atomic counter or global `Mutex<Rng>` across threads causes cross-tenant weight interference and lock contention at 500 req/s. **Resolution:** Scope `WrrGroup` and `NaiveRrGroup` per `(user_id, model_pk, protocol, pool_tier)` and draw random numbers from a lock-free thread-local `ChaCha12Rng` seeded from `EntropySource` (`SRDD Section 10.3.2, Section 21.3`). |

### 3.6a Sub-phase 3.6a: Key and route health state machine (`health_version`), credential strike verification, and `OriginBreaker`

This sub-phase implements the versioned key/route health state machine in `RuntimeState`, the credential strike verifier, and model-missing 404 route availability. It also implements the per-origin circuit breaker (`OriginBreaker` keyed by `(scheme, host, port, allow_private_network)`) (`API-8`, `REL-5`, `SRDD Section 10.5`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.8`, `Section 7.2`, `Section 10.5`, `Section 10.5.1`, `Section 21.5`; Requirements `API-8`, `REL-5` |
| Crates touched | `crates/core::health`, `crates/core::breaker` |
| TDD step (`Red -> Green`) | Write unit tests (`core::health::tests`, `#[doc = "Spec: API-8, REL-5"]`) verifying: (1) `CredentialInvalid` marks a key `invalid` only when strikes arrive from `>= 2` distinct `client_key_id`s within 10 minutes or when a single-flight `GET /models` verification probe returns 401/403 (while a 2xx probe clears strikes), (2) OpenRouter 403 `"input was flagged"` three times from one client key leaves the key `healthy` and returns 403 to the caller, (3) Command Code 403 `"upgrade_required"` inserts a `route_protocol_blocks` entry for `(K, M, messages)` while leaving `(K, M, chat)` eligible, (4) `POST /v1/messages/count_tokens` returning 404 sets `aux_unsupported(K, count_tokens)` for 6 h without touching `route_health`, whereas an inference 404 matching `model_not_found_substrings` sets `route_health = unavailable` for 10 min, (5) `OriginBreaker` opens on 5 consecutive host failures (`Connect`, `Tls`, `WriteStalled`, or 502/503/504 before first valid frame) or `>= 50%` host failure ratio over `>= 20` requests in six 10 s buckets, and closes on any HTTP response to its `HalfOpen` probe, and (6) `health_version` prevents stale data-plane `HealthWrite` items from overwriting a newer control-plane `reset-health`. |
| Atomic commits | 1. `feat(core/3.6a): implement versioned key/route health machine, credential strike verifier, and per-origin circuit breaker [Spec: API-8, REL-5]` |
| Risks and resolutions | **Risk:** A single caller sending content that triggers an upstream 403 or a single 404 on `/v1/messages/count_tokens` disables a shared or global key for hours. **Resolution:** Mandate two distinct client keys or a failing `/models` probe for `invalid`, and restrict `route_health = unavailable` to inference 404s matching `model_not_found_substrings` (`SRDD Section 10.5`). |

### 3.6b Sub-phase 3.6b: `PinStore` state machine, durable `response_links` session inheritance, and pre-first-byte failover loop

This sub-phase implements `PinStore` (`DashMap<PinKey, Arc<Mutex<PinEntry>>>` driven by `Clock`), race-free loser adoption on concurrent failover, and `repin_on_personal_recovery`. It also implements durable `response_links` with `session_hash` inheritance (`Section 11.1.2`), `RoutingConstraint`, normative timeouts, and terminal error selection (`RTE-2`, `RTE-7`..`RTE-12`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.7`, `Section 10.4`, `Section 10.5`, `Section 11.1.2`, `Section 21.2`; Requirements `RTE-2`, `RTE-7`..`RTE-12`, `CLK-2` |
| Crates touched | `crates/core::pins`, `crates/store::pins`, `crates/proxy::failover` |
| TDD step (`Red -> Green`) | Write concurrency, state-machine, and failover integration tests (`tests/integration_routing_and_failover.rs`, `#[doc = "Spec: RTE-2, RTE-7, RTE-8, RTE-9, RTE-10, RTE-11, RTE-12, CLK-2"]`): (1) 100 concurrent turn-1 requests for a new session select one key and write one confirmed `pins` row on first valid frame, (2) 10 concurrent turn-1 requests where `K1` returns 429 (`Retry-After: 30`) all coalesce onto the **same** failover winner `K2` via loser adoption (`ticket.generation < gen`), with zero calls to any third key `K3`, (3) a provisional pin with all holders cancelled expires after `50 s` on `FakeClock`, and an admin pin move during an in-flight stream is not reverted by a stale `FirstValidFrame`, (4) when `PinStore.len() >= max_active_pins`, new sessions route unpinned with `pin_outcome = 'unpinned_capacity'` without evicting confirmed pins, (5) `repin_on_personal_recovery` keeps a session on `Global` at `60 s` idle and repins to a recovered `Own` key at `301 s` idle (`pin_outcome = 'repin_recovery'`), (6) three chained Responses turns carrying `previous_response_id` inherit the parent `session_hash` (`Section 11.1.2`), enqueue durable `response_links` at `response.created`, return `429 rate_limit_error` without failing over when the linked `HardKey(K)` is cooling down (`RTE-12`), and update a disagreeing `prompt_cache_key` pin to `K`, and (7) non-streaming calls up to `600 s` and SSE streams sending `: OPENROUTER PROCESSING` comments every `5 s` for `90 s` succeed on a single try without duplicate-billing failover. |
| Atomic commits | 1. `feat(core/3.6b): implement Clock-driven PinStore mutex state machine, loser adoption, and recovery repin [Spec: RTE-2, RTE-7, RTE-8, RTE-10, RTE-11, CLK-2]` 2. `feat(proxy/3.6b): implement durable response_links session inheritance, RoutingConstraint failover loop, and terminal error selector [Spec: RTE-9, RTE-12]` |
| Risks and resolutions | **Risk:** A 30 s TTFB failover resends slow long-context or non-streaming requests that the first upstream is already computing, billing the user twice. **Resolution:** Never fail over on non-streaming requests or after `first_valid_frame_timeout_ms` (`180 s`), count SSE keep-alive comments as progress, and gate `first_activity_timeout_ms` (`60 s`) failover on `provider.config.timeouts.timeout_failover == true` (default `false`, `SRDD Section 10.5.2`). |

### 3.7 Sub-phase 3.7: `/v1/*` orchestrator v2 (full routing, pinning, response links, and failover)

This sub-phase upgrades the `/v1/*` handler from single-route selection (`2.7`) to the full ten-step routing, pinning, response-link, and failover lifecycle (`SRDD Section 5.3`). It also re-runs the load benchmark suite.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.3`, `Section 10.3`, `Section 10.4`, `Section 10.5`, `Section 14.1`; Requirements `RTE-1`..`RTE-14`, `Partial: API-1`, `Partial: PERF-1` |
| Crates touched | `crates/proxy::orchestrator`, `tests/integration_routing_and_failover.rs` |
| TDD step (`Red -> Green`) | Write end-to-end lifecycle tests (`tests/integration_routing_and_failover.rs`, `#[doc = "Spec: RTE-1, RTE-2, RTE-3, RTE-4, RTE-5, RTE-6, RTE-7, RTE-8, RTE-9, RTE-10, RTE-11, RTE-12, RTE-13, RTE-14, Partial: API-1, Partial: PERF-1"]`) verifying hot-reload switching between `smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, and `power_of_two_choices` (preserving stored weights and existing confirmed pins) and exercising `POST /v1/chat/completions`, `POST /v1/responses`, `GET`/`DELETE /v1/responses/{id}`, `/input_items`, `/cancel`, `POST /v1/responses/compact`, `POST /v1/messages`, and `POST /v1/messages/count_tokens` across multi-user personal, shared, and global pools, verifying terminal error selection (three 429 tries with `Retry-After` 7, 3, 12 return 429 with `Retry-After: 3`) and re-running the `2.7` load benchmark. |
| Atomic commits | 1. `feat(proxy/3.7): integrate full router, PinStore, response_links, and failover into /v1/* orchestrator v2 [Spec: RTE-1, RTE-2, RTE-3, RTE-12, Partial: API-1, Partial: PERF-1]` |
| Risks and resolutions | **Risk:** Auxiliary endpoints (`POST /v1/messages/count_tokens`) fail Check `C8` when a key's budget window is at 100%, breaking free token counting in Claude Code. **Resolution:** Classify `count_tokens` as `EndpointKind::Auxiliary` (skipping `C8`) and answer locally with 404 `not_found_error` when all eligible keys have `aux_unsupported` (`SRDD Section 10.5.2`). |
## Epic 4: Usage taps, three-queue batching writer, 5-minute & hourly rollups, orchestrator v3, and retention

Epic 4 builds the inline SSE and JSON token extractors (`4.1`), the price-book APIs (`4.2`), and the three-queue batching SQLite writer (`4.3`). It also builds `/v1/*` orchestrator v3 (`4.6`) and the retention sweeper and WAL checkpoint supervisor (`4.7`). Pure-core pricing, window rings, and monthly accumulators were built in Sub-phases `3.0a`–`3.0c`, and startup window reconstruction plus source `/usage` polling live in `big-bang-phases-4b.md`. Epic 4 corresponds to Milestone `M3` (`SRDD Sections 2.5, 6.8, 6.9, 7.2, 9, 12.5, 13.1, 21`).

### 4.1 Sub-phase 4.1: Inline `SseUsageTap`, `JsonUsageExtractor`, unified cache subtraction, and `sse_tap` fuzzer

This sub-phase extracts token counts across `chat`, `responses`, and `messages` inline in `poll_next` (`SRDD Section 12.4.1, Section 12.5`) and subtracts `cache_read` and `cache_write` (`USG-2`). It also verifies injected usage chunk capture (`API-5`), accumulates decoded output text for aborted streams (`USG-3`), and adds the `sse_tap` fuzzer (`USG-1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.5`, `Section 6.8`, `Section 6.9`, `Section 12.4.1`, `Section 12.5`; Requirements `API-5`, `USG-1`, `USG-2` |
| Crates touched | `crates/proto::sse_tap`, `crates/proto::json_usage`, `fixtures/sse/*`, `fuzz/fuzz_targets/sse_tap.rs` |
| TDD step (`Red -> Green`) | Write golden fixture and fuzz tests (`proto::sse_tap::golden_tests`, `#[doc = "Spec: API-5, USG-1, USG-2"]`) replaying recorded streams for OpenAI `chat`, OpenAI `responses` (`response.created`, `response.completed`, `response.incomplete`, `response.failed`), Anthropic `messages` (`message_start`, `message_delta`, `message_stop`), OpenRouter `usage.cost`, and truncated streams: (1) assert `input_uncached = prompt_cache_miss_tokens` when present, else `max(0, prompt_tokens - cache_read - cache_write)` on `chat` and `max(0, input_tokens - cache_read - cache_write)` on `responses`, while `messages` uses the last observed `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, and `output_tokens` across `message_start` and `message_delta`, (2) assert that an injected `include_usage` chunk (`"choices":[]`) is captured by `SseUsageTap` and omitted from the client stream (`API-5`), and (3) assert that an aborted stream with `10 KiB` of decoded delta text estimates `2,500` output tokens (`ceil(decoded_text_bytes / 4)`). Run `fuzz/fuzz_targets/sse_tap.rs` in CI. |
| Atomic commits | 1. `feat(proto/4.1): implement inline SseUsageTap and JsonUsageExtractor with unified cache_read/cache_write subtraction and sse_tap fuzzer [Spec: API-5, USG-1, USG-2]` |
| Risks and resolutions | **Risk:** Subtracting only `cached_tokens` on OpenRouter-routed Anthropic `chat` calls double-charges `cache_write_tokens`, and estimating aborted output from raw SSE framing inflates token counts by 3–5x. **Resolution:** Subtract both `cache_read` and `cache_write` on `chat` and `responses`, and estimate aborted output strictly from `decoded_text_bytes` accumulated from content, reasoning, and tool-argument deltas (`SRDD Section 12.5, Section 13.1`). |

### 4.2 Sub-phase 4.2: Body-based price book APIs and opt-in `models.dev` price catalog sync

This sub-phase connects the `crates/core::pricing` engine (`3.0a`) to the `prices` table and builds the opt-in `models.dev` catalog importer (`USG-3`). It also implements the body-based price upsert endpoints (`PUT /admin/api/prices` and `PUT /api/user/providers/{id}/prices`) supporting `/` in `upstream_model_id` (`SRDD Section 14.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.9`, `Section 9.1`, `Section 13.1`, `Section 14.2.1`, `Section 14.2.2`; Requirements `USG-3`, `Partial: PRIV-1` |
| Crates touched | `crates/store::prices`, `crates/admin::prices` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/integration_price_book.rs`, `#[doc = "Spec: USG-3, Partial: PRIV-1"]`) verifying: (1) `PUT /admin/api/prices` and `PUT /api/user/providers/{id}/prices` accept vendor-prefixed IDs containing `/` (such as `deepseek/deepseek-v4.1-flash`) in the JSON body and work behind a reverse proxy that decodes `%2F`, (2) `ConfigSnapshot` keys prices by `(provider_id_or_empty, provider_kind, upstream_model_id)` and resolves provider-specific overrides before preset defaults `''`, (3) overlapping tiers or overlapping UTC schedule minute ranges are rejected with HTTP 422 on write, and (4) `POST /admin/api/prices/sync` fetches `price_sync_catalog_url` only when enabled and inserts suggested aliases with `confirmed = 0`. |
| Atomic commits | 1. `feat(admin/4.2): implement body-based price book endpoints, provider-specific price lookup, and opt-in models.dev importer [Spec: USG-3, Partial: PRIV-1]` |
| Risks and resolutions | **Risk:** Placing `upstream_model_id` (`deepseek/deepseek-v4.1-flash`) in a URL path segment fails routing in `axum` or when reverse proxies decode `%2F`. **Resolution:** Pass `upstream_model_id` in the JSON request body on both admin and user price endpoints (`SRDD Section 14.2`). |

### 4.3 Sub-phase 4.3: Three-queue batching SQLite writer (`durable_state`, `rollups`, `raw_events`), `usage_attempts`, and `usage_rollups_5min`

This sub-phase implements the three-queue data-plane writer (`durable_state`, `rollups`, and `raw_events` `Mutex<VecDeque<RawItem>>` dropping oldest). It commits `usage_events`, `usage_attempts`, `usage_rollups_5min`, and `usage_rollups_hourly` (with `''`/`'none'` sentinels for pre-route rejections) and verifies `REL-2` and `REL-3` (`USG-1`, `SHR-6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.2`, `Section 9.1`, `Section 9.2`, `Section 21.6`; Requirements `USG-1`, `SHR-6`, `REL-2`, `REL-3`, `Partial: TRC-9` |
| Crates touched | `crates/store::usage_writer`, `crates/store::rollups` |
| TDD step (`Red -> Green`) | Write fault-injection and crash-recovery tests (`tests/integration_usage_writer.rs`, `#[doc = "Spec: USG-1, SHR-6, REL-2, REL-3, Partial: TRC-9"]`): (1) pause the SQLite writer task and push 10,500 `UsageEvent` items; assert that inference completes in `< 1 ms`, the `Mutex<VecDeque<RawItem>>` evicts the **oldest** 500 raw events (`llmroller_usage_dropped_total{kind="usage_event"} == 500`), and 100% of all 10,500 token and cost totals are preserved in `usage_rollups_5min` and `usage_rollups_hourly` once the writer resumes (`REL-3`), (2) pre-route rejections (`503 no_eligible_route`) fold into rollups using `upstream_key_id = ''`, `provider_id = ''`, and `provider_scope = 'none'`, while `/v1/models`, `/v1/usage`, and `/v1/key` emit metrics only and zero `usage_events` rows, (3) failed upstream tries record `usage_attempts` rows (`attempt_no 1..3`) in the same batch, and (4) confirm a pin, response link, health write, and rollup delta, wait `1.2 s`, send `SIGKILL` (`kill -9`), restart, and assert zero lost rows (`REL-2`). |
| Atomic commits | 1. `feat(store/4.3): implement three-queue batching writer, usage_attempts, 5-minute and hourly rollups, and SIGKILL recovery [Spec: USG-1, SHR-6, REL-2, REL-3]` |
| Risks and resolutions | **Risk:** A standard `tokio::sync::mpsc` channel cannot drop its oldest item on overflow and would either block inference or drop the newest item. **Resolution:** Use a `Mutex<VecDeque<RawItem>>` (capped at `10,000` items and `64 MiB`) that pops the oldest item after its cost has already been folded into the non-dropping `rollups` map (`SRDD Section 21.6.1`). |

### 4.6 Sub-phase 4.6: `/v1/*` orchestrator v3 (live usage accounting, attempt charging, and reservation finalization)

This sub-phase wires live `UsageEvent` and `usage_attempts` emission plus conservative `estimated_input` charging into `/v1/*` orchestrator v3. It also wires `Reservation` finalization in `RequestLifecycle::drop` and threshold `/usage` poll triggers (`SRDD Section 5.3, Section 13.2.1, Section 21.1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.3`, `Section 6.9`, `Section 10.5.2`, `Section 13.2.1`, `Section 21.1`; Requirements `USG-1`..`USG-3`, `LIM-1` |
| Crates touched | `crates/proxy::orchestrator`, `crates/proxy::lifecycle`, `tests/integration_usage_writer.rs` |
| TDD step (`Red -> Green`) | Write end-to-end accounting tests (`tests/integration_usage_writer.rs`, `#[doc = "Spec: USG-1, USG-2, USG-3, LIM-1"]`) verifying: (1) an upstream try that fails with HTTP 502 after the request body is written records a `usage_attempts` row with `charge_basis = 'estimated_input'` and posts `charged_micro_usd = ceil(estimate_input_tokens(body) * rate_in / 1_000_000)` to that key's windows, share accumulator, and user global accumulator before failing over to key 2, (2) a client disconnecting mid-stream on each protocol (`chat`, `responses`, `messages`) emits exactly one `usage_events` row with `termination = 'client_gone'`, `incomplete = 1`, and non-zero cost (`cost_source = 'estimated_incomplete'`), and (3) `Reservation::drop` posts actual cost to rings/accumulators before decrementing `reserved_in_flight`. Re-run the `2.7` load benchmark. |
| Atomic commits | 1. `feat(proxy/4.6): wire live usage emission, usage_attempts charging, and Reservation finalization into orchestrator v3 [Spec: USG-1, USG-2, USG-3, LIM-1]` |
| Risks and resolutions | **Risk:** Failed upstream tries that consumed prefill tokens (502/504 after body write or opt-in wait timeouts) leave local window and share counters under-counted. **Resolution:** Record every upstream try in `usage_attempts` and charge `estimated_input` for tries in the possibly-consumed class (`SRDD Section 9.1, Section 10.5.2`). |

### 4.7 Sub-phase 4.7: General retention sweeper, WAL checkpoint supervisor, and `SQLITE_FULL` degraded mode

This sub-phase implements the portable ULID-range retention sweeper for `usage_events` (30 days or `50,000,000` rows), `usage_attempts`, `usage_rollups_5min`, `response_links`, `pins`, and `web_sessions`. It also adds the 30 s WAL checkpoint supervisor and `SQLITE_FULL` degraded-mode handling (`SRDD Section 3.3 A6, Section 9.2, Section 21.6.3`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 3.3 (A6)`, `Section 9.2`, `Section 21.6.3`; Requirements `REL-3`, `Partial: PERF-4` |
| Crates touched | `crates/store::retention`, `crates/store::checkpoint`, `crates/server::jobs` |
| TDD step (`Red -> Green`) | Write retention and WAL tests (`tests/integration_usage_writer.rs`, `#[doc = "Spec: REL-3, Partial: PERF-4"]`) verifying: (1) the sweeper deletes expired `usage_events` in 1,000-row chunks using portable `WHERE id IN (SELECT id FROM usage_events WHERE id < :cutoff_ulid ORDER BY id LIMIT 1000)` and enforces `usage_events_max_rows = 50_000_000` followed by `PRAGMA incremental_vacuum(500)`, (2) the checkpoint supervisor runs `wal_checkpoint(PASSIVE)` every 30 s, triggers `wal_checkpoint(RESTART)` between writer batches when the WAL exceeds `256 MiB`, and sets `/readyz` to HTTP 503 when the WAL exceeds `1 GiB`, and (3) simulated `SQLITE_FULL` drops incoming raw events (`llmroller_storage_full = 1`, `/readyz` -> 503) while continuing to retry `durable_state` and `rollups` commits every 5 s. |
| Atomic commits | 1. `feat(store/4.7): implement portable ULID-range retention sweeper, WAL checkpoint supervisor, and SQLITE_FULL handler [Spec: REL-3, Partial: PERF-4]` |
| Risks and resolutions | **Risk:** Long read transactions pin the WAL tail and prevent `PASSIVE` checkpoints from reclaiming space. **Resolution:** Bound all read-pool queries by paging (`<= 5,000` rows or `<= 50` turns per read transaction) and trigger `wal_checkpoint(RESTART)` from the writer between batches when WAL size exceeds `256 MiB` (`SRDD Section 21.6.3`). |
## Epic 4b: Startup 5-minute rollup reconstruction and source provider `/usage` engine

Epic 4b connects the pure-core window and monthly accumulator engines (`3.0b` and `3.0c`) to persistent 5-minute rollups (`usage_rollups_5min`) and manual calibration (`budget_adjustments`). It also builds the source provider `/usage` poller, observation reconciler, and pass-through APIs. It corresponds to Milestone `M4` (`SRDD Sections 2.4, 5.5, 6.9, 7.1, 8.2, 9.1, 13.2–13.4, 21.4, 21.8`).

### 4b.1 Sub-phase 4b.1: Startup window and accumulator reconstruction from `usage_rollups_5min` and manual calibration

This sub-phase implements `< 2 s` startup reconstruction of all budget windows and monthly accumulators from `usage_rollups_5min`, `budget_adjustments`, and `upstream_usage_snapshots` (`PERF-4`, `LIM-1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 6.9`, `Section 7.1`, `Section 9.1`, `Section 13.2`; Requirements `LIM-1`, `PERF-4` |
| Crates touched | `crates/store::budgets`, `crates/server::startup` |
| TDD step (`Red -> Green`) | Write startup reconstruction and benchmark tests (`store::budgets::tests::startup_rebuild`, `#[doc = "Spec: LIM-1, PERF-4"]`) using `FakeClock`: (1) seed 7 days of `usage_rollups_5min` rows across 500 upstream keys (plus 50M `usage_events` rows in the database), restart the store, and assert that `serve` reconstructs all 5 h, 7 d, and 1 mo windows plus share, user-global, and client-key monthly accumulators and reaches `/readyz` HTTP 200 in `< 2 s` (`PERF-4`), (2) verify that reconstruction holds even when raw `usage_events` rows were dropped under `REL-3` writer pressure, and (3) verify that `manual_calibration` rows in `budget_adjustments` apply `effective_at` inside rolling rings (aging out with the bucket) and `window_instance_start` on anchored windows (`SRDD Section 13.2`). |
| Atomic commits | 1. `feat(store/4b.1): implement <2s startup window and monthly accumulator reconstruction from usage_rollups_5min and manual calibration [Spec: LIM-1, PERF-4]` |
| Risks and resolutions | **Risk:** Scanning raw `usage_events` for the trailing 5 h at 500 req/s reads 9,000,000 rows (taking tens of seconds) and under-counts whenever `REL-3` drops raw events. **Resolution:** Reconstruct every window ring and monthly accumulator exclusively from the non-dropping `usage_rollups_5min` table (`SRDD Section 7.1 PERF-4, Section 13.2`). |

### 4b.2 Sub-phase 4b.2: Source provider `/usage` poller, single-flight rate limiter, `UpstreamObservation` sync, and `/v1/usage` pass-through

This sub-phase implements upstream `/usage` polling with per-key single-flight and global concurrency limits (`PRV-6`, `USG-4`) and sealed `raw_json_enc` persistence (`USG-5`, `SEC-18`). It also builds `UpstreamObservation` reconciliation (`USG-6`) and the `/v1/usage`, `/v1/key`, and `/v1/usage/raw` pass-through endpoints (`API-1`, `USG-7`, `USG-8`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 2.4`, `Section 6.3`, `Section 6.8`, `Section 6.9`, `Section 8.2`, `Section 13.3`, `Section 13.4`; Requirements `PRV-6`, `API-1`, `USG-4`..`USG-8`, `SEC-18` |
| Crates touched | `crates/proto::usage_poller`, `crates/store::usage_snapshots`, `crates/proxy::usage_passthrough`, `fixtures/usage/*` |
| TDD step (`Red -> Green`) | Write golden parser and integration tests (`tests/integration_usage_and_passthrough.rs`, `#[doc = "Spec: PRV-6, API-1, USG-4, USG-5, USG-6, USG-7, USG-8, SEC-18"]`): (1) replay `fixtures/usage/opencode_go.json`, `command_code_credits.json`, and `openrouter_key.json`, verifying that `raw_json_enc` is sealed with `XChaCha20-Poly1305` (`domain = "usage_raw"`) while `sanitized_json` serializes `SanitizedUsageSnapshot` (`SEC-18`), (2) verify that `poll(key)` captures `t0 = now` and `snapshot_local` at poll start and updates `w.observation` (`UpstreamObservation`) on windows matching `upstream_window_kind` so reporting `25%` while local is `10%` sets `effective_used` to `25%` and subsequent `+2%` local spend reads `27%` with **zero** rows inserted into `budget_adjustments` (`USG-6`), (3) verify that a snapshot reporting `100%` sets `exhausted` until `resets_at_ms`, and if subsequent polls return 500 after `resets_at_ms`, the observation expires and the key becomes eligible again at `resets_at_ms + 1 ms`, (4) verify that 40 concurrent 429s trigger exactly one upstream poll (`poll_in_flight: AtomicBool`, 30 s automatic cooldown, `usage_poll_concurrency = 8`), and (5) verify that `GET /v1/usage` rejects `?session_id=` query strings with HTTP 400 (accepting session headers only), honors `?fresh=1` only for `Own`-tier owners or admins (returning cached data with `x-llmroller-usage-stale-ms` to grantees/global callers), and serves `/v1/usage/raw` strictly from stored snapshots (`USG-7`, `USG-8`). |
| Atomic commits | 1. `feat(proto/4b.2): implement single-flight /usage poller, sealed raw_json_enc storage, and allow-list SanitizedUsageSnapshot [Spec: PRV-6, USG-4, USG-5, SEC-18]` 2. `feat(core/4b.2): wire UpstreamObservation window reconciliation and exhaustion recovery [Spec: USG-6]` 3. `feat(proxy/4b.2): implement header-scoped GET /v1/usage, GET /v1/key, and stored-snapshot GET /v1/usage/raw [Spec: API-1, USG-7, USG-8]` |
| Risks and resolutions | **Risk:** Allowing any shared or global caller to trigger live upstream `/usage` fetches via `?fresh=1` or `/v1/usage/raw`, or passing raw session IDs in URL query strings, risks upstream rate-limiting and leaks session IDs into access logs. **Resolution:** Accept session IDs only via HTTP headers (`400` on `?session_id=`), serve `/v1/usage/raw` from stored snapshots only, and restrict live `?fresh=1` polls to `Own`-tier key owners and admins (`SRDD Section 13.3.2, Section 13.4`). |
## Epic 5: Opt-in content traceability mode, content-addressed blocks, 2-thread sealer pool, and chat reconstructor

Epic 5 builds the admin opt-in `traceability_config` (`5.1`) and the bounded `TraceTap` with 1 MiB output accumulation and binary replacement (`5.2`). It also builds content-addressed `trace_blocks` with the 2-thread sealer pool (`5.3`) and the block-hash LCP Session Chat Preview reconstructor (`5.4`). It corresponds to Milestone `M3b` (`SRDD Sections 6.10, 7.1, 8.2, 9.1, 9.2, 12.6, 21.1, 21.6`).

### 5.1 Sub-phase 5.1: Admin opt-in `traceability_config`, list-intersection matcher, `auto_disable_at`, and purge confirmation

This sub-phase implements the default-off `traceability_config` setting (`enabled`, `match: {user_ids, client_key_ids, models}`, `retention_days`, `auto_disable_at`), request-start evaluation, sweeper auto-disable, and the `"confirm_purge": true` safety gate (`TRC-1`, `TRC-7`, `AUD-1`, `SRDD Section 6.10, Section 9.3.3`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 6.10`, `Section 6.11`, `Section 9.3.3`; Requirements `TRC-1`, `TRC-7`, `AUD-1` |
| Crates touched | `crates/core::trace_config`, `crates/store::settings`, `crates/admin::trace_settings` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`core::trace_config::tests`, `#[doc = "Spec: TRC-1, TRC-7, AUD-1"]`) verifying: (1) fresh instances have `traceability_config.enabled == false`, (2) `traced: bool` is evaluated once at request start (`enabled && (auto_disable_at.is_none() || now < auto_disable_at)`) where a request matches iff every non-empty list in `match` (`user_ids`, `client_key_ids`, `models`) contains the request's value, (3) once `now >= auto_disable_at`, the 60 s sweeper sets `enabled = false` and appends a system audit row `traceability.auto_disabled`, and (4) `PUT /admin/api/settings/traceability` with a lower `retention_days` returns HTTP 409 (reporting affected row count) unless `"confirm_purge": true` is supplied. |
| Atomic commits | 1. `feat(core/5.1): implement TraceabilityConfig list-intersection matcher and request-start evaluation [Spec: TRC-1, TRC-7]` 2. `feat(admin/5.1): implement PUT /admin/api/settings/traceability with confirm_purge guard, auto_disable sweeper, and audit logging [Spec: TRC-1, TRC-7, AUD-1]` |
| Risks and resolutions | **Risk:** Lowering `retention_days` by typo (e.g., `1` instead of `14`) silently and irreversibly purges historical traces, and mid-stream setting toggles produce half-traced turns. **Resolution:** Evaluate `traced: bool` once at request start and enforce `"confirm_purge": true` whenever `retention_days` decreases (`SRDD Section 6.10`). |

### 5.2 Sub-phase 5.2: Bounded `TraceTap`, binary block omission, head/tail truncation, `trace_tap` fuzzer, and 2,000-stream memory soak

This sub-phase implements bounded `TraceTap` (`mpsc::channel(256)` via `try_send`, `64 MiB` `trace_inflight_bytes`, and `256 MiB` global accumulator budget) and binary block replacement (`> 1 KiB`). It also enforces the `4 MiB` request and `1 MiB` output caps (`PERF-5`) and runs the `trace_tap` fuzzer and 2,000-stream memory soak test (`TRC-2`, `TRC-3`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.10`, `Section 7.1`, `Section 9.2`, `Section 12.6`, `Section 21.1`; Requirements `TRC-2`, `TRC-3`, `PERF-2`, `PERF-5` |
| Crates touched | `crates/proto::trace_tap`, `fuzz/fuzz_targets/trace_tap.rs`, `benches/proxy_latency.rs` |
| TDD step (`Red -> Green`) | Write golden SSE reassembly, truncation, fuzz, and soak tests (`proto::trace_tap::tests`, `#[doc = "Spec: TRC-2, TRC-3, PERF-2, PERF-5"]`): (1) replay multi-chunk streams across `chat`, `responses`, and `messages` interleaving reasoning, parallel tool calls, and text, (2) verify that binary content in request blocks (`> 1 KiB` base64 images/PDFs/`data:` URLs) is always replaced with `{"type":"<orig>","llmroller_omitted":"binary","sha256":"<hex>","bytes":<n>}` before hashing, (3) verify that new request blocks exceeding `4 MiB` keep the newest blocks from the end (`dropped_block_count`) while response output exceeding `1 MiB` keeps the first `512 KiB` and last `512 KiB` with `[llmroller: N bytes omitted]` (`truncated_content = 1`), (4) verify that stalling the trace collector triggers `try_send` drop (`truncated_content = 1`) with zero impact on `< 1 ms` chunk forwarding (`PERF-2`), and (5) run 2,000 open traced streams and assert RSS stays within `PERF-3` plus the `256 MiB` global trace accumulator budget (`PERF-5`). |
| Atomic commits | 1. `feat(proto/5.2): implement bounded TraceTap, binary block replacement, 4MiB/1MiB head-tail truncation, and trace_tap fuzzer [Spec: TRC-2, TRC-3, PERF-2, PERF-5]` |
| Risks and resolutions | **Risk:** Holding a `zstd::stream::Encoder` per active stream consumes `~3.5 MiB` per stream (`ZSTD_estimateCStreamSize(3)`), consuming `~7 GB` of RAM across 2,000 streams on a 2 GB host. **Resolution:** Never hold a compressor per stream; accumulate structured output (`<= 1 MiB` per stream, `<= 256 MiB` globally) and compress once at turn end on the 2-thread sealer pool (`SRDD Section 7.1 PERF-5`). |

### 5.3 Sub-phase 5.3: Content-addressed `trace_blocks`, dedicated 2-thread OS sealer pool, and unreferenced-block purge

This sub-phase implements content-addressed `trace_blocks` and `trace_record_blocks` (`StoredTurnTrace v2`) with `(ts, request_id)` ordering and `thread_id` (`TRC-4`, `SEC-19`). It also adds the dedicated 2-thread OS sealer pool (`crossbeam_channel::bounded(64)`), standalone commits for traces `> 1 MiB`, and reference-aware trace purge (`TRC-7`, `PERF-5`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.10`, `Section 7.1`, `Section 8.2`, `Section 9.1`, `Section 9.2`; Requirements `TRC-4`, `TRC-7`, `PERF-5`, `SEC-19` |
| Crates touched | `crates/store::traces`, `crates/server::sealer`, `crates/server::jobs` |
| TDD step (`Red -> Green`) | Write cryptographic, self-containment, and backpressure tests (`store::traces::tests`, `#[doc = "Spec: TRC-4, TRC-7, PERF-5, SEC-19"]`) verifying: (1) each request block is hashed (`cache_control` and `citations` stripped) and deduplicated per `(owner_user_id, block_hash)` in `trace_blocks` (`domain = "trace_block"`) via a 100,000-entry LRU cache, while `trace_records` stores `request_skeleton`, `block_refs`, and `response` (`domain = "trace_record"`, ordered by `(ts, request_id)` with `thread_id`), (2) dropping turn 3 of 5 from the raw queue leaves turns 4 and 5 100% self-contained and renderable, (3) artificially slowing the 2 sealer OS threads to 1 job/s under 500 req/s load bounds the `crossbeam_channel::bounded(64)` backlog, sets `usage_events.trace_dropped = 1` (`llmroller_trace_dropped_total{reason="sealer_backlog"}`), and never delays inference, (4) trace records `> 1 MiB` commit in their own standalone writer transaction so `Control` commands never wait behind multi-MiB blobs, and (5) `DELETE /admin/api/sessions/{session_hash}/trace` deletes the session's `trace_records` and unreferenced `trace_blocks` for that owner while preserving `trace_blocks` shared with another session of the same user and all `usage_events` rows (`TRC-7`). |
| Atomic commits | 1. `sec(store/5.3): implement content-addressed trace_blocks, StoredTurnTrace v2, and dedicated 2-thread crossbeam sealer pool [Spec: TRC-4, PERF-5, SEC-19]` 2. `feat(store/5.3): implement 64MiB-bounded trace retention sweeper and reference-aware session trace purge [Spec: TRC-7]` |
| Risks and resolutions | **Risk:** Write-time delta chaining (`turn k` referencing `turn k-1`) breaks whenever an earlier turn is dropped under `REL-3` overflow, purged, or compacted, and `tokio::task::spawn_blocking` cannot bound thread count or queue memory. **Resolution:** Store self-contained `request_skeleton` + content-addressed `trace_blocks` and run compression on 2 dedicated OS threads fed by `crossbeam_channel::bounded(64)` (`SRDD Section 9.2`). |

### 5.4 Sub-phase 5.4: Block-hash LCP timeline reconstructor, predecessor-inclusive cursor pagination, and XSS-hardened preview API

This sub-phase implements `crates/core::chat_preview` (`SRDD Section 12.6`) computing LCP over `block_refs` arrays with predecessor-inclusive cursor pagination (`?after=<ts>:<request_id>&limit=50`, `<= 8 MiB` per page). It also adds per-client-key recently active session lookup with live `PinStore` bindings (`TRC-9`) and `project_key` redaction. Finally, it enforces deduplicated `trace.viewed` audit logging and `pulldown-cmark` + `ammonia` / `<pre>` escaping (`TRC-5`, `TRC-8`, `SEC-19`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 4.6`, `Section 6.10`, `Section 6.11`, `Section 8.2`, `Section 12.6`, `Section 12.6.1`, `Section 12.6.2`; Requirements `TRC-5`, `TRC-8`, `TRC-9`, `SEC-17`, `SEC-19`, `Partial: TRC-6` |
| Crates touched | `crates/core::chat_preview`, `crates/admin::traces` |
| TDD step (`Red -> Green`) | Write reconstruction, pagination, privacy, and XSS tests (`tests/integration_content_traceability.rs`, `#[doc = "Spec: TRC-5, TRC-8, TRC-9, SEC-17, SEC-19, Partial: TRC-6"]`): (1) a 5-turn stateless session (plus multi-model auxiliary turns and subagent threads) computes LCP on `block_refs` without decrypting repeated prefix blocks and emits each new message once in `(ts, request_id)` order, along with `current_pins` showing which provider and upstream key each model in the session is pinned to right now, (2) fetching page 2 (`?after=<ts>:<request_id>&limit=50`) loads `limit + 1` records starting one record before the cursor so the first turn of page 2 computes its exact LCP against the last turn of page 1 without false `context_compacted_or_branched` dividers, (3) turns exceeding the `8 MiB` per-page decryption cap return `"load_more"` placeholders fetched via `GET /api/user/traces/{request_id}`, (4) `GET /api/user/client-keys/{id}/sessions` and `GET /admin/api/client-keys/{id}/sessions` return recently active sessions for that client key ordered by `last_active_at DESC` with logged request/token/cost totals, `traced_turn_count`, `last_served_route`, and live `PinStore` `current_pins` (projected via `project_key` so standard users see provider name + `"global pool"` on global keys, and returning `current_pins: []` with `last_served_route` preserved once the pin expires, `TRC-9`), (5) Grantor Alice receives HTTP 404 when querying Grantee Bob's `session_hash` or client-key sessions (`TRC-8`), and admin views write at most one `trace.viewed` audit row per `(admin, session_hash)` per 30 minutes, and (6) `<script>alert(1)</script><img src=x onerror=alert(1)>` injected into assistant Markdown, reasoning, tool arguments, tool results, and raw JSON is neutralized (`pulldown-cmark` raw HTML disabled + `ammonia` allow-list for assistant text; Askama auto-escaped `<pre>` for all other fields, `SEC-19`). |
| Atomic commits | 1. `feat(core/5.4): implement block-hash LCP timeline reconstructor and predecessor-inclusive cursor pagination [Spec: TRC-5]` 2. `sec(admin/5.4): implement RBAC-isolated chat preview and per-client-key recently active sessions APIs with live PinStore projection and ammonia/pre escaping [Spec: TRC-8, TRC-9, SEC-17, SEC-19, Partial: TRC-6]` |
| Risks and resolutions | **Risk:** Paginating turns without loading the preceding turn breaks LCP on page boundaries, and decrypting all 150 turns in one read transaction pins the SQLite WAL and costs hundreds of megabytes per view. **Resolution:** Load `limit + 1` `trace_records` rows in a short read transaction, compute LCP on `block_refs` hashes without decrypting prefix blocks, and decrypt only the new suffix blocks (`<= 8 MiB` per page, `SRDD Section 12.6`). |
## Epic 6: Dual-workspace HTMX web UI, key projection, share consent modals, and Session Chat Preview UI

Epic 6 builds the embedded web interface (`askama`, `htmx`, `uPlot`, `rust-embed`) and completes the control-plane APIs. It covers the Personal Workspace (`/app/*`), the Admin Console (`/admin/*`), and the Session Chat Preview UI. It corresponds to Milestone `M5` (`SRDD Sections 4, 6.10, 6.11, 8.2, 14`).

### 6.1 Sub-phase 6.1: Askama + HTMX shell, embedded assets, strict CSP configuration, and template lint gate

This sub-phase sets up the Askama templates, embeds vendored `htmx 2.x` and `uPlot` assets via `rust-embed` (`UI-3`), and configures HTMX for strict CSP compatibility. It also enforces `x-csrf-token` checks (`SEC-5`), role-aware navigation (`UI-1`), and the global traceability banner (`UI-4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.11`, `Section 8.2`, `Section 14.2`, `Section 14.3`; Requirements `UI-1`, `UI-3`, `UI-4`, `SEC-5` |
| Crates touched | `crates/admin::ui`, `crates/admin::assets`, `crates/admin::csrf` |
| TDD step (`Red -> Green`) | Write integration and template-lint tests (`tests/security_csrf_and_headers.rs`, `#[doc = "Spec: UI-1, UI-3, UI-4, SEC-5"]`) asserting: (1) every HTML response includes `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; frame-ancestors 'none'`, `X-Frame-Options: DENY`, and `X-Content-Type-Options: nosniff`, (2) `/static/app.js` sets `htmx.config.includeIndicatorStyles = false`, `htmx.config.allowEval = false`, and `htmx.config.allowScriptTags = false` before HTMX initializes, spinner CSS lives in `/static/app.css`, and a template lint test scans all `crates/admin/templates/**/*.html` files to assert zero external `http(s)://` asset URLs (`UI-3`) and zero `hx-on:*` or `js:` attributes, (3) any mutating request without a valid `x-csrf-token` returns HTTP 403 (`SEC-5`), and (4) active `traceability_config.enabled` renders the global banner (`UI-4`). |
| Atomic commits | 1. `feat(admin/6.1): embed HTMX and uPlot assets, configure CSP-safe HTMX flags, and add template lint gate [Spec: UI-3, SEC-5]` 2. `feat(admin/6.1): implement role-aware layout shell and content traceability status banner [Spec: UI-1, UI-4]` |
| Risks and resolutions | **Risk:** Default `htmx 2.x` injects an inline `<style>` tag for spinner classes and uses `eval` for `hx-on:*` and `js:` attributes, triggering CSP violations under `style-src 'self'; script-src 'self'`. **Resolution:** Set `includeIndicatorStyles = false`, `allowEval = false`, and `allowScriptTags = false` in `/static/app.js`, ship spinner styles in `/static/app.css`, and ban `hx-on:*` and `js:` in templates via CI lint (`SRDD Section 8.2 SEC-5`). |

### 6.2 Sub-phase 6.2: Standard user Personal Workspace (`/app/*` and `/api/user/*`), `project_key` guard, and share acceptance modal

This sub-phase implements the self-service `/api/user/*` endpoints and `/app/*` screens for standard `user` accounts (`SRDD Section 14.2.1, Section 14.3`). It includes the share acceptance modal, constant-session mode controls, body-based price overrides, machine principal service accounts (`CLK-6`), inline client key pause/resume (`CLK-7`), UI preferences (`UI-5`), and universal `project_key(viewer, key)` redaction (`USR-2`..`USR-4`, `UI-1`, `UI-2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 6.2`..`6.5`, `Section 12.6.2`, `Section 14.2.1`, `Section 14.3`; Requirements `USR-2`..`USR-4`, `UI-1`, `UI-2`, `CLK-6`, `CLK-7`, `UI-5`, `Partial: TRC-9` |
| Crates touched | `crates/admin::projection`, `crates/admin::user_api`, `crates/admin::templates::app` |
| TDD step (`Red -> Green`) | Write RBAC, projection, and UI integration tests (`tests/integration_rbac_isolation.rs`, `#[doc = "Spec: USR-2, USR-3, USR-4, UI-1, UI-2"]`): (1) Alice creates a personal provider and key, tests `/models` and `/usage`, sets `user_route_preferences` overrides (restricted to personal and shared routes only), creates a share for Bob, and issues an `llr_...` client key (`own_only` / `personal_first`), (2) Bob inspects the pending share in **Shared With Me** (showing provider kind, base URL host, key label, and the operator visibility disclosure) and calls `POST /api/user/shares/{id}/accept` with `config_fingerprint`, (3) an automated response-scanner iterates every `/api/user/*` and `/app/*` endpoint as a standard user and asserts that `crates/admin::projection::project_key` redacts all `global` keys to `upstream_key_id: null` and `upstream_key_label: "global pool"` (aggregating global keys into one `"Global pool"` column in the Routing Matrix and omitting `priority`, `secret_last4`, and `health_until`, `USR-4`), and (4) unauthorized reads of another user's resource always return HTTP 404 (`not_found`, never 403) while unauthorized edits of a readable shared key return HTTP 403 (`USR-3`). |
| Atomic commits | 1. `feat(admin/6.2): implement project_key projection and /api/user/* endpoints for personal providers, keys, and share consent [Spec: USR-2, USR-3, USR-4, UI-2]` 2. `feat(admin/6.2): implement /app/* HTMX screens for shares, single-column global routing matrix, and client keys [Spec: UI-1]` |
| Risks and resolutions | **Risk:** Individual handlers or templates forget to redact global key labels or return 403 vs. 404 inconsistently on cross-tenant IDs (creating an existence oracle). **Resolution:** Route all key serialization through `crates/admin::projection::project_key(viewer, key)` and enforce 404 for all unauthorized reads (`SRDD Section 6.2 USR-3, USR-4`). |

### 6.3 Sub-phase 6.3: Admin Console (`/admin/*` and `/admin/api/*`), user-resource overrides, and audited user deletion

This sub-phase implements the `/admin/api/*` endpoints and `/admin/*` screens for instance administration (`SRDD Section 14.2.2, Section 14.3`). It includes admin endpoints for user resources, custom-provider share approval, `?confirm=<username>` user deletion (`deleted_users`), control plane request logging (`AUD-2`), and catalog merge/split controls (`AUTH-4`, `USR-5`, `MDL-3`, `MDL-4`, `UI-1`, `AUD-1`, `AUD-2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 6.1`..`6.3`, `Section 6.6`, `Section 10.3.3`, `Section 14.2.2`, `Section 14.3`; Requirements `AUTH-4`, `USR-5`, `MDL-3`, `MDL-4`, `UI-1`, `AUD-1`, `AUD-2`, `Partial: RTE-14` |
| Crates touched | `crates/admin::admin_api`, `crates/admin::templates::admin` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/e2e_dashboard_htmx.rs`, `#[doc = "Spec: AUTH-4, USR-5, MDL-3, MDL-4, UI-1, AUD-1"]`) verifying: (1) every `/admin/*` and `/admin/api/*` route returns HTTP 403 for `Role::User`, (2) admins can inspect and manage other users' personal resources via `/admin/api/users/{id}/providers`, `/admin/api/users/{id}/shares`, `/admin/api/shares/{id}/approve`, and `/admin/api/users/{id}/pins` (each writing an audit row with the owner's `user_id` in `detail_json`), (3) `DELETE /admin/api/users/{id}?confirm=<username>` disables the user's granted shares, deletes their keys/providers/pins/traces/sessions, records `deleted_users(user_id, username, deleted_at)`, and preserves historical `usage_events` and rollups in one transaction, and (4) `PUT /admin/api/model-aliases` and `DELETE /admin/api/model-aliases?alias=<urlencoded>` handle aliases containing `/`. |
| Atomic commits | 1. `feat(admin/6.3): implement /admin/api/* endpoints for global providers, catalog merge/split/confirm-merge, and query/body aliases [Spec: MDL-3, MDL-4, UI-1]` 2. `feat(admin/6.3): implement admin user-resource endpoints, deleted_users preservation, and audit log screens [Spec: AUTH-4, USR-5, AUD-1]` |
| Risks and resolutions | **Risk:** Deleting a user either orphans historical usage reports or cascades away `usage_events` needed for team accounting. **Resolution:** Keep `usage_events` and rollups intact without a cascading FK on `owner_user_id` and record `(user_id, username, deleted_at)` in `deleted_users` (`SRDD Section 9.1, Section 14.2.2`). |

### 6.4 Sub-phase 6.4: Overview dashboard, live `RuntimeState` health badges, disk projection meter (`A6`), Active Sessions, and Usage Explorer

This sub-phase implements the Overview Dashboard with `uPlot` charts, local ring and `UpstreamObservation` gauges, live `RuntimeState` health badges, and the `A6` disk-usage meter (`UI-6`). It also implements the Active Sessions pin table, the Dual-Stream Logs Explorer (`USG-9`), the Usage Ledger with subscription plan coverage (`USG-10`), and the paged Usage Explorer with formula-safe CSV/JSON export (`UI-1`, `UI-2`, `UI-6`, `USG-9`, `USG-10`, `SEC-6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 3.3 (A6)`, `Section 6.9`, `Section 6.11`, `Section 8.2`, `Section 12.6.2`, `Section 14.3`; Requirements `UI-1`, `UI-2`, `UI-6`, `USG-9`, `USG-10`, `SEC-6`, `Partial: TRC-9` |
| Crates touched | `crates/admin::dashboard`, `crates/admin::sessions`, `crates/admin::usage_explorer` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/e2e_dashboard_htmx.rs`, `#[doc = "Spec: UI-1, UI-2, SEC-6"]`) asserting: (1) key cards render live health from `RuntimeState` plus both local ring spend and `UpstreamObservation` percentages (`5h`, `7d`, `1mo`), (2) the Overview and Settings screens display the `A6` projected disk use calculation (`daily_requests * retention_days * 600 B * 1.3`), (3) opening the **Recently Active Sessions** drawer on a client key (or filtering Active Sessions by `client_key_id`) displays each recent `session_hash`, its live pinned provider and `project_key`-redacted upstream key right now, logged request and trace counts, and **View Requests** / **Preview Chat** links (`Partial: TRC-9`), while moving an active pin increments `PinEntry.generation` in `PinStore` and routes the next turn to the new key, and (4) Usage Explorer reads in bounded pages (`<= 5,000` rows per read transaction) and prefixes CSV cells starting with `=`, `+`, `-`, `@`, `	`, or `
` with `'` (`SEC-6`). |
| Atomic commits | 1. `feat(admin/6.4): implement Overview Dashboard with uPlot charts, RuntimeState health badges, and A6 disk meter [Spec: UI-1, UI-2]` 2. `feat(admin/6.4): implement Active Sessions pin manager and paged Usage Explorer with formula-safe CSV export [Spec: UI-1, SEC-6]` |
| Risks and resolutions | **Risk:** Unbounded CSV export queries hold a read snapshot across millions of `usage_events` rows, starving WAL checkpoints. **Resolution:** Page all Usage Explorer and CSV export reads in short transactions of at most `5,000` rows using the ULID cursor (`SRDD Section 21.6.3`). |

### 6.5 Sub-phase 6.5: Read-only Session Chat Preview UI (`TRC-5`, `TRC-6`, `UI-4`)

This sub-phase builds the read-only Session Chat Preview drawer and full-page view (`/app/sessions/{session_hash}/chat` and `/admin/sessions/{session_hash}/chat`). It renders per-thread `threads` metadata, per-turn model/protocol/thread chips, `project_key` key labels, `"load_more"` placeholders, and raw JSON drawers (`TRC-5`, `TRC-6`, `TRC-8`, `UI-4`, `SEC-19`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.10`, `Section 8.2`, `Section 12.6`, `Section 12.6.1`, `Section 12.6.2`, `Section 14.3`; Requirements `TRC-5`, `TRC-6`, `TRC-8`, `TRC-9`, `UI-4`, `SEC-19` |
| Crates touched | `crates/admin::chat_preview_ui`, `crates/admin::templates::chat_preview` |
| TDD step (`Red -> Green`) | Write end-to-end HTMX rendering and XSS tests (`tests/e2e_dashboard_htmx.rs`, `#[doc = "Spec: TRC-5, TRC-6, TRC-8, TRC-9, UI-4, SEC-19"]`) verifying: (1) multi-model sessions (main thread on `claude-sonnet-4.6` plus subagent thread on a fast model) render live `current_pins` header badges (showing which provider and `project_key`-redacted upstream key each model in the session is pinned to right now), per-thread `system_instructions`/`tools`, and per-turn `ordinal`, `canonical_model`, `protocol`, `thread_id`, and `system_changed` badges, (2) standard users viewing a turn served by a global key see `"global pool"` (`project_key`), (3) assistant Markdown renders through `pulldown-cmark` (raw HTML disabled) + `ammonia` while reasoning, user text, tool arguments, tool results, and raw JSON render inside Askama auto-escaped `<pre>` blocks (never `|safe`), and (4) cursor pagination (`?after=<ts>:<request_id>&limit=50`) and `"load_more"` block expansion work without page-boundary glitches. |
| Atomic commits | 1. `feat(admin/6.5): implement read-only Session Chat Preview UI with per-thread headers, cursor pagination, and XSS-safe rendering [Spec: TRC-5, TRC-6, TRC-8, UI-4, SEC-19]` |
| Risks and resolutions | **Risk:** Rendering tool arguments, tool results, or reasoning through a Markdown/HTML pipeline or using `|safe` in Askama templates creates an XSS vector from prompt-injected tool output. **Resolution:** Pass only assistant text through `pulldown-cmark` + `ammonia` and render all other fields as Askama auto-escaped `<pre>` text (`SRDD Section 12.6.1`). |
## Epic 7: System hardening, fuzzing, load verification, live harness E2E, and v1.0.0 release

Epic 7 runs extended fuzzing and canary verification (`7.1`), full-workspace mutation testing and traceability checks (`7.2`), and 500 req/s and 2,000-stream soak benchmarks (`7.3`). It also verifies `llmroller backup`, CLI exit codes `0..14`, headless CLI agent E2E workflows (`7.4`), and signed release packaging (`7.5`). It corresponds to Milestone `M6` (`SRDD Sections 5.5, 7, 8, 16, 17, 18, 19, 21`).

### 7.1 Sub-phase 7.1: Extended fuzzing campaigns (`cargo-fuzz`) and full-stack canary secret verification

This sub-phase runs extended fuzzing campaigns across all six fuzz targets (`headers`, `body_edit`, `sse_frame`, `sse_tap`, `trace_tap`, `step8_fingerprint`). It also runs the full-system canary secret redaction suite (`SEC-6`, `SEC-14`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 8.2`, `Section 16.5.3`; Requirements `SEC-6`, `SEC-8`, `SEC-14`, `SEC-15`, `TRN-2` |
| Crates touched | `fuzz/fuzz_targets/*`, `tests/security_canary_and_ssrf.rs` |
| TDD step (`Red -> Green`) | 1. Run all six `cargo-fuzz` targets (`headers`, `body_edit`, `sse_frame`, `sse_tap`, `trace_tap`, `step8_fingerprint`) asserting zero panics, zero out-of-bounds slices, zero duplicate-key bypasses, and bounded memory allocation on arbitrary byte streams. 2. Run `tests/security_canary_and_ssrf.rs` (`#[doc = "Spec: SEC-6, SEC-14"]`) planting canary secrets in upstream keys, client keys, cookies, setup tokens, and master keys, forcing upstream 401/403/500 errors, echoed `sk-*`/`llr_*` strings in upstream error bodies, malformed SSE streams, CSV exports with formula prefixes (`=`, `+`, `-`, `@`, `	`, `
`), and simulated handler panics, and asserting zero occurrences of any canary substring across stdout/stderr JSON logs, HTTP responses, `/metrics`, `audit_log`, and SQLite tables. |
| Atomic commits | 1. `sec(tests/7.1): complete six-target cargo-fuzz suite and full-stack canary redaction and CSV formula-injection verification [Spec: SEC-6, SEC-8, SEC-14, SEC-15, TRN-2]` |
| Risks and resolutions | **Risk:** Upstream error bodies occasionally echo the `Authorization` bearer token back in an error message, leaking the key into `last_error` or logs. **Resolution:** Scrub all `sk-*`, `llr_*`, `user_*`, and configured secret substrings from upstream error bodies before persisting `last_error` or logging (`SRDD Section 8.2 SEC-6`). |

### 7.2 Sub-phase 7.2: Full-workspace mutation sweep (`cargo-mutants`) and 100% `xtask traceability` gate

This sub-phase runs the full workspace `cargo mutants --workspace` sweep (`SRDD Section 16.5.6`). It also runs `cargo xtask traceability --milestone M6`, `cargo xtask doc-consistency`, and `cargo xtask docs-bundle --check` (`SRDD Section 16.6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 16.5.6`, `Section 16.6`; Requirements `G8`, `ADR-8`, all requirement IDs in `docs/srdd/requirements.toml` |
| Crates touched | All test modules across `crates/{core,proto,store,proxy,admin,server}`, `xtask` |
| TDD step (`Red -> Green`) | Run `cargo mutants --workspace` across all crates, verify that every `#[mutants::skip]` annotation carries an `// equivalent: <reason>` comment, and add boundary or negative tests for any unannotated surviving mutant until zero survive. Run `cargo xtask traceability --milestone M6` and `cargo xtask doc-consistency` to confirm that 100% of requirement IDs in `docs/srdd/requirements.toml` and `SRDD Sections 6, 7, 8` map to passing `#[doc = "Spec: ..."]` tests (with zero `Partial:`-only requirements remaining). |
| Atomic commits | 1. `test(workspace/7.2): eliminate remaining surviving mutants and verify equivalent-mutant annotations across workspace [Spec: G8, ADR-8]` 2. `ci(workspace/7.2): enforce cargo xtask traceability --milestone M6 and doc-consistency gates in CI [Spec: G8]` |
| Risks and resolutions | **Risk:** Unreviewed `#[mutants::skip]` attributes mask real behavioral regressions. **Resolution:** Fail CI in `xtask` if any `#[mutants::skip]` lacks `// equivalent: <reason>` and enforce CODEOWNERS approval on skip changes (`SRDD Section 16.5.6`). |

### 7.3 Sub-phase 7.3: Full-scale load and soak benchmarks (`PERF-1`..`PERF-5`), 50M-row DB verification, and CLI exit-code/backup resilience (`REL-1`..`REL-6`)

This sub-phase runs the full-scale latency and memory soak benchmarks (`PERF-1`..`PERF-5`) against a 50M-row database and log-normal TTFF stub cluster. It also verifies crash-safe `llmroller backup` (`REL-6`) and all CLI exit codes `0..14` (`SRDD Section 5.5, Section 7.1, Section 7.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 7.1`, `Section 7.2`, `Section 21.8`; Requirements `PERF-1`..`PERF-5`, `REL-1`..`REL-6` |
| Crates touched | `benches/proxy_latency.rs`, `tests/e2e_sigterm_drain.rs`, `tests/e2e_kill9_recovery.rs`, `crates/server::backup`, `crates/server::cli` |
| TDD step (`Red -> Green`) | 1. Seed a SQLite database with `50,000,000` `usage_events` rows, 7 days of `usage_rollups_5min` for 500 keys, and `250,000` confirmed pins; assert cold start to `/readyz` HTTP 200 completes in `<= 2 s` and idle RSS (up to 10,000 routes) stays `<= 50 MB` (`PERF-4`). 2. Run the load harness (`500 req/s` for `<= 256 KiB` bodies and `budget / (mean_body * ttff)` for `64 KiB`, `512 KiB`, and `2 MiB` bodies against the log-normal TTFF stub, plus `2,000` open SSE streams with `traceability_config.enabled` both `false` and `true`) and assert: p50 overhead `<= 1 ms` and p99 `<= 5 ms` (`PERF-1`), per-frame forwarding delay `<= 1 ms` (`PERF-2`), RSS `<= 300 MB` plus in-flight pre-first-byte bodies (`PERF-3`), and traceability RSS within `PERF-3` plus the `256 MiB` global trace accumulator budget using the 2-thread sealer pool (`PERF-5`). 3. Run `llmroller backup --out <dest>` during active write load and verify it writes `<dest>.tmp` in the destination directory, `fsync`s the file, runs `PRAGMA integrity_check`, renames to `<dest>`, `fsync`s the directory, refuses to overwrite without `--overwrite`, and outputs JSON including `master_key_id` (`REL-6`). 4. Verify deterministic CLI exit codes `0` and `2..14` (`SRDD Section 5.5`). |
| Atomic commits | 1. `feat(server/7.3): implement crash-safe fsync llmroller backup CLI with master_key_id output and verify exit codes 0..14 [Spec: REL-6]` 2. `perf(workspace/7.3): verify PERF-1 through PERF-5 and REL-1 through REL-5 against 50M-row DB and 2,000-stream soak [Spec: PERF-1, PERF-2, PERF-3, PERF-4, PERF-5, REL-1, REL-2, REL-3, REL-4, REL-5]` |
| Risks and resolutions | **Risk:** Writing `<dest>.tmp` in `/tmp` fails `rename` (`EXDEV`) when `<dest>` is on another mount, and omitting file/directory `fsync` leaves a truncated backup on power loss. **Resolution:** Create `<dest>.tmp` inside the parent directory of `<dest>` and `fsync` both the file and parent directory around `rename` (`SRDD Section 5.5`). |

### 7.4 Sub-phase 7.4: Headless CLI harness compatibility (`COM-1`), Responses session inheritance, and network sandbox egress check (`PRIV-1`)

This sub-phase runs headless E2E workflows with `Claude Code`, `Codex CLI`, `opencode`, and the official OpenAI and Anthropic SDKs (`COM-1`). It also verifies stateful `previous_response_id` session inheritance, network sandbox egress isolation (`PRIV-1`), and the opt-in live provider scripts (`SRDD Section 7.3, Section 19.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.3`, `Section 7.4`, `Section 16.5.5`, `Section 19.2`; Requirements `COM-1`, `OBS-1`, `PRIV-1` |
| Crates touched | `tests/e2e_cli_harnesses.rs`, `tests/e2e_lifecycle.rs`, `scripts/verify_cache_affinity.sh` |
| TDD step (`Red -> Green`) | Run `tests/e2e_cli_harnesses.rs` (`#[doc = "Spec: COM-1, OBS-1, PRIV-1"]`) pointing each CLI harness and SDK at LLMRoller backed by multi-turn tool-calling stub upstreams: (1) verify that `Claude Code` (including subagents with `x-claude-code-agent-id` and `POST /v1/messages/count_tokens`), `Codex CLI` (chaining `previous_response_id` across turns into a single `session_hash`, one `pins` row, and a unified multi-turn chat preview), `opencode`, and Python/TypeScript SDKs complete multi-turn streaming tool-call loops with 100% pin stickiness, and (2) verify under network namespace monitoring that LLMRoller emits zero outbound packets to any host outside configured upstream origins (`PRIV-1`). |
| Atomic commits | 1. `test(e2e/7.4): add headless CLI harness suite for Claude Code, Codex CLI Responses chains, opencode, and SDKs with egress sandbox check [Spec: COM-1, OBS-1, PRIV-1]` 2. `feat(scripts/7.4): add opt-in live cache-affinity and /usage verification script [Spec: RTE-2, USG-4]` |
| Risks and resolutions | **Risk:** `Codex CLI` rotates `previous_response_id` on every turn (`resp_A`, `resp_B`, `resp_C`), which would split a stateful conversation into separate `session_hash` pins if `previous_response_id` were hashed as a session ID. **Resolution:** Inherit `link.session_hash` from `response_links` (`SRDD Section 11.1.2`) so all turns of a Codex conversation share one `session_hash` and one multi-turn chat preview. |

### 7.5 Sub-phase 7.5: Multi-arch static builds, `< 50 MB` distroless container, `docker compose` smoke & routing gate, CycloneDX SBOM, `cosign` signing, and `v1.0.0` release

This sub-phase packages static binaries for all five `POR-1` targets and builds the `< 50 MB` distroless and dev containers (`Section 22.1`). It also runs the containerized `docker compose` smoke, routing-strategy, and `SIGTERM` drain gate (`POR-1`, `SEC-11`, `REL-1`, `RTE-14`). It then generates the CycloneDX SBOM, signs all release files with `cosign`, and tags `v1.0.0`.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 7.4`, `Section 8.2`, `Section 10.3`, `Section 22.1`..`22.4`; Requirements `POR-1`, `REL-1`, `RTE-14`, `SEC-10`, `SEC-11` |
| Crates touched | `.github/workflows/release.yml`, `Dockerfile`, `.dockerignore`, `docker-compose.yml`, `.env.example`, `deploy/{Caddyfile,prometheus.yml}`, `scripts/verify_release_package.sh` |
| TDD step (`Red -> Green`) | Run `scripts/verify_release_package.sh` (`#[doc = "Spec: POR-1, REL-1, RTE-14, SEC-10, SEC-11"]`) asserting: (1) `runtime-distroless` image size is `< 50 MB`, runs as UID `65532:65532` with all Linux capabilities dropped and a read-only root filesystem, (2) `docker compose --profile dev up -d --wait` runs `llmroller-init` to initialize `/var/lib/llmroller` ownership (`65532:65532`) and converges `llmroller` to `healthy` via `llmroller healthcheck --url http://127.0.0.1:8080/readyz`, (3) integration tests against the containerized stack verify live routing strategy switching across `smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, and `power_of_two_choices` via both `LLMROLLER_DEFAULT_ROUTING_STRATEGY` env vars and `PATCH /admin/api/settings` / `PATCH /admin/api/models/{canonical_id}` (`llmroller_routing_selections_total{strategy=...}`), (4) `docker compose stop llmroller` sends `SIGTERM` and completes the 8-step graceful drain with exit code `0` within `stop_grace_period: 90s`, (5) `llmroller --version`, `generate-master-key`, `backup`, `rotate-master-key`, `verify-audit-log`, and `reset-admin` succeed against the container volume, and (6) SHA-256 checksums, CycloneDX SBOM, and `cosign` signatures validate cleanly. |
| Atomic commits | 1. `ci(release/7.5): finalize multi-arch static release packaging, <50MB distroless container, SBOM, cosign signing, and --version contract [Spec: POR-1, SEC-10, SEC-11]` 2. `test(docker/7.5): add docker compose healthcheck convergence, routing-strategy switching, and SIGTERM drain integration gate [Spec: POR-1, REL-1, RTE-14, SEC-11]` |
| Risks and resolutions | **Risk:** Drift between the split SRDD / Big-Bang files and the combined bundle files at release time. **Resolution:** Run `cargo xtask docs-bundle --check`, `cargo xtask doc-consistency`, and `cargo xtask traceability --milestone M6` as mandatory release gates before tagging `v1.0.0`. |
