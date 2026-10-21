//! DNS-level SSRF guard regressions.
//!
//! Spec: Section 8.2, Requirements: SEC-7.

#[tokio::test]
async fn test_guarded_client_blocks_domain_resolving_to_loopback_when_private_egress_disabled() {
    let client = llmroller_proxy::ssrf::create_guarded_client(false).expect("guarded client");
    let result = client.get("http://localhost:8080/v1/models").send().await;

    assert!(
        result.is_err(),
        "a hostname resolving to loopback must never connect"
    );
    let error = result.expect_err("transport error");
    let message = format!("{error:?}");
    assert!(
        message.contains("SSRF guard blocked"),
        "the DNS guard must reject the resolution, got: {message}"
    );
}

#[tokio::test]
async fn test_guarded_client_allows_loopback_resolution_when_private_egress_is_enabled() {
    let client = llmroller_proxy::ssrf::create_guarded_client(true).expect("guarded client");
    let result = client
        .get("http://127.0.0.1:9/v1/models")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    let error = result.expect_err("connection must fail");
    let message = format!("{error:?}");
    assert!(
        !message.contains("SSRF guard blocked"),
        "private egress must not be blocked by the DNS guard, got: {message}"
    );
}
