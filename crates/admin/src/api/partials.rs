use super::*;
use sqlx::Row as _;

pub(crate) async fn get_partial_pins_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let is_admin = actor.is_admin();
    let uid = actor.user_id().map(|u| u.to_string()).unwrap_or_default();
    let now_ms = state.auth.clock.now_ms();

    // Same loader as the JSON view, so the table and `/api/user/pins` can never disagree about
    // which rows exist (`Section 14.t.1`; live `PinStore` entries merged with durable rows).
    let rows = load_active_pins(&state, &actor, now_ms).await?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"7\" class=\"text-center text-muted\">No active sessions pinned right now.</td></tr>".to_string()
        ));
    }

    let mut html = String::new();
    for r in rows {
        let hash = r.session_hash;
        let short_hash = if hash.len() > 12 { &hash[..12] } else { &hash };
        let short_hash = escape_html(short_hash);
        let clk = r.client_key_id;
        let short_clk = if clk.len() > 10 { &clk[..10] } else { &clk };
        let short_clk = escape_html(short_clk);
        let model: String = r.canonical_id.unwrap_or_else(|| "unresolved".to_string());
        let model = escape_html(&model);
        let pool_tier: i64 = r.pool_tier;
        let display_label = mask_key_label(
            is_admin,
            pool_tier,
            r.key_owner.as_deref(),
            Some(uid.as_str()),
            r.key_label,
        )
        .unwrap_or_else(|| "key".to_string());
        let key_label = escape_html(&display_label);
        let last_used: i64 = r.last_used_at;
        let time_str = format!(
            "{}s ago",
            (state.auth.clock.now_ms() - last_used).max(0) / 1000
        );
        let unpin_button = if is_admin {
            format!(
                "<button class=\"btn btn-xs btn-danger\" hx-delete=\"/admin/api/sessions/{hash}/pin\" hx-target=\"closest tr\" hx-swap=\"outerHTML\">Unpin</button>"
            )
        } else if model == "unresolved" {
            String::new()
        } else {
            format!(
                "<button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/pins/{clk}/{hash}/{model}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\">Unpin</button>"
            )
        };

        html.push_str(&format!(
            "<tr>\
                <td><code>{short_hash}...</code></td>\
                <td><code>{short_clk}...</code></td>\
                <td><strong>{model}</strong></td>\
                <td>{key_label}</td>\
                <td><span class=\"badge badge-info\">Tier {pool_tier}</span></td>\
                <td>{time_str}</td>\
                <td>\
                    <button class=\"btn btn-xs btn-primary\" data-action=\"preview-chat\" data-session-hash=\"{hash}\">Preview Chat</button>\
                    {unpin_button}\
                </td>\
            </tr>"
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_client_keys_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let rows = sqlx::query(
        "SELECT id, prefix, label, routing_scope, rate_limit_rpm, budget_micro_usd_month,
                disabled, paused_at, created_at
         FROM client_keys
         WHERE owner_user_id = ? AND revoked_at IS NULL
         ORDER BY created_at DESC",
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"7\" class=\"text-center text-muted\">No client keys issued yet. Click \"+ New Client Key\" above.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let prefix: String = r.try_get("prefix").unwrap_or_default();
        let label: String = r.try_get("label").unwrap_or_default();
        let scope: String = r.try_get("routing_scope").unwrap_or_default();
        let id = escape_html(&id);
        let prefix = escape_html(&prefix);
        let label = escape_html(&label);
        let scope = escape_html(&scope);
        let rpm: Option<i64> = r.try_get("rate_limit_rpm").unwrap_or_default();
        let rpm_str = rpm.map_or_else(|| "Unlimited".to_string(), |v| format!("{v} RPM"));
        let budget: Option<i64> = r.try_get("budget_micro_usd_month").unwrap_or_default();
        let budget_str = budget.map_or_else(
            || "No Cap".to_string(),
            |b| format!("${:.2}", b as f64 / 1_000_000.0),
        );
        let disabled: bool = r.try_get::<i64, _>("disabled").unwrap_or(0) != 0;
        let paused: bool = r
            .try_get::<Option<i64>, _>("paused_at")
            .unwrap_or_default()
            .is_some();
        let (status_badge, pause_btn) = if paused {
            (
                "<span class=\"badge badge-warning\">● Paused</span>",
                format!(
                    "<button class=\"btn btn-xs btn-secondary\" data-action=\"resume-client-key\" data-id=\"{id}\">Resume</button> "
                ),
            )
        } else if disabled {
            (
                "<span class=\"badge badge-danger\">● Disabled</span>",
                format!(
                    "<button class=\"btn btn-xs btn-secondary\" data-action=\"resume-client-key\" data-id=\"{id}\">Resume</button> "
                ),
            )
        } else {
            (
                "<span class=\"badge badge-success\">● Active</span>",
                format!(
                    "<button class=\"btn btn-xs btn-outline\" data-action=\"pause-client-key\" data-id=\"{id}\">Pause</button> "
                ),
            )
        };

        html.push_str(&format!(
            "<tr>\
                <td><code>{prefix}...</code></td>\
                <td><strong>{label}</strong></td>\
                <td><span class=\"badge badge-info\">{scope}</span></td>\
                <td>{rpm_str}</td>\
                <td>{budget_str}</td>\
                <td>{status_badge}</td>\
                <td>\
                    {pause_btn}\
                    <button class=\"btn btn-xs btn-outline\" data-action=\"rotate-client-key\" data-id=\"{id}\">Rotate</button> \
                    <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/client-keys/{id}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Revoke this client key?\">Revoke</button>\
                </td>\
            </tr>"
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_providers_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let prov_rows = sqlx::query(
        "SELECT id, name, kind, base_url, config_json, enabled FROM providers WHERE owner_user_id = ? ORDER BY created_at ASC",
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if prov_rows.is_empty() {
        return Ok(Html(
            "<p class=\"text-muted\">No personal providers configured yet. Click \"+ Add Provider\" above.</p>".to_string(),
        ));
    }

    let mut html = String::new();
    for p in prov_rows {
        let pid: String = p.try_get("id").unwrap_or_default();
        let name: String = p.try_get("name").unwrap_or_default();
        let kind: String = p.try_get("kind").unwrap_or_default();
        let base_url: String = p.try_get("base_url").unwrap_or_default();
        let config_json: String = p.try_get("config_json").unwrap_or_default();
        let preset_badge = ProviderConfig::from_json(&config_json)
            .preset_id
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                format!(
                    " <span class=\"badge badge-info\">{}</span>",
                    escape_html(&s)
                )
            })
            .unwrap_or_default();
        let pid = escape_html(&pid);
        let name = escape_html(&name);
        let kind = escape_html(&kind);
        let base_url = escape_html(&base_url);

        let key_rows = sqlx::query(
            "SELECT id, label, secret_last4, priority, strict_tier, enabled FROM upstream_keys WHERE provider_id = ?",
        )
        .bind(&pid)
        .fetch_all(&state.auth.pool)
        .await
        .unwrap_or_default();

        let mut keys_html = String::new();
        if key_rows.is_empty() {
            keys_html = "<p class=\"text-muted py-2-sm\">No upstream keys under this provider. Add one to enable routing.</p>".to_string();
        } else {
            keys_html.push_str("<table class=\"data-table table-sub\"><thead><tr><th>Label</th><th>Priority</th><th>Strict Tier</th><th>Health</th><th>Action</th></tr></thead><tbody>");
            for k in key_rows {
                let kid: String = k.try_get("id").unwrap_or_default();
                let klabel: String = k.try_get("label").unwrap_or_default();
                let last4: Option<String> = k.try_get("secret_last4").unwrap_or_default();
                let kid = escape_html(&kid);
                let klabel = escape_html(&klabel);
                let mask = last4.map_or_else(|| "sealed".to_string(), |l| format!("••••{l}"));
                let mask = escape_html(&mask);
                let prio: i64 = k.try_get("priority").unwrap_or(100);
                let tier: i64 = k.try_get("strict_tier").unwrap_or(1);
                let enabled: bool = k.try_get::<i64, _>("enabled").unwrap_or(1) == 1;

                keys_html.push_str(&format!(
                    "<tr>\
                        <td><strong>{klabel}</strong> <span class=\"text-muted\">({mask})</span></td>\
                        <td>{prio}</td>\
                        <td>Tier {tier}</td>\
                        <td><span class=\"badge badge-{}\">{}</span></td>\
                        <td>\
                            <button class=\"btn btn-xs btn-ghost\" data-action=\"test-key\" data-key-id=\"{kid}\">Test</button><button class=\"btn btn-xs btn-outline\" data-modal-open=\"modal-budget-window\" data-key-id=\"{kid}\">Budget</button>\
                            <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/upstream-keys/{kid}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Remove this upstream key?\">Remove</button>\
                        </td>\
                    </tr>",
                    if enabled { "success" } else { "danger" },
                    if enabled { "Active" } else { "Disabled" }
                ));
            }
            keys_html.push_str("</tbody></table>");
        }

        html.push_str(&format!(
            "<div class=\"provider-box\">\
                <div class=\"provider-header\">\
                    <div>\
                        <h4 class=\"prov-title\">{name} <span class=\"badge badge-info\">{kind}</span>{preset_badge}</h4>\
                        <div class=\"prov-url-mono\">{base_url}</div>\
                    </div>\
                    <div>\
                        <button class=\"btn btn-xs btn-primary\" data-modal-open=\"modal-new-upstream-key\" data-provider-id=\"{pid}\" data-provider-name=\"{name}\">+ Add Key</button>\
                        <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/providers/{pid}\" hx-target=\"closest .provider-box\" hx-swap=\"outerHTML\" hx-confirm=\"Delete this provider and all attached keys?\">Delete</button>\
                    </div>\
                </div>\
                <div class=\"keys-sublist\">{keys_html}</div>\
            </div>"
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_shares_outgoing_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let rows = sqlx::query(
        "SELECT id, grantee_user_id, allowed_models, max_monthly_micro_usd, enabled
         FROM provider_shares WHERE owner_user_id = ? ORDER BY created_at DESC",
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"5\" class=\"text-center text-muted\">No outgoing shares granted yet. Click \"+ Grant Share\" above.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let grantee: String = r.try_get("grantee_user_id").unwrap_or_default();
        let short_grantee = if grantee.len() > 10 {
            &grantee[..10]
        } else {
            &grantee
        };
        let short_grantee = escape_html(short_grantee);
        let id = escape_html(&id);
        let models: Option<String> = r.try_get("allowed_models").unwrap_or_default();
        let models_str = models.unwrap_or_else(|| "All".to_string());
        let models_str = escape_html(&models_str);
        let cap: Option<i64> = r.try_get("max_monthly_micro_usd").unwrap_or_default();
        let cap_str = cap.map_or_else(
            || "No Cap".to_string(),
            |c| format!("${:.2}", c as f64 / 1_000_000.0),
        );
        let enabled: bool = r.try_get::<i64, _>("enabled").unwrap_or(1) == 1;

        html.push_str(&format!(
            "<tr>\
                <td><code>{short_grantee}...</code></td>\
                <td>{models_str}</td>\
                <td>{cap_str}</td>\
                <td><span class=\"badge badge-{}\">{}</span></td>\
                <td>\
                    <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/shares/{id}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Revoke this share?\">Revoke</button>\
                </td>\
            </tr>",
            if enabled { "success" } else { "danger" },
            if enabled { "Active" } else { "Revoked" }
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_shares_incoming_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let rows = sqlx::query(
        "SELECT s.id, s.owner_user_id, s.allowed_models, s.accepted_config_fingerprint, p.name as provider_name, p.kind as provider_kind, p.share_config_fingerprint as current_fingerprint
         FROM provider_shares s
         JOIN providers p ON s.provider_id = p.id
         WHERE s.grantee_user_id = ? ORDER BY s.created_at DESC",
    )
    .bind(user_id.to_string())
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"5\" class=\"text-center text-muted\">No shares shared with you.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let id = escape_html(&id);
        let pname: String = r.try_get("provider_name").unwrap_or_default();
        let pname = escape_html(&pname);
        let pkind: String = r.try_get("provider_kind").unwrap_or_default();
        let pkind = escape_html(&pkind);
        let owner: String = r.try_get("owner_user_id").unwrap_or_default();
        let short_owner = if owner.len() > 10 {
            &owner[..10]
        } else {
            &owner
        };
        let short_owner = escape_html(short_owner);
        let models: Option<String> = r.try_get("allowed_models").unwrap_or_default();
        let models_str = models.unwrap_or_else(|| "All".to_string());
        let models_str = escape_html(&models_str);
        let accepted_fp: Option<String> =
            r.try_get("accepted_config_fingerprint").unwrap_or_default();
        let current_fp: String = r.try_get("current_fingerprint").unwrap_or_default();
        let is_matching = accepted_fp.as_deref() == Some(&current_fp);
        let current_fp = escape_html(&current_fp);

        let action_btn = if is_matching {
            "<span class=\"text-muted\">Active</span>".to_string()
        } else {
            format!(
                "<button class=\"btn btn-xs btn-primary\" data-action=\"accept-share\" data-id=\"{id}\" data-fingerprint=\"{current_fp}\">Accept Share</button>"
            )
        };

        html.push_str(&format!(
            "<tr>\
                <td><strong>{pname}</strong> <span class=\"badge badge-info\">{pkind}</span></td>\
                <td><code>{short_owner}...</code></td>\
                <td>{models_str}</td>\
                <td><span class=\"badge badge-{}\">{}</span></td>\
                <td>{action_btn}</td>\
            </tr>",
            if is_matching { "success" } else { "warning" },
            if is_matching {
                "Accepted"
            } else {
                "Pending Consent"
            }
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_matrix_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let res = get_user_routes_handler(State(state), headers).await?;
    let json = res.0;
    let models = json.get("models").and_then(|v| v.as_array());

    let Some(arr) = models else {
        return Ok(Html(
            "<tr><td colspan=\"5\" class=\"text-center text-muted\">No models found.</td></tr>"
                .to_string(),
        ));
    };

    if arr.is_empty() {
        return Ok(Html("<tr><td colspan=\"5\" class=\"text-center text-muted\">No models discovered yet. Add an upstream key to discover routes.</td></tr>".to_string()));
    }

    let mut html = String::new();
    for m in arr {
        let cid = m["canonical_id"].as_str().unwrap_or("unknown");
        let cid = escape_html(cid);
        let strategy = m["routing_strategy"]
            .as_str()
            .unwrap_or("smooth_weighted_rr");
        let strategy = escape_html(strategy);
        let own_keys = m["own_keys"].as_array().map_or("—".to_string(), |keys| {
            if keys.is_empty() {
                "—".to_string()
            } else {
                keys.iter()
                    .map(|k| {
                        format!(
                            "{} (Prio: {})",
                            k["label"].as_str().unwrap_or("key"),
                            k["priority_override"]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        });
        let shared_keys = m["shared_keys"].as_array().map_or("—".to_string(), |keys| {
            if keys.is_empty() {
                "—".to_string()
            } else {
                keys.iter()
                    .map(|k| {
                        format!(
                            "{} (Prio: {})",
                            k["label"].as_str().unwrap_or("key"),
                            k["priority_override"]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        });
        let own_keys = escape_html(&own_keys);
        let shared_keys = escape_html(&shared_keys);
        let global_avail = m["global_pool"]["available"].as_bool().unwrap_or(false);
        let global_html = if global_avail {
            "<span class=\"badge badge-success\">Global pool</span>"
        } else {
            "—"
        };

        html.push_str(&format!(
            "<tr>\
                <td><strong>{cid}</strong></td>\
                <td><span class=\"badge badge-info\">{strategy}</span></td>\
                <td>{own_keys}</td>\
                <td>{shared_keys}</td>\
                <td>{global_html}</td>\
            </tr>"
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_admin_overview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let res = get_admin_overview_telemetry_handler(State(state.clone()), headers).await?;
    let data = res.0;

    let proj_disk = data["projected_disk_mb"].as_f64().unwrap_or(0.0);
    let unpriced = data["unpriced_models_count"].as_i64().unwrap_or(0);
    let events_cnt = data["usage_events_count"].as_i64().unwrap_or(0);
    let ret_days = data["retention_days"].as_i64().unwrap_or(7);

    let users_count: i64 = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
        .fetch_one(&state.auth.pool)
        .await
        .unwrap_or(0);

    let prov_count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM providers WHERE ownership_scope = 'global'",
    )
    .fetch_one(&state.auth.pool)
    .await
    .unwrap_or(0);

    let unpriced_badge = if unpriced > 0 {
        format!("<span class=\"badge badge-warning\" >{unpriced} unpriced models</span>")
    } else {
        String::new()
    };

    let html = format!(
        "<div class=\"stats-grid\">\
            <div class=\"card stat-card\">\
                <div class=\"stat-label\">Projected Disk Usage</div>\
                <div id=\"disk-meter\" class=\"disk-meter\">\
                    <p class=\"stat-val\" >Projected Disk Usage: {proj_disk} MB</p>\
                    {unpriced_badge}\
                    <div class=\"stat-desc\" >\
                        {events_cnt} events ({ret_days}d retention)\
                    </div>\
                </div>\
                <div class=\"stat-desc\">A6 disk telemetry meter</div>\
            </div>\
            <div class=\"card stat-card\">\
                <div class=\"stat-label\">Total Users</div>\
                <div class=\"stat-val\">{users_count}</div>\
                <div class=\"stat-desc\">Standard & admin accounts</div>\
            </div>\
            <div class=\"card stat-card\">\
                <div class=\"stat-label\">Global Providers</div>\
                <div class=\"stat-val\">{prov_count}</div>\
                <div class=\"stat-desc\">Shared upstream routes</div>\
            </div>\
            <div class=\"card stat-card\">\
                <div class=\"stat-label\">System Health</div>\
                <div class=\"stat-val text-success\">Healthy</div>\
                <div class=\"stat-desc\">WAL Checkpoint Normal</div>\
            </div>\
        </div>"
    );

    Ok(Html(html))
}

pub(crate) async fn get_partial_admin_users_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    let rows = sqlx::query(
        "SELECT id, username, role, can_use_global_providers, global_budget_micro_usd_month, disabled
         FROM users ORDER BY created_at ASC LIMIT 200",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut html = String::new();
    for r in rows {
        let id: String = r.try_get("id").unwrap_or_default();
        let id = escape_html(&id);
        let username: String = r.try_get("username").unwrap_or_default();
        let username = escape_html(&username);
        let role: String = r.try_get("role").unwrap_or_default();
        let role = escape_html(&role);
        let can_use: bool = r.try_get::<i64, _>("can_use_global_providers").unwrap_or(1) == 1;
        let budget: Option<i64> = r
            .try_get("global_budget_micro_usd_month")
            .unwrap_or_default();
        let budget_str = budget.map_or_else(
            || "No Cap".to_string(),
            |b| format!("${:.2}", b as f64 / 1_000_000.0),
        );
        let disabled: bool = r.try_get::<i64, _>("disabled").unwrap_or(0) == 1;

        let delete_btn = if role != "admin" {
            format!(
                "<button class=\"btn btn-xs btn-danger\" hx-delete=\"/admin/api/users/{id}?confirm={username}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Delete user {username}?\">Delete</button>"
            )
        } else {
            String::new()
        };

        html.push_str(&format!(
            "<tr>\
                <td><strong>{username}</strong></td>\
                <td><span class=\"badge badge-{}\">{role}</span></td>\
                <td>{}</td>\
                <td>{budget_str}</td>\
                <td><span class=\"badge badge-{}\">{}</span></td>\
                <td>Active</td>\
                <td>\
                    <button class=\"btn btn-xs btn-secondary\" data-action=\"toggle-user\" data-id=\"{id}\" data-disabled=\"{}\">{}</button>\
                    {delete_btn}\
                </td>\
            </tr>",
            if role == "admin" { "primary" } else { "info" },
            if can_use { "Allowed" } else { "Restricted" },
            if disabled { "danger" } else { "success" },
            if disabled { "Disabled" } else { "Active" },
            !disabled,
            if disabled { "Enable" } else { "Disable" }
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_admin_providers_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    let prov_rows = sqlx::query(
        "SELECT id, name, kind, base_url, config_json, enabled FROM providers WHERE ownership_scope = 'global' ORDER BY created_at ASC",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if prov_rows.is_empty() {
        return Ok(Html(
            "<p class=\"text-muted\">No global providers configured yet. Click \"+ Add Global Provider\" above.</p>".to_string(),
        ));
    }

    let mut html = String::new();
    for p in prov_rows {
        let pid: String = p.try_get("id").unwrap_or_default();
        let name: String = p.try_get("name").unwrap_or_default();
        let kind: String = p.try_get("kind").unwrap_or_default();
        let base_url: String = p.try_get("base_url").unwrap_or_default();
        let config_json: String = p.try_get("config_json").unwrap_or_default();
        let preset_badge = ProviderConfig::from_json(&config_json)
            .preset_id
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                format!(
                    " <span class=\"badge badge-info\">{}</span>",
                    escape_html(&s)
                )
            })
            .unwrap_or_default();
        let pid = escape_html(&pid);
        let name = escape_html(&name);
        let kind = escape_html(&kind);
        let base_url = escape_html(&base_url);

        let key_rows = sqlx::query(
            "SELECT id, label, secret_last4, priority, strict_tier, enabled FROM upstream_keys WHERE provider_id = ?",
        )
        .bind(&pid)
        .fetch_all(&state.auth.pool)
        .await
        .unwrap_or_default();

        let mut keys_html = String::new();
        if key_rows.is_empty() {
            keys_html =
                "<p class=\"text-muted py-2-sm\">No upstream keys under this global provider.</p>"
                    .to_string();
        } else {
            keys_html.push_str("<table class=\"data-table table-sub\"><thead><tr><th>Label</th><th>Priority</th><th>Strict Tier</th><th>Health</th><th>Action</th></tr></thead><tbody>");
            for k in key_rows {
                let kid: String = k.try_get("id").unwrap_or_default();
                let klabel: String = k.try_get("label").unwrap_or_default();
                let last4: Option<String> = k.try_get("secret_last4").unwrap_or_default();
                let kid = escape_html(&kid);
                let klabel = escape_html(&klabel);
                let mask = last4.map_or_else(|| "sealed".to_string(), |l| format!("••••{l}"));
                let mask = escape_html(&mask);
                let prio: i64 = k.try_get("priority").unwrap_or(100);
                let tier: i64 = k.try_get("strict_tier").unwrap_or(1);
                let enabled: bool = k.try_get::<i64, _>("enabled").unwrap_or(1) == 1;

                keys_html.push_str(&format!(
                    "<tr>\
                        <td><strong>{klabel}</strong> <span class=\"text-muted\">({mask})</span></td>\
                        <td>{prio}</td>\
                        <td>Tier {tier}</td>\
                        <td><span class=\"badge badge-{}\">{}</span></td>\
                        <td>\
                            <button class=\"btn btn-xs btn-ghost\" data-action=\"test-key\" data-key-id=\"{kid}\">Test</button>\
                            <button class=\"btn btn-xs btn-outline\" data-modal-open=\"modal-budget-window\" data-key-id=\"{kid}\">Budget</button>\
                            <button class=\"btn btn-xs btn-danger\" hx-delete=\"/api/user/upstream-keys/{kid}\" hx-target=\"closest tr\" hx-swap=\"outerHTML\" hx-confirm=\"Remove this upstream key?\">Remove</button>\
                        </td>\
                    </tr>",
                    if enabled { "success" } else { "danger" },
                    if enabled { "Active" } else { "Disabled" }
                ));
            }
            keys_html.push_str("</tbody></table>");
        }

        html.push_str(&format!(
            "<div class=\"provider-box\">\
                <div class=\"provider-header\">\
                    <div>\
                        <h4 class=\"prov-title\">{name} <span class=\"badge badge-info\">{kind}</span>{preset_badge}</h4>\
                        <div class=\"prov-url-mono\">{base_url}</div>\
                    </div>\
                    <div>\
                        <button class=\"btn btn-xs btn-primary\" data-modal-open=\"modal-new-upstream-key\" data-provider-id=\"{pid}\" data-provider-name=\"{name}\">+ Add Key</button>\
                        <button class=\"btn btn-xs btn-danger\" hx-delete=\"/admin/api/providers/{pid}\" hx-target=\"closest .provider-box\" hx-swap=\"outerHTML\" hx-confirm=\"Delete this global provider and all attached keys?\">Delete</button>\
                    </div>\
                </div>\
                <div class=\"keys-sublist\">{keys_html}</div>\
            </div>"
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_admin_catalog_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    let model_rows = sqlx::query(
        "SELECT m.model_pk, m.canonical_id, m.origin, m.exposed, m.merge_state,
                (SELECT COUNT(*) FROM model_routes r WHERE r.model_pk = m.model_pk) as route_count
         FROM models m ORDER BY m.canonical_id ASC",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if model_rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"6\" class=\"text-center text-muted\">No models in catalog.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in model_rows {
        let pk: String = r.try_get("model_pk").unwrap_or_default();
        let pk = escape_html(&pk);
        let cid: String = r.try_get("canonical_id").unwrap_or_default();
        let cid = escape_html(&cid);
        let origin: String = r.try_get("origin").unwrap_or_default();
        let origin = escape_html(&origin);
        let exposed: Option<i64> = r.try_get("exposed").unwrap_or_default();
        let is_exposed = exposed != Some(0);
        let merge_state: String = r.try_get("merge_state").unwrap_or_default();
        let merge_state = escape_html(&merge_state);
        let count: i64 = r.try_get("route_count").unwrap_or(0);

        let confirm_btn = if merge_state == "pending_review" {
            format!(
                "<button class=\"btn btn-xs btn-primary\" data-action=\"confirm-merge\" data-id=\"{pk}\">Confirm Merge</button> "
            )
        } else {
            String::new()
        };

        html.push_str(&format!(
            "<tr>\
                <td><strong>{cid}</strong></td>\
                <td><span class=\"badge badge-info\">{origin}</span></td>\
                <td><span class=\"badge badge-{}\">{merge_state}</span></td>\
                <td><span class=\"badge badge-{}\">{}</span></td>\
                <td>{count} routes</td>\
                <td>\
                    {confirm_btn}\
                    <button class=\"btn btn-xs btn-{}\" data-action=\"toggle-model\" data-id=\"{pk}\" data-exposed=\"{}\">{}</button>\
                </td>\
            </tr>",
            if merge_state == "confirmed" { "success" } else { "warning" },
            if is_exposed { "success" } else { "danger" },
            if is_exposed { "Exposed" } else { "Disabled" },
            if is_exposed { "danger" } else { "success" },
            if is_exposed { 0 } else { 1 },
            if is_exposed { "Kill-Switch" } else { "Expose" }
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_user_audit_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AuditLogQuery>,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    // Same predicate macro as `get_user_audit_log_handler` -- the two handlers differ only in the
    // columns they select, never in who may see what (`ISSUE-0172`).
    let rows = bind_tenant_audit_visibility!(
        sqlx::query(concat!(
            "SELECT seq, ts, action, target_type, target_id, detail_json
             FROM audit_log
             WHERE ",
            tenant_audit_visibility!(),
            "
               AND (? IS NULL OR action = ?)
               AND (? IS NULL OR target_type = ?)
               AND (? IS NULL OR ts >= ?)
               AND (? IS NULL OR ts <= ?)
               AND (? IS NULL OR seq < ?)
             ORDER BY seq DESC LIMIT ?"
        )),
        user_id.to_string()
    )
    .bind(query.action.as_deref())
    .bind(query.action.as_deref())
    .bind(query.target_type.as_deref())
    .bind(query.target_type.as_deref())
    .bind(query.from)
    .bind(query.from)
    .bind(query.to)
    .bind(query.to)
    .bind(query.cursor)
    .bind(query.cursor)
    .bind(limit)
    .fetch_all(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"5\" class=\"text-center text-muted\">No audit log entries recorded yet.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in rows {
        let seq: i64 = r.try_get("seq").unwrap_or(0);
        let ts: i64 = r.try_get("ts").unwrap_or(0);
        let action: String = r.try_get("action").unwrap_or_default();
        let action = escape_html(&action);
        let target_type: String = r
            .try_get::<Option<String>, _>("target_type")
            .unwrap_or_default()
            .unwrap_or_else(|| "—".to_string());
        let target_id: Option<String> = r.try_get("target_id").unwrap_or_default();
        let target_str = target_id.map_or(target_type.clone(), |tid| {
            let short = if tid.len() > 8 { &tid[..8] } else { &tid };
            format!("{target_type} ({short})")
        });
        let target_str = escape_html(&target_str);
        let payload: String = r.try_get("detail_json").unwrap_or_default();
        let payload = escape_html(&payload);

        html.push_str(&format!(
            "<tr>\
                <td><code>#{seq}</code></td>\
                <td>{ts}</td>\
                <td><span class=\"badge badge-info\">{action}</span></td>\
                <td>{target_str}</td>\
                <td class=\"cell-payload-mono\">{payload}</td>\
            </tr>"
        ));
    }

    Ok(Html(html))
}

pub(crate) async fn get_partial_admin_audit_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;

    let rows = sqlx::query(
        "SELECT seq, ts, actor_user_id, action, target_type, target_id, detail_json
         FROM audit_log ORDER BY seq DESC LIMIT 100",
    )
    .fetch_all(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(Html(
            "<tr><td colspan=\"6\" class=\"text-center text-muted\">No audit log entries recorded yet.</td></tr>".to_string(),
        ));
    }

    let mut html = String::new();
    for r in rows {
        let seq: i64 = r.try_get("seq").unwrap_or(0);
        let ts: i64 = r.try_get("ts").unwrap_or(0);
        let actor: Option<String> = r.try_get("actor_user_id").unwrap_or_default();
        let actor_str = actor.as_deref().map_or("System".to_string(), |a| {
            if a.len() > 10 {
                format!("{}...", &a[..10])
            } else {
                a.to_string()
            }
        });
        let actor_str = escape_html(&actor_str);
        let action: String = r.try_get("action").unwrap_or_default();
        let action = escape_html(&action);
        let target_type: String = r
            .try_get::<Option<String>, _>("target_type")
            .unwrap_or_default()
            .unwrap_or_else(|| "—".to_string());
        let target_id: Option<String> = r.try_get("target_id").unwrap_or_default();
        let target_str = target_id.map_or(target_type.clone(), |tid| {
            let short = if tid.len() > 8 { &tid[..8] } else { &tid };
            format!("{target_type} ({short})")
        });
        let target_str = escape_html(&target_str);
        let payload: String = r.try_get("detail_json").unwrap_or_default();
        let payload = escape_html(&payload);

        html.push_str(&format!(
            "<tr>\
                <td><code>#{seq}</code></td>\
                <td>{ts}</td>\
                <td><code>{actor_str}</code></td>\
                <td><span class=\"badge badge-info\">{action}</span></td>\
                <td>{target_str}</td>\
                <td class=\"cell-payload-mono\">{payload}</td>\
            </tr>"
        ));
    }

    Ok(Html(html))
}

pub(crate) fn render_logs_table_html(
    json: &serde_json::Value,
    is_api: bool,
    detail_base: &str,
) -> String {
    let mut out =
        String::from("<div class=\"table-container\"><table class=\"data-table\"><thead><tr>");
    if is_api {
        out.push_str("<th>Time</th><th>Status</th><th>Endpoint</th><th>User</th><th>Duration</th></tr></thead><tbody>");
    } else {
        out.push_str("<th>Time</th><th>Status</th><th>Model</th><th>User / Key</th><th>Duration</th><th>Tokens</th><th>Cost</th></tr></thead><tbody>");
    }

    let Some(rows) = json.get("rows").and_then(|r| r.as_array()) else {
        let cols = if is_api { 5 } else { 7 };
        out.push_str(&format!("<tr><td colspan=\"{cols}\" class=\"text-center text-muted\">No log entries found.</td></tr></tbody></table></div>"));
        return out;
    };
    if rows.is_empty() {
        let cols = if is_api { 5 } else { 7 };
        out.push_str(&format!("<tr><td colspan=\"{cols}\" class=\"text-center text-muted\">No log entries found.</td></tr></tbody></table></div>"));
        return out;
    }

    if is_api {
        for r in rows {
            let ts = r.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
            let status = r.get("status").and_then(|v| v.as_i64()).unwrap_or(200);
            let endpoint = escape_html(r.get("path").and_then(|v| v.as_str()).unwrap_or("/"));
            let method = escape_html(r.get("method").and_then(|v| v.as_str()).unwrap_or("GET"));
            let user = escape_html(
                r.get("actor_user_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
            );
            let dur = escape_html(
                r.get("duration_formatted")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
            );

            let status_badge = if (200..300).contains(&status) {
                format!("<span class=\"badge badge-success\">● {status}</span>")
            } else if (400..500).contains(&status) {
                format!("<span class=\"badge badge-warning\">● {status}</span>")
            } else {
                format!("<span class=\"badge badge-danger\">● {status}</span>")
            };

            out.push_str(&format!(
                "<tr><td>{ts}</td><td>{status_badge}</td><td><code>{method} {endpoint}</code></td><td>{user}</td><td>{dur}</td></tr>"
            ));
        }
    } else {
        for r in rows {
            let req_id = escape_html(r.get("request_id").and_then(|v| v.as_str()).unwrap_or(""));
            let ts = r.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
            let status = r.get("status").and_then(|v| v.as_i64()).unwrap_or(200);
            let model = escape_html(
                r.get("canonical_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
            );
            let key_label = escape_html(
                r.get("client_key_label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
            );
            let latency_ms = r
                .get("latency_ms")
                .and_then(|v| v.as_i64())
                .map(|l| format!("{l} ms"))
                .unwrap_or_else(|| "—".to_string());
            let cost_formatted = escape_html(
                r.get("cost_formatted")
                    .and_then(|v| v.as_str())
                    .unwrap_or("—"),
            );
            let total_tokens = r
                .get("total_tokens")
                .and_then(|v| v.as_i64())
                .map(|t| t.to_string())
                .unwrap_or_else(|| "—".to_string());

            let status_badge = if status == 200 {
                "<span class=\"badge badge-success\">● 200</span>".to_string()
            } else if status == 429 {
                "<span class=\"badge badge-warning\">● 429 Rejected</span>".to_string()
            } else {
                format!("<span class=\"badge badge-danger\">● {status}</span>")
            };

            out.push_str(&format!(
                "<tr class=\"row-clickable\" hx-get=\"{detail_base}/{req_id}\" hx-target=\"#drawer-content\" data-action=\"open-drawer\">\
                    <td>{ts}</td>\
                    <td>{status_badge}</td>\
                    <td><strong>{model}</strong></td>\
                    <td>{key_label}</td>\
                    <td>{latency_ms}</td>\
                    <td>{total_tokens}</td>\
                    <td>{cost_formatted}</td>\
                </tr>"
            ));
        }
    }
    out.push_str("</tbody></table></div>");
    out
}

pub(crate) fn render_usage_ledger_html(json: &serde_json::Value) -> String {
    let summary = json.get("summary");
    let total_tokens = escape_html(
        summary
            .and_then(|s| s.get("total_tokens_formatted"))
            .and_then(|v| v.as_str())
            .unwrap_or("0"),
    );
    let input_tokens = escape_html(
        summary
            .and_then(|s| s.get("input_tokens_formatted"))
            .and_then(|v| v.as_str())
            .unwrap_or("0"),
    );
    let output_tokens = escape_html(
        summary
            .and_then(|s| s.get("output_tokens_formatted"))
            .and_then(|v| v.as_str())
            .unwrap_or("0"),
    );
    let cache_read = escape_html(
        summary
            .and_then(|s| s.get("cache_read_tokens_formatted"))
            .and_then(|v| v.as_str())
            .unwrap_or("0"),
    );
    let cache_write = escape_html(
        summary
            .and_then(|s| s.get("cache_write_tokens_formatted"))
            .and_then(|v| v.as_str())
            .unwrap_or("0"),
    );

    let billed_usd = summary
        .and_then(|s| s.get("billed_cost_micro_usd"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        / 1_000_000.0;
    let list_usd = summary
        .and_then(|s| s.get("list_cost_micro_usd"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        / 1_000_000.0;
    let savings_usd = summary
        .and_then(|s| s.get("plan_covered_savings_micro_usd"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        / 1_000_000.0;

    let mut out = format!(
        "<div class=\"card mb-6\">\
            <div class=\"ledger-strip-grid\">\
                <div><div class=\"text-muted ledger-strip-label\">Total tokens</div><div class=\"ledger-strip-val\">{total_tokens}</div></div>\
                <div><div class=\"text-muted ledger-strip-label\">Input tokens</div><div class=\"ledger-strip-val\">{input_tokens}</div></div>\
                <div><div class=\"text-muted ledger-strip-label\">Output tokens</div><div class=\"ledger-strip-val\">{output_tokens}</div></div>\
                <div><div class=\"text-muted ledger-strip-label\">Cache read tokens</div><div class=\"ledger-strip-val\">{cache_read}</div></div>\
                <div><div class=\"text-muted ledger-strip-label\">Cache write tokens</div><div class=\"ledger-strip-val\">{cache_write}</div></div>\
            </div>\
            <div class=\"ledger-strip-footer\">\
                <div>Billed Cost: <strong>${billed_usd:.4}</strong> (List: <span class=\"text-strike\">${list_usd:.4}</span>)</div>\
                <div class=\"text-success\">Plan Covered Savings: <strong>${savings_usd:.4}</strong></div>\
            </div>\
        </div>\
        <div class=\"table-container\">\
            <table class=\"data-table\">\
                <thead>\
                    <tr>\
                        <th>Time</th>\
                        <th>Initiator</th>\
                        <th>Model</th>\
                        <th>Input tokens</th>\
                        <th>Output tokens</th>\
                        <th>Cache read</th>\
                        <th>Cache write</th>\
                        <th>Credits used</th>\
                    </tr>\
                </thead>\
                <tbody>"
    );

    let Some(rows) = json
        .get("rows")
        .or_else(|| json.get("items"))
        .and_then(|i| i.as_array())
    else {
        out.push_str("<tr><td colspan=\"8\" class=\"text-center text-muted\">No usage records found.</td></tr></tbody></table></div>");
        return out;
    };

    if rows.is_empty() {
        out.push_str("<tr><td colspan=\"8\" class=\"text-center text-muted\">No usage records found.</td></tr></tbody></table></div>");
        return out;
    }

    for it in rows {
        let ts = it.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
        let initiator = escape_html(
            it.get("client_key_id")
                .and_then(|v| v.as_str())
                .unwrap_or("-"),
        );
        let model = escape_html(
            it.get("canonical_id")
                .and_then(|v| v.as_str())
                .unwrap_or("-"),
        );
        let in_tok = it.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        let out_tok = it
            .get("output_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let cr_tok = it
            .get("cache_read_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let cw_tok = it
            .get("cache_write_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let cost_usd = it
            .get("cost_micro_usd")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as f64
            / 1_000_000.0;
        let list_usd = it
            .get("list_cost_micro_usd")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as f64
            / 1_000_000.0;
        let coverage = it
            .get("billing_coverage_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("pay_as_you_go");

        let credits_used = if coverage == "subscription_plan" {
            format!(
                "<span class=\"text-strike\">${list_usd:.4}</span> <span class=\"badge badge-success\">Covered by plan credits</span>"
            )
        } else {
            format!("<span>${cost_usd:.4}</span>")
        };

        out.push_str(&format!(
            "<tr>\
                <td>{ts}</td>\
                <td><code>{initiator}</code></td>\
                <td><strong>{model}</strong></td>\
                <td>{in_tok}</td>\
                <td>{out_tok}</td>\
                <td>{cr_tok}</td>\
                <td>{cw_tok}</td>\
                <td>{credits_used}</td>\
            </tr>"
        ));
    }

    out.push_str("</tbody></table></div>");
    out
}

pub(crate) async fn get_partial_user_ledger_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LedgerQuery>,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let uid = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let json_res = get_usage_ledger_internal(&state, Some(uid.to_string()), query).await?;
    let html = render_usage_ledger_html(&json_res.0);
    Ok(Html(html))
}

pub(crate) async fn get_partial_admin_ledger_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<LedgerQuery>,
) -> Result<Html<String>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, false).await?;
    let json_res = get_usage_ledger_internal(&state, None, query).await?;
    let html = render_usage_ledger_html(&json_res.0);
    Ok(Html(html))
}
