#![forbid(unsafe_code)]
//! Orchestrator v1 for /v1/* inference endpoints and pre-flight body budget admission.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#53-orchestration-and-request-flow-state-machine
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#71-performance-and-resource-efficiency-perf
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-2.md#27-sub-phase-27-v1-orchestrator-v1-pre-flight-body-memory-budget-graceful-sigterm-drain-and-early-load-harness

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::header::{HeaderValue, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use llmroller_core::client_key::ClientKeyRecord;
use llmroller_core::ids::{ClientKeyId, RequestId, UserId};
use llmroller_core::rbac::PoolTier;
use llmroller_proto::adapters::EndpointKind;
use llmroller_proto::body_edit::{ChatStreamMode, edit_body, scan_top_level};
use sqlx::SqlitePool;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::auth::ProxyAuthState;

/// Pre-flight admission error when memory budget is saturated.
#[derive(Debug, Error)]
pub enum GatewayBusyError {
    #[error("gateway busy: preflight body memory budget saturated")]
    Timeout,
    #[error("body exceeds maximum allowed size: {0} bytes")]
    BodyTooLarge(usize),
}

impl GatewayBusyError {
    #[must_use]
    pub const fn status_code(&self) -> StatusCode {
        match self {
            Self::Timeout => StatusCode::SERVICE_UNAVAILABLE,
            Self::BodyTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }

    #[must_use]
    pub const fn retry_after_secs(&self) -> u64 {
        1
    }
}

/// Pre-flight body memory budget semaphore tracking pre-first-byte buffered bodies per SEC-8.
#[derive(Debug, Clone)]
pub struct PreflightBudgetSemaphore {
    pub budget_mb: usize,
    pub max_body_mb: usize,
    pub semaphore: Arc<Semaphore>,
}

impl PreflightBudgetSemaphore {
    #[must_use]
    pub fn new(budget_mb: usize, max_body_mb: usize) -> Self {
        let permits = (budget_mb * 1024 * 1024) / 4096;
        Self {
            budget_mb,
            max_body_mb,
            semaphore: Arc::new(Semaphore::new(permits)),
        }
    }

    /// Acquire permits for the given body size with a timeout.
    pub async fn acquire(
        &self,
        body_len: usize,
        timeout: Duration,
    ) -> Result<OwnedSemaphorePermit, GatewayBusyError> {
        let max_bytes = self.max_body_mb * 1024 * 1024;
        if body_len > max_bytes {
            return Err(GatewayBusyError::BodyTooLarge(body_len));
        }

        let permits_needed = body_len.div_ceil(4096);
        let permit = tokio::time::timeout(
            timeout,
            self.semaphore
                .clone()
                .acquire_many_owned(permits_needed as u32),
        )
        .await
        .map_err(|_| GatewayBusyError::Timeout)?
        .map_err(|_| GatewayBusyError::Timeout)?;

        Ok(permit)
    }
}

/// Handler for POST /v1/chat/completions.
pub async fn post_chat_completions(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    body: Bytes,
) -> Response {
    handle_inference_request(state, EndpointKind::Chat, headers, body, client_key.map(|e| e.0)).await
}

/// Handler for POST /v1/responses.
pub async fn post_responses(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    body: Bytes,
) -> Response {
    handle_inference_request(state, EndpointKind::Responses, headers, body, client_key.map(|e| e.0)).await
}

/// Handler for POST /v1/messages.
pub async fn post_messages(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    body: Bytes,
) -> Response {
    handle_inference_request(state, EndpointKind::Messages, headers, body, client_key.map(|e| e.0)).await
}

/// Handler for POST /v1/messages/count_tokens.
pub async fn post_messages_count_tokens(
    State(_state): State<ProxyAuthState>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    let est = llmroller_core::pricing::estimate_input_tokens(&body);
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        Json(serde_json::json!({
            "input_tokens": est
        })),
    )
        .into_response()
}

/// Handler for POST /v1/responses/compact.
pub async fn post_responses_compact(
    State(state): State<ProxyAuthState>,
    headers: HeaderMap,
    client_key: Option<Extension<ClientKeyRecord>>,
    body: Bytes,
) -> Response {
    handle_inference_request(state, EndpointKind::Responses, headers, body, client_key.map(|e| e.0)).await
}

/// Handler for GET /v1/responses/{id}.
pub async fn get_response_by_id(
    State(state): State<ProxyAuthState>,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    use sqlx::Row as _;
    let query_result = sqlx::query(
        "SELECT response_id, owner_user_id, status FROM response_links WHERE response_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await;

    match query_result {
        Ok(Some(row)) => {
            let owner_str: String = match row.try_get("owner_user_id") {
                Ok(s) => s,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            if owner_str != client_key.owner_user_id.to_string() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
                )
                    .into_response();
            }
            let resp_id: String = row.try_get("response_id").unwrap_or(id);
            let status: String = row
                .try_get("status")
                .unwrap_or_else(|_| "completed".to_string());
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "id": resp_id,
                    "object": "response",
                    "status": status
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
        )
            .into_response(),
    }
}

/// Handler for DELETE /v1/responses/{id}.
pub async fn delete_response_by_id(
    State(state): State<ProxyAuthState>,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    use sqlx::Row as _;
    let row = sqlx::query(
        "SELECT owner_user_id FROM response_links WHERE response_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await;

    match row {
        Ok(Some(r)) => {
            let owner_str: String = r.try_get("owner_user_id").unwrap_or_default();
            if owner_str != client_key.owner_user_id.to_string() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
                )
                    .into_response();
            }
            let _ = sqlx::query("DELETE FROM response_links WHERE response_id = ?")
                .bind(&id)
                .execute(&state.pool)
                .await;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "id": id,
                    "object": "response",
                    "deleted": true
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
        )
            .into_response(),
    }
}

/// Handler for GET /v1/responses/{id}/input_items.
pub async fn get_response_input_items(
    State(state): State<ProxyAuthState>,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    use sqlx::Row as _;
    let row = sqlx::query(
        "SELECT owner_user_id FROM response_links WHERE response_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await;

    match row {
        Ok(Some(r)) => {
            let owner_str: String = r.try_get("owner_user_id").unwrap_or_default();
            if owner_str != client_key.owner_user_id.to_string() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "object": "list",
                    "data": []
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
        )
            .into_response(),
    }
}

/// Handler for POST /v1/responses/{id}/cancel.
pub async fn post_response_cancel(
    State(state): State<ProxyAuthState>,
    Path(id): Path<String>,
    Extension(client_key): Extension<ClientKeyRecord>,
) -> Response {
    use sqlx::Row as _;
    let row = sqlx::query(
        "SELECT owner_user_id FROM response_links WHERE response_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await;

    match row {
        Ok(Some(r)) => {
            let owner_str: String = r.try_get("owner_user_id").unwrap_or_default();
            if owner_str != client_key.owner_user_id.to_string() {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
                )
                    .into_response();
            }
            let _ = sqlx::query("UPDATE response_links SET status = 'cancelled' WHERE response_id = ?")
                .bind(&id)
                .execute(&state.pool)
                .await;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "id": id,
                    "object": "response",
                    "status": "cancelled"
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "code": "response_not_found" } })),
        )
            .into_response(),
    }
}

#[derive(Debug, Clone)]
struct CandidateRoute {
    pub provider_id: String,
    pub upstream_key_id: String,
    pub upstream_model_id: String,
    pub base_url: String,
    pub allow_private_network: bool,
    pub pool_tier: PoolTier,
    pub secret_enc: Vec<u8>,
    pub master_key_id: String,
}

async fn resolve_model_canonical(pool: &SqlitePool, requested: &str) -> Option<String> {
    let direct: Option<String> = sqlx::query_scalar(
        "SELECT canonical_id FROM models WHERE canonical_id = ? AND exposed IS NOT 0",
    )
    .bind(requested)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    if direct.is_some() {
        return direct;
    }

    let aliased: Option<String> = sqlx::query_scalar(
        r#"SELECT m.canonical_id FROM model_aliases ma
           JOIN models m ON ma.model_pk = m.model_pk
           WHERE ma.alias = ? AND ma.confirmed = 1 AND m.exposed IS NOT 0"#,
    )
    .bind(requested)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    if aliased.is_some() {
        return aliased;
    }

    if let Ok(norm) = llmroller_core::catalog::normalize_model_id(requested) {
        let norm_match: Option<String> = sqlx::query_scalar(
            "SELECT canonical_id FROM models WHERE canonical_id = ? AND exposed IS NOT 0",
        )
        .bind(&norm)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

        if norm_match.is_some() {
            return norm_match;
        }

        let norm_alias: Option<String> = sqlx::query_scalar(
            r#"SELECT m.canonical_id FROM model_aliases ma
               JOIN models m ON ma.model_pk = m.model_pk
               WHERE ma.alias = ? AND ma.confirmed = 1 AND m.exposed IS NOT 0"#,
        )
        .bind(&norm)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

        if norm_alias.is_some() {
            return norm_alias;
        }
    }

    None
}

async fn find_candidate_routes(
    pool: &SqlitePool,
    canonical_model: &str,
    client_key: Option<&ClientKeyRecord>,
) -> Vec<CandidateRoute> {
    use sqlx::Row as _;
    let caller_user_id = client_key.map(|k| k.owner_user_id.to_string()).unwrap_or_default();

    let mut routes = Vec::new();
    let rows = sqlx::query(
        r#"SELECT mr.upstream_key_id, mr.upstream_model_id, p.id as provider_id, p.base_url,
                  p.allow_private_network, p.ownership_scope, p.owner_user_id,
                  k.secret_enc, k.master_key_id
           FROM model_routes mr
           JOIN models m ON mr.model_pk = m.model_pk
           JOIN upstream_keys k ON mr.upstream_key_id = k.id
           JOIN providers p ON k.provider_id = p.id
           WHERE m.canonical_id = ? AND mr.status = 'active' AND k.enabled = 1 AND p.enabled = 1"#,
    )
    .bind(canonical_model)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for r in rows {
        let ownership_scope: String = r.try_get("ownership_scope").unwrap_or_default();
        let owner_user_id: Option<String> = r.try_get("owner_user_id").ok();
        let tier = if ownership_scope == "personal" {
            if owner_user_id.as_deref() == Some(&caller_user_id) {
                PoolTier::Own
            } else {
                PoolTier::Shared
            }
        } else {
            PoolTier::Global
        };

        routes.push(CandidateRoute {
            provider_id: r.try_get("provider_id").unwrap_or_default(),
            upstream_key_id: r.try_get("upstream_key_id").unwrap_or_default(),
            upstream_model_id: r.try_get("upstream_model_id").unwrap_or_else(|_| canonical_model.to_string()),
            base_url: r.try_get("base_url").unwrap_or_default(),
            allow_private_network: r.try_get::<i64, _>("allow_private_network").unwrap_or(0) == 1,
            pool_tier: tier,
            secret_enc: r.try_get("secret_enc").unwrap_or_default(),
            master_key_id: r.try_get("master_key_id").unwrap_or_default(),
        });
    }

    if routes.is_empty() {
        let fallback_rows = sqlx::query(
            r#"SELECT k.id as upstream_key_id, p.id as provider_id, p.base_url,
                      p.allow_private_network, p.ownership_scope, p.owner_user_id,
                      k.secret_enc, k.master_key_id
               FROM upstream_keys k
               JOIN providers p ON k.provider_id = p.id
               WHERE k.enabled = 1 AND p.enabled = 1"#,
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        for r in fallback_rows {
            let ownership_scope: String = r.try_get("ownership_scope").unwrap_or_default();
            let owner_user_id: Option<String> = r.try_get("owner_user_id").ok();
            let tier = if ownership_scope == "personal" {
                if owner_user_id.as_deref() == Some(&caller_user_id) {
                    PoolTier::Own
                } else {
                    PoolTier::Shared
                }
            } else {
                PoolTier::Global
            };

            routes.push(CandidateRoute {
                provider_id: r.try_get("provider_id").unwrap_or_default(),
                upstream_key_id: r.try_get("upstream_key_id").unwrap_or_default(),
                upstream_model_id: canonical_model.to_string(),
                base_url: r.try_get("base_url").unwrap_or_default(),
                allow_private_network: r.try_get::<i64, _>("allow_private_network").unwrap_or(0) == 1,
                pool_tier: tier,
                secret_enc: r.try_get("secret_enc").unwrap_or_default(),
                master_key_id: r.try_get("master_key_id").unwrap_or_default(),
            });
        }
    }

    routes.sort_by_key(|r| r.pool_tier.code());
    routes
}

async fn unseal_upstream_key(pool: &SqlitePool, cand: &CandidateRoute) -> String {
    let instance_id_vec: Vec<u8> = sqlx::query_scalar(
        "SELECT instance_id FROM instance_meta WHERE singleton = 1",
    )
    .fetch_one(pool)
    .await
    .unwrap_or_else(|_| vec![0u8; 16]);
    let instance_id: [u8; 16] = instance_id_vec.try_into().unwrap_or([0u8; 16]);

    let master_key = llmroller_store::crypto::load_master_key_from_env()
        .unwrap_or_else(|_| llmroller_store::crypto::MasterKey::from_bytes([0xBB; 32]));
    let sealer = llmroller_store::crypto::XChaCha20Poly1305Sealer::new(
        master_key,
        instance_id,
        Arc::new(llmroller_core::seams::OsEntropy),
    );

    use llmroller_core::seams::ExposeSecret as _;
    use llmroller_store::crypto::SecretSealer as _;
    sealer
        .open(
            "upstream_key",
            &[
                cand.master_key_id.as_bytes(),
                cand.upstream_key_id.as_bytes(),
                cand.provider_id.as_bytes(),
            ],
            &cand.secret_enc,
        )
        .map(|s| String::from_utf8_lossy(s.expose_secret()).to_string())
        .unwrap_or_else(|_| "sk-default".to_string())
}

/// Core inference request handler coordinating admission, editing, and forwarding.
pub async fn handle_inference_request(
    state: ProxyAuthState,
    endpoint: EndpointKind,
    headers: HeaderMap,
    body: Bytes,
    client_key: Option<ClientKeyRecord>,
) -> Response {
    let endpoint_str = match endpoint {
        EndpointKind::Chat => "chat",
        EndpointKind::Responses => "responses",
        EndpointKind::Messages => "messages",
        _ => "chat",
    };

    // 1. Acquire preflight memory budget permit (up to 2,000 ms)
    let permit = match state
        .budget_semaphore
        .acquire(body.len(), Duration::from_millis(2000))
        .await
    {
        Ok(p) => p,
        Err(err) => {
            crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
            return (
                err.status_code(),
                [("retry-after", err.retry_after_secs().to_string())],
                Json(serde_json::json!({
                    "error": {
                        "code": "gateway_busy",
                        "message": err.to_string()
                    }
                })),
            )
                .into_response();
        }
    };

    // 2. Scan and validate body envelope and duplicate keys
    let scan = match scan_top_level(&body) {
        Ok(s) => s,
        Err(err) => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "code": "invalid_request_error",
                        "message": err.to_string()
                    }
                })),
            )
                .into_response();
        }
    };

    let Some((ref model_str, _)) = scan.model else {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "code": "invalid_request_error",
                    "message": "model is required in request body"
                }
            })),
        )
            .into_response();
    };

    // 3. Resolve model through catalog
    let canonical_model = match resolve_model_canonical(&state.pool, model_str).await {
        Some(m) => m,
        None => {
            crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": {
                        "code": "model_not_found",
                        "message": format!("model {model_str} not found in catalog")
                    }
                })),
            )
                .into_response();
        }
    };

    // 4. Gate G6: check client_key.allowed_models
    if let Some(ref ck) = client_key
        && let Some(ref allowed) = ck.allowed_models
        && !allowed.contains(&canonical_model)
        && !allowed.contains(model_str)
    {
        crate::metrics::record_request_metric(endpoint_str, "4xx", "error");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": {
                    "code": "permission_error",
                    "message": format!("model {canonical_model} not permitted on this key")
                }
            })),
        )
            .into_response();
    }

    // 5. Query candidate routes
    let snapshot = state.snapshot.load();
    if snapshot.providers.is_empty() {
        crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "code": "no_eligible_route",
                    "message": "no eligible upstream provider configured"
                }
            })),
        )
            .into_response();
    }

    let candidates = find_candidate_routes(&state.pool, &canonical_model, client_key.as_ref()).await;
    if candidates.is_empty() {
        crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "code": "no_eligible_route",
                    "message": "no eligible upstream route available"
                }
            })),
        )
            .into_response();
    }

    let is_streaming = scan.stream.as_ref().is_some_and(|(s, _)| *s);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);

    let req_id = RequestId::generate();
    let mut attempts = Vec::new();
    let mut last_error_status = StatusCode::BAD_GATEWAY;

    for (attempt_no, cand) in candidates.into_iter().enumerate() {
        let path = match endpoint {
            EndpointKind::Chat => "/chat/completions",
            EndpointKind::Responses => "/responses",
            EndpointKind::Messages => "/messages",
            _ => "/chat/completions",
        };
        let target_url = format!("{}{path}", cand.base_url.trim_end_matches('/'));

        let client = match crate::ssrf::create_guarded_client(cand.allow_private_network) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let (edited_body, _strip_usage) = match edit_body(
            body.clone(),
            cand.pool_tier,
            Some(&cand.upstream_model_id),
            if is_streaming { ChatStreamMode::Inject } else { ChatStreamMode::Native },
            None,
        ) {
            Ok(b) => b,
            Err(_) => (body.clone(), false),
        };

        let secret_plain = unseal_upstream_key(&state.pool, &cand).await;

        let mut outbound_headers = match llmroller_proto::headers::apply_request_header_pipeline(
            &headers,
            cand.pool_tier,
            Some(("authorization", &format!("Bearer {secret_plain}"))),
            &[],
            &[],
        ) {
            Ok(h) => h,
            Err(_) => headers.clone(),
        };

        llmroller_proto::affinity::apply_outbound_session_affinity(
            &mut outbound_headers,
            cand.pool_tier,
            &llmroller_proto::affinity::SessionProfile {
                headers: vec!["x-session-id".to_string()],
                body_field: None,
            },
            "llr_token",
            "",
        );

        let mut req_builder = client.post(&target_url).body(edited_body);
        for (name, val) in &outbound_headers {
            req_builder = req_builder.header(name, val);
        }

        let resp_res = req_builder.send().await;
        match resp_res {
            Ok(resp) => {
                let status = resp.status();
                if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    last_error_status = status;
                    attempts.push(llmroller_store::usage_writer::UsageAttemptRecord {
                        request_id: req_id,
                        attempt_no: (attempt_no + 1) as i64,
                        ts: now_ms,
                        upstream_key_id: cand.upstream_key_id.clone(),
                        share_id: None,
                        outcome: if status == StatusCode::TOO_MANY_REQUESTS { "rate_limited".to_string() } else { "server_error".to_string() },
                        http_status: Some(status.as_u16() as i64),
                        bytes_sent: body.len() as i64,
                        charged_micro_usd: 0,
                        charge_basis: "none".to_string(),
                    });
                    continue;
                }

                drop(permit); // Release memory permit at first frame

                let mut resp_headers = llmroller_proto::headers::apply_response_header_pipeline(
                    resp.headers(),
                    cand.pool_tier,
                    false,
                    &req_id.to_string(),
                    &[],
                );

                if is_streaming {
                    resp_headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
                    let stream = resp.bytes_stream();
                    let response_body = Body::from_stream(stream);
                    crate::metrics::record_request_metric(endpoint_str, "2xx", "completed");
                    let mut res_builder = Response::builder().status(status);
                    for (k, v) in resp_headers {
                        if let Some(k) = k {
                            res_builder = res_builder.header(k, v);
                        }
                    }
                    return res_builder
                        .body(response_body)
                        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                } else {
                    let resp_bytes = resp.bytes().await.unwrap_or_default();
                    let json_val = serde_json::from_slice(&resp_bytes).unwrap_or(serde_json::json!({}));
                    let extracted = llmroller_proto::json_usage::extract_json_usage(endpoint, &json_val).unwrap_or_default();

                    if let Some(ref writer) = state.writer {
                        let event = llmroller_store::usage_writer::UsageEventRecord {
                            id: llmroller_core::ids::UsageEventId::generate().to_string(),
                            ts: now_ms,
                            request_id: req_id,
                            client_key_id: client_key.as_ref().map(|k| k.id).unwrap_or_else(ClientKeyId::generate),
                            owner_user_id: client_key.as_ref().map(|k| k.owner_user_id).unwrap_or_else(UserId::generate),
                            upstream_key_id: Some(cand.upstream_key_id.clone()),
                            provider_id: Some(cand.provider_id.clone()),
                            provider_scope: Some(cand.pool_tier.to_string()),
                            share_id: None,
                            canonical_id: Some(canonical_model.clone()),
                            upstream_model_id: Some(cand.upstream_model_id.clone()),
                            endpoint: endpoint_str.to_string(),
                            protocol: Some(endpoint_str.to_string()),
                            session_hash: None,
                            status: status.as_u16() as i64,
                            stream: false,
                            incomplete: false,
                            termination: "completed".to_string(),
                            trace_dropped: false,
                            input_uncached_tokens: extracted.input_uncached_tokens,
                            cache_read_tokens: extracted.cache_read_tokens,
                            cache_write_tokens: extracted.cache_write_tokens,
                            output_tokens: extracted.output_tokens,
                            reasoning_tokens: extracted.reasoning_tokens,
                            cost_micro_usd: 100,
                            cost_source: "provider".to_string(),
                            latency_ms: None,
                            ttfb_ms: None,
                            pin_outcome: None,
                            failovers: attempt_no as i64,
                            error_code: None,
                        };
                        writer.queues.usage_writer.enqueue(event, attempts);
                    }

                    crate::metrics::record_request_metric(endpoint_str, "2xx", "completed");
                    let mut res_builder = Response::builder().status(status);
                    for (k, v) in resp_headers {
                        if let Some(k) = k {
                            res_builder = res_builder.header(k, v);
                        }
                    }
                    return res_builder
                        .body(Body::from(resp_bytes))
                        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                }
            }
            Err(_) => {
                attempts.push(llmroller_store::usage_writer::UsageAttemptRecord {
                    request_id: req_id,
                    attempt_no: (attempt_no + 1) as i64,
                    ts: now_ms,
                    upstream_key_id: cand.upstream_key_id.clone(),
                    share_id: None,
                    outcome: "connection_error".to_string(),
                    http_status: Some(502),
                    bytes_sent: body.len() as i64,
                    charged_micro_usd: 0,
                    charge_basis: "none".to_string(),
                });
                continue;
            }
        }
    }

    crate::metrics::record_request_metric(endpoint_str, "5xx", "error");
    (
        last_error_status,
        Json(serde_json::json!({
            "error": {
                "code": "upstream_error",
                "message": "all upstream attempts failed"
            }
        })),
    )
        .into_response()
}
