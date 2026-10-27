use super::*;

/// Builds a protocol-aware OpenAI or Anthropic error envelope (`API-1`, `API-2`, `API-8`).
#[must_use]
pub fn build_protocol_error_response(
    is_messages_path: bool,
    status: StatusCode,
    error_type: &str,
    code: &str,
    message: &str,
    request_id: Option<&str>,
    retry_after_secs: Option<u64>,
) -> Response {
    let body = if is_messages_path {
        serde_json::json!({
            "type": "error",
            "error": {
                "type": error_type,
                "code": code,
                "message": message,
            },
            "request_id": request_id,
        })
    } else {
        serde_json::json!({
            "error": {
                "message": message,
                "type": error_type,
                "param": serde_json::Value::Null,
                "code": code,
            }
        })
    };
    let mut response = (status, Json(body)).into_response();
    if let Some(rid) = request_id
        && let Ok(val) = HeaderValue::from_str(rid)
    {
        response.headers_mut().insert("x-request-id", val);
    }
    if let Some(secs) = retry_after_secs
        && let Ok(val) = HeaderValue::from_str(&secs.max(1).to_string())
    {
        response.headers_mut().insert("retry-after", val);
    }
    response
}

/// Maps a stable LLMRoller error code onto the OpenAI `type` vocabulary.
///
/// The OpenAI schema only requires a string, but clients branch on the
/// conventional values, so keep the vocabulary stable and predictable.
#[must_use]
pub(crate) fn openai_error_type_for_code(code: &str) -> &'static str {
    match code {
        // Authentication / authorization.
        "authentication_error"
        | "permission_error"
        | "model_disabled"
        | "ip_restricted"
        | "service_account_disabled"
        | "client_key_paused" => "permission_error",
        // Credits / rate shaping.
        "rate_limit_error"
        | "client_key_budget_exhausted"
        | "user_budget_exhausted"
        | "provider_share_budget_exhausted"
        | "rate_limit_exceeded" => "rate_limit_error",
        // Request shape.
        "invalid_request_error"
        | "invalid_encoding"
        | "model_not_found"
        | "model_redirect_loop"
        | "ambiguous_model_id"
        | "response_not_found" => "invalid_request_error",
        // Missing resources.
        "not_found_error" => "not_found_error",
        // Provider/transport failures.
        "upstream_unreachable"
        | "upstream_error"
        | "gateway_busy"
        | "no_eligible_route"
        | "upstream_truncated"
        | "timeout_error" => "server_error",
        _ => "server_error",
    }
}

/// Maps a stable LLMRoller error code onto the Anthropic `error.type` enum
/// (`AnthropicMessagesErrorResponse`, OpenRouter OpenAPI).
///
/// `server_error` is not a member of that enum, so transport failures must be
/// reported as `api_error` and saturation as `overloaded_error`.
#[must_use]
pub(crate) fn anthropic_error_type_for_code(code: &str) -> &'static str {
    match code {
        "authentication_error" => "authentication_error",
        "permission_error"
        | "model_disabled"
        | "ip_restricted"
        | "service_account_disabled"
        | "client_key_paused" => "permission_error",
        "rate_limit_error"
        | "client_key_budget_exhausted"
        | "user_budget_exhausted"
        | "provider_share_budget_exhausted"
        | "rate_limit_exceeded" => "rate_limit_error",
        "invalid_request_error"
        | "invalid_encoding"
        | "model_not_found"
        | "model_redirect_loop"
        | "ambiguous_model_id"
        | "response_not_found" => "invalid_request_error",
        "not_found_error" => "not_found_error",
        "gateway_busy" | "no_eligible_route" => "overloaded_error",
        "timeout_error" => "timeout_error",
        // A corrupt/undecodable upstream body is a gateway fault the client can act on,
        // so it gets its own type rather than the generic `api_error` catch-all.
        "upstream_decode_error" => "upstream_decode_error",
        _ => "api_error",
    }
}

/// Builds a schema-conformant error envelope for the request's wire protocol.
///
/// This is the single entry point every data-plane error return must use so
/// OpenAI callers always receive `type`/`param` and Anthropic callers always
/// receive `type`/`code`/`request_id` (`F-021`), and so `Retry-After` can never
/// be forgotten on a 429/503 (`F-022`).
pub(crate) fn protocol_error(
    is_messages_path: bool,
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
    request_id: Option<&str>,
    retry_after_secs: Option<u64>,
) -> Response {
    let message = message.into();
    let error_type = if is_messages_path {
        anthropic_error_type_for_code(code)
    } else {
        openai_error_type_for_code(code)
    };
    build_protocol_error_response(
        is_messages_path,
        status,
        error_type,
        code,
        &message,
        request_id,
        retry_after_secs,
    )
}

pub(crate) fn json_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    protocol_error(false, status, code, message, None, None)
}

/// Anthropic errors always carry a concrete `request_id`, so generate one when
/// the caller has not created it yet (schema requires the field).
pub(crate) fn messages_error(
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
) -> Response {
    let request_id = RequestId::generate().to_string();
    protocol_error(true, status, code, message, Some(&request_id), None)
}

pub(crate) fn response_not_found(response_id: &str) -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        "response_not_found",
        format!("response '{response_id}' not found"),
    )
}

pub(crate) fn permission_error(message: impl Into<String>) -> Response {
    json_error(StatusCode::FORBIDDEN, "permission_error", message)
}

pub(crate) fn rate_limit_error(retry_after_secs: u64) -> Response {
    protocol_error(
        false,
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limit_error",
        "the upstream key that holds this response is temporarily unavailable",
        None,
        Some(retry_after_secs),
    )
}

/// Return a classified upstream error body verbatim with the filtered response headers.
pub(crate) fn upstream_error_response(
    status: StatusCode,
    upstream_headers: &HeaderMap,
    body: Bytes,
    candidate: &RouteCandidate,
    request_id: &str,
    is_admin: bool,
) -> Response {
    let headers_out = llmroller_proto::headers::apply_response_header_pipeline(
        upstream_headers,
        candidate.pool_tier(),
        is_admin,
        request_id,
        &candidate.config.response_header_allow,
    );
    let mut builder = Response::builder().status(status);
    for (name, value) in headers_out {
        if let Some(name) = name {
            builder = builder.header(name, value);
        }
    }
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Parse a Go-style duration string (`"6m0s"`, `"1h2m3.5s"`, `"500ms"`, `"45s"`) or plain seconds into milliseconds (`F029`).
#[must_use]
pub fn parse_go_duration_ms(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(secs) = s.parse::<f64>()
        && secs.is_finite()
        && secs >= 0.0
    {
        return Some((secs * 1000.0).ceil() as i64);
    }
    if let Some(ms_str) = s.strip_suffix("ms")
        && let Ok(ms) = ms_str.trim().parse::<f64>()
        && ms.is_finite()
        && ms >= 0.0
    {
        return Some(ms.ceil() as i64);
    }
    let mut total_ms: f64 = 0.0;
    let mut num_buf = String::new();
    let mut matched_unit = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_ascii_digit() || ch == '.' {
            num_buf.push(ch);
        } else if ch == 'm' && chars.peek() == Some(&'s') {
            let _ = chars.next();
            let val: f64 = num_buf.parse().ok()?;
            num_buf.clear();
            total_ms += val;
            matched_unit = true;
        } else if matches!(ch, 'h' | 'm' | 's') {
            let val: f64 = num_buf.parse().ok()?;
            num_buf.clear();
            let mult = match ch {
                'h' => 3_600_000.0,
                'm' => 60_000.0,
                's' => 1_000.0,
                _ => return None,
            };
            total_ms += val * mult;
            matched_unit = true;
        } else if !ch.is_ascii_whitespace() {
            return None;
        }
    }
    if matched_unit && num_buf.is_empty() && total_ms.is_finite() && total_ms >= 0.0 {
        Some(total_ms.ceil() as i64)
    } else {
        None
    }
}

/// Parse rate-limit backoff delay in milliseconds from upstream headers and optional error body (`F029`).
#[must_use]
pub fn parse_rate_limit_backoff_ms(headers: &HeaderMap, body: &[u8], now_ms: i64) -> Option<i64> {
    if let Some(ms_str) = headers
        .get("retry-after-ms")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        && let Ok(ms) = ms_str.parse::<f64>()
        && ms.is_finite()
        && ms >= 0.0
    {
        return Some(ms.ceil() as i64);
    }

    if let Some(ra_str) = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    {
        if let Some(ms) = parse_go_duration_ms(ra_str) {
            return Some(ms);
        }
        if let Ok(st) = httpdate::parse_http_date(ra_str) {
            let target_ms = match st.duration_since(std::time::UNIX_EPOCH) {
                Ok(dur) => i64::try_from(dur.as_millis()).unwrap_or(now_ms),
                Err(_) => now_ms,
            };
            return Some((target_ms - now_ms).max(0));
        }
    }

    for hdr in [
        "x-codex-primary-reset-after-seconds",
        "x-ratelimit-reset-after",
        "x-ratelimit-reset-requests",
        "x-ratelimit-reset-tokens",
    ] {
        if let Some(val_str) = headers.get(hdr).and_then(|v| v.to_str().ok())
            && let Some(ms) = parse_go_duration_ms(val_str)
        {
            return Some(ms);
        }
    }

    if let Some(reset_str) = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    {
        if let Ok(num) = reset_str.parse::<i64>()
            && num > 0
        {
            if num >= 1_000_000_000_000 {
                return Some((num - now_ms).max(0));
            } else if num >= 1_000_000_000 {
                return Some((num.saturating_mul(1000) - now_ms).max(0));
            } else {
                return Some(num.saturating_mul(1000));
            }
        }
        if let Some(ms) = parse_go_duration_ms(reset_str) {
            return Some(ms);
        }
    }

    if !body.is_empty()
        && let Ok(val) = serde_json::from_slice::<serde_json::Value>(body)
        && let Some(secs) = val
            .pointer("/error/reset_seconds")
            .or_else(|| val.pointer("/error/retry_after"))
            .and_then(serde_json::Value::as_f64)
        && secs.is_finite()
        && secs >= 0.0
    {
        return Some((secs * 1000.0).ceil() as i64);
    }

    None
}

pub(crate) fn retry_after_ms(headers: &HeaderMap) -> Option<i64> {
    parse_rate_limit_backoff_ms(headers, b"", now_ms())
}

pub(crate) fn gate_error_response(
    error: &llmroller_core::accumulators::GateG7Error,
    is_messages: bool,
) -> Response {
    use llmroller_core::accumulators::GateG7Error;
    let (code, used, cap, retry_after) = match error {
        GateG7Error::ClientKeyBudgetExhausted {
            retry_after_secs,
            used_micro_usd,
            budget_micro_usd_month,
        } => (
            "client_key_budget_exhausted",
            *used_micro_usd,
            *budget_micro_usd_month,
            Some(*retry_after_secs),
        ),
        GateG7Error::UserGlobalBudgetExhausted {
            used_micro_usd,
            budget_micro_usd_month,
        } => (
            "user_budget_exhausted",
            *used_micro_usd,
            *budget_micro_usd_month,
            None,
        ),
        GateG7Error::ProviderShareCapExhausted {
            used_micro_usd,
            max_monthly_micro_usd,
        } => (
            "provider_share_budget_exhausted",
            *used_micro_usd,
            *max_monthly_micro_usd,
            None,
        ),
    };

    let message = format!("monthly budget cap exhausted (used {used} of {cap} micro-USD)");
    let mut response = protocol_error(
        is_messages,
        StatusCode::TOO_MANY_REQUESTS,
        code,
        message.clone(),
        None,
        // `F-022`: every 429 must advertise a retry horizon.
        Some(retry_after.unwrap_or(60)),
    );
    // Surface the counters the control plane and clients use to explain the cap.
    let body = if is_messages {
        serde_json::json!({
            "type": "error",
            "error": {
                "type": "rate_limit_error",
                "code": code,
                "message": message,
                "used_micro_usd": used,
                "budget_micro_usd": cap,
            },
            "request_id": serde_json::Value::Null,
        })
    } else {
        serde_json::json!({
            "error": {
                "message": message,
                "type": "rate_limit_error",
                "param": serde_json::Value::Null,
                "code": code,
                "used_micro_usd": used,
                "budget_micro_usd": cap,
            }
        })
    };
    if let Ok(bytes) = serde_json::to_vec(&body) {
        *response.body_mut() = Body::from(bytes);
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    response
}
