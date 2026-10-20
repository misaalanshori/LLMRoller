//! Integration test for Reloader task, ConfigSnapshot slices, and RuntimeState.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#72-reliability-and-availability-rel
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#2162-versioned-configsnapshot-publication-reloader
//! Phase: ./docs/bigbang/big-bang-phases-1.md#16-sub-phase-16-dual-priority-sqlite-writer-closed-settings-registry-reloader-task-and-runtimestate

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::seams::OsEntropy;
use llmroller_core::snapshot::{CacheInvalidationEvent, ConfigDomain};
use llmroller_core::strategy::RoutingStrategy;
use llmroller_server::reloader::{load_initial_snapshot, start_reloader};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::{SettingsError, seed_defaults, update_setting, validate_setting};
use llmroller_store::users::{NewUser, create_user};
use llmroller_store::writer::{ControlOp, WriterError, start_writer};
use sqlx::{Sqlite, Transaction};
use tempfile::tempdir;
use tokio::sync::mpsc;

struct UpdateSettingOp {
    key: String,
    value_json: String,
}

impl ControlOp for UpdateSettingOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WriterError>> + Send + 'a>>
    {
        Box::pin(async move {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            sqlx::query(
                "INSERT OR REPLACE INTO settings (key, value_json, updated_at) VALUES (?, ?, ?)",
            )
            .bind(self.key)
            .bind(self.value_json)
            .bind(now)
            .execute(&mut **tx)
            .await?;
            Ok(())
        })
    }
}

struct UpdateUserOp {
    user_id: UserId,
    disabled: bool,
}

impl ControlOp for UpdateUserOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WriterError>> + Send + 'a>>
    {
        Box::pin(async move {
            let disabled_val = if self.disabled { 1i64 } else { 0i64 };
            sqlx::query("UPDATE users SET disabled = ? WHERE id = ?")
                .bind(disabled_val)
                .bind(self.user_id.to_string())
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    }
}

async fn setup_test_context() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    sqlx::SqlitePool,
    UserId,
) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("llmroller.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    let reader_pool = open_pool(&db_path, PoolRole::Reader)
        .await
        .expect("reader pool");

    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0x42; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance init");

    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let initial_user = create_user(
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
    .expect("initial user");

    (dir, writer_pool, reader_pool, initial_user.id)
}

/// Spec: REL-4
/// 50 concurrent alternating control-plane mutations commit through the Control queue
/// (incrementing instance_meta.config_version in the same transaction) and the single
/// Reloader task coalesces notifications, rebuilds only changed domain Arc slices in one
/// read transaction, never publishes an older config_version over a newer one, and applies
/// CacheInvalidationEvents after arc_swap.store(...).
#[doc = "Spec: REL-4"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_50_concurrent_control_mutations_and_reloader_coalescing() {
    let (_dir, writer_pool, reader_pool, initial_user_id) = setup_test_context().await;

    let initial_snapshot = load_initial_snapshot(&reader_pool)
        .await
        .expect("load initial snapshot");
    assert_eq!(initial_snapshot.config_version, 0);
    assert_eq!(initial_snapshot.users.len(), 1);
    assert!(initial_snapshot.users.contains_key(&initial_user_id));

    let shared_snapshot = Arc::new(ArcSwap::new(Arc::new(initial_snapshot.clone())));
    let runtime_state = Arc::new(RuntimeState::new());

    let (reloader_tx, reloader_rx) = mpsc::channel(100);
    let (writer, _writer_task) = start_writer(writer_pool.clone(), reloader_tx.clone());
    let (reloader, _reloader_task) = start_reloader(
        reader_pool.clone(),
        shared_snapshot.clone(),
        runtime_state.clone(),
        reloader_rx,
        reloader_tx,
    );

    // Verify selective slice rebuild baseline before concurrency:
    // Update settings only -> snapshot.users Arc should be identical pointer!
    let v1 = writer
        .execute_control(
            vec![ConfigDomain::Settings],
            vec![CacheInvalidationEvent::SettingsChanged],
            UpdateSettingOp {
                key: "pin_ttl_secs".to_string(),
                value_json: "90000".to_string(),
            },
        )
        .await
        .expect("execute settings control");
    reloader
        .wait_for_version(v1, Duration::from_secs(1))
        .await
        .expect("wait for v1");

    let snap_after_settings = reloader.snapshot();
    assert_eq!(snap_after_settings.config_version, v1);
    assert_eq!(snap_after_settings.settings.pin_ttl_secs, 90_000);
    // Unchanged slice pointer equality check per REL-4 selective slice rebuilding
    assert!(Arc::ptr_eq(
        &snap_after_settings.users,
        &initial_snapshot.users
    ));
    assert!(!Arc::ptr_eq(
        &snap_after_settings.settings,
        &initial_snapshot.settings
    ));

    // Now spawn 50 concurrent alternating control mutations
    let mut handles = Vec::new();
    let num_mutations = 50;

    for i in 0..num_mutations {
        let writer = writer.clone();
        let reloader = reloader.clone();
        let user_id = initial_user_id;

        handles.push(tokio::spawn(async move {
            let start = Instant::now();
            let (target_v, domain) = if i % 2 == 0 {
                // Settings mutation: alternate pin_ttl_secs
                let val = 86_400 + i as i64;
                let v = writer
                    .execute_control(
                        vec![ConfigDomain::Settings],
                        vec![CacheInvalidationEvent::SettingsChanged],
                        UpdateSettingOp {
                            key: "pin_ttl_secs".to_string(),
                            value_json: val.to_string(),
                        },
                    )
                    .await
                    .expect("control settings");
                (v, ConfigDomain::Settings)
            } else {
                // User mutation: toggle disabled
                let disabled = (i % 4) == 1;
                let v = writer
                    .execute_control(
                        vec![ConfigDomain::Users],
                        vec![CacheInvalidationEvent::UserChanged(user_id)],
                        UpdateUserOp { user_id, disabled },
                    )
                    .await
                    .expect("control user");
                (v, ConfigDomain::Users)
            };

            // Await reloader's version notification (must be within 1 s per REL-4)
            let published_v = reloader
                .wait_for_version(target_v, Duration::from_secs(1))
                .await
                .expect("reloader wait_for_version");
            assert!(
                published_v >= target_v,
                "published version {published_v} must be >= target {target_v}"
            );
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "control mutation and reloader notification must complete within 1 s"
            );
            (target_v, domain)
        }));
    }

    let mut versions = Vec::new();
    for handle in handles {
        let (v, _domain) = handle.await.expect("join task");
        versions.push(v);
    }

    assert_eq!(versions.len(), 50);

    // Verify final state: snapshot version must match instance_meta.config_version in DB
    let final_db_version: i64 = sqlx::query_scalar("SELECT config_version FROM instance_meta")
        .fetch_one(&reader_pool)
        .await
        .expect("fetch final db version");

    let final_snap = reloader.snapshot();
    assert_eq!(final_snap.config_version, final_db_version);
    assert!(final_snap.config_version >= 50);

    // Verify CacheInvalidationEvents were applied to RuntimeState
    assert!(
        runtime_state
            .last_reload_ts
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
    );
    assert!(
        runtime_state
            .reload_count
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
    );
}

/// Spec: REL-4
/// While the Data queue is saturated with 4 MiB batches, a control-plane mutation
/// commits and propagates to ConfigSnapshot within 1 s.
#[doc = "Spec: REL-4"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_control_mutation_propagates_within_1s_under_data_queue_saturation() {
    let (_dir, writer_pool, reader_pool, _initial_user_id) = setup_test_context().await;

    let initial_snapshot = load_initial_snapshot(&reader_pool)
        .await
        .expect("load initial snapshot");
    let shared_snapshot = Arc::new(ArcSwap::new(Arc::new(initial_snapshot)));
    let runtime_state = Arc::new(RuntimeState::new());

    let (reloader_tx, reloader_rx) = mpsc::channel(100);
    let (writer, _writer_task) = start_writer(writer_pool.clone(), reloader_tx.clone());
    let (reloader, _reloader_task) = start_reloader(
        reader_pool.clone(),
        shared_snapshot.clone(),
        runtime_state.clone(),
        reloader_rx,
        reloader_tx,
    );

    // Saturate the Data queue with 4 MiB of raw event bytes
    let item_size = 1024; // 1 KiB
    let item_str = "x".repeat(item_size);
    let items_to_push = 4096; // 4 MiB
    for _ in 0..items_to_push {
        writer.queues.push_raw(item_str.clone());
    }

    // Now execute a control-plane mutation while data queue is loaded
    let start = Instant::now();
    let new_v = writer
        .execute_control(
            vec![ConfigDomain::Settings],
            vec![CacheInvalidationEvent::StrategyChanged],
            UpdateSettingOp {
                key: "default_routing_strategy".to_string(),
                value_json: "\"weighted_rr\"".to_string(),
            },
        )
        .await
        .expect("execute control under saturation");

    let published_v = reloader
        .wait_for_version(new_v, Duration::from_secs(1))
        .await
        .expect("reloader propagation under saturation");

    let elapsed = start.elapsed();
    assert!(
        published_v >= new_v,
        "published {published_v} must be >= {new_v}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "mutation must propagate within 1 s under saturation (took {elapsed:?})"
    );

    let snap = reloader.snapshot();
    assert_eq!(
        snap.settings.default_routing_strategy,
        RoutingStrategy::WeightedRr
    );
}

/// Spec: REL-4
/// Unknown keys in settings are rejected with HTTP 422.
#[doc = "Spec: REL-4"]
#[tokio::test]
async fn test_unknown_settings_keys_rejected_with_422() {
    let (_dir, writer_pool, _reader_pool, _initial_user_id) = setup_test_context().await;

    // 1. Unknown key validation
    let err =
        validate_setting("unknown_fake_setting", "true").expect_err("unknown key must be rejected");
    assert_eq!(err.status_code(), 422);
    assert!(matches!(err, SettingsError::UnknownKey { .. }));

    // 2. Out-of-bounds valid key validation
    let err_val =
        validate_setting("pin_ttl_secs", "10").expect_err("out of bounds value must be rejected");
    assert_eq!(err_val.status_code(), 422);
    assert!(matches!(err_val, SettingsError::InvalidValue { .. }));

    // 3. update_setting rejected on unknown key
    let update_err = update_setting(&writer_pool, "unknown_key", "123", 1)
        .await
        .expect_err("update with unknown key must be rejected");
    assert_eq!(update_err.status_code(), 422);
}
