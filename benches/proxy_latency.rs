#![forbid(unsafe_code)]
//! End-to-end performance and resource benchmark harness for PERF-1 through PERF-5.
//!
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-latency-and-capacity-perf
//! Spec: ./docs/srdd/16. SRDD - Testing Plan and TDD Governance.md
//! Phase: ./docs/bigbang/big-bang-phases-2.md#27-sub-phase-27-v1-orchestrator-v1-pre-flight-body-memory-budget-graceful-sigterm-drain-and-early-load-harness
//!
//! Run the default quick suite with `cargo bench -p llmroller-server --bench proxy_latency`.
//! Run the full reference scale (50,000,000 raw usage rows, 2,000 streams, 250,000 pins) with
//! `cargo bench -p llmroller-server --bench proxy_latency -- --full`.
//!
//! `cargo test --all-targets` also compiles and invokes this `harness = false` target in the dev
//! profile. Debug builds print the invocation hint and skip the load run so ordinary test matrices
//! do not seed a 50M-row database. The bench profile used by `cargo bench` has debug assertions
//! disabled and therefore runs the real assertions.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::stream::BoxStream;
use http_body_util::BodyExt as _;
use llmroller_core::eligibility::CandidatePoolTier;
use llmroller_core::ids::{ClientKeyId, ModelPk, UpstreamKeyId, UserId};
use llmroller_core::pins::{PinEntry, PinKey, PinState, PinStore};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role};
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::seams::{
    EntropySource, ExposeSecret as _, OsEntropy, secret_bytes, secret_string,
};
use llmroller_proto::adapters::EndpointKind;
use llmroller_proto::sse_frame::extract_frame_error;
use llmroller_proto::sse_tap::SseUsageTap;
use llmroller_proto::trace_tap::{TraceTap, prepare_request_blocks, replace_binary_blocks};
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::forward::SseChunkForwarder;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_server::reloader::load_initial_snapshot;
use llmroller_store::budgets::reconstruct_budgets_from_rollups;
use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
use llmroller_store::crypto::{MasterKey, XChaCha20Poly1305Sealer};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pins::{ActivePinRow, load_active_pins};
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::traces::SealerPool;
use serde_json::json;
use sqlx::SqlitePool;
use sysinfo::{ProcessesToUpdate, System};
use tokio::task::JoinSet;
use tower::ServiceExt as _;

const MIB: u64 = 1024 * 1024;
const PERF3_RSS_LIMIT: u64 = 300 * MIB + 64 * MIB;
const PERF5_TRACE_BUDGET: u64 = 204 * MIB + 64 * MIB;
const PERF4_COLD_START_LIMIT: Duration = Duration::from_secs(2);
const PERF4_IDLE_RSS_LIMIT: u64 = 50 * MIB;
/// Open SSE streams are already past their first frame, so this body is not
/// part of the pre-first-byte retention under test. PERF-3 separately holds 64
/// real 1 MiB bodies in the pre-first phase.
const PERF3_STREAM_BODY_BYTES: usize = 1024;
/// Representative chat-completions body for the traceability TTFB A/B test.
const PERF5_BODY_BYTES: usize = 64 * 1024;
const BENCH_MODEL: &str = "bench-model";
const INSTANCE_KEY_ID: &str = "bench-master-key";

#[derive(Debug, Clone, Copy)]
struct Scale {
    full: bool,
    events: usize,
    keys: usize,
    models: usize,
    rollups_per_key: usize,
    perf3_pins: usize,
    perf4_pins: usize,
    streams: usize,
    pre_first: usize,
    perf1_64k_requests: usize,
    perf1_512k_requests: usize,
    perf1_2m_requests: usize,
    perf5_requests: usize,
    sse_frames: usize,
    slow_delay_ms: u64,
}

impl Scale {
    const fn quick() -> Self {
        Self {
            full: false,
            events: 100_000,
            keys: 50,
            models: 10,
            rollups_per_key: 2_016,
            perf3_pins: 10_000,
            perf4_pins: 5_000,
            streams: 200,
            pre_first: 16,
            perf1_64k_requests: 500,
            perf1_512k_requests: 100,
            perf1_2m_requests: 50,
            perf5_requests: 300,
            sse_frames: 10_000,
            slow_delay_ms: 5_000,
        }
    }

    const fn full() -> Self {
        Self {
            full: true,
            events: 50_000_000,
            keys: 500,
            models: 20,
            rollups_per_key: 2_016,
            perf3_pins: 250_000,
            perf4_pins: 10_000,
            streams: 2_000,
            pre_first: 64,
            perf1_64k_requests: 2_000,
            perf1_512k_requests: 500,
            perf1_2m_requests: 100,
            perf5_requests: 1_000,
            sse_frames: 50_000,
            slow_delay_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Stats {
    count: usize,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    mean_ms: f64,
}

impl Stats {
    fn from_durations(durations: &[Duration]) -> Self {
        let mut ms: Vec<f64> = durations
            .iter()
            .map(|duration| duration.as_secs_f64() * 1_000.0)
            .collect();
        ms.sort_by(|a, b| a.total_cmp(b));
        let count = ms.len();
        if count == 0 {
            return Self {
                count: 0,
                p50_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
                max_ms: 0.0,
                mean_ms: 0.0,
            };
        }
        let percentile = |q: f64| {
            let index = ((count - 1) as f64 * q).round() as usize;
            ms[index.min(count - 1)]
        };
        Self {
            count,
            p50_ms: percentile(0.50),
            p95_ms: percentile(0.95),
            p99_ms: percentile(0.99),
            max_ms: ms[count - 1],
            mean_ms: ms.iter().sum::<f64>() / count as f64,
        }
    }

    fn print(self, label: &str) {
        println!(
            "{label}: n={} p50={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms mean={:.3}ms",
            self.count, self.p50_ms, self.p95_ms, self.p99_ms, self.max_ms, self.mean_ms
        );
    }
}

#[derive(Debug, Clone)]
struct LoadResult {
    stats: Stats,
    statuses: Vec<StatusCode>,
    elapsed: Duration,
    target_rate: f64,
}

impl LoadResult {
    fn assert_all_ok(&self, label: &str) {
        let failures: Vec<StatusCode> = self
            .statuses
            .iter()
            .copied()
            .filter(|status| *status != StatusCode::OK)
            .collect();
        assert!(
            failures.is_empty(),
            "{label}: {} of {} requests failed; first failures: {:?}",
            failures.len(),
            self.statuses.len(),
            failures.iter().take(8).collect::<Vec<_>>()
        );
    }

    fn achieved_rate(&self) -> f64 {
        if self.elapsed.is_zero() {
            0.0
        } else {
            self.statuses.len() as f64 / self.elapsed.as_secs_f64()
        }
    }
}

#[derive(Debug, Clone)]
struct Perf2Result {
    forward: Stats,
    trace: Stats,
}

#[derive(Debug, Clone, Copy)]
struct SoakResult {
    open_streams: usize,
    baseline_rss: u64,
    after_pins_rss: u64,
    after_streams_rss: u64,
}

#[derive(Debug, Clone)]
struct Perf4Result {
    cold_start: Duration,
    idle_rss_before: u64,
    idle_rss_after: u64,
    events: usize,
    rollups: usize,
    routes: usize,
    pins: usize,
}

#[derive(Debug)]
struct Harness {
    _dir: tempfile::TempDir,
    app: Router,
    state: ProxyAuthState,
    token: String,
    client_key_id: ClientKeyId,
    upstream_key_id: UpstreamKeyId,
    canonical_id: String,
}

#[derive(Clone, Default)]
struct MockState {
    calls: Arc<AtomicUsize>,
}

async fn mock_chat(State(state): State<MockState>, headers: HeaderMap, body: Bytes) -> Response {
    state.calls.fetch_add(1, Ordering::Relaxed);

    let delay_ms = headers
        .get("x-mock-delay-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let is_streaming = String::from_utf8_lossy(&body).contains("\"stream\":true");
    drop(body);
    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }

    if !is_streaming {
        let payload = json!({
            "id": "bench-json",
            "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
        .to_string();
        let mut response = Response::new(Body::from(payload));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        return response;
    }

    let hold = headers.contains_key("x-mock-hold");
    let first = Bytes::from_static(
        b"data: {\"id\":\"chunk-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}\n\n",
    );
    let tail = Bytes::from_static(
        b"data: {\"id\":\"chunk-2\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"y\"}}]}\n\ndata: [DONE]\n\n",
    );

    let stream: BoxStream<'static, Result<Bytes, io::Error>> = if hold {
        Box::pin(
            futures_util::stream::iter(vec![Ok::<Bytes, io::Error>(first)])
                .chain(futures_util::stream::pending::<Result<Bytes, io::Error>>()),
        )
    } else {
        Box::pin(futures_util::stream::iter(vec![
            Ok::<Bytes, io::Error>(first),
            Ok(tail),
        ]))
    };

    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

async fn spawn_mock_upstream() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock upstream");
    let address = listener.local_addr().expect("mock upstream address");
    let router = Router::new()
        .route("/chat/completions", post(mock_chat))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(MockState::default());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    address
}

async fn build_harness(mock_address: SocketAddr, trace_enabled: bool) -> Harness {
    let dir = tempfile::tempdir().expect("temporary database directory");
    let database_path = dir.path().join("bench.db");
    let pool = open_pool(&database_path, PoolRole::Writer)
        .await
        .expect("open writer pool");
    migrate(&pool).await.expect("run migrations");

    let master_key = MasterKey::from_bytes([0x5A; 32]);
    let entropy: Arc<dyn EntropySource> = Arc::new(OsEntropy);
    load_or_initialize(&pool, INSTANCE_KEY_ID, &master_key, entropy.clone(), 1_000)
        .await
        .expect("initialize instance");
    seed_defaults(&pool, None, 1_000)
        .await
        .expect("seed settings");

    let now_ms = current_epoch_millis();
    let user_id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at)
         VALUES (?, ?, 'bench-hash', 'admin', 0, ?)",
    )
    .bind(user_id.to_string())
    .bind("bench-user")
    .bind(now_ms)
    .execute(&pool)
    .await
    .expect("insert bench user");

    let actor = ActorScope::user(user_id, Role::Admin);
    let mut tx = pool.begin().await.expect("begin setup transaction");
    let provider = create_provider_tx(
        &mut tx,
        &actor,
        NewProvider {
            name: "Bench Provider".to_owned(),
            kind: ProviderKind::Custom,
            base_url: format!("http://{mock_address}"),
            config_json: "{}".to_owned(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: true,
            enabled: true,
        },
        now_ms,
    )
    .await
    .expect("create provider");
    let upstream_key = create_upstream_key_tx(
        &mut tx,
        &actor,
        &master_key,
        INSTANCE_KEY_ID,
        entropy.clone(),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Bench Key".to_owned(),
            secret: secret_string("sk-bench-upstream"),
            priority: Some(100),
            strict_tier: None,
            enabled: Some(true),
        },
        now_ms,
    )
    .await
    .expect("create upstream key");
    let client_key = create_client_key_tx(
        &mut tx,
        &actor,
        user_id,
        NewClientKey {
            label: "Bench Client Key".to_owned(),
            routing_scope: None,
            allowed_models: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: None,
            expires_at: None,
        },
        entropy.as_ref(),
        now_ms,
    )
    .await
    .expect("create client key");

    let model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, display_name, distribution, origin, created_by_user_id)
         VALUES (?, ?, ?, 'weighted', 'global', ?)",
    )
    .bind(model_pk.to_string())
    .bind(BENCH_MODEL)
    .bind(BENCH_MODEL)
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model");
    sqlx::query(
        "INSERT INTO model_routes
            (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at)
         VALUES (?, ?, ?, '[\"chat\"]', 'active', 1, ?)",
    )
    .bind(upstream_key.id.to_string())
    .bind(model_pk.to_string())
    .bind(BENCH_MODEL)
    .bind(now_ms)
    .execute(&mut *tx)
    .await
    .expect("insert model route");
    tx.commit().await.expect("commit setup");

    let snapshot = load_initial_snapshot(&pool).await.expect("load snapshot");
    let instance_id = snapshot.instance_id;
    let snapshot = if trace_enabled {
        let mut snapshot = snapshot;
        let mut settings = (*snapshot.settings).clone();
        settings.traceability_config.enabled = true;
        settings.traceability_config.match_filters = Default::default();
        snapshot.settings = Arc::new(settings);
        snapshot
    } else {
        snapshot
    };
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let mut state = ProxyAuthState::new(shared_snapshot, pool.clone(), Vec::new())
        .with_master_key(master_key.clone());
    state.is_ready.store(true, Ordering::SeqCst);
    if trace_enabled {
        let sealer = Arc::new(XChaCha20Poly1305Sealer::new(
            master_key,
            instance_id,
            Arc::new(OsEntropy),
        ));
        let (sealer_pool, _result_rx) = SealerPool::new(sealer);
        state.sealer_pool = Some(Arc::new(sealer_pool));
        state.master_key_id = Some(INSTANCE_KEY_ID.to_owned());
    }
    let app = data_plane_router(state.clone());
    let token = client_key.raw_token.expose_secret().to_owned();

    Harness {
        _dir: dir,
        app,
        state,
        token,
        client_key_id: client_key.client_key.id,
        upstream_key_id: upstream_key.id,
        canonical_id: BENCH_MODEL.to_owned(),
    }
}

fn current_epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

fn rss_bytes() -> u64 {
    let Ok(pid) = sysinfo::get_current_pid() else {
        return 0;
    };
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).map_or(0, |process| process.memory())
}

fn body_for_size(size: usize) -> Bytes {
    let prefix = r#"{"model":"bench-model","stream":true,"messages":[{"role":"user","content":""#;
    let suffix = r#""}]}"#;
    let padding = size.saturating_sub(prefix.len() + suffix.len());
    let mut body = String::with_capacity(prefix.len() + padding + suffix.len());
    body.push_str(prefix);
    body.push_str(&"x".repeat(padding));
    body.push_str(suffix);
    Bytes::from(body)
}

fn build_request(token: &str, body: Bytes, hold: bool, delay_ms: Option<u64>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if hold {
        builder = builder.header("x-mock-hold", "1");
    }
    if let Some(delay_ms) = delay_ms {
        builder = builder.header("x-mock-delay-ms", delay_ms.to_string());
    }
    builder.body(Body::from(body)).expect("build request")
}

async fn send_one(
    app: &Router,
    token: &str,
    body: Bytes,
    hold: bool,
    delay_ms: Option<u64>,
) -> Result<(Duration, StatusCode), String> {
    let request = build_request(token, body, hold, delay_ms);
    let start = Instant::now();
    let response = app
        .clone()
        .oneshot(request)
        .await
        .map_err(|error| format!("router error: {error}"))?;
    let status = response.status();
    let elapsed = start.elapsed();
    if !hold && status == StatusCode::OK {
        // Drain the finite mock response so the upstream connection returns to the pool.
        let _ = response.into_body().collect().await;
    }
    Ok((elapsed, status))
}

async fn warm_up(harness: &Harness, requests: usize, body_size: usize) {
    let body = body_for_size(body_size);
    for _ in 0..requests {
        let result = send_one(&harness.app, &harness.token, body.clone(), false, None).await;
        assert!(
            result.is_ok(),
            "benchmark warm-up request failed: {result:?}"
        );
        assert_eq!(
            result.expect("warm-up result").1,
            StatusCode::OK,
            "benchmark warm-up returned a non-200 status"
        );
    }
}

async fn drain_trace_sealer(harness: &Harness) {
    let Some(pool) = harness.state.sealer_pool.as_ref() else {
        return;
    };
    while pool.in_flight() > 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn run_load(
    harness: &Harness,
    body: Bytes,
    requests: usize,
    target_rate: f64,
    delay_for: impl Fn(usize) -> Option<u64> + Send + Sync + 'static,
) -> LoadResult {
    const SCHEDULER_TICK: Duration = Duration::from_millis(20);
    let mut ticker = tokio::time::interval(SCHEDULER_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tasks = JoinSet::new();
    let started = Instant::now();
    let mut spawned = 0_usize;
    let mut owed = 0.0_f64;

    while spawned < requests {
        ticker.tick().await;
        owed += target_rate * SCHEDULER_TICK.as_secs_f64();
        let batch = owed.floor() as usize;
        owed -= batch as f64;
        for _ in 0..batch.min(requests - spawned) {
            let app = harness.app.clone();
            let token = harness.token.clone();
            let body = body.clone();
            let delay = delay_for(spawned);
            tasks.spawn(async move { send_one(&app, &token, body, false, delay).await });
            spawned += 1;
        }
    }

    let mut durations = Vec::with_capacity(requests);
    let mut statuses = Vec::with_capacity(requests);
    while let Some(joined) = tasks.join_next().await {
        let (elapsed, status) = joined
            .expect("load task panicked")
            .unwrap_or_else(|error| panic!("load request failed: {error}"));
        durations.push(elapsed);
        statuses.push(status);
    }

    LoadResult {
        stats: Stats::from_durations(&durations),
        statuses,
        elapsed: started.elapsed(),
        target_rate,
    }
}

async fn run_direct_load(
    address: SocketAddr,
    body: Bytes,
    requests: usize,
    target_rate: f64,
) -> LoadResult {
    const SCHEDULER_TICK: Duration = Duration::from_millis(20);
    let client = reqwest::Client::new();
    let url = format!("http://{address}/chat/completions");
    let mut ticker = tokio::time::interval(SCHEDULER_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tasks = JoinSet::new();
    let started = Instant::now();
    let mut spawned = 0_usize;
    let mut owed = 0.0_f64;

    while spawned < requests {
        ticker.tick().await;
        owed += target_rate * SCHEDULER_TICK.as_secs_f64();
        let batch = owed.floor() as usize;
        owed -= batch as f64;
        for _ in 0..batch.min(requests - spawned) {
            let client = client.clone();
            let url = url.clone();
            let body = body.clone();
            tasks.spawn(async move {
                let start = Instant::now();
                let response = client
                    .post(&url)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(body)
                    .send()
                    .await
                    .map_err(|error| format!("direct mock request failed: {error}"))?;
                let status = response.status();
                let elapsed = start.elapsed();
                let _ = response.bytes().await;
                Ok::<_, String>((elapsed, status))
            });
            spawned += 1;
        }
    }

    let mut durations = Vec::with_capacity(requests);
    let mut statuses = Vec::with_capacity(requests);
    while let Some(joined) = tasks.join_next().await {
        let (elapsed, status) = joined
            .expect("direct load task panicked")
            .unwrap_or_else(|error| panic!("direct load request failed: {error}"));
        durations.push(elapsed);
        statuses.push(status);
    }
    LoadResult {
        stats: Stats::from_durations(&durations),
        statuses,
        elapsed: started.elapsed(),
        target_rate,
    }
}

fn deterministic_lognormal_delay_ms(state: &mut u64) -> u64 {
    // Log-normal with median 2,000 ms and p95 8,000 ms:
    // sigma = ln(4) / 1.645 ~= 0.8427, mu = ln(2000).
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let u1 = (((*state >> 11) as f64) + 1.0) / ((1_u64 << 53) as f64 + 2.0);
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let u2 = (((*state >> 11) as f64) + 1.0) / ((1_u64 << 53) as f64 + 2.0);
    let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
    let delay = 2_000.0 * (0.842_7 * z).exp();
    delay.clamp(50.0, 30_000.0) as u64
}

#[doc = "Spec: PERF-1"]
async fn bench_perf1(harness: &Harness, scale: &Scale, mock_address: SocketAddr) -> LoadResult {
    warm_up(harness, 20, 64 * 1024).await;
    let body_64k = body_for_size(64 * 1024);
    let direct_64k = run_direct_load(mock_address, body_64k.clone(), 20, 100.0).await;
    direct_64k.assert_all_ok("PERF-1 direct baseline warm-up");

    let rounds = if scale.full { 5 } else { 3 };
    let mut added_p50s = Vec::with_capacity(rounds);
    let mut added_p99s = Vec::with_capacity(rounds);
    let mut last_proxy: Option<LoadResult> = None;

    for round in 0..rounds {
        let direct_64k = run_direct_load(
            mock_address,
            body_64k.clone(),
            scale.perf1_64k_requests,
            500.0,
        )
        .await;
        direct_64k.assert_all_ok("PERF-1 direct baseline 64 KiB");
        let zero_64k = run_load(
            harness,
            body_64k.clone(),
            scale.perf1_64k_requests,
            500.0,
            |_| None,
        )
        .await;
        zero_64k.assert_all_ok("PERF-1 zero-delay 64 KiB");
        let added_p50 = (zero_64k.stats.p50_ms - direct_64k.stats.p50_ms).max(0.0);
        let added_p99 = (zero_64k.stats.p99_ms - direct_64k.stats.p99_ms).max(0.0);
        println!(
            "PERF-1 round {}: direct p50 {:.3}ms p99 {:.3}ms, proxy p50 {:.3}ms p99 {:.3}ms, added p50 {:.3}ms p99 {:.3}ms",
            round + 1,
            direct_64k.stats.p50_ms,
            direct_64k.stats.p99_ms,
            zero_64k.stats.p50_ms,
            zero_64k.stats.p99_ms,
            added_p50,
            added_p99
        );
        added_p50s.push(added_p50);
        added_p99s.push(added_p99);
        last_proxy = Some(zero_64k);
    }

    added_p50s.sort_by(f64::total_cmp);
    added_p99s.sort_by(f64::total_cmp);
    let median_p50 = added_p50s[added_p50s.len() / 2];
    let median_p99 = added_p99s[added_p99s.len() / 2];
    println!(
        "PERF-1 median added proxy latency 64 KiB: p50={median_p50:.3}ms p99={median_p99:.3}ms"
    );
    assert!(
        median_p50 <= 1.0,
        "PERF-1 added p50 {median_p50:.3}ms exceeds 1ms for bodies <= 256 KiB"
    );
    assert!(
        median_p99 <= 5.0,
        "PERF-1 added p99 {median_p99:.3}ms exceeds 5ms for bodies <= 256 KiB"
    );

    let zero_512k = run_load(
        harness,
        body_for_size(512 * 1024),
        scale.perf1_512k_requests,
        100.0,
        |_| None,
    )
    .await;
    zero_512k.assert_all_ok("PERF-1 zero-delay 512 KiB");
    zero_512k
        .stats
        .print("PERF-1 zero-delay 512 KiB (recorded)");

    let zero_2m = run_load(
        harness,
        body_for_size(2 * 1024 * 1024),
        scale.perf1_2m_requests,
        50.0,
        |_| None,
    )
    .await;
    zero_2m.assert_all_ok("PERF-1 zero-delay 2 MiB");
    zero_2m.stats.print("PERF-1 zero-delay 2 MiB (recorded)");

    last_proxy.expect("PERF-1 executed at least one round")
}

async fn bench_perf1_log_normal(harness: &Harness, scale: &Scale) -> LoadResult {
    assert!(
        scale.full,
        "the log-normal TTFF reference run is part of --full"
    );
    let mut seed = 0x5EED_1234_ABCD_9876_u64;
    let mut delays = Vec::with_capacity(819);
    for _ in 0..819 {
        delays.push(deterministic_lognormal_delay_ms(&mut seed));
    }
    let result = run_load(
        harness,
        body_for_size(1024 * 1024),
        819,
        409.5,
        move |index| Some(delays[index % delays.len()]),
    )
    .await;
    result.assert_all_ok("PERF-1 log-normal TTFF 1 MiB");
    result.stats.print("PERF-1 log-normal TTFF 1 MiB");
    println!(
        "PERF-1 log-normal achieved {:.1} req/s (budget target {:.1})",
        result.achieved_rate(),
        result.target_rate
    );
    result
}

fn sse_frame(index: usize) -> Vec<u8> {
    format!(
        "data: {{\"id\":\"bench-{index}\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"token-{index}\"}}}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}\n\n"
    )
    .into_bytes()
}

fn bench_perf2(frames: usize) -> Perf2Result {
    let mut forwarder = SseChunkForwarder::new(true);
    let mut tap = SseUsageTap::new(EndpointKind::Chat);
    let mut forward_durations = Vec::with_capacity(frames);
    let mut forwarded_bytes = 0_usize;

    for index in 0..frames {
        let frame = sse_frame(index);
        let start = Instant::now();
        for chunk in frame.chunks(7) {
            for output in forwarder.process_chunk(chunk) {
                forwarded_bytes = forwarded_bytes.saturating_add(output.len());
            }
            tap.feed_chunk(chunk);
            let _ = extract_frame_error(chunk);
        }
        forward_durations.push(start.elapsed());
    }
    let forward = Stats::from_durations(&forward_durations);
    forward.print("PERF-2 SSE forwarder");
    assert!(
        forwarded_bytes >= frames,
        "PERF-2 forwarder did not emit every frame"
    );
    assert!(
        tap.observed_usage.is_some(),
        "PERF-2 usage tap did not observe an injected usage frame"
    );
    assert!(
        forward.p99_ms <= 1.0,
        "PERF-2 p99 {:.3}ms exceeds 1ms per complete SSE frame",
        forward.p99_ms
    );

    let (mut trace, _receiver) = TraceTap::new();
    let mut trace_durations = Vec::with_capacity(frames);
    for index in 0..frames {
        let frame = sse_frame(index);
        let start = Instant::now();
        trace.feed_sse_chunk(&frame);
        let _ = trace.try_feed_chunk(Bytes::copy_from_slice(&frame));
        trace_durations.push(start.elapsed());
    }
    let trace_stats = Stats::from_durations(&trace_durations);
    trace_stats.print("PERF-2 SSE with stalled TraceTap");
    assert!(
        trace_stats.p99_ms <= 1.0,
        "PERF-2 trace-active p99 {:.3}ms exceeds 1ms per complete SSE frame",
        trace_stats.p99_ms
    );
    let delta_ms = trace_stats.p99_ms - forward.p99_ms;
    println!("PERF-2 stalled-trace added p99: {delta_ms:.3}ms");
    assert!(
        delta_ms <= 0.5,
        "PERF-2 stalled TraceTap added {delta_ms:.3}ms at p99, exceeding 0.5ms"
    );

    Perf2Result {
        forward,
        trace: trace_stats,
    }
}

async fn open_held_stream(app: Router, token: String, body: Bytes) -> Result<Body, String> {
    let request = build_request(&token, body, true, None);
    let response = app
        .oneshot(request)
        .await
        .map_err(|error| format!("router error: {error}"))?;
    if response.status() != StatusCode::OK {
        return Err(format!("stream request returned {}", response.status()));
    }
    let mut body = response.into_body();
    match body.frame().await {
        Some(Ok(_)) => Ok(body),
        Some(Err(error)) => Err(format!("first stream frame failed: {error}")),
        None => Err("stream ended before the first frame".to_owned()),
    }
}

async fn bench_perf3(harness: &Harness, scale: &Scale, rss_limit: u64, label: &str) -> SoakResult {
    let baseline_rss = rss_bytes();
    let stream_body_bytes = PERF3_STREAM_BODY_BYTES;
    warm_up(harness, 10, 64 * 1024).await;
    let pins = PinStore::new(scale.perf3_pins);
    let now_ms = current_epoch_millis();
    for index in 0..scale.perf3_pins {
        pins.insert_hydrated(
            PinKey {
                client_key_id: harness.client_key_id,
                canonical_id: harness.canonical_id.clone(),
                session_hash: format!("bench-session-{index}"),
            },
            PinEntry {
                state: PinState::Confirmed,
                upstream_key_id: harness.upstream_key_id,
                generation: 1,
                pool_tier: CandidatePoolTier::Global,
                created_ms: now_ms,
                last_used_ms: now_ms,
                expires_ms: now_ms + 86_400_000,
                repin_count: 0,
                tried_this_generation: Vec::new(),
            },
        );
    }
    assert_eq!(
        pins.len(),
        scale.perf3_pins,
        "PERF-3 pin store rejected hydrated pins"
    );
    let after_pins_rss = rss_bytes();
    println!(
        "{label} RSS baseline {:.1} MiB, after {} pins {:.1} MiB (+{:.1} MiB)",
        baseline_rss as f64 / MIB as f64,
        scale.perf3_pins,
        after_pins_rss as f64 / MIB as f64,
        (after_pins_rss.saturating_sub(baseline_rss)) as f64 / MIB as f64
    );

    // Hold real 1 MiB pre-first-byte request bodies against a slow mock upstream.
    let mut slow_tasks = JoinSet::new();
    let slow_delay_ms = scale.slow_delay_ms;
    let slow_body = body_for_size(1024 * 1024);
    for _ in 0..scale.pre_first {
        let app = harness.app.clone();
        let token = harness.token.clone();
        let body = slow_body.clone();
        slow_tasks
            .spawn(async move { send_one(&app, &token, body, false, Some(slow_delay_ms)).await });
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    let after_pre_first_rss = rss_bytes();
    println!(
        "{label} RSS after {} pre-first 1 MiB requests {:.1} MiB (+{:.1} MiB from pins)",
        scale.pre_first,
        after_pre_first_rss as f64 / MIB as f64,
        (after_pre_first_rss.saturating_sub(after_pins_rss)) as f64 / MIB as f64
    );

    let mut stream_tasks = JoinSet::new();
    for _ in 0..scale.streams {
        let app = harness.app.clone();
        let token = harness.token.clone();
        let body = body_for_size(stream_body_bytes);
        stream_tasks.spawn(async move { open_held_stream(app, token, body).await });
    }
    let mut bodies = Vec::with_capacity(scale.streams);
    while let Some(joined) = stream_tasks.join_next().await {
        bodies.push(
            joined
                .expect("stream task panicked")
                .unwrap_or_else(|error| panic!("open stream failed: {error}")),
        );
    }

    let mut waited = 0_u32;
    while harness.state.open_streams.load(Ordering::SeqCst) < scale.streams && waited < 500 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += 1;
    }
    let open_streams = harness.state.open_streams.load(Ordering::SeqCst);
    let after_streams_rss = rss_bytes();
    println!("{label} open stream request body: {stream_body_bytes} bytes");
    println!(
        "{label} RSS after {} live streams {:.1} MiB (+{:.1} MiB from baseline), limit {:.1} MiB",
        open_streams,
        after_streams_rss as f64 / MIB as f64,
        (after_streams_rss.saturating_sub(baseline_rss)) as f64 / MIB as f64,
        rss_limit as f64 / MIB as f64
    );

    assert_eq!(
        open_streams, scale.streams,
        "{label} did not retain every live stream"
    );
    assert!(
        after_streams_rss <= rss_limit,
        "{label} RSS {:.1} MiB exceeds {:.1} MiB",
        after_streams_rss as f64 / MIB as f64,
        rss_limit as f64 / MIB as f64
    );

    drop(bodies);
    drop(pins);
    slow_tasks.abort_all();
    drop(slow_tasks);
    tokio::time::sleep(Duration::from_millis(100)).await;

    SoakResult {
        open_streams,
        baseline_rss,
        after_pins_rss,
        after_streams_rss,
    }
}

async fn insert_usage_events(
    pool: &SqlitePool,
    events: usize,
    now_ms: i64,
    client_key_id: &str,
    user_id: &str,
) {
    const CHUNK: usize = 100_000;
    let report_every = if events >= 1_000_000 {
        (events / 10).max(1)
    } else {
        usize::MAX
    };
    let mut next_report = report_every;
    let mut start = 1_usize;
    while start <= events {
        let end = (start + CHUNK - 1).min(events);
        let count = end - start + 1;
        sqlx::query(
            "WITH RECURSIVE seq(n) AS (
                 SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < ?
             )
             INSERT INTO usage_events
                 (id, ts, request_id, client_key_id, owner_user_id, endpoint, status, stream,
                  cost_micro_usd, cost_source)
             SELECT 'ev-' || ? || '-' || n, ? - n, 'req-' || ? || '-' || n,
                    ?, ?, 'chat', 200, 0, 1, 'price_book'
             FROM seq",
        )
        .bind(count as i64)
        .bind(start as i64)
        .bind(now_ms)
        .bind(start as i64)
        .bind(client_key_id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("seed usage_events");
        if start >= next_report {
            println!("PERF-4 seeded {end} / {events} usage_events");
            next_report = next_report.saturating_add(report_every);
        }
        start = end + 1;
    }
}

async fn seed_cold_start_database(
    pool: &SqlitePool,
    now_ms: i64,
    scale: &Scale,
) -> (usize, usize, usize, Vec<ActivePinRow>) {
    let master_key = MasterKey::from_bytes([0x7C; 32]);
    let entropy: Arc<dyn EntropySource> = Arc::new(OsEntropy);
    load_or_initialize(pool, INSTANCE_KEY_ID, &master_key, entropy, now_ms)
        .await
        .expect("initialize cold-start instance");
    seed_defaults(pool, None, now_ms)
        .await
        .expect("seed cold-start settings");

    let _ = sqlx::query("PRAGMA synchronous = OFF").execute(pool).await;

    let user_id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at)
         VALUES (?, 'cold-user', 'bench-hash', 'admin', 0, ?)",
    )
    .bind(user_id.to_string())
    .bind(now_ms)
    .execute(pool)
    .await
    .expect("insert cold-start user");

    let provider_id = llmroller_core::ids::ProviderId::generate();
    sqlx::query(
        "INSERT INTO providers
            (id, ownership_scope, owner_user_id, kind, name, base_url, config_json,
             usage_url_cross_origin_ack, allow_private_network, enabled, created_at)
         VALUES (?, 'global', NULL, 'custom', 'Cold Provider', 'http://127.0.0.1:1', '{}', 0, 1, 1, ?)",
    )
    .bind(provider_id.to_string())
    .bind(now_ms)
    .execute(pool)
    .await
    .expect("insert cold-start provider");

    let client_key_id = ClientKeyId::generate();
    // The cold-start path only needs a syntactically valid hash; no request uses this key.
    let key_hash = "0123456789abcdef".repeat(4);
    sqlx::query(
        "INSERT INTO client_keys
            (id, owner_user_id, label, prefix, key_hash, routing_scope, created_at)
         VALUES (?, ?, 'Cold Client', 'llr_', ?, 'inherit', ?)",
    )
    .bind(client_key_id.to_string())
    .bind(user_id.to_string())
    .bind(key_hash)
    .bind(now_ms)
    .execute(pool)
    .await
    .expect("insert cold-start client key");

    let mut key_ids = Vec::with_capacity(scale.keys);
    let mut tx = pool.begin().await.expect("begin key seed");
    for index in 0..scale.keys {
        let key_id = UpstreamKeyId::generate();
        key_ids.push(key_id);
        sqlx::query(
            "INSERT INTO upstream_keys
                (id, provider_id, owner_user_id, label, secret_enc, master_key_id, secret_last4,
                 enabled, priority, strict_tier, health, created_at)
             VALUES (?, ?, NULL, ?, ?, ?, '0000', 1, 100, 1, 'healthy', ?)",
        )
        .bind(key_id.to_string())
        .bind(provider_id.to_string())
        .bind(format!("Cold Key {index}"))
        .bind(
            secret_bytes(b"bench-encrypted".to_vec())
                .expose_secret()
                .to_vec(),
        )
        .bind(INSTANCE_KEY_ID)
        .bind(now_ms)
        .execute(&mut *tx)
        .await
        .expect("insert cold-start upstream key");
    }
    tx.commit().await.expect("commit key seed");

    let mut model_pks = Vec::with_capacity(scale.models);
    let mut tx = pool.begin().await.expect("begin model seed");
    for index in 0..scale.models {
        let model_pk = ModelPk::generate();
        model_pks.push(model_pk);
        sqlx::query(
            "INSERT INTO models
                (model_pk, canonical_id, display_name, distribution, origin, created_by_user_id)
             VALUES (?, ?, ?, 'weighted', 'global', ?)",
        )
        .bind(model_pk.to_string())
        .bind(format!("bench-cold-model-{index}"))
        .bind(format!("Bench Cold Model {index}"))
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert cold-start model");
    }
    tx.commit().await.expect("commit model seed");

    let mut tx = pool.begin().await.expect("begin route seed");
    for key_id in &key_ids {
        for (index, model_pk) in model_pks.iter().enumerate() {
            sqlx::query(
                "INSERT INTO model_routes
                    (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at)
                 VALUES (?, ?, ?, '[\"chat\"]', 'active', 1, ?)",
            )
            .bind(key_id.to_string())
            .bind(model_pk.to_string())
            .bind(format!("upstream-cold-model-{index}"))
            .bind(now_ms)
            .execute(&mut *tx)
            .await
            .expect("insert cold-start route");
        }
    }
    tx.commit().await.expect("commit route seed");
    let routes = key_ids.len() * model_pks.len();

    // Pins exercise the RTE-10 preload path without dominating the idle-memory measurement.
    let pin_count = scale.perf4_pins;
    for chunk_start in (0..pin_count).step_by(10_000) {
        let chunk_len = (pin_count - chunk_start).min(10_000);
        sqlx::query(
            "WITH RECURSIVE seq(n) AS (
                 SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < ?
             )
             INSERT INTO pins
                 (client_key_id, session_hash, model_pk, upstream_key_id, pool_tier,
                  created_at, last_used_at, expires_at, repin_count)
             SELECT ?, 'cold-pin-' || (? + n), ?, ?, 2, ?, ?, ?, 0
             FROM seq",
        )
        .bind(chunk_len as i64)
        .bind(client_key_id.to_string())
        .bind(chunk_start as i64)
        .bind(model_pks[0].to_string())
        .bind(key_ids[0].to_string())
        .bind(now_ms)
        .bind(now_ms)
        .bind(now_ms + 86_400_000)
        .execute(pool)
        .await
        .expect("seed cold-start pins");
    }

    let rollup_start = (now_ms - 7 * 86_400_000) / 300_000 * 300_000;
    sqlx::query("CREATE TEMP TABLE bench_rollup_keys (id TEXT PRIMARY KEY)")
        .execute(pool)
        .await
        .expect("create cold-start rollup key set");
    for key_id in &key_ids {
        sqlx::query("INSERT INTO bench_rollup_keys (id) VALUES (?)")
            .bind(key_id.to_string())
            .execute(pool)
            .await
            .expect("seed cold-start rollup key");
    }
    // Insert bucket-major, matching how production rollups accumulate over
    // time. Per-key insertion fragments the composite primary key and turns a
    // cold sequential scan into random page reads.
    for bucket_index in 0..scale.rollups_per_key {
        let bucket_ms = rollup_start + bucket_index as i64 * 300_000;
        sqlx::query(
            "INSERT INTO usage_rollups_5min
                 (bucket_ms, upstream_key_id, canonical_id, client_key_id, owner_user_id,
                  provider_scope, share_id, cost_micro_usd, requests)
             SELECT ?, id, 'bench-cold-model-0', ?, ?, 'global', '', 1, 1
             FROM bench_rollup_keys",
        )
        .bind(bucket_ms)
        .bind(client_key_id.to_string())
        .bind(user_id.to_string())
        .execute(pool)
        .await
        .expect("seed cold-start rollups");
    }
    let rollups = key_ids.len() * scale.rollups_per_key;

    // Cold start never reads usage_events. Dropping its secondary indexes keeps
    // the 50M-row fixture tractable and makes an accidental scan stricter.
    let _ = sqlx::query("PRAGMA foreign_keys = OFF").execute(pool).await;
    if let Ok(mode) = sqlx::query_scalar::<_, String>("PRAGMA journal_mode = OFF")
        .fetch_one(pool)
        .await
    {
        println!("PERF-4 seed journal mode: {mode}");
    }
    for index in [
        "idx_usage_ts",
        "idx_usage_owner_ts",
        "idx_usage_client_key_ts",
        "idx_usage_client_key_session_ts",
    ] {
        let statement = match index {
            "idx_usage_ts" => "DROP INDEX IF EXISTS idx_usage_ts",
            "idx_usage_owner_ts" => "DROP INDEX IF EXISTS idx_usage_owner_ts",
            "idx_usage_client_key_ts" => "DROP INDEX IF EXISTS idx_usage_client_key_ts",
            _ => "DROP INDEX IF EXISTS idx_usage_client_key_session_ts",
        };
        sqlx::query(statement)
            .execute(pool)
            .await
            .expect("drop usage_events seed index");
    }

    insert_usage_events(
        pool,
        scale.events,
        now_ms,
        &client_key_id.to_string(),
        &user_id.to_string(),
    )
    .await;
    let _ = sqlx::query("PRAGMA journal_mode = WAL").execute(pool).await;
    let _ = sqlx::query("PRAGMA synchronous = NORMAL")
        .execute(pool)
        .await;
    let _ = sqlx::query("PRAGMA foreign_keys = ON").execute(pool).await;
    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await;

    let pins = load_active_pins(pool, now_ms)
        .await
        .expect("load cold-start pins");
    (scale.events, rollups, routes, pins)
}

async fn bench_perf4(scale: &Scale) -> Perf4Result {
    // Set LLMROLLER_BENCH_PERF4_DB to keep the seeded 50M-row database between
    // diagnostic runs. The default remains an isolated temporary database.
    let temp_dir = if std::env::var_os("LLMROLLER_BENCH_PERF4_DB").is_some() {
        None
    } else {
        Some(tempfile::tempdir().expect("cold-start temporary database"))
    };
    let database_path = temp_dir.as_ref().map_or_else(
        || PathBuf::from(std::env::var_os("LLMROLLER_BENCH_PERF4_DB").expect("db path")),
        |dir| dir.path().join("cold.db"),
    );
    let reuse = temp_dir.is_none() && database_path.exists();
    let now_ms = current_epoch_millis();
    let pool = open_pool(&database_path, PoolRole::Writer)
        .await
        .expect("open cold-start pool");

    let (events, rollups, routes, pins) = if reuse {
        println!("PERF-4 reusing seeded database {}", database_path.display());
        let pins = load_active_pins(&pool, now_ms)
            .await
            .expect("load cold-start pins");
        (
            scale.events,
            scale.keys * scale.rollups_per_key,
            scale.keys * scale.models,
            pins,
        )
    } else {
        migrate(&pool).await.expect("run cold-start migrations");
        println!(
            "PERF-4 seeding {} events, {} rollups, {} routes and {} pins...",
            scale.events,
            scale.keys * scale.rollups_per_key,
            scale.keys * scale.models,
            scale.perf4_pins
        );
        let seed_started = Instant::now();
        let seeded = seed_cold_start_database(&pool, now_ms, scale).await;
        println!(
            "PERF-4 seed completed in {:.2}s",
            seed_started.elapsed().as_secs_f64()
        );
        seeded
    };

    // Measure readiness as a real restart would: close the writer and reopen a
    // fresh pool so the timed path does not inherit the seeder's page cache,
    // WAL frame state, or connection-local PRAGMAs.
    pool.close().await;
    let pool = open_pool(&database_path, PoolRole::Writer)
        .await
        .expect("reopen cold-start pool");

    let idle_rss_before = rss_bytes();
    let started = Instant::now();
    let snapshot_started = Instant::now();
    let snapshot = load_initial_snapshot(&pool)
        .await
        .expect("load cold-start snapshot");
    let snapshot_elapsed = snapshot_started.elapsed();
    let rebuild_started = Instant::now();
    let rebuilt = reconstruct_budgets_from_rollups(&pool, now_ms, 30 * 86_400_000)
        .await
        .expect("rebuild cold-start budgets");
    let rebuild_elapsed = rebuild_started.elapsed();
    let runtime_started = Instant::now();
    let runtime_state = RuntimeState::new();
    runtime_state
        .accumulators
        .merge_from(&rebuilt.monthly_accumulators);
    runtime_state.rebuild_window_counters(&snapshot, now_ms);
    let runtime_elapsed = runtime_started.elapsed();
    let cold_start = started.elapsed();
    let idle_rss_after = rss_bytes();
    let snapshot_routes = snapshot
        .routes_by_model
        .values()
        .map(Vec::len)
        .sum::<usize>();
    let snapshot_rings = rebuilt.key_rings.len();
    drop(rebuilt);
    let pin_store = PinStore::new(scale.perf4_pins.max(1));
    for pin in &pins {
        let tier = match pin.pool_tier {
            0 => CandidatePoolTier::Own,
            1 => CandidatePoolTier::Shared,
            2 => CandidatePoolTier::Global,
            _ => CandidatePoolTier::Blended,
        };
        pin_store.insert_hydrated(
            PinKey {
                client_key_id: pin.client_key_id,
                canonical_id: pin.canonical_id.clone(),
                session_hash: pin.session_hash.clone(),
            },
            PinEntry {
                state: PinState::Confirmed,
                upstream_key_id: pin.upstream_key_id,
                generation: 1,
                pool_tier: tier,
                created_ms: pin.created_at,
                last_used_ms: pin.last_used_at,
                expires_ms: pin.expires_at,
                repin_count: u32::try_from(pin.repin_count).unwrap_or(0),
                tried_this_generation: Vec::new(),
            },
        );
    }
    drop(pins);

    println!(
        "PERF-4 cold start {:.3}s (limit 2.000s), snapshot {:.3}s, rebuild {:.3}s, runtime {:.3}s, snapshot routes {snapshot_routes}, key rings {snapshot_rings}, budget windows {}",
        cold_start.as_secs_f64(),
        snapshot_elapsed.as_secs_f64(),
        rebuild_elapsed.as_secs_f64(),
        runtime_elapsed.as_secs_f64(),
        snapshot.budget_windows.len()
    );
    println!(
        "PERF-4 idle RSS {:.1} MiB before, {:.1} MiB after (limit {:.1} MiB)",
        idle_rss_before as f64 / MIB as f64,
        idle_rss_after as f64 / MIB as f64,
        PERF4_IDLE_RSS_LIMIT as f64 / MIB as f64
    );
    assert!(
        cold_start <= PERF4_COLD_START_LIMIT,
        "PERF-4 cold start {:.3}s exceeds 2s",
        cold_start.as_secs_f64()
    );
    assert!(
        idle_rss_after <= PERF4_IDLE_RSS_LIMIT,
        "PERF-4 idle RSS {:.1} MiB exceeds 50 MiB",
        idle_rss_after as f64 / MIB as f64
    );

    Perf4Result {
        cold_start,
        idle_rss_before,
        idle_rss_after,
        events,
        rollups,
        routes,
        pins: pin_store.len(),
    }
}

fn set_trace_enabled(harness: &Harness, enabled: bool) {
    let mut snapshot = (*harness.state.snapshot.load_full()).clone();
    let mut settings = (*snapshot.settings).clone();
    settings.traceability_config.enabled = enabled;
    snapshot.settings = Arc::new(settings);
    harness.state.snapshot.store(Arc::new(snapshot));
}

async fn bench_perf5(scale: &Scale, mock_address: SocketAddr) -> (Stats, Stats) {
    let harness = build_harness(mock_address, true).await;
    let trace_body_bytes = PERF5_BODY_BYTES;
    let body = body_for_size(trace_body_bytes);
    let prep_started = Instant::now();
    for _ in 0..1_000 {
        let mut value: serde_json::Value =
            serde_json::from_slice(&body).expect("trace preparation benchmark body");
        replace_binary_blocks(&mut value);
        let _ = prepare_request_blocks(&value);
    }
    println!(
        "PERF-5 trace request preparation: {:.3}ms average over 1000 iterations for a {} byte body",
        prep_started.elapsed().as_secs_f64() * 1_000.0 / 1_000.0,
        trace_body_bytes
    );
    set_trace_enabled(&harness, true);
    warm_up(&harness, 10, trace_body_bytes).await;
    drain_trace_sealer(&harness).await;

    let rounds = if scale.full { 5 } else { 3 };
    let mut observations: Vec<(Stats, Stats, f64)> = Vec::with_capacity(rounds);

    for round in 0..rounds {
        set_trace_enabled(&harness, false);
        let plain_result = run_load(&harness, body.clone(), scale.perf5_requests, 500.0, |_| {
            None
        })
        .await;
        set_trace_enabled(&harness, true);
        let trace_result = run_load(&harness, body.clone(), scale.perf5_requests, 500.0, |_| {
            None
        })
        .await;

        plain_result.assert_all_ok("PERF-5 plain TTFB");
        trace_result.assert_all_ok("PERF-5 trace-enabled TTFB");
        let added_p99 = (trace_result.stats.p99_ms - plain_result.stats.p99_ms).max(0.0);
        println!(
            "PERF-5 round {}: plain p99 {:.3}ms, trace p99 {:.3}ms, added p99 {:.3}ms",
            round + 1,
            plain_result.stats.p99_ms,
            trace_result.stats.p99_ms,
            added_p99
        );
        observations.push((plain_result.stats, trace_result.stats, added_p99));
        drain_trace_sealer(&harness).await;
    }

    // TTFB p99 is noisy across independent load runs. The requirement is the
    // trace-mode overhead, so use the median added p99 across repeated runs on
    // the same harness instead of a single pair skewed by unrelated scheduling
    // noise.
    observations.sort_by(|left, right| left.2.total_cmp(&right.2));
    let (plain_stats, trace_stats, median_added_p99) = observations[observations.len() / 2];
    println!("PERF-5 trace-enabled median added p99 TTFB: {median_added_p99:.3}ms");
    assert!(
        median_added_p99 <= 0.5,
        "PERF-5 trace-enabled median p99 TTFB overhead {median_added_p99:.3}ms exceeds 0.5ms"
    );

    // The trace heap budget is additive to PERF-3: 300+64 MiB plus
    // min(256 MiB, 10% of 2 GiB) plus the 64 MiB body allowance.
    set_trace_enabled(&harness, true);
    let trace_limit = PERF3_RSS_LIMIT + PERF5_TRACE_BUDGET;
    let _trace_soak = bench_perf3(&harness, scale, trace_limit, "PERF-5 trace soak").await;

    (plain_stats, trace_stats)
}

async fn run(full: bool, only: Option<String>) {
    let scale = if full { Scale::full() } else { Scale::quick() };
    println!(
        "LLMRoller PERF benchmark scale: {} ({} events, {} streams, {} pins)",
        if full { "full" } else { "quick" },
        scale.events,
        scale.streams,
        scale.perf3_pins
    );
    let mock_address = spawn_mock_upstream().await;

    let selected = |name: &str| only.as_deref().is_none_or(|value| value == name);

    if selected("perf4") {
        let perf4 = bench_perf4(&scale).await;
        println!(
            "PERF-4 summary: {} events, {} rollups, {} routes, {} pins; cold start {:.3}s; idle RSS {:.1} MiB -> {:.1} MiB",
            perf4.events,
            perf4.rollups,
            perf4.routes,
            perf4.pins,
            perf4.cold_start.as_secs_f64(),
            perf4.idle_rss_before as f64 / MIB as f64,
            perf4.idle_rss_after as f64 / MIB as f64
        );
    }

    if selected("perf2") {
        let perf2 = bench_perf2(scale.sse_frames);
        println!(
            "PERF-2 summary: forward p99 {:.3}ms, stalled-trace p99 {:.3}ms",
            perf2.forward.p99_ms, perf2.trace.p99_ms
        );
    }

    let mut harness: Option<Harness> = None;
    if selected("perf1") || selected("perf3") || selected("perf5") {
        harness = Some(build_harness(mock_address, false).await);
    }

    if selected("perf1") {
        let harness = harness.as_ref().expect("harness");
        let _perf1 = bench_perf1(harness, &scale, mock_address).await;
        if scale.full {
            let _log_normal = bench_perf1_log_normal(harness, &scale).await;
        } else {
            println!(
                "PERF-1 log-normal TTFF reference run skipped in quick mode; use --full to run 819 x 1 MiB at 409.5 req/s"
            );
        }
    }

    if selected("perf3") {
        let harness = harness.as_ref().expect("harness");
        let perf3 = bench_perf3(harness, &scale, PERF3_RSS_LIMIT, "PERF-3 soak").await;
        println!(
            "PERF-3 summary: {} live streams, baseline {:.1} MiB, pins {:.1} MiB, streams {:.1} MiB",
            perf3.open_streams,
            perf3.baseline_rss as f64 / MIB as f64,
            perf3.after_pins_rss as f64 / MIB as f64,
            perf3.after_streams_rss as f64 / MIB as f64
        );
    }

    if selected("perf5") {
        let (plain, trace) = bench_perf5(&scale, mock_address).await;
        println!(
            "PERF-5 summary: plain p99 {:.3}ms, trace p99 {:.3}ms",
            plain.p99_ms, trace.p99_ms
        );
    }

    println!("LLMRoller PERF benchmark completed successfully");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let full = args.iter().any(|argument| argument == "--full");
    let only = args
        .iter()
        .find_map(|argument| argument.strip_prefix("--only="))
        .map(ToOwned::to_owned);

    if cfg!(debug_assertions) && !args.iter().any(|argument| argument == "--allow-debug") {
        println!(
            "LLMRoller PERF benchmark compiled with debug assertions; skipping load run. \
             Use `cargo bench -p llmroller-server --bench proxy_latency` for the release-like bench profile, \
             or pass `--allow-debug` to force this executable."
        );
        return;
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build benchmark runtime");
    runtime.block_on(run(full, only));
}
