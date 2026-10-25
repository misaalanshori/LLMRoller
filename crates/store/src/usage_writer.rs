//! Three-queue batching SQLite writer for usage events, attempts, and rollups.
//!
//! Spec: Section 7.2, Section 9.1, Section 9.2, Section 21.6,
//! Requirements: USG-1, SHR-6, REL-2, REL-3, Partial: TRC-9.

use parking_lot::Mutex;
use sqlx::{Sqlite, Transaction};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};

use llmroller_core::ids::{ClientKeyId, RequestId, UserId};
use llmroller_core::pricing::CostSource;
use llmroller_core::usage::AttemptOutcome;

/// Usage event data ready to be persisted into SQLite.
#[derive(Debug, Clone)]
pub struct UsageEventRecord {
    pub id: String, // ULID
    pub ts: i64,
    pub request_id: RequestId,
    pub client_key_id: ClientKeyId,
    pub owner_user_id: UserId,
    pub upstream_key_id: Option<String>,
    pub provider_id: Option<String>,
    pub provider_scope: Option<String>,
    pub share_id: Option<String>,
    pub canonical_id: Option<String>,
    pub upstream_model_id: Option<String>,
    pub endpoint: String,
    pub protocol: Option<String>,
    pub session_hash: Option<String>,
    pub status: i64,
    pub stream: bool,
    pub incomplete: bool,
    pub termination: String,
    pub trace_dropped: bool,
    pub input_uncached_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cost_micro_usd: i64,
    pub cost_source: CostSource,
    pub latency_ms: Option<i64>,
    pub ttfb_ms: Option<i64>,
    pub pin_outcome: Option<String>,
    pub failovers: i64,
    pub error_code: Option<String>,
    pub list_cost_micro_usd: i64,
    pub plan_multiplier_bps: i64,
    pub billing_coverage_mode: String,
    pub service_account_id: Option<String>,
    pub stop_reason: Option<String>,
    pub request_headers_json: Option<String>,
    pub metadata_json: Option<String>,
}

/// Upstream attempt data recorded during the failover loop.
#[derive(Debug, Clone)]
pub struct UsageAttemptRecord {
    pub request_id: RequestId,
    pub attempt_no: i64,
    pub ts: i64,
    pub upstream_key_id: String,
    pub share_id: Option<String>,
    pub outcome: AttemptOutcome,
    pub http_status: Option<i64>,
    pub bytes_sent: i64,
    pub charged_micro_usd: i64,
    pub charge_basis: String,
}

/// Key identifying a 5-minute rollup bucket.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rollup5MinKey {
    pub bucket_ms: i64,
    pub upstream_key_id: String,
    pub canonical_id: String,
    pub client_key_id: String,
    pub share_id: String,
    pub owner_user_id: String,
    pub provider_scope: String,
}

/// Key identifying an hourly rollup bucket.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RollupHourlyKey {
    pub hour_ms: i64,
    pub owner_user_id: String,
    pub client_key_id: String,
    pub upstream_key_id: String,
    pub provider_id: String,
    pub provider_scope: String,
    pub share_id: String,
    pub canonical_id: String,
}

/// Accumulated usage delta for rollups.
#[derive(Debug, Clone, Default)]
pub struct RollupDelta {
    pub requests: i64,
    pub errors: i64,
    pub input_uncached_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cost_micro_usd: i64,
    pub list_cost_micro_usd: i64,
}

impl RollupDelta {
    pub fn merge(&mut self, other: &Self) {
        self.requests += other.requests;
        self.errors += other.errors;
        self.input_uncached_tokens += other.input_uncached_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.cost_micro_usd += other.cost_micro_usd;
        self.list_cost_micro_usd += other.list_cost_micro_usd;
    }
}

/// Three-queue in-memory usage batcher:
/// 1. `durable_state`: essential confirmation/pin/link queue
/// 2. `rollups`: non-dropping aggregated metric queues
/// 3. `raw_events`: bounded FIFO queue dropping oldest under pressure (REL-3)
#[derive(Debug)]
pub struct ThreeQueueUsageWriter {
    pub rollups_5min: Mutex<HashMap<Rollup5MinKey, RollupDelta>>,
    pub rollups_hourly: Mutex<HashMap<RollupHourlyKey, RollupDelta>>,
    pub raw_events: Mutex<VecDeque<(UsageEventRecord, Vec<UsageAttemptRecord>)>>,
    pub dropped_raw_events_count: AtomicUsize,
    max_raw_items: usize,
}

impl ThreeQueueUsageWriter {
    pub fn new(max_raw_items: usize) -> Self {
        Self {
            rollups_5min: Mutex::new(HashMap::new()),
            rollups_hourly: Mutex::new(HashMap::new()),
            raw_events: Mutex::new(VecDeque::new()),
            dropped_raw_events_count: AtomicUsize::new(0),
            max_raw_items,
        }
    }

    /// Enqueue a completed or failed request.
    ///
    /// Preserves 100% of tokens and costs in rollups even if raw events are evicted (REL-3).
    pub fn enqueue(&self, event: UsageEventRecord, attempts: Vec<UsageAttemptRecord>) {
        let is_error = event.status >= 400 && event.termination != "client_gone";

        // 1. Fold into non-dropping 5-minute rollups
        let bucket_5min = (event.ts / 300_000) * 300_000;
        let r5_key = Rollup5MinKey {
            bucket_ms: bucket_5min,
            upstream_key_id: event.upstream_key_id.clone().unwrap_or_default(),
            canonical_id: event.canonical_id.clone().unwrap_or_default(),
            client_key_id: event.client_key_id.to_string(),
            share_id: event.share_id.clone().unwrap_or_default(),
            owner_user_id: event.owner_user_id.to_string(),
            provider_scope: event
                .provider_scope
                .clone()
                .unwrap_or_else(|| "none".to_string()),
        };

        {
            let mut r5_lock = self.rollups_5min.lock();
            let d = r5_lock.entry(r5_key).or_default();
            d.requests += 1;
            d.cost_micro_usd += event.cost_micro_usd;
        }

        // 2. Fold into non-dropping hourly rollups
        let bucket_hourly = (event.ts / 3_600_000) * 3_600_000;
        let rh_key = RollupHourlyKey {
            hour_ms: bucket_hourly,
            owner_user_id: event.owner_user_id.to_string(),
            client_key_id: event.client_key_id.to_string(),
            upstream_key_id: event.upstream_key_id.clone().unwrap_or_default(),
            provider_id: event.provider_id.clone().unwrap_or_default(),
            provider_scope: event
                .provider_scope
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            share_id: event.share_id.clone().unwrap_or_default(),
            canonical_id: event.canonical_id.clone().unwrap_or_default(),
        };

        {
            let mut rh_lock = self.rollups_hourly.lock();
            let d = rh_lock.entry(rh_key).or_default();
            d.requests += 1;
            if is_error {
                d.errors += 1;
            }
            d.input_uncached_tokens += event.input_uncached_tokens;
            d.cache_read_tokens += event.cache_read_tokens;
            d.cache_write_tokens += event.cache_write_tokens;
            d.output_tokens += event.output_tokens;
            d.reasoning_tokens += event.reasoning_tokens;
            d.cost_micro_usd += event.cost_micro_usd;
            d.list_cost_micro_usd += event.list_cost_micro_usd;
        }

        // 2b. Fold non-final failed attempt charges into 5-minute and hourly rollups (USG-1, USG-3, SHR-6)
        for att in &attempts {
            if att.outcome != AttemptOutcome::Served
                && att.charged_micro_usd > 0
                && !att.upstream_key_id.is_empty()
                && att.upstream_key_id.as_str() != event.upstream_key_id.as_deref().unwrap_or("")
            {
                let att_bucket_5min = (att.ts / 300_000) * 300_000;
                let att_r5_key = Rollup5MinKey {
                    bucket_ms: att_bucket_5min,
                    upstream_key_id: att.upstream_key_id.clone(),
                    canonical_id: event.canonical_id.clone().unwrap_or_default(),
                    client_key_id: event.client_key_id.to_string(),
                    share_id: att.share_id.clone().unwrap_or_default(),
                    owner_user_id: event.owner_user_id.to_string(),
                    provider_scope: if att.share_id.is_some() {
                        "shared".to_string()
                    } else {
                        event
                            .provider_scope
                            .clone()
                            .unwrap_or_else(|| "none".to_string())
                    },
                };
                {
                    let mut r5_lock = self.rollups_5min.lock();
                    let d = r5_lock.entry(att_r5_key).or_default();
                    d.cost_micro_usd += att.charged_micro_usd;
                }

                let att_bucket_hourly = (att.ts / 3_600_000) * 3_600_000;
                let att_rh_key = RollupHourlyKey {
                    hour_ms: att_bucket_hourly,
                    owner_user_id: event.owner_user_id.to_string(),
                    client_key_id: event.client_key_id.to_string(),
                    upstream_key_id: att.upstream_key_id.clone(),
                    provider_id: event.provider_id.clone().unwrap_or_default(),
                    provider_scope: if att.share_id.is_some() {
                        "shared".to_string()
                    } else {
                        event
                            .provider_scope
                            .clone()
                            .unwrap_or_else(|| "none".to_string())
                    },
                    share_id: att.share_id.clone().unwrap_or_default(),
                    canonical_id: event.canonical_id.clone().unwrap_or_default(),
                };
                {
                    let mut rh_lock = self.rollups_hourly.lock();
                    let d = rh_lock.entry(att_rh_key).or_default();
                    d.errors += 1;
                    d.cost_micro_usd += att.charged_micro_usd;
                    d.list_cost_micro_usd += att.charged_micro_usd;
                }
            }
        }

        // 3. Enqueue into bounded raw_events, evicting oldest if capacity exceeded (REL-3)
        {
            let mut raw_lock = self.raw_events.lock();
            while raw_lock.len() >= self.max_raw_items {
                raw_lock.pop_front();
                self.dropped_raw_events_count
                    .fetch_add(1, Ordering::Relaxed);
            }
            raw_lock.push_back((event, attempts));
        }
    }

    /// Restores a previously drained batch into memory if a transaction fails before commit (`REL-1`, `REL-2`).
    pub fn restore_batch(
        &self,
        r5_items: HashMap<Rollup5MinKey, RollupDelta>,
        rh_items: HashMap<RollupHourlyKey, RollupDelta>,
        raw_items: VecDeque<(UsageEventRecord, Vec<UsageAttemptRecord>)>,
    ) {
        if !r5_items.is_empty() {
            let mut lock = self.rollups_5min.lock();
            for (k, v) in r5_items {
                lock.entry(k).or_default().merge(&v);
            }
        }
        if !rh_items.is_empty() {
            let mut lock = self.rollups_hourly.lock();
            for (k, v) in rh_items {
                lock.entry(k).or_default().merge(&v);
            }
        }
        if !raw_items.is_empty() {
            let mut lock = self.raw_events.lock();
            for item in raw_items.into_iter().rev() {
                if lock.len() < self.max_raw_items {
                    lock.push_front(item);
                }
            }
        }
    }

    /// Flushes all enqueued rollups and raw events into SQLite in a single transaction.
    pub async fn flush_to_db(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<usize, sqlx::Error> {
        let r5_items: HashMap<Rollup5MinKey, RollupDelta> = {
            let mut lock = self.rollups_5min.lock();
            std::mem::take(&mut *lock)
        };

        let rh_items: HashMap<RollupHourlyKey, RollupDelta> = {
            let mut lock = self.rollups_hourly.lock();
            std::mem::take(&mut *lock)
        };

        let raw_items: VecDeque<(UsageEventRecord, Vec<UsageAttemptRecord>)> = {
            let mut lock = self.raw_events.lock();
            std::mem::take(&mut *lock)
        };

        let r5_backup = r5_items.clone();
        let rh_backup = rh_items.clone();
        let raw_backup = raw_items.clone();

        let res = self
            .flush_to_db_inner(tx, r5_items, rh_items, raw_items)
            .await;
        if res.is_err() {
            self.restore_batch(r5_backup, rh_backup, raw_backup);
        }
        res
    }

    async fn flush_to_db_inner(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        r5_items: HashMap<Rollup5MinKey, RollupDelta>,
        rh_items: HashMap<RollupHourlyKey, RollupDelta>,
        raw_items: VecDeque<(UsageEventRecord, Vec<UsageAttemptRecord>)>,
    ) -> Result<usize, sqlx::Error> {
        // 1. Commit 5-minute rollups
        for (k, d) in r5_items {
            sqlx::query(
                r#"INSERT INTO usage_rollups_5min (
                       bucket_ms, upstream_key_id, canonical_id, client_key_id,
                       owner_user_id, provider_scope, share_id, cost_micro_usd, requests
                   )
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT (bucket_ms, upstream_key_id, canonical_id, client_key_id, share_id) DO UPDATE SET
                       cost_micro_usd = usage_rollups_5min.cost_micro_usd + excluded.cost_micro_usd,
                       requests = usage_rollups_5min.requests + excluded.requests"#,
            )
            .bind(k.bucket_ms)
            .bind(&k.upstream_key_id)
            .bind(&k.canonical_id)
            .bind(&k.client_key_id)
            .bind(&k.owner_user_id)
            .bind(&k.provider_scope)
            .bind(&k.share_id)
            .bind(d.cost_micro_usd)
            .bind(d.requests)
            .execute(&mut **tx)
            .await?;
        }

        // 2. Commit hourly rollups
        for (k, d) in rh_items {
            sqlx::query(
                r#"INSERT INTO usage_rollups_hourly (
                       hour_ms, owner_user_id, client_key_id, upstream_key_id,
                       provider_id, provider_scope, share_id, canonical_id,
                       requests, errors, input_uncached_tokens, cache_read_tokens,
                       cache_write_tokens, output_tokens, reasoning_tokens, cost_micro_usd,
                       list_cost_micro_usd
                   )
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT (hour_ms, client_key_id, upstream_key_id, canonical_id, share_id) DO UPDATE SET
                       requests = usage_rollups_hourly.requests + excluded.requests,
                       errors = usage_rollups_hourly.errors + excluded.errors,
                       input_uncached_tokens = usage_rollups_hourly.input_uncached_tokens + excluded.input_uncached_tokens,
                       cache_read_tokens = usage_rollups_hourly.cache_read_tokens + excluded.cache_read_tokens,
                       cache_write_tokens = usage_rollups_hourly.cache_write_tokens + excluded.cache_write_tokens,
                       output_tokens = usage_rollups_hourly.output_tokens + excluded.output_tokens,
                       reasoning_tokens = usage_rollups_hourly.reasoning_tokens + excluded.reasoning_tokens,
                       cost_micro_usd = usage_rollups_hourly.cost_micro_usd + excluded.cost_micro_usd,
                       list_cost_micro_usd = usage_rollups_hourly.list_cost_micro_usd + excluded.list_cost_micro_usd"#,
            )
            .bind(k.hour_ms)
            .bind(&k.owner_user_id)
            .bind(&k.client_key_id)
            .bind(&k.upstream_key_id)
            .bind(&k.provider_id)
            .bind(&k.provider_scope)
            .bind(&k.share_id)
            .bind(&k.canonical_id)
            .bind(d.requests)
            .bind(d.errors)
            .bind(d.input_uncached_tokens)
            .bind(d.cache_read_tokens)
            .bind(d.cache_write_tokens)
            .bind(d.output_tokens)
            .bind(d.reasoning_tokens)
            .bind(d.cost_micro_usd)
            .bind(d.list_cost_micro_usd)
            .execute(&mut **tx)
            .await?;
        }

        let raw_count = raw_items.len();

        // 3. Commit raw usage_events and usage_attempts with per-record SAVEPOINT isolation.
        // A single malformed row (constraint or foreign-key race) must never roll back the
        // valid rows batched in the same transaction.
        for (e, attempts) in raw_items {
            sqlx::query("SAVEPOINT sp_usage_event")
                .execute(&mut **tx)
                .await?;
            let event_result = insert_usage_event(tx, &e).await;
            if let Err(err) = event_result {
                tracing::warn!(
                    usage_event_id = %e.id,
                    request_id = %e.request_id,
                    error = %err,
                    "dropping invalid usage_event row without aborting the batch"
                );
                sqlx::query("ROLLBACK TO SAVEPOINT sp_usage_event")
                    .execute(&mut **tx)
                    .await?;
                sqlx::query("RELEASE SAVEPOINT sp_usage_event")
                    .execute(&mut **tx)
                    .await?;
                continue;
            }
            sqlx::query("RELEASE SAVEPOINT sp_usage_event")
                .execute(&mut **tx)
                .await?;

            for a in attempts {
                sqlx::query("SAVEPOINT sp_usage_attempt")
                    .execute(&mut **tx)
                    .await?;
                let attempt_result = insert_usage_attempt(tx, &a).await;
                match attempt_result {
                    Ok(()) => {
                        sqlx::query("RELEASE SAVEPOINT sp_usage_attempt")
                            .execute(&mut **tx)
                            .await?;
                    }
                    Err(err) => {
                        tracing::warn!(
                            request_id = %a.request_id,
                            attempt_no = a.attempt_no,
                            outcome = a.outcome.as_str(),
                            error = %err,
                            "dropping invalid usage_attempt row without aborting the batch"
                        );
                        sqlx::query("ROLLBACK TO SAVEPOINT sp_usage_attempt")
                            .execute(&mut **tx)
                            .await?;
                        sqlx::query("RELEASE SAVEPOINT sp_usage_attempt")
                            .execute(&mut **tx)
                            .await?;
                    }
                }
            }
        }

        Ok(raw_count)
    }
}

fn extract_json_string_field(
    json_opt: Option<&str>,
    keys: &[&str],
    max_len: usize,
) -> Option<String> {
    let raw = json_opt?;
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = val.as_object()?;
    for (k, v) in obj {
        if keys.iter().any(|target| k.eq_ignore_ascii_case(target))
            && let Some(s) = v.as_str()
        {
            let sanitized: String = s
                .chars()
                .filter(|c| !c.is_control())
                .take(max_len)
                .collect();
            if !sanitized.is_empty() {
                return Some(sanitized);
            }
        }
    }
    None
}

async fn insert_usage_event(
    tx: &mut Transaction<'_, Sqlite>,
    e: &UsageEventRecord,
) -> Result<(), sqlx::Error> {
    let upstream_key = e
        .upstream_key_id
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    let provider = e.provider_id.as_deref().filter(|s| !s.trim().is_empty());
    let share = e.share_id.as_deref().filter(|s| !s.trim().is_empty());
    let client_user_agent =
        extract_json_string_field(e.request_headers_json.as_deref(), &["user-agent"], 256).or_else(
            || {
                extract_json_string_field(
                    e.metadata_json.as_deref(),
                    &["client_user_agent", "user_agent"],
                    256,
                )
            },
        );
    let client_location = extract_json_string_field(
        e.metadata_json.as_deref(),
        &["client_location", "location"],
        128,
    )
    .or_else(|| {
        extract_json_string_field(
            e.request_headers_json.as_deref(),
            &["cf-ipcountry", "x-client-location"],
            128,
        )
    });

    sqlx::query(
        r#"INSERT INTO usage_events (
               id, ts, request_id, client_key_id, owner_user_id,
               upstream_key_id, provider_id, provider_scope, share_id,
               canonical_id, upstream_model_id, endpoint, protocol,
               session_hash, status, stream, incomplete, termination,
               trace_dropped, input_uncached_tokens, cache_read_tokens,
               cache_write_tokens, output_tokens, reasoning_tokens,
               cost_micro_usd, cost_source, latency_ms, ttfb_ms,
               pin_outcome, failovers, error_code,
               list_cost_micro_usd, plan_multiplier_bps, billing_coverage_mode,
               service_account_id, stop_reason, client_user_agent, client_location,
               request_headers_json, metadata_json
           )
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&e.id)
    .bind(e.ts)
    .bind(e.request_id.to_string())
    .bind(e.client_key_id.to_string())
    .bind(e.owner_user_id.to_string())
    .bind(upstream_key)
    .bind(provider)
    .bind(&e.provider_scope)
    .bind(share)
    .bind(&e.canonical_id)
    .bind(&e.upstream_model_id)
    .bind(&e.endpoint)
    .bind(&e.protocol)
    .bind(&e.session_hash)
    .bind(e.status)
    .bind(if e.stream { 1 } else { 0 })
    .bind(if e.incomplete { 1 } else { 0 })
    .bind(&e.termination)
    .bind(if e.trace_dropped { 1 } else { 0 })
    .bind(e.input_uncached_tokens)
    .bind(e.cache_read_tokens)
    .bind(e.cache_write_tokens)
    .bind(e.output_tokens)
    .bind(e.reasoning_tokens)
    .bind(e.cost_micro_usd)
    .bind(e.cost_source.as_str())
    .bind(e.latency_ms)
    .bind(e.ttfb_ms)
    .bind(&e.pin_outcome)
    .bind(e.failovers)
    .bind(&e.error_code)
    .bind(e.list_cost_micro_usd)
    .bind(e.plan_multiplier_bps)
    .bind(&e.billing_coverage_mode)
    .bind(&e.service_account_id)
    .bind(&e.stop_reason)
    .bind(client_user_agent)
    .bind(client_location)
    .bind(&e.request_headers_json)
    .bind(&e.metadata_json)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_usage_attempt(
    tx: &mut Transaction<'_, Sqlite>,
    a: &UsageAttemptRecord,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO usage_attempts (
               request_id, attempt_no, ts, upstream_key_id,
               share_id, outcome, http_status, bytes_sent,
               charged_micro_usd, charge_basis
           )
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(a.request_id.to_string())
    .bind(a.attempt_no)
    .bind(a.ts)
    .bind(&a.upstream_key_id)
    .bind(&a.share_id)
    .bind(a.outcome.as_str())
    .bind(a.http_status)
    .bind(a.bytes_sent)
    .bind(a.charged_micro_usd)
    .bind(&a.charge_basis)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::migrate;
    use crate::pool::{Role, open_pool};
    use sqlx::Row as _;

    #[tokio::test]
    #[doc = "Spec: USG-1, USG-3, SHR-6, USG-9"]
    async fn failed_attempt_charges_and_client_user_agent_are_persisted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = open_pool(&dir.path().join("usage_writer.db"), Role::Writer)
            .await
            .expect("open pool");
        migrate(&pool).await.expect("migrate");

        let writer = ThreeQueueUsageWriter::new(100);
        let req_id = RequestId::generate();
        let ck_id = ClientKeyId::generate();
        let uid = UserId::generate();

        let event = UsageEventRecord {
            id: ulid::Ulid::generate().to_string(),
            ts: 600_000,
            request_id: req_id,
            client_key_id: ck_id,
            owner_user_id: uid,
            upstream_key_id: Some("uk_final".to_string()),
            provider_id: Some("prv_1".to_string()),
            provider_scope: Some("global".to_string()),
            share_id: None,
            canonical_id: Some("gpt-5.4".to_string()),
            upstream_model_id: Some("openai/gpt-5.4".to_string()),
            endpoint: "chat".to_string(),
            protocol: Some("chat".to_string()),
            session_hash: None,
            status: 200,
            stream: false,
            incomplete: false,
            termination: "completed".to_string(),
            trace_dropped: false,
            input_uncached_tokens: 100,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            output_tokens: 50,
            reasoning_tokens: 0,
            cost_micro_usd: 1_500,
            cost_source: CostSource::PriceBook,
            latency_ms: Some(120),
            ttfb_ms: Some(40),
            pin_outcome: Some("new_pin".to_string()),
            failovers: 1,
            error_code: None,
            list_cost_micro_usd: 1_500,
            plan_multiplier_bps: 10_000,
            billing_coverage_mode: "pay_as_you_go".to_string(),
            service_account_id: None,
            stop_reason: Some("stop".to_string()),
            request_headers_json: Some(
                r#"{"user-agent":"ClaudeCode/1.2.3","cf-ipcountry":"US"}"#.to_string(),
            ),
            metadata_json: None,
        };
        let attempts = vec![
            UsageAttemptRecord {
                request_id: req_id,
                attempt_no: 1,
                ts: 599_000,
                upstream_key_id: "uk_failed_1".to_string(),
                share_id: None,
                outcome: AttemptOutcome::Http5xx,
                http_status: Some(502),
                bytes_sent: 0,
                charged_micro_usd: 400,
                charge_basis: "estimated_input".to_string(),
            },
            UsageAttemptRecord {
                request_id: req_id,
                attempt_no: 2,
                ts: 600_000,
                upstream_key_id: "uk_final".to_string(),
                share_id: None,
                outcome: AttemptOutcome::Served,
                http_status: Some(200),
                bytes_sent: 512,
                charged_micro_usd: 1_500,
                charge_basis: "provider_reported".to_string(),
            },
        ];

        writer.enqueue(event, attempts);

        let mut tx = pool.begin().await.expect("begin");
        writer.flush_to_db(&mut tx).await.expect("flush");
        tx.commit().await.expect("commit");

        let failed_rollup_cost: i64 = sqlx::query_scalar(
            "SELECT cost_micro_usd FROM usage_rollups_5min WHERE upstream_key_id = 'uk_failed_1'",
        )
        .fetch_one(&pool)
        .await
        .expect("failed attempt rollup");
        assert_eq!(failed_rollup_cost, 400);

        let row = sqlx::query(
            "SELECT client_user_agent, client_location FROM usage_events WHERE request_id = ?",
        )
        .bind(req_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("usage_event row");
        assert_eq!(
            row.try_get::<Option<String>, _>("client_user_agent")
                .expect("client_user_agent column")
                .as_deref(),
            Some("ClaudeCode/1.2.3")
        );
        assert_eq!(
            row.try_get::<Option<String>, _>("client_location")
                .expect("client_location column")
                .as_deref(),
            Some("US")
        );
    }
}
