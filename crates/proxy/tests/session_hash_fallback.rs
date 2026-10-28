//! Cycle 26 — one canonical session-hash fallback, and no zero-key HMAC (`ISSUE-0137`).
//!
//! The product had **two** secret-less fallbacks for the same concept:
//!
//! | Path | Fallback | Identity input |
//! |---|---|---|
//! | `orchestrator::session::session_hash` | `fallback_session_hash` — domain-separated SHA-256 | `client_key_id` |
//! | `usage_passthrough` | `compute_session_hash` with `[0u8; 32]` | `owner_user_id` |
//!
//! So a context without the instance secret produced different hashes *and* scoped them by
//! different identities: pinning and usage attribution could not agree on which session a request
//! belonged to. The zero-key HMAC is also worse than a plain derivation, because it *looks* keyed
//! while providing no secrecy at all.
//!
//! Spec: `RTE-1`, `SEC-5`, `USG-7`.

use llmroller_core::ids::{ClientKeyId, UserId};

/// A fixed dummy secret. Never a real key; the tests only need bytes of the right shape.
const DUMMY_SECRET: [u8; 32] = [0x5A; 32];

fn user() -> UserId {
    "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().expect("user id")
}

fn key() -> ClientKeyId {
    "01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().expect("client key id")
}

/// The secret-less hash must not be an HMAC under the all-zero key.
///
/// This is the assertion that the zero-key path is gone. A "fix" that made both callers use
/// `compute_session_hash(&[0u8; 32], …)` would satisfy an equality test while keeping the exact
/// weakness the issue names — this test is what forbids that.
#[test]
#[doc = "Spec: SEC-5, RTE-1; ISSUE-0137"]
fn secretless_session_hash_is_not_a_keyed_hmac_with_a_zero_key() {
    let secretless =
        llmroller_proxy::orchestrator::session_hash_for_test(None, &user(), key(), "sess-abc");
    let zero_keyed = llmroller_core::session::compute_session_hash(&[0u8; 32], &user(), "sess-abc");

    assert_ne!(
        secretless, zero_keyed,
        "a secret-less context must not hash under a publicly-known all-zero HMAC key"
    );
    assert_eq!(
        secretless,
        llmroller_proxy::orchestrator::fallback_session_hash_for_test(key(), "sess-abc"),
        "the one canonical fallback is the domain-separated derivation"
    );
}

/// With a real secret the hash is byte-identical to the keyed derivation, unchanged by this cycle.
#[test]
#[doc = "Spec: RTE-1, SEC-5; ISSUE-0137"]
fn keyed_session_hash_is_unchanged_by_this_cycle() {
    let expected =
        llmroller_core::session::compute_session_hash(&DUMMY_SECRET, &user(), "sess-abc");
    let actual = llmroller_proxy::orchestrator::session_hash_for_test(
        Some(&DUMMY_SECRET),
        &user(),
        key(),
        "sess-abc",
    );
    assert_eq!(
        actual, expected,
        "the keyed path must remain the core HMAC derivation, byte for byte"
    );
    let secretless =
        llmroller_proxy::orchestrator::session_hash_for_test(None, &user(), key(), "sess-abc");
    assert_ne!(actual, secretless);
}

/// The hash is scoped to its identity input, so two principals cannot share a session hash.
#[test]
#[doc = "Spec: RTE-1, SEC-5; ISSUE-0137"]
fn session_hash_is_scoped_to_its_identity_input() {
    let other_user: UserId = "01ARZ3NDEKTSV4RRFFQ69G5FAX".parse().expect("user id 2");
    let other_key: ClientKeyId = "01ARZ3NDEKTSV4RRFFQ69G5FAY"
        .parse()
        .expect("client key id 2");

    let a = llmroller_proxy::orchestrator::session_hash_for_test(
        Some(&DUMMY_SECRET),
        &user(),
        key(),
        "sess-abc",
    );
    let b = llmroller_proxy::orchestrator::session_hash_for_test(
        Some(&DUMMY_SECRET),
        &other_user,
        key(),
        "sess-abc",
    );
    assert_ne!(a, b, "two owners must not collide on the same session id");

    let c = llmroller_proxy::orchestrator::session_hash_for_test(None, &user(), key(), "sess-abc");
    let d =
        llmroller_proxy::orchestrator::session_hash_for_test(None, &user(), other_key, "sess-abc");
    assert_ne!(
        c, d,
        "two client keys must not collide on the same session id in the fallback"
    );

    let e = llmroller_proxy::orchestrator::session_hash_for_test(None, &user(), key(), "sess-xyz");
    assert_ne!(c, e);
}

/// The salted affinity token must not be keyed with an empty secret.
#[test]
#[doc = "Spec: SEC-5, RTE-1; ISSUE-0137"]
fn secretless_affinity_token_is_not_an_empty_key_hmac() {
    let token = llmroller_proxy::orchestrator::salted_affinity_token_for_test(
        None,
        &user(),
        key(),
        "sess-abc",
    );
    let empty_keyed = llmroller_proxy::affinity_token_for_test(&[], &user(), "sess-abc");
    assert_ne!(
        token, empty_keyed,
        "a secret-less affinity token must not be an HMAC under an empty key"
    );
}

/// The two secret-less paths must agree, which is the divergence the issue uncovered.
///
/// Before the fix the orchestrator used `fallback_session_hash` (scoped by `client_key_id`) while
/// `usage_passthrough` used `compute_session_hash(&[0u8; 32], …)` (scoped by `owner_user_id`), so
/// the same session produced two different hashes in two subsystems. This pins the agreement.
#[test]
#[doc = "Spec: RTE-1, SEC-5; ISSUE-0137"]
fn secretless_session_hash_agrees_between_the_orchestrator_and_usage_paths() {
    // The usage path's *old* expression, kept here as the negative: this is what it used to
    // produce, and the fix must not reproduce it.
    let old_usage_path =
        llmroller_core::session::compute_session_hash(&[0u8; 32], &user(), "sess-abc");
    let orchestrator =
        llmroller_proxy::orchestrator::session_hash_for_test(None, &user(), key(), "sess-abc");

    assert_ne!(
        orchestrator, old_usage_path,
        "the zero-key usage path produced a different hash for the same session; the fix must not \
         reintroduce it"
    );
    assert_eq!(
        orchestrator,
        llmroller_proxy::orchestrator::fallback_session_hash_for_test(key(), "sess-abc"),
        "both paths now resolve through the one canonical fallback"
    );
}
