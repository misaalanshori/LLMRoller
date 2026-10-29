//! The per-event cost breakdown must price by the event's **upstream** model id, and must never
//! present invented rates (`ISSUE-0164`).
//!
//! # The defect
//!
//! `get_usage_event_breakdown_internal` looked up the price row by binding `canonical_id` into a
//! column named `upstream_model_id`:
//!
//! ```text
//! .bind(&canonical_id)              // "gpt-4o"
//! WHERE pr.upstream_model_id = ?    // wants "openai/gpt-4o-2024-11-20"
//! ```
//!
//! For any provider whose `upstream_model_id` differs from the canonical id — most of them — the
//! lookup missed. And a miss fell back to `Rates::default()`, the settings default of
//! `$3 / $15 / $0.30 / $3.75 per MTok`, so the popover showed a **confident cost computed from
//! fabricated rates** for a request whose price was never found.
//!
//! That is the invented-value class in its most consequential form: money.
//!
//! # What these tests pin
//!
//! 1. The lookup uses `upstream_model_id` and shows **those** rates.
//! 2. A canonical-id price row is still usable as a deliberate last resort, so canonically-priced
//!    models keep working.
//! 3. The provider-specific row still wins over the provider-agnostic one.
//! 4. **No price row** produces an explicit "unavailable" marker, not a plausible number.
//! 5. `reasoning_tokens` is returned and is **not** a `line_items` entry (status quo, so it cannot
//!    regress).
//!
//! Rates are chosen distinct from the fallback default on purpose: if a test used the default
//! values it would pass whether or not the lookup worked.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::ids::{ClientKeyId, ProviderId, UpstreamKeyId, UserId};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};

const SESSION_TOKEN: &str = "breakdown_pricing_token_0123456789ab";
const CSRF_TOKEN: &str = "breakdown_pricing_csrf_0123456789abcd";

/// The canonical id, and an upstream id that is deliberately *different* from it.
const CANONICAL: &str = "gpt-4o";
const UPSTREAM: &str = "openai/gpt-4o-2024-11-20";

/// `Rates::default()` — the values a missing lookup used to fall back to. Every test rate below is
/// chosen to be distinguishable from these, so a passing test cannot be passing on the default.
const DEFAULT_INPUT: i64 = 3_000_000;
const DEFAULT_OUTPUT: i64 = 15_000_000;

/// Distinctive upstream-model rates: $7 / $21 / $0.70 / $8.75 per MTok.
const UPSTREAM_INPUT: i64 = 7_000_000;
const UPSTREAM_OUTPUT: i64 = 21_000_000;
/// A provider-agnostic row for the same upstream id, at different rates ($11 input).
const AGNOSTIC_INPUT: i64 = 11_000_000;
/// A canonical-id row, at yet another rate ($13 input).
const CANONICAL_INPUT: i64 = 13_000_000;

struct Harness {
    _dir: tempfile::TempDir,
    app: axum::Router,
    pool: sqlx::SqlitePool,
    owner: UserId,
    client_key: ClientKeyId,
    provider: ProviderId,
    /// A *different* provider, so the provider-specific ordering has something to reject.
    other_provider: ProviderId,
    upstream_key: UpstreamKeyId,
}

/// Insert a price row. `provider` is `""` for the provider-agnostic row.
async fn insert_price(
    pool: &sqlx::SqlitePool,
    provider: &str,
    provider_kind: &str,
    upstream_model_id: &str,
    input: i64,
    output: i64,
) {
    sqlx::query(
        "INSERT INTO prices (provider_id, provider_kind, upstream_model_id,
                             input_per_mtok_micro_usd, output_per_mtok_micro_usd,
                             cache_read_per_mtok_micro_usd, cache_write_per_mtok_micro_usd,
                             source, updated_at)
         VALUES (?, ?, ?, ?, ?, 700_000, 8_750_000, 'seed', 1000)",
    )
    .bind(provider)
    .bind(provider_kind)
    .bind(upstream_model_id)
    .bind(input)
    .bind(output)
    .execute(pool)
    .await
    .expect("insert price");
}

impl Harness {
    /// Insert a usage event and return its id.
    async fn seed_event(
        &self,
        id: &str,
        canonical_id: &str,
        upstream_model_id: Option<&str>,
        provider_id: Option<ProviderId>,
        upstream_key_id: Option<UpstreamKeyId>,
    ) {
        sqlx::query(
            "INSERT INTO usage_events (
                 id, ts, request_id, client_key_id, owner_user_id,
                 upstream_key_id, provider_id, canonical_id, upstream_model_id,
                 endpoint, protocol, status, stream, incomplete, termination,
                 input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                 reasoning_tokens, cost_micro_usd, cost_source,
                 list_cost_micro_usd, plan_multiplier_bps, billing_coverage_mode
             ) VALUES (?, 1000, ?, ?, ?, ?, ?, ?, ?, 'chat', 'chat', 200, 0, 0, 'completed',
                       NULL, 0, 0, NULL, NULL, 0, 'price_book', 0, 10_000, 'pay_as_you_go')",
        )
        .bind(id)
        .bind(format!("req-{id}"))
        .bind(self.client_key.to_string())
        .bind(self.owner.to_string())
        .bind(upstream_key_id.map(|k| k.to_string()))
        .bind(provider_id.map(|p| p.to_string()))
        .bind(canonical_id)
        .bind(upstream_model_id)
        .execute(&self.pool)
        .await
        .expect("insert usage event");
    }

    /// `GET /api/user/usage/events/{id}/breakdown`.
    async fn breakdown(&self, id: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("GET")
            .uri(format!("/api/user/usage/events/{id}/breakdown"))
            .header(header::COOKIE, format!("llmroller_session={SESSION_TOKEN}"))
            .body(Body::empty())
            .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }
}

async fn harness() -> Harness {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("breakdown.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let master_key = MasterKey::from_bytes([0x6C; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "breakdown_owner",
            password: "supersecretbreakdownpass",
            role: Role::User,
            must_change_password: false,
        },
        &OsEntropy,
        1,
    )
    .await
    .expect("create user");

    let provider = ProviderId::generate();
    let other_provider = ProviderId::generate();
    for (id, name) in [(provider, "primary"), (other_provider, "other")] {
        sqlx::query(
            "INSERT INTO providers (id, name, kind, base_url, config_json, ownership_scope,
                                    owner_user_id, created_at)
             VALUES (?, ?, 'openrouter', 'https://example.test', '{}', 'personal', ?, 1000)",
        )
        .bind(id.to_string())
        .bind(format!("{name}-provider"))
        .bind(user.id.to_string())
        .execute(&pool)
        .await
        .expect("insert provider");
    }

    let upstream_key = UpstreamKeyId::generate();
    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, owner_user_id, label, secret_enc,
                                    master_key_id, secret_last4, created_at)
         VALUES (?, ?, ?, 'breakdown-key', X'00', 'key-0', '1234', 1000)",
    )
    .bind(upstream_key.to_string())
    .bind(provider.to_string())
    .bind(user.id.to_string())
    .execute(&pool)
    .await
    .expect("insert upstream key");

    let client_key = ClientKeyId::generate();
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, prefix, key_hash, label, routing_scope,
                                  created_at)
         VALUES (?, ?, 'llr_bd', ?, 'breakdown-client', 'personal_first', 1000)",
    )
    .bind(client_key.to_string())
    .bind(user.id.to_string())
    .bind(format!("hash-{client_key}"))
    .execute(&pool)
    .await
    .expect("insert client key");

    let mut auth_state = AuthAppState::new(
        pool.clone(),
        AuthConfig {
            dev_insecure_cookies: true,
            require_tls: false,
            trusted_proxies: Vec::new(),
        },
    );
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());

    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: None,
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let mut api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    api_state.audit_hmac_key = Some(instance_state.audit_hmac_key.clone());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let csrf_hash = llmroller_store::crypto::sha256_hex(CSRF_TOKEN.as_bytes());
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(SESSION_TOKEN.as_bytes()),
            user_id: user.id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("breakdown-test"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("insert session");

    Harness {
        _dir: dir,
        app,
        pool,
        owner: user.id,
        client_key,
        provider,
        other_provider,
        upstream_key,
    }
}

// ---------------------------------------------------------------------------

#[doc = "Spec: USG-3, USG-10, F-070 — price the event by its upstream model id"]
#[doc = "Spec: ISSUE-0164"]
#[tokio::test]
async fn the_breakdown_prices_an_event_by_its_upstream_model_id() {
    let h = harness().await;
    // A price row exists ONLY for the upstream id, never for the canonical id.
    insert_price(
        &h.pool,
        &h.provider.to_string(),
        "openrouter",
        UPSTREAM,
        UPSTREAM_INPUT,
        UPSTREAM_OUTPUT,
    )
    .await;
    h.seed_event(
        "ev-upstream",
        CANONICAL,
        Some(UPSTREAM),
        Some(h.provider),
        Some(h.upstream_key),
    )
    .await;

    let (status, body) = h.breakdown("ev-upstream").await;
    assert_eq!(status, StatusCode::OK, "breakdown must be readable: {body}");

    assert_eq!(
        body["rates"]["input_per_mtok_micro_usd"], UPSTREAM_INPUT,
        "the rates must come from the row keyed by upstream_model_id ({UPSTREAM}), not from a \
         fabricated fallback: {body}"
    );
    assert_eq!(
        body["rates"]["output_per_mtok_micro_usd"], UPSTREAM_OUTPUT,
        "output rate must also come from the upstream row: {body}"
    );
    assert_ne!(
        body["rates"]["input_per_mtok_micro_usd"], DEFAULT_INPUT,
        "the invented fallback rate must not appear when a real price row exists: {body}"
    );
    assert_eq!(
        body["pricing"], "price_book",
        "the breakdown must say the rates came from the price book: {body}"
    );
}

#[doc = "Spec: USG-3, F-070 — a canonical-id price row is a legitimate last resort"]
#[doc = "Spec: ISSUE-0164"]
#[tokio::test]
async fn a_canonical_id_price_row_is_used_when_no_upstream_model_row_exists() {
    let h = harness().await;
    // Only a canonical-id row. Some models are genuinely priced that way, and the old code's bug
    // was using this path *first and only* — not using it at all.
    insert_price(
        &h.pool,
        &h.provider.to_string(),
        "openrouter",
        CANONICAL,
        CANONICAL_INPUT,
        UPSTREAM_OUTPUT,
    )
    .await;
    h.seed_event(
        "ev-canonical",
        CANONICAL,
        Some(UPSTREAM),
        Some(h.provider),
        Some(h.upstream_key),
    )
    .await;

    let (status, body) = h.breakdown("ev-canonical").await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        body["rates"]["input_per_mtok_micro_usd"], CANONICAL_INPUT,
        "a canonical-id price row must still be used when no upstream-model row exists: {body}"
    );
    assert_eq!(
        body["pricing"], "price_book",
        "a canonical-id row is still a real price: {body}"
    );
}

#[doc = "Spec: F-070 — the provider-specific row outranks the provider-agnostic one"]
#[doc = "Spec: ISSUE-0164"]
#[tokio::test]
async fn the_provider_specific_price_row_wins_over_the_provider_agnostic_one() {
    let h = harness().await;
    insert_price(
        &h.pool,
        &h.provider.to_string(),
        "openrouter",
        UPSTREAM,
        UPSTREAM_INPUT,
        UPSTREAM_OUTPUT,
    )
    .await;
    insert_price(
        &h.pool,
        "",
        "openrouter",
        UPSTREAM,
        AGNOSTIC_INPUT,
        UPSTREAM_OUTPUT,
    )
    .await;
    // A row for a *different* provider at a third rate, which must not be chosen.
    insert_price(
        &h.pool,
        &h.other_provider.to_string(),
        "openrouter",
        UPSTREAM,
        17_000_000,
        UPSTREAM_OUTPUT,
    )
    .await;
    h.seed_event(
        "ev-precedence",
        CANONICAL,
        Some(UPSTREAM),
        Some(h.provider),
        Some(h.upstream_key),
    )
    .await;

    let (status, body) = h.breakdown("ev-precedence").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["rates"]["input_per_mtok_micro_usd"], UPSTREAM_INPUT,
        "the event's own provider must win over both the provider-agnostic row and another \
         provider's row: {body}"
    );
}

#[doc = "Spec: USG-3, F-070 — no price row must not become a fabricated cost"]
#[doc = "Spec: ISSUE-0164"]
#[tokio::test]
async fn a_breakdown_with_no_price_row_does_not_report_invented_rates() {
    let h = harness().await;
    // No price rows at all — neither by upstream id nor by canonical id.
    h.seed_event(
        "ev-unpriced",
        CANONICAL,
        Some(UPSTREAM),
        Some(h.provider),
        Some(h.upstream_key),
    )
    .await;

    let (status, body) = h.breakdown("ev-unpriced").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an unpriced event must still render a breakdown, not fail: {body}"
    );

    assert_eq!(
        body["pricing"], "unavailable",
        "the breakdown must say pricing was unavailable rather than present a number: {body}"
    );
    // The fabricated rates must not appear anywhere in the payload.
    let serialized = body.to_string();
    for invented in [DEFAULT_INPUT, DEFAULT_OUTPUT, 300_000, 3_750_000] {
        assert!(
            !serialized.contains(&invented.to_string()),
            "the invented fallback rate {invented} must not appear in an unpriced breakdown: \
             {serialized}"
        );
    }
    assert!(
        body["rates"].is_null(),
        "no rates may be reported when none were found: {body}"
    );
    // The four token classes are metered facts and must still be reported — the Logs Explorer,
    // the Usage Ledger and this popover have to agree on the canonical total even for an event
    // nobody can price. What must be absent is the *money*.
    let line_items = body["line_items"]
        .as_object()
        .expect("line_items must still carry the token classes");
    assert_eq!(
        line_items.len(),
        4,
        "the canonical four token classes must always be present: {body}"
    );
    for (class, item) in line_items {
        assert!(
            item.get("tokens").is_some(),
            "{class} must report its token count even when unpriced: {body}"
        );
        assert!(
            item.get("rate_per_mtok_micro_usd").is_none(),
            "{class} must not carry a rate when no price row exists: {body}"
        );
        assert!(
            item.get("subtotal_micro_usd").is_none(),
            "{class} must not carry a cost subtotal when no price row exists: {body}"
        );
    }
}

#[doc = "Spec: USG-3 — reasoning is returned, and is not a priced line item"]
#[doc = "Spec: ISSUE-0164"]
#[tokio::test]
async fn reasoning_tokens_are_returned_and_are_not_a_line_item() {
    let h = harness().await;
    insert_price(
        &h.pool,
        &h.provider.to_string(),
        "openrouter",
        UPSTREAM,
        UPSTREAM_INPUT,
        UPSTREAM_OUTPUT,
    )
    .await;
    h.seed_event(
        "ev-reasoning",
        CANONICAL,
        Some(UPSTREAM),
        Some(h.provider),
        Some(h.upstream_key),
    )
    .await;
    sqlx::query(
        "UPDATE usage_events SET reasoning_tokens = 1234, output_tokens = 5000 WHERE id = ?",
    )
    .bind("ev-reasoning")
    .execute(&h.pool)
    .await
    .expect("set reasoning tokens");

    let (status, body) = h.breakdown("ev-reasoning").await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        body["reasoning_tokens"], 1234,
        "reasoning must be returned from the row, not hardcoded: {body}"
    );
    assert!(
        body["line_items"].get("reasoning_tokens").is_none(),
        "reasoning is a diagnostic subset of output, so it must not be a priced line item — a \
         fifth line would imply a charge that does not exist: {body}"
    );
    let line_items = body["line_items"].as_object().expect("line_items object");
    assert_eq!(
        line_items.len(),
        4,
        "the canonical total is input_uncached + cache_read + cache_write + output; never five: \
         {body}"
    );
    assert_eq!(
        body["line_items"]["output"]["tokens"], 5000,
        "the output line item must carry the recorded output tokens: {body}"
    );
}
