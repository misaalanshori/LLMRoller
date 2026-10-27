//! Terminal usage accounting: the shared event emitter for every path that ends a
//! request (`USG-1`, `USG-3`).
//!
//! Extracted from `pipeline.rs` verbatim. The doc comments on
//! [`TerminalUsageFacts`] and [`enqueue_terminal_usage`] are the record of two
//! decisions that must not be lost: the attribution rule (a key is `None` **only**
//! when no attempt reached any upstream key) and the "one builder, many call sites"
//! rationale that ended the duplicate-`UsageContext` pattern.
//!
//! Spec: ./docs/srdd/9. SRDD - Data Model.md
//! Spec: ./docs/srdd/13. SRDD - Pricing, Limits, and Source Provider Usage Engine.md

use super::*;

/// Everything needed to enqueue one terminal usage event plus its attempt rows.
///
/// Introduced so the terminal all-attempts-failed path and the charged-timeout abort
/// share one emitter instead of each building a `UsageContext` literal. Six copies of
/// that literal already exist in this crate, and the duplication is how the
/// `timeout_failover == false` branch came to charge money and then emit nothing
/// (`USG-1`, `USG-3`, `ISSUE-0149`).
pub(crate) struct TerminalUsageFacts<'a> {
    pub(crate) request_id: RequestId,
    pub(crate) caller: &'a ClientKeyRecord,
    pub(crate) canonical_model: &'a str,
    /// `None` only when no attempt reached any upstream key (true exhaustion).
    pub(crate) upstream_key_id: Option<String>,
    pub(crate) provider_id: Option<String>,
    /// `None` only when no attempt reached a key; otherwise the real tier label.
    pub(crate) provider_scope: Option<String>,
    pub(crate) share_id: Option<String>,
    pub(crate) upstream_model_id: String,
    pub(crate) endpoint_str: &'a str,
    pub(crate) session_hash: Option<String>,
    pub(crate) pin_outcome: Option<String>,
    pub(crate) failovers: i64,
    pub(crate) error_code: Option<String>,
    pub(crate) billing_coverage_mode: String,
    pub(crate) plan_multiplier_bps: i64,
    pub(crate) request_headers_json: Option<String>,
    /// HTTP status recorded on the event.
    pub(crate) status: StatusCode,
    pub(crate) is_streaming: bool,
    /// `Termination` label; see [`Termination::as_str`].
    pub(crate) termination: &'a str,
    /// Amount actually charged, in micro-USD.
    pub(crate) cost_micro_usd: i64,
    pub(crate) cost_source: CostSource,
    pub(crate) now: i64,
    pub(crate) latency_ms: Option<i64>,
}

/// Enqueue one terminal usage event together with all attempt rows for the request.
///
/// A no-op when no writer is attached (CLI and unit-test paths), which is the same
/// contract the inline blocks had.
pub(crate) fn enqueue_terminal_usage(
    state: &ProxyAuthState,
    facts: TerminalUsageFacts<'_>,
    attempts: Vec<UsageAttemptRecord>,
) {
    let Some(writer) = &state.writer else {
        return;
    };
    let usage = UsageContext {
        request_id: facts.request_id,
        client_key_id: facts.caller.id,
        owner_user_id: facts.caller.owner_user_id,
        canonical_model: facts.canonical_model.to_string(),
        upstream_key_id: facts.upstream_key_id,
        provider_id: facts.provider_id,
        provider_scope: facts.provider_scope,
        share_id: facts.share_id,
        upstream_model_id: facts.upstream_model_id,
        endpoint_str: facts.endpoint_str.to_string(),
        session_hash: facts.session_hash,
        pin_outcome: facts.pin_outcome,
        failovers: facts.failovers,
        error_code: facts.error_code,
        billing_coverage_mode: facts.billing_coverage_mode,
        plan_multiplier_bps: facts.plan_multiplier_bps,
        service_account_id: facts
            .caller
            .service_account_id
            .as_ref()
            .map(ToString::to_string),
        request_headers_json: facts.request_headers_json,
    };
    let event = usage.event(
        facts.now,
        facts.status.as_u16(),
        facts.is_streaming,
        true,
        facts.termination,
        &llmroller_proto::json_usage::ExtractedUsage::default(),
        facts.cost_micro_usd,
        facts.cost_source,
        None,
        None,
        facts.latency_ms,
    );
    writer.queues.usage_writer.enqueue(event, attempts);
}

/// Billing coverage mode and plan multiplier for a provider, defaulting to
/// pay-as-you-go at 1.0x when the provider row is absent.
///
/// Extracted because the same four-line lookup is repeated at four call sites; a fifth
/// was about to be added for `ISSUE-0150`.
pub(crate) fn provider_billing(
    snapshot: &ConfigSnapshot,
    provider_id: &ProviderId,
) -> (String, i64) {
    snapshot
        .providers
        .get(provider_id)
        .map(|p| (p.billing_coverage_mode.clone(), p.plan_multiplier_bps))
        .unwrap_or_else(|| ("pay_as_you_go".to_string(), 10_000))
}
