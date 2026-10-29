//! A trace record must resolve its `{"$blk": …}` block references (`ISSUE-0165`).
//!
//! # The defect
//!
//! `get_trace_record_internal` decrypted `trace_records.payload_enc` and returned the skeleton
//! verbatim — including every `{"$blk": "<sha256>"}` placeholder — without ever reading
//! `trace_blocks`. So `GET /api/user/traces/{request_id}` returned a trace whose prompt content was
//! a list of hashes. The same file already resolved them correctly for the session preview
//! (`get_session_preview_internal`), so the two read paths returned different shapes for the same
//! conceptual trace.
//!
//! # Why these drive the real endpoint
//!
//! The behaviour under test is "what does the endpoint return for a real sealed trace". A unit test
//! of the substitution helper would not cover the handler choosing to call it, nor the ownership
//! predicate on the block query — which is the security-critical part, because **a block hash is
//! not a secret**: it appears in `$blk` markers throughout every trace, so a hash is something any
//! reader of a trace already knows.
//!
//! # The three absences that must stay distinct
//!
//! - block **missing** (retention swept it, or a partial write),
//! - block **fails to decrypt** (wrong master key id, AEAD failure) — not absence, and collapsing
//!   the two hides a key-rotation fault behind a retention story,
//! - block **owned by another tenant**, which must look exactly like missing (`USR-3`, `SEC-17`).

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::ids::{ClientKeyId, RequestId, UserId};
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, OsEntropy};
use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
use llmroller_store::crypto::{MasterKey, XChaCha20Poly1305Sealer};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::traces::{SealJob, StoredTurnTraceV2, insert_sealed_trace_tx, seal_turn_sync};
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};

const SESSION_TOKEN_ALICE: &str = "trace_blocks_alice_token_0123456789ab";
const SESSION_TOKEN_BOB: &str = "trace_blocks_bob_token_0123456789abcd";
const CSRF_TOKEN: &str = "trace_blocks_csrf_token_0123456789abcd";

/// One sealed trace fixture: the request id to fetch, and the blocks it references.
struct SealedTrace {
    request_id: String,
    /// `(hash, plaintext)` for every block stored for this trace.
    blocks: Vec<(String, Vec<u8>)>,
}

struct Harness {
    _dir: tempfile::TempDir,
    app: axum::Router,
    pool: sqlx::SqlitePool,
    master_key: MasterKey,
    sealer: XChaCha20Poly1305Sealer,
    alice: UserId,
    bob: UserId,
}

async fn harness() -> Harness {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("trace_blocks.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x5B; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let alice = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "alice_blocks",
            password: "supersecretalicepass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("alice");
    let bob = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "bob_blocks",
            password: "supersecretbobpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("bob");

    let mut auth_state = AuthAppState::new(
        pool.clone(),
        AuthConfig {
            dev_insecure_cookies: true,
            require_tls: false,
            trusted_proxies: Vec::new(),
        },
    );
    auth_state.master_key = Some(master_key.clone());
    auth_state.master_key_id = "key-0".to_string();
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());

    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: None,
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let mut api_state =
        ApiAppState::new(auth_state.clone(), master_key.clone(), "key-0".to_string());
    api_state.audit_hmac_key = Some(instance_state.audit_hmac_key.clone());
    let app = control_plane_router_full(setup_state, auth_state.clone(), api_state);

    for (user, token) in [(alice.id, SESSION_TOKEN_ALICE), (bob.id, SESSION_TOKEN_BOB)] {
        let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
        let csrf_hash = llmroller_store::crypto::sha256_hex(CSRF_TOKEN.as_bytes());
        insert_web_session(
            &pool,
            NewWebSession {
                id_hash: &llmroller_store::crypto::sha256_hex(token.as_bytes()),
                user_id: user,
                csrf_hash: &csrf_hash,
                ip_addr: Some("127.0.0.1"),
                user_agent: Some("trace-blocks-test"),
                created_at: now_ms,
                expires_at: now_ms + 86_400_000,
            },
        )
        .await
        .expect("insert session");
    }

    // The sealer must use the instance's real HKDF salt, because `load_sealer` does — a
    // substitute would derive a different key set and nothing would decrypt.
    let instance_id: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_one(&pool)
            .await
            .expect("instance id");
    let salt: [u8; 16] = instance_id.try_into().expect("16-byte salt");
    let sealer = XChaCha20Poly1305Sealer::new(master_key.clone(), salt, Arc::new(OsEntropy));

    Harness {
        _dir: dir,
        app,
        pool,
        master_key,
        sealer,
        alice: alice.id,
        bob: bob.id,
    }
}

impl Harness {
    /// Create a client key for `owner`.
    async fn client_key(&self, owner: UserId, label: &str) -> ClientKeyId {
        let mut tx = self.pool.begin().await.expect("begin");
        let actor = ActorScope::user(owner, Role::User);
        let key = create_client_key_tx(
            &mut tx,
            &actor,
            owner,
            NewClientKey {
                label: label.to_string(),
                service_account_id: None,
                allowed_models: None,
                routing_scope: None,
                rate_limit_rpm: None,
                budget_micro_usd_month: None,
                allowed_cidrs: None,
                constant_session_mode: None,
                expires_at: None,
            },
            &OsEntropy,
            1000,
        )
        .await
        .expect("create client key");
        tx.commit().await.expect("commit");
        key.client_key.id
    }

    /// Seal one turn trace with the supplied skeleton and blocks, exactly as the write path does.
    ///
    /// `blocks` is `(hash, plaintext)`; `skeleton` is whatever JSON carries the `$blk` markers.
    async fn seal_trace(
        &self,
        owner: UserId,
        client_key: ClientKeyId,
        session_hash: &str,
        skeleton: serde_json::Value,
        block_refs: Vec<String>,
        blocks: Vec<(String, Vec<u8>)>,
    ) -> SealedTrace {
        let request_id = RequestId::generate();
        let usage_event_id = format!("ev-{}-{}", owner, request_id);
        sqlx::query(
            r#"INSERT INTO usage_events (
                   id, ts, request_id, client_key_id, owner_user_id,
                   endpoint, status, stream, incomplete, termination,
                   trace_dropped, cost_micro_usd, cost_source
               )
               VALUES (?, 1000, ?, ?, ?, 'chat', 200, 0, 0, 'completed', 0, 1000, 'provider')"#,
        )
        .bind(&usage_event_id)
        .bind(request_id.to_string())
        .bind(client_key.to_string())
        .bind(owner.to_string())
        .execute(&self.pool)
        .await
        .expect("insert usage event");

        let job = SealJob {
            master_key_id: "key-0".to_string(),
            usage_event_id,
            request_id,
            ts: 1000,
            owner_user_id: owner,
            client_key_id: client_key,
            session_hash: session_hash.to_string(),
            thread_id: "main".to_string(),
            canonical_id: "gpt-4o".to_string(),
            protocol: "chat".to_string(),
            truncated_content: false,
            turn_trace: StoredTurnTraceV2 {
                v: 2,
                request_skeleton: skeleton,
                block_refs,
                response: serde_json::json!({ "text": "assistant answer" }),
                response_raw: None,
                truncation: None,
            },
            blocks: blocks.clone(),
            expires_at: 1000 + 86400 * 14 * 1000,
        };
        let sealed = seal_turn_sync(&job, &self.sealer).expect("seal");
        let mut tx = self.pool.begin().await.expect("begin");
        insert_sealed_trace_tx(&mut tx, &sealed, "key-0")
            .await
            .expect("insert sealed trace");
        tx.commit().await.expect("commit");

        SealedTrace {
            request_id: request_id.to_string(),
            blocks,
        }
    }

    /// `GET /api/user/traces/{request_id}` as the given session token.
    async fn get_trace(&self, token: &str, request_id: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("GET")
            .uri(format!("/api/user/traces/{request_id}"))
            .header(header::COOKIE, format!("llmroller_session={token}"))
            .body(Body::empty())
            .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }
}

/// Every `{"$blk": …}` marker still present anywhere in `value`, as its hash.
///
/// Deliberately a walk rather than a field list: the assertion is "no placeholder survives", which
/// must hold wherever a placeholder happened to be.
fn unresolved_hashes(value: &serde_json::Value) -> Vec<String> {
    let mut found = Vec::new();
    fn walk(value: &serde_json::Value, found: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(fields) => {
                if let Some(hash) = fields.get("$blk").and_then(serde_json::Value::as_str) {
                    found.push(hash.to_string());
                    return;
                }
                for (_, nested) in fields {
                    walk(nested, found);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, found);
                }
            }
            _ => {}
        }
    }
    walk(value, &mut found);
    found
}

/// A skeleton, its ordered `block_refs`, and the `(hash, plaintext)` blocks it references.
type SkeletonFixture = (serde_json::Value, Vec<String>, Vec<(String, Vec<u8>)>);

/// Build a skeleton shaped like the real one: `system`, `tools`, `messages`, plus `block_refs`.
///
/// `block_refs` is a top-level field on `StoredTurnTraceV2`, but the markers live in the skeleton's
/// prompt-bearing fields — which is what `prepare_request_blocks` produces (`trace_tap.rs`).
fn skeleton_with(
    system: &str,
    tool: &str,
    messages: &[&str],
    unexpected: Option<&str>,
) -> SkeletonFixture {
    let mut blocks = Vec::new();
    let mut refs = Vec::new();
    let mut push = |plaintext: &str| {
        let bytes = serde_json::to_vec(&serde_json::json!({ "text": plaintext })).expect("json");
        let hash = llmroller_store::crypto::sha256_hex(&bytes);
        blocks.push((hash.clone(), bytes));
        refs.push(hash.clone());
        serde_json::json!({ "$blk": hash })
    };

    let system_ref = push(system);
    let tool_ref = push(tool);
    let message_refs: Vec<serde_json::Value> = messages.iter().map(|m| push(m)).collect();
    let unexpected_ref = unexpected.map(push);

    let mut skeleton = serde_json::json!({
        "model": "gpt-4o",
        "temperature": 0.7,
        "system": system_ref,
        "tools": [tool_ref],
        "messages": message_refs,
    });
    if let (Some(u), Some(object)) = (unexpected_ref, skeleton.as_object_mut()) {
        // A field the issue does not name, to catch a field-list implementation.
        object.insert("x_custom_block".to_string(), u);
    }
    (skeleton, refs, blocks)
}

// ---------------------------------------------------------------------------

#[doc = "Spec: TRC-2, TRC-4, Section 12.6 — a trace read must resolve its block references"]
#[doc = "Spec: ISSUE-0165"]
#[tokio::test]
async fn a_trace_record_resolves_its_block_references() {
    let h = harness().await;
    let client_key = h.client_key(h.alice, "alice-trc").await;
    let (skeleton, refs, blocks) = skeleton_with(
        "SYSTEM_PROMPT_TEXT",
        "TOOL_SCHEMA_TEXT",
        &["message one"],
        None,
    );
    let sealed = h
        .seal_trace(h.alice, client_key, "alice-session", skeleton, refs, blocks)
        .await;

    let (status, body) = h.get_trace(SESSION_TOKEN_ALICE, &sealed.request_id).await;
    assert_eq!(status, StatusCode::OK, "trace must be readable: {body}");

    assert!(
        unresolved_hashes(&body).is_empty(),
        "no `$blk` placeholder may survive the read; still unresolved: {:?}",
        unresolved_hashes(&body)
    );
    let serialized = body.to_string();
    for needle in ["SYSTEM_PROMPT_TEXT", "TOOL_SCHEMA_TEXT", "message one"] {
        assert!(
            serialized.contains(needle),
            "resolved block content {needle:?} must appear in the response: {serialized}"
        );
    }
}

#[doc = "Spec: TRC-2 — resolution must be a tree walk, not a list of known fields"]
#[doc = "Spec: ISSUE-0165"]
#[tokio::test]
async fn a_block_referenced_from_an_unexpected_field_is_still_resolved() {
    let h = harness().await;
    let client_key = h.client_key(h.alice, "alice-trc").await;
    let (skeleton, refs, blocks) = skeleton_with(
        "SYSTEM_PROMPT_TEXT",
        "TOOL_SCHEMA_TEXT",
        &["message one"],
        Some("UNEXPECTED_FIELD_CONTENT"),
    );
    let sealed = h
        .seal_trace(h.alice, client_key, "alice-session", skeleton, refs, blocks)
        .await;

    let (status, body) = h.get_trace(SESSION_TOKEN_ALICE, &sealed.request_id).await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        body.to_string().contains("UNEXPECTED_FIELD_CONTENT"),
        "a `$blk` in a field the issue does not name must still be resolved; a field-list \
         implementation passes every other test and fails this one: {body}"
    );
    assert!(
        unresolved_hashes(&body).is_empty(),
        "no placeholder may survive: {:?}",
        unresolved_hashes(&body)
    );
}

#[doc = "Spec: USR-3, SEC-17 — a block hash is not a secret, so ownership must be enforced"]
#[doc = "Spec: ISSUE-0165"]
#[tokio::test]
async fn a_block_owned_by_another_user_is_not_resolved() {
    let h = harness().await;
    let alice_key = h.client_key(h.alice, "alice-trc").await;
    let bob_key = h.client_key(h.bob, "bob-trc").await;

    // Bob seals a trace with a secret block.
    let (bob_skeleton, bob_refs, bob_blocks) = skeleton_with(
        "BOB_SECRET_PROMPT",
        "BOB_SECRET_TOOL",
        &["bob message"],
        None,
    );
    let bob_trace = h
        .seal_trace(
            h.bob,
            bob_key,
            "bob-session",
            bob_skeleton,
            bob_refs,
            bob_blocks.clone(),
        )
        .await;
    let bob_secret_hash = bob_trace.blocks[0].0.clone();

    // Alice's own trace references *Bob's* block hash. A hash is not a secret — it appears in
    // `$blk` markers — so this is reachable without any privilege.
    let alice_skeleton = serde_json::json!({
        "model": "gpt-4o",
        "system": { "$blk": bob_secret_hash },
    });
    let alice_trace = h
        .seal_trace(
            h.alice,
            alice_key,
            "alice-session",
            alice_skeleton,
            vec![bob_secret_hash.clone()],
            Vec::new(),
        )
        .await;

    let (status, body) = h
        .get_trace(SESSION_TOKEN_ALICE, &alice_trace.request_id)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "her own trace must still be readable"
    );

    let serialized = body.to_string();
    assert!(
        !serialized.contains("BOB_SECRET_PROMPT"),
        "another tenant's block must never be returned, even by hash: {serialized}"
    );
    // And it must render as *missing*, not as something that hints a row exists elsewhere.
    assert!(
        serialized.contains(&bob_secret_hash),
        "the reference must remain visibly unresolved rather than being dropped: {serialized}"
    );
    // This is the assertion that actually detects a dropped `owner_user_id` predicate.
    //
    // Removing the predicate does **not** leak plaintext, because a block's AAD binds its owner:
    // sealed under `(master_key_id, owner_user_id, block_hash)`, another tenant's row fails to
    // open. So a plaintext-only assertion cannot fail, and a mutant that drops the predicate would
    // look killed when it is not. What it leaks instead is *existence plus key material*: the row
    // is found, so the renderer says `decrypt_failed` rather than `missing_block`, telling a
    // caller that a block with that hash exists under some other owner and was sealed with a
    // different master key id. Foreign blocks must be indistinguishable from swept ones.
    assert!(
        serialized.contains("\"$unresolved\":\"missing_block\"")
            || serialized.contains("\"$unresolved\": \"missing_block\""),
        "another tenant's block must render exactly like a missing one; `decrypt_failed` here \
         would disclose that the row exists and was sealed under a different key: {serialized}"
    );
    assert!(
        !serialized.contains("decrypt_failed"),
        "a foreign block must never be reported as a decryption failure: {serialized}"
    );
    let _ = bob_trace.request_id;
}

#[doc = "Spec: TRC-4 — a swept block must be distinguishable from an empty one"]
#[doc = "Spec: ISSUE-0165"]
#[tokio::test]
async fn a_missing_block_is_distinguishable_from_an_empty_one() {
    let h = harness().await;
    let client_key = h.client_key(h.alice, "alice-trc").await;

    // A block that was never stored: "the block is gone".
    let never_stored = llmroller_store::crypto::sha256_hex(b"a block that was never written");
    // A block that *is* stored and whose content is empty: "the block was empty".
    let empty_bytes = Vec::new();
    let empty_hash = llmroller_store::crypto::sha256_hex(&empty_bytes);

    let skeleton = serde_json::json!({
        "model": "gpt-4o",
        "system": { "$blk": never_stored },
        "messages": [{ "$blk": empty_hash }],
    });
    let sealed = h
        .seal_trace(
            h.alice,
            client_key,
            "alice-session",
            skeleton,
            vec![never_stored.clone(), empty_hash.clone()],
            vec![(empty_hash.clone(), empty_bytes)],
        )
        .await;

    let (status, body) = h.get_trace(SESSION_TOKEN_ALICE, &sealed.request_id).await;
    assert_eq!(status, StatusCode::OK);

    let serialized = body.to_string();
    assert!(
        serialized.contains("missing_block"),
        "a swept block must say so explicitly rather than becoming null or an empty value; \
         an operator must be able to tell 'the block is gone' from 'the block was empty': \
         {serialized}"
    );
    // The empty block resolved successfully, so its marker is gone from the *skeleton*. Scoped to
    // `request_skeleton` because `block_refs` deliberately keeps its ordered hash list — that list
    // is what the LCP reconstruction is defined over, and substituting it would destroy it.
    let skeleton = serde_json::to_string(&body["request_skeleton"]).expect("skeleton");
    assert!(
        !skeleton.contains(&empty_hash),
        "the empty block was stored and opened, so its placeholder must be resolved away — \
         absence and emptiness must not render identically: {skeleton}"
    );
    assert_eq!(
        body["request_skeleton"]["messages"][0], "",
        "an empty block resolves to empty content, which is different from an unresolved marker:          {skeleton}"
    );
}

#[doc = "Spec: TRC-4 — a decrypt failure is not an absence"]
#[doc = "Spec: ISSUE-0165"]
#[tokio::test]
async fn a_block_that_fails_to_decrypt_is_reported_distinctly_from_a_missing_one() {
    let h = harness().await;
    let client_key = h.client_key(h.alice, "alice-trc").await;

    let corrupt_hash = "c".repeat(64);
    let missing_hash = llmroller_store::crypto::sha256_hex(b"never stored at all");

    // A row that exists but whose payload cannot be opened: sealed under a *different* master key
    // id, so AEAD authentication fails.
    let foreign_sealer = XChaCha20Poly1305Sealer::new(
        MasterKey::from_bytes([0x7E; 32]),
        [0x11; 16],
        Arc::new(OsEntropy),
    );
    use llmroller_store::crypto::SecretSealer as _;
    let bogus_payload = foreign_sealer.seal(
        "trace_block",
        &[
            b"key-OTHER",
            h.alice.to_string().as_bytes(),
            corrupt_hash.as_bytes(),
        ],
        b"UNDECRYPTABLE_CONTENT",
    );

    let skeleton = serde_json::json!({
        "model": "gpt-4o",
        "system": { "$blk": corrupt_hash },
        "messages": [{ "$blk": missing_hash }],
    });
    let sealed = h
        .seal_trace(
            h.alice,
            client_key,
            "alice-session",
            skeleton,
            vec![corrupt_hash.clone(), missing_hash.clone()],
            Vec::new(),
        )
        .await;

    // Insert the undecryptable row directly; `insert_sealed_trace_tx` would seal a valid one.
    sqlx::query(
        "INSERT INTO trace_blocks (block_hash, owner_user_id, payload_enc, master_key_id, \
                                   byte_len, last_referenced_at) \
         VALUES (?, ?, ?, 'key-OTHER', 22, 1000)",
    )
    .bind(&corrupt_hash)
    .bind(h.alice.to_string())
    .bind(&bogus_payload)
    .execute(&h.pool)
    .await
    .expect("insert corrupt block");

    let (status, body) = h.get_trace(SESSION_TOKEN_ALICE, &sealed.request_id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "one bad block must not fail the read: {body}"
    );

    let serialized = body.to_string();
    assert!(
        serialized.contains("decrypt_failed"),
        "an AEAD failure must be reported distinctly from absence — otherwise a key-rotation \
         fault is hidden behind a retention story: {serialized}"
    );
    assert!(
        serialized.contains("missing_block"),
        "the missing block must be reported as missing in the same response: {serialized}"
    );
    assert!(
        !serialized.contains("UNDECRYPTABLE_CONTENT"),
        "a block that failed to open must never leak plaintext: {serialized}"
    );
    let _ = h.master_key;
}

#[doc = "Spec: TRC-2, TRC-4 — resolution must not disturb the rest of the payload"]
#[doc = "Spec: ISSUE-0165"]
#[tokio::test]
async fn resolving_blocks_does_not_change_the_rest_of_the_payload() {
    let h = harness().await;
    let client_key = h.client_key(h.alice, "alice-trc").await;
    let (skeleton, refs, blocks) = skeleton_with(
        "SYSTEM_PROMPT_TEXT",
        "TOOL_SCHEMA_TEXT",
        &["message one", "message two"],
        None,
    );
    let sealed = h
        .seal_trace(h.alice, client_key, "alice-session", skeleton, refs, blocks)
        .await;

    let (status, body) = h.get_trace(SESSION_TOKEN_ALICE, &sealed.request_id).await;
    assert_eq!(status, StatusCode::OK);

    // The top-level `StoredTurnTraceV2` shape is what clients parse, so its keys must be exactly
    // as before: only placeholders may change.
    let object = body.as_object().expect("trace is a JSON object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "block_refs",
            "request_skeleton",
            "response",
            "response_raw",
            "truncation",
            "v",
        ],
        "the response shape must not gain or lose fields: {body}"
    );

    // Values that are not placeholders come back untouched.
    assert_eq!(body["v"], 2);
    assert_eq!(body["response"]["text"], "assistant answer");
    assert_eq!(body["request_skeleton"]["model"], "gpt-4o");
    assert_eq!(
        body["request_skeleton"]["temperature"], 0.7,
        "non-block fields must be byte-identical"
    );
    // `block_refs` is the ordered list of hashes, not a prompt field: it stays as hashes.
    assert_eq!(
        body["block_refs"].as_array().map(Vec::len),
        Some(4),
        "block_refs must keep its ordered hash list: {body}"
    );
}

#[doc = "Spec: TRC-2 — a block's bytes may not be JSON, and must not break the read"]
#[doc = "Spec: ISSUE-0165"]
#[tokio::test]
async fn a_non_json_block_is_resolved_as_text_rather_than_failing_the_read() {
    let h = harness().await;
    let client_key = h.client_key(h.alice, "alice-trc").await;

    let raw = b"PLAIN_TEXT_BLOCK_CONTENT".to_vec();
    let hash = llmroller_store::crypto::sha256_hex(&raw);
    let skeleton = serde_json::json!({
        "model": "gpt-4o",
        "system": { "$blk": hash },
    });
    let sealed = h
        .seal_trace(
            h.alice,
            client_key,
            "alice-session",
            skeleton,
            vec![hash],
            vec![(llmroller_store::crypto::sha256_hex(&raw), raw)],
        )
        .await;

    let (status, body) = h.get_trace(SESSION_TOKEN_ALICE, &sealed.request_id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a non-JSON block must not turn the whole trace into a 500: {body}"
    );
    assert!(
        body.to_string().contains("PLAIN_TEXT_BLOCK_CONTENT"),
        "a non-JSON block must come back as text: {body}"
    );
    assert!(unresolved_hashes(&body).is_empty());
}

/// Guard against a test that resolves nothing while claiming to test resolution.
#[doc = "Spec: ISSUE-0165 — fixture self-check"]
#[tokio::test]
async fn the_fixture_actually_stores_the_blocks_it_references() {
    let h = harness().await;
    let client_key = h.client_key(h.alice, "alice-trc").await;
    let (skeleton, refs, blocks) = skeleton_with(
        "SYSTEM_PROMPT_TEXT",
        "TOOL_SCHEMA_TEXT",
        &["message one"],
        None,
    );
    let sealed = h
        .seal_trace(
            h.alice,
            client_key,
            "alice-session",
            skeleton,
            refs.clone(),
            blocks.clone(),
        )
        .await;

    let stored: Vec<String> =
        sqlx::query_scalar("SELECT block_hash FROM trace_blocks WHERE owner_user_id = ?")
            .bind(h.alice.to_string())
            .fetch_all(&h.pool)
            .await
            .expect("select block hashes");
    let stored_set: HashMap<&str, ()> = stored.iter().map(|s| (s.as_str(), ())).collect();
    for (hash, _) in &sealed.blocks {
        assert!(
            stored_set.contains_key(hash.as_str()),
            "fixture bug: referenced block {hash} was not stored, so the read test would assert \
             resolution against nothing"
        );
        assert!(refs.contains(hash), "fixture bug: {hash} not in block_refs");
    }
}
