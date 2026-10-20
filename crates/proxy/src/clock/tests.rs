//! Unit tests for the proxy asynchronous clock extension.
//!
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#164-component-mocking-and-dependency-injection-architecture
//! Phase: ./docs/bigbang/big-bang-phases-1.md#12-sub-phase-12-core-domain-identifiers-actorscope-enum-and-seam-traits

use std::time::Duration;

use llmroller_core::seams::{Clock, FakeClock};

use super::{AsyncClock, AsyncSystemClock, FakeAsyncClock};

#[test]
#[doc = "Partial: RTE-9, Partial: RTE-10"]
fn async_clock_implementations_are_also_synchronous_clocks() {
    fn assert_clock<T: Clock>() {}
    fn assert_async_clock<T: AsyncClock>() {}

    assert_clock::<AsyncSystemClock>();
    assert_async_clock::<AsyncSystemClock>();
    assert_clock::<FakeAsyncClock>();
    assert_async_clock::<FakeAsyncClock>();
}

#[tokio::test]
#[doc = "Partial: RTE-9, Partial: RTE-10"]
async fn fake_async_clock_sleep_advances_time_deterministically() {
    let clock = FakeAsyncClock::new(1_000);

    clock.sleep(Duration::from_millis(250)).await;
    assert_eq!(clock.now_ms(), 1_250);
    assert_eq!(clock.monotonic_ms(), 1_250);

    clock.sleep(Duration::ZERO).await;
    assert_eq!(clock.now_ms(), 1_250);
    assert_eq!(clock.monotonic_ms(), 1_250);
}

#[tokio::test]
#[doc = "Partial: RTE-9, Partial: RTE-10"]
async fn fake_async_clock_is_seeded_from_a_sync_fake_clock() {
    let base = FakeClock::new(5_000);
    let clock = FakeAsyncClock::from_clock(base);

    clock.advance_ms(500);
    clock.sleep(Duration::from_millis(25)).await;

    assert_eq!(clock.now_ms(), 5_525);
    assert_eq!(clock.monotonic_ms(), 5_525);
}

#[tokio::test]
#[doc = "Partial: RTE-9, Partial: RTE-10"]
async fn async_system_clock_delegates_wall_and_monotonic_time() {
    let clock = AsyncSystemClock::new();
    assert!(clock.now_ms() > 0);

    let before = clock.monotonic_ms();
    clock.sleep(Duration::ZERO).await;
    assert!(clock.monotonic_ms() >= before);
}
