# Epic 0: Engineering governance, TDD workflow, atomic commits, and adaptive playbook

This guide defines how engineers and coding agents build LLMRoller from an empty repository (`git init`) to version 1.0.0. It pairs every phase and sub-phase with exact references to the Software Requirements and Design Document (SRDD Sections 0 through 20).

## 0.1 How to use this guide as an adaptive playbook

This document is an adaptive engineering playbook, not a rigid waterfall script. Software development uncovers upstream API quirks, latency bottlenecks, and schema refinements as real code meets real traffic.

The eight epics (`big-bang-phases-0.md` through `big-bang-phases-7.md`) follow the dependency graph of the six Cargo crates (`core`, `proto`, `store`, `proxy`, `admin`, `server`). Engineers may reorder sibling sub-phases or split a sub-phase when an upstream quirk appears. They may also merge two sub-phases when a shared trait simplifies both.

| Invariant that never bends | Flexible aspect that adapts as needed |
| --- | --- |
| Every requirement ID (`AUTH-*`, `USR-*`, `PRV-*`, `SHR-*`, `CLK-*`, `MDL-*`, `RTE-*`, `API-*`, `USG-*`, `LIM-*`, `TRC-*`, `UI-*`, `AUD-*`, `PERF-*`, `REL-*`, `TRN-*`, `COM-*`, `POR-*`, `MNT-*`, `OBS-*`, `PRIV-*`, `SEC-*`) must map to passing tests (`SRDD Section 16.6`). | The internal struct layout, helper functions, and private module organization inside each crate can change freely during refactoring. |
| `crates/core` must remain 100% free of network and disk I/O (`SRDD Section 15.2`, `MNT-2`). | The order in which provider adapters (`opencode_go`, `command_code`, `open_design_amr`, `openrouter`) are implemented can shift based on test key availability. |
| Red-Green-Refactor TDD, the Expected vs. Unexpected failure rules, and zero surviving mutants in [cargo-mutants](https://mutants.rs/) apply to every commit (`SRDD Section 16.2`, `16.3`). | Batch sizes, channel capacities, cache TTL defaults, and SQLite connection pool sizes can be tuned empirically during benchmark phases. |
| Every architectural or contract change cascades in order: `SRDD -> Seam Trait / Contract Test -> Code -> Traceability Check`. | UI layout details and HTMX partial boundaries can evolve as user workflows are tested in Epic 6. |

## 0.2 The seven-step Red-Green-Refactor-Mutate TDD loop

Every sub-phase in Epics 1 through 7 is built through a strict seven-step loop. No production logic lands before its failing contract test exists (`SRDD Section 16.3`).

| Step | Loop stage | Developer action and verification command |
| --- | --- | --- |
| 1 | Trace to SRDD | Read the target SRDD section and requirement IDs listed in the sub-phase header. If a new edge case changes a contract, update the SRDD requirement table first. |
| 2 | Define seam or type | Add or update the domain structs and seam traits (`Clock`, `EntropySource`, `UpstreamTransport`, `UsageEndpointClient`, `StorageRepository`, `SecretSealer` from `SRDD Section 16.4`) with `todo!()` bodies so the crate compiles. |
| 3 | Write failing test (`Red`) | Write unit, property (`proptest`), or integration tests against the observable contract. Annotate each test with `#[doc = "Spec: <REQ-ID>"]` and run `cargo nextest run` to confirm it fails for the expected reason. |
| 4 | Write minimal code (`Green`) | Write the smallest, clearest implementation in safe Rust (`#![forbid(unsafe_code)]`) that makes the new test pass while keeping all prior tests green. |
| 5 | Refactor under green | Remove duplication, simplify control flow, and run `cargo clippy --all-targets -- -D warnings` plus `cargo nextest run`. |
| 6 | Kill surviving mutants | Generate the working diff (`git diff HEAD~1..HEAD > target/commit.diff`) and run `cargo mutants --in-diff target/commit.diff`. If any mutant survives, add a boundary or negative test until zero mutants survive (`SRDD Section 16.5.6`). |
| 7 | Commit atomically | Run `python3 scripts/verify_traceability.py --milestone M<N>`, stage the test and implementation together, and create one atomic commit following Section 0.4. |

## 0.3 Failing test triage: expected vs. unexpected failures

As defined in `SRDD Section 16.2`, failing tests divide into two categories. Treating an unexpected regression like an expected red test destroys the safety net of the suite.

| Failure type | When it occurs | Mandatory engineering response |
| --- | --- | --- |
| Expected failure | (a) A newly written test guard in Step 3 (`Red`) before its implementation lands, or (b) an existing test that pinned an old behavior being intentionally replaced by a new requirement or `ADR-*` decision. | For (a), write the implementation in Step 4 until green. For (b), update or retire the old test in the **same atomic commit** as the implementation change, and name the sanctioning requirement ID or `ADR-*` in the commit message. |
| Unexpected failure | Changing component X causes an existing test in component Y (or an unrelated contract in X) to fail. | **Stop immediately.** Do not loosen assertions, do not add `#[ignore]`, and do not tweak mock return values to force green. Diagnose whether an architectural boundary leaked or an invariant broke, fix the root cause, and add a regression test if a hidden bug was exposed. |

## 0.4 Atomic commit and branch discipline

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

## 0.5 Adaptive change-management and pivot protocol

When real-world conditions change during development, engineers follow the table below to adjust the plan cleanly without accumulating technical debt.

| Trigger event during development | How development changes | Cascade adjustment steps |
| --- | --- | --- |
| An upstream provider changes its `/models` or `/usage` JSON schema or auth header | Only the specific `ProviderAdapter` in `crates/proto` and its preset TOML change (`MNT-2`). `crates/core` and `crates/proxy` stay untouched. | 1. Capture the new live payload into `fixtures/usage/` or `fixtures/sse/`. 2. Update the golden contract test (Expected Failure citing the upstream change). 3. Update the parser in `crates/proto`. |
| A client harness (Claude Code, Codex CLI, opencode) introduces a new session header | Add the new header name to the precedence table in `SRDD Section 11.1` and `crates/core::session`. | 1. Update `SRDD Section 11.1`. 2. Add a table-test row in `core::session::tests`. 3. Add the header constant to the resolver slice. |
| SQLite write latency spikes when content traceability mode (`TRC-2`) is active at 500 req/s | Decouple trace payload persistence from the `usage_events` batch transaction using a dedicated trace ring buffer and separate `zstd` worker pool. | 1. Check `PERF-1` and `PERF-5` benchmarks. 2. Adjust batch interval or worker count in `crates/store::traces`. 3. Verify `REL-3` non-blocking overflow test still passes. |
| A sub-phase reveals a missing edge case in route eligibility or RBAC | Pause feature work in the current sub-phase and lock the boundary in `crates/core` first. | 1. Add the edge case to `SRDD Section 6` or `Section 8`. 2. Write the red unit test in `crates/core`. 3. Fix the pure logic and run `cargo mutants` on `crates/core`. |

## 0.6 Epic overview map (zero to v1.0.0)

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
