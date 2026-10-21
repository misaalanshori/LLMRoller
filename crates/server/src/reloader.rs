#![forbid(unsafe_code)]
//! Versioned ConfigSnapshot reloader task.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#72-reliability-and-availability-rel
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#2162-versioned-configsnapshot-publication-reloader
//! Phase: ./docs/bigbang/big-bang-phases-1.md#16-sub-phase-16-dual-priority-sqlite-writer-closed-settings-registry-reloader-task-and-runtimestate

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::snapshot::{ConfigDomain, ConfigSnapshot};
use llmroller_store::budgets::load_budget_windows_tx;
use llmroller_store::client_keys::{ClientKeyError, load_client_key_records_tx};
use llmroller_store::models::{CatalogSnapshot, load_catalog_snapshot_tx};
use llmroller_store::prices::load_price_entries_tx;
use llmroller_store::providers::{
    ProviderError, load_provider_records_tx, load_upstream_key_records_tx,
};
use llmroller_store::settings::{SettingsError, load_settings_tx};
use llmroller_store::shares::load_share_configs_tx;
use llmroller_store::users::{UserError, load_user_records_tx};
use llmroller_store::writer::ReloadNotification;
use sqlx::SqlitePool;
use thiserror::Error;
use tokio::sync::{mpsc, watch};

/// Errors encountered by the reloader supervisor or callers awaiting publication.
#[derive(Debug, Error)]
pub enum ReloaderError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    User(#[from] UserError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    ClientKey(#[from] ClientKeyError),
    #[error("reloader version notification timed out")]
    Timeout,
    #[error("reloader channel closed")]
    ChannelClosed,
    #[error("reloader error: {0}")]
    Other(String),
}

/// Handle held by control plane handlers and supervisors to query or await published snapshots.
#[derive(Clone, Debug)]
pub struct ReloaderHandle {
    shared_snapshot: Arc<ArcSwap<ConfigSnapshot>>,
    reloader_tx: mpsc::Sender<ReloadNotification>,
    published_version_rx: watch::Receiver<i64>,
}

impl ReloaderHandle {
    /// Load an `Arc` clone of the currently published immutable snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Arc<ConfigSnapshot> {
        self.shared_snapshot.load_full()
    }

    /// Access the underlying shared `ArcSwap` instance.
    #[must_use]
    pub fn shared_snapshot(&self) -> Arc<ArcSwap<ConfigSnapshot>> {
        self.shared_snapshot.clone()
    }

    /// Clone the reloader notification sender for control-plane publication.
    #[must_use]
    pub fn notification_sender(&self) -> mpsc::Sender<ReloadNotification> {
        self.reloader_tx.clone()
    }

    /// Clone the published-version watch receiver.
    #[must_use]
    pub fn version_receiver(&self) -> watch::Receiver<i64> {
        self.published_version_rx.clone()
    }

    /// Request a reload notification directly.
    pub async fn request_reload(
        &self,
        notification: ReloadNotification,
    ) -> Result<(), ReloaderError> {
        self.reloader_tx
            .send(notification)
            .await
            .map_err(|_| ReloaderError::ChannelClosed)
    }

    /// Await until the reloader publishes a configuration snapshot with `config_version >= target_version`.
    ///
    /// Per `REL-4` and SRDD Section 21.6.2, control handlers await this notification
    /// (with up to 1 s timeout) before returning HTTP 2xx to ensure revocations are active.
    pub async fn wait_for_version(
        &self,
        target_version: i64,
        timeout: Duration,
    ) -> Result<i64, ReloaderError> {
        let mut rx = self.published_version_rx.clone();
        if *rx.borrow() >= target_version {
            return Ok(*rx.borrow());
        }

        tokio::time::timeout(timeout, async {
            while rx.changed().await.is_ok() {
                let v = *rx.borrow();
                if v >= target_version {
                    return Ok(v);
                }
            }
            Err(ReloaderError::ChannelClosed)
        })
        .await
        .map_err(|_| ReloaderError::Timeout)?
    }
}

/// Budget/price/catalog slices that are rebuilt together whenever any of their domains change.
#[derive(Debug, Clone)]
struct DomainSlices {
    catalog: CatalogSnapshot,
    shares: HashMap<llmroller_core::ids::UserId, Vec<llmroller_core::shares::ShareConfig>>,
    prices: HashMap<llmroller_core::snapshot::PriceKey, llmroller_core::pricing::PriceEntry>,
    budget_windows: HashMap<
        llmroller_core::ids::UpstreamKeyId,
        Vec<llmroller_core::snapshot::SnapshotBudgetWindow>,
    >,
}

async fn load_domain_slices(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<DomainSlices, ReloaderError> {
    let catalog = load_catalog_snapshot_tx(&mut *tx)
        .await
        .map_err(ReloaderError::Sqlx)?;
    let shares = load_share_configs_tx(&mut *tx)
        .await
        .map_err(ReloaderError::Sqlx)?;
    let prices = load_price_entries_tx(&mut *tx)
        .await
        .map_err(ReloaderError::Sqlx)?;
    let budget_windows = load_budget_windows_tx(&mut *tx)
        .await
        .map_err(ReloaderError::Sqlx)?;
    Ok(DomainSlices {
        catalog,
        shares,
        prices,
        budget_windows,
    })
}

#[derive(Debug, Clone)]
struct DomainArcs {
    models: Arc<HashMap<llmroller_core::ids::ModelPk, llmroller_core::snapshot::SnapshotModel>>,
    models_by_canonical: Arc<HashMap<String, llmroller_core::ids::ModelPk>>,
    aliases: Arc<HashMap<String, llmroller_core::ids::ModelPk>>,
    redirects: Arc<HashMap<String, llmroller_core::ids::ModelPk>>,
    routes_by_model: Arc<
        HashMap<llmroller_core::ids::ModelPk, Vec<llmroller_core::snapshot::SnapshotModelRoute>>,
    >,
    user_route_prefs: Arc<
        HashMap<
            (llmroller_core::ids::UserId, llmroller_core::ids::ModelPk),
            llmroller_core::snapshot::SnapshotRoutePreference,
        >,
    >,
    shares: Arc<HashMap<llmroller_core::ids::UserId, Vec<llmroller_core::shares::ShareConfig>>>,
    prices: Arc<HashMap<llmroller_core::snapshot::PriceKey, llmroller_core::pricing::PriceEntry>>,
    budget_windows: Arc<
        HashMap<
            llmroller_core::ids::UpstreamKeyId,
            Vec<llmroller_core::snapshot::SnapshotBudgetWindow>,
        >,
    >,
}

impl DomainArcs {
    fn from_slices(slices: DomainSlices) -> Self {
        Self {
            models: Arc::new(slices.catalog.models),
            models_by_canonical: Arc::new(slices.catalog.models_by_canonical),
            aliases: Arc::new(slices.catalog.aliases),
            redirects: Arc::new(slices.catalog.redirects),
            routes_by_model: Arc::new(slices.catalog.routes_by_model),
            user_route_prefs: Arc::new(slices.catalog.user_route_prefs),
            shares: Arc::new(slices.shares),
            prices: Arc::new(slices.prices),
            budget_windows: Arc::new(slices.budget_windows),
        }
    }

    fn clone_from(current: &ConfigSnapshot) -> Self {
        Self {
            models: current.models.clone(),
            models_by_canonical: current.models_by_canonical.clone(),
            aliases: current.aliases.clone(),
            redirects: current.redirects.clone(),
            routes_by_model: current.routes_by_model.clone(),
            user_route_prefs: current.user_route_prefs.clone(),
            shares: current.shares_by_grantee.clone(),
            prices: current.prices.clone(),
            budget_windows: current.budget_windows.clone(),
        }
    }
}

async fn load_instance_id(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> [u8; 16] {
    let bytes: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT instance_id FROM instance_meta WHERE singleton = 1")
            .fetch_optional(&mut **tx)
            .await
            .ok()
            .flatten();
    bytes.and_then(|b| b.try_into().ok()).unwrap_or([0_u8; 16])
}

/// Load the initial immutable [`ConfigSnapshot`] directly from SQLite during server startup.
pub async fn load_initial_snapshot(pool: &SqlitePool) -> Result<ConfigSnapshot, ReloaderError> {
    let mut tx = pool.begin().await?;
    let v: i64 = sqlx::query_scalar("SELECT config_version FROM instance_meta")
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(0);

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);

    let instance_id = load_instance_id(&mut tx).await;
    let users = load_user_records_tx(&mut tx).await?;
    let settings = load_settings_tx(&mut tx).await?;
    let providers = load_provider_records_tx(&mut tx).await?;
    let upstream_keys = load_upstream_key_records_tx(&mut tx).await?;
    let client_keys = load_client_key_records_tx(&mut tx, now_ms).await?;
    let slices = load_domain_slices(&mut tx).await?;
    tx.commit().await?;

    Ok(assemble_snapshot(
        v,
        instance_id,
        Arc::new(users),
        Arc::new(settings),
        Arc::new(providers),
        Arc::new(upstream_keys),
        Arc::new(client_keys),
        DomainArcs::from_slices(slices),
    ))
}

fn assemble_snapshot(
    version: i64,
    instance_id: [u8; 16],
    users: Arc<HashMap<llmroller_core::ids::UserId, llmroller_core::snapshot::UserRecord>>,
    settings: Arc<llmroller_core::settings::RuntimeSettings>,
    providers: Arc<
        HashMap<llmroller_core::ids::ProviderId, llmroller_core::snapshot::ProviderRecord>,
    >,
    upstream_keys: Arc<
        HashMap<llmroller_core::ids::UpstreamKeyId, llmroller_core::snapshot::UpstreamKeyRecord>,
    >,
    client_keys: Arc<
        HashMap<
            [u8; 32],
            (
                llmroller_core::client_key::ClientKeyRecord,
                llmroller_core::client_key::HashRole,
            ),
        >,
    >,
    slices: DomainArcs,
) -> ConfigSnapshot {
    ConfigSnapshot {
        config_version: version,
        instance_id,
        users,
        settings,
        providers,
        upstream_keys,
        client_keys_by_hash: client_keys,
        models: slices.models,
        models_by_canonical: slices.models_by_canonical,
        aliases: slices.aliases,
        redirects: slices.redirects,
        routes_by_model: slices.routes_by_model,
        shares_by_grantee: slices.shares,
        user_route_prefs: slices.user_route_prefs,
        prices: slices.prices,
        budget_windows: slices.budget_windows,
    }
}

/// Spawn the single versioned [`Reloader`] background actor per SRDD Section 21.6.2.
pub fn start_reloader(
    pool: SqlitePool,
    shared_snapshot: Arc<ArcSwap<ConfigSnapshot>>,
    runtime_state: Arc<RuntimeState>,
    mut reloader_rx: mpsc::Receiver<ReloadNotification>,
    reloader_tx: mpsc::Sender<ReloadNotification>,
) -> (ReloaderHandle, tokio::task::JoinHandle<()>) {
    let initial_version = shared_snapshot.load().config_version;
    let (published_version_tx, published_version_rx) = watch::channel(initial_version);

    let handle = ReloaderHandle {
        shared_snapshot: shared_snapshot.clone(),
        reloader_tx,
        published_version_rx,
    };

    let join_handle = tokio::spawn(async move {
        while let Some(first_notif) = reloader_rx.recv().await {
            // 1. Drain and coalesce all pending ReloadNotification messages
            let mut _min_version = first_notif.min_version;
            let mut changed_domains = first_notif.changed_domains;
            let mut invalidations = first_notif.invalidations;

            while let Ok(next) = reloader_rx.try_recv() {
                _min_version = _min_version.max(next.min_version);
                for domain in next.changed_domains {
                    if !changed_domains.contains(&domain) {
                        changed_domains.push(domain);
                    }
                }
                invalidations.extend(next.invalidations);
            }

            // 2. Open ONE read transaction
            let mut tx = match pool.begin().await {
                Ok(tx) => tx,
                Err(err) => {
                    tracing::error!("reloader failed to open read transaction: {err}");
                    continue;
                }
            };

            // 3. Read v = SELECT config_version FROM instance_meta
            let db_version: i64 =
                match sqlx::query_scalar("SELECT config_version FROM instance_meta")
                    .fetch_optional(&mut *tx)
                    .await
                {
                    Ok(Some(v)) => v,
                    Ok(None) => 0,
                    Err(err) => {
                        tracing::error!("reloader failed to query config_version: {err}");
                        continue;
                    }
                };

            let current = shared_snapshot.load_full();

            // 4. If v > published.version: rebuild only changed domain Arc slices
            if db_version > current.config_version {
                let reload_all = changed_domains.is_empty();
                let reload_catalog = reload_all
                    || changed_domains.iter().any(|d| {
                        matches!(
                            d,
                            ConfigDomain::Models
                                | ConfigDomain::Shares
                                | ConfigDomain::Prices
                                | ConfigDomain::BudgetWindows
                                | ConfigDomain::Traceability
                        )
                    });

                let next_users = if reload_all || changed_domains.contains(&ConfigDomain::Users) {
                    match load_user_records_tx(&mut tx).await {
                        Ok(users) => Arc::new(users),
                        Err(err) => {
                            tracing::error!("reloader failed to load users slice: {err}");
                            continue;
                        }
                    }
                } else {
                    current.users.clone()
                };

                let next_settings =
                    if reload_all || changed_domains.contains(&ConfigDomain::Settings) {
                        match load_settings_tx(&mut tx).await {
                            Ok(settings) => Arc::new(settings),
                            Err(err) => {
                                tracing::error!("reloader failed to load settings slice: {err}");
                                continue;
                            }
                        }
                    } else {
                        current.settings.clone()
                    };

                let next_providers =
                    if reload_all || changed_domains.contains(&ConfigDomain::Providers) {
                        match load_provider_records_tx(&mut tx).await {
                            Ok(providers) => Arc::new(providers),
                            Err(err) => {
                                tracing::error!("reloader failed to load providers slice: {err}");
                                continue;
                            }
                        }
                    } else {
                        current.providers.clone()
                    };

                let next_upstream_keys = if reload_all
                    || changed_domains.contains(&ConfigDomain::UpstreamKeys)
                {
                    match load_upstream_key_records_tx(&mut tx).await {
                        Ok(keys) => Arc::new(keys),
                        Err(err) => {
                            tracing::error!("reloader failed to load upstream_keys slice: {err}");
                            continue;
                        }
                    }
                } else {
                    current.upstream_keys.clone()
                };

                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64);

                let next_client_keys =
                    if reload_all || changed_domains.contains(&ConfigDomain::ClientKeys) {
                        match load_client_key_records_tx(&mut tx, now_ms).await {
                            Ok(keys) => Arc::new(keys),
                            Err(err) => {
                                tracing::error!("reloader failed to load client_keys slice: {err}");
                                continue;
                            }
                        }
                    } else {
                        current.client_keys_by_hash.clone()
                    };

                let next_slices = if reload_catalog {
                    match load_domain_slices(&mut tx).await {
                        Ok(slices) => DomainArcs::from_slices(slices),
                        Err(err) => {
                            tracing::error!("reloader failed to load catalog slices: {err}");
                            continue;
                        }
                    }
                } else {
                    DomainArcs::clone_from(&current)
                };

                let next_instance_id =
                    if reload_all || changed_domains.contains(&ConfigDomain::Providers) {
                        load_instance_id(&mut tx).await
                    } else {
                        current.instance_id
                    };

                if let Err(err) = tx.commit().await {
                    tracing::error!("reloader failed to commit read transaction: {err}");
                    continue;
                }

                let new_snapshot = assemble_snapshot(
                    db_version,
                    next_instance_id,
                    next_users,
                    next_settings,
                    next_providers,
                    next_upstream_keys,
                    next_client_keys,
                    next_slices,
                );

                // Store new snapshot atomically
                shared_snapshot.store(Arc::new(new_snapshot.clone()));

                // Apply CacheInvalidationEvents after arc_swap.store per REL-4
                runtime_state.apply_invalidations(&invalidations, &new_snapshot, now_ms);

                // Notify waiting control handlers
                let _ = published_version_tx.send_replace(db_version);
            } else {
                let _ = tx.commit().await;
                if *published_version_tx.borrow() < db_version {
                    let _ = published_version_tx.send_replace(db_version);
                }
            }
        }
    });

    (handle, join_handle)
}
