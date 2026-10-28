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

/// Exit code for "database schema version does not match the binary" (`5. SRDD §5.5.1`).
///
/// `1`..`14` are already assigned in that table (`12` is the writer-panic code), so the
/// mismatch code is `15`.
const EXIT_SCHEMA_MISMATCH: i32 = 15;

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
    let admin_port = admin_listener
        .local_addr()
        .expect("local addr")
        .port();
    drop(admin_listener);
    let metrics_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let metrics_port = metrics_listener
        .local_addr()
        .expect("local addr")
        .port();
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
    assert!(ready, "serve must become ready before the fixture database exists");

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
        code, EXIT_SCHEMA_MISMATCH,
        "a schema mismatch must exit {EXIT_SCHEMA_MISMATCH}; stdout: {stdout} stderr: {stderr}"
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
    assert_eq!(
        meta_rows, 0,
        "doctor must not create instance_meta; it did"
    );
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
    assert_eq!(code, 10, "a missing database must exit 10; stderr: {stderr}");
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
