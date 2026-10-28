//! Provider badge derivation for the usage-by-model breakdown (`ISSUE-0131`).
//!
//! # The defect
//!
//! The overview's model breakdown emitted `"badge": "[LLR]"` for every row. `[LLR]` is the
//! *custom/internal* logo mark in `14.g.2`, so the field said "this is a house model" about
//! every model regardless of who served it — a value that conveys nothing, in a field the
//! table uses to tell providers apart.
//!
//! # The vocabulary is closed, and the spec names it
//!
//! `14.g.2` gives exactly five marks — `[GO]` `opencode_go`, `[CC]` `command_code`,
//! `[OD]` `open_design_amr`, `[OR]` `openrouter`, `[LLR]` custom/internal — and `14.g.4`
//! types the projection as `provider_badge_kind: &'static str // "go" | "cc" | "od" | "or" | "llr"`.
//!
//! Those five line up one-to-one with [`ProviderKind`]'s five variants, so the badge is
//! derived rather than invented: `ProviderKind` already is the vocabulary.
//!
//! # The aggregation question the issue did not raise
//!
//! The breakdown groups by `canonical_id`, but the same model can be served by several
//! providers in one window (that is why the header says "`<N>` models · `<M>` providers").
//! So a row needs a rule, not just a lookup. The rule here is "the provider that served the
//! most of this model's requests in the window", ties broken by provider id so the value is
//! stable across identical queries. The alternative — first row encountered — would make the
//! badge change as a side effect of unrelated data.

use super::*;
use axum::http::header;

/// The closed badge set from `14.g.2` / `14.g.4`. A value outside this set renders as an
/// unstyled string in the table, which is a different bug in the same place.
const SPEC_BADGE_KINDS: [&str; 5] = ["go", "cc", "od", "or", "llr"];

/// Seed one hourly rollup row, which is what the breakdown aggregates.
#[allow(clippy::too_many_arguments)]
async fn seed_rollup(
    pool: &sqlx::SqlitePool,
    hour_ms: i64,
    provider_id: &str,
    canonical_id: &str,
    requests: i64,
    cost_micro_usd: i64,
) {
    sqlx::query(
        "INSERT INTO usage_rollups_hourly (
             hour_ms, owner_user_id, client_key_id, upstream_key_id, provider_id,
             provider_scope, share_id, canonical_id, requests, errors,
             input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
             reasoning_tokens, cost_micro_usd, list_cost_micro_usd
         ) VALUES (?, 'u-badge', ?, ?, ?, 'global', '', ?, ?, 0, 10, 0, 0, 10, 0, ?, 0)",
    )
    .bind(hour_ms)
    .bind(format!("ck-{provider_id}-{canonical_id}"))
    .bind(format!("uk-{provider_id}"))
    .bind(provider_id)
    .bind(canonical_id)
    .bind(requests)
    .bind(cost_micro_usd)
    .execute(pool)
    .await
    .expect("seed rollup");
}

/// Insert a provider of a given kind; the badge is derived from this row.
async fn seed_provider(pool: &sqlx::SqlitePool, id: &str, kind: &str) {
    sqlx::query(
        "INSERT INTO providers (id, ownership_scope, kind, name, base_url, config_json, created_at) \
         VALUES (?, 'global', ?, ?, 'http://127.0.0.1:1', '{}', 1)",
    )
    .bind(id)
    .bind(kind)
    .bind(format!("provider-{id}"))
    .execute(pool)
    .await
    .expect("seed provider");
}

/// Insert a web session for a user and return its cookie.
async fn session_cookie(
    pool: &sqlx::SqlitePool,
    auth_state: &AuthAppState,
    user_id: UserId,
    raw_session: &str,
) -> String {
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let id_hash = llmroller_store::crypto::sha256_hex(raw_session.as_bytes());
    let csrf_token = auth_state.derive_session_csrf(raw_session);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf_token.as_bytes());
    llmroller_store::users::insert_web_session(
        pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("badge-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("insert session");
    format!("llmroller_session={raw_session}")
}

/// Build the admin router and return the overview JSON for the admin session.
async fn overview_json(
    state: ApiAppState,
    setup_state: SetupAppState,
    cookie: &str,
) -> serde_json::Value {
    let app =
        llmroller_admin::control_plane_router_full(setup_state, state.auth.clone(), state.clone());
    let req = Request::builder()
        .method("GET")
        .uri("/admin/api/overview")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .expect("req");
    let res = app.oneshot(req).await.expect("overview");
    assert_eq!(res.status(), StatusCode::OK, "overview must succeed");
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("overview json")
}

/// Everything a badge test needs: the pool, both router states, and an admin cookie.
struct BadgeHarness {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    api_state: ApiAppState,
    setup_state: SetupAppState,
    cookie: String,
    hour_ms: i64,
}

async fn build_states() -> BadgeHarness {
    let (dir, pool, master_key, admin_id, instance_state) = setup_dashboard_test_db().await;
    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    let cookie = session_cookie(
        &pool,
        &auth_state,
        admin_id,
        "badge_admin_session_0123456789",
    )
    .await;
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
    };
    let api_state = ApiAppState::new(auth_state, master_key, "key-0".to_string());
    let hour_ms = (api_state.auth.clock.now_ms() / 3_600_000) * 3_600_000;
    BadgeHarness {
        _dir: dir,
        pool,
        api_state,
        setup_state,
        cookie,
        hour_ms,
    }
}

/// The badge must be a real provider mark, not the `[LLR]` placeholder.
#[tokio::test]
#[doc = "Spec: UI-6, 14.g.2 — ISSUE-0131 the model breakdown emits a real provider badge"]
async fn overview_model_breakdown_emits_a_real_badge() {
    let h = build_states().await;
    seed_provider(&h.pool, "prov-openrouter", "openrouter").await;
    seed_rollup(&h.pool, h.hour_ms, "prov-openrouter", "glm-5.3", 10, 1_000).await;

    let json = overview_json(h.api_state, h.setup_state, &h.cookie).await;
    let models = json["models"].as_array().expect("models array");
    assert_eq!(models.len(), 1, "one model in the window; got {json}");
    let badge = models[0]["badge"].as_str().expect("badge is a string");

    assert_ne!(
        badge, "[LLR]",
        "the placeholder is the custom/internal mark, not a badge for every model"
    );
    assert!(
        SPEC_BADGE_KINDS.contains(&badge),
        "the badge must be one of {SPEC_BADGE_KINDS:?} (`14.g.4`), not {badge:?} — \
         a value outside the set renders unstyled"
    );
    assert_eq!(
        badge, "or",
        "an OpenRouter provider must render the `[OR]` mark (`14.g.2`)"
    );
}

/// Every provider kind maps to its own distinct mark — no two kinds share a badge, and the
/// mapping covers the closed set rather than defaulting to `[LLR]`.
#[tokio::test]
#[doc = "Spec: UI-6, 14.g.2 — ISSUE-0131 each provider kind maps to its own spec mark"]
async fn overview_badge_distinguishes_every_provider_kind() {
    let h = build_states().await;
    let hour_ms = h.hour_ms;

    // (provider kind, canonical model, expected `14.g.4` badge kind)
    let cases = [
        ("opencode_go", "model-go", "go"),
        ("command_code", "model-cc", "cc"),
        ("open_design_amr", "model-od", "od"),
        ("openrouter", "model-or", "or"),
        ("custom", "model-llr", "llr"),
    ];
    for (kind, model, _expected) in cases {
        let provider_id = format!("prov-{kind}");
        seed_provider(&h.pool, &provider_id, kind).await;
        seed_rollup(&h.pool, hour_ms, &provider_id, model, 10, 1_000).await;
    }

    let json = overview_json(h.api_state, h.setup_state, &h.cookie).await;
    let models = json["models"].as_array().expect("models array");

    for (kind, model, expected) in cases {
        let row = models
            .iter()
            .find(|m| m["canonical_id"] == model)
            .unwrap_or_else(|| panic!("model {model} missing from breakdown: {json}"));
        let badge = row["badge"].as_str().expect("badge is a string");
        assert_eq!(
            badge, expected,
            "provider kind {kind} must render badge {expected:?} per `14.g.2`; got {badge:?}"
        );
    }

    // No two kinds collapsed onto one mark.
    let mut badges: Vec<&str> = models.iter().filter_map(|m| m["badge"].as_str()).collect();
    badges.sort_unstable();
    badges.dedup();
    assert_eq!(
        badges.len(),
        cases.len(),
        "each provider kind must have a distinct badge; got {badges:?}"
    );
}

/// A model served by several providers shows the one that served most of its requests.
///
/// Without a rule the badge would be whichever row the SQL happened to return first, so it
/// could change between two identical queries.
#[tokio::test]
#[doc = "Spec: UI-6, 14.g.2 — ISSUE-0131 the badge names the provider that served the most requests"]
async fn overview_badge_names_the_provider_with_the_most_requests() {
    let h = build_states().await;
    // Ids chosen so the *dominant* provider sorts **last** alphabetically. With the ids
    // reversed ("prov-major"/"prov-minor") the mutant that drops the `requests DESC` ordering
    // survives, because `ORDER BY provider_id ASC` happens to return the same row — an inert
    // mutant, which would leave this test looking stronger than it is.
    seed_provider(&h.pool, "prov-a-minor", "openrouter").await;
    seed_provider(&h.pool, "prov-z-major", "opencode_go").await;

    // Same canonical model, two providers, one clearly dominant.
    seed_rollup(&h.pool, h.hour_ms, "prov-a-minor", "shared-model", 3, 100).await;
    seed_rollup(
        &h.pool,
        h.hour_ms,
        "prov-z-major",
        "shared-model",
        97,
        9_900,
    )
    .await;

    let json = overview_json(h.api_state, h.setup_state, &h.cookie).await;
    let models = json["models"].as_array().expect("models array");
    let row = models
        .iter()
        .find(|m| m["canonical_id"] == "shared-model")
        .unwrap_or_else(|| panic!("shared-model missing: {json}"));

    assert_eq!(
        row["badge"], "go",
        "the badge must name the provider that served most requests (97 vs 3), not \
         whichever row was read first"
    );
    assert_eq!(
        row["requests"], 100,
        "the row still aggregates across providers; only the badge picks one"
    );
}
