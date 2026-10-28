//! `GET /api/user/routes` must agree with the data plane about custom-share approval
//! (`ISSUE-0171`, `SHR-2`, `USR-4`, `RTE-4`).
//!
//! # The bug this pins
//!
//! `crates/core/src/shares.rs::is_share_eligible` refuses a `custom` provider share whose
//! `admin_approved_at` is `NULL`, so **the inference path cannot use an unapproved custom
//! share**. The admin display queries in `admin/src/api/models.rs` filtered only on
//! `accepted_at` / `revoked_at` / fingerprint, omitting the approval condition. A grantee
//! therefore saw an unapproved share in their routing matrix, could set a preference for
//! it, and then their requests silently did not use it — with the matrix saying it was
//! available. No traffic escaped; the bug is that the UI/API disagreed with the runtime.
//!
//! Two tests are load-bearing beyond the primary red, and both are here on purpose:
//!
//! - [`user_routes_includes_the_same_share_once_admin_approved`] — without it, "filter out
//!   every custom share" passes the primary red.
//! - [`user_routes_keeps_your_own_key_labels`] — without it, blanket label redaction passes
//!   the label test, which is worse than the bug.
//!
//! # Key labels are NOT redacted from a share grantee, and that is deliberate
//!
//! The brief for this cycle asked for grantor-label redaction on shared routes. The
//! specification says the opposite, twice, in the normative requirements:
//!
//! - `USR-4` scopes label-hiding to **`global`** keys — "administrative labels … of
//!   `global` upstream keys … nor `secret_last4` or raw `/usage` JSON of shared keys". For
//!   `shared` keys the secret surface is `secret_last4` and `/usage`, not the label.
//! - `4. SRDD:47`: a grantee "sees provider kind, base URL host, **key label**, health,
//!   allowed models …; never the secret or `secret_last4`".
//! - `4. SRDD:118`: a grantee "sees the provider kind, `base_url` host, and **upstream key
//!   label before accepting the share**" — so the label cannot be a post-acceptance
//!   confidentiality boundary.
//! - `14.s.1`: a standard user's routing matrix renders an accepted shared key as
//!   `shared: <owner>/<label>`.
//!
//! [`user_routes_shows_a_shared_key_label_as_owner_slash_label`] pins the specification's
//! behaviour so a future "redaction" cannot silently break the routing matrix's documented
//! column heading. What *is* protected is `secret_last4`, which this API never emitted.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_admin::auth::AuthConfig;
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, AuthAppState, control_plane_router_full};
use llmroller_core::ids::UserId;
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy, secret_string};
use llmroller_store::crypto::{MasterKey, sha256_hex};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, compute_share_config_fingerprint, create_provider,
    create_upstream_key,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::shares::{
    NewShare, accept_provider_share_tx, approve_provider_share_tx, create_provider_share_tx,
};
use llmroller_store::users::{NewUser, create_user};

/// The grantor's own name for their upstream key. Deliberately distinctive so an assertion
/// about its presence or absence cannot pass by coincidence.
const SHARED_KEY_LABEL: &str = "zz-grantor-key-label-probe-9f3a";

const MODEL: &str = "claude-sonnet-4.6";

struct Fixture {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    app: axum::Router,
    grantee_cookie: String,
    grantee_csrf: String,
    alice_id: UserId,
}

/// Build a world with:
/// - Alice owning a **custom** personal provider + key, shared with Bob,
/// - Bob (the grantee) holding an accepted share,
/// - Bob also owning a personal key of his own,
/// - and `admin_approved_at` left as `None` unless `approve` is true.
async fn fixture(approve: bool) -> Fixture {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("share_display.db");
    let pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let master_key = MasterKey::from_bytes([0xC1; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let mk_user = |name: &'static str, role: Role, pw: &'static str| {
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
                1,
            )
            .await
            .expect("create user")
        }
    };
    let _admin = mk_user("admin_c17", Role::Admin, "supersecretadminpass123").await;
    let alice = mk_user("alice_c17", Role::User, "supersecretalicepass123").await;
    let bob = mk_user("bob_c17", Role::User, "supersecretbobpass123").await;

    let alice_id = alice.id;
    let alice_actor = ActorScope::user(alice_id, Role::User);
    let bob_actor = ActorScope::user(bob.id, Role::User);

    // The model both keys route.
    let model_pk = "01JC17MODEL0000000000000A".to_string();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, exposed, routing_strategy) \
         VALUES (?, ?, 1, 'smooth_weighted_rr')",
    )
    .bind(&model_pk)
    .bind(MODEL)
    .execute(&pool)
    .await
    .expect("insert model");

    // Alice's personal CUSTOM provider + key, shared with Bob.
    let custom_config =
        r#"{"base_url":"https://custom-c17.example/v1","paths":{"chat":"/chat/completions"}}"#;
    let alice_provider = create_provider(
        &pool,
        &alice_actor,
        NewProvider {
            name: "alice_custom_c17".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://custom-c17.example/v1".to_string(),
            config_json: custom_config.to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(alice_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        100,
    )
    .await
    .expect("alice custom provider");

    let alice_key = create_upstream_key(
        &pool,
        &alice_actor,
        &master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: alice_provider.id,
            label: SHARED_KEY_LABEL.to_string(),
            secret: secret_string("sk-alice-c17-shared-secret"),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        100,
    )
    .await
    .expect("alice key");

    sqlx::query(
        "INSERT INTO model_routes \
         (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, 'claude-native', '[\"messages\",\"chat\"]', 'active', 1, 100)",
    )
    .bind(alice_key.id.to_string())
    .bind(&model_pk)
    .execute(&pool)
    .await
    .expect("alice route");

    let fingerprint = compute_share_config_fingerprint(
        alice_provider.kind.as_str(),
        &alice_provider.base_url,
        alice_provider.allow_private_network,
        custom_config,
    );

    // Bob accepts the share; admin approval only if asked for.
    let mut tx = pool.begin().await.expect("begin");
    let share = create_provider_share_tx(
        &mut tx,
        &alice_id,
        NewShare {
            provider_id: alice_provider.id,
            upstream_key_id: Some(alice_key.id),
            grantee_user_id: bob.id,
            allowed_models: None,
            max_monthly_micro_usd: None,
        },
        200,
    )
    .await
    .expect("create share");
    accept_provider_share_tx(&mut tx, &share.share_id, &bob.id, &fingerprint, 300)
        .await
        .expect("bob accepts");
    if approve {
        approve_provider_share_tx(&mut tx, &share.share_id, 400)
            .await
            .expect("admin approves");
    }
    tx.commit().await.expect("commit share");

    // Bob's OWN personal key, so over-redaction shows up.
    let bob_provider = create_provider(
        &pool,
        &bob_actor,
        NewProvider {
            name: "bob_personal_c17".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(bob.id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        100,
    )
    .await
    .expect("bob provider");
    let bob_key = create_upstream_key(
        &pool,
        &bob_actor,
        &master_key,
        "key-0",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: bob_provider.id,
            label: "bob-own-key-c17".to_string(),
            secret: secret_string("sk-bob-c17-own-secret"),
            priority: Some(2),
            strict_tier: None,
            enabled: Some(true),
        },
        100,
    )
    .await
    .expect("bob key");
    sqlx::query(
        "INSERT INTO model_routes \
         (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at) \
         VALUES (?, ?, 'openrouter-model', '[\"chat\"]', 'active', 1, 100)",
    )
    .bind(bob_key.id.to_string())
    .bind(&model_pk)
    .execute(&pool)
    .await
    .expect("bob route");

    // Bob's authenticated web session.
    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    // The session store rejects an expired row, so the timestamps come from the real
    // clock the middleware reads rather than a fixed epoch value.
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let raw_session = "bob_c17_session_token_0123456789abcdef";
    let id_hash = sha256_hex(raw_session.as_bytes());
    let csrf_token = auth_state.derive_session_csrf(raw_session);
    let csrf_hash = sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: bob.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("bob-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("insert bob session");

    Fixture {
        _dir: dir,
        pool,
        app,
        grantee_cookie: format!("llmroller_session={raw_session}"),
        grantee_csrf: csrf_token,
        alice_id,
    }
}

impl Fixture {
    async fn get_routes(&self) -> serde_json::Value {
        let request = Request::builder()
            .method("GET")
            .uri("/api/user/routes")
            .header(header::COOKIE, &self.grantee_cookie)
            .body(Body::empty())
            .expect("routes request");
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("routes response");
        assert_eq!(response.status(), StatusCode::OK, "GET /api/user/routes");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("routes json")
    }

    /// Every key id the grantee was offered, across own and shared columns.
    fn offered_key_ids(json: &serde_json::Value) -> Vec<String> {
        let mut out = Vec::new();
        for model in json["models"].as_array().into_iter().flatten() {
            for column in ["own_keys", "shared_keys"] {
                for entry in model[column].as_array().into_iter().flatten() {
                    if let Some(id) = entry["upstream_key_id"].as_str() {
                        out.push(id.to_string());
                    }
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Primary red: an unapproved custom share must not be offered
// ---------------------------------------------------------------------------

/// Pre-fix the unapproved share appeared in `shared_keys`, so the matrix claimed a route
/// the data plane would refuse. The fix must not break approved shares — see the next test.
#[tokio::test]
#[doc = "Spec: SHR-2, RTE-4 — ISSUE-0171 user_routes omits an unapproved custom share"]
async fn user_routes_omits_an_unapproved_custom_share() {
    let fx = fixture(false).await;
    let alice_key_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM upstream_keys WHERE label = ?")
            .bind(SHARED_KEY_LABEL)
            .fetch_one(&fx.pool)
            .await
            .expect("count alice keys");
    assert_eq!(alice_key_count, 1, "precondition: Alice's key exists");

    let json = fx.get_routes().await;
    let offered = Fixture::offered_key_ids(&json);
    let alice_key_id: String = sqlx::query_scalar("SELECT id FROM upstream_keys WHERE label = ?")
        .bind(SHARED_KEY_LABEL)
        .fetch_one(&fx.pool)
        .await
        .expect("alice key id");

    assert!(
        !offered.contains(&alice_key_id),
        "an unapproved custom share must not be offered to the grantee, or the routing \
         matrix claims a route the data plane refuses; offered was {offered:?}"
    );
    // Bob's own key is still there, so this is not "filter everything".
    let own: Vec<String> = json["models"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|m| m["own_keys"].as_array().cloned().unwrap_or_default())
        .filter_map(|e| e["upstream_key_id"].as_str().map(ToString::to_string))
        .collect();
    assert_eq!(own.len(), 1, "the grantee's own key must still be listed");
}

/// The positive half. Without this, "omit every custom share" passes the test above.
#[tokio::test]
#[doc = "Spec: SHR-2, RTE-4 — ISSUE-0171 user_routes includes the share once admin-approved"]
async fn user_routes_includes_the_same_share_once_admin_approved() {
    let fx = fixture(true).await;
    let json = fx.get_routes().await;
    let offered = Fixture::offered_key_ids(&json);
    let alice_key_id: String = sqlx::query_scalar("SELECT id FROM upstream_keys WHERE label = ?")
        .bind(SHARED_KEY_LABEL)
        .fetch_one(&fx.pool)
        .await
        .expect("alice key id");

    assert!(
        offered.contains(&alice_key_id),
        "an approved custom share must be offered; without this a fix that drops every \
         custom share would still pass the omission test. Offered was {offered:?}"
    );
}

// ---------------------------------------------------------------------------
// Key-label visibility: the specification says the grantee sees it
// ---------------------------------------------------------------------------

/// `14.s.1` renders an accepted shared key as `shared: <owner>/<label>`, and `4. SRDD:118`
/// gives the grantee the label **before** acceptance. A redaction here would break the
/// documented column heading and hide nothing that was ever secret.
///
/// This test exists because the cycle brief asked for the opposite. It is the record of
/// why that request was not implemented: the normative specification wins.
#[tokio::test]
#[doc = "Spec: USR-4, RTE-4 — ISSUE-0171 a shared key label stays visible to the grantee"]
async fn user_routes_shows_a_shared_key_label_as_owner_slash_label() {
    let fx = fixture(true).await;
    let json = fx.get_routes().await;

    let shared_labels: Vec<String> = json["models"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|m| m["shared_keys"].as_array().cloned().unwrap_or_default())
        .filter_map(|e| e["label"].as_str().map(ToString::to_string))
        .collect();

    assert!(
        shared_labels.iter().any(|l| l == SHARED_KEY_LABEL),
        "an approved shared key's label must be shown to the grantee so the matrix can \
         render `shared: <owner>/<label>` (14.s.1, 4:118); got {shared_labels:?}"
    );
    // What USR-4 does protect for shared keys: never a secret suffix.
    let rendered = json.to_string();
    assert!(
        !rendered.contains("secret_last4"),
        "the routes payload must not carry a secret suffix field at all"
    );
    assert!(
        !rendered.contains("sk-alice-c17-shared-secret"),
        "the upstream secret must never appear in a control-plane payload"
    );
}

/// The over-redaction guard, for the fields that *are* redacted: a personal key's label is
/// the user's own name for their own key and must stay visible.
#[tokio::test]
#[doc = "Spec: USR-4 — ISSUE-0171 user_routes keeps your own key labels"]
async fn user_routes_keeps_your_own_key_labels() {
    let fx = fixture(true).await;
    let json = fx.get_routes().await;
    let own_labels: Vec<String> = json["models"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|m| m["own_keys"].as_array().cloned().unwrap_or_default())
        .filter_map(|e| e["label"].as_str().map(ToString::to_string))
        .collect();

    assert!(
        own_labels.iter().any(|l| l == "bob-own-key-c17"),
        "the caller's own key label is theirs and must not be redacted; got {own_labels:?}"
    );
}

// ---------------------------------------------------------------------------
// Write side: a preference cannot target the unapproved share
// ---------------------------------------------------------------------------

/// `PUT /api/user/routes/{key}/{model}` shares the same eligibility semantics. Pre-fix it
/// accepted a preference for the unapproved custom share (200), then the request silently
/// never used that route.
#[tokio::test]
#[doc = "Spec: SHR-2, RTE-5 — ISSUE-0171 route preference cannot target an unapproved custom share"]
async fn route_preference_cannot_target_an_unapproved_custom_share() {
    let fx = fixture(false).await;
    let alice_key_id: String = sqlx::query_scalar("SELECT id FROM upstream_keys WHERE label = ?")
        .bind(SHARED_KEY_LABEL)
        .fetch_one(&fx.pool)
        .await
        .expect("alice key id");

    let request = Request::builder()
        .method("PUT")
        .uri(format!("/api/user/routes/{alice_key_id}/{MODEL}"))
        .header(header::COOKIE, &fx.grantee_cookie)
        .header("x-csrf-token", &fx.grantee_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({"priority_override": 250, "enabled": true}).to_string(),
        ))
        .expect("put request");
    let response = fx.app.clone().oneshot(request).await.expect("put response");

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a preference on an unapproved share must be rejected as not-found, matching the \
         endpoint's existing contract for keys the caller may not use; a 200 stores a \
         preference for a route that will never be selected"
    );
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_route_preferences WHERE upstream_key_id = ?")
            .bind(&alice_key_id)
            .fetch_one(&fx.pool)
            .await
            .expect("count prefs");
    assert_eq!(
        stored, 0,
        "no preference row may be written for a refused share"
    );
}

/// The approved share accepts a preference, so the guard is an approval check and not a
/// blanket refusal of custom shares.
#[tokio::test]
#[doc = "Spec: RTE-5 — ISSUE-0171 route preference works on an approved custom share"]
async fn route_preference_works_on_an_approved_custom_share() {
    let fx = fixture(true).await;
    let alice_key_id: String = sqlx::query_scalar("SELECT id FROM upstream_keys WHERE label = ?")
        .bind(SHARED_KEY_LABEL)
        .fetch_one(&fx.pool)
        .await
        .expect("alice key id");

    let request = Request::builder()
        .method("PUT")
        .uri(format!("/api/user/routes/{alice_key_id}/{MODEL}"))
        .header(header::COOKIE, &fx.grantee_cookie)
        .header("x-csrf-token", &fx.grantee_csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({"priority_override": 250, "enabled": true}).to_string(),
        ))
        .expect("put request");
    let response = fx.app.clone().oneshot(request).await.expect("put response");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an approved custom share must accept a preference"
    );
}

// ---------------------------------------------------------------------------
// Non-regression: the data plane's rule is untouched
// ---------------------------------------------------------------------------

/// The data-plane guard is correct already; this asserts it stays that way while the
/// display path is brought into line.
#[tokio::test]
#[doc = "Spec: SHR-2 — ISSUE-0171 the data plane still refuses an unapproved custom share"]
async fn data_plane_still_refuses_an_unapproved_custom_share() {
    use llmroller_core::shares::{IneligibleReason, is_share_eligible};
    use llmroller_store::shares::get_provider_share_tx;

    let fx = fixture(false).await;
    let mut tx = fx.pool.begin().await.expect("begin");
    let share_id: String = sqlx::query_scalar(
        "SELECT s.id FROM provider_shares s \
         JOIN upstream_keys k ON k.id = s.upstream_key_id \
         WHERE k.label = ?",
    )
    .bind(SHARED_KEY_LABEL)
    .fetch_one(&mut *tx)
    .await
    .expect("share id");
    let share_id = llmroller_core::ids::ShareId::parse(&share_id).expect("parse share id");
    let cfg = get_provider_share_tx(&mut tx, &share_id)
        .await
        .expect("get share");
    let provider_fp: String = sqlx::query_scalar(
        "SELECT p.share_config_fingerprint FROM providers p \
         JOIN provider_shares s ON s.provider_id = p.id WHERE s.id = ?",
    )
    .bind(share_id.to_string())
    .fetch_one(&mut *tx)
    .await
    .expect("fingerprint");

    assert_eq!(
        is_share_eligible(&cfg, ProviderKind::Custom, &provider_fp, false, None),
        Err(IneligibleReason::CustomProviderNeedsAdminApproval),
        "the data plane must keep refusing an unapproved custom share"
    );
    let _ = fx.alice_id;
}
