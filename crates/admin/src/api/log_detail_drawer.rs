//! The log detail drawer renderer (`14.m`).
//!
//! Extracted from `partials.rs` when that file crossed the 1600-line `MNT-3` ceiling. The split is
//! along a real seam rather than at an arbitrary line count: this renderer owns one whole concern —
//! the right-hand inspection drawer — and it is the only renderer that interpolates
//! **attacker-influenced** text (allow-listed request header values), so keeping it in a file of its
//! own makes the escaping auditable in one place.
//!
//! Every interpolated value below goes through `escape_html`. Header names **and** values are both
//! escaped: this grid is the first place header contents reach the DOM.
//!
//! Spec: `14.m`, `SEC-6`, `SEC-19`, `USG-9`.

use super::*;

pub(crate) fn render_log_detail_drawer_html(json: &serde_json::Value) -> String {
    let req_id = escape_html(
        json.get("request_id")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    // `ISSUE-0228` sibling sweep: `usage_events.status` is `NOT NULL`, so this key is always
    // present in a real payload -- but defaulting a *missing* status to `200` invents a **success**
    // for the one field an inspector reads first. `0` is not a valid HTTP status and reads as
    // "unknown" rather than as a healthy response.
    let status = json.get("status").and_then(|v| v.as_i64()).unwrap_or(0);
    let model = escape_html(json.get("model").and_then(|v| v.as_str()).unwrap_or("-"));
    let ts = json.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
    let latency_ms = json
        .get("latency_ms")
        .and_then(|v| v.as_i64())
        .map(|l| format!("{l} ms"))
        .unwrap_or_else(|| "—".to_string());
    let ttfb_ms = json
        .get("ttfb_ms")
        .and_then(|v| v.as_i64())
        .map(|l| format!("{l} ms"))
        .unwrap_or_else(|| "—".to_string());
    let endpoint = escape_html(json.get("endpoint").and_then(|v| v.as_str()).unwrap_or("-"));
    let protocol = escape_html(json.get("protocol").and_then(|v| v.as_str()).unwrap_or("-"));
    let stream = json
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let stop_reason = escape_html(
        json.get("stop_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("—"),
    );

    let tokens = json.get("tokens");
    let input_tokens = tokens
        .and_then(|t| t.get("input"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output_tokens = tokens
        .and_then(|t| t.get("output"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let reasoning_tokens = tokens
        .and_then(|t| t.get("reasoning"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cache_read = tokens
        .and_then(|t| t.get("cache_read"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cache_write = tokens
        .and_then(|t| t.get("cache_write"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let cost_obj = json.get("cost");
    let cost_usd = cost_obj
        .and_then(|c| c.get("cost_micro_usd"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        / 1_000_000.0;
    let list_cost_usd = cost_obj
        .and_then(|c| c.get("list_cost_micro_usd"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        / 1_000_000.0;
    let coverage_mode = escape_html(
        cost_obj
            .and_then(|c| c.get("billing_coverage_mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("pay_as_you_go"),
    );

    let identity = json.get("identity");
    let client_key = escape_html(
        identity
            .and_then(|i| i.get("client_key_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("-"),
    );
    let sa_id = escape_html(
        identity
            .and_then(|i| i.get("service_account_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("-"),
    );

    let session_hash = escape_html(
        json.get("session_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("-"),
    );
    let user_agent = escape_html(
        json.get("client_user_agent")
            .and_then(|v| v.as_str())
            .unwrap_or("-"),
    );

    // `14.m` section 5 "Location": geo headers are gated on trusted proxies (`#158`), so this is
    // genuinely absent on most deployments — a dash, never an invented value.
    let client_location = escape_html(
        json.get("client_location")
            .and_then(|v| v.as_str())
            .unwrap_or("—"),
    );

    // Stored enum strings, rendered verbatim. `14.m` asks for a `cost_source` badge and
    // `termination`/`incomplete` badges; the *values* stay the column's own vocabulary.
    let termination = escape_html(
        json.get("termination")
            .and_then(|v| v.as_str())
            .unwrap_or("—"),
    );
    let cost_source = escape_html(
        json.get("cost_source")
            .and_then(|v| v.as_str())
            .unwrap_or("—"),
    );
    let pin_outcome = escape_html(
        json.get("pin_outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("—"),
    );
    let error_code = escape_html(
        json.get("error_code")
            .and_then(|v| v.as_str())
            .unwrap_or("—"),
    );
    let failovers = json.get("failovers").and_then(|v| v.as_i64()).unwrap_or(0);
    let incomplete = json
        .get("incomplete")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // `14.m` section 6: the headers grid. Every name **and** value is escaped here: header values
    // are attacker-influenced, and this is the first place they reach the drawer.
    let headers_html = render_headers_grid(json.get("request_headers"));

    // `ISSUE-0228`: an empty `usage_attempts` set must not be rendered as an attempt. It used to
    // emit `<li>1 attempt (served)</li>` -- an invented count *and* an invented outcome.
    //
    // What empty actually means, read from the write path: `usage_writer::enqueue` takes
    // `(UsageEventRecord, Vec<UsageAttemptRecord>)`, and `proxy/src/auth.rs` and
    // `orchestrator/state_bound.rs` pass `Vec::new()` for requests rejected before an upstream key
    // was chosen, and for state-bound short-circuits. So empty means **no upstream attempt was
    // made** -- not "attempts failed" and not "recording is missing" (the event row exists because
    // accounting ran). The wording below claims exactly that and no more.
    let mut attempts_html = String::new();
    if let Some(attempts) = json.get("attempts").and_then(|a| a.as_array()) {
        for a in attempts {
            let att_no = a.get("attempt_no").and_then(|v| v.as_i64()).unwrap_or(1);
            let outcome = escape_html(a.get("outcome").and_then(|v| v.as_str()).unwrap_or("-"));
            let key = escape_html(
                a.get("upstream_key_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
            );
            attempts_html.push_str(&format!(
                "<li>Attempt {att_no}: {outcome} (key: {key})</li>"
            ));
        }
    }
    if attempts_html.is_empty() {
        // Same rule this file already applies to `client_location`: state the absence, do not
        // invent a value. Distinguishable at a glance from "one attempt that succeeded".
        attempts_html =
            "<li class=\"text-muted\">No upstream attempt was recorded for this request.</li>"
                .to_string();
    }

    // Traces are opt-in (`ADR-9`), so most rows have none. When there is no trace the section
    // renders nothing rather than a dead link — `trace_href` is only set when a `trace_records`
    // row exists, so this cannot invent one.
    let trace_link_html = json
        .get("trace_href")
        .and_then(|v| v.as_str())
        .map(|href| {
            format!(
                "<p><a class=\"btn btn-xs btn-outline\" href=\"{}\">Inspect content trace</a></p>",
                escape_html(href)
            )
        })
        .unwrap_or_default();

    format!(
        "<div class=\"drawer-header\">\
            <div><strong>Log Detail</strong> <span class=\"badge badge-info\">{status}</span></div>\
            <button class=\"btn btn-xs btn-ghost\" data-action=\"close-drawer\">&times;</button>\
        </div>\
        <div class=\"drawer-body\">\
            <div class=\"drawer-section\">\
                <h2>{model}</h2>\
                <p class=\"text-muted\">{ts} · {latency_ms}</p>\
            </div>\
            <div class=\"drawer-section\">\
                <h4>1. Request</h4>\
                <div class=\"key-val-list\">\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Request ID:</span><code>{req_id}</code></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">HTTP status:</span><span>{status}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Stop reason:</span><span>{stop_reason}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Endpoint:</span><code>{endpoint}</code></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Protocol:</span><span>{protocol}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Streaming:</span><span>{}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Time to first token:</span><span>{ttfb_ms}</span></div>\
                </div>\
            </div>\
            <div class=\"drawer-section\">\
                <h4>2. Model</h4>\
                <div class=\"key-val-list\">\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Canonical Model:</span><span>{model}</span></div>\
                </div>\
            </div>\
            <div class=\"drawer-section\">\
                <h4>3. Tokens &amp; Cost</h4>\
                <div class=\"key-val-list\">\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Input tokens:</span><span>{input_tokens}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Output tokens:</span><span>{output_tokens}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Reasoning tokens:</span><span>{reasoning_tokens}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Cache read:</span><span>{cache_read}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Cache write:</span><span>{cache_write}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Billed cost:</span><span>${cost_usd:.6}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">List cost:</span><span>${list_cost_usd:.6}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Cost source:</span><span class=\"badge badge-info\">{cost_source}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Coverage mode:</span><span class=\"badge badge-info\">{coverage_mode}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Failovers:</span><span>{failovers}</span></div>\
                </div>\
            </div>\
            <div class=\"drawer-section\">\
                <h4>4. Identity</h4>\
                <div class=\"key-val-list\">\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Client key:</span><code>{client_key}</code></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Service account:</span><span>{sa_id}</span></div>\
                </div>\
            </div>\
            <div class=\"drawer-section\">\
                <h4>5. Client</h4>\
                <div class=\"key-val-list\">\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Session hash:</span><code>{session_hash}</code></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">User Agent:</span><span>{user_agent}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Location:</span><span>{client_location}</span></div>\
                </div>\
            </div>\
            <div class=\"drawer-section\">\
                <h4>6. Request headers</h4>\
                {headers_html}\
            </div>\
            <div class=\"drawer-section\">\
                <h4>7. Upstream Attempts</h4>\
                <ul class=\"list-compact\">{attempts_html}</ul>\
            </div>\
            <div class=\"drawer-section\">\
                <h4>8. Trace & Diagnostics</h4>\
                <div class=\"key-val-list\">\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Termination:</span><span class=\"badge badge-info\">{termination}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Incomplete:</span><span class=\"badge badge-warning\">{}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Pin outcome:</span><span>{pin_outcome}</span></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Error code:</span><code>{error_code}</code></div>\
                    <div class=\"key-val-row\"><span class=\"key-val-label\">Has Content Trace:</span><span>{}</span></div>\
                </div>\
                {trace_link_html}\
            </div>\
            </div>\
        </div>",
        if stream { "Yes" } else { "No" },
        if incomplete { "Incomplete" } else { "Complete" },
        if json
            .get("has_trace")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            "Yes"
        } else {
            "No"
        }
    )
}

/// Render the `14.m` section 6 headers grid from the parsed `request_headers` object.
///
/// Both name and value are escaped: header values are attacker-influenced, and this is the first
/// place they reach the drawer. A `parse_error` blob renders the raw text in a `<pre>` (also
/// escaped) with a warning, so a corrupt value is visible rather than silently shown as "none".
fn render_headers_grid(headers: Option<&serde_json::Value>) -> String {
    let Some(headers) = headers else {
        return "<p class=\"text-muted\">None recorded.</p>".to_string();
    };

    if headers
        .get("parse_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let raw = escape_html(
            headers
                .get("raw")
                .and_then(|v| v.as_str())
                .unwrap_or_default(),
        );
        return format!(
            "<p class=\"text-danger\">Headers could not be parsed; showing the stored value.</p>\
             <pre class=\"code-block\">{raw}</pre>"
        );
    }

    let Some(pairs) = headers.get("pairs").and_then(|v| v.as_array()) else {
        return "<p class=\"text-muted\">None recorded.</p>".to_string();
    };
    if pairs.is_empty() {
        return "<p class=\"text-muted\">None recorded.</p>".to_string();
    }

    let mut rows = String::new();
    for pair in pairs {
        let name = escape_html(pair.get(0).and_then(|v| v.as_str()).unwrap_or("-"));
        let value = escape_html(pair.get(1).and_then(|v| v.as_str()).unwrap_or("-"));
        rows.push_str(&format!(
            "<tr><td><code>{name}</code></td><td><code>{value}</code></td></tr>"
        ));
    }
    format!("<table class=\"key-val-table\"><tbody>{rows}</tbody></table>")
}

pub(crate) async fn get_partial_user_logs_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LogsQuery>,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let uid = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let is_api = query.stream.as_deref() == Some("api");
    let json_res = get_logs_internal(&state, Some(uid.to_string()), query).await?;
    let html = render_logs_table_html(&json_res.0, is_api, "/app/partials/logs");
    Ok(Html(html))
}

pub(crate) async fn get_partial_user_log_detail_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let detail_json = get_log_detail_internal(&state, &actor, &request_id).await?;
    let html = render_log_detail_drawer_html(&detail_json.0);
    Ok(Html(html))
}

pub(crate) async fn get_partial_admin_logs_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LogsQuery>,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    let is_api = query.stream.as_deref() == Some("api");
    let json_res = get_logs_internal(&state, None, query).await?;
    let html = render_logs_table_html(&json_res.0, is_api, "/admin/partials/logs");
    Ok(Html(html))
}

pub(crate) async fn get_partial_admin_log_detail_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let detail_json = get_log_detail_internal(&state, &actor, &request_id).await?;
    let html = render_log_detail_drawer_html(&detail_json.0);
    Ok(Html(html))
}
