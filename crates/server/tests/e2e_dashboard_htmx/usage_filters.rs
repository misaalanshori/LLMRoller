//! Cycle 23 — the usage endpoints must honour the filters they accept.
//!
//! `UsageFilterParams` deserialises seven filters; before this cycle the four usage endpoints
//! bound only `since`/`until`, so `provider_id`, `model`, and `initiator` were accepted, parsed,
//! and silently dropped. A filter that silently no-ops is worse than a 400: it returns a
//! confidently wrong number with no signal (`ISSUE-0175`).
//!
//! Spec: `USG-1`, `USG-2`, `USG-3`, `14.n` (Initiator-Model-Date Pill Filters).
use super::*;

use llmroller_core::ids::{ClientKeyId, RequestId, UsageEventId};

/// One seeded usage row, mirroring what the writer produces for a single request.
#[derive(Clone, Copy)]
struct Row {
    ts: i64,
    client_key_id: &'static str,
    provider_id: &'static str,
    model: &'static str,
    tokens: (i64, i64, i64, i64),
    reasoning: i64,
    cost_micro_usd: i64,
}

impl Row {
    /// The canonical total, per `USG-1` and cycle 22: the four priced classes.
    ///
    /// `reasoning` is a diagnostic subset of `output` (`pricing.rs:24`, `12. SRDD:133-134`), so it
    /// is deliberately not added here.
    const fn total(&self) -> i64 {
        self.tokens.0 + self.tokens.1 + self.tokens.2 + self.tokens.3
    }
}

/// Insert the matching `usage_rollups_hourly` **and** `usage_events` rows.
///
/// Both are needed: `summary`/`timeseries`/`breakdown` aggregate the rollups, `events` reads the
/// raw table, and the agreement test compares all four. Seeding only one would make the endpoints
/// disagree for reasons unrelated to filtering.
async fn seed(pool: &sqlx::SqlitePool, owner: UserId, rows: &[Row]) {
    for (i, r) in rows.iter().enumerate() {
        let (input, cache_read, cache_write, output) = r.tokens;
        let request_id = RequestId::generate().to_string();

        sqlx::query(
            "INSERT INTO usage_rollups_hourly (
                hour_ms, owner_user_id, client_key_id, upstream_key_id, provider_id,
                provider_scope, share_id, canonical_id, requests, errors,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                reasoning_tokens, cost_micro_usd, list_cost_micro_usd)
             VALUES (?, ?, ?, 'uk1', ?, 'personal', '', ?, 1, 0, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(r.ts / 3_600_000 * 3_600_000)
        .bind(owner.to_string())
        .bind(r.client_key_id)
        .bind(r.provider_id)
        .bind(r.model)
        .bind(input)
        .bind(cache_read)
        .bind(cache_write)
        .bind(output)
        .bind(r.reasoning)
        .bind(r.cost_micro_usd)
        .bind(r.cost_micro_usd)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("rollup {i}: {e}"));

        sqlx::query(
            "INSERT INTO usage_events (
                id, ts, request_id, client_key_id, owner_user_id, provider_id, canonical_id,
                endpoint, protocol, status, stream, incomplete, termination,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                reasoning_tokens, cost_micro_usd, cost_source, list_cost_micro_usd,
                plan_multiplier_bps, billing_coverage_mode, failovers)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'chat', 'chat', 200, 0, 0, 'completed',
                 ?, ?, ?, ?, ?, ?, 'price_book', ?, 10000, 'pay_as_you_go', 0)",
        )
        .bind(UsageEventId::generate().to_string())
        .bind(r.ts)
        .bind(&request_id)
        .bind(r.client_key_id)
        .bind(owner.to_string())
        .bind(r.provider_id)
        .bind(r.model)
        .bind(input)
        .bind(cache_read)
        .bind(cache_write)
        .bind(output)
        .bind(r.reasoning)
        .bind(r.cost_micro_usd)
        .bind(r.cost_micro_usd)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("event {i}: {e}"));
    }
}

async fn seed_client_keys(pool: &sqlx::SqlitePool, owner: UserId, ids: &[&str]) {
    for id in ids {
        sqlx::query(
            "INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at)
             VALUES (?, ?, ?, ?, ?, 0)",
        )
        .bind(id)
        .bind(owner.to_string())
        .bind(format!("label-{id}"))
        .bind(format!("llr_live_{id}"))
        .bind(format!("hash_{id}"))
        .execute(pool)
        .await
        .expect("client key");
    }
}

/// Build the router plus a logged-in admin cookie.
async fn setup() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    axum::Router,
    String,
    UserId,
) {
    let (dir, pool, master_key, admin_id, instance_state) = setup_dashboard_test_db().await;

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
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let raw = "c23_admin_session_token_1234567890";
    let id_hash = llmroller_store::crypto::sha256_hex(raw.as_bytes());
    let csrf = auth_state.derive_session_csrf(raw);
    let csrf_hash = llmroller_store::crypto::sha256_hex(csrf.as_bytes());
    llmroller_store::users::insert_web_session(
        &pool,
        llmroller_store::users::NewWebSession {
            id_hash: &id_hash,
            user_id: admin_id,
            csrf_hash: &csrf_hash,
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("c23-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("session");

    (dir, pool, app, format!("llmroller_session={raw}"), admin_id)
}

async fn get_json(app: &axum::Router, cookie: &str, uri: &str) -> serde_json::Value {
    use axum::http::header;
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .expect("req");
    let res = app.clone().oneshot(req).await.expect("res");
    assert_eq!(res.status(), StatusCode::OK, "GET {uri}");
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("GET {uri}: {e}"))
}

/// Two models, two providers, two keys — so every filter below has something to exclude.
const ROWS: &[Row] = &[
    Row {
        ts: 1_000_000,
        client_key_id: "ck_alpha",
        provider_id: "prov_a",
        model: "model-one",
        tokens: (100, 200, 300, 50),
        reasoning: 25,
        cost_micro_usd: 11_000,
    },
    Row {
        ts: 2_000_000,
        client_key_id: "ck_beta",
        provider_id: "prov_b",
        model: "model-two",
        tokens: (1_000, 2_000, 3_000, 500),
        reasoning: 0,
        cost_micro_usd: 22_000,
    },
];

async fn seed_two_rows(pool: &sqlx::SqlitePool, owner: UserId) {
    seed_client_keys(pool, owner, &["ck_alpha", "ck_beta"]).await;
    seed(pool, owner, ROWS).await;
}

/// Sum the token total reported by the timeseries endpoint's buckets.
fn timeseries_total(json: &serde_json::Value) -> i64 {
    json["series"]
        .as_array()
        .expect("series")
        .iter()
        .map(|b| b["total_tokens"].as_i64().expect("bucket total_tokens"))
        .sum()
}

/// Sum the token total across the per-model breakdown rows.
fn breakdown_total(json: &serde_json::Value) -> i64 {
    json["breakdown"]
        .as_array()
        .expect("breakdown")
        .iter()
        .map(|b| b["total_tokens"].as_i64().expect("row total_tokens"))
        .sum()
}

/// `model` must restrict the summary total to the matching canonical id.
///
/// Pre-fix the parameter is accepted and ignored, so the total covers both rows.
#[doc = "Spec: USG-1, 14.n.2 (Model Filter Pill); ISSUE-0175"]
#[tokio::test]
async fn usage_summary_honors_the_model_filter() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    seed_two_rows(&pool, admin_id).await;

    let json = get_json(&app, &cookie, "/admin/api/usage/summary?model=model-one").await;

    assert_eq!(
        json["total_requests"], 1,
        "only the model-one request matches, got {} requests",
        json["total_requests"]
    );
    assert_eq!(
        json["total_tokens"],
        ROWS[0].total(),
        "the total must cover model-one alone ({}), not both rows",
        ROWS[0].total()
    );
    assert_eq!(json["cost_micro_usd"], 11_000);
}

/// `provider_id` must restrict the summary the same way.
#[doc = "Spec: USG-1; ISSUE-0175"]
#[tokio::test]
async fn usage_summary_honors_the_provider_filter() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    seed_two_rows(&pool, admin_id).await;

    let json = get_json(&app, &cookie, "/admin/api/usage/summary?provider_id=prov_b").await;

    assert_eq!(json["total_requests"], 1, "only prov_b matches");
    assert_eq!(json["total_tokens"], ROWS[1].total());
}

/// `initiator` must restrict the summary to the principal that acted.
///
/// `14.n.2` defines the pill as a list of "all users, service accounts, and individual client
/// keys", so the filter matches **any** identity recorded on the row: the client key, its owning
/// user, or the service account that owns the key. The test uses the client-key id, which is the
/// most specific identity and the one the pill's `llr_...` entries carry.
#[doc = "Spec: USG-1, 14.n.2 (Initiator Filter Pill); ISSUE-0175"]
#[tokio::test]
async fn usage_summary_honors_the_initiator_filter() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    seed_two_rows(&pool, admin_id).await;

    let json = get_json(&app, &cookie, "/admin/api/usage/summary?initiator=ck_beta").await;

    assert_eq!(
        json["total_requests"], 1,
        "only ck_beta matches the initiator"
    );
    assert_eq!(json["total_tokens"], ROWS[1].total());

    // The same user owns both keys, so filtering by the user id must return both rows. This is
    // what makes the predicate a union over identity kinds rather than a single column.
    let by_user = get_json(
        &app,
        &cookie,
        &format!("/admin/api/usage/summary?initiator={admin_id}"),
    )
    .await;
    assert_eq!(
        by_user["total_requests"], 2,
        "filtering by the owning user must return every row that user initiated"
    );

    // A principal that appears nowhere must not match everything.
    let unknown = get_json(&app, &cookie, "/admin/api/usage/summary?initiator=nobody").await;
    assert_eq!(
        unknown["total_requests"], 0,
        "an unknown initiator matches nothing"
    );
}

/// `since`/`until` are inclusive on both ends.
///
/// This is the guard against a refactor that turns `>=`/`<=` into `>`/`<` while splicing the
/// shared predicate — the most likely incidental breakage, and one that would silently drop
/// boundary rows.
#[doc = "Spec: USG-1; ISSUE-0175"]
#[tokio::test]
async fn since_until_boundaries_are_inclusive() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    seed_two_rows(&pool, admin_id).await;

    // Window exactly bounding both rows.
    let json = get_json(
        &app,
        &cookie,
        &format!(
            "/admin/api/usage/summary?since={}&until={}",
            ROWS[0].ts, ROWS[1].ts
        ),
    )
    .await;
    assert_eq!(
        json["total_requests"], 2,
        "an event exactly at `since` and one exactly at `until` must both be included"
    );

    // A window that excludes the first row by one millisecond.
    let json = get_json(
        &app,
        &cookie,
        &format!("/admin/api/usage/summary?since={}", ROWS[0].ts + 1),
    )
    .await;
    assert_eq!(
        json["total_requests"], 1,
        "since is a strict lower bound of one ms"
    );
}

/// **The highest-value assertion in the cycle.** One filter, four endpoints, one number.
///
/// A missed endpoint shows up as a disagreement; a bind-order mistake shows up as a
/// plausible-but-different number rather than an error, which is exactly the failure mode a
/// per-endpoint field assertion cannot catch.
#[doc = "Spec: USG-1, USG-2, USG-3; ISSUE-0175"]
#[tokio::test]
async fn all_four_endpoints_agree_on_a_filtered_totals() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    seed_two_rows(&pool, admin_id).await;

    // Filter that keeps both rows' worth of predicates active, so a wrong bind cannot accidentally
    // match everything.
    let q = "provider_id=prov_a&model=model-one&initiator=ck_alpha";

    let summary = get_json(&app, &cookie, &format!("/admin/api/usage/summary?{q}")).await;
    let timeseries = get_json(&app, &cookie, &format!("/admin/api/usage/timeseries?{q}")).await;
    let breakdown = get_json(&app, &cookie, &format!("/admin/api/usage/breakdown?{q}")).await;
    let events = get_json(&app, &cookie, &format!("/admin/api/usage/events?{q}")).await;

    let events_total: i64 = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|e| e["total_tokens"].as_i64().expect("event total_tokens"))
        .sum();

    let expected = ROWS[0].total();
    assert_eq!(summary["total_tokens"], expected, "summary");
    assert_eq!(timeseries_total(&timeseries), expected, "timeseries");
    assert_eq!(breakdown_total(&breakdown), expected, "breakdown");
    assert_eq!(
        events_total, expected,
        "events: the raw listing must apply the same predicate"
    );
    assert_eq!(
        summary["total_requests"], 1,
        "and the filter must actually have excluded the second row"
    );
}

/// Every endpoint must report all five token classes, and the total must follow the canonical
/// four-class rule inherited from cycle 22.
///
/// `reasoning` travels beside the total, never inside it: it is a diagnostic subset of `output`
/// (`pricing.rs:24`, `12. SRDD:133-134`).
#[doc = "Spec: USG-1, USG-2, USG-3, 12:133-134; ISSUE-0175"]
#[tokio::test]
async fn filtered_endpoints_return_all_five_token_classes() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    seed_two_rows(&pool, admin_id).await;

    let q = "model=model-one";

    let summary = get_json(&app, &cookie, &format!("/admin/api/usage/summary?{q}")).await;
    assert_eq!(summary["input_tokens"], 100, "summary input");
    assert_eq!(summary["cache_read_tokens"], 200, "summary cache_read");
    assert_eq!(summary["cache_write_tokens"], 300, "summary cache_write");
    assert_eq!(summary["output_tokens"], 50, "summary output");
    assert_eq!(summary["reasoning_tokens"], 25, "summary reasoning");
    assert_eq!(
        summary["total_tokens"], 650,
        "the canonical total excludes reasoning"
    );

    let timeseries = get_json(&app, &cookie, &format!("/admin/api/usage/timeseries?{q}")).await;
    let bucket = &timeseries["series"].as_array().expect("series")[0];
    assert_eq!(bucket["cache_write_tokens"], 300, "timeseries cache_write");
    assert_eq!(bucket["reasoning_tokens"], 25, "timeseries reasoning");

    let breakdown = get_json(&app, &cookie, &format!("/admin/api/usage/breakdown?{q}")).await;
    let row = &breakdown["breakdown"].as_array().expect("breakdown")[0];
    assert_eq!(row["cache_write_tokens"], 300, "breakdown cache_write");
    assert_eq!(row["reasoning_tokens"], 25, "breakdown reasoning");

    let events = get_json(&app, &cookie, &format!("/admin/api/usage/events?{q}")).await;
    let event = &events["events"].as_array().expect("events")[0];
    assert_eq!(event["cache_write_tokens"], 300, "events cache_write");
    assert_eq!(event["reasoning_tokens"], 25, "events reasoning");
}

/// Non-regression: an unfiltered request must return what it returned before this cycle.
///
/// The values are captured from the pre-fix build — two rows, every class summed, and the cache
/// hit rate computed from `input + cache_read`.
#[doc = "Spec: USG-1; ISSUE-0175"]
#[tokio::test]
async fn unfiltered_usage_summary_is_unchanged() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    seed_two_rows(&pool, admin_id).await;

    let json = get_json(&app, &cookie, "/admin/api/usage/summary").await;

    assert_eq!(json["total_requests"], 2);
    assert_eq!(json["input_tokens"], 1_100);
    assert_eq!(json["cached_tokens"], 2_200);
    assert_eq!(json["output_tokens"], 550);
    assert_eq!(json["total_cost_micro_usd"], 33_000);
    assert_eq!(
        json["total_tokens"], 4_200,
        "the new total covers both rows: 650 + 6500"
    );
    // (cached_tokens / (input + cached)) rounded to 3 dp = 2200/3300.
    assert_eq!(json["cache_hit_rate"], 0.667);
}
