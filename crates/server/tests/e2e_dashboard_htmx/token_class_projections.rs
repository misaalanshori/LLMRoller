//! Cycle 22 — the admin read projections must not drop token classes that the storage layer
//! already records.
//!
//! Three screens read the same `usage_events` row: the Logs Explorer (`get_logs_internal`), the
//! Usage Ledger (`get_usage_ledger_internal`), and the usage-event breakdown popover
//! (`get_usage_event_breakdown_internal`). Before this cycle each computed `total_tokens` its own
//! way, or dropped a class outright, so the same request reported different numbers depending on
//! which screen an operator opened.
//!
//! Spec: `USG-1`, `USG-2`, `USG-3`, `USG-10`, `14.l` (Logs Explorer Columns), `14.n` (Usage Ledger).
use super::*;

use llmroller_core::ids::{ClientKeyId, RequestId, UsageEventId};

/// A client key row is mandatory: `usage_events.client_key_id` carries a foreign key.
async fn seed_client_key(pool: &sqlx::SqlitePool, owner: UserId) -> ClientKeyId {
    let key_id = ClientKeyId::generate();
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at)
         VALUES (?, ?, 'c22-key', 'llr_live_c22', 'hash_c22', 0)",
    )
    .bind(key_id.to_string())
    .bind(owner.to_string())
    .execute(pool)
    .await
    .expect("client key");
    key_id
}

/// The four priced token classes, in the order the canonical sum uses.
type Tokens = (i64, i64, i64, i64);

/// Insert one `usage_events` row with every token class under the caller's control.
///
/// Token columns are `INTEGER` (nullable) in the schema. This helper always writes numbers; the
/// `NULL` ("absent") case is written inline by the test that needs it.
#[allow(clippy::too_many_arguments)]
async fn insert_event(
    pool: &sqlx::SqlitePool,
    owner: UserId,
    client_key_id: &ClientKeyId,
    ts: i64,
    status: i64,
    tokens: Tokens,
    reasoning: i64,
    cost_micro_usd: i64,
) -> String {
    let (input, cache_read, cache_write, output) = tokens;
    let id = UsageEventId::generate().to_string();
    sqlx::query(
        "INSERT INTO usage_events (
            id, ts, request_id, client_key_id, owner_user_id, canonical_id, endpoint, protocol,
            status, stream, incomplete, termination,
            input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
            reasoning_tokens, cost_micro_usd, cost_source, list_cost_micro_usd,
            plan_multiplier_bps, billing_coverage_mode, failovers)
         VALUES (?, ?, ?, ?, ?, 'deepseek-v4.1-flash', 'chat', 'chat',
                 ?, 0, 0, 'completed',
                 ?, ?, ?, ?,
                 ?, ?, 'price_book', ?, 10000, 'pay_as_you_go', 0)",
    )
    .bind(&id)
    .bind(ts)
    .bind(RequestId::generate().to_string())
    .bind(client_key_id.to_string())
    .bind(owner.to_string())
    .bind(status)
    .bind(input)
    .bind(cache_read)
    .bind(cache_write)
    .bind(output)
    .bind(reasoning)
    .bind(cost_micro_usd)
    .bind(cost_micro_usd)
    .execute(pool)
    .await
    .expect("usage event");
    id
}

/// Build the router plus a logged-in admin cookie against a fresh database.
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
    let raw = "c22_admin_session_token_1234567890";
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
            user_agent: Some("c22-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("session");

    let cookie = format!("llmroller_session={raw}");
    (dir, pool, app, cookie, admin_id)
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
    serde_json::from_slice(&bytes).expect("json")
}

/// `total_tokens` must include every class the provider reported.
///
/// The canonical definition lives in the storage layer and is
/// `input_uncached + cache_read + cache_write + output`:
/// `usage.rs::get_usage_ledger_internal` and the overview KPI both use exactly that, and
/// `calculate_cost_micro_usd` prices exactly those four.
///
/// **`reasoning` is deliberately excluded**, because it is a *subset of* `output`, not a fifth
/// class: `TokenUsage::reasoning` is documented as "diagnostic subset of `output`", and
/// `12. SRDD:133-134` states `output_tokens` "includes reasoning". Adding it would double-count.
/// The issue's own suggestion to "sum all 5" would therefore over-report; see the cycle report.
///
/// With input=100, cache_read=200, cache_write=300, output=50, reasoning=25 the total is 650.
#[doc = "Spec: USG-1, 14.l (Tokens column)"]
#[tokio::test]
async fn logs_event_total_tokens_sums_every_class() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let key = seed_client_key(&pool, admin_id).await;
    let now = llmroller_core::seams::SystemClock::new().now_ms();
    insert_event(
        &pool,
        admin_id,
        &key,
        now,
        200,
        (100, 200, 300, 50),
        25,
        5000,
    )
    .await;

    let json = get_json(&app, &cookie, "/admin/api/logs").await;
    let rows = json["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "exactly the row just inserted");

    assert_eq!(
        rows[0]["total_tokens"], 650,
        "total must be input(100) + cache_read(200) + cache_write(300) + output(50); \
         the pre-fix projection reported input+output = 150"
    );
    assert_eq!(rows[0]["tokens_formatted"], "650");
}

/// A row that carried real spend must show it, whatever its status.
///
/// Cycles 10/11 established that a rejected request can still be charged (a charged timeout, an
/// embedded-200 rejection). The projection hid exactly those rows behind `if status == 200`.
#[doc = "Spec: USG-1, USG-10, 14.l (Cost column)"]
#[tokio::test]
async fn logs_event_shows_cost_on_a_rejected_row() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let key = seed_client_key(&pool, admin_id).await;
    let now = llmroller_core::seams::SystemClock::new().now_ms();
    // 20000 micro-USD = $0.02, which `format_cost_usd` renders as "$0.0200".
    insert_event(
        &pool,
        admin_id,
        &key,
        now,
        429,
        (100, 200, 300, 50),
        25,
        20_000,
    )
    .await;

    let json = get_json(&app, &cookie, "/admin/api/logs").await;
    let row = &json["rows"].as_array().expect("rows")[0];

    let cost = row["cost_formatted"].as_str().expect("cost_formatted");
    assert_ne!(
        cost, "—",
        "a 429 row that was charged $0.02 must not render as absent"
    );
    assert_eq!(
        cost, "$0.0200",
        "the displayed amount must be the real cost"
    );

    assert_eq!(
        row["cost_micro_usd"], 20_000,
        "and the raw value travels alongside the formatted one"
    );
    assert_eq!(
        row["tokens_formatted"], "650",
        "tokens are equally informative on a rejected row"
    );
}

/// The complement: `—` must still mean "no data", not "zero".
///
/// This is the guard that stops a blanket removal of the `if status == 200` branches. A real `0`
/// is informative and renders; a `NULL` column is genuinely absent and renders `—`.
///
/// If the fix had simply deleted both guards, the NULL row below would render `0` and `$0.00000`
/// and this test would fail — which is the point of having it.
#[doc = "Spec: USG-1, 14.l"]
#[tokio::test]
async fn logs_event_renders_dash_only_when_the_column_is_absent() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let key = seed_client_key(&pool, admin_id).await;
    let now = llmroller_core::seams::SystemClock::new().now_ms();

    // (a) A genuine zero on a rejected row: informative, must render as zero.
    insert_event(&pool, admin_id, &key, now, 403, (0, 0, 0, 0), 0, 0).await;

    // (b) A row whose token columns are SQL NULL: genuinely absent, must render `—`.
    let null_id = UsageEventId::generate().to_string();
    sqlx::query(
        "INSERT INTO usage_events (
            id, ts, request_id, client_key_id, owner_user_id, canonical_id, endpoint,
            status, stream, incomplete, termination,
            input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
            reasoning_tokens, cost_micro_usd, cost_source, list_cost_micro_usd,
            plan_multiplier_bps, billing_coverage_mode, failovers)
         VALUES (?, ?, ?, ?, ?, 'deepseek-v4.1-flash', 'chat',
                403, 0, 0, 'completed',
                 NULL, NULL, NULL, NULL,
                 NULL, 0, 'fallback_rate', 0,
                 10000, 'pay_as_you_go', 0)",
    )
    .bind(&null_id)
    .bind(now + 1)
    .bind(RequestId::generate().to_string())
    .bind(key.to_string())
    .bind(admin_id.to_string())
    .execute(&pool)
    .await
    .expect("null event");

    let json = get_json(&app, &cookie, "/admin/api/logs").await;
    let rows = json["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);

    // Select by timestamp rather than position, so the test does not depend on the cursor order.
    let null_row = rows
        .iter()
        .find(|r| r["ts"].as_i64() == Some(now + 1))
        .expect("the NULL-column row");
    let zero_row = rows
        .iter()
        .find(|r| r["ts"].as_i64() == Some(now))
        .expect("the genuine-zero row");

    assert_eq!(
        null_row["tokens_formatted"], "—",
        "a NULL token column is absent data, not a zero"
    );
    assert_eq!(
        zero_row["tokens_formatted"], "0",
        "a real 0 on a rejected row is informative and must render, not hide behind a dash"
    );

    // Same rule for cost, which is NOT NULL in the schema but is 0 when nothing was charged.
    assert_eq!(
        zero_row["cost_formatted"], "$0.00000",
        "a genuinely-zero charge renders as zero on a rejected row"
    );
}

/// The ledger must report reasoning tokens from the row, not a hardcoded 0.
///
/// The `TokenUsage` literal in `get_usage_event_breakdown_internal` hardcodes `reasoning: 0`, so
/// the breakdown popover that reads that JSON can never show the class.
///
/// It is surfaced as a **diagnostic field, not a priced line item**: `reasoning` is a subset of
/// `output` (`12. SRDD:133-134`), so giving it its own subtotal would imply a charge that does not
/// exist and would double-count against the output line. The assertion pins both halves — the
/// value is real, and there is no `reasoning` line item.
#[doc = "Spec: USG-3, USG-10, 14.n; ISSUE-0176"]
#[tokio::test]
async fn usage_breakdown_reports_reasoning_tokens() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let key = seed_client_key(&pool, admin_id).await;
    let now = llmroller_core::seams::SystemClock::new().now_ms();
    let event_id = insert_event(
        &pool,
        admin_id,
        &key,
        now,
        200,
        (100, 200, 300, 50),
        25,
        5000,
    )
    .await;

    let json = get_json(
        &app,
        &cookie,
        &format!("/admin/api/usage/events/{event_id}/breakdown"),
    )
    .await;

    assert_eq!(
        json["reasoning_tokens"], 25,
        "the breakdown must read reasoning_tokens from the row; pre-fix this was a hardcoded 0"
    );
    assert!(
        json["line_items"]["reasoning"].is_null(),
        "reasoning is a subset of output and must not be priced as its own line item; \
         a fifth line item would double-count it"
    );
    assert_eq!(
        json["line_items"]["output"]["tokens"], 50,
        "the output line still carries the full completion count, reasoning included"
    );
}

/// The ledger's summary and per-row payload must carry `reasoning_tokens`.
///
/// #176's title names `get_usage_ledger_internal`, but the hardcoded `0` was in the *breakdown*
/// handler. The ledger persisted the omission a different way: its `SELECT` lists omit
/// `reasoning_tokens` entirely, so the class never reaches the strip or the rows.
#[doc = "Spec: USG-3, 14.n; ISSUE-0176"]
#[tokio::test]
async fn usage_ledger_reports_reasoning_tokens() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let key = seed_client_key(&pool, admin_id).await;
    let now = llmroller_core::seams::SystemClock::new().now_ms();
    insert_event(
        &pool,
        admin_id,
        &key,
        now,
        200,
        (100, 200, 300, 50),
        25,
        5000,
    )
    .await;

    let json = get_json(&app, &cookie, "/admin/api/usage/ledger").await;

    assert_eq!(
        json["summary"]["reasoning_tokens"], 25,
        "the summary strip must sum reasoning_tokens"
    );
    let rows = json["rows"].as_array().expect("rows");
    assert_eq!(
        rows[0]["reasoning_tokens"], 25,
        "each ledger row must carry its reasoning_tokens"
    );
}

/// **The highest-value assertion in the cycle.** One event, two screens, one total.
///
/// The root cause was never a single wrong sum — it was that two projections each own their own
/// definition, so they drift. This pins them together: if a future change alters one screen's
/// `total_tokens`, this fails and names the disagreement.
///
/// It compares all three screens that report a total for the same event.
#[doc = "Spec: USG-1, USG-3, USG-10, 14.l, 14.n"]
#[tokio::test]
async fn usage_ledger_totals_match_the_logs_projection() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let key = seed_client_key(&pool, admin_id).await;
    let now = llmroller_core::seams::SystemClock::new().now_ms();
    // A reasoning-heavy, cache-heavy row: exactly the shape that exposed the divergence.
    let event_id = insert_event(
        &pool,
        admin_id,
        &key,
        now,
        200,
        (100, 200, 300, 50),
        25,
        5000,
    )
    .await;

    let logs = get_json(&app, &cookie, "/admin/api/logs").await;
    let logs_total = logs["rows"][0]["total_tokens"]
        .as_i64()
        .expect("logs total_tokens");

    let ledger = get_json(&app, &cookie, "/admin/api/usage/ledger").await;
    let ledger_total = ledger["summary"]["total_tokens"]
        .as_i64()
        .expect("ledger total_tokens");

    // The third screen: the breakdown popover, whose line items must sum to the same total.
    let breakdown = get_json(
        &app,
        &cookie,
        &format!("/admin/api/usage/events/{event_id}/breakdown"),
    )
    .await;
    let items = &breakdown["line_items"];
    let breakdown_total: i64 = ["input_uncached", "cache_read", "cache_write", "output"]
        .iter()
        .map(|k| items[*k]["tokens"].as_i64().expect("line item tokens"))
        .sum();

    assert_eq!(
        logs_total, ledger_total,
        "the Logs Explorer and the Usage Ledger must report the same total for the same event \
         (logs={logs_total}, ledger={ledger_total})"
    );
    assert_eq!(
        breakdown_total, logs_total,
        "the breakdown popover's line items must sum to the same total as the logs projection \
         (breakdown={breakdown_total}, logs={logs_total})"
    );
    assert_eq!(
        logs_total, 650,
        "and that shared total is the canonical four-class sum"
    );
}

/// An upstream 5xx is a *failure*, not a rejection.
///
/// `#173` also names this: the projection labels every non-200 row "Rejected", so a gateway or
/// upstream fault — where the operator should be looking at the provider, not at admission — reads
/// as a policy decision about the caller's request. `14.l` describes rejected and failed rows as
/// sharing a colour but leaves the wording to the host, and `Termination` is the field that
/// distinguishes "the upstream broke" from "we refused".
#[doc = "Spec: 14.l (Status column), USG-1; ISSUE-0173"]
#[tokio::test]
async fn logs_event_labels_upstream_failure_as_failed_not_rejected() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let key = seed_client_key(&pool, admin_id).await;
    let now = llmroller_core::seams::SystemClock::new().now_ms();

    insert_event(&pool, admin_id, &key, now, 502, (0, 0, 0, 0), 0, 0).await; // upstream failure
    insert_event(&pool, admin_id, &key, now + 1, 429, (0, 0, 0, 0), 0, 0).await; // admission rejection

    let json = get_json(&app, &cookie, "/admin/api/logs").await;
    let rows = json["rows"].as_array().expect("rows");
    // Select by timestamp: the cursor orders on ULID `id`, which does not track `ts`.
    let recent = rows
        .iter()
        .find(|r| r["ts"].as_i64() == Some(now + 1))
        .expect("the 429 row");
    let older = rows
        .iter()
        .find(|r| r["ts"].as_i64() == Some(now))
        .expect("the 502 row");

    assert_eq!(
        recent["status_badge"], "429 Rejected",
        "a 4xx admission rejection keeps the Rejected label"
    );
    assert_eq!(
        older["status_badge"], "502 Failed",
        "a 5xx upstream failure must not be described as the caller's request being rejected"
    );
}
