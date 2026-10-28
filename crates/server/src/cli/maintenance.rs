use std::fs;
use std::process::ExitCode;
use std::sync::Arc;

use llmroller_core::seams::{Clock, ExposeSecret, OsEntropy, SystemClock};
use llmroller_store::crypto::{load_master_key_from_env, load_master_key_id_from_env};
use llmroller_store::instance::{load_instance_state, load_or_initialize};
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

/// Exit code for "the file's `_llmroller_schema.version` is **older** than this binary
/// supports" (`5. SRDD §5.5.1`, `ISSUE-0183`).
///
/// `0..=14` are all assigned in that table -- `14` is the *newer*-file code and `12` is the
/// writer-panic code -- making `15` the first free value. The direction matters: `serve`'s
/// migration path already exits `14` for a newer file, so a single "mismatch" code would
/// have one binary telling an operator two different things about one database.
const EXIT_SCHEMA_OLDER: u8 = 15;

/// Exit code for "the file's `_llmroller_schema.version` is **newer** than this binary
/// supports" (`5. SRDD §5.5.1` row 14).
///
/// Deliberately equal to [`llmroller_store::migrations::MigrationError::SchemaTooNew`]'s
/// code, so `doctor` and `serve` agree on the same database (`ISSUE-0183`).
const EXIT_SCHEMA_NEWER: u8 = 14;

/// The schema version actually recorded in the database file.
///
/// Returns `None` when `_llmroller_schema` has no usable row, which means the file is not a
/// recognised LLMRoller database at all. This is a *read*: `migrations::SCHEMA_VERSION` is
/// what the binary was compiled against and says nothing about the file on disk, which is
/// the distinction `doctor` exists to surface (`ISSUE-0183`).
async fn read_database_schema_version(pool: &sqlx::SqlitePool) -> Option<i64> {
    // The scalar must be `Option<i64>`, not `i64`. `MAX()` over an empty table is SQL NULL,
    // and decoding that into a non-nullable `i64` yields `Some(0)` -- which reads as "this
    // file is at version 0", i.e. a very old database, when in fact there is no version row
    // at all. That is the "unreadable" case, and it must not be mistaken for a drift.
    sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM _llmroller_schema")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .flatten()
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
    // A query failure is not a failed check. Collapsing both into "integrity failed" would
    // send an operator hunting for corruption when SQLite simply refused to run the pragma.
    let integrity: String = match sqlx::query_scalar("PRAGMA integrity_check;")
        .fetch_one(&pool)
        .await
    {
        Ok(value) => value,
        Err(e) => {
            eprintln!("error: sqlite integrity_check could not run: {e}");
            return ExitCode::from(13);
        }
    };
    if integrity != "ok" {
        eprintln!("error: sqlite integrity_check failed: {integrity}");
        return ExitCode::from(13);
    }
    for kind in llmroller_core::provider::ProviderKind::ALL {
        let _ = kind.preset_toml();
    }
    let database_schema_version = read_database_schema_version(&pool).await;
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
        // A diagnostic must never initialise. `load_or_initialize` would create
        // `instance_meta`/`instance_secrets` on exactly the partially-restored database this
        // command is meant to report on, so the read-only loader is used instead
        // (`ISSUE-0183`).
        let entropy = Arc::new(OsEntropy::new());
        let state = match load_instance_state(&pool, &mk_id, &mk, entropy).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: instance state check failed: {e}");
                let code = u8::try_from(e.exit_code()).unwrap_or(3);
                // Report the schema we did read, so a drifted file is not misreported as a
                // bare instance fault.
                if let Some(version) = database_schema_version {
                    eprintln!(
                        "error: database schema version is {version}, this binary supports {}",
                        llmroller_store::migrations::SCHEMA_VERSION
                    );
                }
                return ExitCode::from(code);
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

    // Report the version the *file* declares, not the one this binary was built for. Equal
    // is `ok`; anything else is schema drift, which is the condition this command cannot
    // otherwise detect (`ISSUE-0183`).
    //
    // The three-way split is deliberate. A *newer* file is §5.5.1's row 14, the code
    // `serve`'s `SchemaTooNew` already returns -- one binary must not describe one database
    // two ways. An *older* file is `15`, a drift the table did not previously cover. An
    // unreadable version row is neither: the file is not a recognised LLMRoller database.
    let compiled = llmroller_store::migrations::SCHEMA_VERSION;
    let (status, exit_code) = match database_schema_version {
        Some(version) if version == compiled => ("ok", ExitCode::SUCCESS),
        Some(version) if version > compiled => {
            eprintln!(
                "error: database schema version is {version}, this binary supports {compiled}; \
                 this database was written by a newer build -- run the binary that matches it"
            );
            ("schema_newer", ExitCode::from(EXIT_SCHEMA_NEWER))
        }
        Some(version) => {
            eprintln!(
                "error: database schema version is {version}, this binary supports {compiled}; \
                 run the binary that matches the database, or migrate it"
            );
            ("schema_older", ExitCode::from(EXIT_SCHEMA_OLDER))
        }
        None => {
            eprintln!("error: database has no readable _llmroller_schema version row");
            ("schema_unknown", ExitCode::from(EXIT_SCHEMA_OLDER))
        }
    };
    println!(
        "{}",
        serde_json::json!({
            "status": status,
            "database": database.to_string_lossy(),
            "integrity": "ok",
            "schema_version": database_schema_version,
            "binary_schema_version": compiled,
        })
    );
    exit_code
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
    // Same rule as `doctor`: report what the file says, not what the binary was built for
    // (`ISSUE-0183`).
    let database_schema_version = read_database_schema_version(&pool).await;
    pool.close().await;
    let payload = serde_json::json!({
        "version": version_string(),
        "schema_version": database_schema_version,
        "binary_schema_version": llmroller_store::migrations::SCHEMA_VERSION,
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
