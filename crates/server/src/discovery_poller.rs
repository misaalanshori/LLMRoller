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
    DiscoveryTarget, MAX_DISCOVERY_BYTES, discovery_endpoint, discovery_headers, parse_models_body,
};
use llmroller_store::crypto::{MasterKey, SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_store::models::{is_valid_upstream_model_id, upsert_discovered_models_tx};
use llmroller_store::writer::{ControlOp, DurableKey, DurableOp, WriterError, WriterHandle};
use sqlx::{Row as _, Sqlite, SqlitePool, Transaction};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

/// Scheduled full-catalog refresh cadence required by `MDL-1`.
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Per-key upstream discovery request timeout.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of concurrent discovery requests.
const MAX_CONCURRENT_DISCOVERIES: usize = 4;

/// Startup dependencies for the `/models` discovery supervisor.
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
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(async move {
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

        loop {
            let target = tokio::select! {
                _ = ticker.tick() => DiscoveryTarget::AllEnabled,
                Some(target) = rx.recv() => target,
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

    let models = parse_models_body(&body).map_err(|error| error.to_string())?;
    let invalid = models
        .iter()
        .filter(|model| !is_valid_upstream_model_id(model))
        .count();
    if invalid > 0 {
        tracing::warn!(key = %key.id, skipped = invalid, "skipping invalid upstream model IDs");
    }

    let op = PersistDiscovery {
        key_id: key.id,
        is_global: key.is_global,
        owner_user_id: key.owner_user_id,
        now_ms: llmroller_core::seams::SystemClock::new().now_ms(),
        models,
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
    // between discovery passes (`API-8`). The in-memory clear is paired with a
    // durable delete so restart cannot resurrect a superseded block.
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
    response_bytes_len: usize,
}

impl ControlOp for PersistDiscovery {
    fn call<'a>(
        self: Box<Self>,
        tx: &'a mut Transaction<'_, Sqlite>,
    ) -> Pin<Box<dyn Future<Output = Result<(), WriterError>> + Send + 'a>> {
        Box::pin(async move {
            let result = upsert_discovered_models_tx(
                tx,
                &self.key_id,
                self.is_global,
                self.owner_user_id.as_ref(),
                self.now_ms,
                &self.models,
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
