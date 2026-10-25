#![forbid(unsafe_code)]
#![doc = "Spec: PERF-1, PERF-2, PERF-3, PERF-4, PERF-5"]
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


#[path = "proxy_latency/types.rs"]
pub(crate) mod types;
#[path = "proxy_latency/mock.rs"]
pub(crate) mod mock;
#[path = "proxy_latency/harness.rs"]
pub(crate) mod harness;
#[path = "proxy_latency/benchmarks.rs"]
pub(crate) mod benchmarks;

pub(crate) use benchmarks::*;
pub(crate) use harness::*;
pub(crate) use mock::*;
pub(crate) use types::*;

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
