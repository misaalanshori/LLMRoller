//! Three-tier in-memory login rate limiter (`AUTH-8`, `SEC-12`, `ISSUE-0059`).
//!
//! Extracted from `auth.rs` so the limiter's bounded-map accounting can grow without
//! pushing the handler module further past the project's 1,200-line convention.
//! Re-exported from `crate::auth`, so no caller changed.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use parking_lot::Mutex;

use crate::auth::AuthError;

pub(crate) const RATE_LIMIT_WINDOW_15M_MS: i64 = 15 * 60 * 1000;
pub(crate) const RATE_LIMIT_WINDOW_1H_MS: i64 = 60 * 60 * 1000;

/// Maximum number of distinct keys tracked per limiter map (`AUTH-8`, `SEC-12`,
/// `ISSUE-0059`).
///
/// All three maps are keyed by attacker-controlled values, so all three are bounded.
/// At capacity, expired entries are swept; if a map is still full, one entry is
/// recycled rather than the new one dropped, because dropping the new entry would
/// leave a flooding attacker never rate-limited at all.
pub const MAX_TRACKED_LIMITER_KEYS: usize = 10_000;

/// Tier 1 threshold: failures per IP per 15 minutes.
pub const MAX_FAILURES_PER_IP_15M: usize = 20;
/// Tier 2 threshold: failures per `(username, IP)` per 15 minutes.
pub const MAX_FAILURES_PER_USER_IP_15M: usize = 5;
/// Tier 3 threshold: failures per username per hour, across all IPs.
pub const USERNAME_ATTACK_THRESHOLD_1H: usize = 50;

/// Normalized client IP for rate-limiting (exact IPv4, IPv6 `/64` prefix) per `AUTH-8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NormalizedClientIp {
    V4(Ipv4Addr),
    V6(u64),
}

impl From<IpAddr> for NormalizedClientIp {
    fn from(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(addr) => Self::V4(addr),
            IpAddr::V6(addr) => {
                let segments = addr.segments();
                let prefix = (u64::from(segments[0]) << 48)
                    | (u64::from(segments[1]) << 32)
                    | (u64::from(segments[2]) << 16)
                    | u64::from(segments[3]);
                Self::V6(prefix)
            }
        }
    }
}

/// Three-tier in-memory login rate limiter per `AUTH-8`.
#[derive(Debug)]
pub struct ThreeTierLoginLimiter {
    ip_failures: Mutex<HashMap<NormalizedClientIp, Vec<i64>>>,
    user_ip_failures: Mutex<HashMap<(String, NormalizedClientIp), Vec<i64>>>,
    username_failures: Mutex<HashMap<String, Vec<i64>>>,
}

impl Default for ThreeTierLoginLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Evaluation outcome from the three-tier login limiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimiterOutcome {
    Allowed,
    EnforceTotpOrDelay,
}

impl ThreeTierLoginLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ip_failures: Mutex::new(HashMap::new()),
            user_ip_failures: Mutex::new(HashMap::new()),
            username_failures: Mutex::new(HashMap::new()),
        }
    }

    /// Current key counts for the three maps, for asserting the bound (`ISSUE-0059`).
    ///
    /// Returns `(ip_failures, user_ip_failures, username_failures)`. A bound that no
    /// test can observe is how this defect survived.
    #[must_use]
    pub fn tracked_key_counts(&self) -> (usize, usize, usize) {
        (
            self.ip_failures.lock().len(),
            self.user_ip_failures.lock().len(),
            self.username_failures.lock().len(),
        )
    }

    /// Check whether a login attempt is admitted under the three-tier policy.
    pub fn check(
        &self,
        username: &str,
        ip: IpAddr,
        now_ms: i64,
    ) -> Result<LimiterOutcome, AuthError> {
        let client_key = NormalizedClientIp::from(ip);

        // Tier 1: 20 failures per 15 minutes per IP
        {
            let mut ip_map = self.ip_failures.lock();
            sweep_if_at_capacity(&mut ip_map, RATE_LIMIT_WINDOW_15M_MS, now_ms);
            let attempts = ip_map.entry(client_key).or_default();
            attempts.retain(|&ts| now_ms - ts < RATE_LIMIT_WINDOW_15M_MS);
            if attempts.len() >= MAX_FAILURES_PER_IP_15M {
                return Err(AuthError::RateLimited);
            }
        }

        // Tier 2: 5 failures per 15 minutes per (username, IP)
        {
            let mut user_ip_map = self.user_ip_failures.lock();
            sweep_if_at_capacity(&mut user_ip_map, RATE_LIMIT_WINDOW_15M_MS, now_ms);
            let key = (username.to_ascii_lowercase(), client_key);
            let attempts = user_ip_map.entry(key).or_default();
            attempts.retain(|&ts| now_ms - ts < RATE_LIMIT_WINDOW_15M_MS);
            if attempts.len() >= MAX_FAILURES_PER_USER_IP_15M {
                return Err(AuthError::RateLimited);
            }
        }

        // Tier 3: Across all IPs for this username, > 50 failures per 1 hour
        {
            let mut user_map = self.username_failures.lock();
            sweep_if_at_capacity(&mut user_map, RATE_LIMIT_WINDOW_1H_MS, now_ms);
            let attempts = user_map.entry(username.to_ascii_lowercase()).or_default();
            attempts.retain(|&ts| now_ms - ts < RATE_LIMIT_WINDOW_1H_MS);
            if attempts.len() >= USERNAME_ATTACK_THRESHOLD_1H {
                return Ok(LimiterOutcome::EnforceTotpOrDelay);
            }
        }

        Ok(LimiterOutcome::Allowed)
    }

    /// Record a failed login attempt.
    pub fn record_failure(&self, username: &str, ip: IpAddr, now_ms: i64) {
        let client_key = NormalizedClientIp::from(ip);
        let normalized = username.to_ascii_lowercase();

        {
            let mut ip_map = self.ip_failures.lock();
            sweep_if_at_capacity(&mut ip_map, RATE_LIMIT_WINDOW_15M_MS, now_ms);
            ip_map.entry(client_key).or_default().push(now_ms);
        }
        {
            let mut user_ip_map = self.user_ip_failures.lock();
            sweep_if_at_capacity(&mut user_ip_map, RATE_LIMIT_WINDOW_15M_MS, now_ms);
            user_ip_map
                .entry((normalized.clone(), client_key))
                .or_default()
                .push(now_ms);
        }
        {
            let mut user_map = self.username_failures.lock();
            sweep_if_at_capacity(&mut user_map, RATE_LIMIT_WINDOW_1H_MS, now_ms);
            user_map.entry(normalized).or_default().push(now_ms);
        }
    }

    /// Reset failure counters for a username and IP on successful authentication.
    ///
    /// Clears the `(username, IP)` tier **and** the per-username tier. Clearing only
    /// the pair left the username tier accumulating, so a user who fat-fingered their
    /// password fifty times across an hour stayed escalated to TOTP-or-delay forever
    /// even after authenticating successfully. The per-IP tier is deliberately *not*
    /// cleared: it is keyed by source address, and one successful login from an
    /// address does not mean the address is not also attacking other accounts.
    pub fn record_success(&self, username: &str, ip: IpAddr) {
        let client_key = NormalizedClientIp::from(ip);
        let normalized = username.to_ascii_lowercase();

        {
            let mut user_ip_map = self.user_ip_failures.lock();
            user_ip_map.remove(&(normalized.clone(), client_key));
        }
        {
            let mut user_map = self.username_failures.lock();
            user_map.remove(&normalized);
        }
    }
}

/// Keep a limiter map within [`MAX_TRACKED_LIMITER_KEYS`], sweeping expired entries.
///
/// Shared by all three maps and called from both `check()` and `record_failure()` so
/// the two insertion paths cannot drift apart again — three hand-copied guards is
/// exactly how `record_failure` came to have none.
///
/// Stage 1 drops every key whose window holds no failure newer than `window_ms`.
/// Stage 2, reached only by a genuine `MAX_TRACKED_LIMITER_KEYS`-distinct-key flood
/// inside a single window, recycles slots. Recycling resets the evicted key's counter,
/// which is the accepted cost of a hard bound; dropping the new key instead would
/// leave a flooding attacker never rate-limited at all, which is worse.
///
/// Runs only when the map is at capacity, never unconditionally: a sweep on every
/// request would be a DoS amplifier.
fn sweep_if_at_capacity<K>(map: &mut HashMap<K, Vec<i64>>, window_ms: i64, now_ms: i64)
where
    K: std::hash::Hash + Eq + Clone,
{
    if map.len() < MAX_TRACKED_LIMITER_KEYS {
        return;
    }
    map.retain(|_, attempts| {
        attempts.retain(|&ts| now_ms - ts < window_ms);
        !attempts.is_empty()
    });
    if map.len() >= MAX_TRACKED_LIMITER_KEYS {
        let excess = map.len() - MAX_TRACKED_LIMITER_KEYS + 1;
        let victims: Vec<K> = map.keys().take(excess).cloned().collect();
        for victim in victims {
            map.remove(&victim);
        }
    }
}
