#![forbid(unsafe_code)]
//! Command-line interface and offline maintenance commands.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes

use std::env;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use llmroller_core::seams::{Clock, ExposeSecret, OsEntropy, SystemClock};
use llmroller_server::reloader::{load_initial_snapshot, start_reloader_with_pin_store};
use llmroller_server::shutdown::{JobSupervisor, ShutdownSupervisor};
use llmroller_store::crypto::{MasterKey, load_master_key_from_env, load_master_key_id_from_env};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};
use llmroller_store::rotation::rotate_master_key;
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::reset_admin;
use llmroller_store::writer::start_writer;

const DATA_DIR_ENV: &str = "LLMROLLER_DATA_DIR";
const DEFAULT_DATA_DIR: &str = "/var/lib/llmroller";

fn version_string() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        let sha = option_env!("LLMROLLER_BUILD_GIT_SHA")
            .or(option_env!("GIT_SHA"))
            .unwrap_or("dev");
        format!(
            "{} ({}, schema {})",
            env!("CARGO_PKG_VERSION"),
            sha,
            llmroller_store::migrations::SCHEMA_VERSION
        )
    })
}

/// Top-level `llmroller` command.
#[derive(Debug, Parser)]
#[command(
    name = "llmroller",
    version = version_string(),
    about = "LLMRoller proxy server and offline maintenance tools"
)]
pub(crate) struct Cli {
    /// Maintenance subcommand to execute.
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Run the proxy and management server daemon (POR-1, REL-1).
    Serve(ServeArgs),
    /// Re-seal all encrypted rows under a replacement master key.
    RotateMasterKey(RotateMasterKeyArgs),
    /// Restore an administrator account offline.
    ResetAdmin(ResetAdminArgs),
    /// Verify the audit-log HMAC chain.
    VerifyAuditLog(VerifyAuditLogArgs),
    /// Probe the container readiness endpoint.
    Healthcheck(HealthcheckArgs),
    /// Create a crash-safe atomic backup of the database (REL-6).
    Backup(BackupArgs),
    /// Restore a verified SQLite backup into the data directory (`REL-6`, `F083`).
    Restore(RestoreArgs),
    /// Verify database integrity, schema, master key, and audit chain (`F083`).
    Doctor(DoctorArgs),
    /// Export a redacted support diagnostics JSON bundle (`F086`).
    ExportDiagnostics(ExportDiagnosticsArgs),
    /// Generate a 32-byte cryptographically secure master key (SEC-2).
    GenerateMasterKey,
}

/// Arguments for `llmroller serve`.
#[derive(Debug, Args)]
pub(crate) struct ServeArgs {
    /// Data-plane HTTP listener address.
    #[arg(long, env = "LLMROLLER_LISTEN", default_value = "0.0.0.0:8080")]
    pub(crate) listen: String,

    /// Control-plane HTTP listener address.
    #[arg(long, env = "LLMROLLER_ADMIN_LISTEN", default_value = "127.0.0.1:8081")]
    pub(crate) admin_listen: String,

    /// Metrics listener address.
    #[arg(
        long,
        env = "LLMROLLER_METRICS_LISTEN",
        default_value = "127.0.0.1:9090"
    )]
    pub(crate) metrics_listen: String,

    /// Data directory containing `llmroller.db` and `llmroller.lock`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,

    /// Allow non-HTTPS cookies for local dev.
    #[arg(long, default_value_t = false)]
    pub(crate) dev_insecure_cookies: bool,
}

/// Arguments for `llmroller backup`.
#[derive(Debug, Args)]
pub(crate) struct BackupArgs {
    /// Destination output path.
    #[arg(long)]
    pub(crate) out: PathBuf,
    /// Overwrite destination if it already exists.
    #[arg(long, default_value_t = false)]
    pub(crate) overwrite: bool,
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Arguments for `llmroller restore` (`F083`, `REL-6`).
#[derive(Debug, Args)]
pub(crate) struct RestoreArgs {
    /// Source backup file path to restore from.
    #[arg(long)]
    pub(crate) from: PathBuf,
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Arguments for `llmroller doctor` (`F083`).
#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Arguments for `llmroller export-diagnostics` (`F086`).
#[derive(Debug, Args)]
pub(crate) struct ExportDiagnosticsArgs {
    /// Optional output JSON file path (writes to stdout when omitted).
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Arguments for `llmroller healthcheck`.
#[derive(Debug, Args)]
pub(crate) struct HealthcheckArgs {
    /// URL to probe.
    #[arg(long, default_value = "http://127.0.0.1:8080/readyz")]
    pub(crate) url: String,
    /// Timeout in milliseconds.
    #[arg(long, default_value_t = 2000)]
    pub(crate) timeout_ms: u64,
}

/// Arguments for `llmroller verify-audit-log`.
#[derive(Debug, Args)]
pub(crate) struct VerifyAuditLogArgs {
    /// Expected anchor sequence number.
    #[arg(long)]
    pub(crate) anchor_seq: Option<i64>,
    /// Expected anchor HMAC-SHA256 hex hash.
    #[arg(long)]
    pub(crate) anchor_hash: Option<String>,
    /// Data directory containing `llmroller.db`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
    /// Output summary in JSON format.
    #[arg(long)]
    pub(crate) json: bool,
}

/// Arguments for `llmroller reset-admin`.
#[derive(Debug, Args)]
pub(crate) struct ResetAdminArgs {
    /// Username of the administrator account to restore.
    #[arg(long)]
    pub(crate) username: String,
    /// Data directory containing `llmroller.db` and `llmroller.lock`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Arguments for `llmroller rotate-master-key`.
#[derive(Debug, Args)]
pub(crate) struct RotateMasterKeyArgs {
    /// Environment variable containing the current base64 master key.
    #[arg(long)]
    pub(crate) old_key_env: String,
    /// Environment variable containing the replacement base64 master key.
    #[arg(long)]
    pub(crate) new_key_env: String,
    /// Identifier to store as the new active master-key id.
    #[arg(long)]
    pub(crate) new_key_id: String,
    /// Data directory containing `llmroller.db` and `llmroller.lock`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

/// Parse arguments and dispatch one command.
pub(crate) async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => run_serve(args).await,
        Command::RotateMasterKey(args) => run_rotation(args).await,
        Command::ResetAdmin(args) => run_reset_admin(args).await,
        Command::VerifyAuditLog(args) => run_verify_audit_log(args).await,
        Command::Healthcheck(args) => run_healthcheck(args).await,
        Command::Backup(args) => run_backup_cmd(args).await,
        Command::Restore(args) => run_restore_cmd(args).await,
        Command::Doctor(args) => run_doctor_cmd(args).await,
        Command::ExportDiagnostics(args) => run_export_diagnostics_cmd(args).await,
        Command::GenerateMasterKey => run_generate_master_key(),
    }
}

async fn run_serve(args: ServeArgs) -> ExitCode {
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
    match llmroller_proxy::metrics::check_data_plane_bind_security(&args.listen, has_tls_proxy, require_tls)
    {
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
            if let Ok(delta) = llmroller_store::budgets::sum_budget_adjustments_for_window(
                &pool,
                &window.id,
                clock.now_ms(),
            )
            .await
                && delta != 0
                && let Some(counter) = runtime_state.window_counter(&window.id)
            {
                let period = window.period_ms.unwrap_or(2_678_400_000);
                counter.set_manual_offset(Some(llmroller_core::windows::ManualOffset {
                    effective_at_ms: clock.now_ms(),
                    delta_micro_usd: delta,
                    period_ms: period,
                }));
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
            let tier = match pin.pool_tier {
                0 => CandidatePoolTier::Own,
                1 => CandidatePoolTier::Shared,
                2 => CandidatePoolTier::Global,
                _ => CandidatePoolTier::Blended,
            };
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
    let (sealer_pool, sealed_trace_rx) = llmroller_store::traces::SealerPool::new(trace_sealer);
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
    let trace_writer = writer_handle.clone();
    std::thread::Builder::new()
        .name("llmroller-trace-enqueue".to_string())
        .spawn(move || {
            while let Ok(sealed) = sealed_trace_rx.recv() {
                trace_writer.queues.push_sealed_trace(sealed);
            }
        })
        .expect("spawn trace enqueue thread");

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

    let mut stop_rx1 = stop_rx_init;
    let mut stop_rx2 = stop_tx.subscribe();
    let mut stop_rx3 = stop_tx.subscribe();

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
                                let wal_bytes = std::fs::metadata(&wal_path).map_or(0, |m| m.len());
                                llmroller_proxy::metrics::record_wal_bytes(wal_bytes);
                                let _ = wal_supervisor.run_checkpoint(&bg_pool, wal_bytes).await;
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
    .with_background_stop(stop_tx.clone())
    .with_sealer_pool(sealer_pool.clone())
    .with_runtime_state(runtime_state.clone());
    let exit_code = shutdown_sup.execute_graceful_shutdown().await;

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

fn run_generate_master_key() -> ExitCode {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use llmroller_core::seams::{EntropySource, OsEntropy};
    use zeroize::Zeroize;

    let mut key = [0_u8; 32];
    OsEntropy.fill_bytes(&mut key);
    let encoded = BASE64.encode(key);
    key.zeroize();
    println!("{encoded}");
    ExitCode::SUCCESS
}

async fn run_restore_cmd(args: RestoreArgs) -> ExitCode {
    let data_dir = resolve_data_dir(args.data_dir);
    if let Err(e) = fs::create_dir_all(&data_dir) {
        eprintln!("error: cannot create data directory: {e}");
        return ExitCode::from(13);
    }
    let _lock = match acquire_lock(&data_dir) {
        Ok(lock) => lock,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            return ExitCode::from(failure.code);
        }
    };
    let target_db = data_dir.join("llmroller.db");
    let master_key = load_master_key_from_env().ok();
    let master_key_id = load_master_key_id_from_env().ok();
    match llmroller_server::backup::restore_backup(
        &args.from,
        &target_db,
        master_key.as_ref(),
        master_key_id.as_deref(),
    )
    .await
    {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({
                    "status": "restored",
                    "from": args.from.to_string_lossy(),
                    "target_db": target_db.to_string_lossy(),
                })
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: restore failed: {e}");
            ExitCode::from(13)
        }
    }
}

async fn run_doctor_cmd(args: DoctorArgs) -> ExitCode {
    let data_dir = resolve_data_dir(args.data_dir);
    let database = data_dir.join("llmroller.db");
    if !database.is_file() {
        eprintln!("error: database {} does not exist", database.display());
        return ExitCode::from(10);
    }
    let pool = match open_pool(&database, Role::cli_read_only()).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot open {}: {e}", database.display());
            return ExitCode::from(13);
        }
    };
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check;")
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|_| "error".to_string());
    if integrity != "ok" {
        eprintln!("error: sqlite integrity_check failed: {integrity}");
        return ExitCode::from(13);
    }
    for kind in llmroller_core::provider::ProviderKind::ALL {
        let _ = kind.preset_toml();
    }
    if let (Ok(mk), Ok(mk_id)) = (load_master_key_from_env(), load_master_key_id_from_env()) {
        let entropy = Arc::new(OsEntropy::new());
        let clock = SystemClock::new();
        let state = match load_or_initialize(&pool, &mk_id, &mk, entropy, clock.now_ms()).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: master key check failed: {e}");
                return ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(3));
            }
        };
        if let Err(e) = llmroller_store::audit::verify_audit_log(
            &pool,
            state.audit_hmac_key.expose_secret(),
            None,
            None,
        )
        .await
        {
            eprintln!("error: audit chain verification failed: {e}");
            return ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(8));
        }
    }
    pool.close().await;
    println!(
        "{}",
        serde_json::json!({
            "status": "ok",
            "database": database.to_string_lossy(),
            "integrity": "ok",
            "schema_version": llmroller_store::migrations::SCHEMA_VERSION,
        })
    );
    ExitCode::SUCCESS
}

/// Redacts sensitive header values (`authorization`, `*key*`, `*token*`, `*secret*`, `cookie`)
/// inside a provider `config_json` object before exporting diagnostics (`F086`, `SEC-6`).
#[must_use]
pub(crate) fn redact_provider_config_json(raw: &str) -> serde_json::Value {
    let Ok(mut val) = serde_json::from_str::<serde_json::Value>(raw) else {
        return serde_json::json!({});
    };
    let is_sensitive = |name: &str| {
        let l = name.to_ascii_lowercase();
        l == "authorization"
            || l == "cookie"
            || l.contains("key")
            || l.contains("token")
            || l.contains("secret")
    };
    if let Some(obj) = val.as_object_mut() {
        if let Some(add_headers) = obj.get_mut("add_headers").and_then(|v| v.as_object_mut()) {
            for (k, v) in add_headers.iter_mut() {
                if is_sensitive(k) {
                    *v = serde_json::Value::String("[REDACTED]".to_string());
                }
            }
        }
        if let Some(edits) = obj.get_mut("header_edits").and_then(|v| v.as_array_mut()) {
            for edit in edits.iter_mut() {
                if let Some(eobj) = edit.as_object_mut() {
                    let name_sens = eobj
                        .get("name")
                        .and_then(|n| n.as_str())
                        .is_some_and(is_sensitive);
                    if name_sens && eobj.contains_key("value") {
                        eobj.insert(
                            "value".to_string(),
                            serde_json::Value::String("[REDACTED]".to_string()),
                        );
                    }
                }
            }
        }
    }
    val
}

async fn run_export_diagnostics_cmd(args: ExportDiagnosticsArgs) -> ExitCode {
    use sqlx::Row as _;
    let data_dir = resolve_data_dir(args.data_dir);
    let database = data_dir.join("llmroller.db");
    let Ok(pool) = open_pool(&database, Role::cli_read_only()).await else {
        eprintln!("error: cannot open {}", database.display());
        return ExitCode::from(13);
    };
    let rows = sqlx::query("SELECT id, kind, name, base_url, config_json, enabled FROM providers")
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
    let providers: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let cfg: String = r.try_get("config_json").unwrap_or_default();
            serde_json::json!({
                "id": r.try_get::<String, _>("id").unwrap_or_default(),
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "base_url": r.try_get::<String, _>("base_url").unwrap_or_default(),
                "enabled": r.try_get::<i64, _>("enabled").unwrap_or(0) != 0,
                "config": redact_provider_config_json(&cfg),
            })
        })
        .collect();
    pool.close().await;
    let payload = serde_json::json!({
        "version": version_string(),
        "schema_version": llmroller_store::migrations::SCHEMA_VERSION,
        "providers": providers,
    });
    let serialized = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
    if let Some(out) = args.out {
        if let Err(e) = fs::write(&out, &serialized) {
            eprintln!("error: failed to write diagnostics to {}: {e}", out.display());
            return ExitCode::from(6);
        }
    } else {
        println!("{serialized}");
    }
    ExitCode::SUCCESS
}

async fn run_backup_cmd(args: BackupArgs) -> ExitCode {
    match backup_command(args).await {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

async fn backup_command(args: BackupArgs) -> Result<serde_json::Value, CliFailure> {
    let data_dir = resolve_data_dir(args.data_dir);
    let database = data_dir.join("llmroller.db");
    if !database.exists() {
        return Err(CliFailure::new(
            10,
            format!("database not found: {}", database.display()),
        ));
    }
    let pool = open_pool(&database, Role::cli_read_write())
        .await
        .map_err(|e| CliFailure::new(13, format!("cannot open database: {e}")))?;

    // REL-6: the exported summary identifies the instance's active key, not a
    // possibly stale value inherited from the invoking shell.
    let master_key_id: String =
        sqlx::query_scalar("SELECT active_master_key_id FROM instance_meta WHERE singleton = 1")
            .fetch_optional(&pool)
            .await
            .map_err(|e| CliFailure::new(13, format!("cannot read instance metadata: {e}")))?
            .ok_or_else(|| {
                CliFailure::new(
                    10,
                    format!(
                        "database is not an initialized LLMRoller instance: {}",
                        database.display()
                    ),
                )
            })?;

    let res =
        llmroller_server::backup::run_backup(&pool, &args.out, args.overwrite, &master_key_id)
            .await
            .map_err(|e| match e {
                llmroller_server::backup::BackupError::AlreadyExists
                | llmroller_server::backup::BackupError::Io(_) => CliFailure::new(6, e.to_string()),
                _ => CliFailure::new(13, e.to_string()),
            })?;

    Ok(res)
}

async fn run_healthcheck(args: HealthcheckArgs) -> ExitCode {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(args.timeout_ms))
        .build()
    {
        Ok(c) => c,
        Err(_) => return ExitCode::from(1),
    };

    match client.get(&args.url).send().await {
        Ok(resp) if resp.status().is_success() => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

async fn run_rotation(args: RotateMasterKeyArgs) -> ExitCode {
    match rotation_command(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

async fn rotation_command(args: RotateMasterKeyArgs) -> Result<(), CliFailure> {
    let data_dir = resolve_data_dir(args.data_dir);
    fs::create_dir_all(&data_dir).map_err(|error| {
        CliFailure::new(
            13,
            format!(
                "cannot create data directory {}: {error}",
                data_dir.display()
            ),
        )
    })?;
    let _lock = acquire_lock(&data_dir)?;

    let old_key = read_key_from_env(&args.old_key_env, "old")?;
    let new_key = read_key_from_env(&args.new_key_env, "new")?;

    let database = data_dir.join("llmroller.db");
    let pool = open_pool(&database, Role::Writer).await.map_err(|error| {
        CliFailure::new(13, format!("cannot open {}: {error}", database.display()))
    })?;
    if let Err(error) = migrate(&pool).await {
        let code = u8::try_from(error.exit_code()).unwrap_or(1);
        return Err(CliFailure::new(code, error.to_string()));
    }

    let clock = SystemClock::new();
    let report = rotate_master_key(
        &pool,
        old_key,
        new_key,
        &args.new_key_id,
        Arc::new(OsEntropy::new()),
        clock.now_ms(),
    )
    .await
    .map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(1);
        CliFailure::new(code, error.to_string())
    })?;
    pool.close().await;

    eprintln!(
        "rotation complete: {} rows re-sealed in {} batches{}",
        report.rows_re_sealed,
        report.batches,
        if report.resumed { " (resumed)" } else { "" }
    );
    eprintln!(
        "Update the master key to the replacement value and set LLMROLLER_MASTER_KEY_ID={:?} before starting serve.",
        args.new_key_id
    );
    Ok(())
}

async fn run_reset_admin(args: ResetAdminArgs) -> ExitCode {
    match reset_admin_command(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

async fn reset_admin_command(args: ResetAdminArgs) -> Result<(), CliFailure> {
    let data_dir = resolve_data_dir(args.data_dir);
    fs::create_dir_all(&data_dir).map_err(|error| {
        CliFailure::new(
            13,
            format!(
                "cannot create data directory {}: {error}",
                data_dir.display()
            ),
        )
    })?;
    let _lock = acquire_lock(&data_dir)?;

    let master_key = load_master_key_from_env().map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;
    let master_key_id = load_master_key_id_from_env().map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;

    let database = data_dir.join("llmroller.db");
    let pool = open_pool(&database, Role::cli_read_write())
        .await
        .map_err(|error| {
            CliFailure::new(13, format!("cannot open {}: {error}", database.display()))
        })?;
    if let Err(error) = migrate(&pool).await {
        let code = u8::try_from(error.exit_code()).unwrap_or(1);
        return Err(CliFailure::new(code, error.to_string()));
    }

    let clock = SystemClock::new();
    let entropy = Arc::new(OsEntropy::new());
    let instance_state = load_or_initialize(
        &pool,
        &master_key_id,
        &master_key,
        entropy.clone(),
        clock.now_ms(),
    )
    .await
    .map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;

    let result = reset_admin(
        &pool,
        &args.username,
        entropy.as_ref(),
        instance_state.audit_hmac_key.expose_secret(),
        clock.now_ms(),
    )
    .await
    .map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(13);
        CliFailure::new(code, error.to_string())
    })?;
    pool.close().await;

    eprintln!(
        "temporary password for administrator {:?}: {}",
        args.username,
        result.temporary_password.expose_secret()
    );
    Ok(())
}

async fn run_verify_audit_log(args: VerifyAuditLogArgs) -> ExitCode {
    match verify_audit_log_command(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

async fn verify_audit_log_command(args: VerifyAuditLogArgs) -> Result<(), CliFailure> {
    let data_dir = resolve_data_dir(args.data_dir);
    let master_key = load_master_key_from_env().map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;
    let master_key_id = load_master_key_id_from_env().map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(3);
        CliFailure::new(code, error.to_string())
    })?;

    let database = data_dir.join("llmroller.db");
    if !database.is_file() {
        return Err(CliFailure::new(
            10,
            format!("database {} does not exist", database.display()),
        ));
    }

    let pool = open_pool(&database, Role::cli_read_only())
        .await
        .map_err(|error| {
            CliFailure::new(13, format!("cannot open {}: {error}", database.display()))
        })?;

    let has_meta: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM instance_meta WHERE singleton = 1")
            .fetch_optional(&pool)
            .await
            .map_err(|e| CliFailure::new(13, e.to_string()))?;
    if has_meta.is_none() {
        return Err(CliFailure::new(10, "instance is uninitialized".to_string()));
    }

    let clock = SystemClock::new();
    let entropy = Arc::new(OsEntropy::new());
    let instance_state =
        load_or_initialize(&pool, &master_key_id, &master_key, entropy, clock.now_ms())
            .await
            .map_err(|error| {
                let code = u8::try_from(error.exit_code()).unwrap_or(3);
                CliFailure::new(code, error.to_string())
            })?;

    let report = llmroller_store::audit::verify_audit_log(
        &pool,
        instance_state.audit_hmac_key.expose_secret(),
        args.anchor_seq,
        args.anchor_hash.as_deref(),
    )
    .await
    .map_err(|error| {
        let code = u8::try_from(error.exit_code()).unwrap_or(13);
        CliFailure::new(code, error.to_string())
    })?;
    pool.close().await;

    if args.json {
        let json_output =
            serde_json::to_string(&report).map_err(|e| CliFailure::new(13, e.to_string()))?;
        println!("{json_output}");
    } else {
        eprintln!(
            "audit log verified: {} rows intact (tail seq {}, hash {})",
            report.rows_verified, report.max_seq, report.tail_hash
        );
    }
    Ok(())
}

fn resolve_data_dir(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| env::var_os(DATA_DIR_ENV).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR))
}

fn acquire_lock(data_dir: &Path) -> Result<File, CliFailure> {
    let path = data_dir.join("llmroller.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| CliFailure::new(13, format!("cannot open {}: {error}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(CliFailure::new(
            4,
            format!(
                "{} is held by another process; stop serve before rotating",
                path.display()
            ),
        )),
        Err(TryLockError::Error(error)) => Err(CliFailure::new(
            13,
            format!("cannot lock {}: {error}", path.display()),
        )),
    }
}

fn read_key_from_env(variable: &str, role: &str) -> Result<MasterKey, CliFailure> {
    let encoded = match env::var(variable) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => {
            return Err(CliFailure::new(
                3,
                format!("{role} master key environment variable {variable:?} is not set"),
            ));
        }
        Err(env::VarError::NotUnicode(_)) => {
            return Err(CliFailure::new(
                3,
                format!("{role} master key environment variable {variable:?} is not UTF-8"),
            ));
        }
    };
    MasterKey::from_base64(&encoded).map_err(|error| CliFailure::new(3, error.to_string()))
}

#[derive(Debug)]
struct CliFailure {
    code: u8,
    message: String,
}

impl CliFailure {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;

    #[test]
    #[doc = "Partial: SEC-2"]
    fn rotate_master_key_parses_required_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "rotate-master-key",
            "--old-key-env",
            "OLD_KEY",
            "--new-key-env",
            "NEW_KEY",
            "--new-key-id",
            "mk_next",
            "--data-dir",
            "/tmp/data",
        ])
        .expect("valid CLI");
        match cli.command {
            Command::RotateMasterKey(args) => {
                assert_eq!(args.old_key_env, "OLD_KEY");
                assert_eq!(args.new_key_env, "NEW_KEY");
                assert_eq!(args.new_key_id, "mk_next");
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
            }
            _ => panic!("expected RotateMasterKey"),
        }
    }

    #[test]
    #[doc = "Spec: AUTH-5"]
    fn reset_admin_parses_required_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "reset-admin",
            "--username",
            "admin",
            "--data-dir",
            "/tmp/data",
        ])
        .expect("valid reset-admin CLI");
        match cli.command {
            Command::ResetAdmin(args) => {
                assert_eq!(args.username, "admin");
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
            }
            _ => panic!("expected ResetAdmin"),
        }
    }

    #[test]
    #[doc = "Spec: AUD-1, Spec: SEC-13"]
    fn verify_audit_log_parses_required_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "verify-audit-log",
            "--anchor-seq",
            "100",
            "--anchor-hash",
            "abcdef123456",
            "--data-dir",
            "/tmp/data",
            "--json",
        ])
        .expect("valid verify-audit-log CLI");
        match cli.command {
            Command::VerifyAuditLog(args) => {
                assert_eq!(args.anchor_seq, Some(100));
                assert_eq!(args.anchor_hash.as_deref(), Some("abcdef123456"));
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
                assert!(args.json);
            }
            _ => panic!("expected VerifyAuditLog"),
        }
    }

    #[test]
    #[doc = "Spec: SEC-2"]
    fn generate_master_key_parses_without_arguments() {
        let cli = Cli::try_parse_from(["llmroller", "generate-master-key"]).expect("valid CLI");
        match cli.command {
            Command::GenerateMasterKey => {}
            _ => panic!("expected GenerateMasterKey"),
        }
    }

    #[test]
    #[doc = "Spec: POR-1, Spec: REL-1"]
    fn serve_parses_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "serve",
            "--listen",
            "127.0.0.1:8080",
            "--admin-listen",
            "127.0.0.1:8081",
            "--metrics-listen",
            "127.0.0.1:9090",
            "--data-dir",
            "/tmp/data",
            "--dev-insecure-cookies",
        ])
        .expect("valid serve CLI");
        match cli.command {
            Command::Serve(args) => {
                assert_eq!(args.listen, "127.0.0.1:8080");
                assert_eq!(args.admin_listen, "127.0.0.1:8081");
                assert_eq!(args.metrics_listen, "127.0.0.1:9090");
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
                assert!(args.dev_insecure_cookies);
            }
            _ => panic!("expected Serve"),
        }
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

#[cfg(test)]
mod phase7_tests {
    use super::*;

    #[test]
    #[doc = "Spec: REL-4, LIM-1"]
    fn hydrating_window_preserves_original_bucket_timestamps_for_rolling_expiry() {
        let runtime = llmroller_core::runtime_state::RuntimeState::new();
        let key_id = llmroller_core::ids::UpstreamKeyId::generate();
        let period_ms = 5 * 3_600_000; // 5 hours
        let now_ms = 100 * 3_600_000;
        let old_bucket_ms = now_ms - period_ms + 300_000; // 4h55m ago (expires in 5m)
        let recent_bucket_ms = now_ms - 600_000; // 10m ago

        let mut ring = llmroller_core::windows::RollingRing::new(period_ms);
        ring.record(old_bucket_ms, 4_000_000);
        ring.record(recent_bucket_ms, 2_000_000);

        let window = llmroller_core::snapshot::SnapshotBudgetWindow {
            id: "w-rolling-5h".to_string(),
            scope_kind: "upstream_key".to_string(),
            upstream_key_id: key_id,
            model_pk: None,
            window_kind: "rolling".to_string(),
            period_ms: Some(period_ms),
            anchor_at: None,
            upstream_window_kind: None,
            limit_micro_usd: 10_000_000,
            reserve_pct: 0,
        };
        runtime.hydrate_window_counter_from_ring(&window, Some(&ring), now_ms);
        let counter = runtime.window_counter("w-rolling-5h").expect("counter");
        assert_eq!(counter.local_used(now_ms), 6_000_000);
        // 6 minutes later, the 4h55m-old bucket must have expired, leaving only 2_000_000!
        assert_eq!(counter.local_used(now_ms + 360_000), 2_000_000);
    }

    #[test]
    #[doc = "Spec: SEC-10, OBS-1"]
    fn non_loopback_plaintext_bind_fails_when_require_tls_is_true() {
        assert_eq!(
            llmroller_proxy::metrics::check_data_plane_bind_security("0.0.0.0:8080", false, true),
            Err(2)
        );
        assert_eq!(
            llmroller_proxy::metrics::check_data_plane_bind_security("0.0.0.0:8080", false, false),
            Ok(true)
        );
        assert_eq!(
            llmroller_proxy::metrics::check_data_plane_bind_security("127.0.0.1:8080", false, true),
            Ok(false)
        );
    }

    #[tokio::test]
    #[doc = "Spec: TRC-1, AUD-1, REL-2"]
    async fn traceability_auto_disable_updates_value_json_reloads_snapshot_and_audits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("llmroller.db");
        let pool = open_pool(&db, Role::Writer).await.expect("open pool");
        migrate(&pool).await.expect("migrate");
        let master_key = MasterKey::from_bytes([0x11; 32]);
        load_or_initialize(&pool, "mk_test", &master_key, Arc::new(OsEntropy), 1_000)
            .await
            .expect("init instance");
        seed_defaults(
            &pool,
            Some(llmroller_core::strategy::RoutingStrategy::SmoothWeightedRr),
            1_000,
        )
        .await
        .expect("seed");

        let cfg = llmroller_core::settings::TraceabilityConfig {
            enabled: true,
            match_filters: llmroller_core::settings::TraceabilityMatch::default(),
            retention_days: 7,
            auto_disable_at: Some(5_000),
        };
        let (reloader_tx, mut reloader_rx) = tokio::sync::mpsc::channel(4);
        let disabled = auto_disable_expired_traceability(
            &pool,
            &cfg,
            b"0123456789abcdef0123456789abcdef",
            Some(&reloader_tx),
            6_000,
        )
        .await
        .expect("auto disable");
        assert!(disabled);

        let loaded = llmroller_store::settings::load_settings(&pool)
            .await
            .expect("load settings");
        assert!(!loaded.traceability_config.enabled);
        let notif = reloader_rx.try_recv().expect("reloader notified");
        assert!(
            notif
                .changed_domains
                .contains(&llmroller_core::snapshot::ConfigDomain::Settings)
        );
    }
}
