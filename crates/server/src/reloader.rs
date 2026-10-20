#![forbid(unsafe_code)]
//! Versioned ConfigSnapshot reloader task.
//!
//! Spec: ./docs/srdd/5. SRDD - System Architecture.md#54-hot-path-state-reloader-ordering-and-listener-isolation
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#72-reliability-and-availability-rel
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#2162-versioned-configsnapshot-publication-reloader
//! Phase: ./docs/bigbang/big-bang-phases-1.md#16-sub-phase-16-dual-priority-sqlite-writer-closed-settings-registry-reloader-task-and-runtimestate

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::snapshot::{ConfigDomain, ConfigSnapshot};
use llmroller_store::client_keys::{ClientKeyError, load_client_key_records_tx};
use llmroller_store::providers::{
    ProviderError, load_provider_records_tx, load_upstream_key_records_tx,
};
use llmroller_store::settings::{SettingsError, load_settings_tx};
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

    let users = load_user_records_tx(&mut tx).await?;
    let settings = load_settings_tx(&mut tx).await?;
    let providers = load_provider_records_tx(&mut tx).await?;
    let upstream_keys = load_upstream_key_records_tx(&mut tx).await?;
    let client_keys = load_client_key_records_tx(&mut tx, now_ms).await?;
    tx.commit().await?;

    Ok(ConfigSnapshot {
        config_version: v,
        users: Arc::new(users),
        settings: Arc::new(settings),
        providers: Arc::new(providers),
        upstream_keys: Arc::new(upstream_keys),
        client_keys_by_hash: Arc::new(client_keys),
    })
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

            let current = shared_snapshot.load();

            // 4. If v > published.version: rebuild only changed domain Arc slices
            if db_version > current.config_version {
                let reload_all = changed_domains.is_empty();

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

                if let Err(err) = tx.commit().await {
                    tracing::error!("reloader failed to commit read transaction: {err}");
                    continue;
                }

                let new_snapshot = ConfigSnapshot {
                    config_version: db_version,
                    users: next_users,
                    settings: next_settings,
                    providers: next_providers,
                    upstream_keys: next_upstream_keys,
                    client_keys_by_hash: next_client_keys,
                };

                // Store new snapshot atomically
                shared_snapshot.store(Arc::new(new_snapshot));

                // Apply CacheInvalidationEvents after arc_swap.store per REL-4
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64);
                runtime_state.apply_invalidations(&invalidations, now_ms);

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
