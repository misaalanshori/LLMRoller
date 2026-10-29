//! Coverage for the two spec-required periodic tasks that were never spawned
//! (`ISSUE-0186`, `ISSUE-0187`).
//!
//! # Why these tests exist in this shape
//!
//! Both defects are the same shape: a requirement says "every hour" / "every 10 minutes", the
//! code logs it **once** and never again, and nothing fails. `SEC-13`/`AUD-1` want an
//! `audit_anchor` every 100 rows *and at least hourly*; `SEC-9` wants the plaintext-listener
//! error repeated every 10 minutes. An idle instance satisfied neither, and no test noticed —
//! because neither behaviour had a test, and because no test can wait an hour.
//!
//! So the *tick body* is extracted into a function and driven directly here. The test never
//! sleeps for the real period: it passes a short `Duration` and the extracted function runs one
//! pass. That is also the only shape that can assert the two negative requirements — "no anchor
//! when one is already recent", "no repeater task on a loopback bind", "nothing at startup" —
//! because those are claims about what did *not* happen, and a negative is only testable where
//! the code path can be observed ending.
//!
//! ## Capturing log output without new machinery
//!
//! The assertion target is `tracing` output, so the tests install a process-global subscriber that
//! records every event (target, level, message, fields) into a buffer. Two consequences, both
//! deliberate:
//!
//! - The buffer is global, so the tracing-dependent tests serialize on [`capture_guard`]. Six
//!   short tests, no new dependency.
//! - Comparing the *timer's* anchor line against the *row counter's* anchor line is done field by
//!   field rather than by diffing rendered JSON, because the JSON envelope is `tracing-subscriber`'s
//!   job (`OBS-2`) and not our logic. What belongs to us is: same target, same level, same message,
//!   same `seq`/`entry_hash`/`ts`, same `anchor` payload — which is the "one anchor format" the
//!   requirement is actually about.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::*;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};
use sqlx::{Connection as _, SqlitePool};

const AUDIT_KEY: [u8; 32] = [0x5a; 32];
const ANCHOR_TARGET: &str = "llmroller::audit_anchor";
const REPEATER_TARGET: &str = "llmroller::plaintext_repeater";
/// One hour, the `SEC-13` anchor period, passed explicitly so the tests never sleep for it.
const PERIOD: Duration = Duration::from_secs(3600);

// ---------------------------------------------------------------------------
// Log capture
// ---------------------------------------------------------------------------

/// One recorded `tracing` event.
#[derive(Clone, Debug, Default)]
struct Record {
    target: String,
    level: String,
    message: String,
    fields: Vec<(String, String)>,
}

impl Record {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Records every event into a process-global buffer.
#[derive(Default)]
struct Capture {
    records: Mutex<Vec<Record>>,
}

static CAPTURE: OnceLock<Capture> = OnceLock::new();
/// Serializes the tracing-dependent tests. Async because a guard is held across `await`: the tests
/// drive real async tick bodies, and a std guard there would be held across a suspension point.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Capture {
    fn global() -> &'static Self {
        CAPTURE.get_or_init(|| {
            let capture = Capture::default();
            // Ignore the error: another test module in this binary may already have installed a
            // subscriber, in which case these tests simply observe the same global dispatcher.
            let _ = tracing::subscriber::set_global_default(CaptureSubscriber);
            capture
        })
    }

    fn clear(&self) {
        if let Ok(mut records) = self.records.lock() {
            records.clear();
        }
    }

    fn records_for(&self, target: &str) -> Vec<Record> {
        self.records
            .lock()
            .map(|records| {
                records
                    .iter()
                    .filter(|record| record.target == target)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Serialize tracing-dependent tests and start each with an empty buffer.
async fn capture_guard() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = TEST_LOCK.lock().await;
    Capture::global().clear();
    guard
}

struct CaptureSubscriber;

impl tracing::Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields {
            message: String,
            fields: Vec<(String, String)>,
        }

        impl tracing::field::Visit for Fields {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.fields
                    .push((field.name().to_string(), value.to_string()));
            }

            fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
                self.fields
                    .push((field.name().to_string(), value.to_string()));
            }

            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                self.fields
                    .push((field.name().to_string(), value.to_string()));
            }

            fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
                self.fields
                    .push((field.name().to_string(), value.to_string()));
            }

            fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
                self.fields
                    .push((field.name().to_string(), value.to_string()));
            }

            fn record_error(
                &mut self,
                field: &tracing::field::Field,
                value: &(dyn std::error::Error + 'static),
            ) {
                self.fields
                    .push((field.name().to_string(), value.to_string()));
            }

            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                let rendered = format!("{value:?}");
                // `%value` (Display) arrives here wrapped in `format_args!`, which keeps the
                // quotes off; plain `?value` arrives with surrounding quotes. Normalizing lets the
                // tests compare a message payload regardless of which sigil the code used.
                let trimmed = rendered
                    .strip_prefix('"')
                    .and_then(|inner| inner.strip_suffix('"'))
                    .unwrap_or(&rendered)
                    .to_string();
                if field.name() == "message" {
                    self.message = trimmed;
                } else {
                    self.fields.push((field.name().to_string(), trimmed));
                }
            }
        }

        let mut fields = Fields {
            message: String::new(),
            fields: Vec::new(),
        };
        event.record(&mut fields);

        let metadata = event.metadata();
        if let Ok(mut records) = Capture::global().records.lock() {
            records.push(Record {
                target: metadata.target().to_string(),
                level: metadata.level().to_string(),
                message: fields.message,
                fields: fields.fields,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

async fn fresh_pool(name: &str) -> (tempfile::TempDir, SqlitePool) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let pool = open_pool(&directory.path().join(name), Role::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("schema must migrate");
    (directory, pool)
}

/// Append `count` audit rows through the real writer path and return `(seq, hash)` per row.
async fn append_rows(pool: &SqlitePool, count: usize) -> Vec<(i64, String)> {
    let mut appended = Vec::with_capacity(count);
    for index in 0..count {
        let mut connection = pool.acquire().await.expect("acquire");
        let mut transaction = connection.begin().await.expect("begin");
        let action = format!("periodic_action_{index}");
        let (seq, hash) = llmroller_store::audit::append(
            &mut transaction,
            &AUDIT_KEY,
            llmroller_store::audit::AuditEntry {
                id: &action,
                ts: 1_700_000_000_000 + index as i64,
                actor_user_id: None,
                ip_addr: None,
                action: &action,
                target_type: "instance",
                target_id: None,
                detail_json: None,
            },
        )
        .await
        .expect("append audit row");
        transaction.commit().await.expect("commit");
        appended.push((seq, hash));
    }
    appended
}

// ---------------------------------------------------------------------------
// ISSUE-0186 — the hourly audit anchor
// ---------------------------------------------------------------------------

/// One timer tick on an audit log whose tip has never been anchored must emit an anchor for the
/// current tip.
///
/// `SEC-13`/`AUD-1`: "every 100 rows **and at least hourly**". With 5 rows the row counter (which
/// fires at `seq % 100 == 0`) is silent, so the timer is the only source — which is exactly the
/// state an idle instance is in, and exactly the state that had no anchor at all.
#[tokio::test]
#[doc = "Spec: AUD-1, Spec: SEC-13"]
async fn hourly_anchor_emits_when_no_anchor_was_logged_recently() {
    let _guard = capture_guard().await;
    let (_directory, pool) = fresh_pool("anchor_emits.db").await;
    let appended = append_rows(&pool, 5).await;
    let (tip_seq, tip_hash) = appended.last().cloned().expect("at least one row");

    assert!(
        run_audit_anchor_tick(&pool, 3_600_000, PERIOD, None)
            .await
            .expect("tick must not fail"),
        "a tick with an unanchored non-empty chain must emit"
    );

    let anchors = Capture::global().records_for(ANCHOR_TARGET);
    assert_eq!(
        anchors.len(),
        1,
        "exactly one anchor per tick, got {anchors:?}"
    );
    let anchor = &anchors[0];
    assert_eq!(
        anchor.field("seq"),
        Some(tip_seq.to_string().as_str()),
        "the anchor must attest the current chain tip"
    );
    assert_eq!(
        anchor.field("entry_hash"),
        Some(tip_hash.as_str()),
        "the anchor must carry the tip's hash"
    );
    assert_eq!(
        anchor.level, "INFO",
        "the timer must not change the anchor's level (SEC-13)"
    );
}

/// `§1.2`'s guard: a tip already anchored inside the last hour must not be anchored again.
///
/// Without this, a busy instance emits one anchor from the 100-row counter and a second from the
/// timer for the *same* tip. Two attestations for one tip is not more evidence; it makes the
/// anchor stream ambiguous about which event is which.
#[tokio::test]
#[doc = "Spec: AUD-1, Spec: SEC-13"]
async fn hourly_anchor_does_not_duplicate_a_row_counter_anchor() {
    let _guard = capture_guard().await;
    let (_directory, pool) = fresh_pool("anchor_dedupe.db").await;
    // 100 rows, so the counter emits exactly one anchor at seq 100.
    let appended = append_rows(&pool, 100).await;
    let (tip_seq, _tip_hash) = appended.last().cloned().expect("rows");

    let counter_anchors = Capture::global().records_for(ANCHOR_TARGET);
    assert_eq!(
        counter_anchors.len(),
        1,
        "the row counter must anchor once at seq 100 (precondition of this test)"
    );
    assert_eq!(counter_anchors[0].field("seq"), Some("100"));

    // A tick immediately after the counter's anchor: the counter publishes the emission time into
    // the shared register, so the timer must see it and stand down.
    let last = llmroller_store::audit::last_anchor_at_ms();
    assert!(
        last.is_some(),
        "the row counter must have published its anchor to the shared register"
    );
    let emitted = run_audit_anchor_tick(&pool, last.expect("just asserted") + 60_000, PERIOD, last)
        .await
        .expect("tick must not fail");
    assert!(
        !emitted,
        "a tip anchored 60 s ago must not be anchored again (the last_anchored guard is missing)"
    );
    assert_eq!(
        Capture::global().records_for(ANCHOR_TARGET).len(),
        1,
        "the timer must not add a second anchor for tip seq {tip_seq}"
    );
}

/// `§1.3`: a fresh database has no chain tip, and an anchor for it must not be invented.
///
/// A `{"seq":0,"hash":""}` line is worse than silence — it is an attestation of a chain that does
/// not exist, and `verify-audit-log --anchor-seq 0` would then have something to "verify" against.
#[tokio::test]
#[doc = "Spec: AUD-1, Spec: SEC-13"]
async fn hourly_anchor_emits_nothing_on_an_empty_audit_log() {
    let _guard = capture_guard().await;
    let (_directory, pool) = fresh_pool("anchor_empty.db").await;

    let emitted = run_audit_anchor_tick(&pool, 3_600_000, PERIOD, None)
        .await
        .expect("tick must not fail on an empty log");
    assert!(!emitted, "an empty audit log must emit no anchor");
    assert!(
        Capture::global().records_for(ANCHOR_TARGET).is_empty(),
        "an empty audit log must not fabricate an anchor"
    );
}

/// The timer and the row counter must produce the *same* anchor for the same tip.
///
/// This is what "reuse `format_audit_anchor`" means operationally: a second JSON shape for the
/// timer would leave an operator with two anchor dialects and a verifier that accepts one.
#[tokio::test]
#[doc = "Spec: AUD-1, Spec: SEC-13"]
async fn anchor_uses_the_same_format_as_the_row_counter() {
    let _guard = capture_guard().await;
    let (_directory, pool) = fresh_pool("anchor_format.db").await;
    // Seq 100 makes the row counter emit; the timer then anchors the same tip.
    let appended = append_rows(&pool, 100).await;
    let (tip_seq, _tip_hash) = appended.last().cloned().expect("rows");

    let counter_anchor = Capture::global()
        .records_for(ANCHOR_TARGET)
        .into_iter()
        .next()
        .expect("the row counter must have anchored at seq 100");

    // Force the timer past its guard so it re-anchors the same tip: this is the one case where a
    // duplicate is intentional, and it exists only to compare the two renderings.
    let last = llmroller_store::audit::last_anchor_at_ms().expect("counter published");
    let period_ms = i64::try_from(PERIOD.as_millis()).expect("period fits");
    assert!(
        run_audit_anchor_tick(&pool, last + period_ms, PERIOD, Some(last))
            .await
            .expect("tick"),
        "the timer must emit once a full period has passed"
    );
    let timer_anchor = Capture::global()
        .records_for(ANCHOR_TARGET)
        .into_iter()
        .next_back()
        .expect("the timer must have anchored");

    assert_eq!(timer_anchor.level, counter_anchor.level);
    assert_eq!(timer_anchor.message, counter_anchor.message);
    assert_eq!(timer_anchor.field("seq"), counter_anchor.field("seq"));
    assert_eq!(timer_anchor.field("seq"), Some("100"));
    assert_eq!(
        timer_anchor.field("entry_hash"),
        counter_anchor.field("entry_hash")
    );
    assert_eq!(
        timer_anchor.field("anchor"),
        counter_anchor.field("anchor"),
        "both producers must emit the identical anchor payload for tip seq {tip_seq}"
    );
}

// ---------------------------------------------------------------------------
// ISSUE-0187 — the repeating plaintext-listener error
// ---------------------------------------------------------------------------

/// With the condition holding, one repeater tick emits `error` (`SEC-9`).
///
/// Level matters: the startup line is the warning, the repeater is the error. Copying the startup
/// level would have made the heartbeat indistinguishable from the one-off notice it is meant to
/// supplement.
#[tokio::test]
#[doc = "Spec: SEC-9, Spec: SEC-10"]
async fn plaintext_non_loopback_bind_repeats_the_error() {
    let _guard = capture_guard().await;

    emit_plaintext_repeater_error("0.0.0.0:8080");

    let records = Capture::global().records_for(REPEATER_TARGET);
    assert_eq!(
        records.len(),
        1,
        "one tick must emit exactly one repeater line"
    );
    assert_eq!(
        records[0].level, "ERROR",
        "SEC-9 says the repeating notice is an error, not a warning"
    );
    assert!(
        records[0].message.to_lowercase().contains("plaintext"),
        "the message must name the condition: {:?}",
        records[0].message
    );
    assert!(
        records[0]
            .fields
            .iter()
            .any(|(_, value)| value.contains("0.0.0.0:8080")),
        "the repeated error must name the address it is about: {:?}",
        records[0].fields
    );
}

/// A loopback bind must spawn **no task at all**.
///
/// Asserting "the task logs nothing" would pass for a task that wakes every 10 minutes to decide
/// not to log — a wakeup for nothing, forever, on every normal deployment. The handle is therefore
/// `None`, and this asserts the absence of the spawn rather than the absence of output.
#[tokio::test]
#[doc = "Spec: SEC-9"]
async fn loopback_bind_spawns_no_repeater() {
    // The sender is kept alive: dropping it would close the channel and make the spawned task exit
    // at once, which would make "is_some()" true but the task already dead — the assertion would
    // pass for the wrong reason.
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    assert!(
        spawn_plaintext_repeater(
            stop_rx.clone(),
            false,
            "127.0.0.1:8080".to_string(),
            Duration::from_secs(600)
        )
        .is_none(),
        "a loopback bind must not spawn a repeater task"
    );

    let spawned = spawn_plaintext_repeater(
        stop_rx,
        true,
        "0.0.0.0:8080".to_string(),
        Duration::from_secs(600),
    );
    assert!(
        spawned.is_some(),
        "a non-loopback plaintext bind must spawn the repeater"
    );
    let _ = stop_tx.send(true);
    if let Some(task) = spawned {
        task.shutdown().await;
    }
}

/// The repeater must not fire at `t = 0` (`§2`'s "classic `interval` trap").
///
/// The bug is invisible in manual testing: one extra error line next to the startup warning reads
/// as a duplicate rather than as a defect. It is only visible if a test looks at the window
/// *before* the first interval elapses, which is what this does.
#[tokio::test]
#[doc = "Spec: SEC-9"]
async fn the_repeater_does_not_fire_at_startup() {
    let _guard = capture_guard().await;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let interval = Duration::from_millis(400);

    let handle = spawn_plaintext_repeater(stop_rx, true, "0.0.0.0:8080".to_string(), interval)
        .expect("repeater must spawn");

    // A quarter of the way into the first interval: a naive `interval(..)` (whose first tick is
    // immediate) has already logged by now; a correct one has not.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let early = Capture::global().records_for(REPEATER_TARGET).len();
    let _ = stop_tx.send(true);
    handle.shutdown().await;
    assert_eq!(
        early, 0,
        "the repeater must not fire at startup; the first emission belongs at t = interval"
    );
}

// ---------------------------------------------------------------------------
// Both tasks — shutdown
// ---------------------------------------------------------------------------

/// Both periodic tasks must stop when the shutdown watch fires (`§1.4`, `§5`).
///
/// They share the `stop_rx`/`select!` shape, so one test covers both. A task that ignored the watch
/// would keep querying the audit table after a graceful drain had finished, which is the failure
/// mode "every spawned task must be stoppable" exists to prevent.
#[tokio::test]
#[doc = "Spec: REL-1, Spec: SEC-13, Spec: SEC-9"]
async fn the_periodic_tasks_stop_on_shutdown() {
    let (stop_tx, _stop_rx) = tokio::sync::watch::channel(false);

    let repeater = spawn_plaintext_repeater(
        stop_tx.subscribe(),
        true,
        "0.0.0.0:8080".to_string(),
        Duration::from_secs(600),
    )
    .expect("repeater must spawn");
    let anchor = spawn_audit_anchor_ticker(
        // No pool is queried before the first interval, so this handle is unused here; the test is
        // about the stop signal, not about the query.
        None,
        stop_tx.subscribe(),
        Duration::from_secs(3600),
        || 0,
    );

    let _ = stop_tx.send(true);

    // A task that observes the stop signal returns promptly; one that ignores it never does.
    tokio::time::timeout(Duration::from_secs(2), async {
        repeater.shutdown().await;
        anchor.shutdown().await;
    })
    .await
    .expect("both periodic tasks must stop when the shutdown watch fires");
}

// ---------------------------------------------------------------------------
// Wiring — the defect these issues describe is a *missing call*
// ---------------------------------------------------------------------------

/// `serve_command` must actually spawn both timers.
///
/// # Why this assertion is structural
///
/// The defect in `ISSUE-0186`/`ISSUE-0187` is not "the tick body is wrong", it is "the tick body is
/// never called". `run_retention_pass` in this same directory is the precedent: it was written,
/// tested, and never invoked, so the suite was green while the behaviour was absent. A test of the
/// function alone cannot catch an absent call.
///
/// Driving `serve_command` instead is not possible: it binds three listeners and runs until
/// `SIGTERM`, so any test that called it would have to be the very hang this cycle is trying to
/// avoid. That leaves reading the source, which is a weaker claim than an execution — and is
/// labelled as such rather than dressed up.
///
/// It is still worth having: it fails on the exact regression that produced these two issues
/// (someone deleting the spawn), which no behavioural test in this file would notice.
#[test]
#[doc = "Spec: AUD-1, Spec: SEC-13, Spec: SEC-9"]
fn serve_command_spawns_both_periodic_tasks() {
    const SOURCE: &str = include_str!("../serve.rs");

    // Scoped to `serve_command` so an unused helper elsewhere cannot satisfy the assertion.
    let start = SOURCE
        .find("async fn serve_command")
        .expect("serve_command must exist");
    let body = &SOURCE[start..];
    let end = body.find("\nasync fn ").unwrap_or(body.len());
    let serve_command_body = &body[..end];

    assert!(
        serve_command_body.contains("spawn_audit_anchor_ticker("),
        "serve_command must spawn the hourly audit anchor (ISSUE-0186); the tick body being \
         tested in isolation is what let the missing call survive"
    );
    assert!(
        serve_command_body.contains("spawn_plaintext_repeater("),
        "serve_command must spawn the plaintext repeater (ISSUE-0187)"
    );
    assert!(
        serve_command_body.contains("periodic_intervals("),
        "both tasks must take their period from `periodic_intervals`, so the documented defaults \
         cannot drift into a literal at the call site"
    );
}

/// The documented defaults are the numbers the specs name, and the test override cannot leak into
/// production by accident.
///
/// `SEC-13`/`AUD-1` say "at least hourly" and `SEC-9` says "every 10 minutes". Those two numbers are
/// the requirement; everything else in this file is about delivering them.
#[test]
#[doc = "Spec: AUD-1, Spec: SEC-13, Spec: SEC-9"]
fn periodic_interval_defaults_match_the_specs() {
    let (repeater, anchor) = periodic_intervals(None);
    assert_eq!(repeater.as_secs(), 600, "SEC-9 names 10 minutes");
    assert_eq!(anchor.as_secs(), 3600, "SEC-13 and AUD-1 name an hour");
    assert_eq!(
        (repeater, anchor),
        (PLAINTEXT_REPEAT_INTERVAL, AUDIT_ANCHOR_INTERVAL),
        "the fallback must be the named constants, not a second copy of the numbers"
    );

    // A zero or unparseable override must fall back rather than disable a timer.
    assert_eq!(
        periodic_intervals(Some("0")),
        (PLAINTEXT_REPEAT_INTERVAL, AUDIT_ANCHOR_INTERVAL),
        "a zero override must not silently disable the timers"
    );
    assert_eq!(
        periodic_intervals(Some("not-a-number")),
        (PLAINTEXT_REPEAT_INTERVAL, AUDIT_ANCHOR_INTERVAL),
        "an unparseable override must not change production defaults"
    );
    assert_eq!(
        periodic_intervals(Some("  ")),
        (PLAINTEXT_REPEAT_INTERVAL, AUDIT_ANCHOR_INTERVAL),
        "a blank override must not change production defaults"
    );

    // And a valid override does take effect, or the end-to-end test below could not exist.
    assert_eq!(
        periodic_intervals(Some("2")),
        (Duration::from_secs(2), Duration::from_secs(2))
    );
}
