//! Share approval must be kind-checked, audited, and published (`ISSUE-0191`).
//!
//! # What was already right, and what was not
//!
//! `approve_provider_share_tx` already rejects a revoked share: the `UPDATE` carries
//! `AND revoked_at IS NULL` and a zero-row result is `ShareError::NotFound`. That half of the
//! issue is closed by code, and `approving_a_revoked_share_is_rejected` is a status-quo pin so
//! it stays closed.
//!
//! Two gaps were real:
//!
//! 1. **A non-custom share could be approved.** `SHR-3` only requires `admin_approved_at` for
//!    **custom** providers (`is_share_eligible` gates on `provider_kind == Custom`), so approving
//!    a built-in provider's share sets a column nothing reads — an approval that granted nothing,
//!    and a false entry in the audit trail.
//! 2. **No audit row and no snapshot publish.** The handler opened its own transaction and
//!    committed, bypassing `execute_atomic_mutation`, so an admin approval — a privilege-granting
//!    action — left no `audit_log` trace and did not reach the data plane's snapshot.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tempfile::TempDir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::ids::{ProviderId, ShareId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy, secret_string};
use llmroller_core::shares::{IneligibleReason, is_share_eligible};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::{InstanceState, load_or_initialize};
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, compute_share_config_fingerprint, create_provider_tx,
    create_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::shares::{
    NewShare, accept_provider_share_tx, create_provider_share_tx, get_provider_share_tx,
    revoke_provider_share_tx,
};
use llmroller_store::users::{NewUser, create_user};

const ADMIN_SESSION: &str = "c21_admin_session_0123456789abcdef";
const ADMIN_CSRF: &str = "c21_admin_csrf_0123456789abcdef";

struct Harness {
    _dir: TempDir,
    pool: sqlx::SqlitePool,
    app: axum::Router,
    admin_id: UserId,
    alice_id: UserId,
    bob_id: UserId,
    master_key: MasterKey,
    instance: InstanceState,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("c21_shares.db"), PoolRole::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    seed_defaults(&pool, None, 1).await.expect("seed");

    let master_key = MasterKey::from_bytes([0xF1; 32]);
    let instance = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance");

    let now = llmroller_core::seams::SystemClock::new().now_ms();
    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let mk_user = |name: &'static str, role: Role, pw: &'static str, now: i64| {
        let pool = pool.clone();
        async move {
            create_user(
                &pool,
                &system_actor,
                NewUser {
                    username: name,
                    password: pw,
                    role,
                    must_change_password: false,
                },
                &OsEntropy,
                now,
            )
            .await
            .expect("user")
        }
    };
    let admin = mk_user("admin_c21", Role::Admin, "supersecretadminpass123", now).await;
    let alice = mk_user("alice_c21", Role::User, "supersecretalicepass123", now).await;
    let bob = mk_user("bob_c21", Role::User, "supersecretbobpass123", now).await;

    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(ADMIN_SESSION.as_bytes()),
            user_id: admin.id,
            csrf_hash: &llmroller_store::crypto::sha256_hex(ADMIN_CSRF.as_bytes()),
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("c21"),
            created_at: now,
            expires_at: now + 86_400_000,
        },
    )
    .await
    .expect("session");

    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance.session_hmac_secret.clone());
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: None,
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let mut api_state =
        ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(vec![0x31; 32]));
    let app = control_plane_router_full(setup_state, auth_state, api_state);

    Harness {
        _dir: dir,
        pool,
        app,
        admin_id: admin.id,
        alice_id: alice.id,
        bob_id: bob.id,
        master_key,
        instance,
    }
}

impl Harness {
    async fn approve(&self, share_id: ShareId) -> StatusCode {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/admin/api/shares/{share_id}/approve"))
            .header(header::COOKIE, format!("llmroller_session={ADMIN_SESSION}"))
            .header("x-csrf-token", ADMIN_CSRF)
            .body(Body::empty())
            .expect("approve request");
        self.app
            .clone()
            .oneshot(request)
            .await
            .expect("approve response")
            .status()
    }
}

/// Build an accepted share on a provider of the given kind, returning its id and fingerprint.
async fn accepted_share(
    h: &Harness,
    kind: ProviderKind,
    label: &str,
) -> (ShareId, String, ProviderId) {
    let alice_actor = ActorScope::user(h.alice_id, Role::User);
    let base_url = match kind {
        ProviderKind::Custom => "https://custom-c21.example/v1",
        _ => "https://openrouter.ai/api/v1",
    };
    let config_json = "{}";
    let mut tx = h.pool.begin().await.expect("begin");
    let provider = create_provider_tx(
        &mut tx,
        &alice_actor,
        NewProvider {
            name: label.to_string(),
            kind,
            base_url: base_url.to_string(),
            config_json: config_json.to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(h.alice_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("provider");
    let key = create_upstream_key_tx(
        &mut tx,
        &alice_actor,
        &h.master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: format!("{label}-key"),
            secret: secret_string(format!("sk-{label}")),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("key");

    let fingerprint = compute_share_config_fingerprint(
        provider.kind.as_str(),
        &provider.base_url,
        provider.allow_private_network,
        config_json,
    );

    let share = create_provider_share_tx(
        &mut tx,
        &h.alice_id,
        NewShare {
            provider_id: provider.id,
            upstream_key_id: Some(key.id),
            grantee_user_id: h.bob_id,
            allowed_models: None,
            max_monthly_micro_usd: None,
        },
        2000,
    )
    .await
    .expect("share");
    accept_provider_share_tx(&mut tx, &share.share_id, &h.bob_id, &fingerprint, 3000)
        .await
        .expect("accept");
    tx.commit().await.expect("commit");
    (share.share_id, fingerprint, provider.id)
}

// ---------------------------------------------------------------------------
// Status-quo pin: the revoked guard is already in place
// ---------------------------------------------------------------------------

/// The issue's first half is already implemented; this keeps it closed.
#[tokio::test]
#[doc = "Spec: SHR-5 — ISSUE-0191 approving a revoked share is rejected"]
async fn approving_a_revoked_share_is_rejected() {
    let h = harness().await;
    let (share_id, _, _) = accepted_share(&h, ProviderKind::Custom, "c21-revoked").await;

    let mut tx = h.pool.begin().await.expect("begin");
    revoke_provider_share_tx(&mut tx, &share_id, &h.alice_id, 4000)
        .await
        .expect("revoke");
    tx.commit().await.expect("commit");

    let status = h.approve(share_id).await;
    assert!(
        status.is_client_error(),
        "approving a revoked share must be refused, got {status}"
    );

    // And the column really was not set.
    let approved: Option<i64> =
        sqlx::query_scalar("SELECT admin_approved_at FROM provider_shares WHERE id = ?")
            .bind(share_id.to_string())
            .fetch_one(&h.pool)
            .await
            .expect("row");
    assert!(
        approved.is_none(),
        "a refused approval must not stamp admin_approved_at"
    );
}

// ---------------------------------------------------------------------------
// Primary red: a non-custom share must not be approvable
// ---------------------------------------------------------------------------

/// `SHR-3` only requires approval for **custom** providers, so approving a built-in provider's
/// share sets a column nothing reads. Pre-fix this returned 200.
#[tokio::test]
#[doc = "Spec: SHR-3 — ISSUE-0191 approving a non-custom share is rejected"]
async fn approving_a_non_custom_share_is_rejected() {
    let h = harness().await;
    let (share_id, _, _) = accepted_share(&h, ProviderKind::Openrouter, "c21-openrouter").await;

    let status = h.approve(share_id).await;
    assert!(
        status.is_client_error(),
        "approving a share on a built-in provider grants nothing and must be refused; got {status}"
    );

    let approved: Option<i64> =
        sqlx::query_scalar("SELECT admin_approved_at FROM provider_shares WHERE id = ?")
            .bind(share_id.to_string())
            .fetch_one(&h.pool)
            .await
            .expect("row");
    assert!(
        approved.is_none(),
        "the refused approval must not write admin_approved_at"
    );
}

/// The positive half. Without it, "reject every approval" passes the test above.
#[tokio::test]
#[doc = "Spec: SHR-2, SHR-3 — ISSUE-0191 approving a custom share succeeds and grants eligibility"]
async fn approving_a_custom_share_succeeds_and_grants_eligibility() {
    let h = harness().await;
    let (share_id, fingerprint, _) = accepted_share(&h, ProviderKind::Custom, "c21-custom").await;

    // Before approval the data plane refuses it — this is the state the approval changes.
    let mut tx = h.pool.begin().await.expect("begin");
    let cfg = get_provider_share_tx(&mut tx, &share_id)
        .await
        .expect("share");
    assert_eq!(
        is_share_eligible(&cfg, ProviderKind::Custom, &fingerprint, false, None),
        Err(IneligibleReason::CustomProviderNeedsAdminApproval),
        "precondition: an unapproved custom share is ineligible"
    );
    tx.commit().await.expect("commit");

    assert_eq!(h.approve(share_id).await, StatusCode::OK);

    let mut tx = h.pool.begin().await.expect("begin");
    let cfg = get_provider_share_tx(&mut tx, &share_id)
        .await
        .expect("share");
    assert!(
        is_share_eligible(&cfg, ProviderKind::Custom, &fingerprint, false, None).is_ok(),
        "after approval the share must be eligible, or the approval granted nothing"
    );
    tx.commit().await.expect("commit");
}

// ---------------------------------------------------------------------------
// The two gaps the issue did not name
// ---------------------------------------------------------------------------

/// A privilege-granting admin action must leave an audit trail.
#[tokio::test]
#[doc = "Spec: AUD-1 — ISSUE-0191 approving a share writes an audit row"]
async fn approving_a_share_writes_an_audit_row() {
    let h = harness().await;
    let (share_id, _, _) = accepted_share(&h, ProviderKind::Custom, "c21-audit").await;

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert_eq!(h.approve(share_id).await, StatusCode::OK);
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    assert!(
        after > before,
        "an admin approval grants access and must be audited; audit_log did not grow"
    );

    let (action, target_type, target_id): (String, String, Option<String>) = sqlx::query_as(
        "SELECT action, target_type, target_id FROM audit_log ORDER BY seq DESC LIMIT 1",
    )
    .fetch_one(&h.pool)
    .await
    .expect("audit row");
    assert!(
        action.contains("approve"),
        "the audit action must name the approval; got {action:?}"
    );
    assert_eq!(
        target_type, "share",
        "the audit target must be the share; got {target_type:?}"
    );
    let _ = target_id;
}

/// The publish the handler was missing. Pre-fix the snapshot kept the share unapproved.
#[tokio::test]
#[doc = "Spec: SHR-2, REL-4 — ISSUE-0191 approving a share publishes the shares domain"]
async fn approving_a_share_bumps_the_config_version() {
    let h = harness().await;
    let (share_id, _, _) = accepted_share(&h, ProviderKind::Custom, "c21-publish").await;

    let before: i64 = sqlx::query_scalar("SELECT config_version FROM instance_meta")
        .fetch_one(&h.pool)
        .await
        .expect("version");
    assert_eq!(h.approve(share_id).await, StatusCode::OK);
    let after: i64 = sqlx::query_scalar("SELECT config_version FROM instance_meta")
        .fetch_one(&h.pool)
        .await
        .expect("version");
    assert!(
        after > before,
        "approval must bump config_version so the reloader picks up the new share state; \
         {before} -> {after}"
    );
}

/// Approving a share twice is idempotent in effect — the row is already approved — but it must
/// not error, because an admin retrying a request should not see a failure.
#[tokio::test]
#[doc = "Spec: SHR-2 — ISSUE-0191 approving an already-approved custom share is accepted"]
async fn approving_twice_is_accepted() {
    let h = harness().await;
    let (share_id, _, _) = accepted_share(&h, ProviderKind::Custom, "c21-twice").await;
    assert_eq!(h.approve(share_id).await, StatusCode::OK);
    assert_eq!(
        h.approve(share_id).await,
        StatusCode::OK,
        "a repeated approval of an already-approved share must not fail"
    );
    let _ = h.admin_id;
    let _ = h.instance;
}

// Silence unused-field warnings when only some tests construct the full harness.
#[allow(dead_code)]
fn _keep(_: &InstanceState) {}
