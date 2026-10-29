#![forbid(unsafe_code)]
//! JSON control-plane CRUD handlers for providers, keys, client keys, and settings.
//!
//! Spec: ./docs/srdd/14. SRDD - API and Web UI Specification.md#142-control-plane-json-api-apiuser-and-adminapi
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#63-provider-and-upstream-key-management-prv
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk
//! Spec: ./docs/srdd/9. SRDD - Data Model.md#933-closed-runtime-settings-registry
//! Phase: ./docs/bigbang/big-bang-phases-2.md#20-sub-phase-20-minimal-control-plane-json-crud-for-providers-keys-client-keys-and-settings

pub(crate) use std::net::SocketAddr;

pub(crate) use axum::extract::{ConnectInfo, Path, Query, Request, State};
pub(crate) use axum::http::{HeaderMap, Method, StatusCode};
pub(crate) use axum::middleware::{self, Next};
pub(crate) use axum::response::{Html, Response};
pub(crate) use axum::routing::{delete, get, patch, post, put};
pub(crate) use axum::{Json, Router};
pub(crate) use llmroller_core::chat_preview::escape_html;
pub(crate) use llmroller_core::ids::{ClientKeyId, ProviderId, UpstreamKeyId, UserId};
pub(crate) use llmroller_core::provider::{ProviderConfig, ProviderKind};
pub(crate) use llmroller_core::rbac::{ActorScope, OwnershipScope};
pub(crate) use llmroller_core::seams::secret_string;
pub(crate) use llmroller_core::snapshot::ConfigDomain;
pub(crate) use llmroller_store::client_keys::{
    ClientKey, ClientKeyError, ClientKeyPatch, NewClientKey, create_client_key, delete_client_key,
    list_client_keys, patch_client_key, set_client_key_pause_tx,
};
pub(crate) use llmroller_store::crypto::MasterKey;
pub(crate) use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, Provider, ProviderError, ProviderPatch, UpstreamKey,
    UpstreamKeyPatch, create_provider, create_upstream_key, delete_provider, delete_upstream_key,
    get_provider, get_upstream_key, list_providers, list_upstream_keys, patch_provider,
    patch_upstream_key,
};
pub(crate) use llmroller_store::service_accounts::{
    NewServiceAccount, ServiceAccountError, ServiceAccountPatch, create_service_account_tx,
    delete_service_account_tx, list_service_accounts, patch_service_account_tx,
    set_service_account_cidrs_tx,
};
pub(crate) use llmroller_store::settings::{
    RuntimeSettings, SettingsError, load_settings, update_setting, validate_setting,
};
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use sqlx::{Connection as _, Sqlite, Transaction};
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::future::Future;
pub(crate) use std::pin::Pin;
pub(crate) use std::sync::{Arc, Mutex as StdMutex};

pub(crate) use crate::auth::{AuthAppState, AuthError, authenticate_session};

type MutationResponseError = (StatusCode, Json<serde_json::Value>);
type MutationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MutationResponseError>> + Send + 'a>>;

tokio::task_local! {
    /// Audit metadata for the mutation currently executing inside the control
    /// plane. Set by `control_mutation_barrier` before dispatch (F09).
    static CURRENT_MUTATION_AUDIT: Option<MutationAuditContext>;
    /// Set by `execute_atomic_mutation` when a handler already wrote its entity
    /// SQL, `audit_log` row, and `config_version` bump atomically. The barrier
    /// then skips its fallback audit/version publication.
    static MUTATION_HANDLED: std::sync::atomic::AtomicBool;
}

pub(crate) mod active_pins;
pub(crate) mod budgets;
pub(crate) mod client_keys;
pub(crate) mod common;
pub(crate) mod log_detail_drawer;
pub(crate) mod logs_and_audit;
pub(crate) mod models;
pub(crate) mod partials;
pub(crate) mod pin_move_eligibility;
pub(crate) mod providers;
pub(crate) mod router;
pub(crate) mod service_accounts;
pub(crate) mod sessions_and_traces;
pub(crate) mod settings;
pub(crate) mod shares;
pub(crate) mod trace_rehydration;
pub(crate) mod usage;
pub(crate) mod usage_filters;
pub(crate) mod users_and_auth;

pub(crate) use active_pins::*;
pub(crate) use budgets::*;
pub(crate) use client_keys::*;
pub use common::ApiAppState;
pub(crate) use common::*;
pub(crate) use log_detail_drawer::*;
pub(crate) use logs_and_audit::*;
pub(crate) use models::*;
pub(crate) use partials::*;
pub(crate) use pin_move_eligibility::{
    verify_move_target_for_admin, verify_move_target_for_owned_pins,
};
pub(crate) use providers::*;
pub use router::*;
pub(crate) use service_accounts::*;
pub(crate) use sessions_and_traces::*;
pub(crate) use settings::*;
pub(crate) use shares::*;
pub(crate) use usage::*;
pub(crate) use usage_filters::{
    api_logs_export_query, audit_log_query, bind_api_logs_export_filters, bind_audit_log_filters,
    bind_ledger_filters, bind_logs_export_filters, bind_tenant_audit_visibility,
    bind_usage_filters, canonical_total, event_query, ledger_query, logs_export_query,
    rollup_query, tenant_audit_visibility,
};
#[doc(hidden)]
pub use users_and_auth::load_sealer;
pub(crate) use users_and_auth::*;

#[cfg(test)]
mod audit_visibility_tests;
#[cfg(test)]
mod catalog_sync_tests;
#[cfg(test)]
mod log_detail_drawer_tests;
