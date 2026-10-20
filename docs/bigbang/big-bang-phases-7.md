# Epic 7: System hardening, fuzzing, load verification, live harness E2E, and v1.0.0 release

Epic 7 runs extended fuzzing and canary verification (`7.1`), full-workspace mutation testing and traceability checks (`7.2`), and 500 req/s and 2,000-stream soak benchmarks (`7.3`). It also verifies `llmroller backup`, CLI exit codes `0..14`, headless CLI agent E2E workflows (`7.4`), and signed release packaging (`7.5`). It corresponds to Milestone `M6` (`SRDD Sections 5.5, 7, 8, 16, 17, 18, 19, 21`).

## 7.1 Sub-phase 7.1: Extended fuzzing campaigns (`cargo-fuzz`) and full-stack canary secret verification

This sub-phase runs extended fuzzing campaigns across all six fuzz targets (`headers`, `body_edit`, `sse_frame`, `sse_tap`, `trace_tap`, `step8_fingerprint`). It also runs the full-system canary secret redaction suite (`SEC-6`, `SEC-14`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 8.2`, `Section 16.5.3`; Requirements `SEC-6`, `SEC-8`, `SEC-14`, `SEC-15`, `TRN-2` |
| Crates touched | `fuzz/fuzz_targets/*`, `tests/security_canary_and_ssrf.rs` |
| TDD step (`Red -> Green`) | 1. Run all six `cargo-fuzz` targets (`headers`, `body_edit`, `sse_frame`, `sse_tap`, `trace_tap`, `step8_fingerprint`) asserting zero panics, zero out-of-bounds slices, zero duplicate-key bypasses, and bounded memory allocation on arbitrary byte streams. 2. Run `tests/security_canary_and_ssrf.rs` (`#[doc = "Spec: SEC-6, SEC-14"]`) planting canary secrets in upstream keys, client keys, cookies, setup tokens, and master keys, forcing upstream 401/403/500 errors, echoed `sk-*`/`llr_*` strings in upstream error bodies, malformed SSE streams, CSV exports with formula prefixes (`=`, `+`, `-`, `@`, `	`, `
`), and simulated handler panics, and asserting zero occurrences of any canary substring across stdout/stderr JSON logs, HTTP responses, `/metrics`, `audit_log`, and SQLite tables. |
| Atomic commits | 1. `sec(tests/7.1): complete six-target cargo-fuzz suite and full-stack canary redaction and CSV formula-injection verification [Spec: SEC-6, SEC-8, SEC-14, SEC-15, TRN-2]` |
| Risks and resolutions | **Risk:** Upstream error bodies occasionally echo the `Authorization` bearer token back in an error message, leaking the key into `last_error` or logs. **Resolution:** Scrub all `sk-*`, `llr_*`, `user_*`, and configured secret substrings from upstream error bodies before persisting `last_error` or logging (`SRDD Section 8.2 SEC-6`). |

## 7.2 Sub-phase 7.2: Full-workspace mutation sweep (`cargo-mutants`) and 100% `xtask traceability` gate

This sub-phase runs the full workspace `cargo mutants --workspace` sweep (`SRDD Section 16.5.6`). It also runs `cargo xtask traceability --milestone M6`, `cargo xtask doc-consistency`, and `cargo xtask docs-bundle --check` (`SRDD Section 16.6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 16.5.6`, `Section 16.6`; Requirements `G8`, `ADR-8`, all requirement IDs in `docs/srdd/requirements.toml` |
| Crates touched | All test modules across `crates/{core,proto,store,proxy,admin,server}`, `xtask` |
| TDD step (`Red -> Green`) | Run `cargo mutants --workspace` across all crates, verify that every `#[mutants::skip]` annotation carries an `// equivalent: <reason>` comment, and add boundary or negative tests for any unannotated surviving mutant until zero survive. Run `cargo xtask traceability --milestone M6` and `cargo xtask doc-consistency` to confirm that 100% of requirement IDs in `docs/srdd/requirements.toml` and `SRDD Sections 6, 7, 8` map to passing `#[doc = "Spec: ..."]` tests (with zero `Partial:`-only requirements remaining). |
| Atomic commits | 1. `test(workspace/7.2): eliminate remaining surviving mutants and verify equivalent-mutant annotations across workspace [Spec: G8, ADR-8]` 2. `ci(workspace/7.2): enforce cargo xtask traceability --milestone M6 and doc-consistency gates in CI [Spec: G8]` |
| Risks and resolutions | **Risk:** Unreviewed `#[mutants::skip]` attributes mask real behavioral regressions. **Resolution:** Fail CI in `xtask` if any `#[mutants::skip]` lacks `// equivalent: <reason>` and enforce CODEOWNERS approval on skip changes (`SRDD Section 16.5.6`). |

## 7.3 Sub-phase 7.3: Full-scale load and soak benchmarks (`PERF-1`..`PERF-5`), 50M-row DB verification, and CLI exit-code/backup resilience (`REL-1`..`REL-6`)

This sub-phase runs the full-scale latency and memory soak benchmarks (`PERF-1`..`PERF-5`) against a 50M-row database and log-normal TTFF stub cluster. It also verifies crash-safe `llmroller backup` (`REL-6`) and all CLI exit codes `0..14` (`SRDD Section 5.5, Section 7.1, Section 7.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 7.1`, `Section 7.2`, `Section 21.8`; Requirements `PERF-1`..`PERF-5`, `REL-1`..`REL-6` |
| Crates touched | `benches/proxy_latency.rs`, `tests/e2e_sigterm_drain.rs`, `tests/e2e_kill9_recovery.rs`, `crates/server::backup`, `crates/server::cli` |
| TDD step (`Red -> Green`) | 1. Seed a SQLite database with `50,000,000` `usage_events` rows, 7 days of `usage_rollups_5min` for 500 keys, and `250,000` confirmed pins; assert cold start to `/readyz` HTTP 200 completes in `<= 2 s` and idle RSS (up to 10,000 routes) stays `<= 50 MB` (`PERF-4`). 2. Run the load harness (`500 req/s` for `<= 256 KiB` bodies and `budget / (mean_body * ttff)` for `64 KiB`, `512 KiB`, and `2 MiB` bodies against the log-normal TTFF stub, plus `2,000` open SSE streams with `traceability_config.enabled` both `false` and `true`) and assert: p50 overhead `<= 1 ms` and p99 `<= 5 ms` (`PERF-1`), per-frame forwarding delay `<= 1 ms` (`PERF-2`), RSS `<= 300 MB` plus in-flight pre-first-byte bodies (`PERF-3`), and traceability RSS within `PERF-3` plus the `256 MiB` global trace accumulator budget using the 2-thread sealer pool (`PERF-5`). 3. Run `llmroller backup --out <dest>` during active write load and verify it writes `<dest>.tmp` in the destination directory, `fsync`s the file, runs `PRAGMA integrity_check`, renames to `<dest>`, `fsync`s the directory, refuses to overwrite without `--overwrite`, and outputs JSON including `master_key_id` (`REL-6`). 4. Verify deterministic CLI exit codes `0` and `2..14` (`SRDD Section 5.5`). |
| Atomic commits | 1. `feat(server/7.3): implement crash-safe fsync llmroller backup CLI with master_key_id output and verify exit codes 0..14 [Spec: REL-6]` 2. `perf(workspace/7.3): verify PERF-1 through PERF-5 and REL-1 through REL-5 against 50M-row DB and 2,000-stream soak [Spec: PERF-1, PERF-2, PERF-3, PERF-4, PERF-5, REL-1, REL-2, REL-3, REL-4, REL-5]` |
| Risks and resolutions | **Risk:** Writing `<dest>.tmp` in `/tmp` fails `rename` (`EXDEV`) when `<dest>` is on another mount, and omitting file/directory `fsync` leaves a truncated backup on power loss. **Resolution:** Create `<dest>.tmp` inside the parent directory of `<dest>` and `fsync` both the file and parent directory around `rename` (`SRDD Section 5.5`). |

## 7.4 Sub-phase 7.4: Headless CLI harness compatibility (`COM-1`), Responses session inheritance, and network sandbox egress check (`PRIV-1`)

This sub-phase runs headless E2E workflows with `Claude Code`, `Codex CLI`, `opencode`, and the official OpenAI and Anthropic SDKs (`COM-1`). It also verifies stateful `previous_response_id` session inheritance, network sandbox egress isolation (`PRIV-1`), and the opt-in live provider scripts (`SRDD Section 7.3, Section 19.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 7.3`, `Section 7.4`, `Section 16.5.5`, `Section 19.2`; Requirements `COM-1`, `OBS-1`, `PRIV-1` |
| Crates touched | `tests/e2e_cli_harnesses.rs`, `tests/e2e_lifecycle.rs`, `scripts/verify_cache_affinity.sh` |
| TDD step (`Red -> Green`) | Run `tests/e2e_cli_harnesses.rs` (`#[doc = "Spec: COM-1, OBS-1, PRIV-1"]`) pointing each CLI harness and SDK at LLMRoller backed by multi-turn tool-calling stub upstreams: (1) verify that `Claude Code` (including subagents with `x-claude-code-agent-id` and `POST /v1/messages/count_tokens`), `Codex CLI` (chaining `previous_response_id` across turns into a single `session_hash`, one `pins` row, and a unified multi-turn chat preview), `opencode`, and Python/TypeScript SDKs complete multi-turn streaming tool-call loops with 100% pin stickiness, and (2) verify under network namespace monitoring that LLMRoller emits zero outbound packets to any host outside configured upstream origins (`PRIV-1`). |
| Atomic commits | 1. `test(e2e/7.4): add headless CLI harness suite for Claude Code, Codex CLI Responses chains, opencode, and SDKs with egress sandbox check [Spec: COM-1, OBS-1, PRIV-1]` 2. `feat(scripts/7.4): add opt-in live cache-affinity and /usage verification script [Spec: RTE-2, USG-4]` |
| Risks and resolutions | **Risk:** `Codex CLI` rotates `previous_response_id` on every turn (`resp_A`, `resp_B`, `resp_C`), which would split a stateful conversation into separate `session_hash` pins if `previous_response_id` were hashed as a session ID. **Resolution:** Inherit `link.session_hash` from `response_links` (`SRDD Section 11.1.2`) so all turns of a Codex conversation share one `session_hash` and one multi-turn chat preview. |

## 7.5 Sub-phase 7.5: Multi-arch static builds, `< 50 MB` distroless container, `docker compose` smoke & routing gate, CycloneDX SBOM, `cosign` signing, and `v1.0.0` release

This sub-phase packages static binaries for all five `POR-1` targets and builds the `< 50 MB` distroless and dev containers (`Section 22.1`). It also runs the containerized `docker compose` smoke, routing-strategy, and `SIGTERM` drain gate (`POR-1`, `SEC-11`, `REL-1`, `RTE-14`). It then generates the CycloneDX SBOM, signs all release files with `cosign`, and tags `v1.0.0`.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 7.4`, `Section 8.2`, `Section 10.3`, `Section 22.1`..`22.4`; Requirements `POR-1`, `REL-1`, `RTE-14`, `SEC-10`, `SEC-11` |
| Crates touched | `.github/workflows/release.yml`, `Dockerfile`, `.dockerignore`, `docker-compose.yml`, `.env.example`, `deploy/{Caddyfile,prometheus.yml}`, `scripts/verify_release_package.sh` |
| TDD step (`Red -> Green`) | Run `scripts/verify_release_package.sh` (`#[doc = "Spec: POR-1, REL-1, RTE-14, SEC-10, SEC-11"]`) asserting: (1) `runtime-distroless` image size is `< 50 MB`, runs as UID `65532:65532` with all Linux capabilities dropped and a read-only root filesystem, (2) `docker compose --profile dev up -d --wait` runs `llmroller-init` to initialize `/var/lib/llmroller` ownership (`65532:65532`) and converges `llmroller` to `healthy` via `llmroller healthcheck --url http://127.0.0.1:8080/readyz`, (3) integration tests against the containerized stack verify live routing strategy switching across `smooth_weighted_rr`, `naive_rr`, `weighted_rr`, `uniform_random`, `weighted_random`, and `power_of_two_choices` via both `LLMROLLER_DEFAULT_ROUTING_STRATEGY` env vars and `PATCH /admin/api/settings` / `PATCH /admin/api/models/{canonical_id}` (`llmroller_routing_selections_total{strategy=...}`), (4) `docker compose stop llmroller` sends `SIGTERM` and completes the 8-step graceful drain with exit code `0` within `stop_grace_period: 90s`, (5) `llmroller --version`, `generate-master-key`, `backup`, `rotate-master-key`, `verify-audit-log`, and `reset-admin` succeed against the container volume, and (6) SHA-256 checksums, CycloneDX SBOM, and `cosign` signatures validate cleanly. |
| Atomic commits | 1. `ci(release/7.5): finalize multi-arch static release packaging, <50MB distroless container, SBOM, cosign signing, and --version contract [Spec: POR-1, SEC-10, SEC-11]` 2. `test(docker/7.5): add docker compose healthcheck convergence, routing-strategy switching, and SIGTERM drain integration gate [Spec: POR-1, REL-1, RTE-14, SEC-11]` |
| Risks and resolutions | **Risk:** Drift between the split SRDD / Big-Bang files and the combined bundle files at release time. **Resolution:** Run `cargo xtask docs-bundle --check`, `cargo xtask doc-consistency`, and `cargo xtask traceability --milestone M6` as mandatory release gates before tagging `v1.0.0`. |
