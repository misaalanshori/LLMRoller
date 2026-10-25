#![forbid(unsafe_code)]
//! Background and on-demand upstream `/models` discovery worker.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#66-model-discovery-catalog-and-merging-mdl
//! Spec: ./docs/srdd/7. SRDD - Non-Functional Requirements.md#74-portability-maintainability-observability-and-privacy-por-mnt-obs-priv

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use llmroller_core::ids::{UpstreamKeyId, UserId};
use llmroller_core::provider::{ProviderConfig, ProviderKind};
use llmroller_core::runtime_state::RuntimeState;
use llmroller_core::seams::{Clock as _, ExposeSecret as _};
use llmroller_proto::discovery::{
    DiscoveryTarget, MAX_DISCOVERY_BYTES, discovery_endpoint, discovery_headers,
};
use llmroller_store::crypto::{MasterKey, SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_store::models::is_valid_upstream_model_id;
use llmroller_store::writer::{ControlOp, DurableKey, DurableOp, WriterError, WriterHandle};
use sqlx::{Row as _, Sqlite, SqlitePool, Transaction};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use crate::shutdown::JobSupervisor;

/// Scheduled full-catalog refresh cadence required by `MDL-1`.
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Auto-recovery probe cadence (`15 min`) for upstream keys marked `Invalid` (`REL-5`, `API-8`, `F-103`).
const INVALID_KEY_RECOVERY_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Per-key upstream discovery request timeout.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of concurrent discovery requests.
const MAX_CONCURRENT_DISCOVERIES: usize = 4;

/// Startup dependencies for the `/models` discovery supervisor.
#[derive(Clone)]
pub struct DiscoveryPollerDeps {
    pub pool: SqlitePool,
    pub writer: WriterHandle,
    pub runtime_state: Arc<RuntimeState>,
    pub master_key: MasterKey,
    pub instance_id: [u8; 16],
}

impl std::fmt::Debug for DiscoveryPollerDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DiscoveryPollerDeps")
            .field("pool", &self.pool)
            .field("writer", &self.writer)
            .field("runtime_state", &self.runtime_state)
            .field("master_key", &"<redacted>")
            .field("instance_id", &self.instance_id)
            .finish()
    }
}

#[derive(Debug, Clone)]
struct DiscoveryKey {
    id: UpstreamKeyId,
    secret_enc: Vec<u8>,
    master_key_id: String,
    provider_id: String,
    owner_user_id: Option<UserId>,
    kind: ProviderKind,
    base_url: String,
    config: ProviderConfig,
    is_global: bool,
    allow_private_network: bool,
}

/// Spawn the discovery supervisor and return the trigger sender.
pub fn start_discovery_poller(
    deps: DiscoveryPollerDeps,
) -> (
    mpsc::UnboundedSender<DiscoveryTarget>,
    tokio::task::JoinHandle<()>,
) {
    let (stop_tx, stop_rx) = watch::channel(false);
    start_discovery_poller_with_stop(deps, stop_rx, Some(stop_tx))
}

/// Spawn the discovery supervisor bound to an external shutdown `watch::Receiver<bool>` (`F052`, `F055`).
pub fn start_discovery_poller_with_stop(
    deps: DiscoveryPollerDeps,
    stop_rx: watch::Receiver<bool>,
    keep_alive_tx: Option<watch::Sender<bool>>,
) -> (
    mpsc::UnboundedSender<DiscoveryTarget>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let initial_tx = tx.clone();
    let rx_arc = Arc::new(tokio::sync::Mutex::new(rx));
    let supervisor = JobSupervisor::new("discovery");

    let handle = tokio::spawn(async move {
        let _keep_alive = keep_alive_tx;
        // Trigger initial discovery pass for unpopulated keys shortly after boot (MDL-1)
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = initial_tx.send(DiscoveryTarget::AllEnabled);
        });

        supervisor
            .run_supervised(stop_rx, move |mut job_stop| {
                let rx_arc = rx_arc.clone();
                let deps = deps.clone();
                async move {
                    let sealer = Arc::new(XChaCha20Poly1305Sealer::new(
                        deps.master_key.clone(),
                        deps.instance_id,
                        Arc::new(llmroller_core::seams::OsEntropy),
                    ));
                    let in_flight: Arc<DashMap<UpstreamKeyId, ()>> = Arc::new(DashMap::new());

                    // The first tick completes immediately; consume it so startup does not
                    // hammer every upstream before the server is ready.
                    let mut ticker = tokio::time::interval(DISCOVERY_INTERVAL);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    ticker.tick().await;

                    let mut recovery_ticker = tokio::time::interval(INVALID_KEY_RECOVERY_INTERVAL);
                    recovery_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    recovery_ticker.tick().await;

                    let mut rx_guard = rx_arc.lock().await;
                    loop {
                        let target = tokio::select! {
                            _ = job_stop.changed() => break,
                            _ = ticker.tick() => DiscoveryTarget::AllEnabled,
                            _ = recovery_ticker.tick() => DiscoveryTarget::AllEnabled,
                            Some(target) = rx_guard.recv() => target,
                            else => break,
                        };

                        let keys = match load_discovery_keys(&deps.pool, target).await {
                            Ok(keys) => keys,
                            Err(error) => {
                                tracing::warn!(?target, "cannot load upstream keys for discovery: {error}");
                                continue;
                            }
                        };

                        let mut tasks = JoinSet::new();
                        for key in keys {
                            if in_flight.insert(key.id, ()).is_some() {
                                continue;
                            }
                            if tasks.len() >= MAX_CONCURRENT_DISCOVERIES {
                                let _ = tasks.join_next().await;
                            }
                            let writer = deps.writer.clone();
                            let runtime_state = deps.runtime_state.clone();
                            let sealer = sealer.clone();
                            let in_flight = in_flight.clone();
                            let key_id = key.id;
                            tasks.spawn(async move {
                                if let Err(error) = discover_key(&writer, &runtime_state, &sealer, &key).await {
                                    tracing::warn!(key = %key.id, "model discovery failed: {error}");
                                }
                                in_flight.remove(&key_id);
                            });
                        }

                        while tasks.join_next().await.is_some() {}
                    }
                }
            })
            .await;
    });

    (tx, handle)
}

async fn load_discovery_keys(
    pool: &SqlitePool,
    target: DiscoveryTarget,
) -> Result<Vec<DiscoveryKey>, sqlx::Error> {
    let rows = match target {
        DiscoveryTarget::Key(key_id) => sqlx::query(
            "SELECT k.id AS key_id, k.secret_enc, k.master_key_id, k.provider_id, k.owner_user_id,
                    p.kind, p.base_url, p.config_json, p.ownership_scope,
                    p.owner_user_id AS provider_owner_user_id, p.allow_private_network
             FROM upstream_keys k
             JOIN providers p ON p.id = k.provider_id
             WHERE k.id = ? AND k.enabled = 1 AND p.enabled = 1",
        )
        .bind(key_id.to_string())
        .fetch_all(pool)
        .await?,
        DiscoveryTarget::AllEnabled => sqlx::query(
            "SELECT k.id AS key_id, k.secret_enc, k.master_key_id, k.provider_id, k.owner_user_id,
                    p.kind, p.base_url, p.config_json, p.ownership_scope,
                    p.owner_user_id AS provider_owner_user_id, p.allow_private_network
             FROM upstream_keys k
             JOIN providers p ON p.id = k.provider_id
             WHERE k.enabled = 1 AND p.enabled = 1",
        )
        .fetch_all(pool)
        .await?,
    };

    Ok(rows.iter().filter_map(discovery_key_from_row).collect())
}

fn discovery_key_from_row(row: &sqlx::sqlite::SqliteRow) -> Option<DiscoveryKey> {
    let id = UpstreamKeyId::parse(&row.try_get::<String, _>("key_id").ok()?).ok()?;
    let kind = row
        .try_get::<String, _>("kind")
        .ok()?
        .parse::<ProviderKind>()
        .ok()?;
    let config_json: String = row.try_get("config_json").ok()?;
    let owner_user_id = row
        .try_get::<Option<String>, _>("owner_user_id")
        .ok()
        .flatten()
        .or_else(|| {
            row.try_get::<Option<String>, _>("provider_owner_user_id")
                .ok()
                .flatten()
        })
        .and_then(|value| UserId::parse(&value).ok());
    Some(DiscoveryKey {
        id,
        secret_enc: row.try_get("secret_enc").ok()?,
        master_key_id: row.try_get("master_key_id").ok()?,
        provider_id: row.try_get("provider_id").ok()?,
        owner_user_id,
        kind,
        base_url: row.try_get("base_url").ok()?,
        config: ProviderConfig::from_json(&config_json),
        is_global: row.try_get::<String, _>("ownership_scope").ok()? == "global",
        allow_private_network: row.try_get::<i64, _>("allow_private_network").ok()? != 0,
    })
}

async fn discover_key(
    writer: &WriterHandle,
    runtime_state: &RuntimeState,
    sealer: &XChaCha20Poly1305Sealer,
    key: &DiscoveryKey,
) -> Result<(), String> {
    // The AEAD associated data includes the provider id.
    let provider_id = &key.provider_id;
    let secret = sealer
        .open(
            "upstream_key",
            &[
                key.master_key_id.as_bytes(),
                key.id.to_string().as_bytes(),
                provider_id.as_bytes(),
            ],
            &key.secret_enc,
        )
        .map_err(|_| "cannot unseal upstream key".to_string())?;
    let secret = String::from_utf8_lossy(secret.expose_secret()).to_string();

    let url = discovery_endpoint(key.kind, &key.base_url, &key.config);
    llmroller_core::ssrf::validate_url(&url, key.allow_private_network)
        .map_err(|_| "discovery URL is not allowed".to_string())?;
    let client = llmroller_proxy::ssrf::create_guarded_client(key.allow_private_network)
        .map_err(|_| "cannot build guarded HTTP client".to_string())?;

    let mut response = client
        .get(&url)
        .headers(discovery_headers(&key.config, &secret))
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|error| error.to_string())?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("upstream /models returned HTTP {status}"));
    }

    let mut body = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if body.len().saturating_add(chunk.len()) > MAX_DISCOVERY_BYTES {
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    if truncated {
        tracing::warn!(
            key = %key.id,
            "upstream /models exceeded {MAX_DISCOVERY_BYTES} bytes; keeping the catalog unchanged"
        );
        return Ok(());
    }

    let discovered = llmroller_proto::discovery::parse_models_response(&body)
        .map_err(|error| error.to_string())?;
    let invalid = discovered
        .iter()
        .filter(|model| !is_valid_upstream_model_id(&model.id))
        .count();
    if invalid > 0 {
        tracing::warn!(key = %key.id, skipped = invalid, "skipping invalid upstream model IDs");
    }
    let mut context_windows = std::collections::HashMap::new();
    let mut model_protocols = std::collections::HashMap::new();
    let models: Vec<String> = discovered
        .into_iter()
        .map(|model| {
            if let Some(context_window) = model.context_window {
                context_windows.insert(model.id.clone(), context_window);
            }
            if let Some(protos) = model.supported_protocols {
                model_protocols.insert(model.id.clone(), protos);
            }
            model.id
        })
        .collect();

    let op = PersistDiscovery {
        key_id: key.id,
        is_global: key.is_global,
        owner_user_id: key.owner_user_id,
        now_ms: llmroller_core::seams::SystemClock::new().now_ms(),
        models,
        context_windows,
        model_protocols,
        response_bytes_len: body.len(),
    };
    writer
        .execute_control(
            vec![llmroller_core::snapshot::ConfigDomain::Models],
            Vec::new(),
            op,
        )
        .await
        .map_err(|error| error.to_string())?;

    // A successful refresh supersedes any PlanRestricted route blocks written
    // between discovery passes (`API-8`) and recovers keys previously marked
    // `Invalid` (`REL-5`, `F-103`).
    recover_invalid_key_after_models_probe(runtime_state, Some(writer), key.id);
    runtime_state.clear_route_protocol_blocks(key.id);
    writer.queues.durable_state.lock().insert(
        DurableKey::RouteProtocolClear(key.id.to_string()),
        DurableOp::RouteProtocolBlockClear {
            upstream_key_id: key.id.to_string(),
        },
    );
    Ok(())
}

struct PersistDiscovery {
    key_id: UpstreamKeyId,
    is_global: bool,
    owner_user_id: Option<UserId>,
    now_ms: i64,
    models: Vec<String>,
    context_windows: std::collections::HashMap<String, i64>,
    model_protocols: std::collections::HashMap<String, Vec<String>>,
    response_bytes_len: usize,
}

impl ControlOp for PersistDiscovery {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), WriterError>> + Send + 'a>> {
        Box::pin(async move {
            let result = llmroller_store::models::upsert_discovered_models_with_metadata_tx(
                tx,
                &self.key_id,
                self.is_global,
                self.owner_user_id.as_ref(),
                self.now_ms,
                &self.models,
                &self.context_windows,
                &self.model_protocols,
                self.response_bytes_len,
            )
            .await?;
            tracing::info!(
                key = %self.key_id,
                discovered = result.total_discovered,
                updated = result.routes_added_or_updated,
                missing = result.routes_marked_missing,
                retired = result.routes_retired,
                partial = result.is_partial,
                "upstream model discovery completed"
            );
            Ok(())
        })
    }
}

/// Transitions an `Invalid` / quarantined upstream key back to `Healthy` when a `GET /models`
/// probe succeeds with HTTP 2xx, bumping `health_version` and enqueueing a durable health update (`REL-5`, `API-8`, `F-103`).
pub fn recover_invalid_key_after_models_probe(
    runtime_state: &RuntimeState,
    writer: Option<&WriterHandle>,
    key_id: UpstreamKeyId,
) -> bool {
    let was_unhealthy = runtime_state
        .key_health
        .get(&key_id)
        .is_some_and(|entry| !entry.healthy || entry.quarantine_reason.is_some());
    if !was_unhealthy {
        return false;
    }
    runtime_state.key_health.insert(
        key_id,
        llmroller_core::runtime_state::KeyHealthEntry::default(),
    );
    let version = i64::try_from(runtime_state.health.bump_version()).unwrap_or(i64::MAX);
    if let Some(writer) = writer {
        writer.queues.durable_state.lock().insert(
            DurableKey::HealthVersion(key_id.to_string()),
            DurableOp::HealthVersionUpdate {
                upstream_key_id: key_id.to_string(),
                health: "healthy".to_string(),
                health_until: None,
                health_version: version,
                last_error: None,
            },
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmroller_core::provider::{ProtocolAuthConfig, ProviderAuthScheme};

    #[test]
    #[doc = "Spec: MDL-1, PRV-1, PRV-3"]
    fn discovery_poller_uses_configured_models_path_and_auth_header_mode() {
        let mut cfg = ProviderConfig::default();
        cfg.paths.models = Some("/custom/v2/models".to_string());
        cfg.auth.models = Some(ProtocolAuthConfig::Explicit(ProviderAuthScheme {
            header: "x-api-key".to_string(),
            prefix: String::new(),
        }));
        let url = discovery_endpoint(ProviderKind::Custom, "https://api.example.com/", &cfg);
        assert_eq!(url, "https://api.example.com/custom/v2/models");
        let headers = discovery_headers(&cfg, "secret-token");
        assert_eq!(
            headers.get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("secret-token")
        );
    }

    #[test]
    #[doc = "Spec: REL-5, API-8"]
    fn successful_models_probe_recovers_invalid_key_to_healthy() {
        let runtime = RuntimeState::new();
        let key_id = UpstreamKeyId::generate();
        runtime.quarantine_key(key_id, "credential_rejected");
        assert!(!runtime.is_key_routable(key_id, 1_000));
        assert!(recover_invalid_key_after_models_probe(
            &runtime, None, key_id
        ));
        assert!(runtime.is_key_routable(key_id, 1_000));
    }
}
