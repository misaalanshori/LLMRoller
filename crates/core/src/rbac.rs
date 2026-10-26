#![forbid(unsafe_code)]
//! Role-based access control and actor attribution.
//!
//! Spec: ./docs/srdd/4. SRDD - Users, Roles, and Provider Ownership.md#41-user-classes-and-actorscope
//! Spec: ./docs/srdd/21. SRDD - Concurrency, Lifecycle, and Failure-Recovery Model.md#217-actorscope-isolation-for-human-and-system-actors

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::UserId;

/// Human role stored in `users.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Instance administrator with cross-user management authority.
    Admin,
    /// Standard multi-tenant user restricted to owned or shared resources.
    User,
}

impl Role {
    /// Lowercase database and JSON representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Error returned when parsing an unrecognized role string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRoleError(pub String);

impl fmt::Display for InvalidRoleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid role {:?}; expected admin or user",
            self.0
        )
    }
}

impl std::error::Error for InvalidRoleError {}

impl std::str::FromStr for Role {
    type Err = InvalidRoleError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "admin" => Ok(Self::Admin),
            "user" => Ok(Self::User),
            other => Err(InvalidRoleError(other.to_owned())),
        }
    }
}

/// Ownership scope stored on every provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipScope {
    /// Shared instance-wide provider managed by administrators.
    Global,
    /// Provider owned by exactly one human user.
    Personal,
}

impl OwnershipScope {
    /// Lowercase database and JSON representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Personal => "personal",
        }
    }
}

impl fmt::Display for OwnershipScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Ordered candidate-tier policy for a client key or user default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingScope {
    /// Own, then shared, then global tiers.
    #[default]
    PersonalFirst,
    /// One combined own + shared + global pool.
    Blended,
    /// Own, then shared tiers; never global.
    PersonalOnly,
    /// Own tier only.
    OwnOnly,
    /// Global tier only.
    GlobalOnly,
}

impl RoutingScope {
    /// Lowercase configuration representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersonalFirst => "personal_first",
            Self::Blended => "blended",
            Self::PersonalOnly => "personal_only",
            Self::OwnOnly => "own_only",
            Self::GlobalOnly => "global_only",
        }
    }

    /// Ordered tier groups. The first non-empty group wins.
    #[must_use]
    pub const fn tier_groups(self) -> &'static [&'static [PoolTier]] {
        match self {
            Self::PersonalFirst => &[&[PoolTier::Own], &[PoolTier::Shared], &[PoolTier::Global]],
            Self::Blended => &[&[PoolTier::Own, PoolTier::Shared, PoolTier::Global]],
            Self::PersonalOnly => &[&[PoolTier::Own], &[PoolTier::Shared]],
            Self::OwnOnly => &[&[PoolTier::Own]],
            Self::GlobalOnly => &[&[PoolTier::Global]],
        }
    }

    /// Whether a concrete pool tier is authorized by this routing scope.
    #[must_use]
    pub const fn allows_tier(self, tier: PoolTier) -> bool {
        match tier {
            PoolTier::Blended => matches!(self, Self::Blended),
            PoolTier::Own => !matches!(self, Self::GlobalOnly),
            PoolTier::Shared => matches!(
                self,
                Self::PersonalFirst | Self::Blended | Self::PersonalOnly
            ),
            PoolTier::Global => {
                matches!(self, Self::PersonalFirst | Self::Blended | Self::GlobalOnly)
            }
        }
    }
}

/// Error returned when parsing an unrecognized routing scope string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidRoutingScopeError(pub String);

impl fmt::Display for InvalidRoutingScopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid routing scope {:?}; expected personal_first, blended, personal_only, own_only, or global_only",
            self.0
        )
    }
}

impl std::error::Error for InvalidRoutingScopeError {}

impl std::str::FromStr for RoutingScope {
    type Err = InvalidRoutingScopeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "personal_first" => Ok(Self::PersonalFirst),
            "blended" => Ok(Self::Blended),
            "personal_only" => Ok(Self::PersonalOnly),
            "own_only" => Ok(Self::OwnOnly),
            "global_only" => Ok(Self::GlobalOnly),
            other => Err(InvalidRoutingScopeError(other.to_owned())),
        }
    }
}

impl fmt::Display for RoutingScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Tier recorded on a session pin.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolTier {
    /// Caller-owned personal route.
    Own = 0,
    /// Consented share from another user.
    Shared = 1,
    /// Instance-wide route.
    Global = 2,
    /// Combined own + shared + global selection.
    Blended = 3,
}

impl PoolTier {
    /// Numeric database representation.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decode the numeric database representation.
    #[must_use]
    #[must_use]
    pub const fn from_u8(code: u8) -> Option<Self> {
        Self::from_code(code)
    }

    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Own),
            1 => Some(Self::Shared),
            2 => Some(Self::Global),
            3 => Some(Self::Blended),
            _ => None,
        }
    }

    /// Lowercase database and JSON representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Shared => "shared",
            Self::Global => "global",
            Self::Blended => "blended",
        }
    }
}

impl fmt::Display for PoolTier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// CLI maintenance commands that run under a system actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CliCommand {
    /// Run the HTTP server and background tasks.
    #[serde(rename = "serve")]
    Serve,
    /// Produce a consistent SQLite online backup.
    #[serde(rename = "backup")]
    Backup,
    /// Resume offline master-key rotation.
    #[serde(rename = "rotate-master-key")]
    RotateMasterKey,
    /// Verify the audit-log HMAC chain.
    #[serde(rename = "verify-audit-log")]
    VerifyAuditLog,
    /// Restore an administrator account offline.
    #[serde(rename = "reset-admin")]
    ResetAdmin,
    /// Print a newly generated master key.
    #[serde(rename = "generate-master-key")]
    GenerateMasterKey,
    /// Probe loopback readiness for container health checks.
    #[serde(rename = "healthcheck")]
    Healthcheck,
}

impl CliCommand {
    /// Canonical command-line spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Serve => "serve",
            Self::Backup => "backup",
            Self::RotateMasterKey => "rotate-master-key",
            Self::VerifyAuditLog => "verify-audit-log",
            Self::ResetAdmin => "reset-admin",
            Self::GenerateMasterKey => "generate-master-key",
            Self::Healthcheck => "healthcheck",
        }
    }
}

impl fmt::Display for CliCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Internal task identity written to `audit_log.detail_json.system_task`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemTask {
    /// Dual-priority SQLite writer.
    Writer,
    /// Configuration snapshot reloader.
    Reloader,
    /// Scheduled `/models` discovery.
    Discovery,
    /// Scheduled provider `/usage` polling.
    UsagePoller,
    /// Retention and vacuum sweeper.
    RetentionSweeper,
    /// Data-plane usage batching.
    DataPlaneUsage,
    /// Offline CLI maintenance command.
    Cli(CliCommand),
}

impl SystemTask {
    /// Stable audit attribution label.
    #[must_use]
    pub fn audit_label(&self) -> String {
        match self {
            Self::Cli(command) => format!("cli:{}", command.as_str()),
            Self::Writer => "writer".to_owned(),
            Self::Reloader => "reloader".to_owned(),
            Self::Discovery => "discovery".to_owned(),
            Self::UsagePoller => "usage_poller".to_owned(),
            Self::RetentionSweeper => "retention_sweeper".to_owned(),
            Self::DataPlaneUsage => "data_plane_usage".to_owned(),
        }
    }
}

impl fmt::Display for SystemTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.audit_label())
    }
}

/// Construction token for [`ActorScope::System`].
///
/// The token is exposed only so `crates/store` and `crates/server` can attribute
/// background work without inventing a user. `crates/server/tests/workspace_boundaries.rs`
/// rejects any reference to this type from `crates/admin`, `crates/proxy`, and
/// `crates/proto`, so control-plane handlers cannot construct a system actor.
#[derive(Debug, Clone, Copy)]
pub struct SystemSeal(());

impl SystemSeal {
    /// Seal for the server process's background tasks.
    #[doc(hidden)]
    #[must_use]
    pub const fn server() -> Self {
        Self(())
    }

    /// Seal for store maintenance and startup work.
    #[doc(hidden)]
    #[must_use]
    pub const fn store() -> Self {
        Self(())
    }
}

/// Authenticated actor attached to every control-plane and repository operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorScope {
    /// Human account resolved by authentication middleware.
    User {
        /// Human account identifier.
        user_id: UserId,
        /// Human role.
        role: Role,
    },
    /// Internal background task or offline CLI command.
    System {
        /// Task identity recorded in the audit row.
        task: SystemTask,
    },
}

impl ActorScope {
    /// Construct a human actor.
    #[must_use]
    pub const fn user(user_id: UserId, role: Role) -> Self {
        Self::User { user_id, role }
    }

    /// Construct a system actor for `crates/store` or `crates/server`.
    #[doc(hidden)]
    #[must_use]
    pub const fn system(task: SystemTask, _seal: SystemSeal) -> Self {
        Self::System { task }
    }

    /// Human user identifier, or `None` for system tasks.
    #[must_use]
    pub const fn user_id(&self) -> Option<UserId> {
        match self {
            Self::User { user_id, .. } => Some(*user_id),
            Self::System { .. } => None,
        }
    }

    /// Human role, or `None` for system tasks.
    #[must_use]
    pub const fn role(&self) -> Option<Role> {
        match self {
            Self::User { role, .. } => Some(*role),
            Self::System { .. } => None,
        }
    }

    /// System task identity, or `None` for human actors.
    #[must_use]
    pub const fn system_task(&self) -> Option<&SystemTask> {
        match self {
            Self::User { .. } => None,
            Self::System { task } => Some(task),
        }
    }

    /// `detail_json.system_task` attribution for a system actor.
    #[must_use]
    pub fn audit_system_task(&self) -> Option<String> {
        self.system_task().map(SystemTask::audit_label)
    }

    /// Whether this is an administrator actor. System tasks are never admins.
    #[must_use]
    pub const fn is_admin(&self) -> bool {
        matches!(
            self,
            Self::User {
                role: Role::Admin,
                ..
            }
        )
    }

    /// Whether this is an internal system actor.
    #[must_use]
    pub const fn is_system(&self) -> bool {
        matches!(self, Self::System { .. })
    }

    /// Authorize an action against a resource scope.
    #[must_use]
    pub fn authorize(&self, action: ResourceAction, resource: ResourceScope) -> AccessDecision {
        match self {
            Self::System { .. } => AccessDecision::Allow,
            Self::User { user_id, role } => match resource {
                ResourceScope::Global => match (role, action) {
                    (_, ResourceAction::Read | ResourceAction::Route) => AccessDecision::Allow,
                    (Role::Admin, ResourceAction::Manage) => AccessDecision::Allow,
                    (Role::User, ResourceAction::Manage) => AccessDecision::Forbidden,
                },
                ResourceScope::Personal { owner_user_id } => {
                    if owner_user_id == *user_id || *role == Role::Admin {
                        AccessDecision::Allow
                    } else {
                        AccessDecision::NotFound
                    }
                }
                ResourceScope::Shared { grantee_user_id } => {
                    if grantee_user_id == *user_id || *role == Role::Admin {
                        if *role == Role::User && action == ResourceAction::Manage {
                            AccessDecision::Forbidden
                        } else {
                            AccessDecision::Allow
                        }
                    } else {
                        AccessDecision::NotFound
                    }
                }
            },
        }
    }
}

/// Action requested by a control-plane handler or repository method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceAction {
    /// Inspect metadata or details.
    Read,
    /// Create, edit, test, calibrate, or delete.
    Manage,
    /// Send inference traffic through the resource.
    Route,
}

/// Ownership context attached to an authorization check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceScope {
    /// Global instance resource.
    Global,
    /// Personal resource owned by exactly one user.
    Personal {
        /// Owner of the personal resource.
        owner_user_id: UserId,
    },
    /// Personal resource shared with one grantee.
    Shared {
        /// Grantee evaluating the shared resource.
        grantee_user_id: UserId,
    },
}

/// Authorization result mapped directly to the HTTP contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessDecision {
    /// Action is permitted.
    Allow,
    /// Resource is visible but the modification is not permitted (HTTP 403).
    Forbidden,
    /// Resource must be indistinguishable from a missing resource (HTTP 404).
    NotFound,
}

impl AccessDecision {
    /// Whether the action is permitted.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }

    /// HTTP status required by USR-3.
    #[must_use]
    pub const fn http_status(self) -> u16 {
        match self {
            Self::Allow => 200,
            Self::Forbidden => 403,
            Self::NotFound => 404,
        }
    }

    /// Stable API error code, or `None` when allowed.
    #[must_use]
    pub const fn error_code(self) -> Option<&'static str> {
        match self {
            Self::Allow => None,
            Self::Forbidden => Some("permission_error"),
            Self::NotFound => Some("not_found"),
        }
    }
}

#[cfg(test)]
mod tests;
