//! Tests for candidate checks C1-C8 and three-tier routing_scope pool selector.
//!
//! Spec: Section 4.5, Section 6.5, Section 6.7, Section 10.0, Section 10.1, Section 10.2,
//! Requirements: CLK-3, RTE-3.

use super::*;
use crate::ids::ShareId;
use crate::provider::ProviderKind;

fn make_valid_candidate(
    owner_id: UserId,
    ownership: OwnershipScope,
    model_pk: ModelPk,
) -> CandidateRoute {
    CandidateRoute {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk,
        provider_id: ProviderId::generate(),
        provider_kind: ProviderKind::Openrouter,
        provider_ownership: ownership,
        provider_owner_id: Some(owner_id),
        provider_fingerprint: "fp-test".to_string(),
        provider_enabled: true,
        key_enabled: true,
        route_enabled: true,
        route_status: RouteStatus::Active,
        protocols: vec!["chat".to_string(), "messages".to_string()],
        protocol_blocked: false,
        key_health_ok: true,
        route_health_ok: true,
        breaker_closed: true,
        user_pref_enabled: None,
        priority: 100,
        budget_admitted: true,
        budget_exhausted_due_to_monthly_cap: false,
    }
}

#[doc = "Spec: CLK-3, RTE-3"]
#[test]
fn test_candidate_checks_c1_through_c8_toggled_individually() {
    let alice = UserId::generate();
    let model_pk = ModelPk::generate();

    let ctx = CallerContext {
        caller_user_id: &alice,
        can_use_global: true,
        owner_disabled: false,
        model_exposed: None,
        merge_state: MergeState::Confirmed,
        inbound_protocol: "chat",
        model_canonical_id: "test-model",
    };

    // Base route is fully valid and should pass as Own tier
    let base = make_valid_candidate(alice, OwnershipScope::Personal, model_pk);
    assert_eq!(
        evaluate_candidate(&base, &ctx, &[]),
        Ok(CandidatePoolTier::Own)
    );

    // C1: provider disabled
    let mut c1_prov = base.clone();
    c1_prov.provider_enabled = false;
    assert_eq!(
        evaluate_candidate(&c1_prov, &ctx, &[]),
        Err(CheckFailure::C1DisabledOrInactive)
    );

    // C1: key disabled
    let mut c1_key = base.clone();
    c1_key.key_enabled = false;
    assert_eq!(
        evaluate_candidate(&c1_key, &ctx, &[]),
        Err(CheckFailure::C1DisabledOrInactive)
    );

    // C1: route missing/retired
    let mut c1_route = base.clone();
    c1_route.route_status = RouteStatus::Missing;
    assert_eq!(
        evaluate_candidate(&c1_route, &ctx, &[]),
        Err(CheckFailure::C1DisabledOrInactive)
    );

    // C2: Tier authorization - Bob calling Alice's personal key without share
    let bob = UserId::generate();
    let bob_ctx = CallerContext {
        caller_user_id: &bob,
        can_use_global: true,
        owner_disabled: false,
        model_exposed: None,
        merge_state: MergeState::Confirmed,
        inbound_protocol: "chat",
        model_canonical_id: "test-model",
    };
    assert_eq!(
        evaluate_candidate(&base, &bob_ctx, &[]),
        Err(CheckFailure::C2TierNotAuthorized)
    );

    // C3: Owner disabled
    let mut c3_ctx = ctx;
    c3_ctx.owner_disabled = true;
    assert_eq!(
        evaluate_candidate(&base, &c3_ctx, &[]),
        Err(CheckFailure::C3GrantorDisabled)
    );

    // C4: Model kill switch (exposed = Some(false))
    let mut c4_ctx = ctx;
    c4_ctx.model_exposed = Some(false);
    assert_eq!(
        evaluate_candidate(&base, &c4_ctx, &[]),
        Err(CheckFailure::C4ModelNotExposedOrKilled)
    );

    // C4: User route preference disabled
    let mut c4_pref = base.clone();
    c4_pref.user_pref_enabled = Some(false);
    assert_eq!(
        evaluate_candidate(&c4_pref, &ctx, &[]),
        Err(CheckFailure::C4ModelNotExposedOrKilled)
    );

    // C5: Protocol blocked or missing
    let mut c5_proto = base.clone();
    c5_proto.protocol_blocked = true;
    assert_eq!(
        evaluate_candidate(&c5_proto, &ctx, &[]),
        Err(CheckFailure::C5ProtocolBlocked)
    );

    // C6: Key or route health unhealthy
    let mut c6_health = base.clone();
    c6_health.route_health_ok = false;
    assert_eq!(
        evaluate_candidate(&c6_health, &ctx, &[]),
        Err(CheckFailure::C6HealthUnhealthy)
    );

    // C7: Origin breaker open
    let mut c7_breaker = base.clone();
    c7_breaker.breaker_closed = false;
    assert_eq!(
        evaluate_candidate(&c7_breaker, &ctx, &[]),
        Err(CheckFailure::C7BreakerOpen)
    );

    // C8: Budget exhausted
    let mut c8_budget = base.clone();
    c8_budget.budget_admitted = false;
    assert_eq!(
        evaluate_candidate(&c8_budget, &ctx, &[]),
        Err(CheckFailure::C8BudgetExhausted)
    );
}

#[doc = "Spec: CLK-3, RTE-3"]
#[test]
fn test_three_tier_pool_selector_personal_first_ordering_and_caps() {
    let alice = UserId::generate();
    let bob = UserId::generate();
    let model_pk = ModelPk::generate();

    // Bob has his own key (Own tier)
    let mut bob_own_route = make_valid_candidate(bob, OwnershipScope::Personal, model_pk);
    bob_own_route.priority = 100;

    // Alice shares her key with Bob (Shared tier)
    let mut alice_shared_route = make_valid_candidate(alice, OwnershipScope::Personal, model_pk);
    alice_shared_route.priority = 50;
    let alice_share = ShareConfig {
        share_id: ShareId::generate(),
        grantor_user_id: alice,
        grantee_user_id: bob,
        provider_id: alice_shared_route.provider_id,
        upstream_key_id: Some(alice_shared_route.upstream_key_id),
        allowed_models: None,
        max_monthly_micro_usd: Some(5_000_000),
        accepted_config_fingerprint: Some("fp-test".to_string()),
        admin_approved_at: None,
        enabled: true,
    };

    // Global route (Global tier)
    let mut global_route =
        make_valid_candidate(UserId::generate(), OwnershipScope::Global, model_pk);
    global_route.provider_owner_id = None;
    global_route.priority = 10;

    let all_routes = vec![
        bob_own_route.clone(),
        alice_shared_route.clone(),
        global_route.clone(),
    ];

    let ctx = CallerContext {
        caller_user_id: &bob,
        can_use_global: true,
        owner_disabled: false,
        model_exposed: None,
        merge_state: MergeState::Confirmed,
        inbound_protocol: "chat",
        model_canonical_id: "deepseek-v4.1-flash",
    };

    // 1. Under personal_first, when Bob's own key is eligible, routes to Own tier
    let (tier, candidates) = select_pool(
        &all_routes,
        &ctx,
        std::slice::from_ref(&alice_share),
        RoutingScope::PersonalFirst,
    )
    .expect("pool selection must succeed");

    assert_eq!(tier, CandidatePoolTier::Own);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].upstream_key_id, bob_own_route.upstream_key_id);

    // 2. Bob's own key is exhausted (budget_admitted = false) -> routes to Alice's Shared tier
    let mut bob_exhausted = bob_own_route.clone();
    bob_exhausted.budget_admitted = false;
    let routes_bob_exhausted = vec![
        bob_exhausted,
        alice_shared_route.clone(),
        global_route.clone(),
    ];

    let (tier, candidates) = select_pool(
        &routes_bob_exhausted,
        &ctx,
        std::slice::from_ref(&alice_share),
        RoutingScope::PersonalFirst,
    )
    .expect("pool selection must succeed");

    assert_eq!(tier, CandidatePoolTier::Shared);
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].upstream_key_id,
        alice_shared_route.upstream_key_id
    );

    // 3. Both Bob's own key and Alice's shared key are exhausted -> routes to Global tier
    let mut alice_exhausted = alice_shared_route.clone();
    alice_exhausted.budget_admitted = false;
    let mut bob_exhausted_2 = bob_own_route.clone();
    bob_exhausted_2.budget_admitted = false;
    let routes_both_exhausted = vec![bob_exhausted_2, alice_exhausted, global_route];

    let (tier, candidates) = select_pool(
        &routes_both_exhausted,
        &ctx,
        std::slice::from_ref(&alice_share),
        RoutingScope::PersonalFirst,
    )
    .expect("pool selection must succeed");

    assert_eq!(tier, CandidatePoolTier::Global);
    assert_eq!(candidates.len(), 1);

    // 4. Monthly cap exhausted vs NoEligibleRoute error codes
    let mut alice_cap_fail = alice_shared_route;
    alice_cap_fail.budget_admitted = false;
    alice_cap_fail.budget_exhausted_due_to_monthly_cap = true;
    let mut bob_cap_fail = bob_own_route;
    bob_cap_fail.budget_admitted = false;
    bob_cap_fail.budget_exhausted_due_to_monthly_cap = true;

    let res_cap = select_pool(
        &[bob_cap_fail, alice_cap_fail],
        &ctx,
        std::slice::from_ref(&alice_share),
        RoutingScope::PersonalOnly,
    );
    assert_eq!(res_cap, Err(PoolSelectionError::MonthlyCapExhausted));

    let res_no_route = select_pool(&[], &ctx, &[], RoutingScope::OwnOnly);
    assert_eq!(res_no_route, Err(PoolSelectionError::NoEligibleRoute));
}
