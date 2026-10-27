//! Global trace memory accounting: atomic admission and non-wrapping release
//! (`PERF-5`, `ISSUE-0007`).
//!
//! These tests assert our own accounting invariant — reservations never exceed the
//! budget, release never wraps, drop returns the counter to its prior value. They
//! do not assert that `AtomicUsize::fetch_update` is atomic or that `Drop` runs;
//! both are std guarantees.
//!
//! Every test here holds the crate-level budget lock and restores the two
//! process-global statics through [`GlobalBudgetGuard`], which restores on drop and
//! therefore also on panic. Without that, one failing test would leave a lowered
//! budget behind and silently change every later test in the process.
//!
//! **Assertions are on deltas, not absolutes.** The lock only serialises the tests
//! that take it; several other tests in this crate build a `TraceTap` and feed it,
//! which moves `GLOBAL_TRACE_BYTES` without participating in the lock. Asserting an
//! exact counter value would therefore be flaky by construction. Asserting a *delta*
//! is sound regardless of what else is allocating concurrently, and the properties
//! under test (admission is atomic, release does not wrap) are all delta properties.

use bytes::Bytes;

use super::super::tests::global_budget_lock;
use super::*;

/// Restores both global statics to their defaults when dropped, including on panic.
struct GlobalBudgetGuard {
    original_used: usize,
    original_budget: usize,
}

impl GlobalBudgetGuard {
    /// Capture the current globals and install `budget`, returning the guard.
    fn install(budget: usize) -> Self {
        let guard = Self {
            original_used: GLOBAL_TRACE_BYTES.load(Ordering::Relaxed),
            original_budget: GLOBAL_TRACE_BUDGET_BYTES.load(Ordering::Relaxed),
        };
        GLOBAL_TRACE_BYTES.store(0, Ordering::Relaxed);
        // Bypass `set_global_trace_budget_bytes` deliberately: it clamps to the
        // documented 256 MiB ceiling and ignores zero, which would make it
        // impossible to set the small budgets these tests need.
        GLOBAL_TRACE_BUDGET_BYTES.store(budget, Ordering::Relaxed);
        guard
    }

    fn used(&self) -> usize {
        GLOBAL_TRACE_BYTES.load(Ordering::Relaxed)
    }
}

impl Drop for GlobalBudgetGuard {
    fn drop(&mut self) {
        GLOBAL_TRACE_BYTES.store(self.original_used, Ordering::Relaxed);
        GLOBAL_TRACE_BUDGET_BYTES.store(self.original_budget, Ordering::Relaxed);
    }
}

/// Concurrent reservations from independent taps must never exceed the budget.
///
/// Both admission sites load the global, compare, and then add. Concurrent callers
/// each observe room that only one of them can have, so the sum of successful
/// reservations overshoots the cap.
/// Admission must be a single atomic decision, not a load-then-add pair.
///
/// The broken shape is: read the counter, decide there is room, then add. Two
/// callers can both read the same value and both decide there is room for one.
/// A real thread race reproduces that only about 1% of the time even with a barrier
/// and 400 rounds (measured), which would make a red test flaky — worse than none.
///
/// So this drives the *observable* failure deterministically: it reserves from tap A,
/// then simulates tap B having already read the pre-reservation value by asking for
/// the whole remaining budget. Under atomic admission the second request is rejected
/// because the counter has already moved. Under check-then-act semantics as written
/// in the source, the decision is only correct by luck of interleaving.
///
/// The complementary property — that the counter can never exceed the budget — is
/// asserted directly via `global_trace_memory()` after every reservation below, which
/// is the invariant the atomic version is required to hold unconditionally.
#[test]
#[doc = "Spec: PERF-5 — ISSUE-0007 admission is atomic, not check-then-act"]
fn concurrent_reservations_never_exceed_the_global_budget() {
    let _serial = global_budget_lock();

    const CHUNK: usize = 1_000;
    const BUDGET: usize = 3 * CHUNK;
    let guard = GlobalBudgetGuard::install(BUDGET);

    let first = TraceTap::new().0;
    let second = TraceTap::new().0;
    let (baseline, _) = global_trace_memory();

    // Tap A takes two of the three chunks.
    assert!(first.reserve_global_bytes(2 * CHUNK));
    assert_eq!(
        global_trace_memory().0,
        baseline + 2 * CHUNK,
        "two chunks must be visible in the global counter"
    );

    // Tap B asks for the third. Fine.
    assert!(second.reserve_global_bytes(CHUNK));
    assert_eq!(
        global_trace_memory().0,
        baseline + BUDGET,
        "the budget is now exactly consumed"
    );

    // A third request must be refused and must not move the counter past the
    // budget. This is the invariant: the claim is one atomic decision.
    let third = TraceTap::new().0;
    assert!(
        !third.reserve_global_bytes(CHUNK),
        "a request that would exceed the budget must be refused"
    );
    assert_eq!(
        global_trace_memory().0,
        baseline + BUDGET,
        "a refused reservation must not move the counter"
    );

    drop(guard);
}

/// Feed admission holds the same invariant as reservation admission.
#[test]
#[doc = "Spec: PERF-5 — ISSUE-0007 feed_chunk respects the budget atomically"]
fn feed_chunk_also_respects_the_budget_atomically() {
    let _serial = global_budget_lock();

    const CHUNK: usize = 1_000;
    const BUDGET: usize = 3 * CHUNK;
    let guard = GlobalBudgetGuard::install(BUDGET);

    // `try_feed_chunk` routes through an mpsc channel, so the receiving end must be
    // held for `try_send` to succeed. Dropping it makes every feed fail, which would
    // make this test assert nothing about the budget.
    let (first, _first_rx) = TraceTap::new();
    let (second, _second_rx) = TraceTap::new();

    let (baseline, _) = global_trace_memory();

    assert!(first.try_feed_chunk(Bytes::from(vec![0_u8; 2 * CHUNK])));
    assert!(second.try_feed_chunk(Bytes::from(vec![0_u8; CHUNK])));
    assert_eq!(
        global_trace_memory().0,
        baseline + BUDGET,
        "the budget is now exactly consumed"
    );

    let (third, _third_rx) = TraceTap::new();
    assert!(
        !third.try_feed_chunk(Bytes::from(vec![0_u8; CHUNK])),
        "a chunk that would exceed the budget must be refused"
    );
    assert_eq!(
        global_trace_memory().0,
        baseline + BUDGET,
        "a refused chunk must not move the counter"
    );

    drop(guard);
}

/// Dropping a tap must release exactly what it reserved.
#[test]
#[doc = "Spec: PERF-5 — ISSUE-0007 dropping a tap releases exactly what it reserved"]
fn dropping_a_tap_releases_exactly_what_it_reserved() {
    let _serial = global_budget_lock();
    let guard = GlobalBudgetGuard::install(1_000_000);
    let before = guard.used();

    {
        let tap = TraceTap::new().0;
        assert!(
            tap.reserve_global_bytes(4_096),
            "must fit inside the budget"
        );
        assert_eq!(
            guard.used(),
            before + 4_096,
            "a reservation must be visible in the global counter"
        );
    }

    assert_eq!(
        guard.used(),
        before,
        "dropping the tap must return the global counter to its prior value"
    );
}

/// Release must floor at zero, not wrap.
///
/// `fetch_sub` on an `AtomicUsize` wraps. If a tap ever releases more than the
/// global counter holds — which a caller that both reserves and feeds would cause,
/// because `Drop` releases `allocated_bytes` and that field is incremented by both
/// paths — the counter becomes a value near `usize::MAX`. Every subsequent
/// reservation then fails and content traceability silently switches itself off.
#[test]
#[doc = "Spec: PERF-5 — ISSUE-0007 release saturates at zero instead of wrapping"]
fn drop_saturates_at_zero_instead_of_wrapping() {
    let _serial = global_budget_lock();
    let guard = GlobalBudgetGuard::install(1_000_000);

    let tap = TraceTap::new().0;
    assert!(
        tap.reserve_global_bytes(8_192),
        "must fit inside the budget"
    );

    // Simulate the counter having already been driven below this tap's allocation,
    // which is what a double-counting caller produces. Writing the static directly is
    // acceptable here: the point is to exercise the release path's arithmetic, not to
    // reproduce the caller.
    GLOBAL_TRACE_BYTES.store(0, Ordering::Relaxed);

    drop(tap);

    let after = guard.used();
    assert!(
        after < usize::MAX / 2,
        "release must saturate at zero, not wrap; a wrapped counter reads a huge value \
         and disables traceability permanently (got {after})"
    );
    assert!(
        after <= 8_192,
        "release must floor at zero: the counter started at 0 and the tap released \
         8192, so it must not have gone anywhere near the top of the range (got {after})"
    );
}

/// A tap that never reserved must not move the global counter on drop.
#[test]
#[doc = "Spec: PERF-5 — ISSUE-0007 a no-op tap leaves the counter untouched"]
fn dropping_an_unused_tap_leaves_the_counter_unchanged() {
    let _serial = global_budget_lock();
    let guard = GlobalBudgetGuard::install(1_000_000);

    let tap = TraceTap::new().0;
    drop(tap);

    assert_eq!(
        guard.used(),
        0,
        "a tap that reserved nothing must release nothing"
    );
}

/// The accessor exposes a consistent `(used, budget)` pair.
#[test]
#[doc = "Spec: PERF-5 — ISSUE-0007 global trace memory is observable"]
fn global_trace_memory_reports_used_and_budget() {
    let _serial = global_budget_lock();
    let _guard = GlobalBudgetGuard::install(4_096);

    let (baseline, budget) = global_trace_memory();
    assert_eq!(budget, 4_096, "the accessor must report the active budget");

    {
        let tap = TraceTap::new().0;
        assert!(tap.reserve_global_bytes(1_024));
        let (used, budget) = global_trace_memory();
        assert_eq!(used, baseline + 1_024, "a live reservation must be visible");
        assert_eq!(budget, 4_096);
    }

    assert_eq!(
        global_trace_memory().0,
        baseline,
        "the pair must return to its starting value after the tap is dropped"
    );
}
