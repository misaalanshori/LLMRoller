use std::fs;
use std::process::ExitCode;
use std::sync::Arc;

use llmroller_core::seams::{Clock, ExposeSecret, OsEntropy, SystemClock};
use llmroller_store::crypto::{load_master_key_from_env, load_master_key_id_from_env};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};
use llmroller_store::rotation::rotate_master_key;
use llmroller_store::users::reset_admin;

use super::args::{
    BackupArgs, DoctorArgs, ExportDiagnosticsArgs, HealthcheckArgs, ResetAdminArgs, RestoreArgs,
    RotateMasterKeyArgs, VerifyAuditLogArgs, version_string,
};
use super::helpers::{CliFailure, acquire_lock, read_key_from_env, resolve_data_dir};

pub(crate) fn run_generate_master_key() -> ExitCode {
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

pub(crate) async fn run_restore_cmd(args: RestoreArgs) -> ExitCode {
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
    let master_key = match load_master_key_from_env() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(3));
        }
    };
    let master_key_id = match load_master_key_id_from_env() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(3));
        }
    };
    match llmroller_server::backup::restore_backup(
        &args.from,
        &target_db,
        Some(&master_key),
        Some(master_key_id.as_str()),
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

pub(crate) async fn run_doctor_cmd(args: DoctorArgs) -> ExitCode {
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
    let mk = match load_master_key_from_env() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: master key check failed: {e}");
            return ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(3));
        }
    };
    let mk_id = match load_master_key_id_from_env() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: master key id check failed: {e}");
            return ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(3));
        }
    };
    {
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

pub(crate) async fn run_export_diagnostics_cmd(args: ExportDiagnosticsArgs) -> ExitCode {
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
            eprintln!(
                "error: failed to write diagnostics to {}: {e}",
                out.display()
            );
            return ExitCode::from(6);
        }
    } else {
        println!("{serialized}");
    }
    ExitCode::SUCCESS
}

pub(crate) async fn run_backup_cmd(args: BackupArgs) -> ExitCode {
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

pub(crate) async fn run_healthcheck(args: HealthcheckArgs) -> ExitCode {
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

pub(crate) async fn run_rotation(args: RotateMasterKeyArgs) -> ExitCode {
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

pub(crate) async fn run_reset_admin(args: ResetAdminArgs) -> ExitCode {
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

pub(crate) async fn run_verify_audit_log(args: VerifyAuditLogArgs) -> ExitCode {
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
