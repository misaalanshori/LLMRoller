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
use llmroller_server::reloader::{load_initial_snapshot, start_reloader};
use llmroller_server::shutdown::ShutdownSupervisor;
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
    /// Generate a 32-byte cryptographically secure master key (SEC-2).
    GenerateMasterKey,
}

/// Arguments for `llmroller serve`.
#[derive(Debug, Args)]
pub(crate) struct ServeArgs {
    /// Data-plane HTTP listener address.
    #[arg(long, default_value = "0.0.0.0:8080")]
    pub(crate) listen: String,

    /// Control-plane HTTP listener address.
    #[arg(long, default_value = "127.0.0.1:8081")]
    pub(crate) admin_listen: String,

    /// Metrics listener address.
    #[arg(long, default_value = "127.0.0.1:9090")]
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
    let master_key = load_master_key_from_env().map_err(|e| {
        let code = u8::try_from(e.exit_code()).unwrap_or(3);
        CliFailure::new(code, e.to_string())
    })?;
    let master_key_id = load_master_key_id_from_env().unwrap_or_else(|_| "mk_default".to_string());

    // 3. Connect SQLite pool (Role::Writer) and run forward-only migrations
    let database = data_dir.join("llmroller.db");
    let pool = open_pool(&database, Role::Writer)
        .await
        .map_err(|e| CliFailure::new(13, format!("cannot open database: {e}")))?;
    if let Err(error) = migrate(&pool).await {
        let code = u8::try_from(error.exit_code()).unwrap_or(13);
        return Err(CliFailure::new(code, error.to_string()));
    }

    // 4. Load or initialize instance meta and instance secrets
    let clock = Arc::new(SystemClock::new());
    let entropy = Arc::new(OsEntropy::new());
    let _instance_state = load_or_initialize(
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
    seed_defaults(&pool, None, clock.now_ms())
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
    let (_reloader_handle, _reloader_task) = start_reloader(
        pool.clone(),
        shared_snapshot.clone(),
        runtime_state.clone(),
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
    runtime_state.rebuild_window_counters(&shared_snapshot.load());
    for (key_id, ring) in &rebuilt.key_rings {
        let windows = shared_snapshot
            .load()
            .budget_windows
            .get(key_id)
            .cloned()
            .unwrap_or_default();
        for window in windows {
            runtime_state.hydrate_window_counter(
                &window,
                ring.sum_at(clock.now_ms()),
                clock.now_ms(),
            );
        }
    }

    // Seed in-memory key health from persisted health rows.
    if let Ok(rows) = sqlx::query_as::<_, (String, String, Option<i64>, i64)>(
        "SELECT id, health, health_until, health_version FROM upstream_keys",
    )
    .fetch_all(&pool)
    .await
    {
        for (id, health, health_until, _version) in rows {
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
    }

    // 8. Preload unexpired confirmed pins
    let pin_store = Arc::new(llmroller_core::pins::PinStore::new(250_000));
    if let Ok(active_pins) = llmroller_store::pins::load_active_pins(&pool, clock.now_ms()).await {
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

    let trusted_proxies_vec: Vec<String> = if trusted_proxies.trim().is_empty() {
        vec![]
    } else {
        trusted_proxies
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let mut proxy_auth_state = llmroller_proxy::auth::ProxyAuthState::new(
        shared_snapshot.clone(),
        pool.clone(),
        trusted_proxies_vec.clone(),
    );
    proxy_auth_state.is_ready = is_ready.clone();
    proxy_auth_state.pin_store = pin_store.clone();
    proxy_auth_state.writer = Some(writer_handle.clone());
    proxy_auth_state.master_key = Some(master_key.clone());
    let data_router = llmroller_proxy::listeners::data_plane_router(proxy_auth_state);

    let auth_config = llmroller_admin::auth::AuthConfig {
        dev_insecure_cookies: args.dev_insecure_cookies,
        require_tls,
        trusted_proxies: trusted_proxies_vec.clone(),
    };
    let mut auth_state = llmroller_admin::AuthAppState::new(pool.clone(), auth_config);
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = master_key_id.clone();
    let setup_state = llmroller_admin::SetupAppState {
        pool: pool.clone(),
        expected_token: setup_token,
        data_dir: data_dir.clone(),
        clock: clock.clone(),
        entropy: entropy.clone(),
    };
    let mut api_state =
        llmroller_admin::ApiAppState::new(auth_state.clone(), master_key.clone(), master_key_id);
    api_state.writer = Some(writer_handle.clone());
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

    let (stop_tx, mut stop_rx1) = tokio::sync::watch::channel(false);
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
    let _bg_jobs = tokio::spawn(async move {
        let wal_supervisor = llmroller_store::checkpoint::WalCheckpointSupervisor::new(bg_ready);
        let mut stop_rx = stop_rx_bg;
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        while !*stop_rx.borrow_and_update() {
            tokio::select! {
                _ = interval.tick() => {
                    let _ = wal_supervisor.run_checkpoint(&bg_pool, 0).await;
                    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
                    if let Ok(mut tx) = bg_pool.begin().await {
                        let _ = llmroller_store::retention::sweep_expired_web_sessions(&mut tx, now_ms).await;
                        let _ = llmroller_store::retention::sweep_expired_response_links(&mut tx, now_ms).await;
                        let _ = tx.commit().await;
                    }
                }
                _ = stop_rx.changed() => break,
            }
        }
    });

    wait_for_shutdown_signal().await;
    eprintln!("Shutdown signal received; initiating graceful drain...");

    let shutdown_sup = ShutdownSupervisor::new(
        is_ready.clone(),
        open_streams.clone(),
        shutdown_notify.clone(),
        pool.clone(),
        Duration::from_millis(500),
        Duration::from_secs(5),
    );
    let exit_code = shutdown_sup.execute_graceful_shutdown().await;

    let _ = stop_tx.send(true);
    let _ = tokio::join!(data_server, admin_server, metrics_server);

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
    let master_key_id = load_master_key_id_from_env().unwrap_or_else(|_| "key-0".to_string());
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
    let pool = open_pool(&database, Role::cli_read_only())
        .await
        .map_err(|error| {
            CliFailure::new(13, format!("cannot open {}: {error}", database.display()))
        })?;

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
