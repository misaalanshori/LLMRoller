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

/// `settings` for the export.
///
/// Included verbatim: the closed 14-key registry holds routing policy, retention windows and
/// the traceability config — none of which is a credential, and all of which are the first
/// thing a support engineer needs to reproduce a routing complaint. The registry is closed
/// (`ALL_SETTINGS_KEYS`), so this cannot grow a secret without someone adding a secret to a
/// documented list first.
async fn export_settings_section(pool: &sqlx::SqlitePool) -> Vec<serde_json::Value> {
    use sqlx::Row as _;
    let rows = sqlx::query("SELECT key, value_json, updated_at FROM settings ORDER BY key")
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            let raw: String = row.try_get("value_json").unwrap_or_default();
            // `value_json` is JSON *text* in the column. Emitting it as a string would force an
            // operator to double-decode; parse it so the export is shaped like the config it
            // describes. A row that will not parse becomes a visible error object, matching the
            // provider-config rule, rather than a silently-stringified blob.
            let value = serde_json::from_str::<serde_json::Value>(&raw)
                .unwrap_or_else(|_| serde_json::json!({"error": "value_json is not valid JSON"}));
            serde_json::json!({
                "key": row.try_get::<String, _>("key").unwrap_or_default(),
                "value": value,
                "updated_at": row.try_get::<i64, _>("updated_at").unwrap_or_default(),
            })
        })
        .collect()
}

/// `models` for the export.
///
/// Explicitly column-named, though none of these columns is secret: naming them means a later
/// schema addition cannot silently widen the export, which is the same discipline the
/// `upstream_keys` section needs for a much better reason.
async fn export_models_section(pool: &sqlx::SqlitePool) -> Vec<serde_json::Value> {
    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT model_pk, canonical_id, display_name, exposed, distribution, routing_strategy, \
         merge_state, origin, created_by_user_id, context_window \
         FROM models ORDER BY canonical_id",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            serde_json::json!({
                "model_pk": row.try_get::<String, _>("model_pk").unwrap_or_default(),
                "canonical_id": row.try_get::<String, _>("canonical_id").unwrap_or_default(),
                "display_name": row.try_get::<Option<String>, _>("display_name").unwrap_or_default(),
                "exposed": row.try_get::<Option<i64>, _>("exposed").unwrap_or_default(),
                "distribution": row.try_get::<String, _>("distribution").unwrap_or_default(),
                "routing_strategy": row.try_get::<Option<String>, _>("routing_strategy").unwrap_or_default(),
                "merge_state": row.try_get::<String, _>("merge_state").unwrap_or_default(),
                "origin": row.try_get::<String, _>("origin").unwrap_or_default(),
                "created_by_user_id": row.try_get::<Option<String>, _>("created_by_user_id").unwrap_or_default(),
                "context_window": row.try_get::<Option<i64>, _>("context_window").unwrap_or_default(),
            })
        })
        .collect()
}

/// `upstream_keys` for the export — the one section with a real leak risk.
///
/// **Columns included**, each because it identifies a key without revealing it:
///
/// | Column | Why it is safe |
/// | --- | --- |
/// | `id` | Opaque ULID-shaped identifier; already shown in the UI. |
/// | `provider_id` | Foreign key; needed to correlate the key with its provider. |
/// | `owner_user_id` | Foreign key; a user id, not a credential. |
/// | `label` | Operator-chosen display name. |
/// | `master_key_id` | *Which* master key sealed the secret, not the secret. Essential for diagnosing a rotation fault. |
/// | `secret_last4` | The four characters the admin UI already displays to identify a key. |
/// | `enabled`, `priority`, `strict_tier` | Routing configuration. |
/// | `health`, `health_until`, `health_version` | Health state; the main reason to export this table. |
/// | `last_error` | The last failure text. Scanned by `SEC-6`'s scrubber before storage; not a credential. |
/// | `created_at` | Timestamp. |
///
/// **Column omitted, with the reason:**
///
/// | Column | Why it is omitted |
/// | --- | --- |
/// | `secret_enc` | The sealed credential itself. Sealed is not safe to export — an export plus a leaked master key is a full credential compromise, and the export is meant to be emailable. |
///
/// The column list is written out rather than `SELECT *` so a future schema addition — a
/// `refresh_token`, a `client_cert` — cannot silently join the export. That is the whole point
/// of naming them.
async fn export_upstream_keys_section(pool: &sqlx::SqlitePool) -> Vec<serde_json::Value> {
    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT id, provider_id, owner_user_id, label, master_key_id, secret_last4, enabled, \
         priority, strict_tier, health, health_until, health_version, last_error, created_at \
         FROM upstream_keys ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.try_get::<String, _>("id").unwrap_or_default(),
                "provider_id": row.try_get::<String, _>("provider_id").unwrap_or_default(),
                "owner_user_id": row.try_get::<Option<String>, _>("owner_user_id").unwrap_or_default(),
                "label": row.try_get::<String, _>("label").unwrap_or_default(),
                "master_key_id": row.try_get::<String, _>("master_key_id").unwrap_or_default(),
                "secret_last4": row.try_get::<String, _>("secret_last4").unwrap_or_default(),
                "enabled": row.try_get::<i64, _>("enabled").unwrap_or(0) != 0,
                "priority": row.try_get::<i64, _>("priority").unwrap_or_default(),
                "strict_tier": row.try_get::<i64, _>("strict_tier").unwrap_or_default(),
                "health": row.try_get::<String, _>("health").unwrap_or_default(),
                "health_until": row.try_get::<Option<i64>, _>("health_until").unwrap_or_default(),
                "health_version": row.try_get::<i64, _>("health_version").unwrap_or_default(),
                "last_error": row.try_get::<Option<String>, _>("last_error").unwrap_or_default(),
                "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
            })
        })
        .collect()
}

/// `budget_windows` for the export.
///
/// Limits and their scope, no secrets: a window bound to an `upstream_key_id` is identified by
/// that same opaque id, never by the key.
async fn export_budget_windows_section(pool: &sqlx::SqlitePool) -> Vec<serde_json::Value> {
    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT id, scope_kind, upstream_key_id, model_pk, window_kind, period_ms, anchor_at, \
         upstream_window_kind, limit_micro_usd, reserve_pct FROM budget_windows ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.try_get::<String, _>("id").unwrap_or_default(),
                "scope_kind": row.try_get::<String, _>("scope_kind").unwrap_or_default(),
                "upstream_key_id": row.try_get::<String, _>("upstream_key_id").unwrap_or_default(),
                "model_pk": row.try_get::<Option<String>, _>("model_pk").unwrap_or_default(),
                "window_kind": row.try_get::<String, _>("window_kind").unwrap_or_default(),
                "period_ms": row.try_get::<Option<i64>, _>("period_ms").unwrap_or_default(),
                "anchor_at": row.try_get::<Option<i64>, _>("anchor_at").unwrap_or_default(),
                "upstream_window_kind": row.try_get::<Option<String>, _>("upstream_window_kind").unwrap_or_default(),
                "limit_micro_usd": row.try_get::<i64, _>("limit_micro_usd").unwrap_or_default(),
                "reserve_pct": row.try_get::<i64, _>("reserve_pct").unwrap_or_default(),
            })
        })
        .collect()
}

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

/// Replacement text for a redacted value.
///
/// Visible rather than empty so an operator can tell "this provider has no OAuth config" from
/// "this provider has OAuth config that was removed here" — the same rule the log-detail drawer
/// applies to an absent value.
const REDACTED_VALUE: &str = "[REDACTED]";

/// Substrings that mark a JSON **key** as holding a credential (`SEC-6`, `F-086`).
///
/// This is an explicit vocabulary rather than three broad substrings, because the previous
/// `name.contains("key") || name.contains("token") || name.contains("secret")` was both
/// over- and under-inclusive, and the under-inclusive half is the leak:
///
/// - **Added** (previously missed, all genuine credential carriers): `password`, `passphrase`,
///   `credential`, `bearer`, `signature`, `private`, `passwd`, `auth`.
/// - **Deliberately dropped**: the bare `key` substring. It redacted `key_id`, `public_key`,
///   `keys_per_second` and `monkey` — none of which is a secret, and redacting `key_id` in
///   particular makes the export *less* useful for its main job, correlating a key you can see
///   with a failure you are debugging. `api_key` and `private_key` are covered by their own
///   entries, so nothing secret is lost by dropping the bare form.
///
/// `auth` is included as a whole-word-ish substring because operator configs use `auth`,
/// `auth_token` and `authorization` interchangeably. `key_id` cannot match it.
///
/// Not redacted, on purpose, and each is pinned by `redaction_keeps_non_secret_identifiers`:
/// `*_id` (identifiers), `public_key` (public by definition), `keyboard*` (UI config),
/// `keys_per_second` / `models_per_minute` (rate limits), `max_tokens` (**the model's output
/// budget**, not a credential — the bare `token` substring used to redact this, which silently
/// corrupted an interesting diagnostic field).
const SENSITIVE_KEY_SUBSTRINGS: [&str; 12] = [
    "token",
    "secret",
    "password",
    "passwd",
    "passphrase",
    "credential",
    "authorization",
    "cookie",
    "bearer",
    "signature",
    "private",
    "api_key",
];

/// Whole-name matches that the substring list above cannot express without over-matching.
///
/// `auth` is here rather than in the substring list: as a substring it would redact
/// `authorization` (fine) but also an `oauth_provider_name`-style key (not fine). Matched as an
/// exact name or a `_`-delimited segment instead.
const SENSITIVE_KEY_SEGMENTS: [&str; 2] = ["auth", "key"];

/// Whether a JSON key names a credential (`SEC-6`).
///
/// Two tests, in this order:
///
/// 1. **Non-secret suffixes win by fiat.** `key_id`, `public_key`, `max_tokens` and
///    `keys_per_second` are checked *first* and return `false`. This is what keeps the
///    matcher from being over-inclusive, and over-inclusion is a real regression here: a fix
///    that redacts everything containing `key` makes the export useless and reads as success.
/// 2. **Substring and segment matches**, per the constants above.
///
/// Order matters and is deliberate: `max_tokens` contains `token`, so the allow-first check has
/// to come before the substring scan or the model's output budget gets redacted.
fn is_sensitive_key(name: &str) -> bool {
    // Hyphens and dots are header-name and JSON-path separators (`x-api-key`, `$.api_key`), and
    // treating them as ordinary characters made `x-api-key` invisible to the `api_key` entry.
    // Normalizing to `_` first is what lets one vocabulary serve header names, config keys and
    // edit paths.
    let lowered = name.to_ascii_lowercase().replace(['-', '.'], "_");

    // (1) Non-secret names that a substring rule would otherwise swallow.
    const NOT_SECRET: [&str; 7] = [
        "key_id",
        "public_key",
        "max_tokens",
        "token_count",
        "keys_per_second",
        "keyboard_shortcut",
        "id",
    ];
    if NOT_SECRET.contains(&lowered.as_str()) {
        return false;
    }
    // `*_id` suffixes are identifiers in this schema by convention (`provider_id`, `model_pk`,
    // `key_id`), so redacting them would remove every correlation handle in the export.
    if lowered.ends_with("_id") || lowered == "id" {
        return false;
    }

    // (2) The deliberate vocabulary.
    if SENSITIVE_KEY_SUBSTRINGS
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        return true;
    }
    // `key` and `auth` only as whole `_`-delimited segments, so `api_key`/`auth_token`/
    // `x_api_key` match while `monkey` and `keys_per_second` do not.
    lowered
        .split('_')
        .any(|segment| SENSITIVE_KEY_SEGMENTS.contains(&segment))
}

/// Field names whose **value** names something the surrounding object then supplies a value for.
///
/// `header_edits` is `[{op, name, value}]` and `body_edits` is `[{op, path, value, from}]`: the
/// credential is in `value`, and what makes it a credential is the sibling `name` or `path`.
/// A key-name-only walker cannot see this — `value` is not a sensitive name — which is why the
/// old implementation needed a bespoke `header_edits` branch and why a generic rewrite would
/// have silently dropped the protection.
const DESCRIBING_FIELDS: [&str; 3] = ["name", "path", "header"];

/// Fields inside such an object that hold the credential the describing field refers to.
const DESCRIBED_SECRET_FIELDS: [&str; 2] = ["value", "from"];

/// Whether an object describes a sensitive target via a sibling field.
///
/// Deliberately not limited to `header_edits`: `body_edits` has the same shape with `path`, and
/// an operator writing `{"op":"set","path":"$.api_key","value":"…"}` is doing exactly the same
/// thing. Matching the *shape* rather than the parent key is what makes this survive a new
/// edit type.
fn describes_a_sensitive_target(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    DESCRIBING_FIELDS.iter().any(|field| {
        map.get(*field)
            .and_then(serde_json::Value::as_str)
            .is_some_and(is_sensitive_key)
    })
}

/// Redact every credential in a provider `config_json` before exporting diagnostics.
///
/// Walks the **whole** tree depth-first, over objects *and* arrays. The previous implementation
/// inspected exactly two hardcoded paths (`add_headers`, `header_edits`), which cannot be
/// complete: provider config is operator- and plugin-shaped, so there is no enumerable set of
/// paths a secret can occupy (`ISSUE-0184`).
///
/// Rules, and why each:
///
/// - **An object entry whose key matches** has its value replaced by [`REDACTED_VALUE`],
///   whatever the value's type. Replacing rather than truncating: a prefix of an `sk-` key still
///   identifies its scope.
/// - **Untouched keys still have their values walked.** `{"oauth": {...}}` redacts
///   `refresh_token` inside, and `{"headers": [{...}]}` reaches the array elements. This is the
///   half a depth-limited walker gets wrong.
/// - **Arrays recurse into every element**, including nested arrays.
///
/// An unparseable config returns `{"error": "..."}` rather than `{}`. `{}` reads as "this
/// provider has no configuration", which is a different and false statement about an
/// **unreadable** one — the invented-value class. The error names the cause so an operator
/// knows to fix the row rather than wonder where the config went.
#[must_use]
pub(crate) fn redact_provider_config_json(raw: &str) -> serde_json::Value {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return serde_json::json!({
            "error": "provider config_json is not valid JSON; the stored value could not be \
                      redacted or exported",
        });
    };
    redact_value_in_place(&mut value);
    value
}

/// Depth-first redaction of one JSON value, recursing through objects and arrays.
fn redact_value_in_place(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            // Collect first, mutate after: replacing a value while iterating forces a second
            // pass anyway, and a collected list keeps the mutation in one place.
            let keys_to_redact: Vec<String> = map
                .keys()
                .filter(|key| is_sensitive_key(key))
                .cloned()
                .collect();
            for key in keys_to_redact {
                map.insert(key, serde_json::Value::String(REDACTED_VALUE.to_string()));
            }
            // The structural rule: `{name|path|header: <sensitive>, value: …}` names its secret in
            // one field and carries it in another, so no key-name test on `value` can find it.
            if describes_a_sensitive_target(map) {
                for field in DESCRIBED_SECRET_FIELDS {
                    if map.contains_key(field) {
                        map.insert(
                            field.to_string(),
                            serde_json::Value::String(REDACTED_VALUE.to_string()),
                        );
                    }
                }
            }
            // Walk the survivors' values. A redacted entry is now a string and its recursion is
            // a no-op, so this is safe to do unconditionally and keeps the code single-path.
            for value in map.values_mut() {
                redact_value_in_place(value);
            }
        }
        serde_json::Value::Array(elements) => {
            for element in elements {
                redact_value_in_place(element);
            }
        }
        // Scalars carry no keys, so there is nothing to match on. This arm is the walker's
        // termination condition.
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
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

    // The remaining sections the issue asks for. Each one is aggregate configuration and
    // accounting metadata, none of it secret; `upstream_keys` is the only section with a leak
    // risk and it is column-named below (`ISSUE-0184`).
    let settings = export_settings_section(&pool).await;
    let models = export_models_section(&pool).await;
    let upstream_keys = export_upstream_keys_section(&pool).await;
    let budget_windows = export_budget_windows_section(&pool).await;

    // Same rule as `doctor`: report what the file says, not what the binary was built for
    // (`ISSUE-0183`).
    let database_schema_version = read_database_schema_version(&pool).await;
    pool.close().await;
    let payload = serde_json::json!({
        "version": version_string(),
        "schema_version": database_schema_version,
        "binary_schema_version": llmroller_store::migrations::SCHEMA_VERSION,
        "providers": providers,
        "settings": settings,
        "models": models,
        "upstream_keys": upstream_keys,
        "budget_windows": budget_windows,
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

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod maintenance_tests;
