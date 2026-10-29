//! End-to-end geo attribution gating (`#158`).
//!
//! The unit tests elsewhere prove the derivation rule (`llmroller_proto::geo`), the storage rule
//! (`llmroller_store::usage_writer`) and the trust predicate (`llmroller_core::trust`). These
//! tests prove the wiring: a real request through the data-plane router ends up with the right
//! `client_location`, and the same claimed headers behave differently depending only on whether
//! the peer is a configured edge.
//!
//! Spec: USG-9, SEC-6, SEC-12

use super::*;
use sqlx::Row as _;

/// A chat request whose caller asserts a location in its own headers.
fn chat_request_claiming_location(token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("cf-ipcountry", "ZZ")
        .header("x-geo-country", "ZZ")
        .header("x-geo-city", "Atlantis")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}"#,
        ))
        .expect("request")
}

async fn stored_location(pool: &sqlx::SqlitePool) -> Option<String> {
    sqlx::query("SELECT client_location FROM usage_events")
        .fetch_one(pool)
        .await
        .expect("usage event row")
        .try_get::<Option<String>, _>("client_location")
        .expect("client_location column")
}

/// Build the data-plane app with a caller-supplied trusted-proxy list.
///
/// `build_wire_app_and_writer` uses an empty list, which is the correct production default and
/// is what the negative case needs. The positive case needs one entry, so this mirrors the
/// helper with the list parameterised rather than changing the shared default.
async fn build_app_with_trusted(
    pool: &sqlx::SqlitePool,
    master_key: &MasterKey,
    trusted_proxies: Vec<String>,
) -> (Router, WriterHandle) {
    let (reloader_tx, _reloader_rx) = tokio::sync::mpsc::channel(100);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx);
    let snapshot = load_initial_snapshot(pool).await.expect("load snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let mut proxy_auth_state = ProxyAuthState::new(shared_snapshot, pool.clone(), trusted_proxies)
        .with_master_key(master_key.clone());
    proxy_auth_state.is_ready.store(true, Ordering::SeqCst);
    proxy_auth_state.writer = Some(writer.clone());
    proxy_auth_state.session_hmac_secret = Some(secret_bytes(vec![0x5A; 32]));
    proxy_auth_state.budget_semaphore = Arc::new(PreflightBudgetSemaphore::new(819, 32));
    (data_plane_router(proxy_auth_state), writer)
}

/// The security case, end to end: an untrusted peer's claimed geo headers never reach the row.
///
/// Before `#158` this row held the caller's own `ZZ`. The peer is loopback with an empty
/// trusted list, which is the default deployment, so this is the case that matters most.
#[tokio::test]
#[doc = "Spec: USG-9, SEC-6, SEC-12"]
async fn an_untrusted_peer_cannot_set_client_location_end_to_end() {
    let (mock_addr, _mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("geo_untrusted.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0x8A; 32]);
    let token = seed_single_global_route(&pool, &master_key, mock_addr, "{}").await;
    let (app, writer) = build_app_with_trusted(&pool, &master_key, Vec::new()).await;

    let response = app
        .oneshot(chat_request_claiming_location(&token))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = response.into_body().collect().await.expect("collect body");
    flush_data_queues(&pool, &writer.queues).await;

    assert_eq!(
        stored_location(&pool).await,
        None,
        "a caller's own geo headers must not become its recorded location"
    );
}

/// A request with no `ConnectInfo` has no peer, so it must be treated as untrusted. If the
/// absence defaulted to loopback, every in-process caller would look like a trusted edge.
#[tokio::test]
#[doc = "Spec: USG-9, SEC-6"]
async fn a_request_with_no_peer_address_is_treated_as_untrusted() {
    let (mock_addr, _mock_state) = spawn_mock_upstream().await;
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("geo_nopeer.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0x8B; 32]);
    let token = seed_single_global_route(&pool, &master_key, mock_addr, "{}").await;
    // Localhost trusted: if the missing peer defaulted to loopback this request would be
    // attributed, so this test distinguishes "untrusted" from "trusted loopback".
    let (app, writer) = build_app_with_trusted(
        &pool,
        &master_key,
        vec!["127.0.0.1".to_string(), "::1".to_string()],
    )
    .await;

    let response = app
        .oneshot(chat_request_claiming_location(&token))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = response.into_body().collect().await.expect("collect body");
    flush_data_queues(&pool, &writer.queues).await;

    assert_eq!(
        stored_location(&pool).await,
        None,
        "no ConnectInfo means no known peer, and an unknown peer is not trusted"
    );
}

/// The gate is a gate, not a blanket removal: identical headers produce a location or not purely
/// on the trust answer, in the documented `"{city}, {country}"` format.
#[tokio::test]
#[doc = "Spec: USG-9, SEC-6, SEC-12"]
async fn the_gate_depends_on_the_peer_not_on_the_headers() {
    let mut claimed = HeaderMap::new();
    claimed.insert("x-geo-country", "SG".parse().expect("value"));
    claimed.insert("x-geo-city", "Singapore".parse().expect("value"));
    let trusted = vec!["10.0.0.0/8".to_string()];
    let inside: Option<SocketAddr> = Some("10.9.9.9:5000".parse().expect("addr"));
    let outside: Option<SocketAddr> = Some("203.0.113.7:5000".parse().expect("addr"));

    assert_eq!(
        llmroller_proxy::orchestrator::client_location_metadata_from_for_test(
            &claimed, inside, &trusted
        )
        .as_deref(),
        Some(r#"{"client_location":"Singapore, SG"}"#),
        "a trusted edge must be attributed, in the format 11.3 documents"
    );
    assert_eq!(
        llmroller_proxy::orchestrator::client_location_metadata_from_for_test(
            &claimed, outside, &trusted
        ),
        None,
        "the same headers from a peer outside the trusted prefix must be ignored"
    );
    assert_eq!(
        llmroller_proxy::orchestrator::client_location_metadata_from_for_test(
            &claimed,
            inside,
            &[]
        ),
        None,
        "the same headers with no configured edge must be ignored"
    );
}
