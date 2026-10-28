//! `llmroller doctor` and `export-diagnostics` must report the schema version they actually
//! read, and must never initialise the database they are diagnosing.
//!
//! Requirements: REL-6, SEC-2, OBS-1. Issues: #183.
//!
//! These drive the real binary, because a diagnostic command's output *is* its product. A
//! unit test that asserts a `serde_json::json!` literal would pass while the shipped
//! command printed something else — which is exactly the defect #183 reports.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tempfile::tempdir;

/// Exit code for "the file records an **older** schema than this binary supports".
///
/// `5. SRDD §5.5.1` assigns `14` to a *newer* file (and `migrations::MigrationError`
/// already returns it), so an older file needs its own code: `15`, the first free value
/// (`0..=14` are all assigned, `12` being the writer-panic code).
const EXIT_SCHEMA_OLDER: i32 = 15;

/// Exit code for "the file records a **newer** schema than this binary supports".
///
/// This is `§5.5.1`'s existing row 14, and the code `serve`'s migration path already
/// returns for the same condition. `doctor` must agree with `serve` (`ISSUE-0183`).
const EXIT_SCHEMA_NEWER: i32 = 14;

fn llmroller_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("current exe");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("llmroller");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

/// A master key that exists only so the master-key guard does not fire first.
fn dummy_master_key() -> String {
    BASE64.encode([0x7E_u8; 32])
}

/// Build a real database by running `serve` until it reports ready, then shutting it down.
///
/// Using the shipped binary rather than calling `migrate` directly is deliberate: the test
/// must observe what an operator's database actually looks like, including every migration
/// the build applies.
async fn create_live_database(data_dir: &Path) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    let admin_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let admin_port = admin_listener.local_addr().expect("local addr").port();
    drop(admin_listener);
    let metrics_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let metrics_port = metrics_listener.local_addr().expect("local addr").port();
    drop(metrics_listener);

    let mut child = tokio::process::Command::new(llmroller_binary())
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--admin-listen")
        .arg(format!("127.0.0.1:{admin_port}"))
        .arg("--metrics-listen")
        .arg(format!("127.0.0.1:{metrics_port}"))
        .arg("--dev-insecure-cookies")
        .env("LLMROLLER_MASTER_KEY", dummy_master_key())
        .env("LLMROLLER_MASTER_KEY_ID", "key-doctor-test")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        if let Ok(resp) = client
            .get(format!("http://127.0.0.1:{port}/readyz"))
            .timeout(std::time::Duration::from_millis(500))
            .send()
            .await
            && resp.status().is_success()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
    assert!(
        ready,
        "serve must become ready before the fixture database exists"
    );

    // `serve` holds a flock; it exits on kill, but the lock file lingers harmlessly.
    let lock = data_dir.join("llmroller.lock");
    let _ = std::fs::remove_file(lock);
}

async fn run_cli(args: &[&str], data_dir: &Path) -> (i32, String, String) {
    let output = tokio::process::Command::new(llmroller_binary())
        .args(args)
        .arg("--data-dir")
        .arg(data_dir)
        .env("LLMROLLER_MASTER_KEY", dummy_master_key())
        .env("LLMROLLER_MASTER_KEY_ID", "key-doctor-test")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("run cli");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Overwrite `_llmroller_schema.version` so the file claims an older schema than the build.
///
/// `version` is the primary key, so this is a replace rather than an update.
async fn downgrade_schema_version(data_dir: &Path, to: i64) {
    use sqlx::Connection as _;
    let db = data_dir.join("llmroller.db");
    let mut conn = sqlx::SqliteConnection::connect(&format!("sqlite://{}", db.display()))
        .await
        .expect("open db");
    sqlx::query("DELETE FROM _llmroller_schema")
        .execute(&mut conn)
        .await
        .expect("clear schema rows");
    sqlx::query("INSERT INTO _llmroller_schema (version, checksum) VALUES (?, NULL)")
        .bind(to)
        .execute(&mut conn)
        .await
        .expect("insert downgraded version");
    conn.close().await.expect("close");
}

/// `doctor` must report the version in the **database**, not the one the binary was compiled
/// against (`ISSUE-0183`).
///
/// Pre-fix it prints `SCHEMA_VERSION` from the binary and returns `0` even when the file is
/// older — so it reports `"status": "ok"` on precisely the condition it exists to detect.
#[tokio::test]
#[doc = "Spec: REL-6"]
async fn doctor_reports_the_database_schema_version_not_the_binary_one() {
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();
    create_live_database(data_dir).await;

    // Sanity: an in-sync database reports ok and the version the binary supports.
    let (code, stdout, stderr) = run_cli(&["doctor"], data_dir).await;
    assert_eq!(code, 0, "an in-sync database must exit 0; stderr: {stderr}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("doctor json");
    let compiled = llmroller_store::migrations::SCHEMA_VERSION;
    assert_eq!(
        parsed["schema_version"].as_i64(),
        Some(compiled),
        "in-sync run must report the binary's version"
    );
    assert_eq!(parsed["status"].as_str(), Some("ok"));

    // Downgrade the file, keeping every table so only the version can explain a mismatch.
    downgrade_schema_version(data_dir, compiled - 1).await;

    let (code, stdout, stderr) = run_cli(&["doctor"], data_dir).await;
    assert_eq!(
        code, EXIT_SCHEMA_OLDER,
        "an older file must exit {EXIT_SCHEMA_OLDER}; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "doctor must still emit its JSON report on mismatch"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("doctor json");
    assert_eq!(
        parsed["schema_version"].as_i64(),
        Some(compiled - 1),
        "doctor must report the database's version, not the binary's"
    );
    assert_ne!(
        parsed["status"].as_str(),
        Some("ok"),
        "a schema mismatch must not be reported as ok"
    );
    // The message must name both numbers, or an operator cannot act on it.
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains(&format!("{compiled}")),
        "the report must name the binary's version; got {combined}"
    );
}

/// `doctor` must never initialise the database it is diagnosing (`ISSUE-0183`).
///
/// Hashing out `instance_meta` leaves a database with the full schema but no instance. The
/// old code called `load_or_initialize`, whose first act on such a file is to *create* the
/// row. A diagnostic must report instead.
#[tokio::test]
#[doc = "Spec: SEC-2"]
async fn doctor_does_not_initialize_a_bare_database() {
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();
    create_live_database(data_dir).await;

    use sqlx::Connection as _;
    let db = data_dir.join("llmroller.db");
    let mut conn = sqlx::SqliteConnection::connect(&format!("sqlite://{}", db.display()))
        .await
        .expect("open db");
    sqlx::query("DELETE FROM instance_secrets")
        .execute(&mut conn)
        .await
        .expect("clear secrets");
    sqlx::query("DELETE FROM instance_meta")
        .execute(&mut conn)
        .await
        .expect("clear meta");
    conn.close().await.expect("close");

    let (code, stdout, stderr) = run_cli(&["doctor"], data_dir).await;
    assert_ne!(code, 0, "an uninitialised instance must not report success");
    assert!(
        !stdout.contains("\"status\":\"ok\""),
        "an uninitialised instance must not report ok; stdout: {stdout}"
    );
    assert!(
        stderr.to_ascii_lowercase().contains("instance")
            || stderr.to_ascii_lowercase().contains("initial"),
        "the error must say what is wrong; stderr: {stderr}"
    );

    // The row must still be absent: a diagnostic that wrote it would have masked the fault.
    let mut conn = sqlx::SqliteConnection::connect(&format!("sqlite://{}", db.display()))
        .await
        .expect("reopen db");
    let meta_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_meta")
        .fetch_one(&mut conn)
        .await
        .expect("count meta");
    let secret_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instance_secrets")
        .fetch_one(&mut conn)
        .await
        .expect("count secrets");
    conn.close().await.expect("close");
    assert_eq!(meta_rows, 0, "doctor must not create instance_meta; it did");
    assert_eq!(
        secret_rows, 0,
        "doctor must not create instance_secrets; it did"
    );
}

/// A missing database is still `10` — status-quo pin (`ISSUE-0183`, already correct).
#[tokio::test]
#[doc = "Spec: REL-6"]
async fn doctor_still_exits_10_for_a_missing_file() {
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();
    std::fs::create_dir_all(data_dir).expect("data dir");

    let (code, _stdout, stderr) = run_cli(&["doctor"], data_dir).await;
    assert_eq!(
        code, 10,
        "a missing database must exit 10; stderr: {stderr}"
    );
}

/// `export-diagnostics` has the identical constant and must read the same version.
#[tokio::test]
#[doc = "Spec: REL-6"]
async fn export_diagnostics_reports_the_database_schema_version() {
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();
    create_live_database(data_dir).await;
    let compiled = llmroller_store::migrations::SCHEMA_VERSION;
    downgrade_schema_version(data_dir, compiled - 1).await;

    let (code, stdout, stderr) = run_cli(&["export-diagnostics"], data_dir).await;
    assert_eq!(
        code, 0,
        "export-diagnostics must still emit its payload; stderr: {stderr}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("export diagnostics json");
    assert_eq!(
        parsed["schema_version"].as_i64(),
        Some(compiled - 1),
        "export-diagnostics must report the database's version, not the binary's"
    );
}

// ---------------------------------------------------------------------------
// The three-way split: newer file is 14 (agreeing with `serve`), older is 15.
// ---------------------------------------------------------------------------

/// Raise `_llmroller_schema.version` so the file claims a newer schema than the build.
///
/// `migrate` treats this as `SchemaTooNew`. The supplementary tables are left alone: the
/// version row is the only thing `doctor` and `serve` consult for this decision.
async fn upgrade_schema_version(data_dir: &Path, to: i64) {
    use sqlx::Connection as _;
    let db = data_dir.join("llmroller.db");
    let mut conn = sqlx::SqliteConnection::connect(&format!("sqlite://{}", db.display()))
        .await
        .expect("open db");
    sqlx::query("INSERT INTO _llmroller_schema (version, checksum) VALUES (?, NULL)")
        .bind(to)
        .execute(&mut conn)
        .await
        .expect("insert newer version row");
    conn.close().await.expect("close");
}

/// The direction of the drift decides the exit code (`ISSUE-0183`, `5. SRDD §5.5.1`).
///
/// A *newer* file is `14` — §5.5.1's existing row, and what `serve`'s `SchemaTooNew` path
/// already returns. An *older* file is `15`. Before this test, `doctor` returned `15` for
/// both, so an operator scripting against §5.5.1 read `14` from `serve` and missed
/// `doctor`'s answer for the identical database.
///
/// Table-driven over the two directions so a regression cannot fix one and break the other.
#[tokio::test]
#[doc = "Spec: REL-6"]
async fn doctor_splits_newer_and_older_schema_drift_by_exit_code() {
    let compiled = llmroller_store::migrations::SCHEMA_VERSION;

    // (direction, target version, expected code) — one fixture per case, because the
    // version row is the thing under test and cannot hold two values at once.
    let cases: [(&str, i64, i32); 2] = [
        ("newer", compiled + 1, EXIT_SCHEMA_NEWER),
        ("older", compiled - 1, EXIT_SCHEMA_OLDER),
    ];

    for (direction, version, expected_code) in cases {
        let dir = tempdir().expect("temp dir");
        let data_dir = dir.path();
        create_live_database(data_dir).await;

        if direction == "newer" {
            upgrade_schema_version(data_dir, version).await;
        } else {
            downgrade_schema_version(data_dir, version).await;
        }

        let (code, stdout, stderr) = run_cli(&["doctor"], data_dir).await;
        assert_eq!(
            code, expected_code,
            "a {direction} file must exit {expected_code}; stdout: {stdout} stderr: {stderr}"
        );

        let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("doctor must emit JSON for a {direction} file: {e}"));
        assert_eq!(
            parsed["schema_version"].as_i64(),
            Some(version),
            "doctor must report the {direction} file's own version"
        );
        assert_ne!(
            parsed["status"].as_str(),
            Some("ok"),
            "a {direction} file must not be reported as ok"
        );
    }
}

/// `serve` and `doctor` must agree on a newer database (`ISSUE-0183`).
///
/// This is the cross-command consistency guard. The defect the brief cites is not that `15`
/// was wrong in isolation — it is that one binary told an operator two different things
/// about one file depending on which subcommand they ran.
#[tokio::test]
#[doc = "Spec: REL-6"]
async fn serve_and_doctor_agree_on_a_newer_schema() {
    let compiled = llmroller_store::migrations::SCHEMA_VERSION;
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();
    create_live_database(data_dir).await;
    upgrade_schema_version(data_dir, compiled + 1).await;

    let (doctor_code, _stdout, doctor_stderr) = run_cli(&["doctor"], data_dir).await;

    // `serve` must be shown the same file. It exits before binding, so no listener is
    // needed; a data dir holding a too-new database is enough to reach the migration check.
    let serve = tokio::process::Command::new(llmroller_binary())
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .env("LLMROLLER_MASTER_KEY", dummy_master_key())
        .env("LLMROLLER_MASTER_KEY_ID", "key-doctor-test")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("run serve");
    let serve_code = serve.status.code().unwrap_or(-1);
    let serve_stderr = String::from_utf8_lossy(&serve.stderr).into_owned();

    assert_eq!(
        serve_code, EXIT_SCHEMA_NEWER,
        "serve must exit {EXIT_SCHEMA_NEWER} for a newer database; stderr: {serve_stderr}"
    );
    assert_eq!(
        doctor_code, serve_code,
        "doctor and serve must use the SAME exit code for the same file; \
         doctor said {doctor_code} (stderr: {doctor_stderr}), serve said {serve_code} \
         (stderr: {serve_stderr})"
    );
}
