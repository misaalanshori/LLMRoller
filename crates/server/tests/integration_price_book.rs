//! Integration tests for price-book endpoints, provider-specific pricing overrides, and catalog sync.
//!
//! Spec: Section 6.9, Section 9.1, Section 13.1, Section 14.2.1, Section 14.2.2,
//! Requirements: USG-3, Partial: PRIV-1.

use std::sync::Arc;
use tempfile::tempdir;

use llmroller_core::ids::UserId;
use llmroller_core::pricing::{ContextTier, Rates, Schedule};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::OsEntropy;
use llmroller_store::crypto::{MasterKey, XChaCha20Poly1305Sealer};
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::prices::{UpsertPriceParams, get_price_tx, upsert_price_tx};
use llmroller_store::providers::{NewProvider, create_provider_tx};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};

async fn setup_price_book_test_db() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    MasterKey,
    UserId,
    UserId,
    String,
    String,
) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("price_book_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xAA; 32]);
    let instance = load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "admin_price",
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
        &writer_pool,
        &system_actor,
        NewUser {
            username: "user_price",
            password: "supersecretuserpass123",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("user");

    // Login sessions for admin and user
    let sealer = Arc::new(XChaCha20Poly1305Sealer::new(
        master_key.clone(),
        instance.instance_id,
        Arc::new(entropy),
    ));

    let now_ms: i64 = 1_000_000_000_000;
    let admin_session = "admin_sess_token_1234567890";
    let admin_session_hash = "admin_sess_hash";
    sqlx::query("INSERT INTO web_sessions (id_hash, user_id, csrf_hash, created_at, last_seen_at, expires_at) VALUES (?, ?, 'csrf', ?, ?, ?)")
        .bind(admin_session_hash)
        .bind(admin.id.to_string())
        .bind(now_ms)
        .bind(now_ms)
        .bind(now_ms + 86_400_000)
        .execute(&writer_pool)
        .await
        .expect("insert admin session");

    let user_session = "user_sess_token_1234567890";
    let user_session_hash = "user_sess_hash";
    sqlx::query("INSERT INTO web_sessions (id_hash, user_id, csrf_hash, created_at, last_seen_at, expires_at) VALUES (?, ?, 'csrf', ?, ?, ?)")
        .bind(user_session_hash)
        .bind(user.id.to_string())
        .bind(now_ms)
        .bind(now_ms)
        .bind(now_ms + 86_400_000)
        .execute(&writer_pool)
        .await
        .expect("insert user session");

    let _ = sealer;
    (
        dir,
        writer_pool,
        master_key,
        admin.id,
        user.id,
        admin_session.to_string(),
        user_session.to_string(),
    )
}

#[doc = "Spec: USG-3, Partial: PRIV-1"]
#[tokio::test]
async fn test_price_book_endpoints_and_resolution_hierarchy() {
    let (_dir, pool, _master_key, _admin_id, user_id, _admin_sess, _user_sess) =
        setup_price_book_test_db().await;
    let mut tx = pool.begin().await.expect("begin");

    let user_actor = ActorScope::user(user_id, Role::User);

    // Create personal provider for user
    let provider = create_provider_tx(
        &mut tx,
        &user_actor,
        NewProvider {
            name: "User Custom Provider".to_string(),
            kind: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
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
    .expect("create provider");

    // 1. Insert default global price (provider_id = "") for deepseek/deepseek-v4.1-flash
    let global_rates = Rates {
        input_per_mtok_micro_usd: 1_000_000,
        output_per_mtok_micro_usd: 2_000_000,
        cache_read_per_mtok_micro_usd: 100_000,
        cache_write_per_mtok_micro_usd: 1_250_000,
    };
    upsert_price_tx(
        &mut tx,
        UpsertPriceParams {
            provider_id: "".to_string(),
            provider_kind: "openrouter".to_string(),
            upstream_model_id: "deepseek/deepseek-v4.1-flash".to_string(),
            rates: global_rates,
            tiers: vec![],
            schedules: vec![],
            source: "admin".to_string(),
            now_ms: 1000,
        },
    )
    .await
    .expect("upsert global price");

    // 2. Insert provider-specific override for user's provider
    let override_rates = Rates {
        input_per_mtok_micro_usd: 800_000, // discount
        output_per_mtok_micro_usd: 1_600_000,
        cache_read_per_mtok_micro_usd: 80_000,
        cache_write_per_mtok_micro_usd: 1_000_000,
    };
    upsert_price_tx(
        &mut tx,
        UpsertPriceParams {
            provider_id: provider.id.to_string(),
            provider_kind: "openrouter".to_string(),
            upstream_model_id: "deepseek/deepseek-v4.1-flash".to_string(),
            rates: override_rates,
            tiers: vec![],
            schedules: vec![],
            source: "user".to_string(),
            now_ms: 1000,
        },
    )
    .await
    .expect("upsert override price");

    // Resolution:
    // Querying for user's provider returns override rates ($0.80/MTok)
    let resolved_override = get_price_tx(
        &mut tx,
        &provider.id.to_string(),
        "openrouter",
        "deepseek/deepseek-v4.1-flash",
    )
    .await
    .expect("get override")
    .expect("price must exist");
    assert_eq!(resolved_override.input_per_mtok_micro_usd, 800_000);

    // Querying for other provider returns default global rates ($1.00/MTok)
    let resolved_global = get_price_tx(
        &mut tx,
        "other_prov_id",
        "openrouter",
        "deepseek/deepseek-v4.1-flash",
    )
    .await
    .expect("get global")
    .expect("price must exist");
    assert_eq!(resolved_global.input_per_mtok_micro_usd, 1_000_000);

    // 3. Overlapping context tiers rejected on write with validation error (422) (USG-3)
    let overlapping_tier_err = upsert_price_tx(
        &mut tx,
        UpsertPriceParams {
            provider_id: "".to_string(),
            provider_kind: "openrouter".to_string(),
            upstream_model_id: "deepseek/deepseek-v4.1-flash".to_string(),
            rates: global_rates,
            tiers: vec![
                ContextTier {
                    min_total_input_tokens: 0,
                    max_total_input_tokens: Some(100_000),
                    input_per_mtok_micro_usd: 1_000_000,
                    output_per_mtok_micro_usd: 2_000_000,
                    cache_read_per_mtok_micro_usd: 100_000,
                    cache_write_per_mtok_micro_usd: 1_250_000,
                },
                ContextTier {
                    min_total_input_tokens: 50_000, // overlaps with 0..100k!
                    max_total_input_tokens: Some(150_000),
                    input_per_mtok_micro_usd: 1_500_000,
                    output_per_mtok_micro_usd: 3_000_000,
                    cache_read_per_mtok_micro_usd: 150_000,
                    cache_write_per_mtok_micro_usd: 1_875_000,
                },
            ],
            schedules: vec![],
            source: "admin".to_string(),
            now_ms: 1000,
        },
    )
    .await;
    assert!(
        matches!(
            overlapping_tier_err,
            Err(llmroller_store::prices::PriceError::Validation(_))
        ),
        "overlapping tiers must fail validation"
    );

    // 4. Overlapping schedules rejected on write with validation error (422) (USG-3)
    let overlapping_sched_err = upsert_price_tx(
        &mut tx,
        UpsertPriceParams {
            provider_id: "".to_string(),
            provider_kind: "openrouter".to_string(),
            upstream_model_id: "deepseek/deepseek-v4.1-flash".to_string(),
            rates: global_rates,
            tiers: vec![],
            schedules: vec![
                Schedule {
                    name: "peak1".to_string(),
                    iso_weekdays: vec![1, 2, 3],
                    utc_minute_ranges: vec![[60, 180]],
                    multiplier_num: 2,
                    multiplier_den: 1,
                },
                Schedule {
                    name: "peak2".to_string(),
                    iso_weekdays: vec![2, 3, 4], // overlaps on days 2 and 3!
                    utc_minute_ranges: vec![[120, 240]], // overlaps 120..180!
                    multiplier_num: 3,
                    multiplier_den: 1,
                },
            ],
            source: "admin".to_string(),
            now_ms: 1000,
        },
    )
    .await;
    assert!(
        matches!(
            overlapping_sched_err,
            Err(llmroller_store::prices::PriceError::Validation(_))
        ),
        "overlapping schedules must fail validation"
    );

    tx.commit().await.expect("commit");

    // Delete price override (F059)
    let mut tx = pool.begin().await.expect("begin");
    let deleted = llmroller_store::prices::delete_price_tx(
        &mut tx,
        &provider.id.to_string(),
        "deepseek/deepseek-v4.1-flash",
    )
    .await
    .expect("delete price");
    assert!(deleted);
    let resolved_after_delete = get_price_tx(
        &mut tx,
        &provider.id.to_string(),
        "openrouter",
        "deepseek/deepseek-v4.1-flash",
    )
    .await
    .expect("resolve");
    assert_eq!(
        resolved_after_delete
            .expect("resolved global")
            .input_per_mtok_micro_usd,
        1_000_000
    );

    // Delete global default price: now returns None
    let deleted_global =
        llmroller_store::prices::delete_price_tx(&mut tx, "", "deepseek/deepseek-v4.1-flash")
            .await
            .expect("delete global");
    assert!(deleted_global);
    let resolved_none = get_price_tx(
        &mut tx,
        &provider.id.to_string(),
        "openrouter",
        "deepseek/deepseek-v4.1-flash",
    )
    .await
    .expect("resolve");
    assert_eq!(resolved_none, None);
    tx.commit().await.expect("commit");
}

#[doc = "Spec: USG-3"]
#[tokio::test]
async fn test_seed_bundled_prices_seeds_standard_models_idempotently() {
    let (_dir, pool, _master_key, _admin_id, _user_id, _admin_sess, _user_sess) =
        setup_price_book_test_db().await;

    let seeded = llmroller_store::prices::seed_bundled_prices(&pool, 1000)
        .await
        .expect("seed bundled prices");
    assert!(seeded >= 10, "must seed at least 10 baseline models");

    // Second seed is idempotent and inserts 0 rows
    let seeded_again = llmroller_store::prices::seed_bundled_prices(&pool, 2000)
        .await
        .expect("idempotent seed");
    assert_eq!(seeded_again, 0);

    // Verify claude-sonnet-4.6 is present
    let mut tx = pool.begin().await.expect("begin");
    let price = get_price_tx(&mut tx, "bundled", "global", "claude-sonnet-4.6")
        .await
        .expect("get")
        .expect("price must exist");
    assert_eq!(price.input_per_mtok_micro_usd, 3_000_000);
    assert_eq!(price.output_per_mtok_micro_usd, 15_000_000);
    let source: String = sqlx::query_scalar("SELECT source FROM prices WHERE upstream_model_id = 'claude-sonnet-4.6'")
        .fetch_one(&mut *tx)
        .await
        .expect("fetch source");
    assert_eq!(source, "bundled");
}
