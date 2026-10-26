use super::*;

pub(crate) async fn build_harness(mock_address: SocketAddr, trace_enabled: bool) -> Harness {
    let dir = tempfile::tempdir().expect("temporary database directory");
    let database_path = dir.path().join("bench.db");
    let pool = open_pool(&database_path, PoolRole::Writer)
        .await
        .expect("open writer pool");
    migrate(&pool).await.expect("run migrations");

    let master_key = MasterKey::from_bytes([0x5A; 32]);
    let entropy: Arc<dyn EntropySource> = Arc::new(OsEntropy);
    load_or_initialize(&pool, INSTANCE_KEY_ID, &master_key, entropy.clone(), 1_000)
        .await
        .expect("initialize instance");
    seed_defaults(&pool, None, 1_000)
        .await
        .expect("seed settings");

    let now_ms = current_epoch_millis();
    let user_id = UserId::generate();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, disabled, created_at)
         VALUES (?, ?, 'bench-hash', 'admin', 0, ?)",
    )
    .bind(user_id.to_string())
    .bind("bench-user")
    .bind(now_ms)
    .execute(&pool)
    .await
    .expect("insert bench user");

    let actor = ActorScope::user(user_id, Role::Admin);
    let mut tx = pool.begin().await.expect("begin setup transaction");
    let provider = create_provider_tx(
        &mut tx,
        &actor,
        NewProvider {
            name: "Bench Provider".to_owned(),
            kind: ProviderKind::Custom,
            base_url: format!("http://{mock_address}"),
            config_json: "{}".to_owned(),
            ownership_scope: OwnershipScope::Global,
            owner_user_id: None,
            usage_url_cross_origin_ack: false,
            allow_private_network: true,
            enabled: true,
        },
        now_ms,
    )
    .await
    .expect("create provider");
    let upstream_key = create_upstream_key_tx(
        &mut tx,
        &actor,
        &master_key,
        INSTANCE_KEY_ID,
        entropy.clone(),
        NewUpstreamKey {
            provider_id: provider.id,
            label: "Bench Key".to_owned(),
            secret: secret_string("sk-bench-upstream"),
            priority: Some(100),
            strict_tier: None,
            enabled: Some(true),
        },
        now_ms,
    )
    .await
    .expect("create upstream key");
    let client_key = create_client_key_tx(
        &mut tx,
        &actor,
        user_id,
        NewClientKey {
            label: "Bench Client Key".to_owned(),
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
        now_ms,
    )
    .await
    .expect("create client key");

    let model_pk = ModelPk::generate();
    sqlx::query(
        "INSERT INTO models (model_pk, canonical_id, display_name, distribution, origin, created_by_user_id)
         VALUES (?, ?, ?, 'weighted', 'global', ?)",
    )
    .bind(model_pk.to_string())
    .bind(BENCH_MODEL)
    .bind(BENCH_MODEL)
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await
    .expect("insert model");
    sqlx::query(
        "INSERT INTO model_routes
            (upstream_key_id, model_pk, upstream_model_id, protocols, status, enabled, discovered_at)
         VALUES (?, ?, ?, '[\"chat\"]', 'active', 1, ?)",
    )
    .bind(upstream_key.id.to_string())
    .bind(model_pk.to_string())
    .bind(BENCH_MODEL)
    .bind(now_ms)
    .execute(&mut *tx)
    .await
    .expect("insert model route");
    tx.commit().await.expect("commit setup");

    let snapshot = load_initial_snapshot(&pool).await.expect("load snapshot");
    let instance_id = snapshot.instance_id;
    let snapshot = if trace_enabled {
        let mut snapshot = snapshot;
        let mut settings = (*snapshot.settings).clone();
        settings.traceability_config.enabled = true;
        settings.traceability_config.match_filters = Default::default();
        snapshot.settings = Arc::new(settings);
        snapshot
    } else {
        snapshot
    };
    let shared_snapshot = Arc::new(ArcSwap::from_pointee(snapshot));
    let mut state = ProxyAuthState::new(shared_snapshot, pool.clone(), Vec::new())
        .with_master_key(master_key.clone());
    state.runtime_state = Arc::new(RuntimeState::new());
    state.session_hmac_secret = Some(secret_bytes(vec![0x42; 32]));
    state.instance_id = Some(instance_id);
    state.master_key_id = Some(INSTANCE_KEY_ID.to_owned());
    state.is_ready.store(true, Ordering::SeqCst);
    if trace_enabled {
        let sealer = Arc::new(XChaCha20Poly1305Sealer::new(
            master_key,
            instance_id,
            Arc::new(OsEntropy),
        ));
        let (sealer_pool, _result_rx) = SealerPool::new(sealer);
        state.sealer_pool = Some(Arc::new(sealer_pool));
        state.master_key_id = Some(INSTANCE_KEY_ID.to_owned());
    }
    let app = data_plane_router(state.clone());
    let token = client_key.raw_token.expose_secret().to_owned();

    Harness {
        _dir: dir,
        app,
        state,
        token,
        client_key_id: client_key.client_key.id,
        upstream_key_id: upstream_key.id,
        canonical_id: BENCH_MODEL.to_owned(),
    }
}

pub(crate) fn current_epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

pub(crate) fn rss_bytes() -> u64 {
    let Ok(pid) = sysinfo::get_current_pid() else {
        return 0;
    };
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).map_or(0, |process| process.memory())
}

pub(crate) fn body_for_size(size: usize) -> Bytes {
    let prefix = r#"{"model":"bench-model","stream":true,"messages":[{"role":"user","content":""#;
    let suffix = r#""}]}"#;
    let padding = size.saturating_sub(prefix.len() + suffix.len());
    let mut body = String::with_capacity(prefix.len() + padding + suffix.len());
    body.push_str(prefix);
    body.push_str(&"x".repeat(padding));
    body.push_str(suffix);
    Bytes::from(body)
}

pub(crate) fn build_request(
    token: &str,
    body: Bytes,
    hold: bool,
    delay_ms: Option<u64>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if hold {
        builder = builder.header("x-mock-hold", "1");
    }
    if let Some(delay_ms) = delay_ms {
        builder = builder.header("x-mock-delay-ms", delay_ms.to_string());
    }
    builder.body(Body::from(body)).expect("build request")
}

pub(crate) async fn send_one(
    app: &Router,
    token: &str,
    body: Bytes,
    hold: bool,
    delay_ms: Option<u64>,
) -> Result<(Duration, StatusCode), String> {
    let request = build_request(token, body, hold, delay_ms);
    let start = Instant::now();
    let response = app
        .clone()
        .oneshot(request)
        .await
        .map_err(|error| format!("router error: {error}"))?;
    let status = response.status();
    let elapsed = start.elapsed();
    if !hold && status == StatusCode::OK {
        // Drain the finite mock response so the upstream connection returns to the pool.
        let _ = response.into_body().collect().await;
    }
    Ok((elapsed, status))
}

pub(crate) async fn warm_up(harness: &Harness, requests: usize, body_size: usize) {
    let body = body_for_size(body_size);
    for _ in 0..requests {
        let result = send_one(&harness.app, &harness.token, body.clone(), false, None).await;
        assert!(
            result.is_ok(),
            "benchmark warm-up request failed: {result:?}"
        );
        assert_eq!(
            result.expect("warm-up result").1,
            StatusCode::OK,
            "benchmark warm-up returned a non-200 status"
        );
    }
}

pub(crate) async fn drain_trace_sealer(harness: &Harness) {
    let Some(pool) = harness.state.sealer_pool.as_ref() else {
        return;
    };
    while pool.in_flight() > 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(crate) async fn run_load(
    harness: &Harness,
    body: Bytes,
    requests: usize,
    target_rate: f64,
    delay_for: impl Fn(usize) -> Option<u64> + Send + Sync + 'static,
) -> LoadResult {
    const SCHEDULER_TICK: Duration = Duration::from_millis(20);
    let mut ticker = tokio::time::interval(SCHEDULER_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tasks = JoinSet::new();
    let started = Instant::now();
    let mut spawned = 0_usize;
    let mut owed = 0.0_f64;

    while spawned < requests {
        ticker.tick().await;
        owed += target_rate * SCHEDULER_TICK.as_secs_f64();
        let batch = owed.floor() as usize;
        owed -= batch as f64;
        for _ in 0..batch.min(requests - spawned) {
            let app = harness.app.clone();
            let token = harness.token.clone();
            let body = body.clone();
            let delay = delay_for(spawned);
            tasks.spawn(async move { send_one(&app, &token, body, false, delay).await });
            spawned += 1;
        }
    }

    let mut durations = Vec::with_capacity(requests);
    let mut statuses = Vec::with_capacity(requests);
    while let Some(joined) = tasks.join_next().await {
        let (elapsed, status) = joined
            .expect("load task panicked")
            .unwrap_or_else(|error| panic!("load request failed: {error}"));
        durations.push(elapsed);
        statuses.push(status);
    }

    LoadResult {
        stats: Stats::from_durations(&durations),
        statuses,
        elapsed: started.elapsed(),
        target_rate,
    }
}

pub(crate) async fn run_direct_load(
    address: SocketAddr,
    body: Bytes,
    requests: usize,
    target_rate: f64,
) -> LoadResult {
    const SCHEDULER_TICK: Duration = Duration::from_millis(20);
    let client = reqwest::Client::new();
    let url = format!("http://{address}/chat/completions");
    let mut ticker = tokio::time::interval(SCHEDULER_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tasks = JoinSet::new();
    let started = Instant::now();
    let mut spawned = 0_usize;
    let mut owed = 0.0_f64;

    while spawned < requests {
        ticker.tick().await;
        owed += target_rate * SCHEDULER_TICK.as_secs_f64();
        let batch = owed.floor() as usize;
        owed -= batch as f64;
        for _ in 0..batch.min(requests - spawned) {
            let client = client.clone();
            let url = url.clone();
            let body = body.clone();
            tasks.spawn(async move {
                let start = Instant::now();
                let response = client
                    .post(&url)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(body)
                    .send()
                    .await
                    .map_err(|error| format!("direct mock request failed: {error}"))?;
                let status = response.status();
                let elapsed = start.elapsed();
                let _ = response.bytes().await;
                Ok::<_, String>((elapsed, status))
            });
            spawned += 1;
        }
    }

    let mut durations = Vec::with_capacity(requests);
    let mut statuses = Vec::with_capacity(requests);
    while let Some(joined) = tasks.join_next().await {
        let (elapsed, status) = joined
            .expect("direct load task panicked")
            .unwrap_or_else(|error| panic!("direct load request failed: {error}"));
        durations.push(elapsed);
        statuses.push(status);
    }
    LoadResult {
        stats: Stats::from_durations(&durations),
        statuses,
        elapsed: started.elapsed(),
        target_rate,
    }
}

pub(crate) fn deterministic_lognormal_delay_ms(state: &mut u64) -> u64 {
    // Log-normal with median 2,000 ms and p95 8,000 ms:
    // sigma = ln(4) / 1.645 ~= 0.8427, mu = ln(2000).
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let u1 = (((*state >> 11) as f64) + 1.0) / ((1_u64 << 53) as f64 + 2.0);
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let u2 = (((*state >> 11) as f64) + 1.0) / ((1_u64 << 53) as f64 + 2.0);
    let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
    let delay = 2_000.0 * (0.842_7 * z).exp();
    delay.clamp(50.0, 30_000.0) as u64
}
