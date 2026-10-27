use std::env;
use std::fs;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use llmroller_core::seams::{Clock, ExposeSecret, OsEntropy, SystemClock};
use llmroller_server::reloader::{load_initial_snapshot, start_reloader_with_pin_store};
use llmroller_server::shutdown::{JobSupervisor, ShutdownSupervisor};
use llmroller_store::crypto::{load_master_key_from_env, load_master_key_id_from_env};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::writer::start_writer;

use super::args::ServeArgs;
use super::helpers::{CliFailure, acquire_lock, resolve_data_dir};

/// Initializes global `tracing_subscriber` formatting (JSON or compact pretty) and level filtering (`FND-052`, `OBS-2`).
pub(crate) fn init_tracing(log_level: &str, log_format: &str) {
    let filter_directive = std::env::var("LLMROLLER_LOG")
        .unwrap_or_else(|_| format!("{log_level},llmroller={log_level}"));
    let env_filter = tracing_subscriber::EnvFilter::try_new(&filter_directive)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    if log_format.eq_ignore_ascii_case("pretty") || log_format.eq_ignore_ascii_case("text") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .compact()
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .json()
            .try_init();
    }
}

pub(crate) async fn run_serve(args: ServeArgs) -> ExitCode {
    init_tracing(&args.log_level, &args.log_format);
    match serve_command(args).await {
        Ok(code) => ExitCode::from(code),
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

async fn serve_command(args: ServeArgs) -> Result<u8, CliFailure> {
    // 1. Acquire flock on llmroller.lock (exit 4 on contention)
    let data_dir = resolve_data_dir(args.data_dir);
    fs::create_dir_all(&data_dir)
        .map_err(|e| CliFailure::new(13, format!("cannot create data directory: {e}")))?;
    let _lock = acquire_lock(&data_dir)?;

    // 2. Validate listener configuration, require_tls, trusted_proxies, and master key
    let trusted_proxies = env::var("LLMROLLER_TRUSTED_PROXIES").unwrap_or_default();
    let require_tls = env::var("LLMROLLER_REQUIRE_TLS")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);
    if let Err(code) = llmroller_admin::auth::check_admin_listener_address(
        &args.admin_listen,
        require_tls,
        &trusted_proxies,
    ) {
        return Err(CliFailure::new(
            code,
            "admin listener rejects non-loopback without TLS or trusted proxies".to_string(),
        ));
    }
    let has_tls_proxy = !trusted_proxies.trim().is_empty();
    match llmroller_proxy::metrics::check_data_plane_bind_security(
        &args.listen,
        has_tls_proxy,
        require_tls,
    ) {
        Ok(true) => {
            tracing::warn!(
                "Binding plaintext data-plane on non-loopback address {}; setting llmroller_plaintext_data_plane=1 (SEC-10)",
                args.listen
            );
            llmroller_proxy::metrics::set_plaintext_data_plane(1);
        }
        Ok(false) => {
            llmroller_proxy::metrics::set_plaintext_data_plane(0);
        }
        Err(code) => {
            return Err(CliFailure::new(
                code,
                format!(
                    "Refusing to bind plaintext data-plane on non-loopback {} with LLMROLLER_REQUIRE_TLS=true",
                    args.listen
                ),
            ));
        }
    }
    let master_key = load_master_key_from_env().map_err(|e| {
        let code = u8::try_from(e.exit_code()).unwrap_or(3);
        CliFailure::new(code, e.to_string())
    })?;
    let master_key_id = load_master_key_id_from_env().map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;
    let limits = llmroller_server::limits::ProxyLimits::from_env()
        .map_err(|error| CliFailure::new(2, error.to_string()))?;
    let default_routing_strategy = llmroller_server::limits::routing_strategy_from_env()
        .map_err(|error| CliFailure::new(2, error.to_string()))?;

    // 3. Connect SQLite pool (Role::Writer) and run forward-only migrations
    let database = data_dir.join("llmroller.db");
    let pool = open_pool(&database, Role::Writer)
        .await
        .map_err(|e| CliFailure::new(13, format!("cannot open database: {e}")))?;
    if let Err(error) = migrate(&pool).await {
        let code = u8::try_from(error.exit_code()).unwrap_or(13);
        return Err(CliFailure::new(code, error.to_string()));
    }
    // Eight read-only, `query_only = ON` connections so control-plane and
    // usage reads never serialize behind the single writer connection (F036).
    let reader_pool = open_pool(&database, Role::Reader)
        .await
        .map_err(|e| CliFailure::new(13, format!("cannot open reader database: {e}")))?;

    // 4. Load or initialize instance meta and instance secrets
    let clock = Arc::new(SystemClock::new());
    let entropy = Arc::new(OsEntropy::new());
    let instance_state = load_or_initialize(
        &pool,
        &master_key_id,
        &master_key,
        entropy.clone(),
        clock.now_ms(),
    )
    .await
    .map_err(|e| {
        let code = u8::try_from(e.exit_code()).unwrap_or(3);
        CliFailure::new(code, e.to_string())
    })?;
    seed_defaults(&pool, default_routing_strategy, clock.now_ms())
        .await
        .map_err(|e| CliFailure::new(13, e.to_string()))?;
    llmroller_store::prices::seed_bundled_prices(&pool, clock.now_ms())
        .await
        .map_err(|e| CliFailure::new(13, e.to_string()))?;

    // 5. Initialize setup-token if users table is empty
    let setup_token_required = env::var("LLMROLLER_SETUP_TOKEN_REQUIRED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);
    let env_setup_token = env::var("LLMROLLER_SETUP_TOKEN").ok();
    let setup_token = llmroller_admin::setup::resolve_or_create_setup_token(
        &data_dir,
        env_setup_token.as_deref(),
        setup_token_required,
        entropy.as_ref(),
    )
    .map_err(|e| CliFailure::new(1, e.to_string()))?;

    // 6. Start dual-priority writer actor and Reloader task
    let (reloader_tx, reloader_rx) = tokio::sync::mpsc::channel(256);
    let (writer_handle, _writer_task) = start_writer(pool.clone(), reloader_tx.clone());

    let initial_snapshot = load_initial_snapshot(&pool)
        .await
        .map_err(|e| CliFailure::new(13, e.to_string()))?;
    let shared_snapshot = Arc::new(arc_swap::ArcSwap::from_pointee(initial_snapshot));

    let runtime_state = Arc::new(llmroller_core::runtime_state::RuntimeState::new());
    // The reloader owns pin eviction so every publication (disable, delete, share
    // revocation, user disable) can drop unroutable session pins immediately
    // (PRV-8, CLK-4, SHR-5).
    let pin_store = Arc::new(llmroller_core::pins::PinStore::new(250_000));
    let (reloader_handle, _reloader_task) = start_reloader_with_pin_store(
        reader_pool.clone(),
        shared_snapshot.clone(),
        runtime_state.clone(),
        pin_store.clone(),
        reloader_rx,
        reloader_tx.clone(),
    );

    // 7. Fast rebuild window states and monthly accumulators (< 2 s) and hydrate RuntimeState
    let rebuilt = llmroller_store::budgets::reconstruct_budgets_from_rollups(
        &pool,
        clock.now_ms(),
        30 * 86_400_000,
    )
    .await
    .map_err(|e| CliFailure::new(13, format!("budget reconstruction failed: {e}")))?;
    runtime_state
        .accumulators
        .merge_from(&rebuilt.monthly_accumulators);
    runtime_state.rebuild_window_counters(&shared_snapshot.load(), clock.now_ms());
    for (key_id, windows) in shared_snapshot.load().budget_windows.iter() {
        let key_ring = rebuilt.key_rings.get(key_id);
        for window in windows {
            let source_ring = if window.scope_kind == "route" {
                let canonical_id = window.model_pk.and_then(|pk| {
                    shared_snapshot
                        .load()
                        .models
                        .get(&pk)
                        .map(|m| m.canonical_id.clone())
                });
                canonical_id.and_then(|cid| rebuilt.route_rings.get(&(*key_id, cid)))
            } else {
                key_ring
            };
            runtime_state.hydrate_window_counter_from_ring(window, source_ring, clock.now_ms());
            let period = window.period_ms.unwrap_or(2_678_400_000);
            if let Ok(offset) = llmroller_store::budgets::active_manual_offset_for_window(
                &pool,
                &window.id,
                period,
                clock.now_ms(),
            )
            .await
                && let Some(counter) = runtime_state.window_counter(&window.id)
            {
                counter.set_manual_offset(offset);
            }
        }
    }

    // Seed in-memory key health from persisted health rows.
    if let Ok(rows) = sqlx::query_as::<_, (String, String, Option<i64>, i64)>(
        "SELECT id, health, health_until, health_version FROM upstream_keys",
    )
    .fetch_all(&pool)
    .await
    {
        let mut max_health_version = 0_u64;
        for (id, health, health_until, version) in rows {
            max_health_version = max_health_version.max(u64::try_from(version).unwrap_or(0));
            if let Ok(key_id) = llmroller_core::ids::UpstreamKeyId::parse(&id) {
                let healthy = health == "healthy";
                runtime_state.key_health.insert(
                    key_id,
                    llmroller_core::runtime_state::KeyHealthEntry {
                        healthy,
                        cooldown_until_ms: if health == "cooling_down" {
                            health_until
                        } else {
                            None
                        },
                        exhausted_until_ms: if health == "exhausted" {
                            health_until
                        } else {
                            None
                        },
                        consecutive_failures: 0,
                        quarantine_reason: if health == "invalid" {
                            Some("persisted_invalid".to_string())
                        } else {
                            None
                        },
                    },
                );
            }
        }
        runtime_state.health.observe_version(max_health_version);
    }

    // Seed persisted route-level health cooldowns from model_routes (REL-4, REL-5, F-101).
    if let Ok(route_rows) = sqlx::query_as::<_, (String, String, String, Option<i64>)>(
        "SELECT upstream_key_id, model_pk, route_health, route_health_until
         FROM model_routes
         WHERE route_health <> 'healthy' AND route_health_until > ?",
    )
    .bind(clock.now_ms())
    .fetch_all(&pool)
    .await
    {
        for (uk_str, mpk_str, _r_health, until_opt) in route_rows {
            if let (Ok(uk_id), Ok(mpk), Some(until_ms)) = (
                llmroller_core::ids::UpstreamKeyId::parse(&uk_str),
                llmroller_core::ids::ModelPk::parse(&mpk_str),
                until_opt,
            ) {
                for proto in ["chat", "responses", "messages"] {
                    runtime_state
                        .route_protocol_blocks
                        .block_protocol(uk_id, mpk, proto, until_ms);
                }
            }
        }
    }

    // Seed durable PlanRestricted protocol blocks so restart cannot resurrect
    // a route the last discovery pass already cleared (API-8, REL-1).
    if let Ok(blocks) =
        llmroller_store::health::load_active_route_protocol_blocks(&pool, clock.now_ms()).await
    {
        for block in blocks {
            runtime_state.route_protocol_blocks.block_protocol(
                block.upstream_key_id,
                block.model_pk,
                &block.protocol,
                block.until_ms,
            );
        }
    }

    // 8. Preload unexpired confirmed pins
    let active_pins = llmroller_store::pins::load_active_pins(&pool, clock.now_ms())
        .await
        .map_err(|error| CliFailure::new(13, format!("cannot preload session pins: {error}")))?;
    {
        for pin in active_pins {
            use llmroller_core::eligibility::CandidatePoolTier;
            let tier = u8::try_from(pin.pool_tier)
                .ok()
                .and_then(CandidatePoolTier::from_u8)
                .unwrap_or(CandidatePoolTier::Blended);
            pin_store.insert_hydrated(
                llmroller_core::pins::PinKey {
                    client_key_id: pin.client_key_id,
                    canonical_id: pin.canonical_id,
                    session_hash: pin.session_hash,
                },
                llmroller_core::pins::PinEntry {
                    state: llmroller_core::pins::PinState::Confirmed,
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
    }

    // 9. Bind data-plane, control-plane, and metrics TCP listeners
    let is_ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let open_streams = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let shutdown_notify = Arc::new(tokio::sync::Notify::new());
    let shutdown_cut = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let trusted_proxies_vec: Vec<String> = if trusted_proxies.trim().is_empty() {
        vec![]
    } else {
        trusted_proxies
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    // Trace sealing runs on two dedicated OS threads; persist sealed turns off the hot path.
    let trace_sealer = std::sync::Arc::new(llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
        master_key.clone(),
        instance_state.instance_id,
        entropy.clone(),
    ));
    let (sealer_pool, _sealed_trace_rx) = llmroller_store::traces::SealerPool::new_with_queues(
        trace_sealer,
        Some(writer_handle.queues.clone()),
    );
    let sealer_pool = std::sync::Arc::new(sealer_pool);

    let mut proxy_auth_state = llmroller_proxy::auth::ProxyAuthState::new(
        shared_snapshot.clone(),
        pool.clone(),
        trusted_proxies_vec.clone(),
    );
    proxy_auth_state.budget_semaphore = Arc::new(
        llmroller_proxy::orchestrator::PreflightBudgetSemaphore::new(
            limits.preflight_body_budget_mb,
            limits.max_body_mb,
        ),
    );
    llmroller_proto::trace_tap::set_global_trace_budget_bytes(
        limits.trace_accumulator_budget_bytes(),
    );
    proxy_auth_state.is_ready = is_ready.clone();
    proxy_auth_state.open_streams = open_streams.clone();
    proxy_auth_state.shutdown_cut = shutdown_cut.clone();
    proxy_auth_state.shutdown_notify = Some(shutdown_notify.clone());
    proxy_auth_state.pin_store = pin_store.clone();
    proxy_auth_state.runtime_state = runtime_state.clone();
    proxy_auth_state.reader_pool = reader_pool.clone();
    proxy_auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    proxy_auth_state.audit_hmac_key = Some(instance_state.audit_hmac_key.clone());
    proxy_auth_state.writer = Some(writer_handle.clone());
    proxy_auth_state.master_key = Some(master_key.clone());
    proxy_auth_state.master_key_id = Some(master_key_id.clone());
    proxy_auth_state.instance_id = Some(instance_state.instance_id);
    proxy_auth_state.sealer_pool = Some(sealer_pool.clone());
    let (stop_tx, stop_rx_init) = tokio::sync::watch::channel(false);
    let (discovery_tx, _discovery_task) =
        llmroller_server::discovery_poller::start_discovery_poller_with_stop(
            llmroller_server::discovery_poller::DiscoveryPollerDeps {
                pool: pool.clone(),
                writer: writer_handle.clone(),
                runtime_state: runtime_state.clone(),
                master_key: master_key.clone(),
                instance_id: instance_state.instance_id,
            },
            stop_rx_init.clone(),
            None,
        );
    proxy_auth_state.discovery_tx = Some(discovery_tx.clone());
    let data_router = llmroller_proxy::listeners::data_plane_router(proxy_auth_state);

    let auth_config = llmroller_admin::auth::AuthConfig {
        dev_insecure_cookies: args.dev_insecure_cookies,
        require_tls,
        trusted_proxies: trusted_proxies_vec.clone(),
    };
    let mut auth_state = llmroller_admin::AuthAppState::new(pool.clone(), auth_config)
        .with_reader_pool(reader_pool.clone());
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = master_key_id.clone();
    auth_state.audit_hmac_key = Some(instance_state.audit_hmac_key.clone());
    // Session CSRF derivation is keyed by the same instance secret the data plane
    // uses for `session_hash` (`SEC-5`, `FND-041`, ISSUE-0002). Both are
    // domain-separated, so sharing the key is safe.
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    auth_state.writer = Some(writer_handle.clone());
    let setup_state = llmroller_admin::SetupAppState {
        pool: pool.clone(),
        expected_token: setup_token,
        data_dir: data_dir.clone(),
        clock: clock.clone(),
        entropy: entropy.clone(),
        audit_hmac_key: Some(instance_state.audit_hmac_key.clone()),
    };
    let mut api_state = llmroller_admin::ApiAppState::new(
        auth_state.clone(),
        master_key.clone(),
        master_key_id.clone(),
    );
    api_state.writer = Some(writer_handle.clone());
    api_state.audit_hmac_key = Some(instance_state.audit_hmac_key.clone());
    api_state.attach_reloader(
        reloader_handle.notification_sender(),
        reloader_handle.version_receiver(),
    );
    api_state.attach_pin_store(pin_store.clone());
    api_state.discovery_tx = Some(discovery_tx);
    // Sealing happens on the dedicated OS pool. This bridge only moves the
    // sealed payload into the single writer's queue; it never opens SQLite
    // (F09, PERF-1).
    // FND-024: SealerPool workers now push directly into writer_handle.queues before decrementing in_flight.

    let _usage_poller_task = llmroller_server::usage_poller::start_usage_poller_with_stop(
        llmroller_server::usage_poller::UsagePollerDeps {
            snapshot: shared_snapshot.clone(),
            runtime_state: runtime_state.clone(),
            writer: writer_handle.clone(),
            master_key: master_key.clone(),
            master_key_id: master_key_id.clone(),
            instance_id: instance_state.instance_id,
        },
        stop_tx.subscribe(),
        None,
    );
    let admin_router =
        llmroller_admin::control_plane_router_full(setup_state, auth_state, api_state);
    let metrics_router = llmroller_proxy::listeners::metrics_router();

    let data_listener = tokio::net::TcpListener::bind(&args.listen)
        .await
        .map_err(|e| {
            CliFailure::new(5, format!("cannot bind data listener {}: {e}", args.listen))
        })?;
    let admin_listener = tokio::net::TcpListener::bind(&args.admin_listen)
        .await
        .map_err(|e| {
            CliFailure::new(
                5,
                format!("cannot bind admin listener {}: {e}", args.admin_listen),
            )
        })?;
    let metrics_listener = tokio::net::TcpListener::bind(&args.metrics_listen)
        .await
        .map_err(|e| {
            CliFailure::new(
                5,
                format!("cannot bind metrics listener {}: {e}", args.metrics_listen),
            )
        })?;

    // 10. Flip /readyz to 200 OK and spawn listeners
    is_ready.store(true, std::sync::atomic::Ordering::SeqCst);
    eprintln!("LLMRoller server ready on {}", args.listen);

    let (listener_stop_tx, mut stop_rx1) = tokio::sync::watch::channel(false);
    let mut stop_rx2 = listener_stop_tx.subscribe();
    let mut stop_rx3 = listener_stop_tx.subscribe();
    let _bg_stop_rx_unused = stop_rx_init;

    let data_server = tokio::spawn(async move {
        let _ = axum::serve(
            data_listener,
            data_router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            while !*stop_rx1.borrow_and_update() {
                if stop_rx1.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    });

    let admin_server = tokio::spawn(async move {
        let _ = axum::serve(
            admin_listener,
            admin_router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            while !*stop_rx2.borrow_and_update() {
                if stop_rx2.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    });

    let metrics_server = tokio::spawn(async move {
        let _ = axum::serve(metrics_listener, metrics_router)
            .with_graceful_shutdown(async move {
                while !*stop_rx3.borrow_and_update() {
                    if stop_rx3.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
    });

    // Spawn background WalCheckpointSupervisor and RetentionSweeper
    let stop_rx_bg = stop_tx.subscribe();
    let bg_pool = pool.clone();
    let bg_ready = is_ready.clone();
    let bg_snapshot = shared_snapshot.clone();
    let bg_pin_store = pin_store.clone();
    let bg_runtime_state = runtime_state.clone();
    let bg_reloader_tx = reloader_tx.clone();
    let bg_audit_key = instance_state.audit_hmac_key.clone();
    let wal_path = format!("{}-wal", database.display());
    // The maintenance tick is supervised so a panic in the checkpoint/retention path
    // restarts the job with exponential backoff instead of silently losing it for the
    // lifetime of the process (Section 5.5.3, `REL-2`).
    let maintenance_job = JobSupervisor::new("maintenance");
    let _bg_jobs = tokio::spawn(async move {
        maintenance_job
            .run_supervised(stop_rx_bg, move |mut stop_rx| {
                let bg_pool = bg_pool.clone();
                let bg_ready = bg_ready.clone();
                let bg_snapshot = bg_snapshot.clone();
                let bg_pin_store = bg_pin_store.clone();
                let bg_runtime_state = bg_runtime_state.clone();
                let bg_reloader_tx = bg_reloader_tx.clone();
                let bg_audit_key = bg_audit_key.clone();
                let wal_path = wal_path.clone();
                async move {
                    let wal_supervisor =
                        llmroller_store::checkpoint::WalCheckpointSupervisor::new(bg_ready);
                    let mut interval = tokio::time::interval(Duration::from_secs(30));
                    while !*stop_rx.borrow_and_update() {
                        tokio::select! {
                            _ = interval.tick() => {
                                let wal_bytes = wal_supervisor
                                    .run_checkpoint_for_path(&bg_pool, std::path::Path::new(&wal_path))
                                    .await
                                    .unwrap_or_else(|_| std::fs::metadata(&wal_path).map_or(0, |m| m.len()));
                                llmroller_proxy::metrics::record_wal_bytes(wal_bytes);
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_or(0, |d| d.as_millis() as i64);
                                if let Ok(mut tx) = bg_pool.begin().await {
                                    let _ = llmroller_store::retention::sweep_expired_web_sessions(&mut tx, now_ms).await;
                                    let _ = llmroller_store::retention::sweep_expired_response_links(&mut tx, now_ms).await;
                                    let _ = tx.commit().await;
                                }
                                let _ = llmroller_store::retention::sweep_expired_pins(&bg_pool, now_ms).await;
                                bg_pin_store.sweep(now_ms);
                                bg_runtime_state.constant_session_detector.sweep_idle(now_ms);
                                bg_runtime_state.accumulators.prune_old_months(now_ms);
                                let _ = llmroller_store::retention::sweep_expired_trace_records(&bg_pool, now_ms).await;
                                let snapshot = bg_snapshot.load();
                                let retention_days = snapshot.settings.usage_events_retention_days.max(1);
                                let max_rows = snapshot.settings.usage_events_max_rows.max(1);
                                let cutoff = now_ms - retention_days.saturating_mul(86_400_000);
                                let _ = llmroller_store::retention::sweep_expired_usage_events(&bg_pool, cutoff, 5_000).await;
                                let _ = llmroller_store::retention::enforce_max_usage_events_rows_pool(&bg_pool, max_rows, 5_000).await;
                                // Derived data and side tables share the same tick (F038):
                                // rollups keep at least 31 days for long-range charts,
                                // usage snapshots 90 days, abuse counters one day, and
                                // content-addressed trace blocks one full trace window.
                                let rollup_cutoff = now_ms - retention_days.max(31).saturating_mul(86_400_000);
                                let _ = llmroller_store::retention::sweep_expired_usage_rollups_5min(&bg_pool, rollup_cutoff, 5_000).await;
                                let _ = llmroller_store::retention::sweep_expired_usage_rollups_hourly(&bg_pool, rollup_cutoff, 5_000).await;
                                let _ = llmroller_store::retention::sweep_expired_upstream_usage_snapshots(&bg_pool, now_ms - 90_i64 * 86_400_000, 1_000).await;
                                let _ = llmroller_store::retention::sweep_expired_auth_failure_counters(&bg_pool, now_ms - 86_400_000).await;
                                let _ = llmroller_store::retention::sweep_orphaned_trace_blocks(&bg_pool, cutoff, 1_000).await;
                                let _ = llmroller_store::retention::run_incremental_vacuum(&bg_pool, 500).await;

                                // Auto-disable traceability if expiry timestamp reached (TRC-1, AUD-1, F-093)
                                if llmroller_core::trace_config::check_auto_disable_expired(
                                    &snapshot.settings.traceability_config,
                                    now_ms,
                                ) {
                                    let _ = auto_disable_expired_traceability(
                                        &bg_pool,
                                        &snapshot.settings.traceability_config,
                                        bg_audit_key.expose_secret(),
                                        Some(&bg_reloader_tx),
                                        now_ms,
                                    )
                                    .await;
                                }
                            }
                            _ = stop_rx.changed() => break,
                        }
                    }
                }
            })
            .await;
    });

    wait_for_shutdown_signal().await;
    eprintln!("Shutdown signal received; initiating graceful drain...");

    let (drain_grace, drain_timeout) = {
        let snap = shared_snapshot.load();
        let timeout_secs = snap.settings.drain_timeout_secs.clamp(5, 300) as u64;
        (Duration::from_secs(5), Duration::from_secs(timeout_secs))
    };
    let shutdown_sup = ShutdownSupervisor::new(
        is_ready.clone(),
        open_streams.clone(),
        shutdown_notify.clone(),
        pool.clone(),
        drain_grace,
        drain_timeout,
    )
    .with_writer(writer_handle.clone())
    .with_shutdown_cut(shutdown_cut.clone())
    .with_listener_stop(listener_stop_tx.clone())
    .with_background_stop(stop_tx.clone())
    .with_sealer_pool(sealer_pool.clone())
    .with_runtime_state(runtime_state.clone());
    let exit_code = shutdown_sup.execute_graceful_shutdown().await;

    let _ = listener_stop_tx.send(true);
    let _ = stop_tx.send(true);
    let _ = tokio::join!(data_server, admin_server, metrics_server);
    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await;

    Ok(exit_code)
}

async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sigterm) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            let _ = sigterm.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

/// Atomically disables expired content traceability in `settings.value_json`, writes the
/// `traceability.auto_disabled` audit entry, increments `config_version`, and notifies `Reloader` (`TRC-1`, `AUD-1`, `F-093`).
pub(crate) async fn auto_disable_expired_traceability(
    pool: &sqlx::SqlitePool,
    current_config: &llmroller_core::settings::TraceabilityConfig,
    audit_hmac_key: &[u8],
    reloader_tx: Option<&tokio::sync::mpsc::Sender<llmroller_store::writer::ReloadNotification>>,
    now_ms: i64,
) -> Result<bool, sqlx::Error> {
    if !llmroller_core::trace_config::check_auto_disable_expired(current_config, now_ms) {
        return Ok(false);
    }
    let mut updated = current_config.clone();
    updated.enabled = false;
    let Ok(json) = serde_json::to_string(&updated) else {
        return Ok(false);
    };
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO settings (key, value_json, updated_at) VALUES ('traceability_config', ?, ?)
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
    )
    .bind(&json)
    .bind(now_ms)
    .execute(&mut *tx)
    .await?;

    let audit_id = llmroller_core::ids::RequestId::generate().to_string();
    let _ = llmroller_store::audit::append(
        &mut tx,
        audit_hmac_key,
        llmroller_store::audit::AuditEntry {
            id: &audit_id,
            ts: now_ms,
            actor_user_id: None,
            ip_addr: None,
            action: "traceability.auto_disabled",
            target_type: "setting",
            target_id: Some("traceability_config"),
            detail_json: Some("{\"reason\":\"auto_disable_at_expired\"}"),
        },
    )
    .await;

    let new_ver = llmroller_store::writer::bump_config_version_tx(&mut tx).await?;
    tx.commit().await?;

    if let Some(tx_chan) = reloader_tx {
        let _ = tx_chan
            .send(llmroller_store::writer::ReloadNotification {
                min_version: new_ver,
                changed_domains: vec![
                    llmroller_core::snapshot::ConfigDomain::Settings,
                    llmroller_core::snapshot::ConfigDomain::Traceability,
                ],
                invalidations: vec![
                    llmroller_core::snapshot::CacheInvalidationEvent::SettingsChanged,
                ],
            })
            .await;
    }
    Ok(true)
}
