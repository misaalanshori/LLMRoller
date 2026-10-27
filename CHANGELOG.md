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
