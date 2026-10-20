//! Key projection and redaction for tenant privacy (Section 4.2, Section 12.6.2).
//!
//! Spec: USR-4, UI-2.

use llmroller_core::ids::{UpstreamKeyId, UserId};
use llmroller_core::rbac::{ActorScope, OwnershipScope, Role};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectedKeyDto {
    pub id: Option<String>,
    pub label: String,
    pub provider_name: String,
    pub priority: Option<i64>,
    pub secret_last4: Option<String>,
    pub is_global_pool: bool,
}

/// Input parameters for key projection.
#[derive(Debug, Clone)]
pub struct KeyProjectionInput<'a> {
    pub key_id: &'a UpstreamKeyId,
    pub key_label: &'a str,
    pub provider_name: &'a str,
    pub ownership: OwnershipScope,
    pub owner_user_id: Option<&'a UserId>,
    pub priority: Option<i64>,
    pub secret_last4: Option<&'a str>,
}

/// Redacts global and non-owned keys to "global pool" for standard users (USR-4).
pub fn project_key(actor: &ActorScope, input: &KeyProjectionInput<'_>) -> ProjectedKeyDto {
    let is_admin = actor.role() == Some(Role::Admin);
    let is_owner = match (actor.user_id(), input.owner_user_id) {
        (Some(caller_id), Some(owner_id)) => caller_id == *owner_id,
        _ => false,
    };

    if is_admin || is_owner {
        ProjectedKeyDto {
            id: Some(input.key_id.to_string()),
            label: input.key_label.to_string(),
            provider_name: input.provider_name.to_string(),
            priority: input.priority,
            secret_last4: input.secret_last4.map(str::to_string),
            is_global_pool: input.ownership == OwnershipScope::Global,
        }
    } else {
        ProjectedKeyDto {
            id: None,
            label: "global pool".to_string(),
            provider_name: input.provider_name.to_string(),
            priority: None,
            secret_last4: None,
            is_global_pool: true,
        }
    }
}
