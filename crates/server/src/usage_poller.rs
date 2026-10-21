#![forbid(unsafe_code)]
//! Background upstream `/usage` poller and budget reconciliation supervisor.
//!
//! Spec: ./docs/srdd/2. SRDD - Supported Providers and Wire Protocols.md#24-upstream-usage-polling
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#67-usage-observability-and-upstream-passthrough-usg

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use llmroller_core::ids::UpstreamKeyId;
use llmroller_core::runtime_state::{KeyHealthEntry, RuntimeState};
use llmroller_core::seams::{Clock as _, ExposeSecret as _};
use llmroller_core::snapshot::{ConfigSnapshot, UpstreamKeyRecord};
use llmroller_core::windows::{BudgetCounter as _, UpstreamObservation};
use llmroller_proto::usage_poller::{KeyPollerState, UsagePoller, parse_raw_usage_to_sanitized};
use llmroller_store::crypto::{MasterKey, SecretSealer as _, XChaCha20Poly1305Sealer};
use llmroller_store::usage_snapshots::{NewUsageSnapshot, insert_usage_snapshot_tx};
use llmroller_store::writer::WriterHandle;
use sqlx::SqlitePool;

const TICK: Duration = Duration::from_secs(15);

/// Startup dependencies for the usage poller supervisor.
pub struct UsagePollerDeps {
    pub pool: SqlitePool,
    pub snapshot: Arc<ArcSwap<ConfigSnapshot>>,
    pub runtime_state: Arc<RuntimeState>,
    pub writer: WriterHandle,
    pub master_key: MasterKey,
    pub master_key_id: String,
    pub instance_id: [u8; 16],
}

impl std::fmt::Debug for UsagePollerDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UsagePollerDeps")
            .field("pool", &self.pool)
            .field("snapshot", &self.snapshot)
            .field("runtime_state", &self.runtime_state)
            .field("writer", &self.writer)
            .field("master_key", &"<redacted>")
            .field("master_key_id", &self.master_key_id)
            .field("instance_id", &self.instance_id)
            .finish()
    }
}

/// Spawn the supervised usage polling loop. Returns the join handle.
pub fn start_usage_poller(deps: UsagePollerDeps) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let key_states: Arc<DashMap<UpstreamKeyId, KeyPollerState>> = Arc::new(DashMap::new());
        let poller = UsagePoller::default();
        loop {
            tokio::time::sleep(TICK).await;
            let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
            let snapshot = deps.snapshot.load_full();
            for key in snapshot.upstream_keys.values() {
                let Some(provider) = snapshot.providers.get(&key.provider_id) else {
                    continue;
                };
                if !key.enabled || !provider.enabled {
                    continue;
                }
                let Some(endpoint) = usage_endpoint(&provider.config_json) else {
                    continue;
                };
                let state = key_states.entry(key.id).or_default();
                if !state.try_start_poll(now_ms, false) {
                    continue;
                }
                let Ok(permit) = poller.semaphore.clone().acquire_owned().await else {
                    state.finish_poll();
                    continue;
                };
                let key = key.clone();
                let provider = provider.clone();
                let budget_windows = snapshot
                    .budget_windows
                    .get(&key.id)
                    .cloned()
                    .unwrap_or_default();
                let pool = deps.pool.clone();
                let runtime_state = deps.runtime_state.clone();
                let writer = deps.writer.clone();
                let sealer = XChaCha20Poly1305Sealer::new(
                    deps.master_key.clone(),
                    deps.instance_id,
                    Arc::new(llmroller_core::seams::OsEntropy),
                );
                let master_key_id = deps.master_key_id.clone();
                let key_states = key_states.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = poll_key(
                        &pool,
                        &runtime_state,
                        &writer,
                        &sealer,
                        &master_key_id,
                        &key,
                        provider.kind,
                        &budget_windows,
                        &provider.base_url,
                        &endpoint,
                        provider.allow_private_network,
                    )
                    .await
                    {
                        tracing::warn!(key = %key.id, "usage poll failed: {error}");
                    }
                    if let Some(state) = key_states.get(&key.id) {
                        state.finish_poll();
                    }
                });
            }
        }
    })
}

fn usage_endpoint(config_json: &str) -> Option<String> {
    let config: serde_json::Value = serde_json::from_str(config_json).ok()?;
    config
        .get("usage_endpoint")
        .or_else(|| config.get("usage_path"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

#[allow(clippy::too_many_arguments)]
async fn poll_key(
    pool: &SqlitePool,
    runtime_state: &RuntimeState,
    writer: &WriterHandle,
    sealer: &XChaCha20Poly1305Sealer,
    master_key_id: &str,
    key: &UpstreamKeyRecord,
    kind: llmroller_core::provider::ProviderKind,
    budget_windows: &[llmroller_core::snapshot::SnapshotBudgetWindow],
    base_url: &str,
    endpoint: &str,
    allow_private_network: bool,
) -> Result<(), String> {
    let secret = sealer
        .open(
            "upstream_key",
            &[
                key.master_key_id.as_bytes(),
                key.id.to_string().as_bytes(),
                key.provider_id.to_string().as_bytes(),
            ],
            &key.secret_enc,
        )
        .map_err(|_| "cannot unseal upstream key".to_string())?;
    let secret = String::from_utf8_lossy(secret.expose_secret()).to_string();

    let target = format!("{}{}", base_url.trim_end_matches('/'), endpoint);
    llmroller_core::ssrf::validate_url(&target, allow_private_network)
        .map_err(|_| "usage endpoint is not allowed".to_string())?;
    let client = llmroller_proxy::ssrf::create_guarded_client(allow_private_network)
        .map_err(|_| "cannot build guarded HTTP client".to_string())?;
    let response = client
        .get(&target)
        .header("authorization", format!("Bearer {secret}"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let http_status = i64::from(response.status().as_u16());
    let raw_bytes = response
        .bytes()
        .await
        .map_err(|error| error.to_string())?
        .to_vec();
    let raw_json: serde_json::Value =
        serde_json::from_slice(&raw_bytes).map_err(|error| error.to_string())?;
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let mut raw_json = raw_json;
    if raw_json.get("now_ms").is_none()
        && let Some(object) = raw_json.as_object_mut()
    {
        object.insert("now_ms".to_string(), serde_json::json!(now_ms));
    }

    let Some(parsed) = parse_raw_usage_to_sanitized(kind, &raw_json) else {
        return Ok(());
    };
    let sanitized_json = serde_json::to_string(&parsed).map_err(|error| error.to_string())?;
    let windows_json = serde_json::to_string(&parsed.windows).map_err(|error| error.to_string())?;

    let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
    insert_usage_snapshot_tx(
        &mut tx,
        sealer,
        NewUsageSnapshot {
            key_id: &key.id,
            now_ms,
            http_status,
            plan_name: parsed.plan_name.as_deref(),
            windows_json: &windows_json,
            balance_micro_usd: parsed.balance_micro_usd,
            master_key_id,
            sanitized_json: &sanitized_json,
            raw_json_bytes: &raw_bytes,
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    tx.commit().await.map_err(|error| error.to_string())?;

    reconcile(
        runtime_state,
        key.id,
        budget_windows,
        &parsed.windows,
        now_ms,
    );
    let _ = writer;
    Ok(())
}

fn reconcile(
    runtime_state: &RuntimeState,
    key_id: UpstreamKeyId,
    budget_windows: &[llmroller_core::snapshot::SnapshotBudgetWindow],
    windows: &[llmroller_proto::usage_poller::SanitizedWindow],
    now_ms: i64,
) {
    for configured in budget_windows {
        let Some(remote) = windows
            .iter()
            .find(|window| kind_matches(&window.kind, &local_window_kind(configured)))
        else {
            continue;
        };
        let Some(counter) = runtime_state.window_counter(&configured.id) else {
            continue;
        };
        if remote.used_percent >= 100.0 {
            runtime_state.key_health.insert(
                key_id,
                KeyHealthEntry {
                    exhausted_until_ms: remote.resets_at,
                    ..KeyHealthEntry::default()
                },
            );
        }
        let used_micro_usd =
            (counter.limit_micro_usd() as f64 * remote.used_percent / 100.0).round() as i64;
        let local_at_observation = counter.effective_used(now_ms);
        *counter.observation.write() = Some(UpstreamObservation {
            used_micro_usd,
            observed_at_ms: now_ms,
            local_at_observation,
            resets_at_ms: remote.resets_at,
        });
    }
}

fn local_window_kind(window: &llmroller_core::snapshot::SnapshotBudgetWindow) -> String {
    match window.period_ms {
        None => "monthly".to_string(),
        Some(period) if period <= 6 * 3_600_000 => "rolling_5h".to_string(),
        Some(period) if period <= 8 * 86_400_000 => "weekly".to_string(),
        Some(_) => "monthly".to_string(),
    }
}

fn kind_matches(remote: &str, local: &str) -> bool {
    let remote = remote.to_ascii_lowercase();
    let local = local.to_ascii_lowercase();
    remote.contains(&local) || local.contains(&remote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmroller_core::snapshot::SnapshotBudgetWindow;
    use llmroller_core::windows::AtomicBudgetCounter;
    use std::sync::Arc;

    #[test]
    #[doc = "Spec: USG-4"]
    fn extracts_usage_endpoint_from_provider_config() {
        assert_eq!(
            usage_endpoint(r#"{"usage_endpoint":"/v1/usage"}"#).as_deref(),
            Some("/v1/usage")
        );
        assert_eq!(usage_endpoint(r#"{"usage_path":""}"#), None);
        assert_eq!(usage_endpoint("{}"), None);
    }

    #[test]
    #[doc = "Spec: USG-6"]
    fn reconciles_remote_usage_into_window_observation_and_health() {
        let runtime = RuntimeState::new();
        let key_id = UpstreamKeyId::generate();
        let counter = Arc::new(AtomicBudgetCounter::new(1, 10_000_000, 0, 18_000_000));
        runtime
            .window_counters
            .insert(format!("{key_id}:rolling"), counter.clone());
        let window = SnapshotBudgetWindow {
            id: format!("{key_id}:rolling"),
            scope_kind: "upstream_key".to_string(),
            upstream_key_id: key_id,
            model_pk: None,
            window_kind: "rolling".to_string(),
            period_ms: Some(18_000_000),
            limit_micro_usd: 10_000_000,
            reserve_pct: 0,
        };
        let remote = llmroller_proto::usage_poller::SanitizedWindow {
            kind: "rolling_5h".to_string(),
            status: "exhausted".to_string(),
            used_percent: 100.0,
            resets_at: Some(1_000),
            remaining_requests: None,
            remaining_tokens: None,
            remaining_micro_usd: None,
            reset_in_sec: None,
        };
        reconcile(&runtime, key_id, &[window], &[remote], 500);
        let observation = counter.observation.read().expect("observation");
        assert_eq!(observation.used_micro_usd, 10_000_000);
        assert_eq!(observation.resets_at_ms, Some(1_000));
        assert_eq!(
            runtime
                .key_health
                .get(&key_id)
                .and_then(|entry| entry.exhausted_until_ms),
            Some(1_000)
        );
    }
}
