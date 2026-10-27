# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Toolchain pin restored, musl CI leg repaired (ISSUE-0038).** `rust-toolchain.toml`
  pinned `channel = "stable"`, which overrides the `1.99.0` toolchain that
  `.github/workflows/ci.yml` installs. The runner's `stable` channel carries no
  `x86_64-unknown-linux-musl` `rust-std`, so the musl leg failed with
  `error[E0463]: can't find crate for 'core'`. The channel is now pinned to
  `1.99.0`, matching the MSRV, `Dockerfile`, and both workflows.
- **Declared MSRV made honest.** `Cargo.toml`'s `rust-version` changed from the
  never-tested `"1.95"` to `"1.99"`, the only version CI has ever compiled.
  The relationship between the pinned channel, the MSRV, and the four other
  places the version appears is now documented in SRDD Section 3 (assumption A5)
  and Section 15.1, and the misleading word "stable" was removed where it
  implied a floating channel.
- **Regression guard added.** The `xtask` test
  `tools::toolchain_channel_and_declared_msrv_agree` fails if the channel in
  `rust-toolchain.toml` and the MSRV in `Cargo.toml` ever name different
  `major.minor` compilers, or if the channel is a floating name rather than a
  pinned release.

- **Budget admission was blind to in-flight spend (ISSUE-0004).**
  `BudgetCounter::effective_used` meant "committed spend, projected by the latest
  upstream observation". `Reservation::try_reserve` compensated at its own call
  site by adding `reserved_in_flight`, but the two gates that decide *whether to
  try* — `headroom_bps_for_route` and `key_budget_admitted` — did not. A burst of
  concurrent streams could therefore each pass admission against a key whose
  budget was already fully spoken for, and headroom was reported as 10,000 bps for
  a key with none of it free. `effective_used` is now a default trait method that
  includes the in-flight term, and the manual addition is gone from
  `try_reserve`. The reservation term is added after the observation projection,
  not before, because a projection extrapolates upstream-reported spend and
  reservations are spend the provider never reported.
- **`counter_id` was a hash of the window id (ISSUE-0005).** `try_reserve` sorts
  counters by `counter_id` to establish the acquisition order its comment claims
  prevents deadlocks, but the id came from `DefaultHasher`, whose output is
  unspecified and unstable, in a `u64` that makes collisions improbable rather
  than impossible. Ids are now allocated monotonically from a process-wide
  `AtomicU64`, preserving an existing counter's id across rebuilds and hydration so
  the order stays stable across a config reload. No `dedup` was introduced:
  reserving on every counter in the list is correct, and deduplicating would have
  created the window-dropping bug the issue imagined.

### Changed (internal structure)

- **`orchestrator/pipeline.rs` decomposed ahead of the MNT-3 ceiling (ISSUE-0225).**
  The file had grown from 1,284 to 1,476 lines over three cycles, leaving 124 of the
  1,600-line gate's headroom — and the next three accounting issues (#152/#153/#154) all
  target it. Two pure moves, no behaviour change: terminal usage accounting
  (`TerminalUsageFacts`, `enqueue_terminal_usage`, `provider_billing`) to
  `terminal_accounting.rs`, and the 151-line outbound body/header preparation block to
  `attempt_body.rs`. `pipeline.rs` is now 1,245 lines with 355 lines of headroom, and the
  highest-churn region is out of the file that is under pressure. `orchestrator/mod.rs`
  re-exports both, so no caller changed and the test count stayed at 703.

### Fixed (dropped ledger rows)

- **An embedded 200 rejection never reached the ledger (ISSUE-0150).** When an upstream
  answered HTTP 200 with a `RequestRejected` error in the body, the non-streaming handler
  pushed an attempt row and returned a synthesised 400 — with no usage event. The
  streaming first-frame equivalent in `attempt_streaming.rs` was worse: it committed a
  failure charge and *then* returned a rejection without enqueuing, so money was billed
  with nothing recording it. Both now call the shared `enqueue_terminal_usage`, attributed
  to the key and provider that answered.
- **A Gate G7 budget refusal never reached the ledger (ISSUE-0151).** When the client-key
  monthly cap rejected a request at admission, the caller was told their budget was
  exhausted while `usage_events` held no record that they had tried — the refusal was
  invisible to `/usage`. The event now carries no key or provider attribution (the gate
  fails before any upstream call) and no cost, with `error_code` identifying which cap
  was hit.
- Also extracted `provider_billing(snapshot, provider_id)`: the same four-line billing
  lookup was repeated at four call sites and a fifth was about to be added.

Note: the non-retryable-4xx passthrough the issue also names as a drop site **already
emitted** — every non-2xx is intercepted at `pipeline.rs` before the attempt handler runs,
and that path enqueues. The test for it is kept as a status-quo pin rather than a red.

### Fixed (billing integrity)

- **A charged timeout never reached the usage ledger (ISSUE-0149).** On the
  `timeout_failover == false` branch the handler committed `charged_micro_usd` to the
  client-key accumulator and the share/global cap guards and then returned `504` —
  without enqueuing a usage event or its attempt rows. Spend therefore existed with no
  ledger entry, so `/usage` under-reported, budget rollups lost the money, and the
  per-key spend view diverged permanently from what was charged. The terminal
  all-attempts-failed emitter is now extracted into `enqueue_terminal_usage` and called
  from both sites. The timeout path attributes the charge to the upstream key and
  provider that were actually called, rather than to `"none"` — attributing real spend
  to no provider would satisfy "a row exists" while breaking the per-provider
  breakdown.

### Fixed (auxiliary endpoint gates)

- **`/v1/messages/count_tokens` skipped three pre-candidate gates (ISSUE-0145).**
  The handler was a hand-rolled miniature of the inference path: it scanned the
  envelope, resolved the model and enforced G6, but never reserved the preflight
  memory budget, never decompressed a compressed body, and never consumed
  client-key TPM. A compressed body was therefore forwarded **still compressed**, so
  the upstream received gzip bytes instead of JSON — an interop bug as well as a
  missing control. The contiguous pre-candidate prefix of `admit_inbound_request` is
  now extracted into a shared `gate_inbound_request` used by both handlers, so the
  two lists cannot drift apart again. Three of the seven controls the issue listed
  (`is_ip_rate_limited`, the RPM limiter, diagnostic-header extraction) were already
  applied by `client_key_auth_middleware`; they stay there and are deliberately not
  duplicated into the helper.

### Fixed (routing signal)

- **Non-streaming latency was always recorded as 0 ms (ISSUE-0148).**
  `handle_non_streaming_attempt` computed latency as
  `(now - attempt_started).max(0)`, but `now` is the *admission* timestamp and
  `attempt_started` is read just before the upstream send, so the difference is
  negative and clamps to zero. That zero was fed to
  `load_tracker.record_ttfb(route, 0, 0.2)`, so every successful non-streaming
  attempt dragged the route's P2C EWMA toward zero and the slowest route reported as
  the fastest — defeating `power_of_two_choices` for the whole non-streaming traffic
  class. Measured pre-fix against a mock upstream delaying 50 ms: EWMA = 0 ms. The
  measurement point now reads `state.clock.now_ms()`, matching the three sibling
  latency sites in the same file. Only the latency gets a fresh read; cost, usage,
  key-health and audit timestamps stay on the admission timestamp.

### Fixed (memory budget)

- **Decompression ran outside the preflight memory budget (ISSUE-0147).**
  `admit_inbound_request` decompressed the request body and only then requested
  preflight permits, sized on the decompressed length. The semaphore therefore
  bounded post-decompression concurrency only: N concurrent compressed requests
  each allocated up to `MAX_DECOMPRESSED_BYTES` (16 MiB) before any of them touched
  the budget, so an attacker could keep several GB live while the budget still read
  free. Measured pre-fix: 16 of 16 concurrent compressed bodies each inflated 4 MiB
  with no reservation — 64 MiB against a 17 MiB budget. The reservation is now taken
  before decompression and denominated in the decompressed ceiling, then reconciled
  down to the actual size via `OwnedSemaphorePermit::split`. Uncompressed bodies
  reserve only their wire length, so the common path keeps its throughput. The
  permit is held for the request's lifetime and released on every error path, so a
  failed decompression leaks nothing.

### Fixed (routing scope)

- **`inherit` routing scope was silently widened, not inherited (ISSUE-0009).**
  `resolve_caller_scope` resolves `inherit` against `users.default_routing_scope` and
  is used on the inference data plane, but two paths bypassed it and called the raw
  parser: `visible_models` (feeding `GET /v1/models`) and `resolve_usage_key` (the
  usage passthrough). `RoutingScope::from_str` has no `"inherit"` arm and the parser
  falls back to `RoutingScope::default()` = `PersonalFirst`, which is broader than
  `PersonalOnly`/`OwnOnly`/`GlobalOnly`. So an `inherit` key on a user whose default
  was `own_only` resolved to `PersonalFirst` — own plus shared plus global. That
  over-advertised models the data plane would refuse, and on the usage path let a
  strict user's scope resolve a key from an excluded tier. Both sites now call
  `resolve_caller_scope`. The parser's permissive fallback for *invalid* scope
  strings is unchanged and now pinned by a test, pending a decision on the
  client-key write path.

### Fixed (unbounded memory)

- **Per-IP authentication failure map was unbounded (ISSUE-0059).**
  `ProxyAuthState::ip_failures` is keyed by attacker-controlled source address and
  nothing capped it or swept it, so an unauthenticated attacker could grow it without
  limit by spraying distinct IPs. It is now bounded at `MAX_TRACKED_FAILURE_IPS`
  (10,000) with a sweep on insert: expired entries are dropped first, and if the map
  is still full one existing slot is recycled — recycling a live attacker entry
  resets that attacker's counter, which is strictly better than unbounded memory,
  while dropping the *new* entry would leave a flooding attacker never rate-limited
  at all. The sweep runs only at capacity, never on the read path, because an O(n)
  sweep per request would be a DoS amplifier. `tracked_failure_ip_count()` exposes
  the bound so it can be asserted.
- **Login limiter maps grew without bound on the failure path (ISSUE-0059).**
  `ThreeTierLoginLimiter` holds three maps keyed by source IP, arbitrary
  `(username, IP)` pairs and arbitrary usernames. Only `check()` had a size guard,
  and it dropped only empty vectors; `record_failure()` had none and is reachable
  after the CSRF gate without a preceding `check`. One shared
  `sweep_if_at_capacity` helper now serves all three maps from both paths. No
  threshold changed.
- **`record_success` left the username tier accumulating (ISSUE-0059).** It cleared
  only the `(username, IP)` pair, so a user who mistyped their password repeatedly
  across an hour stayed escalated to TOTP-or-delay permanently, even after
  authenticating successfully. It now clears the per-username tier too. The per-IP
  tier is deliberately not cleared, since one successful login from an address does
  not mean that address is not also attacking other accounts.

### Fixed (memory accounting)

- **Global trace memory release could wrap (ISSUE-0007).** `TraceTap`'s `Drop`
  used `fetch_sub` on an `AtomicUsize`, which wraps. Releasing more than the counter
  held drove it to a value near `usize::MAX`, after which every reservation failed
  and content traceability switched itself off silently and permanently. Release now
  floors at zero via compare-and-set. A `shortcut:`-style note in the code records
  that the floor is a defensive guard, not a correctness proof: under-release leaks
  upward and degrades traceability toward off (safe), while over-release wraps and
  kills it instantly (unsafe).
- **Trace memory admission was check-then-act (ISSUE-0007).** Both
  `reserve_global_bytes` and `try_feed_chunk` loaded the global counter, compared it
  against the budget, then added — so concurrent callers could each observe room that
  only one of them could have, and the sum of grants could exceed the cap. Both sites
  now share one compare-and-set helper, with saturating arithmetic so a pathological
  byte count can neither panic in debug nor wrap in release.
- **Trace memory is now observable (PERF-5).** `global_trace_memory()` returns
  `(used, budget)`, and `metrics::record_trace_memory` publishes
  `llmroller_trace_memory_bytes` and `llmroller_trace_budget_bytes` on the existing
  30-second background tick. A silent global counter is the kind of thing that
  degrades for months unnoticed.

### Fixed (security)

- **Login self-deadlock (ISSUE-0001).** `post_login` acquired the Argon2id
  semaphore twice in one request, holding the first permit while waiting for the
  second. With `N` permits, `N` concurrent logins each held one permit and
  blocked on another, so no permit could ever be released; the second acquisition
  was bounded at 2 s and mapped to `503 login_busy`, making every concurrent
  login fail. Both Argon2id call sites now go through one helper that holds a
  single permit for the blocking verification only, and the user lookup no longer
  runs under a permit. The missing-user constant-time dummy verification, which
  previously ran without a permit at all, is now bounded the same way.
- **Session CSRF key was a public constant (ISSUE-0002).** The authenticated
  session CSRF token was `HMAC(a compile-time constant in the source tree,
  session_token)`, because `AuthAppState` had no field for the instance secret
  and every caller used the fallback. Anyone who learned a session token could
  compute the matching CSRF header offline, so the header added no proof over the
  cookie. `AuthAppState` now carries `session_hmac_secret`, wired from
  `instance_secrets` in `cli::serve` exactly as the data plane already does, and
  the hand-rolled HMAC is replaced by `hmac::Hmac<sha2::Sha256>`. The no-secret
  API form is removed; the fallback for test and CLI paths is a per-process
  random key rather than a public literal.
- **Pre-login CSRF comparison leaked token length (ISSUE-0045).** The
  double-submit check compared the cookie and header with
  `subtle::ConstantTimeEq` directly, which returns early on unequal lengths and
  is constant-time only for equal-length inputs. Both sides are now compared as
  fixed-width SHA-256 digests, matching the authenticated-session path. Harmless
  today because the nonce is a constant-width base64url value, but it constrained
  any future token-format change.
- **Signed pre-login CSRF cookie declined (ISSUE-0046).** Signing the cookie was
  analysed and deliberately not implemented: it stops the same attacker for the
  same reason the existing double-submit equality check already stops them, and
  it would cost an HMAC per page render plus a cookie-format migration.

### Fixed (tests)

- **`e2e_sigterm_drain` could not fail for the right reason.** The test built
  `ShutdownSupervisor` via `new(...)` and never called `.with_shutdown_cut(...)`,
  so step 4 of `execute_graceful_shutdown` was a no-op and the assertion labelled
  "Step 4: Stragglers cancelled with ShutdownCut" actually observed
  `notify_waiters()` waking a task. Because `notify_waiters()` drops the
  notification when no waiter is registered yet, the test lost the race
  intermittently (5 failures in 10 runs under `cargo test`). The test now arms the
  cut flag, registers its simulated straggler before the drain begins, and asserts
  the flag itself; wake-up coverage is kept as a separately named assertion. The
  sibling in-crate test `shutdown_broadcasts_background_stop_and_completes` was
  aligned to cover step 4 as well. Production behaviour is unchanged.
