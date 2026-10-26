# Epic 0: Engineering governance, TDD workflow, atomic commits, and adaptive playbook

This guide defines how engineers and coding agents build LLMRoller from an empty repository (`git init`) to version 1.0.0. It pairs every phase and sub-phase with exact references to the Software Requirements and Design Document (SRDD Sections 0 through 22).

## 0.1 How to use this guide as an adaptive playbook

This document is an adaptive engineering playbook, not a rigid waterfall script. Software development uncovers upstream API quirks, latency bottlenecks, and schema refinements as real code meets real traffic.

The split epic files (`big-bang-phases-0.md` through `big-bang-phases-7.md`, including `big-bang-phases-4b.md`) and `docs/srdd/requirements.toml` are authoritative. The combined `big-bang-phases.md` file is a generated bundle built by `cargo xtask docs-bundle` and verified in CI.

The epics follow the dependency graph of the seven workspace members (`core`, `proto`, `store`, `proxy`, `admin`, `server`, `xtask`). Engineers may reorder sibling sub-phases or split a sub-phase when an upstream quirk appears. They may also merge two sub-phases when a shared trait simplifies both.

| Invariant that never bends | Flexible aspect that adapts as needed |
| --- | --- |
| Every requirement ID (`AUTH-*`, `USR-*`, `PRV-*`, `SHR-*`, `CLK-*`, `MDL-*`, `RTE-*`, `API-*`, `USG-*`, `LIM-*`, `TRC-*`, `UI-*`, `AUD-*`, `PERF-*`, `REL-*`, `TRN-*`, `COM-*`, `POR-*`, `MNT-*`, `OBS-*`, `PRIV-*`, `SEC-*`) must map to passing tests via `docs/srdd/requirements.toml` (`SRDD Section 16.6`). | The internal struct layout, helper functions, and private module organization inside each crate can change freely during refactoring. |
| `crates/core` must remain 100% free of network and disk I/O (`SRDD Section 15.2`, `MNT-2`). | The order in which provider adapters (`opencode_go`, `command_code`, `open_design_amr`, `openrouter`) are implemented can shift based on test key availability. |
| Red-Green-Refactor TDD, the Expected vs. Unexpected failure rules, and zero unannotated surviving mutants in [cargo-mutants](https://mutants.rs/) apply to every pull request (`SRDD Section 16.2`, `16.3`). | Batch sizes, channel capacities, cache TTL defaults, and SQLite connection pool sizes can be tuned empirically during benchmark phases. |
| Every architectural or contract change cascades in order: `SRDD + requirements.toml -> Seam Trait / Contract Test -> Code -> xtask traceability & doc-consistency`. | UI layout details and HTMX partial boundaries can evolve as user workflows are tested in Epic 6. |

## 0.2 The seven-step Red-Green-Refactor-Mutate TDD loop

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

## 0.3 Failing test triage: expected vs. unexpected failures

As defined in `SRDD Section 16.2`, failing tests divide into two categories. Treating an unexpected regression like an expected red test destroys the safety net of the suite.

| Failure type | When it occurs | Mandatory engineering response |
| --- | --- | --- |
| Expected failure | (a) A newly written test guard in Step 3 (`Red`) before its implementation lands, or (b) an existing test that pinned an old behavior being intentionally replaced by a new requirement or `ADR-*` decision. | For (a), write the implementation in Step 4 until green; feature branches may contain red commits only if squashed before merge, and `main` never accepts a red commit. For (b), update or retire the old test in the **same atomic commit** as the implementation change, and name the sanctioning requirement ID or `ADR-*` in the commit message. |
| Unexpected failure | Changing component X causes an existing test in component Y (or an unrelated contract in X) to fail. | **Stop immediately.** Do not loosen assertions, do not add `#[ignore]`, and do not tweak mock return values to force green. Diagnose whether an architectural boundary leaked or an invariant broke, fix the root cause, and add a regression test if a hidden bug was exposed. |

## 0.4 Atomic commit and branch discipline

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

## 0.5 Adaptive change-management and pivot protocol

When real-world conditions change during development, engineers follow the table below to adjust the plan cleanly without accumulating technical debt.

| Trigger event during development | How development changes | Cascade adjustment steps |
| --- | --- | --- |
| An upstream provider changes its `/models` or `/usage` JSON schema or auth header | Only the specific `ProviderAdapter` in `crates/proto` and its preset TOML change (`MNT-2`). `crates/core` and `crates/proxy` stay untouched. | 1. Capture the new live payload into `fixtures/usage/` or `fixtures/sse/`. 2. Update the golden contract test (Expected Failure citing the upstream change). 3. Update the parser in `crates/proto`. |
| A client harness (Claude Code, Codex CLI, opencode) introduces a new session header | Add the new header name to the eight-step precedence table in `SRDD Section 11.1` and `crates/core::session`. | 1. Update `SRDD Section 11.1`. 2. Add a table-test row in `core::session::tests`. 3. Add the header constant to the resolver slice. |
| SQLite write latency spikes when content traceability mode (`TRC-2`) is active at 500 req/s | Standalone transactions already isolate trace records `> 1 MiB` (`SRDD Section 9.2`, `Section 21.6.1`); tune `trace_sealer_threads` or batch byte caps in `crates/store::traces`. | 1. Check `PERF-1` and `PERF-5` benchmarks. 2. Adjust `writer_batch_max_bytes` or sealer thread count. 3. Verify `REL-3` three-queue overflow test still passes. |
| A sub-phase reveals a missing edge case in route eligibility or RBAC | Pause feature work in the current sub-phase and lock the boundary in `crates/core` first. | 1. Add the edge case to `SRDD Section 6` or `Section 8`. 2. Write the red unit test in `crates/core`. 3. Fix the pure logic and run `cargo mutants` on `crates/core`. |

## 0.6 Epic overview map (zero to v1.0.0)

The table below maps the eight implementation epics (`big-bang-phases-1.md` through `big-bang-phases-7.md`, including `big-bang-phases-4b.md`) one-to-one to SRDD milestones and requirement families, generated from `docs/srdd/requirements.toml`.

| Epic file | Epic title | SRDD milestone | Requirement families completed (`Spec:`) |
| --- | --- | --- | --- |
| `big-bang-phases-1.md` | Foundation, storage engine, cryptography, RBAC, and snapshot reloader | `M0` | `AUTH-1`..`AUTH-9`, `USR-1`, `AUD-1`, `SEC-1`, `SEC-2`, `SEC-4`, `SEC-5`, `SEC-10`, `SEC-13`, `SEC-14`, `SEC-17`, `REL-4`, `MNT-1`, `MNT-2`, `MNT-3` |
| `big-bang-phases-2.md` | Transparent wire proxy, protocol normalizer, provider adapters, and orchestrator v1 | `M1` | `PRV-1`..`PRV-5`, `PRV-7`, `CLK-1`, `CLK-4`, `API-2`..`API-4`, `API-6`, `API-7`, `TRN-0`..`TRN-3`, `SEC-3`, `SEC-6`..`SEC-9`, `SEC-12`, `SEC-15`, `REL-1`, `OBS-1` |
| `big-bang-phases-3.md` | Core pricing/windows/accumulators, model catalog, sharing, pluggable routing strategies, pinning, and orchestrator v2 | `M2` | `USR-2`..`USR-5`, `PRV-8`, `SHR-1`..`SHR-5`, `CLK-2`, `CLK-3`, `CLK-5`, `MDL-1`..`MDL-7`, `RTE-1`..`RTE-14`, `API-8`, `LIM-1`, `REL-5`, `SEC-16` |
| `big-bang-phases-4.md` | Usage taps, three-queue batching writer, 5-minute & hourly rollups, orchestrator v3, and retention | `M3` | `API-5`, `USG-1`..`USG-3`, `SHR-6`, `REL-2`, `REL-3` |
| `big-bang-phases-4b.md` | Startup 5-minute rollup reconstruction and source provider `/usage` poller, observations, and pass-through | `M4` | `PRV-6`, `API-1`, `USG-4`..`USG-8`, `SEC-18` |
| `big-bang-phases-5.md` | Opt-in content traceability mode, content-addressed blocks, 2-thread sealer pool, and chat reconstructor | `M3b` | `TRC-1`..`TRC-5`, `TRC-7`..`TRC-9`, `PERF-5`, `SEC-19` |
| `big-bang-phases-6.md` | Dual-workspace HTMX web UI, key projection, share consent modals, and Session Chat Preview UI | `M5` | `UI-1`..`UI-4`, `TRC-6` |
| `big-bang-phases-7.md` | System hardening, fuzzing, load verification, live harness E2E, and v1.0.0 release | `M6` | `PERF-1`..`PERF-4`, `REL-6`, `COM-1`, `POR-1`, `PRIV-1`, `SEC-11` |
