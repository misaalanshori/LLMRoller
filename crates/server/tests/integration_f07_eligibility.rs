//! End-to-end coverage for F07 route-preferences, share fallback, and exposure.
//!
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#101-candidate-eligibility-rules-c1-through-c8
//! Requirements: MDL-2, MDL-3, RTE-3, SHR-1.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use llmroller_core::ids::{ModelPk, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role};
use llmroller_core::seams::{ExposeSecret as _, OsEntropy};
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::models::upsert_discovered_models_tx;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::settings::{seed_defaults, update_setting};
use llmroller_store::users::{NewUser, create_user};
use tower::ServiceExt as _;

struct TestContext {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    admin_id: UserId,
    user_id: UserId,
    client_key_token: String,
}

async fn setup() -> TestContext {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("f07.db"), PoolRole::Writer)
        .await
        .expect("pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0x47; 32]);
    let entropy = OsEntropy;
    load_or_initialize(&pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance");
    seed_defaults(&pool, None, 1).await.expect("settings");

    let system_actor = ActorScope::system(
        llmroller_core::rbac::SystemTask::Writer,
        llmroller_core::rbac::SystemSeal::store(),
    );
    let admin = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "f07_admin",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("admin");
    let user = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "f07_user",
            password: "supersecretuserpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("user");

    let mut tx = pool.begin().await.expect("begin");
    let created = create_client_key_tx(
        &mut tx,
        &ActorScope::user(admin.id, Role::Admin),
        admin.id,
        NewClientKey {
            label: "f07".to_string(),
            service_account_id: None,
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
    .expect("client key");
    tx.commit().await.expect("commit");

    TestContext {
        _dir: dir,
        pool,
        admin_id: admin.id,
        user_id: user.id,
        client_key_token: created.raw_token.expose_secret().to_string(),
    }
}

async fn create_global_key(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    master_key: &MasterKey,
    name: &str,
    secret: &str,
) -> (
    llmroller_core::ids::ProviderId,
    llmroller_core::ids::UpstreamKeyId,
) {
    let provider = create_provider_tx(
        tx,
        actor,
        NewProvider {
            name: name.to_string(),
            kind: ProviderKind::Openrouter,
            base_url: format!("https://{name}.example/v1"),
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
        tx,
        actor,
        master_key,
        secret,
        Arc::new(OsEntropy),
        NewUpstreamKey {
            provider_id: provider.id,
            label: format!("{name} key"),
            secret: llmroller_core::seams::secret_string(secret),
            priority: Some(100),
            strict_tier: None,
            enabled: Some(true),
        },
        1000,
    )
    .await
    .expect("key");
    (provider.id, key.id)
}

async fn visible_models(ctx: &TestContext) -> Vec<(String, bool)> {
    let snapshot = llmroller_server::reloader::load_initial_snapshot(&ctx.pool)
        .await
        .expect("snapshot");
    let shared = Arc::new(ArcSwap::from_pointee(snapshot));
    let state = ProxyAuthState::new(shared, ctx.pool.clone(), vec![]);
    state.is_ready.store(true, Ordering::SeqCst);
    let app = data_plane_router(state);

    let request = Request::builder()
        .uri("/v1/models")
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", ctx.client_key_token),
        )
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    json["data"]
        .as_array()
        .expect("data array")
        .iter()
        .filter_map(|model| {
            model["id"].as_str().map(|id| {
                (
                    id.to_string(),
                    model["llmroller"]["available"].as_bool().unwrap_or(false),
                )
            })
        })
        .collect()
}

#[doc = "Spec: RTE-3, SHR-1"]
#[tokio::test]
async fn test_user_route_preferences_are_key_scoped_and_do_not_hide_sibling_routes() {
    let ctx = setup().await;
    let actor = ActorScope::user(ctx.admin_id, Role::Admin);
    let master_key = MasterKey::from_bytes([0x47; 32]);
    let mut tx = ctx.pool.begin().await.expect("begin");

    let (_, key_a) = create_global_key(&mut tx, &actor, &master_key, "a", "sk-a").await;
    let (_, key_b) = create_global_key(&mut tx, &actor, &master_key, "b", "sk-b").await;
    upsert_discovered_models_tx(
        &mut tx,
        &key_a,
        true,
        None,
        1000,
        &["openai/gpt-5".to_string()],
        64,
    )
    .await
    .expect("discovery a");
    upsert_discovered_models_tx(
        &mut tx,
        &key_b,
        true,
        None,
        1000,
        &["openai/gpt-5".to_string()],
        64,
    )
    .await
    .expect("discovery b");

    let model_pk: String =
        sqlx::query_scalar("SELECT model_pk FROM models WHERE canonical_id = 'gpt-5'")
            .fetch_one(&mut *tx)
            .await
            .expect("model");
    // Disable key A for this user while key B remains enabled with a custom weight.
    sqlx::query(
        "INSERT INTO user_route_preferences
             (user_id, upstream_key_id, model_pk, priority_override, enabled)
         VALUES (?, ?, ?, 100, 0), (?, ?, ?, 222, 1)",
    )
    .bind(ctx.user_id.to_string())
    .bind(key_a.to_string())
    .bind(&model_pk)
    .bind(ctx.user_id.to_string())
    .bind(key_b.to_string())
    .bind(&model_pk)
    .execute(&mut *tx)
    .await
    .expect("preferences");
    tx.commit().await.expect("commit");

    let snapshot = llmroller_server::reloader::load_initial_snapshot(&ctx.pool)
        .await
        .expect("snapshot");
    assert_eq!(
        snapshot.user_route_prefs.len(),
        2,
        "both per-key preferences must survive the snapshot build"
    );
    let model_id = ModelPk::parse(&model_pk).expect("model pk");
    let pref_b = snapshot
        .user_route_prefs
        .get(&(ctx.user_id, key_b, model_id))
        .expect("key B preference");
    assert_eq!(pref_b.priority_override, 222);
    assert!(pref_b.enabled);

    let visible = visible_models(&ctx).await;
    assert!(
        visible.iter().any(|(id, _)| id == "gpt-5"),
        "disabling one key must not hide the same model served by another key"
    );
}

#[doc = "Spec: MDL-2, MDL-3, RTE-3"]
#[tokio::test]
async fn test_pending_review_keeps_majority_vendor_routable() {
    let ctx = setup().await;
    let actor = ActorScope::user(ctx.admin_id, Role::Admin);
    let master_key = MasterKey::from_bytes([0x47; 32]);
    let mut tx = ctx.pool.begin().await.expect("begin");

    let mut keys = Vec::new();
    for index in 0..3 {
        let (_, key) = create_global_key(
            &mut tx,
            &actor,
            &master_key,
            &format!("majority-{index}"),
            &format!("sk-majority-{index}"),
        )
        .await;
        keys.push(key);
    }
    let (_, minority_key) =
        create_global_key(&mut tx, &actor, &master_key, "minority", "sk-minority").await;

    for key in &keys {
        upsert_discovered_models_tx(
            &mut tx,
            key,
            true,
            None,
            1000,
            &["openai/gpt-4o".to_string()],
            64,
        )
        .await
        .expect("majority discovery");
    }
    upsert_discovered_models_tx(
        &mut tx,
        &minority_key,
        true,
        None,
        1000,
        &["custom/gpt-4o".to_string()],
        64,
    )
    .await
    .expect("minority discovery");
    tx.commit().await.expect("commit");

    let snapshot = llmroller_server::reloader::load_initial_snapshot(&ctx.pool)
        .await
        .expect("snapshot");
    let model = snapshot
        .models_by_canonical
        .get("gpt-4o")
        .and_then(|pk| snapshot.models.get(pk))
        .expect("model");
    assert_eq!(
        model.merge_state,
        llmroller_core::catalog::MergeState::PendingReview
    );
    assert_eq!(model.majority_vendor_prefix.as_deref(), Some("openai"));

    let visible = visible_models(&ctx).await;
    assert!(
        visible
            .iter()
            .any(|(id, available)| id == "gpt-4o" && *available),
        "the majority vendor route must remain routable during pending review"
    );
}

#[doc = "Spec: MDL-3, RTE-3"]
#[tokio::test]
async fn test_intersection_mode_hides_models_missing_from_a_usable_provider() {
    let ctx = setup().await;
    let actor = ActorScope::user(ctx.admin_id, Role::Admin);
    let master_key = MasterKey::from_bytes([0x47; 32]);
    let mut tx = ctx.pool.begin().await.expect("begin");

    let (_, serving_key) =
        create_global_key(&mut tx, &actor, &master_key, "serving", "sk-serving").await;
    let (_other_provider, _other_key) =
        create_global_key(&mut tx, &actor, &master_key, "other", "sk-other").await;
    upsert_discovered_models_tx(
        &mut tx,
        &serving_key,
        true,
        None,
        1000,
        &["openai/partial-model".to_string()],
        64,
    )
    .await
    .expect("discovery");
    tx.commit().await.expect("commit");

    update_setting(&ctx.pool, "global_exposure_mode", "\"intersection\"", 2000)
        .await
        .expect("setting");

    let visible = visible_models(&ctx).await;
    assert!(
        !visible.iter().any(|(id, _)| id == "partial-model"),
        "intersection must hide a model not served by every usable global provider"
    );

    // An admin force-exposed model bypasses the automatic intersection rule.
    sqlx::query("UPDATE models SET exposed = 1 WHERE canonical_id = 'partial-model'")
        .execute(&ctx.pool)
        .await
        .expect("force expose");
    let visible = visible_models(&ctx).await;
    assert!(
        visible.iter().any(|(id, _)| id == "partial-model"),
        "exposed = 1 must override intersection"
    );
}
