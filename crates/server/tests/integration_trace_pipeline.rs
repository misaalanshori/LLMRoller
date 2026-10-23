//! End-to-end trace pipeline test: request -> sealer pool -> SQLite trace tables.
//!
//! Spec: Section 6.10, Section 9.2, Section 9.3.3
//! Requirements: TRC-1, TRC-2, TRC-3, TRC-4, TRC-9.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt as _;
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_core::ids::{ModelPk, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, SystemSeal, SystemTask};
use llmroller_core::seams::{ExposeSecret as _, OsEntropy, secret_string};
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_server::reloader::load_initial_snapshot;
use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
use llmroller_store::crypto::{MasterKey, SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::traces::{SealerPool, StoredTurnTraceV2};
use llmroller_store::writer::start_writer;

#[derive(Clone, Default)]
struct MockState {
    calls: Arc<AtomicUsize>,
}

async fn mock_chat(State(state): State<MockState>, body: axum::body::Bytes) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    let body_str = String::from_utf8_lossy(&body);
    if body_str.contains("\"stream\":true") || body_str.contains("\"stream\": true") {
        let stream_body = concat!(
            "data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"delta\":{\"content\":\"Hello from \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"the trace stream!\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            stream_body,
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            [("content-type", "application/json")],
            Json(serde_json::json!({
                "id": "chatcmpl-trace-1",
                "object": "chat.completion",
                "model": "gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "trace pipeline answer" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
            })),
        )
            .into_response()
    }
}

async fn spawn_mock_upstream() -> (SocketAddr, MockState) {
    let state = MockState::default();
    let app = Router::new()
        .route("/chat/completions", post(mock_chat))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, state)
}

async fn wait_for_trace_record(pool: &sqlx::SqlitePool, expected_count: usize) {
    for _ in 0..100 {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace_records")
            .fetch_one(pool)
            .await
            .unwrap_or(0);
        if count as usize >= expected_count {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {expected_count} trace records to persist via writer actor");
}

fn decrypt_trace(
    sealer: &XChaCha20Poly1305Sealer,
    payload_enc: &[u8],
    request_id: &str,
    owner_user_id: &str,
    session_hash: &str,
) -> StoredTurnTraceV2 {
    let sealed = sealer
        .open(
            "trace_record",
            &[
                b"key-0",
                request_id.as_bytes(),
                owner_user_id.as_bytes(),
                session_hash.as_bytes(),
            ],
            payload_enc,
        )
        .expect("open trace payload");
    let json = zstd::bulk::decompress(sealed.expose_secret(), 10 * 1024 * 1024)
        .expect("decompress trace payload");
    serde_json::from_slice(&json).expect("parse StoredTurnTraceV2")
}

#[tokio::test]
#[doc = "Spec: TRC-1, TRC-2, TRC-3, TRC-4, TRC-9"]
async fn test_trace_pipeline_persists_streaming_and_non_streaming_turns() {
    let (mock_addr, mock_state) = spawn_mock_upstream().await;

    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("trace_pipeline.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xCC; 32]);
    let instance = load_or_initialize(&pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let mut tx = pool.begin().await.expect("begin");
    let actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let user_id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, 'trace_user', 'h', 'user', 1000)",
    )
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert user");

    let client_key = create_client_key_tx(
        &mut tx,
        &actor,
        user_id,
        NewClientKey {
            label: "trace_key".to_string(),
            service_account_id: None,
            allowed_models: Some(vec!["gpt-4o".to_string()]),
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &entropy,
        1000,
    )
    .await
    .expect("create client key");
    let client_token = client_key.raw_token.expose_secret().to_string();

    let provider = create_provider_tx(
        &mut tx,
        &actor,
        NewProvider {
            name: "Trace Provider".to_string(),
            kind: ProviderKind::Custom,
            base_url: format!("http://{mock_addr}"),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: true,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("create provider");

    let upstream_key = create_upstream_key_tx(
        &mut tx,
        &actor,
        &master_key,
        "key-0",
        Arc::new(entropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Trace Key".to_string(),
            secret: secret_string("sk-trace-key"),
            priority: Some(100),
            strict_tier: Some(1),
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("create upstream key");

    let model_pk = ModelPk::generate();
    sqlx::query("INSERT INTO models (model_pk, canonical_id, exposed) VALUES (?, 'gpt-4o', 1)")
        .bind(model_pk.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert model");
    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, discovered_at) \
         VALUES (?, ?, 'gpt-4o', '[\"chat\"]', 'active', 1000)",
    )
    .bind(upstream_key.id.to_string())
    .bind(model_pk.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert route");
    tx.commit().await.expect("commit");

    let (reloader_tx, _reloader_rx) = tokio::sync::mpsc::channel(100);
    let (writer, _writer_task) = start_writer(pool.clone(), reloader_tx);

    let mut snapshot = load_initial_snapshot(&pool).await.expect("load snapshot");
    let mut settings = (*snapshot.settings).clone();
    settings.traceability_config.enabled = true;
    settings.traceability_config.retention_days = 7;
    snapshot.settings = Arc::new(settings);
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));

    let sealer = Arc::new(XChaCha20Poly1305Sealer::new(
        master_key.clone(),
        instance.instance_id,
        Arc::new(OsEntropy),
    ));
    let (sealer_pool, sealed_rx) = SealerPool::new(sealer.clone());
    let sealer_pool = Arc::new(sealer_pool);

    // Spawn production bridge moving sealed traces into the SQLite writer queue
    let trace_writer = writer.clone();
    std::thread::Builder::new()
        .name("llmroller-trace-enqueue".to_string())
        .spawn(move || {
            while let Ok(sealed) = sealed_rx.recv() {
                trace_writer.queues.push_sealed_trace(sealed);
            }
        })
        .expect("spawn trace enqueue thread");

    let mut proxy_state = ProxyAuthState::new(shared_snapshot, pool.clone(), vec![])
        .with_master_key(master_key.clone());
    proxy_state.is_ready.store(true, Ordering::SeqCst);
    proxy_state.writer = Some(writer);
    proxy_state.master_key_id = Some("key-0".to_string());
    proxy_state.sealer_pool = Some(sealer_pool.clone());
    let app = data_plane_router(proxy_state);

    // Non-streaming turn.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hello trace"}]}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json["choices"][0]["message"]["content"],
        "trace pipeline answer"
    );

    wait_for_trace_record(&pool, 1).await;

    let (payload, request_id, owner_user_id, session_hash) =
        sqlx::query_as::<_, (Vec<u8>, String, String, String)>(
            "SELECT payload_enc, request_id, owner_user_id, session_hash FROM trace_records",
        )
        .fetch_one(&pool)
        .await
        .expect("trace record");
    let turn = decrypt_trace(
        &sealer,
        &payload,
        &request_id,
        &owner_user_id,
        &session_hash,
    );
    assert_eq!(turn.v, 2);
    assert_eq!(turn.response["assistant_text"], "trace pipeline answer");
    assert_eq!(turn.response["response_id"], "chatcmpl-trace-1");
    assert_eq!(turn.block_refs.len(), 1, "one user message block");
    assert_eq!(turn.request_skeleton["model"], "gpt-4o");
    assert_eq!(
        turn.request_skeleton["messages"][0]["$blk"],
        turn.block_refs[0]
    );
    let block_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace_blocks")
        .fetch_one(&pool)
        .await
        .expect("block count");
    assert_eq!(block_count, 1);

    // Streaming turn.
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {client_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-4o","messages":[{"role":"user","content":"stream trace"}],"stream":true}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let stream_body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let stream_text = String::from_utf8_lossy(&stream_body);
    assert!(stream_text.contains("Hello from "));
    assert!(stream_text.contains("the trace stream!"));

    wait_for_trace_record(&pool, 2).await;

    let records: Vec<(Vec<u8>, String, String, String)> = sqlx::query_as(
        "SELECT payload_enc, request_id, owner_user_id, session_hash FROM trace_records ORDER BY ts",
    )
    .fetch_all(&pool)
    .await
    .expect("trace records");
    assert_eq!(records.len(), 2);
    let stream_turn = decrypt_trace(
        &sealer,
        &records[1].0,
        &records[1].1,
        &records[1].2,
        &records[1].3,
    );
    assert_eq!(
        stream_turn.response["assistant_text"],
        "Hello from the trace stream!"
    );
    assert_eq!(stream_turn.response["finish_reason"], "stop");

    let dropped: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_events WHERE trace_dropped = 1")
            .fetch_one(&pool)
            .await
            .expect("dropped count");
    assert_eq!(dropped, 0);
    assert_eq!(mock_state.calls.load(Ordering::SeqCst), 2);
}
