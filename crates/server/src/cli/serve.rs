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

/// The documented-but-unimplemented native TLS listener variables (`ISSUE-0227`).
///
/// Named here so the rejection message and the docs can be checked against one spelling, and so a
/// future native listener has an obvious place to replace this pairing with a real one.
const TLS_CERT_ENV: &str = "LLMROLLER_TLS_CERT";
const TLS_KEY_ENV: &str = "LLMROLLER_TLS_KEY";

/// `SEC-9`: the plaintext-listener error must repeat at this period, in addition to the one-off
/// startup warning.
pub(crate) const PLAINTEXT_REPEAT_INTERVAL: Duration = Duration::from_secs(600);

/// `SEC-13`/`AUD-1`: at least one `audit_anchor` per hour, in addition to the 100-row counter.
pub(crate) const AUDIT_ANCHOR_INTERVAL: Duration = Duration::from_secs(3600);

/// Override the two periodic intervals, in seconds.
///
/// Test-only plumbing, and deliberately a single variable covering both: the alternative is an
/// end-to-end test that waits ten minutes for the repeater and an hour for the anchor, which nobody
/// will keep running. It is read once at startup and has no effect on the documented defaults when
/// unset — an operator cannot set it by accident because it is not in any documented env table.
pub(crate) const PERIODIC_INTERVAL_OVERRIDE_ENV: &str = "LLMROLLER_TEST_PERIODIC_INTERVAL_SECS";

/// Resolve the two periodic intervals from an optional override string.
///
/// Takes the value rather than reading the environment so the decision is a pure function and
/// testable without mutating process-global state (`std::env::set_var` is unsafe in edition 2024,
/// and this crate forbids `unsafe`).
///
/// Returns the documented defaults when the override is absent, unparseable, or zero — a bad test
/// knob must never change production behaviour, and "0" must not silently disable a timer.
pub(crate) fn periodic_intervals(override_value: Option<&str>) -> (Duration, Duration) {
    match override_value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
    {
        Some(seconds) => (Duration::from_secs(seconds), Duration::from_secs(seconds)),
        None => (PLAINTEXT_REPEAT_INTERVAL, AUDIT_ANCHOR_INTERVAL),
    }
}

/// Log target for the repeating plaintext error. The tests select on this target, so it is a
/// constant rather than a literal repeated in two places.
const PLAINTEXT_REPEATER_TARGET: &str = "llmroller::plaintext_repeater";

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

    // `ISSUE-0227`: refuse to start when a native TLS listener is requested.
    //
    // Four spec documents describe `LLMROLLER_TLS_CERT`/`LLMROLLER_TLS_KEY`, and this binary never
    // reads them for a listener (TLS terminates at the reverse proxy — see `A4`). Before this
    // check, a deployment that set them was served **plaintext with no warning**, which is the
    // worst failure mode for a security-relevant option: indistinguishable from one that works.
    //
    // Exit `2` is §5.5.1's existing row for "invalid CLI flag, invalid environment variable";
    // no new code is allocated. Presence is tested with `env::var`, not `env::var(...).is_ok()`,
    // so `LLMROLLER_TLS_CERT=""` counts as **set**: an operator who exported an empty variable
    // believes they configured something, and treating that as absent would reproduce this exact
    // defect at a smaller size.
    if let Some(name) = [TLS_CERT_ENV, TLS_KEY_ENV]
        .into_iter()
        .find(|name| env::var(name).is_ok())
    {
        return Err(CliFailure::new(
            2,
            format!(
                "{name} is set, but this binary does not run a TLS listener. Terminate TLS at the \
                 reverse proxy instead, and list the proxy in LLMROLLER_TRUSTED_PROXIES so \
                 forwarded headers and the real scheme are trusted. Unset {TLS_CERT_ENV} and \
                 {TLS_KEY_ENV} to start the server."
            ),
        ));
    }

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
    // Computed once here, where the bind address is known, and carried into the repeater as a
    // `bool`. Re-deriving it inside the timer would duplicate the loopback classification in
    // `check_data_plane_bind_security`, and duplicated classification drifts (`ISSUE-0187`).
    let non_loopback_plaintext = match llmroller_proxy::metrics::check_data_plane_bind_security(
        &args.listen,
        has_tls_proxy,
        require_tls,
    ) {
        Ok(non_loopback_plaintext) => {
            if non_loopback_plaintext {
                tracing::warn!(
                    "Binding plaintext data-plane on non-loopback address {}; setting llmroller_plaintext_data_plane=1 (SEC-10)",
                    args.listen
                );
            }
            llmroller_proxy::metrics::set_plaintext_data_plane(u8::from(non_loopback_plaintext));
            non_loopback_plaintext
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
    };
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
        argon2_semaphore: auth_state.argon2_semaphore.clone(),
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
                                // Trace memory is process-global and only changes on reservation
                                // and release, so sampling it on this 30 s tick is enough to see
                                // it approaching the cap (`PERF-5`, `OBS-1`).
                                let (trace_used, trace_budget) =
                                    llmroller_proto::trace_tap::global_trace_memory();
                                llmroller_proxy::metrics::record_trace_memory(
                                    trace_used,
                                    trace_budget,
                                );
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_or(0, |d| d.as_millis() as i64);
                                let snapshot = bg_snapshot.load();
                                let _ = run_maintenance_retention_sweep(
                                    &bg_pool,
                                    now_ms,
                                    &snapshot.settings,
                                )
                                .await;
                                // In-memory housekeeping that needs no database work; kept
                                // beside the sweep because it shares the tick, not because it
                                // shares the SQL contract.
                                bg_pin_store.sweep(now_ms);
                                bg_runtime_state.constant_session_detector.sweep_idle(now_ms);
                                bg_runtime_state.accumulators.prune_old_months(now_ms);

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

    // Spec-required periodic tasks (`SEC-13`/`AUD-1` at least hourly, `SEC-9` every 10 minutes).
    //
    // Both are spawned here, after the listeners are up, so their first tick lands a full period
    // after startup rather than racing it. Each stops on `stop_tx`, which the shutdown supervisor
    // sends below -- so neither survives the drain.
    //
    // The plaintext repeater is `None` on a loopback bind and therefore spawns no task at all: a
    // timer that wakes every ten minutes only to decide not to log is a wakeup for nothing on
    // every normal deployment.
    let (repeater_interval, anchor_interval) =
        periodic_intervals(env::var(PERIODIC_INTERVAL_OVERRIDE_ENV).ok().as_deref());
    let plaintext_repeater = spawn_plaintext_repeater(
        stop_tx.subscribe(),
        non_loopback_plaintext,
        args.listen.clone(),
        repeater_interval,
    );
    // The anchor ticker reads the chain tip, so it needs the writer pool. It is not supervised like
    // the maintenance job: its body is one read plus a log line, so a panic is a defect to surface
    // rather than a job to restart with backoff.
    let audit_anchor_ticker =
        spawn_audit_anchor_ticker(Some(pool.clone()), stop_tx.subscribe(), anchor_interval, {
            let anchor_clock = clock.clone();
            move || anchor_clock.now_ms()
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
    // Both periodic tasks observe the same `stop_tx`; awaiting them makes "stopped" an observed
    // fact rather than an assumption about what the select! arm did.
    if let Some(repeater) = plaintext_repeater {
        repeater.shutdown().await;
    }
    audit_anchor_ticker.shutdown().await;
    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await;

    Ok(exit_code)
}

/// Whether an hourly anchor is due at `now_ms`, given when one was last emitted.
///
/// The clock is a parameter rather than a captured instant so tests drive it directly instead of
/// sleeping for an hour (`ISSUE-0186`); production passes the system clock.
///
/// The guard has two halves and both are load-bearing:
///
/// - **No previous anchor** ⇒ due. Otherwise a process that starts and then sits idle emits nothing
///   ever, which is the defect being fixed.
/// - **Previous anchor at least one period ago** ⇒ due. `>=` not `>`: exactly one period later the
///   anchor is due, so "at least hourly" holds rather than "hourly plus a tick".
pub(crate) fn audit_anchor_is_due(
    now_ms: i64,
    last_anchor_at_ms: Option<i64>,
    period: Duration,
) -> bool {
    let period_ms = i64::try_from(period.as_millis()).unwrap_or(i64::MAX);
    match last_anchor_at_ms {
        None => true,
        Some(last) => now_ms.saturating_sub(last) >= period_ms,
    }
}

/// Run one hourly-anchor pass: attest the chain tip if an anchor is due.
///
/// Returns whether an anchor was emitted, so a caller can distinguish "nothing was due" from
/// "nothing was emitted because the log is empty".
///
/// Three negative cases, all deliberate:
///
/// - **Empty `audit_log`** ⇒ nothing. `{"seq":0,"hash":""}` would attest a chain that does not
///   exist, which is worse than silence (`ISSUE-0186`).
/// - **Tip anchored less than a period ago** ⇒ nothing. Without this the timer re-anchors the tip
///   the 100-row counter just wrote, and the stream carries two attestations for one event.
/// - **Read failure** ⇒ propagated, no anchor emitted. Attesting a tip we could not read would be a
///   fabrication; the next tick retries and the absence of anchors is itself the operator's signal.
///
/// # Errors
///
/// Propagates the tip-read failure.
pub(crate) async fn run_audit_anchor_tick(
    pool: &sqlx::SqlitePool,
    now_ms: i64,
    period: Duration,
    last_anchor_at_ms: Option<i64>,
) -> Result<bool, llmroller_store::audit::AuditError> {
    if !audit_anchor_is_due(now_ms, last_anchor_at_ms, period) {
        return Ok(false);
    }
    let Some(tip) = llmroller_store::audit::read_chain_tip(pool).await? else {
        return Ok(false);
    };
    llmroller_store::audit::emit_anchor(tip.seq, &tip.hash, tip.ts, now_ms);
    Ok(true)
}

/// Emit one repeating plaintext-listener error line (`SEC-9`).
///
/// Split from the task so a test drives the tick body without waiting ten minutes. The level is
/// `error` because the startup line is already the warning — repeating a warning reads as noise,
/// and `SEC-9` explicitly asks for an error.
pub(crate) fn emit_plaintext_repeater_error(listen_addr: &str) {
    tracing::error!(
        target: PLAINTEXT_REPEATER_TARGET,
        listen = %listen_addr,
        "Data plane is still serving plaintext on non-loopback {}; TLS is not terminated upstream and no trusted proxy is configured (SEC-9, SEC-10). Set LLMROLLER_TRUSTED_PROXIES or LLMROLLER_REQUIRE_TLS.",
        listen_addr
    );
}

/// Spawn the 10-minute plaintext repeater, or nothing at all on a loopback bind.
///
/// `None` for a loopback bind is the requirement rather than an optimization: a task that wakes
/// every ten minutes only to decide not to log is a wakeup for nothing, forever, on every normal
/// deployment.
///
/// The decision arrives as a `bool` computed once at startup where the bind address is known.
/// Re-deriving it here would duplicate the loopback classification, and duplicated classification
/// drifts.
pub(crate) fn spawn_plaintext_repeater(
    stop_rx: tokio::sync::watch::Receiver<bool>,
    non_loopback_plaintext: bool,
    listen_addr: String,
    period: Duration,
) -> Option<llmroller_server::shutdown::PeriodicTask> {
    if !non_loopback_plaintext {
        return None;
    }
    Some(llmroller_server::shutdown::PeriodicTask::spawn(
        period,
        stop_rx,
        move || {
            // Captured once: the repeater reports the bind it was started for, and a mid-run
            // environment change is not a configuration path.
            let listen_addr = listen_addr.clone();
            async move { emit_plaintext_repeater_error(&listen_addr) }
        },
    ))
}

/// Spawn the hourly audit-anchor ticker.
///
/// `now_ms` is a function rather than a captured instant so tests supply a synthetic clock. `pool`
/// is optional so the shutdown test can exercise the stop path with no database; a `None` pool makes
/// every tick a no-op. The production call site always passes `Some`.
pub(crate) fn spawn_audit_anchor_ticker<F>(
    pool: Option<sqlx::SqlitePool>,
    stop_rx: tokio::sync::watch::Receiver<bool>,
    period: Duration,
    now_ms: F,
) -> llmroller_server::shutdown::PeriodicTask
where
    F: Fn() -> i64 + Send + Sync + 'static,
{
    let now_ms = Arc::new(now_ms);
    llmroller_server::shutdown::PeriodicTask::spawn(period, stop_rx, move || {
        let pool = pool.clone();
        let now_ms = now_ms.clone();
        async move {
            let Some(pool) = pool else {
                return;
            };
            let now = now_ms();
            match run_audit_anchor_tick(
                &pool,
                now,
                period,
                llmroller_store::audit::last_anchor_at_ms(),
            )
            .await
            {
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        target: "llmroller::audit_anchor",
                        error = %error,
                        "hourly audit anchor skipped: chain tip could not be read"
                    );
                }
            }
        }
    })
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
/// One maintenance tick's retention sweep over every time-bounded table.
///
/// Extracted from the tick closure so it is reachable from a test (`ISSUE-0230`). The sweep
/// used to be ~25 statements inline inside a closure passed to `JobSupervisor::run_supervised`
/// → `tokio::spawn` → `run_serve`, which no test could drive. That unreachability is exactly
/// why `control_plane_logs` was silently omitted from it: nothing asserted what the tick did,
/// so an omission looked identical to coverage.
///
/// Windows, each for a stated reason:
///
/// - `usage_events` and `control_plane_logs` — the configured `usage_events_retention_days`.
///   `AUD-2` specifies *"retained for 30 days"* for control-plane logs and `14.l` names the
///   setting (`usage_events_retention_days = 30`), so the window is configured, not fixed. The
///   uncalled `run_retention_pass` used 90 days for this table, which is a divergence from both
///   the spec and the live tick.
/// - rollups — `max(31)` days, so long-range charts always have history (`F038`).
/// - `upstream_usage_snapshots` — a fixed 90 days.
/// - `auth_failure_counters` — one day; they exist for recent abuse detection only.
/// - `trace_blocks` — one full trace window, keyed on the raw cutoff.
///
/// Every sweep is bounded, so one tick can never hold the write lock for a long backlog
/// (`REL-3`).
pub(crate) async fn run_maintenance_retention_sweep(
    pool: &sqlx::SqlitePool,
    now_ms: i64,
    settings: &llmroller_core::settings::RuntimeSettings,
) -> Result<(), sqlx::Error> {
    const DAY_MS: i64 = 86_400_000;
    const CHUNK: i64 = 5_000;

    let retention_days = settings.usage_events_retention_days.max(1);
    let max_rows = settings.usage_events_max_rows.max(1);
    let cutoff = now_ms - retention_days.saturating_mul(DAY_MS);

    // Expiry is the row's own timestamp, so these two share one transaction.
    let mut tx = pool.begin().await?;
    llmroller_store::retention::sweep_expired_web_sessions(&mut tx, now_ms).await?;
    llmroller_store::retention::sweep_expired_response_links(&mut tx, now_ms).await?;
    tx.commit().await?;

    llmroller_store::retention::sweep_expired_pins(pool, now_ms).await?;
    llmroller_store::retention::sweep_expired_trace_records(pool, now_ms).await?;

    llmroller_store::retention::sweep_expired_usage_events(pool, cutoff, CHUNK).await?;
    llmroller_store::retention::enforce_max_usage_events_rows_pool(pool, max_rows, CHUNK).await?;

    // Derived data and side tables share the same tick (`F038`).
    let rollup_cutoff = now_ms - retention_days.max(31).saturating_mul(DAY_MS);
    llmroller_store::retention::sweep_expired_usage_rollups_5min(pool, rollup_cutoff, CHUNK)
        .await?;
    llmroller_store::retention::sweep_expired_usage_rollups_hourly(pool, rollup_cutoff, CHUNK)
        .await?;
    llmroller_store::retention::sweep_expired_upstream_usage_snapshots(
        pool,
        now_ms - 90 * DAY_MS,
        1_000,
    )
    .await?;
    llmroller_store::retention::sweep_expired_auth_failure_counters(pool, now_ms - DAY_MS).await?;
    llmroller_store::retention::sweep_orphaned_trace_blocks(pool, cutoff, 1_000).await?;
    // `control_plane_logs` — the omission this issue is about. Same configured window as
    // `usage_events`, because `AUD-2`/`14.l` say control-plane logs are kept for exactly that
    // long, and bounded for the same `REL-3` reason as every other sweep.
    llmroller_store::retention::sweep_expired_control_plane_logs(pool, cutoff, CHUNK).await?;
    llmroller_store::retention::run_incremental_vacuum(pool, 500).await?;
    Ok(())
}

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
