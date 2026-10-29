//! `doctor` on a corrupted audit chain, and `export-diagnostics` as a *leak* test
//! (`ISSUE-0184`, `ISSUE-0210`).
//!
//! # Why these drive the real binary
//!
//! Both commands' output **is** their product. A unit test asserting on a
//! `serde_json::json!` literal would pass while the shipped command printed something else —
//! and for the export, "what the shipped command printed" is precisely what gets attached to a
//! support ticket.
//!
//! The redaction assertion is deliberately `!stdout.contains(MARKER)` over the **whole**
//! output rather than over a field somebody remembered to check. A leak in a section nobody
//! thought to look at is the failure mode being guarded: the old redactor looked at exactly two
//! paths, so a test that checked those two paths would have agreed with it.
//!
//! ## Which #210 cases are *not* here
//!
//! The other three cases the issue lists (uninitialised DB → exit 10, healthy → exit 0, and
//! the schema-drift split) already exist in `cli_doctor_schema_version.rs` from cycle 35 and
//! are verified there. Only the two genuinely missing cases are added: the corrupted chain,
//! and redaction.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tempfile::tempdir;

/// The marker planted at every placement. Obviously fake — never shaped like a real credential.
const MARKER: &str = "LEAK_MARKER_12345";

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

fn dummy_master_key() -> String {
    BASE64.encode([0x6B_u8; 32])
}

const MASTER_KEY_ID: &str = "key-export-test";

/// Build a real database by running `serve` until ready, then stopping it.
///
/// Using the shipped binary rather than calling `migrate` directly keeps the fixture exactly
/// what an operator has, including every migration this build applies.
async fn create_live_database(data_dir: &Path) {
    let mut listener_ports = Vec::new();
    for _ in 0..3 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        listener_ports.push(listener.local_addr().expect("local addr").port());
        drop(listener);
    }
    let [data_port, admin_port, metrics_port] =
        [listener_ports[0], listener_ports[1], listener_ports[2]];

    let mut child = tokio::process::Command::new(llmroller_binary())
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--listen")
        .arg(format!("127.0.0.1:{data_port}"))
        .arg("--admin-listen")
        .arg(format!("127.0.0.1:{admin_port}"))
        .arg("--metrics-listen")
        .arg(format!("127.0.0.1:{metrics_port}"))
        .arg("--dev-insecure-cookies")
        .env("LLMROLLER_MASTER_KEY", dummy_master_key())
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        if let Ok(resp) = client
            .get(format!("http://127.0.0.1:{data_port}/readyz"))
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
    assert!(ready, "serve must become ready before the fixture exists");

    let _ = std::fs::remove_file(data_dir.join("llmroller.lock"));
}

async fn run_cli(args: &[&str], data_dir: &Path) -> (i32, String, String) {
    let output = tokio::process::Command::new(llmroller_binary())
        .args(args)
        .arg("--data-dir")
        .arg(data_dir)
        .env("LLMROLLER_MASTER_KEY", dummy_master_key())
        .env("LLMROLLER_MASTER_KEY_ID", MASTER_KEY_ID)
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

// ---------------------------------------------------------------------------
// ISSUE-0210 gap 1 — a corrupted audit chain
// ---------------------------------------------------------------------------

/// `doctor` must exit `8` when an `audit_log` row has been tampered with (`ISSUE-0210`).
///
/// This is the case where `doctor`'s value is highest: a healthy-chain test proves almost
/// nothing, whereas a tampered row producing `8` is the whole reason the command verifies the
/// chain at all. Nothing tested it before — `run_doctor_cmd`'s verification branch was
/// previously only exercised on an intact chain.
///
/// The tamper is a one-character change to a row's `action`, which invalidates that row's
/// HMAC and every subsequent `prev_hash` link, i.e. exactly the shape a hand-edited database
/// takes.
#[tokio::test]
#[doc = "Spec: AUD-1, SEC-13, REL-6 — ISSUE-0210"]
async fn doctor_exits_8_on_a_corrupted_audit_chain() {
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();
    create_live_database(data_dir).await;

    // Seed audit rows through the real append path so the chain has real HMACs.
    let pool = llmroller_store::pool::open_pool(
        &data_dir.join("llmroller.db"),
        llmroller_store::pool::Role::Writer,
    )
    .await
    .expect("open pool");
    let master_key =
        llmroller_store::crypto::MasterKey::from_base64(&dummy_master_key()).expect("master key");
    let instance_state = llmroller_store::instance::load_or_initialize(
        &pool,
        MASTER_KEY_ID,
        &master_key,
        std::sync::Arc::new(llmroller_core::seams::OsEntropy::new()),
        1_700_000_000_000,
    )
    .await
    .expect("load instance state");
    {
        use llmroller_core::seams::ExposeSecret as _;
        use sqlx::Connection as _;
        let mut connection = pool.acquire().await.expect("acquire");
        let mut transaction = connection.begin().await.expect("begin");
        for index in 0..3 {
            let action = format!("doctor.chain.seed.{index}");
            llmroller_store::audit::append(
                &mut transaction,
                instance_state.audit_hmac_key.expose_secret(),
                llmroller_store::audit::AuditEntry {
                    id: &action,
                    ts: 1_700_000_000_000 + index,
                    actor_user_id: None,
                    ip_addr: None,
                    action: &action,
                    target_type: "instance",
                    target_id: None,
                    detail_json: None,
                },
            )
            .await
            .expect("append");
        }
        transaction.commit().await.expect("commit");
    }

    // Sanity: an intact chain must exit 0, or the tamper below would prove nothing.
    pool.close().await;
    let (code, _stdout, stderr) = run_cli(&["doctor"], data_dir).await;
    assert_eq!(
        code, 0,
        "an intact chain must exit 0 before tampering; stderr: {stderr}"
    );

    // Tamper: change one row's action, leaving its stored hash stale.
    let pool = llmroller_store::pool::open_pool(
        &data_dir.join("llmroller.db"),
        llmroller_store::pool::Role::Writer,
    )
    .await
    .expect("open pool");
    sqlx::query("UPDATE audit_log SET action = 'tampered' WHERE seq = 2")
        .execute(&pool)
        .await
        .expect("tamper row 2");
    pool.close().await;

    let (code, _stdout, stderr) = run_cli(&["doctor"], data_dir).await;
    assert_eq!(
        code, 8,
        "a corrupted audit chain must exit 8 (SEC-13); stderr: {stderr}"
    );
    assert!(
        stderr.contains("audit chain"),
        "the failure must name the audit chain, not fail opaquely; stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// ISSUE-0184 / ISSUE-0210 gap 2 — the export must not leak
// ---------------------------------------------------------------------------

/// One provider whose `config_json` carries [`MARKER`] at every placement the old redactor
/// could miss, plus the two it did handle.
///
/// The four placements map one-to-one onto the failure modes: `add_headers` and `header_edits`
/// were handled; the top-level key, the nested object, the array element, and the
/// array-inside-object were not.
fn leaky_config_json() -> String {
    serde_json::json!({
        // (1) A top-level key the vocabulary must catch.
        "api_key": MARKER,
        // (2) The path the old redactor handled, kept so a "fix" that drops it is caught.
        "add_headers": { "x-api-key": MARKER, "user-agent": "llmroller/1" },
        // (3) The other path it handled.
        "header_edits": [
            { "op": "set", "name": "authorization", "value": MARKER },
            { "op": "set", "name": "user-agent", "value": "llmroller/1" }
        ],
        // (4) Nested inside an object — the leak the issue names.
        "oauth": {
            "refresh_token": MARKER,
            "client_secret": MARKER,
            "consent_url": "https://example.invalid/oauth"
        },
        // (5) Nested inside an array, and inside an object inside an array.
        "webhooks": [
            { "signing_key": MARKER },
            { "retry": { "passphrase": MARKER } }
        ],
        // (6) A non-secret that must survive, so the test distinguishes redaction from
        //     wholesale deletion.
        "preset_id": "openrouter",
        "key_id": "key_visible_123"
    })
    .to_string()
}

/// The shipped `export-diagnostics` must not reveal a nested provider secret (`ISSUE-0184`).
///
/// Asserts on the **whole** stdout string, because a leak in a section nobody thought to check
/// is exactly the failure mode. A field-by-field assertion would have been written to match
/// whatever the implementation did, which is how the two-path redactor survived review.
#[tokio::test]
#[doc = "Spec: SEC-6, F-086 — ISSUE-0184, ISSUE-0210"]
async fn export_diagnostics_reveals_no_secret_from_a_nested_provider_config() {
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();
    create_live_database(data_dir).await;

    let pool = llmroller_store::pool::open_pool(
        &data_dir.join("llmroller.db"),
        llmroller_store::pool::Role::Writer,
    )
    .await
    .expect("open pool");
    let config = leaky_config_json();
    sqlx::query(
        "INSERT INTO providers (id, ownership_scope, owner_user_id, kind, name, base_url, \
         config_json, created_at) VALUES (?, 'global', NULL, 'openrouter', ?, ?, ?, ?)",
    )
    .bind("prov-leak-test")
    .bind("Leak Test Provider")
    .bind("https://example.invalid/v1")
    .bind(&config)
    .bind(1_700_000_000_000_i64)
    .execute(&pool)
    .await
    .expect("insert provider");
    pool.close().await;

    let (code, stdout, stderr) = run_cli(&["export-diagnostics"], data_dir).await;
    assert_eq!(code, 0, "export must succeed; stderr: {stderr}");

    // (a) The marker appears nowhere at all — not in a field, not in a key, not in an error.
    assert!(
        !stdout.contains(MARKER),
        "the export leaked a provider secret. Output:\n{stdout}"
    );

    // (b) A second, narrower assertion for a different failure: the token's *value* must not
    //     appear even if some future code path re-serializes it under a key the vocabulary does
    //     not recognise.
    assert!(
        !stdout.contains(&format!("refresh_token\": \"{MARKER}")),
        "the nested token value leaked:\n{stdout}"
    );

    // (c) Redaction, not deletion: the non-secret parts must still be exported, or the export
    //     stops being useful and an over-broad "fix" would read as a success.
    assert!(
        stdout.contains("key_visible_123"),
        "non-secret identifiers must survive the export:\n{stdout}"
    );
    assert!(
        stdout.contains("openrouter"),
        "non-secret provider metadata must survive:\n{stdout}"
    );
    assert!(
        stdout.contains("[REDACTED]"),
        "the redaction marker must be visible so an operator can tell a secret was removed:\n{stdout}"
    );

    // (d) And it is still valid JSON with the provider present.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("export json");
    assert_eq!(
        parsed["providers"][0]["id"].as_str(),
        Some("prov-leak-test"),
        "the provider must be present in the export"
    );
}

/// The export must not carry an upstream key's ciphertext (`ISSUE-0184`, `ISSUE-0210`).
///
/// `upstream_keys.secret_enc` is the sealed credential. Even sealed, exporting it is a
/// credential-handling incident waiting for a master key to leak alongside it — and the export
/// is meant to be emailable.
///
/// `secret_last4` is included deliberately: it is the value the UI already shows to identify a
/// key, it is four characters, and it is what makes the export useful for correlating a key
/// with a failure. That choice is stated in the report; the assertion here pins the boundary
/// by planting a *distinctive* ciphertext that must not appear.
#[tokio::test]
#[doc = "Spec: SEC-6 — ISSUE-0184, ISSUE-0210"]
async fn export_diagnostics_omits_upstream_key_ciphertext() {
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();
    create_live_database(data_dir).await;

    let pool = llmroller_store::pool::open_pool(
        &data_dir.join("llmroller.db"),
        llmroller_store::pool::Role::Writer,
    )
    .await
    .expect("open pool");
    sqlx::query(
        "INSERT INTO providers (id, ownership_scope, owner_user_id, kind, name, base_url, \
         config_json, created_at) VALUES (?, 'global', NULL, 'openrouter', ?, ?, '{}', ?)",
    )
    .bind("prov-cipher-test")
    .bind("Cipher Test Provider")
    .bind("https://example.invalid/v1")
    .bind(1_700_000_000_000_i64)
    .execute(&pool)
    .await
    .expect("insert provider");

    // The sealed blob is a distinctive byte string, so any appearance in the export is
    // unambiguous. A real `secret_enc` is binary; this stands in for it.
    const CIPHERTEXT_MARKER: &str = "CIPHERTEXT_MARKER_67890";
    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, owner_user_id, label, secret_enc, \
         master_key_id, secret_last4, created_at) \
         VALUES (?, ?, NULL, ?, ?, ?, ?, ?)",
    )
    .bind("uk-cipher-test")
    .bind("prov-cipher-test")
    .bind("prod key")
    .bind(CIPHERTEXT_MARKER.as_bytes().to_vec())
    .bind(MASTER_KEY_ID)
    .bind("abcd")
    .bind(1_700_000_000_000_i64)
    .execute(&pool)
    .await
    .expect("insert upstream key");
    pool.close().await;

    let (code, stdout, stderr) = run_cli(&["export-diagnostics"], data_dir).await;
    assert_eq!(code, 0, "export must succeed; stderr: {stderr}");

    assert!(
        !stdout.contains(CIPHERTEXT_MARKER),
        "the export leaked an upstream key's sealed credential. Output:\n{stdout}"
    );
    // Belt and braces: the column name itself must not appear either, so a future
    // `SELECT *`-shaped refactor cannot reintroduce the leak by serializing the whole row.
    assert!(
        !stdout.contains("secret_enc"),
        "the ciphertext column must not be present in the export at all:\n{stdout}"
    );

    // The key itself must be representable, or the section is useless: label, last4 and health
    // are what an operator needs to correlate a failure with the key that caused it.
    assert!(
        stdout.contains("uk-cipher-test"),
        "the key must be identifiable in the export:\n{stdout}"
    );
    assert!(
        stdout.contains("prod key"),
        "the key label must be exported:\n{stdout}"
    );
    assert!(
        stdout.contains("abcd"),
        "the last-four hint must be exported so a key can be identified:\n{stdout}"
    );
}

/// The export must carry the non-secret sections the issue asks for (`ISSUE-0184`).
///
/// Without this, deleting the sections would make the two leak tests above pass trivially.
#[tokio::test]
#[doc = "Spec: SEC-6, OBS-1 — ISSUE-0184"]
async fn export_diagnostics_includes_settings_models_and_budget_windows() {
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();
    create_live_database(data_dir).await;

    // One model and one budget window, so there is something to export in each section.
    let pool = llmroller_store::pool::open_pool(
        &data_dir.join("llmroller.db"),
        llmroller_store::pool::Role::Writer,
    )
    .await
    .expect("open pool");
    sqlx::query(
        "INSERT INTO providers (id, ownership_scope, owner_user_id, kind, name, base_url, \
         config_json, created_at) VALUES (?, 'global', NULL, 'openrouter', ?, ?, '{}', ?)",
    )
    .bind("prov-sections-test")
    .bind("Sections Provider")
    .bind("https://example.invalid/v1")
    .bind(1_700_000_000_000_i64)
    .execute(&pool)
    .await
    .expect("insert provider");
    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, owner_user_id, label, secret_enc, \
         master_key_id, secret_last4, created_at) VALUES (?, ?, NULL, ?, ?, ?, ?, ?)",
    )
    .bind("uk-sections-test")
    .bind("prov-sections-test")
    .bind("sections key")
    .bind(b"sealed".to_vec())
    .bind(MASTER_KEY_ID)
    .bind("wxyz")
    .bind(1_700_000_000_000_i64)
    .execute(&pool)
    .await
    .expect("insert upstream key");
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, display_name, distribution, merge_state, \
         origin) VALUES (?, ?, ?, 'weighted', 'confirmed', 'global')",
    )
    .bind("01JABCDEFGHJKMNPQRSTVWXYZ0")
    .bind("openai/gpt-test")
    .bind("GPT Test")
    .execute(&pool)
    .await
    .expect("insert model");
    sqlx::query(
        "INSERT INTO budget_windows (id, scope_kind, upstream_key_id, model_pk, window_kind, \
         period_ms, limit_micro_usd) VALUES (?, 'upstream_key', ?, NULL, 'rolling', ?, ?)",
    )
    .bind("bw-sections-test")
    .bind("uk-sections-test")
    .bind(18_000_000_i64)
    .bind(5_000_000_i64)
    .execute(&pool)
    .await
    .expect("insert budget window");
    pool.close().await;

    let (code, stdout, stderr) = run_cli(&["export-diagnostics"], data_dir).await;
    assert_eq!(code, 0, "export must succeed; stderr: {stderr}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("export json");

    // Settings are seeded by `serve`, so the section must be non-empty.
    assert!(
        parsed["settings"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "the export must include the seeded settings section: {stdout}"
    );
    // The seeded settings include `default_routing_strategy`, which is a stable, non-secret key
    // — asserted by name so an empty-but-present array cannot pass.
    assert!(
        stdout.contains("default_routing_strategy"),
        "the settings section must carry the seeded keys: {stdout}"
    );
    assert_eq!(
        parsed["models"].as_array().map(Vec::len),
        Some(1),
        "the export must include the models section: {stdout}"
    );
    assert!(
        stdout.contains("openai/gpt-test"),
        "the model's canonical id must be exported: {stdout}"
    );
    assert_eq!(
        parsed["budget_windows"].as_array().map(Vec::len),
        Some(1),
        "the export must include the budget_windows section: {stdout}"
    );
    assert!(
        stdout.contains("bw-sections-test"),
        "the budget window must be identifiable: {stdout}"
    );
}
