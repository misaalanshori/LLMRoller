use super::*;
use llmroller_core::seams::OsEntropy;
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role, open_pool};
use llmroller_store::settings::seed_defaults;
use std::sync::Arc;

#[test]
#[doc = "Spec: REL-4, LIM-1"]
fn hydrating_window_preserves_original_bucket_timestamps_for_rolling_expiry() {
    let runtime = llmroller_core::runtime_state::RuntimeState::new();
    let key_id = llmroller_core::ids::UpstreamKeyId::generate();
    let period_ms = 5 * 3_600_000; // 5 hours
    let now_ms = 100 * 3_600_000;
    let old_bucket_ms = now_ms - period_ms + 300_000; // 4h55m ago (expires in 5m)
    let recent_bucket_ms = now_ms - 600_000; // 10m ago

    let mut ring = llmroller_core::windows::RollingRing::new(period_ms);
    ring.record(old_bucket_ms, 4_000_000);
    ring.record(recent_bucket_ms, 2_000_000);

    let window = llmroller_core::snapshot::SnapshotBudgetWindow {
        id: "w-rolling-5h".to_string(),
        scope_kind: "upstream_key".to_string(),
        upstream_key_id: key_id,
        model_pk: None,
        window_kind: "rolling".to_string(),
        period_ms: Some(period_ms),
        anchor_at: None,
        upstream_window_kind: None,
        limit_micro_usd: 10_000_000,
        reserve_pct: 0,
    };
    runtime.hydrate_window_counter_from_ring(&window, Some(&ring), now_ms);
    let counter = runtime.window_counter("w-rolling-5h").expect("counter");
    assert_eq!(counter.local_used(now_ms), 6_000_000);
    // 6 minutes later, the 4h55m-old bucket must have expired, leaving only 2_000_000!
    assert_eq!(counter.local_used(now_ms + 360_000), 2_000_000);
}

#[test]
#[doc = "Spec: SEC-10, OBS-1"]
fn non_loopback_plaintext_bind_fails_when_require_tls_is_true() {
    assert_eq!(
        llmroller_proxy::metrics::check_data_plane_bind_security("0.0.0.0:8080", false, true),
        Err(2)
    );
    assert_eq!(
        llmroller_proxy::metrics::check_data_plane_bind_security("0.0.0.0:8080", false, false),
        Ok(true)
    );
    assert_eq!(
        llmroller_proxy::metrics::check_data_plane_bind_security("127.0.0.1:8080", false, true),
        Ok(false)
    );
}

#[tokio::test]
#[doc = "Spec: TRC-1, AUD-1, REL-2"]
async fn traceability_auto_disable_updates_value_json_reloads_snapshot_and_audits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("llmroller.db");
    let pool = open_pool(&db, Role::Writer).await.expect("open pool");
    migrate(&pool).await.expect("migrate");
    let master_key = MasterKey::from_bytes([0x11; 32]);
    load_or_initialize(&pool, "mk_test", &master_key, Arc::new(OsEntropy), 1_000)
        .await
        .expect("init instance");
    seed_defaults(
        &pool,
        Some(llmroller_core::strategy::RoutingStrategy::SmoothWeightedRr),
        1_000,
    )
    .await
    .expect("seed");

    let cfg = llmroller_core::settings::TraceabilityConfig {
        enabled: true,
        match_filters: llmroller_core::settings::TraceabilityMatch::default(),
        retention_days: 7,
        auto_disable_at: Some(5_000),
    };
    let (reloader_tx, mut reloader_rx) = tokio::sync::mpsc::channel(4);
    let disabled = auto_disable_expired_traceability(
        &pool,
        &cfg,
        b"0123456789abcdef0123456789abcdef",
        Some(&reloader_tx),
        6_000,
    )
    .await
    .expect("auto disable");
    assert!(disabled);

    let loaded = llmroller_store::settings::load_settings(&pool)
        .await
        .expect("load settings");
    assert!(!loaded.traceability_config.enabled);
    let notif = reloader_rx.try_recv().expect("reloader notified");
    assert!(
        notif
            .changed_domains
            .contains(&llmroller_core::snapshot::ConfigDomain::Settings)
    );
}
