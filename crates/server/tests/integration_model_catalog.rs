//! Integration tests for model catalog discovery, normalization, merge, and /v1/models endpoint.
//!
//! Spec: Section 6.6, Section 10.6, Section 14.2.2, Section 19.1 (ADR-7),
//! Requirements: MDL-1, MDL-2, MDL-3, MDL-4, MDL-5, MDL-6, MDL-7.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_core::ids::{ModelPk, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role};
use llmroller_core::seams::OsEntropy;
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::models::{add_model_alias_tx, merge_models_tx, upsert_discovered_models_tx};
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

async fn setup_catalog_test_db() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    MasterKey,
    UserId,
    UserId,
) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let db_path = temp_dir.path().join("catalog_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x77; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(
        llmroller_core::rbac::SystemTask::Writer,
        llmroller_core::rbac::SystemSeal::store(),
    );

    let admin = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "admin_user",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("admin");

    let regular = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "regular_user",
            password: "supersecretuserpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("regular");

    (temp_dir, writer_pool, master_key, admin.id, regular.id)
}

#[doc = "Spec: MDL-1, MDL-2, MDL-3, MDL-4, MDL-5, MDL-6, MDL-7"]
#[tokio::test]
async fn test_models_discovery_lifecycle_and_partial_guards() {
    let (_dir, pool, master_key, admin_id, _regular_id) = setup_catalog_test_db().await;
    let admin_actor = ActorScope::user(admin_id, Role::Admin);
    let mut tx = pool.begin().await.expect("begin");

    let provider = create_provider_tx(
        &mut tx,
        &admin_actor,
        NewProvider {
            name: "Global Provider".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("provider");

    let key = create_upstream_key_tx(
        &mut tx,
        &admin_actor,
        &master_key,
        "sk-test-secret",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Main Key".to_string(),
            secret: llmroller_core::seams::secret_string("sk-test-secret"),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("upstream key");

    // 1. Discovery with invalid character IDs -> skipped with warning (MDL-1)
    let raw_list = vec![
        "anthropic/claude-sonnet-4.6".to_string(),
        "openai/gpt-5.4-pro:free".to_string(),
        "invalid model with space and \n newline".to_string(),
    ];
    let res = upsert_discovered_models_tx(&mut tx, &key.id, true, None, 1_000_000, &raw_list, 100)
        .await
        .expect("discovery");

    assert_eq!(res.total_discovered, 2); // 2 valid, 1 invalid skipped
    assert_eq!(res.routes_added_or_updated, 2);
    assert!(!res.is_partial);

    // 2. Partial discovery (> 1,000 models or > 2 MiB) (MDL-1)
    let mut large_list = Vec::new();
    for i in 0..1005 {
        large_list.push(format!("vendor/model-{i}"));
    }
    let res_partial =
        upsert_discovered_models_tx(&mut tx, &key.id, true, None, 1_000_000, &large_list, 50_000)
            .await
            .expect("partial discovery");

    assert!(res_partial.is_partial);
    // In partial discovery, absent routes are NOT marked missing
    assert_eq!(res_partial.routes_marked_missing, 0);

    // 3. Full discovery transition: absent routes marked missing, then retired after 7 days (MDL-1)
    let fresh_list = vec!["openai/gpt-5.4-pro:free".to_string()];
    let res_full =
        upsert_discovered_models_tx(&mut tx, &key.id, true, None, 2_000_000, &fresh_list, 100)
            .await
            .expect("full discovery");

    assert!(!res_full.is_partial);
    assert!(res_full.routes_marked_missing > 0);

    // Run again after 8 days (now + 8 * 86_400_000): missing routes must transition to retired
    let eight_days_later = 2_000_000 + 8 * 86_400_000;
    let res_retired = upsert_discovered_models_tx(
        &mut tx,
        &key.id,
        true,
        None,
        eight_days_later,
        &fresh_list,
        100,
    )
    .await
    .expect("retired transition");

    assert!(res_retired.routes_retired > 0);

    tx.commit().await.expect("commit");
}

#[doc = "Spec: MDL-1, MDL-2, MDL-3, MDL-4, MDL-5, MDL-6, MDL-7"]
#[tokio::test]
async fn test_models_exposure_merge_and_killswitch() {
    let (_dir, pool, master_key, _admin_id, user_id) = setup_catalog_test_db().await;
    let user_actor = ActorScope::user(user_id, Role::User);
    let mut tx = pool.begin().await.expect("begin");

    let provider = create_provider_tx(
        &mut tx,
        &user_actor,
        NewProvider {
            name: "Personal Provider".to_string(),
            kind: ProviderKind::Custom,
            base_url: "https://personal.ai/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(user_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("provider");

    let key = create_upstream_key_tx(
        &mut tx,
        &user_actor,
        &master_key,
        "sk-personal",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Personal Key".to_string(),
            secret: llmroller_core::seams::secret_string("sk-personal"),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("upstream key");

    // Personal discovery: creates origin = 'personal' (MDL-5)
    let personal_models = vec!["custom/personal-v1".to_string()];
    upsert_discovered_models_tx(
        &mut tx,
        &key.id,
        false,
        Some(&user_id),
        1_000_000,
        &personal_models,
        100,
    )
    .await
    .expect("personal discovery");

    let origin: String =
        sqlx::query_scalar("SELECT origin FROM models WHERE canonical_id = 'personal-v1'")
            .fetch_one(&mut *tx)
            .await
            .expect("origin query");
    assert_eq!(origin, "personal");

    // Global discovery finds the same model -> upgraded to 'global' (MDL-5)
    let system_actor = ActorScope::system(
        llmroller_core::rbac::SystemTask::Writer,
        llmroller_core::rbac::SystemSeal::store(),
    );
    let global_provider = create_provider_tx(
        &mut tx,
        &system_actor,
        NewProvider {
            name: "Global Router".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("global provider");

    let global_key = create_upstream_key_tx(
        &mut tx,
        &system_actor,
        &master_key,
        "sk-global",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: global_provider.id,
            label: "Global Key".to_string(),
            secret: llmroller_core::seams::secret_string("sk-global"),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("global key");

    upsert_discovered_models_tx(
        &mut tx,
        &global_key.id,
        true,
        None,
        1_000_000,
        &personal_models,
        100,
    )
    .await
    .expect("global discovery");

    let upgraded_origin: String =
        sqlx::query_scalar("SELECT origin FROM models WHERE canonical_id = 'personal-v1'")
            .fetch_one(&mut *tx)
            .await
            .expect("upgraded origin query");
    assert_eq!(upgraded_origin, "global");

    // Transactional merge A -> B (MDL-4)
    let pk_a = ModelPk::generate();
    let pk_b = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, origin) VALUES (?, 'model-a', 'global')",
    )
    .bind(pk_a.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model a");
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, origin) VALUES (?, 'model-b', 'global')",
    )
    .bind(pk_b.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model b");

    merge_models_tx(&mut tx, &pk_a, &pk_b, 2_000_000)
        .await
        .expect("merge models");

    let count_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM models WHERE model_pk = ?")
        .bind(pk_a.to_string())
        .fetch_one(&mut *tx)
        .await
        .expect("count a");
    assert_eq!(count_a, 0);

    let redirect_target: String = sqlx::query_scalar(
        "SELECT model_pk FROM model_redirects WHERE old_canonical_id = 'model-a'",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("redirect query");
    assert_eq!(redirect_target, pk_b.to_string());

    // Confirmed alias applies during discovery route attachment (MDL-6)
    add_model_alias_tx(&mut tx, "model-b-alias", &pk_b)
        .await
        .expect("add alias");

    // Discovery sees "vendor/model-b-alias"
    let alias_discovery = vec!["vendor/model-b-alias".to_string()];
    let _res_alias = upsert_discovered_models_tx(
        &mut tx,
        &global_key.id,
        true,
        None,
        1_000_000,
        &alias_discovery,
        100,
    )
    .await
    .expect("discovery with alias");

    // Route must attach to pk_b, not create a new model row
    let attached_pk: String = sqlx::query_scalar(
        "SELECT model_pk FROM model_routes WHERE upstream_key_id = ? AND upstream_model_id = 'vendor/model-b-alias'",
    )
    .bind(global_key.id.to_string())
    .fetch_one(&mut *tx)
    .await
    .expect("fetch attached model_pk");
    assert_eq!(attached_pk, pk_b.to_string());
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM models WHERE canonical_id = 'model-b-alias'")
            .fetch_one(&mut *tx)
            .await
            .expect("count");
    assert_eq!(
        count, 0,
        "Discovered model matching confirmed alias must NOT create duplicate model row (MDL-6)"
    );

    tx.commit().await.expect("commit");
}

#[doc = "Spec: MDL-1, MDL-2, MDL-3, MDL-4, MDL-5, MDL-6, MDL-7"]
#[tokio::test]
async fn test_v1_models_endpoint_openai_and_anthropic_formats() {
    let (_dir, pool, master_key, admin_id, _regular_id) = setup_catalog_test_db().await;
    let admin_actor = ActorScope::user(admin_id, Role::Admin);
    let mut tx = pool.begin().await.expect("begin");

    let created_key = create_client_key_tx(
        &mut tx,
        &admin_actor,
        admin_id,
        NewClientKey {
            label: "Test Key".to_string(),
            allowed_models: None,
            routing_scope: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            expires_at: None,
            constant_session_mode: None,
        },
        &OsEntropy,
        1000,
    )
    .await
    .expect("create client key");

    use llmroller_core::seams::ExposeSecret as _;
    let client_key_token = created_key.raw_token.expose_secret().to_string();

    let pk_available = ModelPk::generate();
    let pk_unavailable = ModelPk::generate();
    let pk_killswitched = ModelPk::generate();

    sqlx::query("INSERT INTO models (model_pk, canonical_id, display_name, exposed, origin) VALUES (?, 'available-model', 'Available Model', NULL, 'global')")
        .bind(pk_available.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert available");

    sqlx::query("INSERT INTO models (model_pk, canonical_id, display_name, exposed, origin) VALUES (?, 'unavailable-model', 'Unavailable Forced Model', 1, 'global')")
        .bind(pk_unavailable.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert unavailable");

    sqlx::query("INSERT INTO models (model_pk, canonical_id, display_name, exposed, origin) VALUES (?, 'killed-model', 'Killed Model', 0, 'global')")
        .bind(pk_killswitched.to_string())
        .execute(&mut *tx)
        .await
        .expect("insert killswitched");

    // Add active route for available-model
    let p = create_provider_tx(
        &mut tx,
        &admin_actor,
        NewProvider {
            name: "P".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        1000,
    )
    .await
    .expect("provider");

    let k = create_upstream_key_tx(
        &mut tx,
        &admin_actor,
        &master_key,
        "sk-test",
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: p.id,
            label: "K".to_string(),
            secret: llmroller_core::seams::secret_string("sk-test"),
            priority: Some(1),
            strict_tier: None,
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("key");

    let protocols = r#"["chat","responses","messages"]"#;
    sqlx::query(
        "INSERT INTO model_routes (upstream_key_id, model_pk, upstream_model_id, protocols, status, discovered_at) VALUES (?, ?, 'avail-upstream', ?, 'active', 1000)",
    )
    .bind(k.id.to_string())
    .bind(pk_available.to_string())
    .bind(protocols)
    .execute(&mut *tx)
    .await
    .expect("route");

    // Add confirmed alias for available-model
    add_model_alias_tx(&mut tx, "avail-alias", &pk_available)
        .await
        .expect("alias");

    tx.commit().await.expect("commit");

    // Setup ProxyAuthState using load_initial_snapshot
    let snapshot = llmroller_server::reloader::load_initial_snapshot(&pool)
        .await
        .expect("load initial snapshot");
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let auth_state = ProxyAuthState::new(shared_snapshot, pool.clone(), vec![]);
    auth_state
        .is_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let app = data_plane_router(auth_state);

    // 1. OpenAI format GET /v1/models (MDL-7)
    let req = Request::builder()
        .uri("/v1/models")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .body(Body::empty())
        .expect("req");

    let resp = app.clone().oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");

    assert_eq!(json["object"], "list");
    let data = json["data"].as_array().expect("data array");

    // Must contain available-model (available: true)
    let avail = data
        .iter()
        .find(|m| m["id"] == "available-model")
        .expect("available-model must be in list");
    assert_eq!(avail["llmroller"]["available"], true);

    // Must contain unavailable-model (exposed = 1, available: false)
    let unavail = data
        .iter()
        .find(|m| m["id"] == "unavailable-model")
        .expect("unavailable-model must be in list");
    assert_eq!(unavail["llmroller"]["available"], false);

    // Must NOT contain killed-model (exposed = 0)
    assert!(
        data.iter().all(|m| m["id"] != "killed-model"),
        "killswitched model must be excluded"
    );

    // Must contain alias item with alias_of
    let alias_item = data
        .iter()
        .find(|m| m["id"] == "avail-alias")
        .expect("alias item must be present");
    assert_eq!(alias_item["llmroller"]["alias_of"], "available-model");

    // 2. Anthropic format GET /v1/models (MDL-7)
    let req_anthropic = Request::builder()
        .uri("/v1/models")
        .header(header::AUTHORIZATION, format!("Bearer {client_key_token}"))
        .header("anthropic-version", "2023-06-01")
        .body(Body::empty())
        .expect("req");

    let resp_anthropic = app.oneshot(req_anthropic).await.expect("resp");
    assert_eq!(resp_anthropic.status(), StatusCode::OK);
    let body_anthropic = resp_anthropic
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let json_anthropic: serde_json::Value = serde_json::from_slice(&body_anthropic).expect("json");

    assert!(json_anthropic["data"].is_array());
    let anthropic_data = json_anthropic["data"].as_array().expect("array");
    assert!(anthropic_data.iter().any(|m| m["id"] == "available-model"));
    assert_eq!(anthropic_data[0]["type"], "model");
}
