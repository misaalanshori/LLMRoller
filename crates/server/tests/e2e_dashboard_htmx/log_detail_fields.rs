//! Cycle 25 — the log detail drawer must project and render every inspector field (`14.m`).
//!
//! `get_log_detail_internal` uses `SELECT *`, so every column is already in the row; the
//! hand-written projection then discards most of them. This is cycle 22's pattern again: no schema
//! change, no query change, purely "the projection throws it away".
//!
//! Spec: `11.3`, `14.m` (Right-Hand Log Inspection Drawer 5-Section Spec), `SEC-6`, `USG-9`.
use super::*;

use llmroller_core::ids::{RequestId, UsageEventId};

/// One fully-populated `usage_events` row.
///
/// Every inspector field carries a **distinctive, obviously-fake** value so an assertion that
/// accidentally passes on a default cannot be mistaken for a real read. Header values are the kind
/// `SEC-6` allows through (diagnostic only, no credentials) — never a real `Authorization` value,
/// even a redacted one.
struct DetailRow {
    request_id: String,
    /// `client_location` from geo headers, gated on trusted proxies (`#158`).
    client_location: Option<&'static str>,
    /// Allow-listed diagnostic headers (`SEC-6`/`USG-9`).
    request_headers_json: Option<&'static str>,
    termination: &'static str,
    incomplete: i64,
    cost_source: &'static str,
    pin_outcome: Option<&'static str>,
    failovers: i64,
    error_code: Option<&'static str>,
    metadata_json: Option<&'static str>,
}

impl DetailRow {
    fn full() -> Self {
        Self {
            request_id: RequestId::generate().to_string(),
            client_location: Some("Singapore, SG"),
            request_headers_json: Some(
                r#"{"content-type":"application/json","x-opencode-session":"sess-abc"}"#,
            ),
            termination: "idle_timeout",
            incomplete: 1,
            cost_source: "estimated_incomplete",
            pin_outcome: Some("repin_recovery"),
            failovers: 2,
            error_code: Some("upstream_error"),
            metadata_json: Some(r#"{"trace_hint":"cycle25"}"#),
        }
    }
}

async fn seed_client_key(pool: &sqlx::SqlitePool, owner: UserId) -> String {
    let id = format!("ck_c25_{}", UsageEventId::generate());
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at)
         VALUES (?, ?, 'c25-key', 'llr_live_c25', ?, 0)",
    )
    .bind(&id)
    .bind(owner.to_string())
    .bind(format!("hash_{id}"))
    .execute(pool)
    .await
    .expect("client key");
    id
}

/// Insert the detail row. Returns the `usage_events.id` so a trace record can reference it.
async fn seed_detail(pool: &sqlx::SqlitePool, owner: UserId, row: &DetailRow) -> String {
    let client_key_id = seed_client_key(pool, owner).await;
    let id = UsageEventId::generate().to_string();
    sqlx::query(
        "INSERT INTO usage_events (
            id, ts, request_id, client_key_id, owner_user_id, canonical_id, endpoint, protocol,
            session_hash, status, stream, incomplete, termination, trace_dropped,
            input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
            reasoning_tokens, cost_micro_usd, cost_source, list_cost_micro_usd,
            plan_multiplier_bps, billing_coverage_mode, latency_ms, ttfb_ms, stop_reason,
            service_account_id, client_user_agent, client_location, request_headers_json,
            metadata_json, pin_outcome, failovers, error_code)
         VALUES (?, 1700000000000, ?, ?, ?, 'deepseek-v4.1-flash', 'chat', 'chat',
            'sess-hash-abc', 200, 1, ?, ?, 0,
            100, 200, 300, 50, 25, 5000, ?, 9000, 10000, 'subscription_plan',
            400, 90, 'stop', NULL, 'c25-agent', ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&row.request_id)
    .bind(&client_key_id)
    .bind(owner.to_string())
    .bind(row.incomplete)
    .bind(row.termination)
    .bind(row.cost_source)
    .bind(row.client_location)
    .bind(row.request_headers_json)
    .bind(row.metadata_json)
    .bind(row.pin_outcome)
    .bind(row.failovers)
    .bind(row.error_code)
    .execute(pool)
    .await
    .expect("usage event");
    id
}

/// Insert a matching `trace_records` row so the drawer has a trace to link to.
async fn seed_trace(
    pool: &sqlx::SqlitePool,
    owner: UserId,
    usage_event_id: &str,
    request_id: &str,
) {
    let client_key_id: String =
        sqlx::query_scalar("SELECT client_key_id FROM usage_events WHERE id = ?")
            .bind(usage_event_id)
            .fetch_one(pool)
            .await
            .expect("client key lookup");
    sqlx::query(
        "INSERT INTO trace_records (
            usage_event_id, request_id, ts, owner_user_id, client_key_id, session_hash,
            thread_id, canonical_id, protocol, truncated_content, payload_enc, master_key_id,
            expires_at)
         VALUES (?, ?, 1700000000000, ?, ?, 'sess-hash-abc', 'main', 'deepseek-v4.1-flash',
            'chat', 0, X'00', 'key-0', 9999999999999)",
    )
    .bind(usage_event_id)
    .bind(request_id)
    .bind(owner.to_string())
    .bind(&client_key_id)
    .execute(pool)
    .await
    .expect("trace record");
}

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
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let raw = "c25_admin_session_token_1234567890";
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
            user_agent: Some("c25-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("session");

    (dir, pool, app, format!("llmroller_session={raw}"), admin_id)
}

async fn get_text(app: &axum::Router, cookie: &str, uri: &str) -> String {
    use axum::http::header;
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .expect("req");
    let res = app.clone().oneshot(req).await.expect("res");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "GET {uri} failed: {}",
        String::from_utf8_lossy(&bytes)
    );
    String::from_utf8(bytes.to_vec()).expect("utf8")
}

async fn get_json(app: &axum::Router, cookie: &str, uri: &str) -> serde_json::Value {
    let body = get_text(app, cookie, uri).await;
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("GET {uri}: {e}: {body}"))
}

/// Every `14.m` inspector field the row carries must reach the response.
#[doc = "Spec: 11.3, 14.m, USG-9; ISSUE-0174"]
#[tokio::test]
async fn log_detail_returns_every_inspector_field() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let row = DetailRow::full();
    seed_detail(&pool, admin_id, &row).await;

    let json = get_json(
        &app,
        &cookie,
        &format!("/admin/api/logs/{}", row.request_id),
    )
    .await;

    assert_eq!(
        json["client_location"], "Singapore, SG",
        "14.m section 5 'Location' needs client_location, got: {json}"
    );
    assert_eq!(
        json["termination"], "idle_timeout",
        "termination is not projected"
    );
    assert_eq!(
        json["cost_source"], "estimated_incomplete",
        "cost_source is not projected"
    );
    assert_eq!(
        json["pin_outcome"], "repin_recovery",
        "pin_outcome is not projected"
    );
    assert_eq!(json["failovers"], 2, "14.m section 3 needs failovers > 0");
    assert_eq!(
        json["error_code"], "upstream_error",
        "error_code is not projected"
    );
}

/// Enum-valued columns come back as their **stored strings**, not remapped to display labels.
///
/// This is the test that stops someone prettifying them at the API boundary: the columns are
/// CHECK-constrained, and a later consumer comparing against the stored vocabulary would break
/// silently if the API layer invented its own words.
#[doc = "Spec: 14.m; ISSUE-0174"]
#[tokio::test]
async fn log_detail_maps_the_stored_enum_strings_verbatim() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let mut row = DetailRow::full();
    row.termination = "shutdown_cut";
    row.cost_source = "estimated_attempt";
    row.pin_outcome = Some("unpinned_capacity");
    seed_detail(&pool, admin_id, &row).await;

    let json = get_json(
        &app,
        &cookie,
        &format!("/admin/api/logs/{}", row.request_id),
    )
    .await;

    assert_eq!(json["termination"], "shutdown_cut");
    assert_eq!(json["cost_source"], "estimated_attempt");
    assert_eq!(json["pin_outcome"], "unpinned_capacity");
}

/// `incomplete` is an INTEGER 0/1 and must be a JSON boolean, not `0`/`1`.
#[doc = "Spec: 14.m; ISSUE-0174"]
#[tokio::test]
async fn log_detail_marks_incomplete_as_a_boolean() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;

    let mut yes = DetailRow::full();
    yes.incomplete = 1;
    seed_detail(&pool, admin_id, &yes).await;

    let mut no = DetailRow::full();
    no.incomplete = 0;
    seed_detail(&pool, admin_id, &no).await;

    let j_yes = get_json(
        &app,
        &cookie,
        &format!("/admin/api/logs/{}", yes.request_id),
    )
    .await;
    let j_no = get_json(&app, &cookie, &format!("/admin/api/logs/{}", no.request_id)).await;

    assert_eq!(j_yes["incomplete"], true, "1 must be the boolean true");
    assert_eq!(j_no["incomplete"], false, "0 must be the boolean false");
}

/// A corrupt `request_headers_json` must surface as raw text, never as `{}`.
///
/// `unwrap_or_default()`-ing a broken blob into an empty object would render "no headers" for a
/// value that exists and is damaged — the silent-empty failure mode this project keeps having to
/// fix. The raw text keeps the corruption visible.
#[doc = "Spec: 14.m (Request headers), SEC-6; ISSUE-0174"]
#[tokio::test]
async fn log_detail_preserves_a_corrupt_headers_blob_rather_than_emptying_it() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let mut row = DetailRow::full();
    row.request_headers_json = Some("{not json at all");
    seed_detail(&pool, admin_id, &row).await;

    let json = get_json(
        &app,
        &cookie,
        &format!("/admin/api/logs/{}", row.request_id),
    )
    .await;

    let headers = &json["request_headers"];
    assert_ne!(
        headers,
        &serde_json::json!({}),
        "a corrupt blob must not be silently emptied into an object"
    );
    assert_eq!(
        headers["parse_error"], true,
        "the response must mark the blob as unparseable rather than pretending it was empty"
    );
    assert_eq!(
        headers["raw"], "{not json at all",
        "the raw text must be preserved so the corruption is visible"
    );
}

/// A valid headers blob parses into a name → value object for the grid.
#[doc = "Spec: 14.m (Request headers), SEC-6; ISSUE-0174"]
#[tokio::test]
async fn log_detail_parses_a_valid_headers_blob_for_the_grid() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let row = DetailRow::full();
    seed_detail(&pool, admin_id, &row).await;

    let json = get_json(
        &app,
        &cookie,
        &format!("/admin/api/logs/{}", row.request_id),
    )
    .await;

    let headers = &json["request_headers"];
    assert_eq!(headers["parse_error"], false);
    let pairs = headers["pairs"].as_array().expect("pairs array");
    assert_eq!(pairs.len(), 2, "both allow-listed headers present");
    // Pairs are (name, value) so the renderer never needs to trust key ordering.
    assert!(
        pairs
            .iter()
            .any(|p| p[0] == "content-type" && p[1] == "application/json"),
        "content-type pair missing: {pairs:?}"
    );
}

/// With no trace record, the drawer must report the absence rather than a dead link.
///
/// Traces are opt-in (`ADR-9`), so *most* rows have none. A dead link in an inspector is worse
/// than an honest null.
#[doc = "Spec: 14.m, TRC-6, ADR-9; ISSUE-0174"]
#[tokio::test]
async fn log_detail_omits_a_trace_link_when_no_trace_exists() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let row = DetailRow::full();
    seed_detail(&pool, admin_id, &row).await;

    let json = get_json(
        &app,
        &cookie,
        &format!("/admin/api/logs/{}", row.request_id),
    )
    .await;

    assert_eq!(json["has_trace"], false);
    assert!(
        json["trace_href"].is_null(),
        "no trace means no link, got: {}",
        json["trace_href"]
    );
}

/// With a trace record, the drawer must return a link built from the real join key.
///
/// The pair with the omission test is what stops a hardcoded answer satisfying either alone.
#[doc = "Spec: 14.m, TRC-6, ADR-9; ISSUE-0174"]
#[tokio::test]
async fn log_detail_includes_a_trace_link_when_one_exists() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let row = DetailRow::full();
    let usage_event_id = seed_detail(&pool, admin_id, &row).await;
    seed_trace(&pool, admin_id, &usage_event_id, &row.request_id).await;

    let json = get_json(
        &app,
        &cookie,
        &format!("/admin/api/logs/{}", row.request_id),
    )
    .await;

    assert_eq!(json["has_trace"], true, "the trace row exists");
    let href = json["trace_href"]
        .as_str()
        .expect("trace_href must be a string when a trace exists");
    assert!(
        href.contains(&row.request_id),
        "the link must address this request, got: {href}"
    );
}

/// **The security-relevant test of the cycle.** Attacker-influenced header values must render inert.
///
/// `request_headers_json` is the first place header values reach the drawer. The drawer is
/// server-rendered through HTMX (`partials.rs`), not JS-built, so the defence is Askama-style
/// escaping in the string formatter — this asserts the escaping actually happens rather than
/// relying on the renderer being correct by construction.
#[doc = "Spec: 14.m, SEC-6, SEC-19; ISSUE-0174"]
#[tokio::test]
async fn log_detail_escapes_header_values_in_the_render() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let mut row = DetailRow::full();
    row.request_headers_json = Some(r#"{"x-evil":"<script>alert(1)</script>","x-quote":"a\"b"}"#);
    seed_detail(&pool, admin_id, &row).await;

    let html = get_text(
        &app,
        &cookie,
        &format!("/admin/partials/logs/{}", row.request_id),
    )
    .await;

    assert!(
        !html.contains("<script>alert(1)</script>"),
        "a header value rendered a live <script> tag:\n{html}"
    );
    assert!(
        html.contains("&lt;script&gt;"),
        "the payload should be present but escaped:\n{html}"
    );
}

/// The rendered drawer must contain the sections `14.m` specifies, with real values.
///
/// A field projected into JSON and never rendered is not done — this is the "Return **and Render**"
/// half of the issue.
#[doc = "Spec: 14.m; ISSUE-0174"]
#[tokio::test]
async fn log_drawer_renders_the_new_sections() {
    let (_dir, pool, app, cookie, admin_id) = setup().await;
    let row = DetailRow::full();
    let usage_event_id = seed_detail(&pool, admin_id, &row).await;
    seed_trace(&pool, admin_id, &usage_event_id, &row.request_id).await;

    let html = get_text(
        &app,
        &cookie,
        &format!("/admin/partials/logs/{}", row.request_id),
    )
    .await;

    assert!(html.contains("Request headers"), "headers section missing");
    assert!(
        html.contains("x-opencode-session"),
        "an allow-listed header name must appear in the grid"
    );
    assert!(
        html.contains("Singapore, SG"),
        "client_location must render in section 5"
    );
    assert!(html.contains("idle_timeout"), "termination must render");
    assert!(
        html.contains("estimated_incomplete"),
        "cost_source must render as a badge/label"
    );
    assert!(html.contains("repin_recovery"), "pin_outcome must render");
    assert!(
        html.contains(&format!("/admin/api/traces/{}", row.request_id)),
        "the trace link must point at the trace endpoint"
    );
    assert!(
        html.contains("Incomplete"),
        "the incomplete badge must render"
    );
}
