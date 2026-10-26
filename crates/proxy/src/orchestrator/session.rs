use super::*;

/// Deterministic fallback pin hash used when no instance session secret is configured.
pub(crate) fn fallback_session_hash(client_key_id: ClientKeyId, raw_session: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"llmroller.pin.v1\0");
    hasher.update(client_key_id.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(raw_session.as_bytes());
    llmroller_core::session::hex_lower(&hasher.finalize())
}

/// Tenant-scoped session hash (`RTE-1`): HMAC-SHA256 with the instance secret.
pub(crate) fn session_hash(
    session_hmac_secret: Option<&[u8]>,
    owner_user_id: &UserId,
    client_key_id: ClientKeyId,
    raw_session: &str,
) -> String {
    session_hmac_secret.map_or_else(
        || fallback_session_hash(client_key_id, raw_session),
        |secret| llmroller_core::session::compute_session_hash(secret, owner_user_id, raw_session),
    )
}

/// Collect Step 7 body session fields from the parsed request body.
pub(crate) fn session_body_fields(body: &serde_json::Value) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for name in ["session_id", "prompt_cache_key", "promptCacheKey"] {
        if let Some(value) = body.get(name).and_then(serde_json::Value::as_str) {
            fields.insert(name.to_string(), value.to_string());
        }
    }
    if let Some(value) = body
        .get("metadata")
        .and_then(|metadata| metadata.get("session_id"))
        .and_then(serde_json::Value::as_str)
    {
        fields.insert("metadata.session_id".to_string(), value.to_string());
    }
    fields
}

/// Inputs shared by session pin resolution.
pub(crate) struct SessionPinContext<'a> {
    pub(crate) client_key: &'a ClientKeyRecord,
    pub(crate) canonical_id: &'a str,
    pub(crate) protocol: &'a str,
    pub(crate) session_hmac_secret: Option<&'a [u8]>,
    pub(crate) runtime_state: &'a RuntimeState,
    pub(crate) writer: Option<&'a WriterHandle>,
    pub(crate) audit_hmac_key: Option<&'a [u8]>,
    pub(crate) pool: Option<&'a sqlx::SqlitePool>,
    pub(crate) now_ms: i64,
}

/// Resolved inbound session plus its tenant-salted outbound affinity token.
pub(crate) struct ResolvedSession {
    pub(crate) pin_key: PinKey,
    pub(crate) raw_session: String,
    pub(crate) salted_token: String,
}

impl ResolvedSession {
    /// The token written to provider affinity fields for the chosen tier.
    pub(crate) fn outbound_token(&self, tier: PoolTier) -> &str {
        if tier == PoolTier::Own && !self.raw_session.is_empty() {
            &self.raw_session
        } else {
            &self.salted_token
        }
    }
}

/// Deterministic in-memory pin key; the raw session string never leaves the process.
///
/// Implements the eight-step inbound session precedence (`RTE-1`), the Step 8
/// canonical opening-message fingerprint fallback, and the 21-slot constant
/// session detector for generic session sources (`RTE-1`, `RTE-2`). It also
/// derives the outbound affinity token used by SRDD Section 11.2.
pub(crate) fn resolve_session(
    headers: &HeaderMap,
    body: &[u8],
    context: &SessionPinContext<'_>,
) -> Option<ResolvedSession> {
    let SessionPinContext {
        client_key,
        canonical_id,
        protocol,
        session_hmac_secret,
        runtime_state,
        writer,
        audit_hmac_key,
        pool,
        now_ms,
    } = *context;
    let header_vec: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    let body_fields = session_body_fields(&parsed);
    let (raw_session, step) =
        llmroller_core::session::extract_session_precedence(&header_vec, &body_fields);

    let canonical = if raw_session.is_empty() || step.is_generic() {
        Some(llmroller_core::session::extract_canonical_sys_and_user1(
            protocol, &parsed,
        ))
    } else {
        None
    };

    let raw_hash = if raw_session.is_empty() {
        None
    } else {
        Some(session_hash(
            session_hmac_secret,
            &client_key.owner_user_id,
            client_key.id,
            &raw_session,
        ))
    };

    let mut detector_flagged = false;
    if let (Some(session_hash), true, Some((sys, usr))) =
        (raw_hash.as_ref(), step.is_generic(), canonical.as_ref())
    {
        let (is_flagged, newly_flagged) = runtime_state.constant_session_detector.observe_and_check(
            client_key.id,
            session_hash.clone(),
            &llmroller_core::session::PromptContext {
                sys,
                usr,
                has_claude_code_agent_id: headers.contains_key("x-claude-code-agent-id"),
                step,
            },
            now_ms,
        );
        detector_flagged = is_flagged;
        if newly_flagged {
            crate::metrics::record_constant_session_suspected();
            if let Some(writer) = writer {
                writer.queues.durable_state.lock().insert(
                    llmroller_store::writer::DurableKey::ClientKeyConstantSessionFlag(
                        client_key.id.to_string(),
                    ),
                    llmroller_store::writer::DurableOp::ClientKeyConstantSessionFlag {
                        client_key_id: client_key.id.to_string(),
                        flagged_at: now_ms,
                    },
                );
            }
            if let (Some(audit_key), Some(pool)) = (audit_hmac_key, pool) {
                let pool = pool.clone();
                let audit_key = audit_key.to_vec();
                let client_key_id = client_key.id.to_string();
                tokio::spawn(async move {
                    if let Ok(mut tx) = pool.begin().await {
                        let audit_id = RequestId::generate().to_string();
                        let _ = llmroller_store::audit::append(
                            &mut tx,
                            &audit_key,
                            llmroller_store::audit::AuditEntry {
                                id: &audit_id,
                                ts: now_ms,
                                actor_user_id: None,
                                ip_addr: None,
                                action: "session.constant_id_suspected",
                                target_type: "client_key",
                                target_id: Some(&client_key_id),
                                detail_json: None,
                            },
                        )
                        .await;
                        let _ = tx.commit().await;
                    }
                });
            }
        }
    }

    let use_step8 = raw_session.is_empty()
        || (step.is_generic()
            && (client_key.constant_session_mode == "fingerprint_generic_ids"
                || (detector_flagged && client_key.constant_session_mode != "allow_constant")));
    let (session_hash, fallback_id_opt) = if use_step8 {
        let (sys, usr) = canonical.as_ref()?;
        // F038: Requests with no user message (only a system prompt or empty messages)
        // must not collide on a shared Step 8 session fingerprint.
        if usr.is_empty() {
            return None;
        }
        let fallback_id = llmroller_core::session::compute_step8_fingerprint(
            &client_key.id,
            canonical_id,
            llmroller_core::session::protocol_code(protocol),
            sys,
            usr,
        );
        let hash = session_hash(
            session_hmac_secret,
            &client_key.owner_user_id,
            client_key.id,
            &fallback_id,
        );
        (hash, Some(fallback_id))
    } else {
        (raw_hash?, None)
    };

    // Step 8 has no client-supplied seed, so the canonical fingerprint is the
    // affinity seed. This also applies when a generic client ID is deliberately
    // fingerprinted via `constant_session_mode == "fingerprint_generic_ids"`.
    // The instance secret is always present in production; an empty key keeps
    // unit-level fixtures deterministic.
    let affinity_seed = if let Some(ref fallback_id) = fallback_id_opt {
        fallback_id.as_str()
    } else {
        raw_session.as_str()
    };
    let salted_token = compute_salted_affinity_token(
        session_hmac_secret.unwrap_or(b""),
        &client_key.owner_user_id,
        affinity_seed,
    );
    let outbound_raw_session = if use_step8 {
        String::new()
    } else {
        raw_session
    };

    Some(ResolvedSession {
        pin_key: PinKey {
            client_key_id: client_key.id,
            canonical_id: canonical_id.to_string(),
            session_hash,
        },
        raw_session: outbound_raw_session,
        salted_token,
    })
}

#[cfg(test)]
pub(crate) fn session_pin_key(
    headers: &HeaderMap,
    body: &[u8],
    context: &SessionPinContext<'_>,
) -> Option<PinKey> {
    resolve_session(headers, body, context).map(|session| session.pin_key)
}

/// Extract a `Retry-After` delay in milliseconds from an upstream response.
/// Read at most [`MAX_ERROR_BODY_BYTES`] from a non-success upstream response.
pub(crate) async fn read_bounded_error_body(response: reqwest::Response) -> Bytes {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::with_capacity(ERROR_CLASSIFY_BYTES);
    while let Some(Ok(chunk)) = stream.next().await {
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(buffer.len());
        if remaining == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if buffer.len() >= MAX_ERROR_BODY_BYTES {
            break;
        }
    }
    Bytes::from(buffer)
}

