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
