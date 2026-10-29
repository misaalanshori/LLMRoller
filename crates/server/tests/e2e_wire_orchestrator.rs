//! End-to-end wire tests for data-plane proxy orchestrator with mock upstream server.
//!
//! Spec: Section 5.3, Section 10.0-10.5, Section 11.1-11.2, Section 12.1-12.4
//! Requirements: API-1, API-3, API-4, API-5, API-6, API-7, RTE-1, RTE-3, RTE-9, TRN-1, TRN-2, TRN-3, USG-1

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt as _;
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_core::ids::{ModelPk, UpstreamKeyId, UserId};
use llmroller_core::provider::ProviderKind;
use llmroller_core::rbac::{ActorScope, OwnershipScope, SystemSeal, SystemTask};
use llmroller_core::seams::{ExposeSecret as _, OsEntropy, secret_bytes, secret_string};
use llmroller_proxy::auth::ProxyAuthState;
use llmroller_proxy::listeners::data_plane_router;
use llmroller_proxy::orchestrator::PreflightBudgetSemaphore;
use llmroller_server::reloader::load_initial_snapshot;
use llmroller_store::client_keys::{NewClientKey, create_client_key_tx};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::providers::{
    NewProvider, NewUpstreamKey, create_provider_tx, create_upstream_key_tx,
};
use llmroller_store::settings::seed_defaults;
use llmroller_store::writer::{WriterHandle, flush_data_queues, start_writer};

#[path = "e2e_wire_orchestrator/common.rs"]
pub(crate) mod common;
#[path = "e2e_wire_orchestrator/failover_and_errors.rs"]
mod failover_and_errors;
#[path = "e2e_wire_orchestrator/forwarding.rs"]
mod forwarding;
#[path = "e2e_wire_orchestrator/geo_attribution.rs"]
mod geo_attribution;
#[path = "e2e_wire_orchestrator/limits_and_quotas.rs"]
mod limits_and_quotas;
#[path = "e2e_wire_orchestrator/protocols_and_headers.rs"]
mod protocols_and_headers;
#[path = "e2e_wire_orchestrator/sessions_and_pins.rs"]
mod sessions_and_pins;

pub(crate) use common::*;
