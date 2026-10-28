//! Integration test for `llmroller serve` startup lifecycle, lock contention, and endpoints.
//!
//! Spec: Section 5.5, Section 5.5.2, Section 5.5.3, Requirements: POR-1, REL-1.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;
use tempfile::tempdir;

fn get_available_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local addr").port()
}

fn llmroller_binary() -> std::path::PathBuf {
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

#[doc = "Spec: POR-1, REL-1"]
#[tokio::test]
async fn test_serve_cli_boots_serves_readyz_and_handles_lock_contention() {
    let dir = tempdir().expect("temp dir");
    let data_dir = dir.path();

    let p_data = get_available_port();
    let p_admin = get_available_port();
    let p_metrics = get_available_port();

    let addr_data = format!("127.0.0.1:{p_data}");
    let addr_admin = format!("127.0.0.1:{p_admin}");
    let addr_metrics = format!("127.0.0.1:{p_metrics}");

    let key_b64 = BASE64.encode([0x42; 32]);

    // 1. Spawn `llmroller serve` daemon
    let mut child = tokio::process::Command::new(llmroller_binary())
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--listen")
        .arg(&addr_data)
        .arg("--admin-listen")
        .arg(&addr_admin)
        .arg("--metrics-listen")
        .arg(&addr_metrics)
        .arg("--dev-insecure-cookies")
        .env("LLMROLLER_MASTER_KEY", &key_b64)
        .env("LLMROLLER_MASTER_KEY_ID", "key-serve-test")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn serve");

    // 2. Poll /readyz until HTTP 200 (within 5 seconds)
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .expect("client");

    let ready_url = format!("http://{addr_data}/readyz");
    let mut is_ready = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(resp) = client.get(&ready_url).send().await
            && resp.status().is_success()
        {
            is_ready = true;
            break;
        }
    }
    assert!(is_ready, "/readyz did not become ready within 5 seconds");

    // Check /healthz
    let health_url = format!("http://{addr_data}/healthz");
    let health_resp = client.get(&health_url).send().await.expect("healthz send");
    assert!(health_resp.status().is_success());

    // Check /metrics
    let metrics_url = format!("http://{addr_metrics}/metrics");
    let metrics_resp = client.get(&metrics_url).send().await.expect("metrics send");
    assert!(metrics_resp.status().is_success());

    // 3. Second `llmroller serve` against same data_dir must exit 4 (lock contention)
    let output2 = tokio::process::Command::new(llmroller_binary())
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--listen")
        .arg(format!("127.0.0.1:{}", get_available_port()))
        .arg("--admin-listen")
        .arg(format!("127.0.0.1:{}", get_available_port()))
        .arg("--metrics-listen")
        .arg(format!("127.0.0.1:{}", get_available_port()))
        .env("LLMROLLER_MASTER_KEY", &key_b64)
        .env("LLMROLLER_MASTER_KEY_ID", "key-serve-test")
        .output()
        .await
        .expect("second serve");
    assert_eq!(
        output2.status.code(),
        Some(4),
        "second serve must exit with code 4 on lock contention"
    );

    // 4. Terminate child process cleanly
    let _ = child.kill().await;
}

#[doc = "Spec: POR-1, REL-1, Section 5.6"]
#[tokio::test]
async fn test_serve_honors_listener_environment_and_cli_precedence() {
    let key_b64 = BASE64.encode([0x43; 32]);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .expect("client");

    let env_dir = tempdir().expect("env temp dir");
    let env_data = format!("127.0.0.1:{}", get_available_port());
    let env_admin = format!("127.0.0.1:{}", get_available_port());
    let env_metrics = format!("127.0.0.1:{}", get_available_port());
    let mut env_child = tokio::process::Command::new(llmroller_binary())
        .arg("serve")
        .arg("--data-dir")
        .arg(env_dir.path())
        .arg("--dev-insecure-cookies")
        .env("LLMROLLER_LISTEN", &env_data)
        .env("LLMROLLER_ADMIN_LISTEN", &env_admin)
        .env("LLMROLLER_METRICS_LISTEN", &env_metrics)
        .env("LLMROLLER_MASTER_KEY", &key_b64)
        .env("LLMROLLER_MASTER_KEY_ID", "key-env-test")
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn env-configured serve");

    let env_ready = format!("http://{env_data}/readyz");
    let mut ready = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(resp) = client.get(&env_ready).send().await
            && resp.status().is_success()
        {
            ready = true;
            break;
        }
    }
    assert!(ready, "LLMROLLER_LISTEN must move the data listener");

    client
        .get(format!("http://{env_admin}/admin"))
        .send()
        .await
        .expect("admin listener must follow LLMROLLER_ADMIN_LISTEN");
    let env_metrics_resp = client
        .get(format!("http://{env_metrics}/metrics"))
        .send()
        .await
        .expect("metrics listener must follow LLMROLLER_METRICS_LISTEN");
    assert!(env_metrics_resp.status().is_success());
    let _ = env_child.kill().await;

    let flag_dir = tempdir().expect("flag temp dir");
    let flag_data = format!("127.0.0.1:{}", get_available_port());
    let flag_admin = format!("127.0.0.1:{}", get_available_port());
    let flag_metrics = format!("127.0.0.1:{}", get_available_port());
    let mut flag_child = tokio::process::Command::new(llmroller_binary())
        .arg("serve")
        .arg("--data-dir")
        .arg(flag_dir.path())
        .arg("--listen")
        .arg(&flag_data)
        .arg("--admin-listen")
        .arg(&flag_admin)
        .arg("--metrics-listen")
        .arg(&flag_metrics)
        .arg("--dev-insecure-cookies")
        .env("LLMROLLER_LISTEN", "127.0.0.1:1")
        .env("LLMROLLER_ADMIN_LISTEN", "127.0.0.1:1")
        .env("LLMROLLER_METRICS_LISTEN", "127.0.0.1:1")
        .env("LLMROLLER_MASTER_KEY", &key_b64)
        .env("LLMROLLER_MASTER_KEY_ID", "key-env-test")
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn flag-configured serve");

    let flag_ready = format!("http://{flag_data}/readyz");
    let mut ready = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(resp) = client.get(&flag_ready).send().await
            && resp.status().is_success()
        {
            ready = true;
            break;
        }
    }
    assert!(
        ready,
        "explicit listener flags must override LLMROLLER_* listener variables"
    );
    client
        .get(format!("http://{flag_admin}/admin"))
        .send()
        .await
        .expect("explicit --admin-listen must override LLMROLLER_ADMIN_LISTEN");
    let flag_metrics_resp = client
        .get(format!("http://{flag_metrics}/metrics"))
        .send()
        .await
        .expect("explicit --metrics-listen must override LLMROLLER_METRICS_LISTEN");
    assert!(flag_metrics_resp.status().is_success());
    let _ = flag_child.kill().await;
}

#[doc = "Spec: SEC-8, PERF-3, Section 5.5.1"]
#[tokio::test]
async fn test_serve_fails_closed_on_invalid_memory_and_body_limit_environment() {
    let key_b64 = BASE64.encode([0x45; 32]);

    for (variable, value) in [
        ("LLMROLLER_MEMORY_LIMIT_MB", "255"),
        ("LLMROLLER_MAX_BODY_MB", "129"),
        ("LLMROLLER_MAX_BODY_MB", "not-a-number"),
        ("LLMROLLER_PREFLIGHT_BODY_BUDGET_MB", "0"),
    ] {
        let dir = tempdir().expect("temp dir");
        let output = tokio::process::Command::new(llmroller_binary())
            .arg("serve")
            .arg("--data-dir")
            .arg(dir.path())
            .arg("--listen")
            .arg(format!("127.0.0.1:{}", get_available_port()))
            .arg("--admin-listen")
            .arg(format!("127.0.0.1:{}", get_available_port()))
            .arg("--metrics-listen")
            .arg(format!("127.0.0.1:{}", get_available_port()))
            .env("LLMROLLER_MASTER_KEY", &key_b64)
            .env("LLMROLLER_MASTER_KEY_ID", "key-limits-test")
            .env(variable, value)
            .kill_on_drop(true)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await
            .expect("run serve with invalid limits");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{variable}={value} must exit 2 per SEC-8"
        );
    }
}

#[doc = "Spec: RTE-14, Section 5.5.1"]
#[tokio::test]
async fn test_serve_rejects_invalid_default_routing_strategy_environment() {
    let key_b64 = BASE64.encode([0x46; 32]);
    let dir = tempdir().expect("temp dir");
    let output = tokio::process::Command::new(llmroller_binary())
        .arg("serve")
        .arg("--data-dir")
        .arg(dir.path())
        .arg("--listen")
        .arg(format!("127.0.0.1:{}", get_available_port()))
        .arg("--admin-listen")
        .arg(format!("127.0.0.1:{}", get_available_port()))
        .arg("--metrics-listen")
        .arg(format!("127.0.0.1:{}", get_available_port()))
        .env("LLMROLLER_MASTER_KEY", &key_b64)
        .env("LLMROLLER_MASTER_KEY_ID", "key-strategy-test")
        .env("LLMROLLER_DEFAULT_ROUTING_STRATEGY", "unknown_strategy")
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .expect("run serve with invalid routing strategy");
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid LLMROLLER_DEFAULT_ROUTING_STRATEGY must exit 2 per RTE-14"
    );
}

#[doc = "Spec: SEC-2, Section 5.5.1"]
#[tokio::test]
async fn test_serve_fails_closed_when_master_key_id_is_missing_or_empty() {
    let key_b64 = BASE64.encode([0x44; 32]);

    for (label, value) in [("missing", None), ("empty", Some(""))] {
        let dir = tempdir().expect("temp dir");
        let mut command = tokio::process::Command::new(llmroller_binary());
        command
            .arg("serve")
            .arg("--data-dir")
            .arg(dir.path())
            .arg("--listen")
            .arg(format!("127.0.0.1:{}", get_available_port()))
            .arg("--admin-listen")
            .arg(format!("127.0.0.1:{}", get_available_port()))
            .arg("--metrics-listen")
            .arg(format!("127.0.0.1:{}", get_available_port()))
            .env("LLMROLLER_MASTER_KEY", &key_b64)
            .env_remove("LLMROLLER_MASTER_KEY_ID")
            .kill_on_drop(true)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(value) = value {
            command.env("LLMROLLER_MASTER_KEY_ID", value);
        }

        let mut child = command.spawn().expect("spawn serve");
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        let Ok(status) = status else {
            let _ = child.kill().await;
            panic!("serve started with a {label} LLMROLLER_MASTER_KEY_ID");
        };
        assert_eq!(
            status.expect("serve status").code(),
            Some(3),
            "a {label} LLMROLLER_MASTER_KEY_ID must exit 3 per SEC-2"
        );
    }
}

// ---------------------------------------------------------------------------
// ISSUE-0227 — a documented TLS listener that is silently ignored
// ---------------------------------------------------------------------------

/// How long a `serve` invocation under test may run before it is considered "still serving".
///
/// A rejected startup exits in milliseconds; a started server runs forever. This bound turns the
/// second case into a fast, loud failure instead of a hang — an unbounded `.output().await` waits
/// for exit, and the pre-fix behaviour is precisely "never exits".
const SERVE_EXIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// Outcome of running `serve` with extra environment.
enum ServeOutcome {
    /// The process exited within the budget, with this code and stderr.
    Exited(Option<i32>, String),
    /// The process was still running when the budget expired — i.e. the server started.
    StillRunning(String),
}

/// Run `serve` with the given extra environment and report how it ended.
///
/// The listener ports are real and ephemeral, the master key is a dummy, and stderr is captured to
/// a file rather than a pipe: a piped stderr would need draining to avoid a full-pipe deadlock on a
/// child that keeps logging, and `kill_on_drop(true)` makes the timeout path clean up either way.
async fn serve_with_env(extra: &[(&str, &str)]) -> ServeOutcome {
    let dir = tempdir().expect("temp dir");
    let stderr_path = dir.path().join("serve.stderr");
    let stderr_file = std::fs::File::create(&stderr_path).expect("stderr file");
    let mut command = tokio::process::Command::new(llmroller_binary());
    command
        .arg("serve")
        .arg("--data-dir")
        .arg(dir.path())
        .arg("--listen")
        .arg(format!("127.0.0.1:{}", get_available_port()))
        .arg("--admin-listen")
        .arg(format!("127.0.0.1:{}", get_available_port()))
        .arg("--metrics-listen")
        .arg(format!("127.0.0.1:{}", get_available_port()))
        .env("LLMROLLER_MASTER_KEY", BASE64.encode([0x47; 32]))
        .env("LLMROLLER_MASTER_KEY_ID", "key-tls-test")
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file));
    for (name, value) in extra {
        command.env(name, value);
    }

    let read_stderr = |path: &std::path::Path| {
        std::fs::read_to_string(path).unwrap_or_else(|_| String::from("<no stderr captured>"))
    };

    match tokio::time::timeout(SERVE_EXIT_BUDGET, command.output()).await {
        // stderr goes to the file, so it is read back here rather than taken from `output`.
        Ok(Ok(output)) => ServeOutcome::Exited(output.status.code(), read_stderr(&stderr_path)),
        Ok(Err(error)) => panic!("could not run serve: {error}"),
        // Dropping the future drops the child (kill_on_drop) before this returns.
        Err(_elapsed) => ServeOutcome::StillRunning(read_stderr(&stderr_path)),
    }
}

/// `LLMROLLER_TLS_CERT` must be rejected at startup, not ignored (`ISSUE-0227`).
///
/// Four spec documents promise a native TLS listener; the binary never reads these variables. A
/// deployment that sets them therefore gets **plaintext with no warning** — the worst failure
/// mode for a security-relevant option, because it is indistinguishable from one that works.
///
/// The decision (owner, cycle 38) is upstream-only termination, so the correct behaviour is to
/// refuse to start and say what to do instead. Pre-fix this starts plaintext and exits 0.
#[doc = "Spec: SEC-9, Section 5.5.1 — ISSUE-0227"]
#[tokio::test]
async fn serve_rejects_tls_cert_at_startup() {
    let stderr =
        match serve_with_env(&[("LLMROLLER_TLS_CERT", "/etc/llmroller/tls/cert.pem")]).await {
            ServeOutcome::Exited(Some(2), stderr) => stderr,
            ServeOutcome::Exited(code, stderr) => panic!(
                "LLMROLLER_TLS_CERT must be rejected with exit 2 (§5.5.1 'invalid environment \
             variable'); got exit {code:?}, stderr: {stderr}"
            ),
            ServeOutcome::StillRunning(stderr) => panic!(
                "serve did not exit within {SERVE_EXIT_BUDGET:?} — it is still running, so \
             LLMROLLER_TLS_CERT was silently ignored and the operator is serving plaintext while \
             believing TLS is configured. stderr: {stderr}"
            ),
        };
    let lower = stderr.to_ascii_lowercase();
    assert!(
        lower.contains("tls"),
        "the rejection must name the variable's subject; stderr: {stderr}"
    );
    assert!(
        lower.contains("trusted_proxies") || lower.contains("reverse proxy"),
        "the rejection must say what to do instead — terminate TLS at the reverse proxy — or an \
         operator is left with a dead configuration and no path forward; stderr: {stderr}"
    );
}

/// `LLMROLLER_TLS_KEY` alone must also be rejected.
///
/// Rejecting only the cert would let a half-configured deployment through, which is the same bug
/// in a smaller size.
#[doc = "Spec: SEC-9, Section 5.5.1 — ISSUE-0227"]
#[tokio::test]
async fn serve_rejects_tls_key_at_startup() {
    let stderr = match serve_with_env(&[("LLMROLLER_TLS_KEY", "/etc/llmroller/tls/key.pem")]).await
    {
        ServeOutcome::Exited(Some(2), stderr) => stderr,
        ServeOutcome::Exited(code, stderr) => panic!(
            "LLMROLLER_TLS_KEY must be rejected with exit 2; got exit {code:?}, stderr: {stderr}"
        ),
        ServeOutcome::StillRunning(stderr) => panic!(
            "serve did not exit within {SERVE_EXIT_BUDGET:?} — LLMROLLER_TLS_KEY was silently \
             ignored. stderr: {stderr}"
        ),
    };
    assert!(
        stderr.to_ascii_lowercase().contains("tls"),
        "the rejection must name the variable; stderr: {stderr}"
    );
}

/// An **empty** `LLMROLLER_TLS_CERT` must be rejected, not treated as absent.
///
/// `LLMROLLER_TLS_CERT=""` is "set but meaningless". An operator who exported an empty variable
/// believes they configured something, and `env::var` returning `Ok("")` is not the same fact as
/// `Err(NotPresent)` — collapsing them with `unwrap_or_default()` is exactly the silent-ignore
/// defect this cycle is about. So presence, not usefulness, is the test.
#[doc = "Spec: SEC-9, Section 5.5.1 — ISSUE-0227"]
#[tokio::test]
async fn serve_rejects_an_empty_tls_cert() {
    match serve_with_env(&[("LLMROLLER_TLS_CERT", "")]).await {
        ServeOutcome::Exited(Some(2), _stderr) => {}
        ServeOutcome::Exited(code, stderr) => panic!(
            "an exported-but-empty LLMROLLER_TLS_CERT must be rejected with exit 2 rather than \
             treated as absent; got exit {code:?}, stderr: {stderr}"
        ),
        ServeOutcome::StillRunning(stderr) => panic!(
            "serve did not exit within {SERVE_EXIT_BUDGET:?} — an empty LLMROLLER_TLS_CERT was \
             treated as absent, so the operator's belief and the binary's behaviour differ. \
             stderr: {stderr}"
        ),
    }
}

/// Without the TLS variables, `serve` must still start — the rejection must not reject everything.
#[doc = "Spec: SEC-9 — ISSUE-0227 status-quo pin"]
#[tokio::test]
async fn serve_starts_normally_without_tls_vars() {
    let dir = tempdir().expect("temp dir");
    let listen = format!("127.0.0.1:{}", get_available_port());
    let admin = format!("127.0.0.1:{}", get_available_port());
    let metrics = format!("127.0.0.1:{}", get_available_port());
    let mut child = tokio::process::Command::new(llmroller_binary())
        .arg("serve")
        .arg("--data-dir")
        .arg(dir.path())
        .arg("--listen")
        .arg(&listen)
        .arg("--admin-listen")
        .arg(&admin)
        .arg("--metrics-listen")
        .arg(&metrics)
        .env("LLMROLLER_MASTER_KEY", BASE64.encode([0x48; 32]))
        .env("LLMROLLER_MASTER_KEY_ID", "key-no-tls-test")
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        if let Ok(resp) = client
            .get(format!("http://{listen}/readyz"))
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
        "serve must still start normally when neither TLS variable is set"
    );
}
