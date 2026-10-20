//! Key and route health state machine, credential strike verification, and protocol blocks.
//!
//! Spec: Section 6.8, Section 7.2, Section 10.5, Section 21.5,
//! Requirements: API-8, REL-5.

use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};

use crate::ids::{ClientKeyId, ModelPk, UpstreamKeyId};

/// Health status of an upstream key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyHealthStatus {
    Healthy,
    CoolingDown { until_ms: i64 },
    Exhausted { until_ms: i64 },
    Invalid,
}

/// Outcome of a credential strike evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrikeResult {
    Healthy,
    ProbeRequired,
    MarkInvalid,
}

/// Verifies credential strikes with a 10-minute sliding window (Section 10.5).
#[derive(Debug, Default)]
pub struct CredentialStrikeVerifier {
    strikes: RwLock<HashMap<UpstreamKeyId, Vec<(ClientKeyId, i64)>>>,
}

impl CredentialStrikeVerifier {
    pub fn new() -> Self {
        Self {
            strikes: RwLock::new(HashMap::new()),
        }
    }

    /// Records a credential strike (401 or credential 403) from a client key.
    pub fn record_strike(
        &self,
        key_id: UpstreamKeyId,
        client_key_id: ClientKeyId,
        now_ms: i64,
    ) -> StrikeResult {
        let mut lock = self.strikes.write();
        let list = lock.entry(key_id).or_default();

        // 1. Prune strikes older than 10 minutes (600,000 ms)
        list.retain(|(_, ts)| now_ms.saturating_sub(*ts) < 600_000);

        // 2. Add current strike
        list.push((client_key_id, now_ms));

        // 3. Count distinct client keys
        let distinct_clients: HashSet<ClientKeyId> = list.iter().map(|(k, _)| *k).collect();
        if distinct_clients.len() >= 2 {
            StrikeResult::MarkInvalid
        } else {
            StrikeResult::ProbeRequired
        }
    }

    /// Records the outcome of a single-flight GET /models verification probe.
    pub fn record_probe_result(&self, key_id: UpstreamKeyId, probe_success: bool) -> StrikeResult {
        let mut lock = self.strikes.write();
        if probe_success {
            // 2xx probe clears all strikes for this key
            lock.remove(&key_id);
            StrikeResult::Healthy
        } else {
            // 401/403 probe marks key invalid immediately
            StrikeResult::MarkInvalid
        }
    }
}

/// In-memory table for route protocol blocks (Section 10.5).
#[derive(Debug, Default)]
pub struct RouteProtocolBlocks {
    blocks: RwLock<HashMap<(UpstreamKeyId, ModelPk, String), i64>>,
}

impl RouteProtocolBlocks {
    pub fn new() -> Self {
        Self {
            blocks: RwLock::new(HashMap::new()),
        }
    }

    pub fn block_protocol(
        &self,
        key: UpstreamKeyId,
        model: ModelPk,
        protocol: &str,
        until_ms: i64,
    ) {
        let mut lock = self.blocks.write();
        lock.insert((key, model, protocol.to_string()), until_ms);
    }

    pub fn is_blocked(
        &self,
        key: UpstreamKeyId,
        model: ModelPk,
        protocol: &str,
        now_ms: i64,
    ) -> bool {
        let lock = self.blocks.read();
        lock.get(&(key, model, protocol.to_string()))
            .is_some_and(|until| now_ms < *until)
    }
}

/// In-memory table for unsupported auxiliary endpoints (Section 10.5.2).
#[derive(Debug, Default)]
pub struct AuxUnsupported {
    unsupported: RwLock<HashMap<(UpstreamKeyId, String), i64>>,
}

impl AuxUnsupported {
    pub fn new() -> Self {
        Self {
            unsupported: RwLock::new(HashMap::new()),
        }
    }

    pub fn mark_unsupported(&self, key: UpstreamKeyId, endpoint: &str, until_ms: i64) {
        let mut lock = self.unsupported.write();
        lock.insert((key, endpoint.to_string()), until_ms);
    }

    pub fn is_unsupported(&self, key: UpstreamKeyId, endpoint: &str, now_ms: i64) -> bool {
        let lock = self.unsupported.read();
        lock.get(&(key, endpoint.to_string()))
            .is_some_and(|until| now_ms < *until)
    }
}

/// Versioned health update check preventing stale data-plane writes.
pub fn is_health_write_valid(current_health_version: i64, write_health_version: i64) -> bool {
    write_health_version >= current_health_version
}

#[cfg(test)]
pub mod tests;
