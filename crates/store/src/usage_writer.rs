//! Three-queue batching SQLite writer for usage events, attempts, and rollups.
//!
//! Spec: Section 7.2, Section 9.1, Section 9.2, Section 21.6,
//! Requirements: USG-1, SHR-6, REL-2, REL-3, Partial: TRC-9.

use parking_lot::Mutex;
use sqlx::{Sqlite, Transaction};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};

use llmroller_core::ids::{ClientKeyId, RequestId, UserId};

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
    pub cost_source: String,
    pub latency_ms: Option<i64>,
    pub ttfb_ms: Option<i64>,
    pub pin_outcome: Option<String>,
    pub failovers: i64,
    pub error_code: Option<String>,
}

/// Upstream attempt data recorded during the failover loop.
#[derive(Debug, Clone)]
pub struct UsageAttemptRecord {
    pub request_id: RequestId,
    pub attempt_no: i64,
    pub ts: i64,
    pub upstream_key_id: String,
    pub share_id: Option<String>,
    pub outcome: String,
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
        let is_error = event.status >= 400;

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
                       cache_write_tokens, output_tokens, reasoning_tokens, cost_micro_usd
                   )
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT (hour_ms, client_key_id, upstream_key_id, canonical_id, share_id) DO UPDATE SET
                       requests = usage_rollups_hourly.requests + excluded.requests,
                       errors = usage_rollups_hourly.errors + excluded.errors,
                       input_uncached_tokens = usage_rollups_hourly.input_uncached_tokens + excluded.input_uncached_tokens,
                       cache_read_tokens = usage_rollups_hourly.cache_read_tokens + excluded.cache_read_tokens,
                       cache_write_tokens = usage_rollups_hourly.cache_write_tokens + excluded.cache_write_tokens,
                       output_tokens = usage_rollups_hourly.output_tokens + excluded.output_tokens,
                       reasoning_tokens = usage_rollups_hourly.reasoning_tokens + excluded.reasoning_tokens,
                       cost_micro_usd = usage_rollups_hourly.cost_micro_usd + excluded.cost_micro_usd"#,
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
            .execute(&mut **tx)
            .await?;
        }

        let raw_count = raw_items.len();

        // 3. Commit raw usage_events and usage_attempts
        for (e, attempts) in raw_items {
            sqlx::query(
                r#"INSERT INTO usage_events (
                       id, ts, request_id, client_key_id, owner_user_id,
                       upstream_key_id, provider_id, provider_scope, share_id,
                       canonical_id, upstream_model_id, endpoint, protocol,
                       session_hash, status, stream, incomplete, termination,
                       trace_dropped, input_uncached_tokens, cache_read_tokens,
                       cache_write_tokens, output_tokens, reasoning_tokens,
                       cost_micro_usd, cost_source, latency_ms, ttfb_ms,
                       pin_outcome, failovers, error_code
                   )
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(&e.id)
            .bind(e.ts)
            .bind(e.request_id.to_string())
            .bind(e.client_key_id.to_string())
            .bind(e.owner_user_id.to_string())
            .bind(&e.upstream_key_id)
            .bind(&e.provider_id)
            .bind(&e.provider_scope)
            .bind(&e.share_id)
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
            .bind(&e.cost_source)
            .bind(e.latency_ms)
            .bind(e.ttfb_ms)
            .bind(&e.pin_outcome)
            .bind(e.failovers)
            .bind(&e.error_code)
            .execute(&mut **tx)
            .await?;

            for a in attempts {
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
                .bind(&a.outcome)
                .bind(a.http_status)
                .bind(a.bytes_sent)
                .bind(a.charged_micro_usd)
                .bind(&a.charge_basis)
                .execute(&mut **tx)
                .await?;
            }
        }

        Ok(raw_count)
    }
}
