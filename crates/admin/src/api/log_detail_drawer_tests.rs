//! The log-detail drawer must not invent facts from absent data (`ISSUE-0228`).
//!
//! Spec: 14.m section 7, REL-6
//!
//! # The defect
//!
//! `attempts_html` emitted `<li>1 attempt (served)</li>` when there were **no** `usage_attempts`
//! rows. Two fabrications at once: an invented *count* and an invented *outcome*.
//!
//! For a drawer whose purpose is post-hoc diagnosis, a gap in the record rendered as a confident
//! success story is worse than an empty list — and this file had already settled the rule for
//! `client_location` ("render a dash, do not invent a value"). The site sat next to the rule and
//! violated it.
//!
//! # What an empty `usage_attempts` set actually means
//!
//! Read from the write path, not assumed. `usage_writer::enqueue` takes
//! `(UsageEventRecord, Vec<UsageAttemptRecord>)`, and two production call sites pass
//! `Vec::new()` deliberately:
//!
//! - `crates/proxy/src/auth.rs` — the request was rejected before any upstream key was chosen
//!   (bad key, rate limit, budget);
//! - `crates/proxy/src/orchestrator/state_bound.rs` — a state-bound short-circuit returned
//!   without contacting an upstream.
//!
//! So an empty set means **"no upstream attempt was made"**. It does *not* mean "zero attempts
//! failed", and it does not mean "recording is missing" — the event row exists precisely because
//! accounting ran. The wording below says exactly that and claims nothing more.

use super::log_detail_drawer::render_log_detail_drawer_html;

/// A minimal event `JSON` with the fields the drawer reads, and **no** `attempts` key.
fn event_without_attempts() -> serde_json::Value {
    serde_json::json!({
        "request_id": "req-228",
        "status": 200,
        "model": "gpt-4o",
        "endpoint": "chat",
        "protocol": "chat",
        "ts": 1_700_000_000_000_i64,
        "latency_ms": 120,
    })
}

/// A minimal event carrying exactly one **served** attempt.
fn event_with_one_served_attempt() -> serde_json::Value {
    serde_json::json!({
        "request_id": "req-228b",
        "status": 200,
        "model": "gpt-4o",
        "endpoint": "chat",
        "protocol": "chat",
        "ts": 1_700_000_000_000_i64,
        "latency_ms": 120,
        "attempts": [{
            "attempt_no": 1,
            "outcome": "served",
            "upstream_key_id": "key-abc",
        }],
    })
}

/// With no `usage_attempts` rows the drawer must not claim an attempt happened.
///
/// Pre-fix the output contains `<li>1 attempt (served)</li>` — a count and an outcome from no
/// data. The assertion is on the *claim*, not on a specific replacement string, so the wording can
/// change without weakening it.
#[test]
#[doc = "Spec: REL-6 — ISSUE-0228 the drawer must not invent an attempt"]
fn drawer_with_no_attempts_does_not_claim_an_attempt() {
    let html = render_log_detail_drawer_html(&event_without_attempts());

    assert!(
        !html.contains("1 attempt"),
        "the drawer must not render an attempt count when there is no attempt data; got: {html}"
    );
    assert!(
        !html.contains("(served)"),
        "the drawer must not render a `(served)` outcome when there is no attempt data; \
         got: {html}"
    );
    // The absence must still be *stated*: deleting the list entirely would hide that the section
    // exists, and an operator would not know whether to look elsewhere.
    assert!(
        html.contains("No upstream attempt"),
        "the drawer must say that no upstream attempt was recorded rather than rendering \
         nothing; got: {html}"
    );
}

/// One served attempt must still read as one served attempt — the non-regression.
///
/// This is the assertion that makes the fix meaningful. Asserting only the negative above would
/// pass if the whole attempts list were deleted, which would "fix" the lie by removing the
/// feature. So both renders are asserted, and they must differ.
#[test]
#[doc = "Spec: REL-6 — ISSUE-0228 one served attempt still reads as one"]
fn drawer_with_one_served_attempt_still_reads_as_one_served_attempt() {
    let with_one = render_log_detail_drawer_html(&event_with_one_served_attempt());
    let with_none = render_log_detail_drawer_html(&event_without_attempts());

    assert!(
        with_one.contains("Attempt 1"),
        "a single recorded attempt must still be rendered with its number; got: {with_one}"
    );
    assert!(
        with_one.contains("served"),
        "the recorded outcome must still be shown; got: {with_one}"
    );
    assert!(
        !with_one.contains("No upstream attempt"),
        "a recorded attempt must not also be reported as absent; got: {with_one}"
    );

    // The inequality is the acceptance criterion: "no data" and "one served attempt" must be
    // distinguishable at a glance.
    assert_ne!(
        with_none, with_one,
        "the render for no attempt data must differ from the render for one served attempt; \
         if these are equal the drawer is still conflating absence with a served attempt"
    );
}
