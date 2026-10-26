use super::*;

pub(crate) const MIB: u64 = 1024 * 1024;
pub(crate) const PERF3_RSS_LIMIT: u64 = 300 * MIB + 64 * MIB;
pub(crate) const PERF5_TRACE_BUDGET: u64 = 204 * MIB + 64 * MIB;
pub(crate) const PERF4_COLD_START_LIMIT: Duration = Duration::from_secs(2);
pub(crate) const PERF4_IDLE_RSS_LIMIT: u64 = 50 * MIB;
/// Open SSE streams are already past their first frame, so this body is not
/// part of the pre-first-byte retention under test. PERF-3 separately holds 64
/// real 1 MiB bodies in the pre-first phase.
pub(crate) const PERF3_STREAM_BODY_BYTES: usize = 1024;
/// Representative chat-completions body for the traceability TTFB A/B test.
pub(crate) const PERF5_BODY_BYTES: usize = 64 * 1024;
pub(crate) const BENCH_MODEL: &str = "bench-model";
pub(crate) const INSTANCE_KEY_ID: &str = "bench-master-key";

#[derive(Debug, Clone, Copy)]
pub(crate) struct Scale {
    pub(crate) full: bool,
    pub(crate) events: usize,
    pub(crate) keys: usize,
    pub(crate) models: usize,
    pub(crate) rollups_per_key: usize,
    pub(crate) perf3_pins: usize,
    pub(crate) perf4_pins: usize,
    pub(crate) streams: usize,
    pub(crate) pre_first: usize,
    pub(crate) perf1_64k_requests: usize,
    pub(crate) perf1_512k_requests: usize,
    pub(crate) perf1_2m_requests: usize,
    pub(crate) perf5_requests: usize,
    pub(crate) sse_frames: usize,
    pub(crate) slow_delay_ms: u64,
}

impl Scale {
    pub(crate) const fn quick() -> Self {
        Self {
            full: false,
            events: 100_000,
            keys: 50,
            models: 10,
            rollups_per_key: 2_016,
            perf3_pins: 10_000,
            perf4_pins: 5_000,
            streams: 200,
            pre_first: 16,
            perf1_64k_requests: 500,
            perf1_512k_requests: 100,
            perf1_2m_requests: 50,
            perf5_requests: 300,
            sse_frames: 10_000,
            slow_delay_ms: 5_000,
        }
    }

    pub(crate) const fn full() -> Self {
        Self {
            full: true,
            events: 50_000_000,
            keys: 500,
            models: 20,
            rollups_per_key: 2_016,
            perf3_pins: 250_000,
            perf4_pins: 10_000,
            streams: 2_000,
            pre_first: 64,
            perf1_64k_requests: 2_000,
            perf1_512k_requests: 500,
            perf1_2m_requests: 100,
            perf5_requests: 1_000,
            sse_frames: 50_000,
            slow_delay_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Stats {
    pub(crate) count: usize,
    pub(crate) p50_ms: f64,
    pub(crate) p95_ms: f64,
    pub(crate) p99_ms: f64,
    pub(crate) max_ms: f64,
    pub(crate) mean_ms: f64,
}

impl Stats {
    pub(crate) fn from_durations(durations: &[Duration]) -> Self {
        let mut ms: Vec<f64> = durations
            .iter()
            .map(|duration| duration.as_secs_f64() * 1_000.0)
            .collect();
        ms.sort_by(|a, b| a.total_cmp(b));
        let count = ms.len();
        if count == 0 {
            return Self {
                count: 0,
                p50_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
                max_ms: 0.0,
                mean_ms: 0.0,
            };
        }
        let percentile = |q: f64| {
            let index = ((count - 1) as f64 * q).round() as usize;
            ms[index.min(count - 1)]
        };
        Self {
            count,
            p50_ms: percentile(0.50),
            p95_ms: percentile(0.95),
            p99_ms: percentile(0.99),
            max_ms: ms[count - 1],
            mean_ms: ms.iter().sum::<f64>() / count as f64,
        }
    }

    pub(crate) fn print(self, label: &str) {
        println!(
            "{label}: n={} p50={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms mean={:.3}ms",
            self.count, self.p50_ms, self.p95_ms, self.p99_ms, self.max_ms, self.mean_ms
        );
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LoadResult {
    pub(crate) stats: Stats,
    pub(crate) statuses: Vec<StatusCode>,
    pub(crate) elapsed: Duration,
    pub(crate) target_rate: f64,
}

impl LoadResult {
    pub(crate) fn assert_all_ok(&self, label: &str) {
        let failures: Vec<StatusCode> = self
            .statuses
            .iter()
            .copied()
            .filter(|status| *status != StatusCode::OK)
            .collect();
        assert!(
            failures.is_empty(),
            "{label}: {} of {} requests failed; first failures: {:?}",
            failures.len(),
            self.statuses.len(),
            failures.iter().take(8).collect::<Vec<_>>()
        );
    }

    pub(crate) fn achieved_rate(&self) -> f64 {
        if self.elapsed.is_zero() {
            0.0
        } else {
            self.statuses.len() as f64 / self.elapsed.as_secs_f64()
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Perf2Result {
    pub(crate) forward: Stats,
    pub(crate) trace: Stats,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SoakResult {
    pub(crate) open_streams: usize,
    pub(crate) baseline_rss: u64,
    pub(crate) after_pins_rss: u64,
    pub(crate) after_streams_rss: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct Perf4Result {
    pub(crate) cold_start: Duration,
    pub(crate) idle_rss_before: u64,
    pub(crate) idle_rss_after: u64,
    pub(crate) events: usize,
    pub(crate) rollups: usize,
    pub(crate) routes: usize,
    pub(crate) pins: usize,
}

#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) _dir: tempfile::TempDir,
    pub(crate) app: Router,
    pub(crate) state: ProxyAuthState,
    pub(crate) token: String,
    pub(crate) client_key_id: ClientKeyId,
    pub(crate) upstream_key_id: UpstreamKeyId,
    pub(crate) canonical_id: String,
}
