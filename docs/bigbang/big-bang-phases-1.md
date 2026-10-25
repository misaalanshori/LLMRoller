# Epic 1: Foundation, storage engine, cryptography, RBAC, and snapshot reloader

Epic 1 builds the seven-member Cargo workspace, the seam traits, the 31-table SQLite WAL store, and the `XChaCha20-Poly1305` secret sealer. It also implements atomic `/setup`, `ActorScope` RBAC, the hash-chained `audit_log`, and the `Reloader` pipeline. It corresponds to Milestone `M0` (`SRDD Sections 4, 5, 6.1, 6.2, 8, 9, 15, 16, 21`).

## 1.1 Sub-phase 1.1: Cargo workspace, `xtask` governance tools, foundational Docker scaffolding, and multi-arch CI matrix

This sub-phase initializes the seven-member repository layout (`SRDD Section 15.2`), foundational container scaffolding (`Dockerfile`, `.dockerignore`, `docker-compose.yml`, `.env.example`, `deploy/` per `SRDD Section 22`), and the Rust `xtask` binary. It also sets up compile-time safety, supply-chain checks, and the `POR-1` multi-arch build matrix (`MNT-1`, `MNT-2`, `MNT-3`, `SEC-10`, `Partial: POR-1`, `Partial: SEC-11`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 15.1`, `Section 15.2`, `Section 16.6`, `Section 17.1`, `Section 22.1`..`22.3`; Requirements `MNT-1`, `MNT-2`, `MNT-3`, `SEC-10`, `Partial: POR-1`, `Partial: SEC-11` |
| Crates touched | Root `Cargo.toml`, `Dockerfile`, `.dockerignore`, `docker-compose.yml`, `.env.example`, `deploy/{Caddyfile,prometheus.yml}`, `deny.toml`, `.cargo/mutants.toml`, `docs/srdd/requirements.toml`, `xtask`, skeleton `crates/{core,proto,store,proxy,admin,server}` |
| TDD step (`Red -> Green`) | Write an architectural dependency test (`tests/workspace_boundaries.rs`, `#[doc = "Spec: MNT-1, MNT-2, Partial: POR-1, Partial: SEC-11"]`) that inspects root `Cargo.toml` (`[workspace.lints.rust] unsafe_code = "forbid"`) plus each crate's `src/lib.rs` / `src/main.rs` header (`#![forbid(unsafe_code)]`), parses `cargo metadata` to assert `crates/core` has zero dependency edges to `tokio`, `sqlx`, `reqwest`, `hyper`, or `axum`, verifies that `crates/admin` cannot construct `ActorScope::System`, and validates `Dockerfile` (`planner`, `builder`, `runtime-distroless`, `runtime-dev`), `.dockerignore`, and `docker-compose.yml` (`docker compose config --quiet`). Add unit tests for `cargo xtask traceability`, `cargo xtask doc-consistency`, and `cargo xtask docs-bundle --check`. Configure CI to compile and run unit tests across all five `POR-1` targets (`x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`) and build the base `runtime-distroless` and `runtime-dev` images from the first commit. |
| Atomic commits | 1. `ci(workspace/1.1): initialize workspace with forbid(unsafe_code), deny.toml, multi-stage Dockerfile, and POR-1 build matrix [Spec: MNT-1, SEC-10, Partial: POR-1, Partial: SEC-11]` 2. `feat(xtask/1.1): implement xtask traceability, doc-consistency, and docs-bundle with boundary and Compose config tests [Spec: MNT-2]` |
| Risks and resolutions | **Risk:** Transitive dependencies pull in duplicate TLS stacks (`openssl-sys`) or C cross-compilation failures late in the project. **Resolution:** Set `default-features = false` on `reqwest` and `sqlx` and run the `cargo-zigbuild` musl matrix and `cargo deny check bans licenses advisories sources` on every PR starting in Sub-phase 1.1. |

## 1.2 Sub-phase 1.2: Core domain identifiers, `ActorScope` enum, and seam traits

This sub-phase defines the domain primitives in `crates/core` (`ULID` wrappers, `Role`, `ActorScope`, `SystemTask`, `OwnershipScope`, `RoutingScope`, `PoolTier`) and the dependency-injection seam traits (`SRDD Section 4.2, Section 16.4, Section 21.7`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.1`, `Section 4.2`, `Section 8.2`, `Section 15.2`, `Section 16.4`, `Section 21.7`; Requirements `USR-1`, `SEC-14`, `SEC-17`, `Partial: USR-2`, `Partial: USR-3`, `Partial: USR-4` |
| Crates touched | `crates/core::ids`, `crates/core::rbac`, `crates/core::seams`, `crates/proxy::clock` |
| TDD step (`Red -> Green`) | Write table-driven RBAC contract tests (`core::rbac::tests::actor_scope_matrix`, `#[doc = "Spec: USR-1, SEC-17, Partial: USR-2, Partial: USR-3, Partial: USR-4"]`) testing `ActorScope::User { user_id, role }` (`Role::Admin`, `Role::User`) and `ActorScope::System { task: SystemTask }` (`Writer`, `Reloader`, `Discovery`, `UsagePoller`, `RetentionSweeper`, `DataPlaneUsage`, `Cli`) against `global`, own `personal`, other users' `personal`, and `shared` resources. Verify that `crates/core::Clock` is purely synchronous (`now_ms`, `monotonic_ms`) while `crates/proxy::AsyncClock: Clock` adds `async fn sleep`. |
| Atomic commits | 1. `feat(core/1.2): define ULID types, SecretString wrappers, synchronous Clock, and EntropySource seams [Spec: SEC-14]` 2. `feat(core/1.2): implement ActorScope User and System variants and RBAC guards [Spec: USR-1, SEC-17, Partial: USR-2, Partial: USR-3, Partial: USR-4]` |
| Risks and resolutions | **Risk:** Background tasks pass a synthetic admin user ID to repository methods, polluting `audit_log` with fake admin actions. **Resolution:** Use `ActorScope::System { task }` with a constructor restricted to `crates/server` and `crates/store`, recording `actor_user_id = NULL` and `detail_json.system_task` on system audit rows (`SRDD Section 21.7`). |

## 1.3 Sub-phase 1.3: Cryptographic sealer (`XChaCha20-Poly1305`), 31-table SQLite WAL store, and resumable key rotation

This sub-phase implements `HKDF-SHA256` subkeys, canonical length-prefixed `aad(domain, fields)`, the `0x01 || nonce || ciphertext || tag` envelope, and `instance_secrets`. It also builds the 31-table SQLite WAL migration, `connect_options(role)`, and offline resumable `rotate-master-key` (`SRDD Section 8.2, Section 9.1, Section 21.6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 8.2`, `Section 9.1`, `Section 9.2`, `Section 21.6`; Requirements `SEC-1`, `SEC-2`, `Partial: PRV-5`, `Partial: SEC-19` |
| Crates touched | `crates/store::crypto`, `crates/store::migrations`, `crates/store::pool`, `fixtures/crypto/aad_vectors.json` |
| TDD step (`Red -> Green`) | 1. Write unit tests (`store::crypto::tests::aead_aad_and_rotation`, `#[doc = "Spec: SEC-1, SEC-2, Partial: PRV-5, Partial: SEC-19"]`) verifying: (a) missing or non-32-byte decoded `LLMROLLER_MASTER_KEY` or group/other-readable `LLMROLLER_MASTER_KEY_FILE` exits with code `3`, (b) deterministic test vectors in `fixtures/crypto/aad_vectors.json` match `aad(domain, fields)` across all six domains (`upstream_key`, `totp_secret`, `usage_raw`, `trace_block`, `trace_record`, `instance_secret`), (c) swapping ciphertext blobs across rows or columns fails AEAD authentication, and (d) `rotate-master-key` re-seals all six encrypted tables in 500-row batches while preserving `instance_secrets` (`session_hmac_secret` and `audit_hmac_key`), resumes cleanly from `rotation_cursor_table`/`rotation_cursor_id` after a simulated mid-rotation kill, and causes `serve` to exit with code `3` if `rotation_state = 'in_progress'` or `LLMROLLER_MASTER_KEY_ID != active_master_key_id`. 2. Write SQLite migration tests verifying `PRAGMA auto_vacuum == 2` (`INCREMENTAL`), `connect_options(role)` pragmas for `Writer`, `Readers`, and `Cli`, `_llmroller_schema(version)` forward-only guard (exit code `14` on newer schema), and all ownership/uniqueness triggers (`trg_client_keys_hash_disjoint`, `trg_upstream_keys_owner_*`, `trg_shares_integrity_*`, `trg_alias_not_canonical`). |
| Atomic commits | 1. `sec(store/1.3): implement HKDF-SHA256 subkeys, canonical length-prefixed AAD sealer, and committed test vectors [Spec: SEC-1, SEC-2, Partial: PRV-5, Partial: SEC-19]` 2. `feat(store/1.3): add connect_options builder, incremental auto_vacuum, and 31-table SQLite schema with triggers [Spec: SEC-17]` 3. `sec(store/1.3): implement instance_meta, instance_secrets, and resumable batched rotate-master-key CLI [Spec: SEC-1, SEC-2]` |
| Risks and resolutions | **Risk:** Per-connection pragmas (`foreign_keys`, `busy_timeout`, `query_only`) placed only in migration SQL do not apply to pool connections or CLI tools. **Resolution:** Enforce all pragmas inside the single `crates/store::pool::connect_options(role: Role)` constructor and assert their values by querying `PRAGMA` on every role in integration tests. |

## 1.4 Sub-phase 1.4: Atomic first-admin `/setup`, `Argon2id` + `TOTP` auth, and `reset-admin` CLI

This sub-phase implements the `${LLMROLLER_DATA_DIR}/setup-token` file (`0600`), serializable first-admin creation, permanent 404 lockout, `Argon2id` hashing, and `TOTP` verification. It also adds in-transaction `last_admin` protection, `llmroller reset-admin`, non-loopback TLS checks, random CSRF tokens, and the three-tier login limiter (`SRDD Section 6.1, Section 8.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 6.1`, `Section 8.2`; Requirements `AUTH-1`..`AUTH-9`, `SEC-4`, `SEC-5`, `Partial: SEC-9`, `Partial: SEC-12` |
| Crates touched | `crates/store::users`, `crates/admin::setup`, `crates/admin::auth`, `crates/server::cli` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/e2e_setup_and_auth.rs`, `#[doc = "Spec: AUTH-1, AUTH-2, AUTH-3, AUTH-4, AUTH-5, AUTH-6, AUTH-7, AUTH-8, AUTH-9, SEC-4, SEC-5, Partial: SEC-9, Partial: SEC-12"]`): (1) auto-generated setup token is written to `${LLMROLLER_DATA_DIR}/setup-token` with mode `0600` (only the path logged to stderr) and deleted on setup completion, (2) 20 concurrent `POST /admin/api/setup` requests result in exactly one HTTP 201, 19 HTTP 409/404 responses, and `SELECT COUNT(*) FROM users == 1`, (3) two admins concurrently demoting or disabling each other inside `BEGIN IMMEDIATE` serialize so the second returns HTTP 409 `last_admin` (`AUTH-5`), (4) `llmroller reset-admin --username <name>` restores admin access offline while holding `llmroller.lock`, (5) 1,000 bad passwords for `admin` across 100 IPs trigger TOTP/2s delay without locking out `admin` from a new IP, while trusted-proxy `X-Forwarded-For` extraction isolates users behind `LLMROLLER_TRUSTED_PROXIES`, and (6) non-loopback control-plane bind without TLS or `LLMROLLER_TRUSTED_PROXIES` exits with code `2`, login sets `__Host-llmroller_login_csrf`, and authenticated sessions use random 256-bit CSRF tokens (`csrf_hash = SHA-256(token)`). |
| Atomic commits | 1. `feat(store/1.4): implement user repository with Argon2id, TOTP, in-transaction last_admin guard, and reset-admin CLI [Spec: AUTH-4, AUTH-5, AUTH-6, AUTH-9, SEC-4]` 2. `feat(admin/1.4): implement setup-token file bootstrap, serializable /setup, CSRF cookies, and three-tier login limiter [Spec: AUTH-1, AUTH-2, AUTH-3, AUTH-7, AUTH-8, SEC-5, Partial: SEC-9, Partial: SEC-12]` |
| Risks and resolutions | **Risk:** `Argon2id` (`>= 19 MiB` memory per hash) can exhaust RAM or stall under login floods. **Resolution:** Bound concurrent `Argon2id` verifications with a semaphore (`max(2, available_parallelism)` permits, 5 s queue timeout returning `503 login_busy`) and evaluate the three-tier limiter before hashing. |

## 1.5 Sub-phase 1.5: Tamper-evident `audit_log` HMAC chain, external anchors, and `verify-audit-log` CLI

This sub-phase implements the `HMAC-SHA256` hash chain on `audit_log` ordered by `seq ASC` using `audit_hmac_key` from `instance_secrets`. It also adds periodic `audit_anchor` stdout log emission and the `llmroller verify-audit-log` CLI (`AUD-1`, `SEC-13`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 6.11`, `Section 8.2`, `Section 9.2.1`; Requirements `AUD-1`, `SEC-13` |
| Crates touched | `crates/store::audit`, `crates/server::cli` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`store::audit::tests::hash_chain_verification`, `#[doc = "Spec: AUD-1, SEC-13"]`) verifying: (1) inserting 10 audit rows (including simulated backward NTP wall-clock steps on `ts`) verifies cleanly over `seq ASC` from the 64-ASCII-zero genesis `prev_hash`, (2) mutating one character in row 4 or deleting row 5 (leaving a `seq` gap) causes `verify-audit-log` to exit with code `8`, (3) every 100 rows (and hourly) emits `{"event":"audit_anchor","seq":...,"hash":...,"ts":...}` to stdout, and (4) deleting the last 3 rows causes `verify-audit-log --anchor-seq N --anchor-hash H` to exit with code `9` (`truncation detected`). |
| Atomic commits | 1. `sec(store/1.5): implement seq-ordered HMAC-SHA256 audit_log chain, stdout anchors, and verify-audit-log CLI [Spec: AUD-1, SEC-13]` |
| Risks and resolutions | **Risk:** Rotating the master key would invalidate the audit chain if the HMAC key were derived directly from the master key. **Resolution:** Sign the chain with the random 256-bit `audit_hmac_key` stored in `instance_secrets` (re-sealed on master-key rotation), keeping historical signatures valid across rotations. |

## 1.6 Sub-phase 1.6: Dual-priority SQLite writer, closed `settings` registry, `Reloader` task, and `RuntimeState`

This sub-phase implements the dual-queue SQLite writer (`Control` priority queue and `4 MiB` `Data` batches) and the closed 14-key `settings` registry (including `default_routing_strategy` and `LLMROLLER_DEFAULT_ROUTING_STRATEGY`). It also adds `RuntimeState` and the single `Reloader` task that publishes versioned per-domain `Arc` slices into `ArcSwap<ConfigSnapshot>` (`REL-4`, `Partial: RTE-14`, `SRDD Section 21.6`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.4`, `Section 7.2`, `Section 9.2`, `Section 9.3.3`, `Section 21.6`; Requirements `REL-4` |
| Crates touched | `crates/store::writer`, `crates/store::settings`, `crates/core::snapshot`, `crates/core::runtime_state`, `crates/server::reloader` |
| TDD step (`Red -> Green`) | Write concurrency and latency tests (`tests/integration_snapshot_reloader.rs`, `#[doc = "Spec: REL-4"]`): (1) 50 concurrent alternating control-plane mutations commit through the `Control` queue (incrementing `instance_meta.config_version` in the same transaction) and the single `Reloader` task coalesces notifications, rebuilds only changed domain `Arc` slices in one read transaction, never publishes an older `config_version` over a newer one, and applies `CacheInvalidationEvent`s after `arc_swap.store(...)`, (2) while the `Data` queue is saturated with `4 MiB` batches, a control-plane mutation commits and propagates to `ConfigSnapshot` within `1 s` (`REL-4`), and (3) unknown keys in `settings` are rejected with HTTP 422. |
| Atomic commits | 1. `feat(store/1.6): implement dual-priority Control/Data writer actor and closed 14-key settings registry with default_routing_strategy [Spec: REL-4, Partial: RTE-14]` 2. `feat(server/1.6): implement versioned Reloader task, per-domain ConfigSnapshot slices, and RuntimeState [Spec: REL-4]` |
| Risks and resolutions | **Risk:** Two concurrent handlers calling `reload_snapshot()` directly can publish an older snapshot over a newer revocation. **Resolution:** Only the single `Reloader` task writes to `ArcSwap<ConfigSnapshot>`, gated on `v > published.version`, and handlers await the reloader's version notification (up to 1 s) before returning 2xx (`SRDD Section 21.6.2`). |

### 1.6.1 Incremental `ConfigSnapshot` domain slices across Epics 1 through 5

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
