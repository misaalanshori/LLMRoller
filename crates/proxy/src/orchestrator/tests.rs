use super::*;
use llmroller_core::accumulators::MonthlyAccumulatorRegistry;
    use llmroller_core::rbac::Role;
    use llmroller_core::shares::ShareConfig;
    use llmroller_core::snapshot::UserRecord;

    #[test]
    #[doc = "Spec: LIM-1 — the pre-flight reservation honors the caller's output-token ceiling"]
    fn requested_max_output_tokens_reads_every_protocol_spelling() {
        let responses = br#"{"model":"m","input":"hi","max_output_tokens":4096}"#;
        assert_eq!(requested_max_output_tokens(responses), Some(4096));
        let chat = br#"{"model":"m","messages":[],"max_completion_tokens":2048}"#;
        assert_eq!(requested_max_output_tokens(chat), Some(2048));
        let messages = br#"{"model":"m","messages":[],"max_tokens":512}"#;
        assert_eq!(requested_max_output_tokens(messages), Some(512));
        // Precedence: the Responses ceiling wins when several are present.
        let both = br#"{"max_tokens":1,"max_output_tokens":9}"#;
        assert_eq!(requested_max_output_tokens(both), Some(9));
        // Absent, zero, negative, or non-JSON bodies fall back to the 8192 default.
        assert_eq!(requested_max_output_tokens(br#"{"messages":[]}"#), None);
        assert_eq!(requested_max_output_tokens(br#"{"max_tokens":0}"#), None);
        assert_eq!(requested_max_output_tokens(br#"{"max_tokens":-5}"#), None);
        assert_eq!(requested_max_output_tokens(b"not json"), None);
    }

    #[test]
    #[doc = "Spec: REL-1 — F02 open_streams tracks live SSE bodies"]
    fn active_stream_guard_tracks_body_lifetime() {
        let counter = Arc::new(AtomicUsize::new(0));
        let first = ActiveStreamGuard::acquire(counter.clone());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        {
            let _second = ActiveStreamGuard::acquire(counter.clone());
            assert_eq!(counter.load(Ordering::SeqCst), 2);
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        drop(first);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    #[doc = "Spec: TRC-4 — F09 tags subagent traces with their thread id"]
    fn subagent_thread_id_prefers_claude_agent_header() {
        let mut headers = HeaderMap::new();
        assert_eq!(extract_trace_thread_id(&headers), "main");

        headers.insert("x-subagent-id", HeaderValue::from_static("subagent-7"));
        assert_eq!(extract_trace_thread_id(&headers), "subagent-7");

        headers.insert(
            "x-claude-code-agent-id",
            HeaderValue::from_static("claude-agent-2"),
        );
        assert_eq!(extract_trace_thread_id(&headers), "claude-agent-2");

        headers.insert("x-claude-code-agent-id", HeaderValue::from_static("  "));
        assert_eq!(extract_trace_thread_id(&headers), "subagent-7");
    }

    fn tier_candidate(tier: CandidatePoolTier, share_id: Option<ShareId>) -> RouteCandidate {
        RouteCandidate {
            upstream_key_id: UpstreamKeyId::generate(),
            provider_id: ProviderId::generate(),
            model_pk: ModelPk::generate(),
            upstream_model_id: "model".to_string(),
            base_url: "https://example.test".to_string(),
            allow_private_network: false,
            config: llmroller_core::provider::ProviderConfig::default(),
            pool_tier: tier,
            ownership_scope: llmroller_core::rbac::OwnershipScope::Global,
            priority: 1,
            strict_tier: 1,
            provider_kind: ProviderKind::OpencodeGo,
            share_id,
        }
    }

    fn tier_snapshot(
        user_id: UserId,
        share_id: ShareId,
        user_cap: Option<i64>,
        share_cap: Option<i64>,
    ) -> ConfigSnapshot {
        ConfigSnapshot {
            users: Arc::new(HashMap::from([(
                user_id,
                UserRecord {
                    id: user_id,
                    username: "caller".to_string(),
                    role: Role::User,
                    disabled: false,
                    can_use_global_providers: true,
                    global_budget_micro_usd_month: user_cap,
                    default_routing_scope: RoutingScope::PersonalFirst,
                },
            )])),
            shares_by_grantee: Arc::new(HashMap::from([(
                user_id,
                vec![ShareConfig {
                    share_id,
                    grantor_user_id: UserId::generate(),
                    grantee_user_id: user_id,
                    provider_id: ProviderId::generate(),
                    upstream_key_id: None,
                    allowed_models: None,
                    max_monthly_micro_usd: share_cap,
                    accepted_config_fingerprint: None,
                    admin_approved_at: None,
                    enabled: true,
                }],
            )])),
            ..ConfigSnapshot::default()
        }
    }

    #[test]
    #[doc = "Spec: USR-5 — F05 own-tier candidates never charge the user-global cap"]
    fn own_tier_reserves_no_user_global_cap() {
        let user_id = UserId::generate();
        let share_id = ShareId::generate();
        let snapshot = tier_snapshot(user_id, share_id, Some(0), Some(0));
        let registry = MonthlyAccumulatorRegistry::new();
        let candidate = tier_candidate(CandidatePoolTier::Own, None);

        let guards =
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000)
                .expect("own tier must ignore the exhausted global cap");
        drop(guards);
        assert_eq!(registry.usage_for(&BudgetScopeId::User(user_id), 1_000), 0);
    }

    #[test]
    #[doc = "Spec: USR-5 — F05 global-tier candidates reserve the user-global cap"]
    fn global_tier_reserves_user_global_cap() {
        let user_id = UserId::generate();
        let share_id = ShareId::generate();
        let snapshot = tier_snapshot(user_id, share_id, Some(0), Some(1_000_000));
        let registry = MonthlyAccumulatorRegistry::new();
        let candidate = tier_candidate(CandidatePoolTier::Global, None);

        assert!(matches!(
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000),
            Err(GateG7Error::UserGlobalBudgetExhausted { .. })
        ));

        let snapshot = tier_snapshot(user_id, share_id, Some(1_000_000), Some(0));
        let guards =
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000)
                .expect("global tier has headroom");
        drop(guards);
        assert!(
            !registry.is_exhausted(&BudgetScopeId::User(user_id), Some(1_000_000), 1_000),
            "a dropped reservation releases its headroom"
        );
    }

    #[test]
    #[doc = "Spec: SHR-1 — F05 shared-tier candidates reserve the grantor share cap"]
    fn shared_tier_reserves_only_the_share_cap() {
        let user_id = UserId::generate();
        let share_id = ShareId::generate();
        let snapshot = tier_snapshot(user_id, share_id, Some(1_000_000), Some(0));
        let registry = MonthlyAccumulatorRegistry::new();
        let candidate = tier_candidate(CandidatePoolTier::Shared, Some(share_id));

        assert!(matches!(
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000),
            Err(GateG7Error::ProviderShareCapExhausted { .. })
        ));

        let snapshot = tier_snapshot(user_id, share_id, Some(0), Some(1_000_000));
        let guards =
            reserve_candidate_caps(&registry, &snapshot, user_id, &candidate, 1_000, 1_000)
                .expect("share cap has headroom");
        drop(guards);
        assert!(
            registry.usage_for(&BudgetScopeId::ProviderShare(share_id), 1_000) == 0,
            "a dropped share reservation must not commit spend"
        );
    }

    fn session_test_key(mode: &str) -> ClientKeyRecord {
        ClientKeyRecord {
            id: ClientKeyId::generate(),
            owner_user_id: UserId::generate(),
            label: "session-test".to_string(),
            prefix: "llr_test".to_string(),
            key_hash: [0; 32],
            prev_key_hash: None,
            prev_key_expires_at: None,
            routing_scope: "inherit".to_string(),
            allowed_models: None,
            rate_limit_rpm: None,
            rate_limit_tpm: None,
            budget_micro_usd_month: None,
            allowed_cidrs: None,
            constant_session_mode: mode.to_string(),
            service_account_id: None,
            disabled: false,
            paused_at: None,
            revoked_at: None,
            expires_at: None,
        }
    }

    fn session_body(prompt_cache_key: Option<&str>, opening: &str) -> Vec<u8> {
        let mut body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "stable system prompt"},
                {"role": "user", "content": opening}
            ]
        });
        if let Some(key) = prompt_cache_key {
            body["prompt_cache_key"] = serde_json::Value::String(key.to_string());
        }
        serde_json::to_vec(&body).expect("serialize body")
    }

    fn test_session_pin(
        key: &ClientKeyRecord,
        body: &[u8],
        session_hmac_secret: Option<&[u8]>,
        runtime: &RuntimeState,
        now_ms: i64,
    ) -> Option<PinKey> {
        session_pin_key(
            &HeaderMap::new(),
            body,
            &SessionPinContext {
                client_key: key,
                canonical_id: "gpt-4o",
                protocol: "chat",
                session_hmac_secret,
                runtime_state: runtime,
                writer: None,
                audit_hmac_key: None,
                pool: None,
                now_ms,
            },
        )
    }

    fn test_resolved_session(
        key: &ClientKeyRecord,
        body: &[u8],
        runtime: &RuntimeState,
    ) -> Option<ResolvedSession> {
        resolve_session(
            &HeaderMap::new(),
            body,
            &SessionPinContext {
                client_key: key,
                canonical_id: "gpt-4o",
                protocol: "chat",
                session_hmac_secret: Some(b"instance-session-secret-32-bytes"),
                runtime_state: runtime,
                writer: None,
                audit_hmac_key: None,
                pool: None,
                now_ms: 1_000,
            },
        )
    }

    #[test]
    #[doc = "Spec: RTE-1, SEC-16 - forced Step 8 affinity is salted even on Own tier"]
    fn fingerprint_generic_ids_uses_salted_affinity_not_the_raw_client_id() {
        let runtime = RuntimeState::new();
        let key = session_test_key("fingerprint_generic_ids");

        let resolved = test_resolved_session(
            &key,
            &session_body(Some("generic-cache-key"), "opening one"),
            &runtime,
        )
        .expect("generic session must resolve");

        assert_eq!(
            resolved.pin_key.session_hash.len(),
            32,
            "session_hash must be a 32-character tenant-scoped HMAC"
        );
        assert!(
            !resolved.pin_key.session_hash.starts_with("fp:"),
            "Step 8 session_hash must not start with raw 'fp:' prefix"
        );
        assert!(
            resolved.raw_session.is_empty(),
            "a Step 8 affinity must not forward the raw generic client ID"
        );
        assert!(
            resolved.salted_token.starts_with("llr_"),
            "Step 8 must always write the tenant-salted token"
        );
        assert_eq!(
            resolved.outbound_token(PoolTier::Own),
            resolved.salted_token
        );
    }

    #[test]
    #[doc = "Spec: RTE-1 - F06 body fields and Step 8 fingerprint drive session pins"]
    fn session_pin_key_uses_body_fields_and_step8_fingerprint() {
        let runtime = RuntimeState::new();
        let key = session_test_key("flag");

        let first = test_session_pin(
            &key,
            &session_body(Some("cache-key"), "opening one"),
            None,
            &runtime,
            1_000,
        )
        .expect("body prompt_cache_key must produce a pin");
        let second = test_session_pin(
            &key,
            &session_body(Some("cache-key"), "opening two"),
            None,
            &runtime,
            2_000,
        )
        .expect("body prompt_cache_key must produce a pin");
        assert_eq!(first.session_hash, second.session_hash);
        assert_eq!(
            first.session_hash,
            fallback_session_hash(key.id, "cache-key"),
            "Step 7 keeps the tenant-scoped raw session hash"
        );

        let step8_first = test_session_pin(
            &key,
            &session_body(None, "same first question"),
            None,
            &runtime,
            3_000,
        )
        .expect("Step 8 must produce a fingerprint pin");
        let step8_second = test_session_pin(
            &key,
            &session_body(None, "same first question"),
            None,
            &runtime,
            4_000,
        )
        .expect("Step 8 must produce a fingerprint pin");
        assert!(!step8_first.session_hash.starts_with("fp:"));
        assert_eq!(step8_first.session_hash, step8_second.session_hash);
        assert_ne!(step8_first.session_hash, first.session_hash);

        let hmac_pin = test_session_pin(
            &key,
            &session_body(Some("cache-key"), "opening one"),
            Some(b"instance-session-secret-32-bytes"),
            &runtime,
            5_000,
        )
        .expect("HMAC session hash must produce a pin");
        assert_eq!(hmac_pin.session_hash.len(), 32);
        assert_ne!(hmac_pin.session_hash, first.session_hash);
    }

    #[test]
    #[doc = "Spec: RTE-1, SEC-9 - F-046 Step 8 fallback session ID is HMAC hashed into 32 hex chars"]
    fn step_8_fallback_session_id_is_hmac_hashed_into_32_hex_chars() {
        let runtime = RuntimeState::new();
        let key = session_test_key("flag");
        let pin = test_session_pin(
            &key,
            &session_body(None, "opening question"),
            Some(b"instance-session-secret-32-bytes"),
            &runtime,
            1_000,
        )
        .expect("Step 8 must produce a pin");

        assert_eq!(pin.session_hash.len(), 32);
        assert!(!pin.session_hash.starts_with("fp:"));
        assert!(
            pin.session_hash
                .chars()
                .all(|c| matches!(c, '0'..='9' | 'a'..='f'))
        );
    }

    #[test]
    #[doc = "Spec: RTE-1 - F06 constant detector observes generic session sources"]
    fn session_pin_key_records_constant_session_anomaly() {
        let runtime = RuntimeState::new();
        let key = session_test_key("flag");

        for minute in 0..21 {
            let body = serde_json::json!({
                "model": "gpt-4o",
                "session_id": "static-session",
                "messages": [
                    {"role": "system", "content": format!("opening system {minute}")},
                    {"role": "user", "content": format!("opening user {minute}")}
                ]
            });
            let pin = test_session_pin(
                &key,
                &serde_json::to_vec(&body).expect("serialize body"),
                None,
                &runtime,
                1_000_000 + minute * 60_000,
            )
            .expect("generic session ID must produce a pin");
            assert_eq!(
                pin.session_hash,
                fallback_session_hash(key.id, "static-session")
            );
        }

        assert!(
            runtime
                .constant_session_detector
                .is_flagged(key.id, &fallback_session_hash(key.id, "static-session")),
            "21 distinct openings within an hour must flag the static session ID"
        );
    }

    #[tokio::test]
    #[doc = "Spec: API-1, API-2, API-8, RTE-7, RTE-8, RTE-9, RTE-12, RTE-13, RTE-14, REL-3, REL-5, USG-1, USG-2, USG-3, USG-9, USG-10, LIM-1, SHR-2, SHR-6, CLK-3, CLK-7, MDL-1, MDL-4, AUD-1, OBS-1, PRV-3, SEC-9, TRN-3"]
    async fn error_envelope_and_failure_charge_and_origin_key_conformance() {
        use http_body_util::BodyExt;
        let msg_resp = build_protocol_error_response(
            true,
            StatusCode::NOT_FOUND,
            "not_found_error",
            "model_not_found",
            "Unknown model",
            Some("req_1"),
            None,
        );
        assert_eq!(msg_resp.status(), StatusCode::NOT_FOUND);
        let msg_body = msg_resp
            .into_body()
            .collect()
            .await
            .expect("collect msg response body")
            .to_bytes();
        let msg_json: serde_json::Value =
            serde_json::from_slice(&msg_body).expect("parse msg error json");
        assert_eq!(msg_json["type"], "error");
        assert_eq!(msg_json["error"]["type"], "not_found_error");
        assert_eq!(msg_json["error"]["code"], "model_not_found");
        assert_eq!(msg_json["error"]["message"], "Unknown model");
        assert_eq!(msg_json["request_id"], "req_1");

        let oai_resp = build_protocol_error_response(
            false,
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "no_eligible_route",
            "No route",
            Some("req_2"),
            Some(5),
        );
        assert_eq!(oai_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            oai_resp
                .headers()
                .get("retry-after")
                .expect("retry-after header")
                .to_str()
                .expect("ASCII retry-after value"),
            "5"
        );
        let oai_body = oai_resp
            .into_body()
            .collect()
            .await
            .expect("collect oai response body")
            .to_bytes();
        let oai_json: serde_json::Value =
            serde_json::from_slice(&oai_body).expect("parse oai error json");
        assert_eq!(oai_json["error"]["type"], "server_error");
        assert_eq!(oai_json["error"]["code"], "no_eligible_route");
        assert_eq!(oai_json["error"]["message"], "No route");
        assert!(oai_json["error"]["param"].is_null());

        assert_eq!(
            failure_charge(llmroller_core::usage::AttemptOutcome::WaitTimeout, 1200),
            (1200, "estimated_input")
        );
        assert_eq!(
            failure_charge(llmroller_core::usage::AttemptOutcome::Http429, 1200),
            (0, "none")
        );

        let origin = OriginKey::from_base_url("http://127.0.0.1:18080/v1", true);
        assert_eq!(origin.host, "127.0.0.1");
        assert_eq!(origin.port, 18080);

        let mut tap = SseUsageTap::new(EndpointKind::Chat);
        tap.feed_chunk(
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\n",
        );
        assert!(!tap.is_complete_at_eof());
        tap.feed_chunk(b"data: [DONE]\n\n");
        assert!(tap.is_complete_at_eof());
    }

    #[test]
    #[doc = "Spec: CLK-3, RTE-3 - routing scope inherit resolves against users.default_routing_scope"]
    fn routing_scope_inherit_resolves_against_user_default() {
        let user_id = UserId::generate();
        let user = UserRecord {
            id: user_id,
            username: "scoped".to_string(),
            role: Role::User,
            disabled: false,
            can_use_global_providers: true,
            global_budget_micro_usd_month: None,
            default_routing_scope: RoutingScope::GlobalOnly,
        };
        let snapshot = ConfigSnapshot {
            users: Arc::new(HashMap::from([(user_id, user)])),
            ..ConfigSnapshot::default()
        };

        let mut key = session_test_key("flag");
        key.owner_user_id = user_id;
        key.routing_scope = "inherit".to_string();

        assert_eq!(
            resolve_caller_scope(&key, &snapshot),
            RoutingScope::GlobalOnly,
            "routing_scope == 'inherit' must resolve to the user's default_routing_scope"
        );

        key.routing_scope = "own_only".to_string();
        assert_eq!(
            resolve_caller_scope(&key, &snapshot),
            RoutingScope::OwnOnly,
            "explicit routing_scope overrides user default"
        );
    }

    #[test]
    #[doc = "Spec: RTE-8, F029 - rate limit backoff parser supports retry-after-ms, Go duration strings, epoch resets, and embedded JSON reset_seconds"]
    fn rate_limit_backoff_parser_supports_all_upstream_header_and_body_formats() {
        let mut h = HeaderMap::new();
        h.insert("retry-after-ms", HeaderValue::from_static("1500"));
        assert_eq!(parse_rate_limit_backoff_ms(&h, b"", 1_000_000), Some(1500));

        let mut h2 = HeaderMap::new();
        h2.insert(
            "x-ratelimit-reset-requests",
            HeaderValue::from_static("6m0s"),
        );
        assert_eq!(
            parse_rate_limit_backoff_ms(&h2, b"", 1_000_000),
            Some(360_000)
        );

        let mut h3 = HeaderMap::new();
        h3.insert(
            "x-codex-primary-reset-after-seconds",
            HeaderValue::from_static("45"),
        );
        assert_eq!(
            parse_rate_limit_backoff_ms(&h3, b"", 1_000_000),
            Some(45_000)
        );

        let mut h4 = HeaderMap::new();
        h4.insert("x-ratelimit-reset", HeaderValue::from_static("1775000010"));
        assert_eq!(
            parse_rate_limit_backoff_ms(&h4, b"", 1_775_000_000_000),
            Some(10_000)
        );

        let h_empty = HeaderMap::new();
        assert_eq!(
            parse_rate_limit_backoff_ms(
                &h_empty,
                br#"{"error":{"reset_seconds":12.5}}"#,
                1_000_000
            ),
            Some(12_500)
        );
    }
