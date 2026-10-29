//! Tenant visibility of `audit_log` (`ISSUE-0172`).
//!
//! # What this governs
//!
//! `/api/user/audit-log` and `/app/partials/audit-log` are tenant-facing views of an
//! instance-wide table. The predicate deciding what a tenant may see is a **security boundary**:
//! a mistake either hides events about the user's own resources or exposes another tenant's.
//!
//! It is therefore tested from both directions for every target type — a positive ("my key's
//! events are visible") and a negative ("someone else's key's events are not"). The negative is
//! the half that matters: it is the one that fails if a subquery loses its `owner_user_id = ?`,
//! and the positive alone would still pass with the ownership test deleted entirely.
//!
//! There is a third direction that neither of those catches: a leak through a target type that is
//! *instance-wide*. `a_user_does_not_see_operator_events` exists for exactly that, because the
//! obvious implementation — "any event whose `target_id` resolves to a row I own" — passes every
//! positive and negative above while exposing operator activity on `setting` and `instance`.
//!
//! # Why these drive the real handlers
//!
//! The predicate is SQL. A unit test of a helper that returns a WHERE fragment would assert the
//! fragment, not what SQLite does with it — and the interesting failures here are exactly the
//! ones where the SQL is syntactically fine and semantically wrong (a missing `owner_user_id`,
//! a subquery scoped to the wrong table). So each test builds a real router, a real session, a
//! real `audit_log`, and calls the real endpoint.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use llmroller_core::ids::UserId;
use llmroller_core::seams::{Clock as _, OsEntropy};
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;

use super::*;

use crate::auth::AuthConfig;
use crate::control_plane_router_full;
use crate::setup::SetupAppState;

const AUDIT_HMAC: [u8; 32] = [0x5C; 32];

/// Two tenants plus an admin, a migrated database, and a router wired the way `serve` wires it.
struct Fixture {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    app: axum::Router,
    tenant_a: UserId,
    tenant_b: UserId,
    cookie_a: String,
    cookie_b: String,
}

/// Build the fixture. `Role::User` for both tenants so neither is an admin — an admin sees the
/// whole chain by design and would make every isolation assertion pass trivially.
async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("audit_visibility.db");
    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = Arc::new(OsEntropy::new());
    let master_key = llmroller_store::crypto::MasterKey::from_bytes([0xB7; 32]);
    let instance_state = llmroller_store::instance::load_or_initialize(
        &writer_pool,
        "key-0",
        &master_key,
        entropy.clone(),
        1,
    )
    .await
    .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    // Users are seeded directly rather than through the store's `create_user` helper. That helper
    // requires an admin actor scope, and this crate is forbidden from constructing the *system*
    // one — the workspace boundary test greps `crates/admin/src` for the system constructor and
    // its seal (USR-1/SEC-17), and it matches comments as well as code. The same workaround is
    // used by `auth/tests/login_concurrency.rs`.
    let admin_id = seed_user(&writer_pool, "admin_visibility", "admin", &entropy, 1).await;
    let tenant_a_id = seed_user(&writer_pool, "tenant_a", "user", &entropy, 1).await;
    let tenant_b_id = seed_user(&writer_pool, "tenant_b", "user", &entropy, 1).await;
    // The admin exists so the instance has one, and so the fixture matches a real deployment.
    // It is not a session-holder: an admin sees the whole chain by design, so authenticating as
    // one would make every isolation assertion below pass trivially.
    assert_ne!(
        admin_id, tenant_a_id,
        "the fixture must have a distinct admin and tenant"
    );

    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: Vec::new(),
    };
    let mut auth_state =
        AuthAppState::new(writer_pool.clone(), auth_config).with_reader_pool(writer_pool.clone());
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();
    auth_state.audit_hmac_key = Some(instance_state.audit_hmac_key.clone());
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());

    let setup_state = SetupAppState {
        pool: writer_pool.clone(),
        expected_token: Some("unused-in-this-fixture".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: entropy.clone(),
        audit_hmac_key: Some(instance_state.audit_hmac_key.clone()),
        argon2_semaphore: auth_state.argon2_semaphore.clone(),
    };
    let mut api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    api_state.audit_hmac_key = Some(instance_state.audit_hmac_key.clone());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    let cookie_a = session_cookie(&writer_pool, &auth_state, tenant_a_id, "tenant-a-token").await;
    let cookie_b = session_cookie(&writer_pool, &auth_state, tenant_b_id, "tenant-b-token").await;

    Fixture {
        _dir: dir,
        pool: writer_pool,
        app,
        tenant_a: tenant_a_id,
        tenant_b: tenant_b_id,
        cookie_a,
        cookie_b,
    }
}

/// Insert one `users` row directly, returning its id.
///
/// Direct INSERT rather than the store's `create_user` because that helper demands an admin actor
/// scope, and this crate is forbidden from constructing the system one (USR-1/SEC-17). The
/// password hash is real, so a test that ever needs to log in as one of these users can.
async fn seed_user(
    pool: &sqlx::SqlitePool,
    username: &str,
    role: &str,
    entropy: &OsEntropy,
    now_ms: i64,
) -> UserId {
    let id = UserId::generate();
    let password_hash =
        llmroller_store::users::hash_password("supersecretpassword123", entropy).expect("hash");
    sqlx::query(
        "INSERT INTO users (
            id, username, password_hash, role, must_change_password,
            totp_enabled, can_use_global_providers, disabled, created_at
         ) VALUES (?, ?, ?, ?, 0, 0, 1, 0, ?)",
    )
    .bind(id.to_string())
    .bind(username)
    .bind(&password_hash)
    .bind(role)
    .bind(now_ms)
    .execute(pool)
    .await
    .expect("seed user");
    id
}

/// Insert a web session for `user` and return the cookie header that authenticates as them.
async fn session_cookie(
    pool: &sqlx::SqlitePool,
    auth_state: &AuthAppState,
    user: UserId,
    raw_token: &str,
) -> String {
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let raw = format!("{raw_token}_0123456789abcdef");
    let id_hash = llmroller_store::crypto::sha256_hex(raw.as_bytes());
    let csrf = auth_state.derive_session_csrf(&raw);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf.as_bytes());
    llmroller_store::users::insert_web_session(
        pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: user,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("cycle-41-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("web session");
    format!("llmroller_session={raw}")
}

/// One audit row to seed. `actor` is `None` for a system task or an admin acting on a resource.
struct Row {
    action: &'static str,
    target_type: &'static str,
    target_id: Option<String>,
    actor: Option<String>,
}

fn row(action: &'static str, target_type: &'static str, target_id: &str) -> Row {
    Row {
        action,
        target_type,
        target_id: Some(target_id.to_string()),
        // No actor: the admin/system case whose visibility is the whole issue.
        actor: None,
    }
}

/// Append rows through the real chained-hash writer so `seq` is monotonic and `doctor`-clean.
async fn seed(fixture: &Fixture, rows: &[Row]) -> Vec<i64> {
    let mut seqs = Vec::with_capacity(rows.len());
    for row in rows {
        let mut tx = fixture.pool.begin().await.expect("tx");
        let id = llmroller_core::ids::UsageEventId::generate().to_string();
        let (seq, _hash) = llmroller_store::audit::append(
            &mut tx,
            &AUDIT_HMAC,
            llmroller_store::audit::AuditEntry {
                id: &id,
                ts: 1_700_000_000_000 + seqs.len() as i64,
                actor_user_id: row.actor.as_deref(),
                ip_addr: None,
                action: row.action,
                target_type: row.target_type,
                target_id: row.target_id.as_deref(),
                detail_json: Some("{}"),
            },
        )
        .await
        .expect("append audit row");
        tx.commit().await.expect("commit");
        seqs.push(seq);
    }
    seqs
}

/// Insert a `client_keys` row owned by `owner`.
async fn seed_client_key(fixture: &Fixture, id: &str, owner: UserId) {
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at)
         VALUES (?, ?, ?, ?, ?, 0)",
    )
    .bind(id)
    .bind(owner.to_string())
    .bind(format!("label-{id}"))
    .bind(format!("llr_live_{id}"))
    .bind(format!("hash_{id}"))
    .execute(&fixture.pool)
    .await
    .expect("client key");
}

/// Insert a `providers` row. `owner = None` makes it a global provider (nullable owner).
async fn seed_provider(fixture: &Fixture, id: &str, owner: Option<UserId>) {
    let (scope, owner_str) = match owner {
        Some(user) => ("personal", Some(user.to_string())),
        None => ("global", None),
    };
    sqlx::query(
        "INSERT INTO providers (id, ownership_scope, owner_user_id, kind, name, base_url,
                                config_json, created_at)
         VALUES (?, ?, ?, 'openrouter', ?, 'https://example.invalid/v1', '{}', 0)",
    )
    .bind(id)
    .bind(scope)
    .bind(owner_str)
    .bind(format!("name-{id}"))
    .execute(&fixture.pool)
    .await
    .expect("provider");
}

/// Insert an `upstream_keys` row. `owner = None` is the nullable-owner case.
async fn seed_upstream_key(fixture: &Fixture, id: &str, provider_id: &str, owner: Option<UserId>) {
    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, owner_user_id, label, secret_enc,
                                    master_key_id, secret_last4, created_at)
         VALUES (?, ?, ?, ?, ?, 'key-0', 'abcd', 0)",
    )
    .bind(id)
    .bind(provider_id)
    .bind(owner.map(|user| user.to_string()))
    .bind(format!("label-{id}"))
    .bind(b"sealed".to_vec())
    .execute(&fixture.pool)
    .await
    .expect("upstream key");
}

/// Insert a `service_accounts` row owned by `owner`.
async fn seed_service_account(fixture: &Fixture, id: &str, owner: UserId) {
    sqlx::query(
        "INSERT INTO service_accounts (id, owner_user_id, name, created_at, updated_at)
         VALUES (?, ?, ?, 0, 0)",
    )
    .bind(id)
    .bind(owner.to_string())
    .bind(format!("label-{id}"))
    .execute(&fixture.pool)
    .await
    .expect("service account");
}

/// The `seq` values the JSON endpoint (`GET /api/user/audit-log`) returns for `cookie`.
async fn visible_seqs_json(fixture: &Fixture, cookie: &str) -> Vec<i64> {
    let body = get_text(&fixture.app, cookie, "/api/user/audit-log").await;
    let parsed: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("audit json: {e}: {body}"));
    parsed
        .as_array()
        .expect("array")
        .iter()
        .map(|entry| entry["seq"].as_i64().expect("seq"))
        .collect()
}

/// The `seq` values the HTMX partial (`/app/partials/audit-log`) returns for `cookie`.
///
/// The partial renders `#<seq>` inside a `<code>` element, so it is scraped rather than parsed —
/// which is also why `both_audit_endpoints_return_the_same_event_set` compares the two.
async fn visible_seqs_partial(fixture: &Fixture, cookie: &str) -> Vec<i64> {
    let html = get_text(&fixture.app, cookie, "/app/partials/audit-log").await;
    let mut seqs = Vec::new();
    let mut rest = html.as_str();
    while let Some(start) = rest.find("<code>#") {
        rest = &rest[start + "<code>#".len()..];
        let end = rest.find("</code>").unwrap_or(0);
        if end == 0 {
            break;
        }
        if let Ok(seq) = rest[..end].trim().parse::<i64>() {
            seqs.push(seq);
        }
    }
    seqs
}

async fn get_text(app: &axum::Router, cookie: &str, uri: &str) -> String {
    use axum::http::header;
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    assert_eq!(
        status,
        StatusCode::OK,
        "GET {uri} failed: {}",
        String::from_utf8_lossy(&bytes)
    );
    String::from_utf8(bytes.to_vec()).expect("utf8")
}

// ---------------------------------------------------------------------------
// The four ownership-scoped target types, each with a positive and a negative
// ---------------------------------------------------------------------------

/// `client_key` events about a user's own key are visible to that user (`ISSUE-0172`).
///
/// An admin disabling, revoking or rotating a user's key writes `actor_user_id IS NULL` with
/// `target_type = 'client_key'`. Before this fix the owning user never saw it, because only
/// `user` and `share` targets were reachable.
#[tokio::test]
#[doc = "Spec: AUD-1, AUD-2, 14.x — ISSUE-0172"]
async fn a_user_sees_an_admin_action_on_their_own_client_key() {
    let f = fixture().await;
    seed_client_key(&f, "ck-owned-by-a", f.tenant_a).await;
    let seqs = seed(
        &f,
        &[row("client_key.disabled", "client_key", "ck-owned-by-a")],
    )
    .await;

    let visible = visible_seqs_json(&f, &f.cookie_a).await;
    assert!(
        visible.contains(&seqs[0]),
        "user A must see an admin action on their own client key; visible={visible:?} expected={:?}",
        seqs
    );
}

/// `client_key` events about another user's key are **not** visible (`ISSUE-0172`).
///
/// This is the isolation half and the more important of the pair: it is what fails if the
/// subquery loses `owner_user_id = ?`.
#[tokio::test]
#[doc = "Spec: AUD-1, SEC-5 — ISSUE-0172"]
async fn a_user_does_not_see_an_admin_action_on_another_users_client_key() {
    let f = fixture().await;
    seed_client_key(&f, "ck-owned-by-b", f.tenant_b).await;
    let seqs = seed(
        &f,
        &[row("client_key.disabled", "client_key", "ck-owned-by-b")],
    )
    .await;

    let visible = visible_seqs_json(&f, &f.cookie_a).await;
    assert!(
        !visible.contains(&seqs[0]),
        "user A must NOT see an action on user B's client key; visible={visible:?} seq={}",
        seqs[0]
    );
}

/// Same pair for `upstream_key` — the type with a **nullable** owner column.
#[tokio::test]
#[doc = "Spec: AUD-1, SEC-5 — ISSUE-0172"]
async fn upstream_key_events_follow_ownership() {
    let f = fixture().await;
    seed_provider(&f, "prov-a", Some(f.tenant_a)).await;
    seed_provider(&f, "prov-b", Some(f.tenant_b)).await;
    seed_upstream_key(&f, "uk-a", "prov-a", Some(f.tenant_a)).await;
    seed_upstream_key(&f, "uk-b", "prov-b", Some(f.tenant_b)).await;
    let seqs = seed(
        &f,
        &[
            row("upstream_key.created", "upstream_key", "uk-a"),
            row("upstream_key.created", "upstream_key", "uk-b"),
        ],
    )
    .await;

    let visible = visible_seqs_json(&f, &f.cookie_a).await;
    assert!(
        visible.contains(&seqs[0]),
        "user A must see their own upstream key; visible={visible:?}"
    );
    assert!(
        !visible.contains(&seqs[1]),
        "user A must NOT see user B's upstream key; visible={visible:?}"
    );
}

/// Same pair for `provider`.
#[tokio::test]
#[doc = "Spec: AUD-1, SEC-5 — ISSUE-0172"]
async fn provider_events_follow_ownership() {
    let f = fixture().await;
    seed_provider(&f, "prov-a", Some(f.tenant_a)).await;
    seed_provider(&f, "prov-b", Some(f.tenant_b)).await;
    let seqs = seed(
        &f,
        &[
            row("provider.updated", "provider", "prov-a"),
            row("provider.updated", "provider", "prov-b"),
        ],
    )
    .await;

    let visible = visible_seqs_json(&f, &f.cookie_a).await;
    assert!(
        visible.contains(&seqs[0]),
        "user A must see their own provider; visible={visible:?}"
    );
    assert!(
        !visible.contains(&seqs[1]),
        "user A must NOT see user B's provider; visible={visible:?}"
    );
}

/// Same pair for `service_account`.
#[tokio::test]
#[doc = "Spec: AUD-1, SEC-5 — ISSUE-0172"]
async fn service_account_events_follow_ownership() {
    let f = fixture().await;
    seed_service_account(&f, "sa-a", f.tenant_a).await;
    seed_service_account(&f, "sa-b", f.tenant_b).await;
    let seqs = seed(
        &f,
        &[
            row("service_account.created", "service_account", "sa-a"),
            row("service_account.created", "service_account", "sa-b"),
        ],
    )
    .await;

    let visible = visible_seqs_json(&f, &f.cookie_a).await;
    assert!(
        visible.contains(&seqs[0]),
        "user A must see their own service account; visible={visible:?}"
    );
    assert!(
        !visible.contains(&seqs[1]),
        "user A must NOT see user B's service account; visible={visible:?}"
    );
}

// ---------------------------------------------------------------------------
// The leak test: instance-wide target types must stay invisible
// ---------------------------------------------------------------------------

/// Operator and instance-wide events must stay invisible to a tenant even when their `target_id`
/// happens to equal a value the tenant owns (`ISSUE-0172`, `§2.3`).
///
/// **This is the test the obvious implementation fails.** "Any event whose `target_id` resolves to
/// a row I own" passes every positive and negative above, because those all use ownership-scoped
/// types — and `setting` / `instance` / `instance_meta` / `http_path` are not ownership-scoped at
/// all. A tenant must not learn about the instance's master-key rotations, other tenants'
/// settings, or operator HTTP activity.
///
/// Each row's `target_id` is deliberately chosen to be a value that *would* match under a naive
/// predicate, so the test cannot pass merely because the ids are unrelated.
#[tokio::test]
#[doc = "Spec: AUD-1, SEC-5, SEC-6 — ISSUE-0172"]
async fn a_user_does_not_see_operator_events() {
    let f = fixture().await;
    seed_client_key(&f, "ck-owned-by-a", f.tenant_a).await;
    let seqs = seed(
        &f,
        &[
            // The tenant's own client-key id used as a *setting* target: instance-wide fact.
            row("setting.updated", "setting", "ck-owned-by-a"),
            // The tenant's own user id as an `instance` target: master-key rotation is not theirs.
            row("instance.master_key_rotated", "instance", "ck-owned-by-a"),
            row("instance_meta.sealed", "instance_meta", "ck-owned-by-a"),
            row("control.control.put", "http_path", "ck-owned-by-a"),
        ],
    )
    .await;

    let visible = visible_seqs_json(&f, &f.cookie_a).await;
    for seq in &seqs {
        assert!(
            !visible.contains(seq),
            "operator/instance event seq {seq} must stay invisible to a tenant; visible={visible:?}"
        );
    }
}

/// A row whose owner column is NULL belongs to no tenant, so nobody sees it (`ISSUE-0172`, `§1`).
///
/// `providers.owner_user_id` and `upstream_keys.owner_user_id` are nullable, and NULL there means
/// "global / unowned". A predicate written as `target_id IN (SELECT id FROM X WHERE owner_user_id
/// = ?)` gets this right by construction (NULL never equals anything); one written as a join with
/// a loose `IS NULL OR` would not. This test pins it.
#[tokio::test]
#[doc = "Spec: AUD-1, SEC-5 — ISSUE-0172"]
async fn an_unowned_resource_event_is_invisible_to_every_user() {
    let f = fixture().await;
    seed_provider(&f, "prov-global", None).await;
    seed_upstream_key(&f, "uk-unowned", "prov-global", None).await;
    let seqs = seed(
        &f,
        &[
            row("provider.updated", "provider", "prov-global"),
            row("upstream_key.created", "upstream_key", "uk-unowned"),
        ],
    )
    .await;

    for (cookie, who) in [(f.cookie_a.as_str(), "A"), (f.cookie_b.as_str(), "B")] {
        let visible = visible_seqs_json(&f, cookie).await;
        for seq in &seqs {
            assert!(
                !visible.contains(seq),
                "global/unowned event seq {seq} must be invisible to tenant {who}; \
                 visible={visible:?}"
            );
        }
    }
}

/// An unrecognised `target_type` gets no visibility (`§2.4`).
///
/// The predicate is an allow-list, so a target type added by a future change defaults to
/// invisible until someone decides otherwise. That is the safe direction for a boundary to fail,
/// and it is only true if no branch matches on anything else.
#[tokio::test]
#[doc = "Spec: AUD-1, SEC-5 — ISSUE-0172"]
async fn an_unrecognised_target_type_is_invisible() {
    let f = fixture().await;
    seed_client_key(&f, "ck-owned-by-a", f.tenant_a).await;
    let seqs = seed(
        &f,
        &[row(
            "future_resource.created",
            "future_resource",
            "ck-owned-by-a",
        )],
    )
    .await;

    let visible = visible_seqs_json(&f, &f.cookie_a).await;
    assert!(
        !visible.contains(&seqs[0]),
        "an unrecognised target type must be invisible until someone allows it; visible={visible:?}"
    );
}

// ---------------------------------------------------------------------------
// The two handlers must agree
// ---------------------------------------------------------------------------

/// Both tenant audit endpoints must return the same event set (`§3`).
///
/// The two handlers have **already drifted** on columns (8 vs 5). The predicate was identical only
/// because nobody had extended it yet; extending it in two places by hand is exactly how the
/// column drift happened. This test is what makes "one predicate" enforceable rather than
/// aspirational — if someone edits one call site and not the other, the sets diverge here.
#[tokio::test]
#[doc = "Spec: AUD-1, AUD-2 — ISSUE-0172"]
async fn both_audit_endpoints_return_the_same_event_set() {
    let f = fixture().await;
    seed_client_key(&f, "ck-owned-by-a", f.tenant_a).await;
    seed_client_key(&f, "ck-owned-by-b", f.tenant_b).await;
    seed_provider(&f, "prov-a", Some(f.tenant_a)).await;
    seed(
        &f,
        &[
            row("client_key.disabled", "client_key", "ck-owned-by-a"),
            row("client_key.disabled", "client_key", "ck-owned-by-b"),
            row("provider.updated", "provider", "prov-a"),
            row("setting.updated", "setting", "ck-owned-by-a"),
            Row {
                action: "user.password_changed",
                target_type: "user",
                target_id: Some(f.tenant_a.to_string()),
                actor: Some(f.tenant_a.to_string()),
            },
        ],
    )
    .await;

    let mut from_json = visible_seqs_json(&f, &f.cookie_a).await;
    let mut from_partial = visible_seqs_partial(&f, &f.cookie_a).await;
    from_json.sort_unstable();
    from_partial.sort_unstable();
    assert_eq!(
        from_json, from_partial,
        "the two tenant audit endpoints must see the same rows; JSON={from_json:?} \
         partial={from_partial:?}"
    );
    assert!(
        !from_json.is_empty(),
        "the fixture must have produced at least one visible row, or the equality is vacuous"
    );
}

// ---------------------------------------------------------------------------
// The pre-existing clauses must keep working
// ---------------------------------------------------------------------------

/// The three clauses that were already correct must not regress (`§1`, `§6`).
///
/// `actor_user_id = ?`, `target_type = 'user'`, and the `share` three-way union all predate this
/// cycle. Extending the union risks dropping one, so each is asserted here rather than relied on.
#[tokio::test]
#[doc = "Spec: AUD-1, AUD-2 — ISSUE-0172"]
async fn the_existing_actor_user_and_share_clauses_still_work() {
    let f = fixture().await;
    // The provider the share is over must exist: `provider_shares.provider_id` is a FK.
    seed_provider(&f, "prov-a", Some(f.tenant_a)).await;
    // A share between A (owner) and B (grantee).
    sqlx::query(
        "INSERT INTO provider_shares (id, provider_id, owner_user_id, grantee_user_id,
                                      enabled, created_at)
         VALUES ('share-ab', 'prov-a', ?, ?, 1, 0)",
    )
    .bind(f.tenant_a.to_string())
    .bind(f.tenant_b.to_string())
    .execute(&f.pool)
    .await
    .expect("share");
    let seqs = seed(
        &f,
        &[
            // Actor-scoped: A did it themselves.
            Row {
                action: "client_key.created",
                target_type: "client_key",
                target_id: Some("ck-not-owned".to_string()),
                actor: Some(f.tenant_a.to_string()),
            },
            // `user` target: an admin reset A's password.
            row("user.password_reset", "user", &f.tenant_a.to_string()),
            // `share` target: B sees the share they are a grantee of.
            row("share.accepted", "share", "share-ab"),
        ],
    )
    .await;

    let visible_a = visible_seqs_json(&f, &f.cookie_a).await;
    assert!(
        visible_a.contains(&seqs[0]),
        "a row whose actor is the caller must stay visible"
    );
    assert!(
        visible_a.contains(&seqs[1]),
        "a `user`-targeted row about the caller must stay visible"
    );
    assert!(
        visible_a.contains(&seqs[2]),
        "a share the caller owns must stay visible"
    );

    // The grantee sees it through the other half of the share union.
    let visible_b = visible_seqs_json(&f, &f.cookie_b).await;
    assert!(
        visible_b.contains(&seqs[2]),
        "a share the caller is grantee of must be visible"
    );
    assert!(
        !visible_b.contains(&seqs[1]),
        "a `user`-targeted row about A must stay invisible to B"
    );
}
