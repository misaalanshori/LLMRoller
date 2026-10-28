//! Cycle 24 — the remaining read endpoints must honour the filters they accept.
//!
//! Cycle 23 fixed `/usage/summary|timeseries|breakdown|events`. These are the same defect in the
//! endpoints that were left: CSV export (`#143`), the admin audit log (`#144`), and the usage
//! ledger's `initiator` (`#142`).
//!
//! Spec: `AUD-2`, `SEC-6, `USG-1`, `USG-3`, `14.n.2`.
use super::*;

use llmroller_core::ids::{RequestId, UsageEventId};

/// A usage row for the inference-CSV and ledger fixtures.
struct UsageRow {
    ts: i64,
    client_key_id: &'static str,
    model: &'static str,
    status: i64,
    error_code: Option<&'static str>,
    tokens: i64,
    cost_micro_usd: i64,
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

async fn seed_usage(pool: &sqlx::SqlitePool, owner: UserId, rows: &[UsageRow]) {
    for r in rows {
        sqlx::query(
            "INSERT INTO usage_events (
                id, ts, request_id, client_key_id, owner_user_id, canonical_id, endpoint, protocol,
                status, stream, incomplete, termination,
                input_uncached_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                reasoning_tokens, cost_micro_usd, cost_source, list_cost_micro_usd,
                plan_multiplier_bps, billing_coverage_mode, failovers, error_code)
             VALUES (?, ?, ?, ?, ?, ?, 'chat', 'chat', ?, 0, 0, 'completed',
                 ?, 0, 0, 0, 0, ?, 'price_book', ?, 10000, 'pay_as_you_go', 0, ?)",
        )
        .bind(UsageEventId::generate().to_string())
        .bind(r.ts)
        .bind(RequestId::generate().to_string())
        .bind(r.client_key_id)
        .bind(owner.to_string())
        .bind(r.model)
        .bind(r.status)
        .bind(r.tokens)
        .bind(r.cost_micro_usd)
        .bind(r.cost_micro_usd)
        .bind(r.error_code)
        .execute(pool)
        .await
        .expect("usage event");
    }
}

/// One control-plane log row, for the API-stream CSV.
async fn seed_cp_log(
    pool: &sqlx::SqlitePool,
    actor: UserId,
    ts: i64,
    method: &str,
    path: &str,
    status: i64,
) {
    sqlx::query(
        "INSERT INTO control_plane_logs (id, ts, request_id, actor_user_id, method, path, status, latency_ms, ip_addr)
         VALUES (?, ?, ?, ?, ?, ?, ?, 5, '127.0.0.1')",
    )
    .bind(llmroller_core::ids::UsageEventId::generate().to_string())
    .bind(ts)
    .bind(RequestId::generate().to_string())
    .bind(actor.to_string())
    .bind(method)
    .bind(path)
    .bind(status)
    .execute(pool)
    .await
    .expect("control plane log");
}

/// `(ts, action, target_type, target_id, ip_addr)` for one audit row.
type AuditRow = (
    i64,
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
);

/// Append audit rows through the real chained-hash writer, so `seq` is monotonic.
///
/// Writing the table directly would be simpler but would bypass the hash chain; using the store
/// helper keeps the fixture honest about how audit rows come into existence.
async fn seed_audit(pool: &sqlx::SqlitePool, hmac: &[u8], entries: &[AuditRow]) {
    for (ts, action, target_type, target_id, ip) in entries {
        let mut tx = pool.begin().await.expect("tx");
        let id = llmroller_core::ids::UsageEventId::generate().to_string();
        llmroller_store::audit::append(
            &mut tx,
            hmac,
            llmroller_store::audit::AuditEntry {
                id: &id,
                ts: *ts,
                actor_user_id: None,
                ip_addr: *ip,
                action,
                target_type,
                target_id: *target_id,
                detail_json: Some("{}"),
            },
        )
        .await
        .expect("audit append");
        tx.commit().await.expect("commit");
    }
}

async fn setup() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    axum::Router,
    String,
    UserId,
    Vec<u8>,
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
    let mut api_state = ApiAppState::new(auth_state.clone(), master_key, "key-0".to_string());
    // A known audit HMAC key, so the fixture can append rows through the real chained-hash writer
    // instead of inserting into `audit_log` directly.
    let hmac = vec![0x24_u8; 32];
    api_state.audit_hmac_key = Some(llmroller_core::seams::secret_bytes(hmac.clone()));
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let raw = "c24_admin_session_token_1234567890";
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
            user_agent: Some("c24-agent"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("session");

    (
        dir,
        pool,
        app,
        format!("llmroller_session={raw}"),
        admin_id,
        hmac,
    )
}

/// Fetch a body as text, asserting 200 and surfacing the body on failure.
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

/// Count CSV data lines, ignoring the header and any comment/marker lines.
fn csv_data_rows(csv: &str) -> Vec<&str> {
    csv.lines()
        .skip_while(|l| l.starts_with("id,"))
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("TRUNCATED"))
        .collect()
}

// ---------------------------------------------------------------------------
// #143 — CSV export must honour every filter
// ---------------------------------------------------------------------------

/// The inference CSV must apply `status`, `model`, and `from`/`to`.
///
/// Pre-fix every branch issues a bare `LIMIT 5000` with no predicate, so the export contains all
/// four rows and an operator who asked for last week's errors receives everything.
#[doc = "Spec: USG-1, SEC-6; ISSUE-0143"]
#[tokio::test]
async fn csv_export_honours_every_filter() {
    let (_dir, pool, app, cookie, admin_id, _hmac) = setup().await;
    seed_client_keys(&pool, admin_id, &["ck_a"]).await;
    let base = 1_700_000_000_000_i64;
    seed_usage(
        &pool,
        admin_id,
        &[
            UsageRow {
                ts: base,
                client_key_id: "ck_a",
                model: "model-one",
                status: 500,
                error_code: Some("upstream_error"),
                tokens: 10,
                cost_micro_usd: 100,
            },
            UsageRow {
                ts: base + 1_000,
                client_key_id: "ck_a",
                model: "model-one",
                status: 200,
                error_code: None,
                tokens: 20,
                cost_micro_usd: 200,
            },
            UsageRow {
                ts: base + 2_000,
                client_key_id: "ck_a",
                model: "model-two",
                status: 500,
                error_code: Some("upstream_error"),
                tokens: 30,
                cost_micro_usd: 300,
            },
            UsageRow {
                ts: base + 3_000,
                client_key_id: "ck_a",
                model: "model-two",
                status: 200,
                error_code: None,
                tokens: 40,
                cost_micro_usd: 400,
            },
        ],
    )
    .await;

    // Unfiltered: all four.
    let all = get_text(&app, &cookie, "/admin/api/logs/export.csv?stream=inference").await;
    assert_eq!(
        csv_data_rows(&all).len(),
        4,
        "unfiltered export has every row"
    );

    // status=500 -> the two failures.
    let errors = get_text(
        &app,
        &cookie,
        "/admin/api/logs/export.csv?stream=inference&status=500",
    )
    .await;
    assert_eq!(
        csv_data_rows(&errors).len(),
        2,
        "status filter must exclude the two 200 rows, got:\n{errors}"
    );

    // model=model-one -> rows 1 and 2.
    let one = get_text(
        &app,
        &cookie,
        "/admin/api/logs/export.csv?stream=inference&model=model-one",
    )
    .await;
    assert_eq!(
        csv_data_rows(&one).len(),
        2,
        "model filter must exclude model-two, got:\n{one}"
    );

    // Combined: model-one AND status=500 -> exactly the first row.
    let both = get_text(
        &app,
        &cookie,
        "/admin/api/logs/export.csv?stream=inference&model=model-one&status=500",
    )
    .await;
    assert_eq!(
        csv_data_rows(&both).len(),
        1,
        "combined filters must intersect, got:\n{both}"
    );

    // Date window is inclusive at both ends.
    let windowed = get_text(
        &app,
        &cookie,
        &format!(
            "/admin/api/logs/export.csv?stream=inference&from={}&to={}",
            base,
            base + 1_000
        ),
    )
    .await;
    assert_eq!(
        csv_data_rows(&windowed).len(),
        2,
        "from/to are inclusive bounds covering the first two rows, got:\n{windowed}"
    );
}

/// The API-stream CSV must apply `method` and `status` to `control_plane_logs`.
///
/// This branch reads a different table with a different column set — no `model`, no
/// `client_key_id`, no `service_account_id` — which is why it cannot share the inference predicate
/// wholesale.
#[doc = "Spec: AUD-2, SEC-6; ISSUE-0143"]
#[tokio::test]
async fn api_logs_csv_export_honours_method_and_status() {
    let (_dir, pool, app, cookie, admin_id, _hmac) = setup().await;
    let base = 1_700_000_000_000_i64;
    seed_cp_log(&pool, admin_id, base, "GET", "/api/user/client-keys", 200).await;
    seed_cp_log(
        &pool,
        admin_id,
        base + 1,
        "POST",
        "/api/user/client-keys",
        400,
    )
    .await;
    seed_cp_log(
        &pool,
        admin_id,
        base + 2,
        "DELETE",
        "/api/user/client-keys/x",
        500,
    )
    .await;

    let all = get_text(&app, &cookie, "/admin/api/logs/export.csv?stream=api").await;
    assert_eq!(csv_data_rows(&all).len(), 3, "unfiltered API export");

    let posts = get_text(
        &app,
        &cookie,
        "/admin/api/logs/export.csv?stream=api&method=POST",
    )
    .await;
    assert_eq!(
        csv_data_rows(&posts).len(),
        1,
        "method filter must exclude GET and DELETE, got:\n{posts}"
    );

    let failed = get_text(
        &app,
        &cookie,
        "/admin/api/logs/export.csv?stream=api&status=500",
    )
    .await;
    assert_eq!(
        csv_data_rows(&failed).len(),
        1,
        "status filter must exclude 200 and 400, got:\n{failed}"
    );
}

/// A truncated export must say so.
///
/// The 5,000-row cap stays (an uncapped export is a memory hazard), but a *silently* truncated
/// export claims a completeness it does not have. Without this test, a fix that filters correctly
/// and still truncates silently passes every other assertion here.
#[doc = "Spec: SEC-6; ISSUE-0143"]
#[tokio::test]
async fn csv_export_reports_truncation_when_capped() {
    let (_dir, pool, app, cookie, admin_id, _hmac) = setup().await;
    seed_client_keys(&pool, admin_id, &["ck_a"]).await;
    let base = 1_700_000_000_000_i64;

    // Caller asks for 2 rows; more exist. This exercises the same reporting path as the hard cap
    // without inserting 5,000 fixtures.
    seed_usage(
        &pool,
        admin_id,
        &[
            UsageRow {
                ts: base,
                client_key_id: "ck_a",
                model: "m",
                status: 200,
                error_code: None,
                tokens: 1,
                cost_micro_usd: 1,
            },
            UsageRow {
                ts: base + 1,
                client_key_id: "ck_a",
                model: "m",
                status: 200,
                error_code: None,
                tokens: 1,
                cost_micro_usd: 1,
            },
            UsageRow {
                ts: base + 2,
                client_key_id: "ck_a",
                model: "m",
                status: 200,
                error_code: None,
                tokens: 1,
                cost_micro_usd: 1,
            },
        ],
    )
    .await;

    let capped = get_text(
        &app,
        &cookie,
        "/admin/api/logs/export.csv?stream=inference&limit=2",
    )
    .await;
    assert_eq!(
        csv_data_rows(&capped).len(),
        2,
        "the limit must be honoured, got:\n{capped}"
    );
    assert!(
        capped.contains("TRUNCATED"),
        "a capped export must carry a discoverable truncation marker, got:\n{capped}"
    );

    // An export that fits must NOT claim truncation.
    let exact = get_text(
        &app,
        &cookie,
        "/admin/api/logs/export.csv?stream=inference&limit=3",
    )
    .await;
    assert_eq!(csv_data_rows(&exact).len(), 3);
    assert!(
        !exact.contains("TRUNCATED"),
        "an export that fits exactly must not claim truncation, got:\n{exact}"
    );
}

/// Status-quo pin: the `SEC-6` formula-injection defence must survive a rewrite of this path.
#[doc = "Spec: SEC-6; ISSUE-0143"]
#[tokio::test]
async fn csv_export_still_sanitises_formulas() {
    let (_dir, pool, app, cookie, admin_id, _hmac) = setup().await;
    seed_client_keys(&pool, admin_id, &["ck_a"]).await;
    let base = 1_700_000_000_000_i64;
    seed_usage(
        &pool,
        admin_id,
        &[UsageRow {
            ts: base,
            client_key_id: "ck_a",
            model: "+CMD|/C calc",
            status: 200,
            error_code: Some("=1+1"),
            tokens: 1,
            cost_micro_usd: 1,
        }],
    )
    .await;

    let csv = get_text(&app, &cookie, "/admin/api/logs/export.csv?stream=inference").await;
    assert!(
        csv.contains("'+CMD|/C calc"),
        "a model id starting with '+' must be neutralised, got:\n{csv}"
    );
    assert!(
        csv.contains("'=1+1"),
        "an error code starting with '=' must be neutralised, got:\n{csv}"
    );
}

// ---------------------------------------------------------------------------
// #144 — the admin audit log must be interrogable
// ---------------------------------------------------------------------------

/// `action`, `target_type`, and `from`/`to` must filter the admin audit chain.
///
/// Pre-fix the handler takes no query parameters at all, so every call returns the newest 100 rows
/// regardless of what was asked.
#[doc = "Spec: AUD-2; ISSUE-0144"]
#[tokio::test]
async fn audit_log_honours_action_and_date_filters() {
    let (_dir, pool, app, cookie, _admin_id, hmac) = setup().await;
    let base = 1_700_000_000_000_i64;
    seed_audit(
        &pool,
        &hmac,
        &[
            (
                base,
                "share.approved",
                "share",
                Some("s1"),
                Some("127.0.0.1"),
            ),
            (
                base + 1_000,
                "key.created",
                "client_key",
                Some("k1"),
                Some("127.0.0.1"),
            ),
            (
                base + 2_000,
                "share.approved",
                "share",
                Some("s2"),
                Some("192.0.2.7"),
            ),
        ],
    )
    .await;

    let all = get_json(&app, &cookie, "/admin/api/audit-log").await;
    assert_eq!(all.as_array().expect("array").len(), 3, "unfiltered audit");

    let approved = get_json(&app, &cookie, "/admin/api/audit-log?action=share.approved").await;
    let rows = approved.as_array().expect("array");
    assert_eq!(
        rows.len(),
        2,
        "action filter must exclude key.created, got: {approved}"
    );
    assert!(rows.iter().all(|r| r["action"] == "share.approved"));

    let by_target = get_json(&app, &cookie, "/admin/api/audit-log?target_type=client_key").await;
    assert_eq!(
        by_target.as_array().expect("array").len(),
        1,
        "target_type filter must exclude the share rows, got: {by_target}"
    );

    // Inclusive date bounds.
    let windowed = get_json(
        &app,
        &cookie,
        &format!("/admin/api/audit-log?from={base}&to={}", base + 1_000),
    )
    .await;
    assert_eq!(
        windowed.as_array().expect("array").len(),
        2,
        "from/to are inclusive and cover the first two rows, got: {windowed}"
    );
}

/// The admin audit response must carry `ip_addr`, which the schema has as nullable `TEXT`.
#[doc = "Spec: AUD-2, SEC-6; ISSUE-0144"]
#[tokio::test]
async fn audit_log_returns_ip_addr() {
    let (_dir, pool, app, cookie, _admin_id, hmac) = setup().await;
    seed_audit(
        &pool,
        &hmac,
        &[(
            1_700_000_000_000,
            "share.approved",
            "share",
            Some("s1"),
            Some("192.0.2.7"),
        )],
    )
    .await;

    let json = get_json(&app, &cookie, "/admin/api/audit-log").await;
    let row = &json.as_array().expect("array")[0];
    assert_eq!(
        row["ip_addr"], "192.0.2.7",
        "the audit row must expose the recorded client IP, got: {row}"
    );
}

/// Paging by `seq` must cover every row exactly once, newest first.
///
/// A cursor that re-returns or skips rows is a compliance problem here rather than a display bug,
/// and a single-page test cannot see it.
#[doc = "Spec: AUD-2; ISSUE-0144"]
#[tokio::test]
async fn audit_log_cursor_paginates_without_duplicates() {
    let (_dir, pool, app, cookie, _admin_id, hmac) = setup().await;
    let base = 1_700_000_000_000_i64;
    let actions = ["a.one", "a.two", "a.three", "a.four", "a.five"];
    let entries: Vec<_> = actions
        .iter()
        .enumerate()
        .map(|(i, a)| (base + i as i64, *a, "thing", Some("t"), Some("127.0.0.1")))
        .collect();
    seed_audit(&pool, &hmac, &entries).await;

    let page1 = get_json(&app, &cookie, "/admin/api/audit-log?limit=2").await;
    let p1 = page1.as_array().expect("array");
    assert_eq!(p1.len(), 2, "page 1 size");
    // Newest first: seq descending means a.five then a.four.
    assert_eq!(p1[0]["action"], "a.five");
    assert_eq!(p1[1]["action"], "a.four");
    let cursor = p1[1]["seq"].as_i64().expect("seq");

    let page2 = get_json(
        &app,
        &cookie,
        &format!("/admin/api/audit-log?limit=2&cursor={cursor}"),
    )
    .await;
    let p2 = page2.as_array().expect("array");
    assert_eq!(p2.len(), 2, "page 2 size");
    assert_eq!(p2[0]["action"], "a.three");
    assert_eq!(p2[1]["action"], "a.two");

    let cursor2 = p2[1]["seq"].as_i64().expect("seq");
    let page3 = get_json(
        &app,
        &cookie,
        &format!("/admin/api/audit-log?limit=2&cursor={cursor2}"),
    )
    .await;
    let p3 = page3.as_array().expect("array");
    assert_eq!(p3.len(), 1, "page 3 has the remainder");
    assert_eq!(p3[0]["action"], "a.one");

    // Exactly once, in order.
    let paged: Vec<&str> = p1
        .iter()
        .chain(p2.iter())
        .chain(p3.iter())
        .map(|r| r["action"].as_str().expect("action"))
        .collect();
    assert_eq!(
        paged,
        vec!["a.five", "a.four", "a.three", "a.two", "a.one"],
        "concatenated pages must cover every row exactly once in seq DESC order"
    );
}

// ---------------------------------------------------------------------------
// #142 — the ledger's initiator filter
// ---------------------------------------------------------------------------

/// `initiator` must match any of the three principal kinds, per `14.n.2` and cycle 23.
///
/// Before this cycle `LedgerQuery::initiator` was declared, `#[allow(dead_code)]`d, and never
/// bound: the ledger answered the unfiltered question whatever the caller asked.
#[doc = "Spec: USG-3, 14.n.2; ISSUE-0142"]
#[tokio::test]
async fn usage_ledger_honours_the_initiator_filter() {
    let (_dir, pool, app, cookie, admin_id, _hmac) = setup().await;
    seed_client_keys(&pool, admin_id, &["ck_x", "ck_y"]).await;
    let base = 1_700_000_000_000_i64;
    seed_usage(
        &pool,
        admin_id,
        &[
            UsageRow {
                ts: base,
                client_key_id: "ck_x",
                model: "m",
                status: 200,
                error_code: None,
                tokens: 100,
                cost_micro_usd: 10,
            },
            UsageRow {
                ts: base + 1,
                client_key_id: "ck_y",
                model: "m",
                status: 200,
                error_code: None,
                tokens: 200,
                cost_micro_usd: 20,
            },
        ],
    )
    .await;

    // Unfiltered baseline.
    let all = get_json(&app, &cookie, "/admin/api/usage/ledger").await;
    assert_eq!(all["summary"]["total_tokens"], 300);

    // By client key: only that key's row.
    let by_key = get_json(&app, &cookie, "/admin/api/usage/ledger?initiator=ck_y").await;
    assert_eq!(
        by_key["summary"]["total_tokens"], 200,
        "filtering by client key must exclude the other key, got: {by_key}"
    );

    // By the owning user: both rows, since one user owns both keys.
    let by_user = get_json(
        &app,
        &cookie,
        &format!("/admin/api/usage/ledger?initiator={admin_id}"),
    )
    .await;
    assert_eq!(
        by_user["summary"]["total_tokens"], 300,
        "filtering by the owning user must return everything that user initiated"
    );

    // Unknown principal: nothing.
    let unknown = get_json(&app, &cookie, "/admin/api/usage/ledger?initiator=nobody").await;
    assert_eq!(
        unknown["summary"]["total_tokens"], 0,
        "an unknown initiator must match nothing, got: {unknown}"
    );
}

// ---------------------------------------------------------------------------
// Cross-check
// ---------------------------------------------------------------------------

/// Each filtered endpoint must agree with a direct `SELECT` over the same table.
///
/// This catches bind-order mistakes, which produce plausible numbers rather than errors. It is
/// cycle 23's cross-check pattern applied to the three endpoints fixed here.
#[doc = "Spec: USG-1, USG-3, AUD-2; ISSUE-0142, ISSUE-0143, ISSUE-0144"]
#[tokio::test]
async fn every_filtered_endpoint_agrees_with_a_direct_query() {
    let (_dir, pool, app, cookie, admin_id, hmac) = setup().await;
    seed_client_keys(&pool, admin_id, &["ck_x"]).await;
    let base = 1_700_000_000_000_i64;
    seed_usage(
        &pool,
        admin_id,
        &[
            UsageRow {
                ts: base,
                client_key_id: "ck_x",
                model: "m1",
                status: 200,
                error_code: None,
                tokens: 111,
                cost_micro_usd: 11,
            },
            UsageRow {
                ts: base + 1,
                client_key_id: "ck_x",
                model: "m2",
                status: 200,
                error_code: None,
                tokens: 222,
                cost_micro_usd: 22,
            },
        ],
    )
    .await;
    seed_audit(
        &pool,
        &hmac,
        &[
            (base, "x.one", "thing", None, None),
            (base + 1, "x.two", "thing", None, None),
        ],
    )
    .await;

    // Ledger, filtered by model, against a direct SUM.
    let direct_tokens: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(input_uncached_tokens), 0) FROM usage_events WHERE canonical_id = 'm2'",
    )
    .fetch_one(&pool)
    .await
    .expect("direct sum");
    let ledger = get_json(&app, &cookie, "/admin/api/usage/ledger?model=m2").await;
    assert_eq!(
        ledger["summary"]["total_tokens"].as_i64(),
        Some(direct_tokens),
        "the ledger's filtered total must equal the same SUM run directly"
    );

    // Audit log, filtered by action, against a direct COUNT.
    let direct_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'x.two'")
            .fetch_one(&pool)
            .await
            .expect("direct count");
    let audit = get_json(&app, &cookie, "/admin/api/audit-log?action=x.two").await;
    assert_eq!(
        audit.as_array().expect("array").len() as i64,
        direct_count,
        "the audit log's filtered count must equal the same COUNT run directly"
    );

    // CSV export, filtered by model, against the same direct figure.
    let csv = get_text(
        &app,
        &cookie,
        "/admin/api/logs/export.csv?stream=inference&model=m2",
    )
    .await;
    assert_eq!(
        csv_data_rows(&csv).len() as i64,
        1,
        "a model-filtered export must contain exactly the matching row, got:\n{csv}"
    );
}
