# Epic 1: Foundation, storage engine, cryptography, and multi-role auth

Epic 1 builds the Cargo workspace, the six seam traits, the 21-table SQLite WAL store, and the `XChaCha20-Poly1305` secret sealer. It also implements atomic `/setup` and role-based access control for `admin` and `user` accounts. It corresponds to Milestone `M0` (`SRDD Sections 4, 5, 6.1, 6.2, 8, 9, 15, 16`).

## 1.1 Sub-phase 1.1: Cargo workspace, lint gates, and traceability harness

This sub-phase initializes the six-crate repository layout (`SRDD Section 15.2`) and enforces compile-time safety and supply-chain checks (`MNT-1`, `MNT-2`, `SEC-10`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 15.1`, `Section 15.2`, `Section 16.6`; Requirements `MNT-1`, `MNT-2`, `SEC-10` |
| Crates touched | Root `Cargo.toml`, `deny.toml`, `.cargo/mutants.toml`, `scripts/verify_traceability.py`, skeleton `crates/{core,proto,store,proxy,admin,server}` |
| TDD step (`Red -> Green`) | Write an architectural dependency test (`tests/workspace_boundaries.rs`, `#[doc = "Spec: MNT-1, MNT-2"]`) that inspects root `Cargo.toml` (`[workspace.lints.rust] unsafe_code = "forbid"`) plus each crate's `src/lib.rs` / `src/main.rs` header (`#![forbid(unsafe_code)]`), and parses `cargo metadata` to assert `crates/core` has zero dependency edges to `tokio`, `sqlx`, `reqwest`, `hyper`, or `axum`. |
| Atomic commits | 1. `ci(workspace/1.1): initialize six-crate workspace with forbid(unsafe_code) and deny.toml [Spec: MNT-1, SEC-10]` 2. `test(workspace/1.1): add cargo-metadata boundary test and verify_traceability.py [Spec: MNT-2]` |
| Risks and resolutions | **Risk:** Transitive dependencies pull in duplicate TLS stacks (`openssl-sys`) or unmaintained crates. **Resolution:** Set `default-features = false` on `reqwest` and `sqlx` immediately and run `cargo deny check bans licenses advisories` on the very first commit. |

## 1.2 Sub-phase 1.2: Core domain identifiers, `ActorScope`, and seam traits

This sub-phase defines the fundamental domain primitives in `crates/core` (`ULID` wrappers, `Role`, `ActorScope`, `OwnershipScope`, `RoutingScope`) and the six dependency-injection seam traits (`SRDD Section 16.4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.1`, `Section 4.2`, `Section 8.2`, `Section 16.4`; Requirements `USR-1`..`USR-5`, `SEC-14`, `SEC-17` |
| Crates touched | `crates/core::ids`, `crates/core::rbac`, `crates/core::seams` |
| TDD step (`Red -> Green`) | Write table-driven RBAC contract tests (`core::rbac::tests::actor_scope_matrix`, `#[doc = "Spec: USR-1, USR-2, USR-3, USR-4, SEC-17"]`) testing every `(Role::Admin, Role::User)` action against `global` resources, own `personal` resources, other users' `personal` resources, and `shared` resources. Implement `ActorScope::can_view_provider`, `can_edit_provider`, `can_share_key`, and `can_view_trace` until green, then run `cargo mutants` on `crates/core::rbac`. |
| Atomic commits | 1. `feat(core/1.2): define ULID types, SecretString wrappers, Clock and EntropySource seams [Spec: SEC-14]` 2. `feat(core/1.2): implement ActorScope RBAC guards for admin and standard user roles [Spec: USR-1, USR-2, USR-3, USR-4, SEC-17]` |
| Risks and resolutions | **Risk:** A future handler forgets to check `ActorScope` before running a query. **Resolution:** Make `&ActorScope` a mandatory parameter on every `StorageRepository` trait method so code that omits the scope fails to compile (`SEC-17`). |

## 1.3 Sub-phase 1.3: Cryptographic sealer (`XChaCha20-Poly1305`) and SQLite WAL store

This sub-phase implements master-key derivation (`HKDF-SHA256`), AEAD encryption with row-bound Associated Data (`AAD`), key rotation, and the 21-table SQLite WAL migration (`SRDD Section 8.2, Section 9.1`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.5`, `Section 8.2`, `Section 9.1`, `Section 9.2`; Requirements `PRV-5`, `SEC-1`, `SEC-2`, `SEC-3`, `SEC-14` (and HKDF subkey prerequisite for `SEC-19`) |
| Crates touched | `crates/store::crypto`, `crates/store::migrations`, `crates/store::pool` |
| TDD step (`Red -> Green`) | 1. Write unit tests (`store::crypto::tests`, `#[doc = "Spec: SEC-1, SEC-2, PRV-5"]`) verifying that: (a) missing or `< 32-byte` `LLMROLLER_MASTER_KEY` returns `StartupError::InvalidMasterKey`, (b) sealing the same secret twice yields distinct ciphertexts (random 24-byte nonce), (c) swapping ciphertext blobs between two `upstream_key.id` rows fails AEAD authentication (`AAD` mismatch), and (d) `rotate_master_key` re-seals all rows under a new `master_key_id`. 2. Write SQLite WAL migration tests verifying all 21 tables and foreign-key constraints. |
| Atomic commits | 1. `sec(store/1.3): implement HKDF-SHA256 domain-separated subkeys, persistent session_hmac_secret, XChaCha20-Poly1305 sealer, and generate/rotate-master-key CLI [Spec: SEC-1, SEC-2, PRV-5]` 2. `feat(store/1.3): add flock single-instance guard, 21-table SQLite WAL migration, and serialized batch writer actor [Spec: REL-2, SEC-17]` |
| Risks and resolutions | **Risk:** `SQLITE_BUSY` errors under concurrent read and write load. **Resolution:** Configure `SqliteConnectOptions` with `journal_mode(Wal)`, `synchronous(Normal)`, `busy_timeout(5s)`, an 8-connection read-only pool, and a single serialized writer connection (`SRDD Section 9.2`). |

## 1.4 Sub-phase 1.4: Atomic first-admin `/setup` and `Argon2id` + `TOTP` authentication

This sub-phase implements the one-time setup token, serializable first-admin creation, permanent 404 lockout after setup, `Argon2id` password hashing, and `TOTP` two-factor verification (`SRDD Section 6.1, Section 8.2`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.1`, `Section 8.2`; Requirements `AUTH-1`..`AUTH-9`, `USR-5`, `SEC-4`, `SEC-5`, `SEC-12` |
| Crates touched | `crates/store::users`, `crates/admin::setup`, `crates/admin::auth` |
| TDD step (`Red -> Green`) | Write an integration race test (`tests/e2e_setup_and_auth.rs`, `#[doc = "Spec: AUTH-1, AUTH-2, AUTH-3, SEC-4, SEC-5"]`) that fires 20 concurrent `POST /admin/api/setup` requests with a valid setup token against an empty database. Assert that exactly one returns HTTP 201, the other 19 return HTTP 404 or 409, and subsequent `GET /setup` calls return HTTP 404 permanently. Also test `AUTH-5` (last active admin cannot be deleted or disabled) and IP/username rate limiting (`AUTH-8`, `SEC-12`). |
| Atomic commits | 1. `feat(store/1.4): implement user repository with Argon2id, TOTP, and last-admin guard [Spec: AUTH-4, AUTH-5, AUTH-6, AUTH-9, USR-5, SEC-4]` 2. `feat(admin/1.4): implement atomic first-admin setup, session cookies, and login rate limiter [Spec: AUTH-1, AUTH-2, AUTH-3, AUTH-7, AUTH-8, SEC-5, SEC-12]` |
| Risks and resolutions | **Risk:** `Argon2id` (`19 MiB` memory per hash) can exhaust RAM if 100 concurrent login requests run at once. **Resolution:** Bound concurrent `Argon2id` verifications with a `tokio::sync::Semaphore` (4 permits) and apply the per-IP rate limiter (`SEC-12`) before hashing. |

## 1.5 Sub-phase 1.5: Tamper-evident hash-chained `audit_log` and `ArcSwap<ConfigSnapshot>`

This sub-phase implements the cryptographic hash chain on `audit_log` (`AUD-1`, `SEC-13`) and the lock-free `ArcSwap<ConfigSnapshot>` loader (`REL-4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 5.4`, `Section 6.11`, `Section 8.2`; Requirements `AUD-1`, `REL-4`, `SEC-13` |
| Crates touched | `crates/store::audit`, `crates/core::snapshot`, `crates/server::snapshot_sync` |
| TDD step (`Red -> Green`) | Write unit and integration tests (`store::audit::tests::hash_chain_verification`, `#[doc = "Spec: AUD-1, SEC-13"]`) that insert 10 audit rows, verify `verify_audit_chain()` returns `Ok(())`, mutate one character in row 4 via raw SQL, and assert `verify_audit_chain()` detects the exact tampered row ID. Write a snapshot test (`#[doc = "Spec: REL-4"]`) proving that mutating a user or key swaps `ArcSwap<ConfigSnapshot>` within 10 ms. |
| Atomic commits | 1. `sec(store/1.5): implement HMAC-SHA256 hash-chained audit_log with monotonic seq and verifier CLI [Spec: AUD-1, SEC-13]` 2. `feat(core/1.5): implement generation-locked ArcSwap<ConfigSnapshot> reload pipeline, /healthz, /readyz, and /metrics listeners [Spec: REL-4, OBS-1, SEC-9]` |
| Risks and resolutions | **Risk:** Concurrent admin actions race on `prev_hash` when appending to `audit_log`. **Resolution:** Route all `audit_log` inserts through the serialized SQLite writer transaction so `prev_hash` is read and written atomically. |

### 1.5.1 Incremental `ConfigSnapshot` evolution across Epics 1 through 5

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
