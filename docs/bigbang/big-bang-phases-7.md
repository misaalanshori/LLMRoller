# Epic 7: System hardening, fuzzing, load verification, live harness E2E, and v1.0.0 release

Epic 7 hardens the gateway through fuzzing, full-workspace mutation testing, and 500 req/s load benchmarks. It also verifies crash recovery, headless CLI agent compatibility (`Claude Code`, `Codex CLI`, `opencode`), and signed release packaging. It corresponds to Milestone `M6` (`SRDD Sections 7, 8, 16, 17, 18, 19`).

## 7.1 Sub-phase 7.1: Structure-aware fuzzing (`cargo-fuzz`) and canary secret verification

This sub-phase runs fuzzing campaigns against all untrusted wire parsers (`body_edit`, `sse_tap`, `trace_tap`, `headers`). It also runs the full-system canary secret redaction test (`SEC-6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 8.2`, `Section 16.5.3`; Requirements `SEC-6`, `SEC-8`, `SEC-15`, `TRN-2` |
| Crates touched | `fuzz/fuzz_targets/*`, `tests/security_canary_and_ssrf.rs` |
| TDD step (`Red -> Green`) | 1. Write four `cargo-fuzz` targets asserting zero panics, zero out-of-bounds slices, and bounded memory allocation on arbitrary byte streams. 2. Run `tests/security_canary_and_ssrf.rs` (`#[doc = "Spec: SEC-6, SEC-14"]`) planting canary secrets in upstream keys, client keys, cookies, and master keys, forcing upstream 401/500 errors, malformed SSE streams, and simulated handler panics, and asserting zero occurrences of any canary substring across stdout/stderr JSON logs, HTTP responses, `audit_log`, and `usage_events`. |
| Atomic commits | 1. `test(fuzz/7.1): add cargo-fuzz targets for JSON span editor, SSE tap, trace reassembler, and header parser [Spec: SEC-8, SEC-15, TRN-2]` 2. `sec(tests/7.1): add full-stack canary secret redaction test across logs, errors, and SQLite tables [Spec: SEC-6, SEC-14]` |
| Risks and resolutions | **Risk:** A fuzzer discovers a multi-byte UTF-8 boundary split across two `Bytes` chunks in `SseUsageTap` or `TraceTap` that causes a slice panic. **Resolution:** Convert any discovered crash input into a permanent regression test under `crates/proto/tests/regressions.rs` (`SRDD Section 16.5.4`) and use `std::str::from_utf8` with incomplete trailing UTF-8 byte carryover across chunk boundaries. |

## 7.2 Sub-phase 7.2: Full-workspace mutation sweep (`cargo-mutants`) and 100% traceability gate

This sub-phase runs a full workspace `cargo mutants --workspace` sweep (`SRDD Section 16.5.6`). It also runs `scripts/verify_traceability.py` (`SRDD Section 16.6`) to verify bidirectional links across all SRDD requirements.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 16.5.6`, `Section 16.6`; Requirements `G8`, `ADR-8`, all requirement IDs in `Sections 6, 7, 8` |
| Crates touched | All test modules across `crates/{core,proto,store,proxy,admin,server}`, `scripts/verify_traceability.py` |
| TDD step (`Red -> Green`) | Run `cargo mutants --workspace` and inspect every surviving mutant. For each survivor, write the missing boundary or negative contract test that kills the mutant. Run `python3 scripts/verify_traceability.py` and confirm 100% of requirement IDs in `6. SRDD - Functional Requirements.md`, `7. SRDD - Non-Functional Requirements.md`, and `8. SRDD - Security and Privacy.md` map to at least one passing test. |
| Atomic commits | 1. `test(workspace/7.2): eliminate remaining surviving mutants across core, proto, store, proxy, and admin [Spec: G8, ADR-8]` 2. `ci(workspace/7.2): enforce 100% SRDD-to-test traceability check in CI [Spec: G8]` |
| Risks and resolutions | **Risk:** Full-workspace mutation testing takes too long on every pull request. **Resolution:** Keep `cargo mutants --in-diff` (typically `< 90 s`) as the blocking PR gate and run full `cargo mutants --workspace` in the nightly and pre-release CI workflow (`SRDD Section 16.5.6`). |

## 7.3 Sub-phase 7.3: Load and soak benchmarks (`PERF-1`..`PERF-5`) and crash/drain resilience (`REL-1`..`REL-6`)

This sub-phase validates the latency and memory targets (`PERF-1`..`PERF-5`) on a 2 vCPU, 2 GB RAM host against a stub upstream cluster. It also verifies graceful `SIGTERM` draining, `SIGKILL` WAL durability, and online SQLite backups (`REL-1`..`REL-6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.1`, `Section 7.2`; Requirements `PERF-1`..`PERF-5`, `REL-1`..`REL-6` |
| Crates touched | `benches/proxy_latency.rs`, `tests/e2e_sigterm_drain.rs`, `tests/e2e_kill9_recovery.rs`, `crates/server::shutdown`, `crates/server::backup` |
| TDD step (`Red -> Green`) | 1. Run the load harness (`500 req/s` and `2,000` concurrent SSE streams, both with `content_traceability_mode` off and on) and assert: p50 overhead `<= 1 ms`, p99 overhead `<= 5 ms` (`PERF-1`), chunk forwarding delay `<= 1 ms` (`PERF-2`), RSS `<= 300 MB` at 2,000 streams (`PERF-3`), idle RSS `<= 50 MB` and startup `<= 2 s` (`PERF-4`), and traceability TTFB delta `<= 0.5 ms` (`PERF-5`). 2. Send `SIGTERM` with 50 active streams and assert all 50 complete cleanly (`REL-1`). 3. Send `SIGKILL` (`kill -9`) 2 s after confirming pins and usage rows and assert zero lost pins or usage rows on restart (`REL-2`). |
| Atomic commits | 1. `feat(server/7.3): implement graceful SIGTERM stream drain and llmroller backup CLI command [Spec: REL-1, REL-6]` 2. `perf(workspace/7.3): add load benchmark harness and verify PERF-1 through PERF-5 and REL-2 [Spec: PERF-1, PERF-2, PERF-3, PERF-4, PERF-5, REL-2]` |
| Risks and resolutions | **Risk:** Under 2,000 open streams with `content_traceability_mode` enabled, per-stream trace buffers could push RSS above `300 MB`. **Resolution:** Stream trace deltas in 16 KiB chunks into a per-stream incremental `zstd::stream::Encoder` (which compresses ~5x in flight) so a 100 KiB transcript occupies `< 20 KiB` of RAM while streaming. |

## 7.4 Sub-phase 7.4: Headless CLI harness compatibility (`COM-1`) and Milestone 1 live checks

This sub-phase runs headless E2E workflows with `Claude Code`, `Codex CLI`, `opencode`, and the OpenAI and Anthropic SDKs (`COM-1`). It also runs the opt-in live provider verification scripts (`SRDD Section 19.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.3`, `Section 16.5.5`, `Section 19.2`; Requirements `COM-1`, `OBS-1`, `PRIV-1` |
| Crates touched | `tests/e2e_cli_harnesses.rs`, `scripts/verify_cache_affinity.sh` |
| TDD step (`Red -> Green`) | Run `tests/e2e_cli_harnesses.rs` (`#[doc = "Spec: COM-1, OBS-1, PRIV-1"]`) pointing each CLI tool and SDK at LLMRoller backed by multi-turn tool-calling stub upstreams. Assert that: (1) every multi-turn tool-call loop completes, (2) all turns in one CLI invocation hit the same pinned `upstream_key_id`, (3) when content traceability mode is enabled, the entire multi-turn tool-call conversation renders accurately in `GET /api/user/sessions/{session_hash}/chat`, and (4) network egress monitoring confirms zero outbound packets to any host outside the configured upstream origins (`PRIV-1`). |
| Atomic commits | 1. `test(e2e/7.4): add headless CLI harness compatibility suite for Claude Code, Codex CLI, opencode, and SDKs [Spec: COM-1, OBS-1, PRIV-1]` 2. `feat(scripts/7.4): add opt-in live cache-affinity and /usage verification script [Spec: RTE-2, USG-4]` |
| Risks and resolutions | **Risk:** A new release of `Claude Code` or `Codex CLI` changes a path or header formatting expectation. **Resolution:** Lock the recorded inbound HTTP requests from each CLI version into `fixtures/harnesses/` so CI runs them deterministically on every PR even when the external CLI binary is not installed in a minimal container. |

## 7.5 Sub-phase 7.5: Multi-arch static builds, distroless container, SBOM, `cosign` signing, and v1.0.0 release

This sub-phase builds static binaries for Linux (`x86_64`, `aarch64`) and macOS (`aarch64-apple-darwin`) and packages the `< 50 MB` distroless container (`POR-1`, `SEC-11`). It then generates the CycloneDX SBOM, signs all release files with `cosign`, and tags `v1.0.0`.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.4`, `Section 8.2`, `Section 12.1`; Requirements `POR-1`, `SEC-10`, `SEC-11` |
| Crates touched | `.github/workflows/release.yml`, `Dockerfile`, `docker-compose.yml` |
| TDD step (`Red -> Green`) | Run the release packaging smoke test (`scripts/verify_release_package.sh`, `#[doc = "Spec: POR-1, SEC-11"]`) asserting: (1) container image compressed/uncompressed size is `< 50 MB`, (2) container runs as UID `65532` with a read-only root filesystem, (3) `llmroller --version`, `llmroller backup`, `llmroller rotate-master-key`, and `llmroller verify-audit-log` succeed on a fresh container start, and (4) SHA-256 checksums and SBOM validate cleanly. |
| Atomic commits | 1. `ci(release/7.5): add multi-arch musl build matrix, distroless Dockerfile (<50MB), SBOM, and cosign signing [Spec: POR-1, SEC-10, SEC-11]` |
| Risks and resolutions | **Risk:** `zstd` C bindings complicate cross-compilation to `aarch64-unknown-linux-musl`. **Resolution:** Compile with `cargo-zigbuild` in CI (which provides a hermetic musl C/C++ cross-toolchain for `x86_64` and `aarch64`) or enable pure-Rust compression fallback where needed. |
