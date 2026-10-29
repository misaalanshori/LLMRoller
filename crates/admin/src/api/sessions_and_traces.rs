use super::*;
use sqlx::Row as _;

pub(crate) async fn get_user_client_key_sessions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let key = llmroller_store::client_keys::get_client_key(&state.auth.pool, &actor, id)
        .await
        .map_err(|_| map_price_error(StatusCode::NOT_FOUND, "client key not found"))?;
    if Some(key.owner_user_id) != actor.user_id() {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "client key not found",
        ));
    }

    list_client_key_sessions_internal(&state, &actor, &id).await
}

pub(crate) async fn get_admin_client_key_sessions_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(id): Path<ClientKeyId>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    list_client_key_sessions_internal(&state, &actor, &id).await
}

pub(crate) async fn list_client_key_sessions_internal(
    state: &ApiAppState,
    actor: &ActorScope,
    client_key_id: &ClientKeyId,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let is_admin = actor.is_admin() || actor.is_system();
    let rows = sqlx::query(
        r#"SELECT session_hash, MAX(ts) as last_active_at, COUNT(*) as traced_turn_count
           FROM trace_records
           WHERE client_key_id = ?
           GROUP BY session_hash
           ORDER BY last_active_at DESC
           LIMIT 50"#,
    )
    .bind(client_key_id.to_string())
    .fetch_all(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Live pins are read from the durable `pins` table (not the hot-path store, which
    // is keyed for routing) so the drawer can show exactly which upstream key and
    // canonical model each traced session is pinned to (`USG-9`, Section 14.t).
    let now_ms = state.auth.clock.now_ms();
    let pin_rows = sqlx::query(
        r#"SELECT p.session_hash, p.upstream_key_id, m.canonical_id, p.pool_tier,
                  p.last_used_at, k.label AS key_label, pr.ownership_scope
           FROM pins p
           JOIN models m ON m.model_pk = p.model_pk
           JOIN upstream_keys k ON k.id = p.upstream_key_id
           JOIN providers pr ON pr.id = k.provider_id
           WHERE p.client_key_id = ? AND p.expires_at > ?
           ORDER BY p.last_used_at DESC"#,
    )
    .bind(client_key_id.to_string())
    .bind(now_ms)
    .fetch_all(&state.auth.reader_pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut pins_by_session: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    let mut pin_last_used: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for pin in pin_rows {
        let session_hash: String = pin.try_get("session_hash").unwrap_or_default();
        let upstream_key_id: String = pin.try_get("upstream_key_id").unwrap_or_default();
        let canonical_id: String = pin.try_get("canonical_id").unwrap_or_default();
        let key_label: String = pin.try_get("key_label").unwrap_or_default();
        let ownership_scope: String = pin.try_get("ownership_scope").unwrap_or_default();
        let pool_tier: i64 = pin.try_get("pool_tier").unwrap_or(0);
        let last_used_at: i64 = pin.try_get("last_used_at").unwrap_or(0);

        let (redacted_key_id, redacted_label) = if is_admin || ownership_scope == "personal" {
            (Some(upstream_key_id), key_label)
        } else if pool_tier == 1 || ownership_scope == "shared" {
            (None, "shared pool".to_string())
        } else {
            (None, "global pool".to_string())
        };

        pin_last_used.insert(session_hash.clone(), last_used_at);
        pins_by_session
            .entry(session_hash)
            .or_default()
            .push(serde_json::json!({
                "upstream_key_id": redacted_key_id,
                "canonical_id": canonical_id,
                "pool_tier": pool_tier,
                "key_label": redacted_label,
            }));
    }

    // Union both sources: a session that already holds a live pin but has not been
    // traced yet (traceability disabled, or the first turn still in flight) must still
    // appear in the drawer with its pin, and a traced session must keep its history.
    let mut sessions: std::collections::BTreeMap<String, (i64, i64)> =
        std::collections::BTreeMap::new();
    for r in rows {
        let session_hash: String = r.try_get("session_hash").unwrap_or_default();
        let last_active_at: i64 = r.try_get("last_active_at").unwrap_or(0);
        let traced_turn_count: i64 = r.try_get("traced_turn_count").unwrap_or(0);
        sessions.insert(session_hash, (last_active_at, traced_turn_count));
    }
    for session_hash in pins_by_session.keys() {
        let last_used_at = pin_last_used.get(session_hash).copied().unwrap_or(0);
        sessions
            .entry(session_hash.clone())
            .or_insert((last_used_at, 0));
    }
    let mut sessions = sessions.into_iter().collect::<Vec<_>>();
    sessions.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
    sessions.truncate(50);

    let data = sessions
        .into_iter()
        .map(|(session_hash, (last_active_at, traced_turn_count))| {
            let current_pins = pins_by_session.remove(&session_hash).unwrap_or_default();
            serde_json::json!({
                "session_hash": session_hash,
                "last_active_at": last_active_at,
                "traced_turn_count": traced_turn_count,
                "current_pins": current_pins
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({ "data": data })))
}

pub(crate) async fn get_user_session_preview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let owner_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    let exists = sqlx::query(
        "SELECT 1 FROM trace_records WHERE owner_user_id = ? AND session_hash = ? LIMIT 1",
    )
    .bind(owner_id.to_string())
    .bind(&session_hash)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if exists.is_none() {
        return Err(map_price_error(StatusCode::NOT_FOUND, "session not found"));
    }

    get_session_preview_internal(&state, &session_hash, Some(owner_id.to_string())).await
}

pub(crate) async fn get_admin_session_preview_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let admin_id = actor.user_id().map(|u| u.to_string());
    let now_ms = state.auth.clock.now_ms();

    let recent_audit: bool = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM audit_log WHERE actor_user_id = ? AND action = 'trace.viewed' AND target_id = ? AND ts >= ? LIMIT 1",
    )
    .bind(&admin_id)
    .bind(&session_hash)
    .bind(now_ms - 30 * 60 * 1000)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .is_some();

    let peer_addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8081));
    let ip_addr = Some(
        crate::auth::extract_client_ip(peer_addr, &headers, &state.auth.config.trusted_proxies)
            .to_string(),
    );

    let owner_user_id: Option<String> = sqlx::query_scalar(
        "SELECT owner_user_id FROM trace_records WHERE session_hash = ? LIMIT 1",
    )
    .bind(&session_hash)
    .fetch_optional(&state.auth.reader_pool)
    .await
    .unwrap_or_default();

    let detail = serde_json::json!({
        "session_owner_user_id": owner_user_id,
    })
    .to_string();

    if !recent_audit {
        let _ = append_audit_entry_helper(
            &state,
            now_ms,
            AuditEnvelope {
                actor_user_id: admin_id.as_deref(),
                ip_addr: ip_addr.as_deref(),
                action: "trace.viewed",
                target_type: "session",
                target_id: Some(&session_hash),
                detail_json: &detail,
            },
        )
        .await;
    }

    get_session_preview_internal(&state, &session_hash, None).await
}

/// Escape every string leaf so preview JSON never carries executable markup (SEC-19).
pub(crate) fn escape_json_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = escape_html(text),
        serde_json::Value::Array(items) => items.iter_mut().for_each(escape_json_strings),
        serde_json::Value::Object(fields) => fields.values_mut().for_each(escape_json_strings),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

pub(crate) async fn get_session_preview_internal(
    state: &ApiAppState,
    session_hash: &str,
    owner_user_id: Option<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_core::chat_preview::{compute_lcp, sanitize_assistant_text};
    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::SecretSealer as _;
    use llmroller_store::traces::StoredTurnTraceV2;

    let query_str = if owner_user_id.is_some() {
        "SELECT usage_event_id, request_id, ts, owner_user_id, client_key_id, session_hash,
                thread_id, canonical_id, protocol, truncated_content, payload_enc, master_key_id
         FROM trace_records
         WHERE session_hash = ? AND owner_user_id = ?
         ORDER BY ts ASC, request_id ASC"
    } else {
        "SELECT usage_event_id, request_id, ts, owner_user_id, client_key_id, session_hash,
                thread_id, canonical_id, protocol, truncated_content, payload_enc, master_key_id
         FROM trace_records
         WHERE session_hash = ?
         ORDER BY ts ASC, request_id ASC"
    };

    let rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(query_str)
            .bind(session_hash)
            .bind(uid)
            .fetch_all(&state.auth.pool)
            .await
    } else {
        sqlx::query(query_str)
            .bind(session_hash)
            .fetch_all(&state.auth.pool)
            .await
    }
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "session trace not found",
        ));
    }

    let sealer = load_sealer(state).await?;

    let mut timeline = Vec::new();
    let mut prev_block_refs: Vec<String> = Vec::new();
    let mut prev_thread_id = String::new();
    let mut first_owner_user_id = String::new();
    let mut first_client_key_id = String::new();

    for (idx, r) in rows.iter().enumerate() {
        let req_id: String = r.try_get("request_id").unwrap_or_default();
        let ts: i64 = r.try_get("ts").unwrap_or(0);
        let row_owner: String = r.try_get("owner_user_id").unwrap_or_default();
        let client_key_id: String = r.try_get("client_key_id").unwrap_or_default();
        let thread_id: String = r
            .try_get("thread_id")
            .unwrap_or_else(|_| "main".to_string());
        let canonical_model: String = r.try_get("canonical_id").unwrap_or_default();
        let protocol: String = r.try_get("protocol").unwrap_or_default();
        let truncated_content: bool = r.try_get::<i64, _>("truncated_content").unwrap_or(0) == 1;
        let payload_enc: Vec<u8> = r.try_get("payload_enc").unwrap_or_default();
        let master_key_id: String = r.try_get("master_key_id").unwrap_or_default();

        if idx == 0 {
            first_owner_user_id = row_owner.clone();
            first_client_key_id = client_key_id.clone();
        }

        let decrypted_trace = sealer
            .open(
                "trace_record",
                &[
                    master_key_id.as_bytes(),
                    req_id.as_bytes(),
                    row_owner.as_bytes(),
                    session_hash.as_bytes(),
                ],
                &payload_enc,
            )
            .map_err(|e| {
                map_price_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("open trace: {e:?}"),
                )
            })?;

        let decompressed_trace =
            zstd::bulk::decompress(decrypted_trace.expose_secret(), 10 * 1024 * 1024).map_err(
                |e| {
                    map_price_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("decompress trace: {e}"),
                    )
                },
            )?;

        let turn_trace: StoredTurnTraceV2 =
            serde_json::from_slice(&decompressed_trace).map_err(|e| {
                map_price_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("parse turn: {e}"),
                )
            })?;

        let lcp = if thread_id == prev_thread_id {
            compute_lcp(&prev_block_refs, &turn_trace.block_refs)
        } else {
            0
        };
        let is_compacted_or_branched = thread_id == prev_thread_id
            && llmroller_core::chat_preview::is_compacted_or_branched(
                &prev_block_refs,
                &turn_trace.block_refs,
            );
        prev_block_refs = turn_trace.block_refs.clone();
        prev_thread_id = thread_id.clone();

        // Tool schemas participate in trace deduplication but are not chat
        // messages; expose them through the per-thread tools view instead of
        // rendering them as input turns (TRC-4, TRC-6).
        let tool_refs: HashSet<String> = turn_trace
            .request_skeleton
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|reference| reference.get("$blk")?.as_str().map(str::to_string))
            .collect();

        let mut new_input_messages = Vec::new();
        for hash in &turn_trace.block_refs[lcp..] {
            if tool_refs.contains(hash) {
                continue;
            }
            let block_row = sqlx::query(
                "SELECT payload_enc, master_key_id FROM trace_blocks WHERE owner_user_id = ? AND block_hash = ?",
            )
            .bind(&row_owner)
            .bind(hash)
            .fetch_optional(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

            if let Some(br) = block_row {
                let blk_enc: Vec<u8> = br.try_get("payload_enc").unwrap_or_default();
                let blk_mk: String = br.try_get("master_key_id").unwrap_or_default();
                let dec_blk = sealer
                    .open(
                        "trace_block",
                        &[blk_mk.as_bytes(), row_owner.as_bytes(), hash.as_bytes()],
                        &blk_enc,
                    )
                    .map_err(|e| {
                        map_price_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("open block: {e:?}"),
                        )
                    })?;

                let decomp_blk = zstd::bulk::decompress(dec_blk.expose_secret(), 10 * 1024 * 1024)
                    .map_err(|e| {
                        map_price_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("decompress block: {e}"),
                        )
                    })?;

                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&decomp_blk) {
                    new_input_messages.push(val);
                } else {
                    let text = String::from_utf8_lossy(&decomp_blk).to_string();
                    new_input_messages.push(serde_json::json!({ "role": "user", "content": text }));
                }
            } else {
                new_input_messages.push(serde_json::json!({
                    "role": "system",
                    "content": "content expired",
                    "expired": true
                }));
            }
        }

        let raw_assistant = turn_trace
            .response
            .get("assistant_text")
            .and_then(|v| v.as_str())
            .or_else(|| turn_trace.response.get("text").and_then(|v| v.as_str()))
            .unwrap_or("");
        let sanitized_assistant = sanitize_assistant_text(raw_assistant);

        let assistant_reasoning = turn_trace
            .response
            .get("assistant_reasoning")
            .and_then(|v| v.as_str())
            .map(escape_html);

        let mut assistant_tool_calls = turn_trace
            .response
            .get("assistant_tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for call in &mut assistant_tool_calls {
            escape_json_strings(call);
        }

        for message in &mut new_input_messages {
            escape_json_strings(message);
        }

        timeline.push(serde_json::json!({
            "ordinal": idx + 1,
            "request_id": escape_html(&req_id),
            "ts": ts,
            "thread_id": escape_html(&thread_id),
            "canonical_model": escape_html(&canonical_model),
            "protocol": escape_html(&protocol),
            "truncated_content": truncated_content,
            "context_compacted_or_branched": is_compacted_or_branched,
            "new_input_messages": new_input_messages,
            "assistant_text": sanitized_assistant,
            "assistant_reasoning": assistant_reasoning,
            "assistant_tool_calls": assistant_tool_calls,
        }));
    }

    // `USR-4` / `SEC-17`: a standard user only sees pins of their own client keys,
    // and pooled (`global` / `shared`) key identities are redacted even then.
    let pin_sql = if owner_user_id.is_some() {
        "SELECT p.upstream_key_id, m.canonical_id, p.pool_tier, pv.ownership_scope,
                k.label as key_label
         FROM pins p
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         LEFT JOIN providers pv ON pv.id = k.provider_id
         JOIN client_keys ck ON ck.id = p.client_key_id
         WHERE p.session_hash = ? AND ck.owner_user_id = ?"
    } else {
        "SELECT p.upstream_key_id, m.canonical_id, p.pool_tier, pv.ownership_scope,
                k.label as key_label
         FROM pins p
         LEFT JOIN models m ON p.model_pk = m.model_pk
         LEFT JOIN upstream_keys k ON p.upstream_key_id = k.id
         LEFT JOIN providers pv ON pv.id = k.provider_id
         WHERE p.session_hash = ?"
    };
    let pin_rows = if let Some(ref uid) = owner_user_id {
        sqlx::query(pin_sql)
            .bind(session_hash)
            .bind(uid)
            .fetch_all(&state.auth.pool)
            .await
    } else {
        sqlx::query(pin_sql)
            .bind(session_hash)
            .fetch_all(&state.auth.pool)
            .await
    }
    .unwrap_or_default();

    let mut current_pins = Vec::new();
    for pr in pin_rows {
        let scope: String = pr.try_get("ownership_scope").unwrap_or_default();
        let redact = owner_user_id.is_some() && scope != "personal";
        let upstream_key_id = pr
            .try_get::<Option<String>, _>("upstream_key_id")
            .unwrap_or_default()
            .filter(|_| !redact)
            .map(|value| escape_html(&value));
        let key_label = pr
            .try_get::<Option<String>, _>("key_label")
            .unwrap_or_default()
            .map(|value| {
                if redact {
                    "pooled".to_string()
                } else {
                    escape_html(&value)
                }
            });
        current_pins.push(serde_json::json!({
            "upstream_key_id": upstream_key_id,
            "canonical_id": pr.try_get::<Option<String>, _>("canonical_id").unwrap_or_default().map(|value| escape_html(&value)),
            "pool_tier": pr.try_get::<i64, _>("pool_tier").unwrap_or(1),
            "key_label": key_label,
            "redacted": redact,
        }));
    }

    Ok(Json(serde_json::json!({
        "session_hash": escape_html(session_hash),
        "owner_user_id": escape_html(&first_owner_user_id),
        "client_key_id": escape_html(&first_client_key_id),
        "total_turns": timeline.len(),
        "current_pins": current_pins,
        "timeline": timeline,
    })))
}

pub(crate) async fn get_user_trace_record_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;

    get_trace_record_internal(&state, &request_id, Some(user_id.to_string())).await
}

pub(crate) async fn get_admin_trace_record_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_admin(&state, &headers, false).await?;
    let admin_id = actor.user_id().map(|u| u.to_string());
    let now_ms = state.auth.clock.now_ms();
    let peer_addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8081));
    let ip_addr = Some(
        crate::auth::extract_client_ip(peer_addr, &headers, &state.auth.config.trusted_proxies)
            .to_string(),
    );

    let session_hash_opt: Option<String> =
        sqlx::query_scalar("SELECT session_hash FROM trace_records WHERE request_id = ? LIMIT 1")
            .bind(&request_id)
            .fetch_optional(&state.auth.reader_pool)
            .await
            .unwrap_or_default();
    let dedup_target = session_hash_opt.as_deref().unwrap_or(&request_id);

    let recent_audit: bool = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM audit_log WHERE actor_user_id = ? AND action = 'trace.viewed' AND target_id IN (?, ?) AND ts >= ? LIMIT 1",
    )
    .bind(&admin_id)
    .bind(dedup_target)
    .bind(&request_id)
    .bind(now_ms - 30 * 60 * 1000)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .is_some();

    if !recent_audit {
        let _ = append_audit_entry_helper(
            &state,
            now_ms,
            AuditEnvelope {
                actor_user_id: admin_id.as_deref(),
                ip_addr: ip_addr.as_deref(),
                action: "trace.viewed",
                target_type: "trace",
                target_id: Some(dedup_target),
                detail_json: &serde_json::json!({ "request_id": request_id }).to_string(),
            },
        )
        .await;
    }

    get_trace_record_internal(&state, &request_id, None).await
}

pub(crate) async fn get_trace_record_internal(
    state: &ApiAppState,
    request_id: &str,
    owner_user_id: Option<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::SecretSealer as _;
    use llmroller_store::traces::StoredTurnTraceV2;

    let row = sqlx::query(
        "SELECT request_id, owner_user_id, session_hash, payload_enc, master_key_id
         FROM trace_records
         WHERE request_id = ?",
    )
    .bind(request_id)
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    .ok_or_else(|| map_price_error(StatusCode::NOT_FOUND, "trace record not found"))?;

    let row_owner: String = row.try_get("owner_user_id").unwrap_or_default();
    if let Some(ref uid) = owner_user_id
        && &row_owner != uid
    {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "trace record not found",
        ));
    }

    let session_hash: String = row.try_get("session_hash").unwrap_or_default();
    let payload_enc: Vec<u8> = row.try_get("payload_enc").unwrap_or_default();
    let master_key_id: String = row.try_get("master_key_id").unwrap_or_default();

    let sealer = load_sealer(state).await?;
    let decrypted = sealer
        .open(
            "trace_record",
            &[
                master_key_id.as_bytes(),
                request_id.as_bytes(),
                row_owner.as_bytes(),
                session_hash.as_bytes(),
            ],
            &payload_enc,
        )
        .map_err(|e| {
            map_price_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("open trace: {e:?}"),
            )
        })?;

    let decompressed = zstd::bulk::decompress(decrypted.expose_secret(), 10 * 1024 * 1024)
        .map_err(|e| {
            map_price_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("decompress trace: {e}"),
            )
        })?;

    let turn_trace: StoredTurnTraceV2 = serde_json::from_slice(&decompressed).map_err(|e| {
        map_price_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("parse turn: {e}"),
        )
    })?;

    Ok(Json(serde_json::to_value(turn_trace).unwrap_or_default()))
}

pub(crate) async fn delete_admin_session_trace_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;

    let rows =
        sqlx::query("SELECT DISTINCT owner_user_id FROM trace_records WHERE session_hash = ?")
            .bind(&session_hash)
            .fetch_all(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if rows.is_empty() {
        return Ok(StatusCode::NO_CONTENT);
    }

    let mut tx = state
        .auth
        .pool
        .begin()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    for r in rows {
        let owner_str: String = r.try_get("owner_user_id").unwrap_or_default();
        if let Ok(owner_user_id) = llmroller_core::ids::UserId::parse(&owner_str) {
            let _ = llmroller_store::traces::delete_session_trace_tx(
                &mut tx,
                &owner_user_id,
                &session_hash,
            )
            .await;
        }
    }

    tx.commit()
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct DeleteTracesQuery {
    /// Optional ISO-millisecond cutoff: only turns at or before this timestamp are
    /// removed. Absent means "all recorded turns" (TRC-6 admin purge).
    pub before_ts: Option<i64>,
}

pub(crate) async fn delete_admin_traces_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Query(query): Query<DeleteTracesQuery>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    // Deleting expired rows only would leave every live trace and every sealed
    // block orphaned (F039). Purge both tables in one writer transaction.
    execute_atomic_mutation(&state, vec![], move |tx| {
        Box::pin(async move {
            match query.before_ts {
                Some(cutoff) => {
                    sqlx::query("DELETE FROM trace_records WHERE ts <= ?")
                        .bind(cutoff)
                        .execute(&mut **tx)
                        .await
                        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                    sqlx::query(
                        r#"DELETE FROM trace_blocks WHERE NOT EXISTS (
                               SELECT 1 FROM trace_record_blocks r
                               WHERE r.owner_user_id = trace_blocks.owner_user_id
                                 AND r.block_hash = trace_blocks.block_hash
                           )"#,
                    )
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                }
                None => {
                    sqlx::query("DELETE FROM trace_records")
                        .execute(&mut **tx)
                        .await
                        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                    sqlx::query("DELETE FROM trace_blocks")
                        .execute(&mut **tx)
                        .await
                        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                }
            }
            Ok(())
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) struct AuditEnvelope<'a> {
    pub(crate) actor_user_id: Option<&'a str>,
    pub(crate) ip_addr: Option<&'a str>,
    pub(crate) action: &'a str,
    pub(crate) target_type: &'a str,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) detail_json: &'a str,
}

pub(crate) async fn append_audit_entry_helper(
    state: &ApiAppState,
    now_ms: i64,
    envelope: AuditEnvelope<'_>,
) -> Result<(), sqlx::Error> {
    use llmroller_core::seams::ExposeSecret as _;

    let audit_id = llmroller_core::ids::RequestId::generate().to_string();
    // No master-key fallback (`ISSUE-0138`, `ADR-11`): the audit chain's key must not depend on a
    // key that `ADR-11` promises is irrelevant to it. A rotation would silently orphan every
    // previously written row. When no instance key is configured, skip the write.
    let Some(instance_key) =
        resolve_audit_hmac_key(state.audit_hmac_key.as_ref(), &state.master_key).ok()
    else {
        return Ok(());
    };
    let key: &[u8] = instance_key.expose_secret();

    let mut tx = state.auth.pool.begin().await?;
    llmroller_store::audit::append(
        &mut tx,
        key,
        llmroller_store::audit::AuditEntry {
            id: &audit_id,
            ts: now_ms,
            actor_user_id: envelope.actor_user_id,
            ip_addr: envelope.ip_addr,
            action: envelope.action,
            target_type: envelope.target_type,
            target_id: envelope.target_id,
            detail_json: Some(envelope.detail_json),
        },
    )
    .await
    .map_err(|_| sqlx::Error::Protocol("audit append failed".to_string()))?;
    tx.commit().await
}

// ---------------------------------------------------------------------------
// Client Key In-Place Rotation (/api/user/client-keys/{id}/rotate)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub(crate) struct RotateKeyRequest {
    pub grace_period_secs: Option<u64>,
}

pub(crate) async fn get_active_pins_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, false).await?;
    let now_ms = state.auth.clock.now_ms();

    // The loader is shared with `/app/partials/pins` so the two views cannot disagree about
    // which rows exist (`Section 14.t.1`; live `PinStore` entries merged with durable rows).
    let list = load_active_pins(&state, &actor, now_ms)
        .await?
        .into_iter()
        .map(|row| {
            let key_label = mask_key_label(
                actor.is_admin(),
                row.pool_tier,
                row.key_owner.as_deref(),
                actor.user_id().map(|u| u.to_string()).as_deref(),
                row.key_label,
            );
            serde_json::json!({
                "session_hash": row.session_hash,
                "client_key_id": row.client_key_id,
                "canonical_id": row.canonical_id,
                "key_label": key_label.unwrap_or_default(),
                "pool_tier": row.pool_tier,
                "created_at": row.created_at,
                "last_used_at": row.last_used_at,
            })
        })
        .collect();

    Ok(Json(list))
}

pub(crate) fn pin_delete_status(headers: &HeaderMap) -> StatusCode {
    if headers.contains_key("hx-request") {
        StatusCode::OK
    } else {
        StatusCode::NO_CONTENT
    }
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
pub(crate) struct MoveSessionPinRequest {
    #[serde(alias = "target_upstream_key_id")]
    pub upstream_key_id: String,
    pub canonical_id: Option<String>,
    pub session_hash: Option<String>,
    pub client_key_id: Option<String>,
}

/// Move active session pin via `POST /admin/api/pins/move` (`RTE-11`, `F-108`).
pub(crate) async fn post_admin_move_pin_body_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<MoveSessionPinRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let session_hash = payload.session_hash.clone().ok_or_else(|| {
        map_price_error(StatusCode::UNPROCESSABLE_ENTITY, "session_hash is required")
    })?;
    post_admin_move_session_pin_handler(State(state), headers, Path(session_hash), Json(payload))
        .await
}

/// Move personal session pin via `POST /api/user/pins/move` (`RTE-11`, `USR-3`, `F-108`).
///
/// # Three defects this used to have (`ISSUE-0232`)
///
/// It ran a bare `let _ = sqlx::query(... UPDATE pins ...).execute(&state.auth.pool).await;`
/// and then unconditionally read the rows back and called `store.admin_move_with_tier(...)`.
///
/// 1. **The `Result` was discarded**, so `SQLITE_BUSY`, a constraint failure or a dropped
///    connection became an invisible no-op and the caller was still told `{"moved": true}`.
/// 2. **Zero rows affected was reported as success.** The `WHERE` is ownership-scoped, so moving
///    a pin in a session the caller owns no client key for matched nothing and returned `200`.
///    `USR-3` requires a foreign resource to be indistinguishable from a missing one; a success
///    claim for another user's session is not indistinguishable, it is false.
/// 3. **`PinStore` was updated even when the database wrote nothing.** The store is what the data
///    plane routes from, so store and database disagreed and the divergence was live traffic.
///
/// # The decisions, recorded rather than inferred
///
/// - **`404` on zero rows.** Matching `delete_user_pin_handler` directly below, which already
///   returns `404` for a foreign client key: one rule for a foreign resource, not two. The
///   ownership predicate is untouched — only the response for "matched nothing" changed.
/// - **The store is updated only for rows the transaction moved.** The `UPDATE`'s `RETURNING`
///   clause yields exactly the rows it changed, so the store update is driven by that result
///   instead of by a separate read-back query. A read-back would re-query by `session_hash`
///   alone and could reach rows the `UPDATE` never touched — that was defect 3.
/// - **`ConfigDomain` is empty.** Pins are runtime routing state, not configuration: they are
///   held in `PinStore` and updated in place (see `delete_pin_handler`, the reference). Publishing
///   a config domain for them would trigger a pointless full catalog rebuild. The
///   `control_mutation_barrier` still supplies the audit row for the mutation, which is the
///   behaviour every other mutation in this file already has.
pub(crate) async fn post_user_move_pin_body_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Json(payload): Json<MoveSessionPinRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Unauthorized))?;
    let session_hash = payload.session_hash.clone().ok_or_else(|| {
        map_price_error(StatusCode::UNPROCESSABLE_ENTITY, "session_hash is required")
    })?;
    let target_key = UpstreamKeyId::parse(&payload.upstream_key_id)
        .map_err(|e| map_price_error(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    // Verify target upstream key belongs to caller (USR-3)
    let key_record = get_upstream_key(&state.auth.pool, &actor, target_key)
        .await
        .map_err(map_provider_error)?;
    if !actor.is_admin() && key_record.owner_user_id != Some(user_id) {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "upstream key not found",
        ));
    }

    let now_ms = state.auth.clock.now_ms();
    let target_tier_int: i64 = if key_record.owner_user_id == Some(user_id) {
        0
    } else if key_record.owner_user_id.is_some() {
        1
    } else {
        2
    };
    let target_tier_enum = match target_tier_int {
        0 => llmroller_core::eligibility::CandidatePoolTier::Own,
        1 => llmroller_core::eligibility::CandidatePoolTier::Shared,
        _ => llmroller_core::eligibility::CandidatePoolTier::Global,
    };

    let user_id_str = user_id.to_string();
    let session_for_tx = session_hash.clone();
    // `RETURNING` gives the rows this statement actually changed, so the store is driven by the
    // write's own result rather than by a second query that could disagree with it.
    let moved: Vec<(String, String)> = execute_atomic_mutation(&state, Vec::new(), move |tx| {
        Box::pin(async move {
            // `RETURNING` in SQLite yields column references only -- no subqueries -- so
            // the moved rows come back with `model_pk` and the canonical id is resolved in
            // one follow-up query *inside the same transaction*. Both queries see the same
            // rows: `pins.model_pk` is `REFERENCES models(model_pk) ON DELETE RESTRICT`, so
            // a moved row's model exists, and a concurrent delete cannot slip between them.
            let rows = sqlx::query(
                "UPDATE pins SET upstream_key_id = ?, pool_tier = ?, last_used_at = ?
                     WHERE session_hash = ?
                       AND client_key_id IN (
                           SELECT id FROM client_keys WHERE owner_user_id = ?
                       )
                     RETURNING client_key_id, model_pk",
            )
            .bind(target_key.to_string())
            .bind(target_tier_int)
            .bind(now_ms)
            .bind(&session_for_tx)
            .bind(&user_id_str)
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

            let mut moved_rows = Vec::with_capacity(rows.len());
            for row in rows {
                // A decode failure here is a real fault, not an empty value: reporting it
                // beats silently skipping the row and leaving the store stale.
                let client_key_id: String = row
                    .try_get("client_key_id")
                    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                let model_pk: String = row
                    .try_get("model_pk")
                    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                moved_rows.push((client_key_id, model_pk));
            }
            resolve_canonical_ids(tx, moved_rows).await
        })
    })
    .await?;

    // Zero rows means this caller owns no pin in that session. `404`, matching the sibling
    // delete handler, and reaching this point wrote neither the database nor the store.
    if moved.is_empty() {
        return Err(map_price_error(StatusCode::NOT_FOUND, "not_found"));
    }

    if let Some(store) = &state.pin_store {
        for (client_key_id, canonical_id) in moved {
            let Ok(client_key_id) = ClientKeyId::parse(&client_key_id) else {
                tracing::warn!(
                    client_key_id,
                    "moved pin row carried a client_key_id that does not parse; \
                     store left untouched for it"
                );
                continue;
            };
            store.admin_move_with_tier(
                &llmroller_core::pins::PinKey {
                    client_key_id,
                    session_hash: session_hash.clone(),
                    canonical_id,
                },
                target_key,
                Some(target_tier_enum),
                now_ms,
            );
        }
    }

    Ok(Json(serde_json::json!({
        "moved": true,
        "session_hash": session_hash,
        "upstream_key_id": target_key.to_string(),
    })))
}

/// Resolve `model_pk` to `canonical_id` for the rows an `UPDATE ... RETURNING` just moved.
///
/// Runs inside the caller's transaction, so it observes exactly the rows the `UPDATE` changed.
/// SQLite's `RETURNING` clause accepts column references only, which is why this is a second
/// statement rather than a subquery in the `RETURNING` list.
///
/// A missing model row is not treated as a fault: `pins.model_pk` is
/// `REFERENCES models(model_pk) ON DELETE RESTRICT`, so a moved pin's model exists, and the
/// `unwrap_or_default` here only covers a canonical id that is genuinely absent. A *decode*
/// failure is still propagated, because that is a fault rather than an empty value.
async fn resolve_canonical_ids(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    rows: Vec<(String, String)>,
) -> Result<Vec<(String, String)>, (StatusCode, Json<serde_json::Value>)> {
    let mut out = Vec::with_capacity(rows.len());
    for (client_key_id, model_pk) in rows {
        let canonical: Option<String> =
            sqlx::query_scalar("SELECT canonical_id FROM models WHERE model_pk = ?")
                .bind(&model_pk)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        out.push((client_key_id, canonical.unwrap_or_default()));
    }
    Ok(out)
}

/// [`resolve_canonical_ids`] carrying each row's `pool_tier` through unchanged.
async fn resolve_canonical_ids_with_tier(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    rows: Vec<(String, String, i64)>,
) -> Result<Vec<(String, String, i64)>, (StatusCode, Json<serde_json::Value>)> {
    let mut out = Vec::with_capacity(rows.len());
    for (client_key_id, model_pk, tier_int) in rows {
        let canonical: Option<String> =
            sqlx::query_scalar("SELECT canonical_id FROM models WHERE model_pk = ?")
                .bind(&model_pk)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        out.push((client_key_id, canonical.unwrap_or_default(), tier_int));
    }
    Ok(out)
}

/// Admin manually moves session pin to a new upstream key (`RTE-11`, `SEC-17`).
///
/// # Same three defects as the user handler (`ISSUE-0232`)
///
/// Two `let _ = sqlx::query(... UPDATE pins ...).execute(&state.auth.pool).await;` sites (one per
/// branch) discarded their `Result`, zero rows was reported as `{"moved": true}`, and the
/// `PinStore` read-back loop ran unconditionally and selected by `session_hash` **alone** — with
/// no ownership or model filter, so it could move store entries the `UPDATE` had not touched.
///
/// # The decisions, and how they differ from the user handler
///
/// - **`404` on zero rows**, same as the user handler, but for a different reason: an admin has no
///   ownership predicate, so zero rows means the session hash simply has no pins. Reporting that
///   as a move is still a false success claim.
/// - **One `UPDATE`, not two branches.** The `canonical_id` filter is expressed as
///   `(? IS NULL OR model_pk IN (...))` so the statement's `RETURNING` set is the exact set of
///   rows moved, in both cases. Two string-built statements differing only in a `WHERE` clause is
///   how the original drifted.
/// - **`ConfigDomain` is empty**, as in the user handler: pins are `PinStore` runtime state, not
///   configuration. The audit row still comes from `control_mutation_barrier`.
pub(crate) async fn post_admin_move_session_pin_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(session_hash): Path<String>,
    Json(payload): Json<MoveSessionPinRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    let target_key = UpstreamKeyId::parse(&payload.upstream_key_id)
        .map_err(|e| map_price_error(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    let now_ms = state.auth.clock.now_ms();
    let key_meta: Option<(Option<String>, String)> = sqlx::query_as(
        "SELECT k.owner_user_id, pr.ownership_scope
         FROM upstream_keys k
         JOIN providers pr ON pr.id = k.provider_id
         WHERE k.id = ?",
    )
    .bind(target_key.to_string())
    .fetch_optional(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let Some((key_owner_uid, key_scope)) = key_meta else {
        return Err(map_price_error(
            StatusCode::NOT_FOUND,
            "upstream key not found",
        ));
    };

    let canonical_filter = payload.canonical_id.clone();
    let session_for_tx = session_hash.clone();
    let moved: Vec<(String, String, i64)> =
        execute_atomic_mutation(&state, Vec::new(), move |tx| {
            Box::pin(async move {
                // One statement for both cases: `? IS NULL` covers "move every pin in the
                // session", otherwise the model filter applies. `RETURNING` reports exactly the
                // rows changed, including the tier the CASE assigned.
                // `RETURNING` yields column references only, so `pool_tier` comes back from
                // the row and the canonical id is resolved in one follow-up query inside the
                // same transaction (see the user handler for why that is safe).
                let rows = sqlx::query(
                    "UPDATE pins
                     SET upstream_key_id = ?,
                         pool_tier = CASE
                             WHEN ? = 'global' THEN 2
                             WHEN ? IS NOT NULL AND ? = (
                                 SELECT owner_user_id FROM client_keys WHERE id = pins.client_key_id
                             ) THEN 0
                             ELSE 1
                         END,
                         last_used_at = ?
                     WHERE session_hash = ?
                       AND (? IS NULL OR model_pk IN (
                           SELECT model_pk FROM models WHERE canonical_id = ?
                       ))
                     RETURNING client_key_id, model_pk, pool_tier",
                )
                .bind(target_key.to_string())
                .bind(&key_scope)
                .bind(&key_owner_uid)
                .bind(&key_owner_uid)
                .bind(now_ms)
                .bind(&session_for_tx)
                .bind(&canonical_filter)
                .bind(&canonical_filter)
                .fetch_all(&mut **tx)
                .await
                .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

                let mut moved_rows = Vec::with_capacity(rows.len());
                for row in rows {
                    let client_key_id: String = row
                        .try_get("client_key_id")
                        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                    let model_pk: String = row
                        .try_get("model_pk")
                        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                    let tier_int: i64 = row
                        .try_get("pool_tier")
                        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
                    moved_rows.push((client_key_id, model_pk, tier_int));
                }
                resolve_canonical_ids_with_tier(tx, moved_rows).await
            })
        })
        .await?;

    if moved.is_empty() {
        return Err(map_price_error(StatusCode::NOT_FOUND, "not_found"));
    }

    if let Some(store) = &state.pin_store {
        for (client_key_id, canonical_id, tier_int) in moved {
            let tier_enum = match tier_int {
                0 => llmroller_core::eligibility::CandidatePoolTier::Own,
                1 => llmroller_core::eligibility::CandidatePoolTier::Shared,
                _ => llmroller_core::eligibility::CandidatePoolTier::Global,
            };
            let Ok(client_key_id) = ClientKeyId::parse(&client_key_id) else {
                tracing::warn!(
                    client_key_id,
                    "moved pin row carried a client_key_id that does not parse; \
                     store left untouched for it"
                );
                continue;
            };
            store.admin_move_with_tier(
                &llmroller_core::pins::PinKey {
                    client_key_id,
                    session_hash: session_hash.clone(),
                    canonical_id,
                },
                target_key,
                Some(tier_enum),
                now_ms,
            );
        }
    }

    Ok(Json(serde_json::json!({
        "moved": true,
        "session_hash": session_hash,
        "upstream_key_id": target_key.to_string(),
    })))
}

/// Admin-wide unpin: every tenant's pin for the session hash is removed.
///
/// `SEC-17`: only administrators may delete pins they do not own.
pub(crate) async fn delete_pin_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let _actor = authenticate_admin(&state, &headers, true).await?;
    sqlx::query("DELETE FROM pins WHERE session_hash = ?")
        .bind(&hash)
        .execute(&state.auth.pool)
        .await
        .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if let Some(store) = &state.pin_store {
        store.remove_by_session_hash(&hash);
    }
    Ok(pin_delete_status(&headers))
}

/// Owner-scoped unpin for the session detail UI.
///
/// `SEC-17` / `USR-3`: the SQL predicate derives ownership from the client key
/// join, and a cross-tenant attempt is an indistinguishable no-op. Only the
/// exact live pin is evicted from the shared `PinStore`.
pub(crate) async fn delete_user_pin_handler(
    State(state): State<ApiAppState>,
    headers: HeaderMap,
    Path((client_key_id, session_hash, canonical_id)): Path<(ClientKeyId, String, String)>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let actor = authenticate_actor(&state, &headers, true).await?;
    let owner_user_id = actor
        .user_id()
        .ok_or_else(|| map_auth_error(AuthError::Forbidden))?
        .to_string();

    // `USR-3`: a foreign client key is indistinguishable from a missing one.
    let owns_client_key: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM client_keys WHERE id = ? AND owner_user_id = ?")
            .bind(client_key_id.to_string())
            .bind(&owner_user_id)
            .fetch_optional(&state.auth.pool)
            .await
            .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if owns_client_key.is_none() {
        return Err(map_price_error(StatusCode::NOT_FOUND, "not_found"));
    }

    let result = sqlx::query(
        "DELETE FROM pins
         WHERE client_key_id = ?
           AND session_hash = ?
           AND model_pk = (SELECT model_pk FROM models WHERE canonical_id = ?)
           AND client_key_id IN (SELECT id FROM client_keys WHERE owner_user_id = ?)",
    )
    .bind(client_key_id.to_string())
    .bind(&session_hash)
    .bind(&canonical_id)
    .bind(owner_user_id)
    .execute(&state.auth.pool)
    .await
    .map_err(|e| map_price_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if result.rows_affected() > 0
        && let Some(store) = &state.pin_store
    {
        store.remove(&llmroller_core::pins::PinKey {
            client_key_id,
            canonical_id,
            session_hash,
        });
    }
    Ok(pin_delete_status(&headers))
}

// ---------------------------------------------------------------------------
// User Routing Matrix (/api/user/routes)
// ---------------------------------------------------------------------------
#[derive(Debug, Deserialize)]
pub(crate) struct PutRoutePreferenceRequest {
    pub priority_override: Option<u32>,
    pub enabled: Option<bool>,
}
