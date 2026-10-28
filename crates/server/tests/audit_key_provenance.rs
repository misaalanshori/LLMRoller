//! Cycle 26 — the audit HMAC key must never be derived from the master key (`ISSUE-0138`).
//!
//! `ADR-11` guarantees that rotating `LLMRoller_MASTER_KEY` does **not** invalidate the `audit_log`
//! HMAC chain. Two production paths broke that guarantee by falling back to
//! `sha256_hex(master_key)` when the instance audit key was absent:
//!
//! - `crates/admin/src/api/common.rs` — `execute_atomic_mutation`
//! - `crates/admin/src/api/sessions_and_traces.rs` — the trace-view audit write
//!
//! A master-key-derived audit key destroys the guarantee *silently*: after a rotation, previously
//! written rows simply stop verifying and new rows chain from a different key. That is discovered
//! during an incident, not before one.
//!
//! Spec: `AUD-1`, `ADR-11`, `SEC-13`.

use llmroller_core::seams::ExposeSecret as _;

/// `audit_hmac_key()` must return the instance key when one is configured.
#[test]
#[doc = "Spec: AUD-1, ADR-11; ISSUE-0138"]
fn audit_key_prefers_the_instance_secret() {
    let instance_key = llmroller_core::seams::secret_bytes(vec![0x11; 32]);
    let master = llmroller_store::crypto::MasterKey::from_bytes([0x22; 32]);

    let resolved = llmroller_admin::resolve_audit_hmac_key(Some(&instance_key), &master)
        .expect("an instance key must resolve");
    assert_eq!(
        resolved.expose_secret(),
        &[0x11u8; 32],
        "the instance key must be used verbatim"
    );
}

/// With no instance key, the resolver must **fail**, not derive from the master key.
///
/// The forbidden alternative is `sha256_hex(master_key)`. Asserting `is_err()` is the whole point:
/// an error is recoverable and visible, whereas a substituted derivation produces a chain that
/// looks valid for new rows and invalid for old ones after a rotation.
#[test]
#[doc = "Spec: AUD-1, ADR-11; ISSUE-0138"]
fn audit_key_is_absent_rather_than_derived_from_the_master_key() {
    let master = llmroller_store::crypto::MasterKey::from_bytes([0x22; 32]);

    let resolved = llmroller_admin::resolve_audit_hmac_key(None, &master);
    assert!(
        resolved.is_err(),
        "a missing instance audit key must be an error, not a master-key derivation"
    );
}

/// The master key must not influence the resolved audit key at all.
///
/// Two different master keys with the same instance key must produce the same audit key. If any
/// master-key path survived, this would differ — and after a rotation the chain would break.
#[test]
#[doc = "Spec: ADR-11; ISSUE-0138"]
fn the_master_key_does_not_influence_the_audit_key() {
    let instance_key = llmroller_core::seams::secret_bytes(vec![0x33; 32]);
    let master_a = llmroller_store::crypto::MasterKey::from_bytes([0x01; 32]);
    let master_b = llmroller_store::crypto::MasterKey::from_bytes([0xFF; 32]);

    let a = llmroller_admin::resolve_audit_hmac_key(Some(&instance_key), &master_a)
        .expect("a")
        .expose_secret()
        .to_vec();
    let b = llmroller_admin::resolve_audit_hmac_key(Some(&instance_key), &master_b)
        .expect("b")
        .expose_secret()
        .to_vec();

    assert_eq!(
        a, b,
        "ADR-11: rotating the master key must not change the audit HMAC key"
    );
}

/// With no instance key, **both** master keys must fail — not derive two different keys.
///
/// This is the direction the previous test cannot see. `resolve_audit_hmac_key(Some(k), master)`
/// naturally ignores `master`, so a master-key derivation hidden in the `None` branch would pass
/// it. Asserting that two *different* master keys both produce an error closes that hole: a
/// derivation would return `Ok` for each and, worse, return *different* keys for the same database.
#[test]
#[doc = "Spec: ADR-11; ISSUE-0138"]
fn a_missing_instance_key_errors_identically_for_any_master_key() {
    let master_a = llmroller_store::crypto::MasterKey::from_bytes([0x01; 32]);
    let master_b = llmroller_store::crypto::MasterKey::from_bytes([0xFF; 32]);

    let a = llmroller_admin::resolve_audit_hmac_key(None, &master_a);
    let b = llmroller_admin::resolve_audit_hmac_key(None, &master_b);

    assert!(a.is_err(), "master key A must not yield an audit key");
    assert!(
        b.is_err(),
        "master key B must not yield an audit key — a derivation would return a different one \
         after a rotation, silently orphaning every previously written audit row"
    );
}
