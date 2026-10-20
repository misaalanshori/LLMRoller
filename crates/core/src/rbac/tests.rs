//! Table-driven RBAC and actor-scope contract tests.
//!
//! Spec: ./docs/srdd/4. SRDD - Users, Roles, and Provider Ownership.md#41-user-classes-and-actorscope
//! Spec: ./docs/srdd/4. SRDD - Users, Roles, and Provider Ownership.md#42-role-based-access-control-matrix
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#217-actorscope-isolation-for-human-and-system-actors
//! Phase: ./docs/bigbang/big-bang-phases-1.md#12-sub-phase-12-core-domain-identifiers-actorscope-enum-and-seam-traits

use ulid::Ulid;

use super::{
    AccessDecision, ActorScope, CliCommand, OwnershipScope, PoolTier, ResourceAction,
    ResourceScope, Role, RoutingScope, SystemSeal, SystemTask,
};
use crate::ids::UserId;

const ALLOW: AccessDecision = AccessDecision::Allow;
const FORBIDDEN: AccessDecision = AccessDecision::Forbidden;
const NOT_FOUND: AccessDecision = AccessDecision::NotFound;

fn user_id(seed: u128) -> UserId {
    UserId::from_ulid(Ulid::from_parts(1_700_000_000_000, seed))
}

fn assert_actions(
    actor: &ActorScope,
    actor_label: &str,
    scope: ResourceScope,
    expected: [AccessDecision; 3],
) {
    let actual = [
        actor.authorize(ResourceAction::Read, scope),
        actor.authorize(ResourceAction::Manage, scope),
        actor.authorize(ResourceAction::Route, scope),
    ];
    assert_eq!(
        actual, expected,
        "{actor_label} on {scope:?}; order is [read, manage, route]"
    );
}

#[test]
#[doc = "Spec: USR-1, SEC-17, Partial: USR-2, Partial: USR-3, Partial: USR-4"]
fn actor_scope_matrix() {
    let alice = user_id(1);
    let bob = user_id(2);
    let admin_user = user_id(3);

    let admin = ActorScope::user(admin_user, Role::Admin);
    let alice_actor = ActorScope::user(alice, Role::User);
    let bob_actor = ActorScope::user(bob, Role::User);
    let system = ActorScope::system(SystemTask::Writer, SystemSeal::store());

    let global = ResourceScope::Global;
    let alice_personal = ResourceScope::Personal {
        owner_user_id: alice,
    };
    let bob_personal = ResourceScope::Personal { owner_user_id: bob };
    let shared_with_alice = ResourceScope::Shared {
        grantee_user_id: alice,
    };
    let shared_with_bob = ResourceScope::Shared {
        grantee_user_id: bob,
    };

    let cases = [
        (
            "global",
            global,
            [ALLOW, ALLOW, ALLOW],
            [ALLOW, FORBIDDEN, ALLOW],
            [ALLOW, FORBIDDEN, ALLOW],
            [ALLOW, ALLOW, ALLOW],
        ),
        (
            "own personal",
            alice_personal,
            [ALLOW, ALLOW, ALLOW],
            [ALLOW, ALLOW, ALLOW],
            [NOT_FOUND, NOT_FOUND, NOT_FOUND],
            [ALLOW, ALLOW, ALLOW],
        ),
        (
            "admin personal",
            bob_personal,
            [ALLOW, ALLOW, ALLOW],
            [NOT_FOUND, NOT_FOUND, NOT_FOUND],
            [ALLOW, ALLOW, ALLOW],
            [ALLOW, ALLOW, ALLOW],
        ),
        (
            "shared with self",
            shared_with_alice,
            [ALLOW, ALLOW, ALLOW],
            [ALLOW, FORBIDDEN, ALLOW],
            [NOT_FOUND, NOT_FOUND, NOT_FOUND],
            [ALLOW, ALLOW, ALLOW],
        ),
        (
            "shared with another user",
            shared_with_bob,
            [ALLOW, ALLOW, ALLOW],
            [NOT_FOUND, NOT_FOUND, NOT_FOUND],
            [ALLOW, FORBIDDEN, ALLOW],
            [ALLOW, ALLOW, ALLOW],
        ),
    ];

    for (label, scope, admin_expected, alice_expected, bob_expected, system_expected) in cases {
        assert_actions(&admin, "admin", scope, admin_expected);
        assert_actions(&alice_actor, label, scope, alice_expected);
        assert_actions(&bob_actor, "other standard user", scope, bob_expected);
        assert_actions(&system, "system task", scope, system_expected);
    }
}

#[test]
#[doc = "Spec: USR-1"]
fn user_actor_preserves_identity_and_role() {
    let alice = user_id(11);
    let standard_user = ActorScope::user(alice, Role::User);

    assert_eq!(standard_user.user_id(), Some(alice));
    assert_eq!(standard_user.role(), Some(Role::User));
    assert_eq!(standard_user.system_task(), None);
    assert!(!standard_user.is_admin());
    assert!(!standard_user.is_system());

    let admin_user = ActorScope::user(user_id(12), Role::Admin);
    assert!(admin_user.is_admin());
    assert!(!admin_user.is_system());
}

#[test]
#[doc = "Spec: USR-1, Partial: AUD-1"]
fn system_actor_never_masquerades_as_a_user_and_has_explicit_attribution() {
    let tasks = [
        SystemTask::Writer,
        SystemTask::Reloader,
        SystemTask::Discovery,
        SystemTask::UsagePoller,
        SystemTask::RetentionSweeper,
        SystemTask::DataPlaneUsage,
        SystemTask::Cli(CliCommand::RotateMasterKey),
    ];

    for task in tasks {
        let actor = ActorScope::system(task, SystemSeal::server());

        assert!(actor.is_system());
        assert!(!actor.is_admin());
        assert_eq!(
            actor.user_id(),
            None,
            "system rows must use actor_user_id = NULL"
        );
        assert_eq!(actor.role(), None);
        assert_eq!(actor.system_task().copied(), Some(task));
        assert_eq!(actor.audit_system_task(), Some(task.audit_label()));
    }
}

#[test]
#[doc = "Partial: AUD-1"]
fn system_task_audit_labels_are_stable() {
    let cases = [
        (SystemTask::Writer, "writer"),
        (SystemTask::Reloader, "reloader"),
        (SystemTask::Discovery, "discovery"),
        (SystemTask::UsagePoller, "usage_poller"),
        (SystemTask::RetentionSweeper, "retention_sweeper"),
        (SystemTask::DataPlaneUsage, "data_plane_usage"),
        (SystemTask::Cli(CliCommand::Serve), "cli:serve"),
        (SystemTask::Cli(CliCommand::Backup), "cli:backup"),
        (
            SystemTask::Cli(CliCommand::RotateMasterKey),
            "cli:rotate-master-key",
        ),
        (
            SystemTask::Cli(CliCommand::VerifyAuditLog),
            "cli:verify-audit-log",
        ),
        (SystemTask::Cli(CliCommand::ResetAdmin), "cli:reset-admin"),
        (
            SystemTask::Cli(CliCommand::GenerateMasterKey),
            "cli:generate-master-key",
        ),
        (SystemTask::Cli(CliCommand::Healthcheck), "cli:healthcheck"),
    ];

    for (task, label) in cases {
        assert_eq!(task.audit_label(), label);
    }
}

#[test]
#[doc = "Partial: USR-3"]
fn access_decisions_map_to_http_contract() {
    assert_eq!(AccessDecision::Allow.http_status(), 200);
    assert_eq!(AccessDecision::Allow.error_code(), None);
    assert_eq!(AccessDecision::Forbidden.http_status(), 403);
    assert_eq!(
        AccessDecision::Forbidden.error_code(),
        Some("permission_error")
    );
    assert_eq!(AccessDecision::NotFound.http_status(), 404);
    assert_eq!(AccessDecision::NotFound.error_code(), Some("not_found"));
}

#[test]
#[doc = "Partial: USR-2, Partial: RTE-14"]
fn ownership_routing_and_pool_tiers_match_the_closed_contract() {
    assert_eq!(OwnershipScope::Global.as_str(), "global");
    assert_eq!(OwnershipScope::Personal.as_str(), "personal");
    assert_eq!(Role::Admin.as_str(), "admin");
    assert_eq!(Role::User.as_str(), "user");

    assert_eq!(PoolTier::Own.code(), 0);
    assert_eq!(PoolTier::Shared.code(), 1);
    assert_eq!(PoolTier::Global.code(), 2);
    assert_eq!(PoolTier::Blended.code(), 3);
    assert_eq!(PoolTier::from_code(0), Some(PoolTier::Own));
    assert_eq!(PoolTier::from_code(1), Some(PoolTier::Shared));
    assert_eq!(PoolTier::from_code(2), Some(PoolTier::Global));
    assert_eq!(PoolTier::from_code(3), Some(PoolTier::Blended));
    assert_eq!(PoolTier::from_code(4), None);

    assert_eq!(RoutingScope::default(), RoutingScope::PersonalFirst);
    assert_eq!(
        RoutingScope::PersonalFirst.tier_groups(),
        &[&[PoolTier::Own], &[PoolTier::Shared], &[PoolTier::Global]]
    );
    assert_eq!(
        RoutingScope::Blended.tier_groups(),
        &[&[PoolTier::Own, PoolTier::Shared, PoolTier::Global]]
    );
    assert_eq!(
        RoutingScope::PersonalOnly.tier_groups(),
        &[&[PoolTier::Own], &[PoolTier::Shared]]
    );
    assert_eq!(RoutingScope::OwnOnly.tier_groups(), &[&[PoolTier::Own]]);
    assert_eq!(
        RoutingScope::GlobalOnly.tier_groups(),
        &[&[PoolTier::Global]]
    );
}
