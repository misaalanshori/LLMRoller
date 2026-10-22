//! Pin invalidation on snapshot publication.
//!
//! Spec: Section 6.7, Section 10.4,
//! Requirements: PRV-8, CLK-4, SHR-5.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use llmroller_core::eligibility::CandidatePoolTier;
use llmroller_core::ids::{ClientKeyId, ProviderId, UpstreamKeyId, UserId};
use llmroller_core::pins::{PinEntry, PinKey, PinState, PinStore};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role, SystemSeal, SystemTask};
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::seams::{OsEntropy, SecretString};
use llmroller_core::snapshot::{CacheInvalidationEvent, ConfigDomain};
use llmroller_server::reloader::{load_initial_snapshot, start_reloader_with_pin_store};
use llmroller_store::client_keys::{
    ClientKeyPatch, NewClientKey, create_client_key, patch_client_key_tx,
};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, ProviderPatch, UpstreamKeyPatch, create_provider_tx,
    create_upstream_key_tx, delete_upstream_key_tx, patch_provider_tx, patch_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, create_user};
use llmroller_store::writer::{ControlOp, WriterError, start_writer};
use sqlx::{Sqlite, SqlitePool, Transaction};
use tempfile::tempdir;
use tokio::sync::mpsc;

const NOW_MS: i64 = 1_700_000_000_000;

struct PatchKeyOp {
    actor: ActorScope,
    key_id: UpstreamKeyId,
    patch: UpstreamKeyPatch,
}

impl ControlOp for PatchKeyOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WriterError>> + Send + 'a>>
    {
        Box::pin(async move {
            patch_upstream_key_tx(tx, &self.actor, self.key_id, self.patch)
                .await
                .map_err(|error| WriterError::Execution(error.to_string()))?;
            Ok(())
        })
    }
}

struct DeleteKeyOp {
    actor: ActorScope,
    key_id: UpstreamKeyId,
}

impl ControlOp for DeleteKeyOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WriterError>> + Send + 'a>>
    {
        Box::pin(async move {
            delete_upstream_key_tx(tx, &self.actor, self.key_id)
                .await
                .map_err(|error| WriterError::Execution(error.to_string()))?;
            Ok(())
        })
    }
}

struct PatchProviderOp {
    actor: ActorScope,
    provider_id: ProviderId,
    patch: ProviderPatch,
}

impl ControlOp for PatchProviderOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WriterError>> + Send + 'a>>
    {
        Box::pin(async move {
            patch_provider_tx(tx, &self.actor, self.provider_id, self.patch, NOW_MS)
                .await
                .map_err(|error| WriterError::Execution(error.to_string()))?;
            Ok(())
        })
    }
}

struct PatchClientKeyOp {
    actor: ActorScope,
    client_key_id: ClientKeyId,
    patch: ClientKeyPatch,
}

impl ControlOp for PatchClientKeyOp {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WriterError>> + Send + 'a>>
    {
        Box::pin(async move {
            patch_client_key_tx(tx, &self.actor, self.client_key_id, self.patch)
                .await
                .map_err(|error| WriterError::Execution(error.to_string()))?;
            Ok(())
        })
    }
}

struct NoopOp;

impl ControlOp for NoopOp {
    fn call<'a>(
        self: Box<Self>,
        _tx: &'a mut Transaction<'_, Sqlite>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), WriterError>> + Send + 'a>>
    {
        Box::pin(async move { Ok(()) })
    }
}

async fn seed_pin(store: &PinStore, client_key_id: ClientKeyId, upstream_key_id: UpstreamKeyId) {
    store.insert_hydrated(
        PinKey {
            client_key_id,
            canonical_id: "test-model".to_string(),
            session_hash: "session-hash".to_string(),
        },
        PinEntry {
            state: PinState::Confirmed,
            upstream_key_id,
            generation: 1,
            pool_tier: CandidatePoolTier::Own,
            created_ms: NOW_MS,
            last_used_ms: NOW_MS,
            expires_ms: NOW_MS + 86_400_000,
            repin_count: 0,
            tried_this_generation: Vec::new(),
        },
    );
}

async fn create_personal_key(
    pool: &SqlitePool,
    actor: &ActorScope,
    user_id: UserId,
    master_key: &MasterKey,
    entropy: Arc<OsEntropy>,
    label: &str,
) -> UpstreamKeyId {
    let mut tx = pool.begin().await.expect("begin provider tx");
    let provider = create_provider_tx(
        &mut tx,
        actor,
        NewProvider {
            name: format!("{label}-provider"),
            kind: ProviderKind::Custom,
            base_url: "https://api.example.com/v1".to_string(),
            config_json: "{}".to_string(),
            ownership_scope: OwnershipScope::Personal,
            owner_user_id: Some(user_id),
            usage_url_cross_origin_ack: false,
            allow_private_network: false,
            enabled: true,
        },
        NOW_MS,
    )
    .await
    .expect("create provider");
    let key = create_upstream_key_tx(
        &mut tx,
        actor,
        master_key,
        "key-0",
        entropy,
        NewUpstreamKey {
            provider_id: provider.id,
            label: format!("{label}-key"),
            secret: SecretString::new("sk-test-secret".to_string().into()),
            priority: None,
            strict_tier: None,
            enabled: Some(true),
        },
        NOW_MS,
    )
    .await
    .expect("create upstream key");
    tx.commit().await.expect("commit provider tx");
    key.id
}

#[doc = "Spec: PRV-8, CLK-4, SHR-5"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_pin_store_evicts_on_key_provider_client_key_and_share_invalidation() {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("pin_invalidation.db");
    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    let reader_pool = open_pool(&db_path, PoolRole::Reader)
        .await
        .expect("reader pool");
    migrate(&writer_pool).await.expect("migrate");

    let master_key = MasterKey::from_bytes([0x44; 32]);
    let entropy = Arc::new(OsEntropy);
    load_or_initialize(&writer_pool, "key-0", &master_key, entropy.clone(), NOW_MS)
        .await
        .expect("instance");
    seed_defaults(&writer_pool, None, NOW_MS)
        .await
        .expect("settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "admin_user",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        entropy.as_ref(),
        NOW_MS,
    )
    .await
    .expect("admin");
    let admin_actor = ActorScope::user(admin.id, Role::Admin);

    let key_id = create_personal_key(
        &writer_pool,
        &admin_actor,
        admin.id,
        &master_key,
        entropy.clone(),
        "primary",
    )
    .await;

    let created = create_client_key(
        &writer_pool,
        &admin_actor,
        admin.id,
        NewClientKey {
            label: "cli".to_string(),
            service_account_id: None,
            routing_scope: None,
            allowed_models: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: None,
            expires_at: None,
        },
        entropy.as_ref(),
        NOW_MS,
    )
    .await
    .expect("client key");
    let client_key_id = created.client_key.id;

    let initial_snapshot = load_initial_snapshot(&reader_pool)
        .await
        .expect("initial snapshot");
    let shared_snapshot = Arc::new(ArcSwap::new(Arc::new(initial_snapshot)));
    let runtime_state = Arc::new(RuntimeState::new());
    let pin_store = Arc::new(PinStore::new(10_000));

    let (reloader_tx, reloader_rx) = mpsc::channel(64);
    let (writer, _writer_task) = start_writer(writer_pool.clone(), reloader_tx.clone());
    let (reloader, _reloader_task) = start_reloader_with_pin_store(
        reader_pool.clone(),
        shared_snapshot.clone(),
        runtime_state,
        pin_store.clone(),
        reloader_rx,
        reloader_tx,
    );

    // 1. Disabling an upstream key evicts its pins.
    seed_pin(&pin_store, client_key_id, key_id).await;
    assert_eq!(pin_store.len(), 1);
    let version = writer
        .execute_control(
            vec![ConfigDomain::UpstreamKeys],
            vec![CacheInvalidationEvent::UpstreamKeyChanged(key_id)],
            PatchKeyOp {
                actor: admin_actor.clone(),
                key_id,
                patch: UpstreamKeyPatch {
                    enabled: Some(false),
                    ..UpstreamKeyPatch::default()
                },
            },
        )
        .await
        .expect("disable key");
    reloader
        .wait_for_version(version, Duration::from_secs(5))
        .await
        .expect("published disable");
    assert!(!shared_snapshot.load().upstream_keys[&key_id].enabled);
    assert_eq!(pin_store.len(), 0, "disabled key pin survived publication");

    // 2. Deleting an upstream key evicts its pins.
    writer
        .execute_control(
            vec![ConfigDomain::UpstreamKeys],
            vec![CacheInvalidationEvent::UpstreamKeyChanged(key_id)],
            PatchKeyOp {
                actor: admin_actor.clone(),
                key_id,
                patch: UpstreamKeyPatch {
                    enabled: Some(true),
                    ..UpstreamKeyPatch::default()
                },
            },
        )
        .await
        .expect("re-enable key");
    seed_pin(&pin_store, client_key_id, key_id).await;
    assert_eq!(pin_store.len(), 1);
    let version = writer
        .execute_control(
            vec![ConfigDomain::UpstreamKeys],
            vec![CacheInvalidationEvent::UpstreamKeyChanged(key_id)],
            DeleteKeyOp {
                actor: admin_actor.clone(),
                key_id,
            },
        )
        .await
        .expect("delete key");
    reloader
        .wait_for_version(version, Duration::from_secs(5))
        .await
        .expect("published delete");
    assert!(!shared_snapshot.load().upstream_keys.contains_key(&key_id));
    assert_eq!(pin_store.len(), 0, "deleted key pin survived publication");

    // 3. Disabling the owning provider evicts pins to all of its keys.
    let second_key_id = create_personal_key(
        &writer_pool,
        &admin_actor,
        admin.id,
        &master_key,
        entropy.clone(),
        "secondary",
    )
    .await;
    let second_key_provider = {
        // Reload so the newly created key/provider exists in the snapshot.
        let version = writer
            .execute_control(
                vec![ConfigDomain::Providers, ConfigDomain::UpstreamKeys],
                Vec::new(),
                NoopOp,
            )
            .await
            .expect("reload second provider");
        reloader
            .wait_for_version(version, Duration::from_secs(5))
            .await
            .expect("published second provider");
        shared_snapshot.load().upstream_keys[&second_key_id].provider_id
    };
    seed_pin(&pin_store, client_key_id, second_key_id).await;
    assert_eq!(pin_store.len(), 1);
    let version = writer
        .execute_control(
            vec![ConfigDomain::Providers],
            vec![CacheInvalidationEvent::ProviderChanged(second_key_provider)],
            PatchProviderOp {
                actor: admin_actor.clone(),
                provider_id: second_key_provider,
                patch: ProviderPatch {
                    enabled: Some(false),
                    ..ProviderPatch::default()
                },
            },
        )
        .await
        .expect("disable provider");
    reloader
        .wait_for_version(version, Duration::from_secs(5))
        .await
        .expect("published provider disable");
    assert!(!shared_snapshot.load().providers[&second_key_provider].enabled);
    assert_eq!(
        pin_store.len(),
        0,
        "disabled provider pin survived publication"
    );

    // 4. Disabling a client key evicts every pin it owns.
    seed_pin(&pin_store, client_key_id, second_key_id).await;
    assert_eq!(pin_store.len(), 1);
    let version = writer
        .execute_control(
            vec![ConfigDomain::ClientKeys],
            vec![CacheInvalidationEvent::ClientKeyChanged(client_key_id)],
            PatchClientKeyOp {
                actor: admin_actor.clone(),
                client_key_id,
                patch: ClientKeyPatch {
                    disabled: Some(true),
                    ..ClientKeyPatch::default()
                },
            },
        )
        .await
        .expect("disable client key");
    reloader
        .wait_for_version(version, Duration::from_secs(5))
        .await
        .expect("published client key disable");
    assert_eq!(
        pin_store.len(),
        0,
        "disabled client key pin survived publication"
    );

    // 5. A share change evicts every pin owned by the affected grantee.
    let other = create_client_key(
        &writer_pool,
        &admin_actor,
        admin.id,
        NewClientKey {
            label: "other".to_string(),
            service_account_id: None,
            routing_scope: None,
            allowed_models: None,
            rate_limit_rpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: None,
            expires_at: None,
        },
        entropy.as_ref(),
        NOW_MS,
    )
    .await
    .expect("second client key");
    let other_client_key_id = other.client_key.id;
    let version = writer
        .execute_control(vec![ConfigDomain::ClientKeys], Vec::new(), NoopOp)
        .await
        .expect("reload client keys");
    reloader
        .wait_for_version(version, Duration::from_secs(5))
        .await
        .expect("published client key reload");
    seed_pin(&pin_store, other_client_key_id, second_key_id).await;
    assert_eq!(pin_store.len(), 1);
    let version = writer
        .execute_control(
            Vec::new(),
            vec![CacheInvalidationEvent::ShareChanged(admin.id)],
            NoopOp,
        )
        .await
        .expect("share invalidation");
    reloader
        .wait_for_version(version, Duration::from_secs(5))
        .await
        .expect("published share change");
    assert_eq!(pin_store.len(), 0, "grantee pin survived ShareChanged");
}
