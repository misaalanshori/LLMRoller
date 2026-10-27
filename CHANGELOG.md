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
