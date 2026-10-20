//! Unit tests for secret wrappers and the synchronous clock/entropy seams.
//!
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md#164-component-mocking-and-dependency-injection-architecture
//! Phase: ./docs/bigbang/big-bang-phases-1.md#12-sub-phase-12-core-domain-identifiers-actorscope-enum-and-seam-traits

use zeroize::ZeroizeOnDrop;

use super::{
    Clock, DeterministicRng, EntropySource, ExposeSecret, FakeClock, OsEntropy, SystemClock,
    secret_bytes, secret_string,
};

const CANARY: &str = "sk-canary-9988776655443322";

#[test]
#[doc = "Spec: SEC-14"]
fn secret_string_redacts_debug_and_exposes_only_explicitly() {
    let secret = secret_string(CANARY);

    assert_eq!(secret.expose_secret(), CANARY);
    let debug = format!("{secret:?}");
    assert!(
        !debug.contains(CANARY),
        "secret leaked through Debug: {debug}"
    );
    assert!(
        debug.contains("REDACTED"),
        "Debug must identify redaction: {debug}"
    );
}

#[test]
#[doc = "Spec: SEC-14"]
fn secret_bytes_redacts_debug_and_exposes_only_explicitly() {
    let secret = secret_bytes(CANARY.as_bytes().to_vec());

    assert_eq!(secret.expose_secret(), CANARY.as_bytes());
    let debug = format!("{secret:?}");
    assert!(
        !debug.contains(CANARY),
        "secret leaked through Debug: {debug}"
    );
    assert!(
        debug.contains("REDACTED"),
        "Debug must identify redaction: {debug}"
    );
}

#[test]
#[doc = "Spec: SEC-14"]
fn secret_wrappers_zeroize_on_drop() {
    fn assert_zeroize_on_drop<T: ZeroizeOnDrop>() {}

    assert_zeroize_on_drop::<super::SecretString>();
    assert_zeroize_on_drop::<super::SecretBytes>();
}

#[test]
#[doc = "Partial: RTE-9, Partial: RTE-10"]
fn fake_clock_advances_wall_and_monotonic_time_together() {
    let clock = FakeClock::new(1_000);
    assert_eq!(clock.now_ms(), 1_000);
    assert_eq!(clock.monotonic_ms(), 1_000);

    clock.advance_ms(250);
    assert_eq!(clock.now_ms(), 1_250);
    assert_eq!(clock.monotonic_ms(), 1_250);

    clock.advance_monotonic_ms(25);
    assert_eq!(clock.now_ms(), 1_250);
    assert_eq!(clock.monotonic_ms(), 1_275);
}

#[test]
#[doc = "Partial: RTE-9, Partial: RTE-10"]
fn fake_clock_allows_wall_clock_steps_without_rewinding_monotonic_time() {
    let clock = FakeClock::new(10_000);
    clock.advance_ms(500);

    clock.set_now_ms(9_000);
    assert_eq!(clock.now_ms(), 9_000);
    assert_eq!(clock.monotonic_ms(), 10_500);

    clock.advance_monotonic_ms(100);
    assert_eq!(clock.monotonic_ms(), 10_600);
}

#[test]
#[doc = "Partial: RTE-9, Partial: RTE-10"]
fn system_clock_reports_epoch_millis_and_monotonic_time() {
    let clock = SystemClock::new();
    assert!(
        clock.now_ms() > 0,
        "system wall time must be after the Unix epoch"
    );

    let first = clock.monotonic_ms();
    let second = clock.monotonic_ms();
    assert!(second >= first, "monotonic time must never move backwards");
}

#[test]
#[doc = "Partial: SEC-3, Partial: RTE-14"]
fn deterministic_rng_replays_identical_sequences_for_the_same_seed() {
    let first = DeterministicRng::new(42);
    let second = DeterministicRng::new(42);

    let mut first_bytes = [0_u8; 64];
    let mut second_bytes = [0_u8; 64];
    first.fill_bytes(&mut first_bytes);
    second.fill_bytes(&mut second_bytes);
    assert_eq!(first_bytes, second_bytes);

    for _ in 0..128 {
        assert_eq!(
            first.next_u64_bounded(10_000),
            second.next_u64_bounded(10_000)
        );
    }
}

#[test]
#[doc = "Partial: SEC-3, Partial: RTE-14"]
fn bounded_entropy_never_returns_a_value_at_or_above_the_bound() {
    let entropy = DeterministicRng::new(7);

    for bound in [1_u64, 2, 3, 7, 100, 1_000_000] {
        for _ in 0..2_000 {
            assert!(entropy.next_u64_bounded(bound) < bound);
        }
    }
}

#[test]
#[doc = "Partial: SEC-3, Partial: RTE-14"]
fn os_entropy_fills_buffers_and_produces_bounded_values() {
    let entropy = OsEntropy::new();

    let mut bytes = [0_u8; 32];
    entropy.fill_bytes(&mut bytes);
    entropy.fill_bytes(&mut []);

    for _ in 0..256 {
        assert!(entropy.next_u64_bounded(1_000) < 1_000);
    }
}

#[test]
#[doc = "Partial: SEC-3, Partial: RTE-14"]
#[should_panic(expected = "bound must be greater than zero")]
fn zero_bound_is_rejected() {
    let entropy = DeterministicRng::new(1);
    let _ = entropy.next_u64_bounded(0);
}
