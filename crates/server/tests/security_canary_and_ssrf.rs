//! SSRF protection, URL validation, and GuardedConnector tests.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#21-sub-phase-21-socket-level-ssrf-guardedconnector-and-url-validation

use std::sync::Arc;

use http::Uri;
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_proto::headers::{sanitize_csv_cell, scrub_canary_tokens};
use llmroller_proxy::ssrf::{GuardedConnector, SsrfError};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{NewProvider, ProviderError, create_provider};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};
use tempfile::tempdir;
use tower::Service;

async fn setup_test_context() -> (tempfile::TempDir, sqlx::SqlitePool, ActorScope, ActorScope) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("llmroller.db");

    let pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x66; 32]);
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance init");

    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin_user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "admin_ssrf",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("admin user");

    let reg_user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "user_ssrf",
            password: "supersecretuserpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("reg user");

    let admin_actor = ActorScope::user(admin_user.id, Role::Admin);
    let user_actor = ActorScope::user(reg_user.id, Role::User);

    (dir, pool, admin_actor, user_actor)
}

/// Spec: PRV-7, SEC-7
/// Verify that GuardedConnector and URL validation block:
/// 1. IP-literal https://127.0.0.1
/// 2. IPv4-mapped IPv6 https://[::ffff:127.0.0.1]
/// 3. 6to4 metadata https://[2002:a9fe:a9fe::1]
/// 4. Non-standard decimal integer host https://2130706433 at save
/// 5. Plaintext http:// schemes without allow_private_network
/// 6. allow_private_network = 1 allows admin to configure private endpoints.
#[doc = "Spec: PRV-7, SEC-7"]
#[tokio::test]
async fn test_guarded_connector_and_save_time_ssrf_blocking() {
    let (_dir, pool, admin_actor, user_actor) = setup_test_context().await;

    // 1. Save-time validation: Loopback IP literal rejected
    let err = create_provider(
        &pool,
        &user_actor,
        NewProvider {
            name: "loopback_ssrf".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://127.0.0.1:8080".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: user_actor.user_id(),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1,
    )
    .await
    .expect_err("127.0.0.1 must be rejected at save time");
    assert!(matches!(err, ProviderError::Validation(_)));

    // 2. Save-time validation: IPv4-mapped IPv6 rejected
    let err = create_provider(
        &pool,
        &user_actor,
        NewProvider {
            name: "mapped_ipv6_ssrf".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://[::ffff:127.0.0.1]:8080".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: user_actor.user_id(),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1,
    )
    .await
    .expect_err("mapped IPv6 must be rejected at save time");
    assert!(matches!(err, ProviderError::Validation(_)));

    // 3. Save-time validation: 6to4 AWS metadata address rejected
    let err = create_provider(
        &pool,
        &user_actor,
        NewProvider {
            name: "6to4_ssrf".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://[2002:a9fe:a9fe::1]:8080".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: user_actor.user_id(),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1,
    )
    .await
    .expect_err("6to4 metadata IP must be rejected at save time");
    assert!(matches!(err, ProviderError::Validation(_)));

    // 4. Save-time validation: decimal integer IP rejected
    let err = create_provider(
        &pool,
        &user_actor,
        NewProvider {
            name: "integer_ip_ssrf".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://2130706433/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: user_actor.user_id(),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1,
    )
    .await
    .expect_err("decimal integer IP must be rejected at save time");
    assert!(matches!(err, ProviderError::Validation(_)));

    // 5. Insecure http scheme rejected by default
    let err = create_provider(
        &pool,
        &user_actor,
        NewProvider {
            name: "http_ssrf".to_string(),
            kind: ProviderKind::Custom,
            base_url: "http://api.openai.com/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: user_actor.user_id(),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1,
    )
    .await
    .expect_err("http must be rejected without allow_private_network");
    assert!(matches!(err, ProviderError::Validation(_)));

    // 6. allow_private_network = 1 allows admin to configure private network endpoint
    let admin_prov = create_provider(
        &pool,
        &admin_actor,
        NewProvider {
            name: "local_llm_admin".to_string(),
            kind: ProviderKind::Custom,
            base_url: "http://127.0.0.1:11434".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: true,
            enabled: true,
        },
        1,
    )
    .await
    .expect("admin can create private network provider with allow_private_network=true");
    assert!(admin_prov.allow_private_network);

    // 7. GuardedConnector socket validation
    let mut connector = GuardedConnector::new(false);
    let uri: Uri = "https://127.0.0.1:8080".parse().expect("uri");
    let err = connector
        .call(uri)
        .await
        .expect_err("GuardedConnector must block 127.0.0.1 socket connect");
    assert!(matches!(err, SsrfError::BlockedIp(_)));

    let uri_6to4: Uri = "https://[2002:a9fe:a9fe::1]:443".parse().expect("uri");
    let err_6to4 = connector
        .call(uri_6to4)
        .await
        .expect_err("GuardedConnector must block 6to4 socket connect");
    assert!(matches!(err_6to4, SsrfError::BlockedIp(_)));
}

/// Spec: SEC-6
/// Automated canary tests: verify that a planted secret string is scrubbed and never echoed,
/// and CSV cells starting with formula prefixes are sanitized.
#[doc = "Spec: SEC-6"]
#[tokio::test]
async fn test_canary_secret_redaction_and_formula_injection() {
    let planted_canary = "sk-canary-9988776655443322";

    // 1. Scrub canary tokens from error messages
    let log_msg = format!("upstream connection error: request with token {planted_canary} failed");
    let scrubbed = scrub_canary_tokens(&log_msg, &[planted_canary]);
    assert!(!scrubbed.contains(planted_canary));
    assert!(scrubbed.contains("[REDACTED_CANARY]"));

    // 2. CSV formula injection protection
    for prefix in ['=', '+', '-', '@', '\t', '\r'] {
        let malicious = format!("{prefix}1+1");
        let sanitized = sanitize_csv_cell(&malicious);
        assert!(
            sanitized.starts_with('\''),
            "cell starting with {prefix:?} must be prefixed with single quote"
        );
    }
}
