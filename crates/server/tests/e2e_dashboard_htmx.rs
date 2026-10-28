#![doc = "Spec: USG-9, Spec: TRC-6, Spec: UI-1, Spec: UI-2, Spec: UI-6, Spec: AUD-2, Spec: AUTH-4, Spec: USR-5, Spec: CLK-6, Spec: CLK-7, Spec: USG-10, Spec: TRC-9, Spec: UI-4, Spec: UI-5"]
//! End-to-end integration tests for HTMX web UI, dashboards, key projection, and chat preview.
//!
//! Spec: Section 3.3 (A6), Section 6.10, Section 6.11, Section 8.2, Section 12.6, Section 14.3,
//! Requirements: UI-1, UI-2, UI-4, TRC-6, AUTH-4, USR-5, MDL-3, MDL-4, AUD-1, SEC-6, TRC-5, TRC-8, TRC-9, SEC-19.

use std::sync::Arc;
use tempfile::tempdir;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::chat_preview::{escape_html, sanitize_assistant_text};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock, OsEntropy};
use llmroller_proto::headers::sanitize_csv_cell;
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
pub(crate) use llmroller_store::users::{NewUser, create_user};

#[path = "e2e_dashboard_htmx/app_module_split.rs"]
mod app_module_split;
#[path = "e2e_dashboard_htmx/dashboards_and_partials.rs"]
mod dashboards_and_partials;
#[path = "e2e_dashboard_htmx/log_detail_fields.rs"]
mod log_detail_fields;
#[path = "e2e_dashboard_htmx/logs_and_analytics.rs"]
mod logs_and_analytics;
#[path = "e2e_dashboard_htmx/logs_retention_settings.rs"]
mod logs_retention_settings;
#[path = "e2e_dashboard_htmx/overview_retention_settings.rs"]
mod overview_retention_settings;
#[path = "e2e_dashboard_htmx/provider_badge.rs"]
mod provider_badge;
#[path = "e2e_dashboard_htmx/remaining_filters.rs"]
mod remaining_filters;
#[path = "e2e_dashboard_htmx/setup.rs"]
pub(crate) mod setup;
#[path = "e2e_dashboard_htmx/token_class_projections.rs"]
mod token_class_projections;
#[path = "e2e_dashboard_htmx/usage_filters.rs"]
mod usage_filters;
#[path = "e2e_dashboard_htmx/usage_ledger_and_csv.rs"]
mod usage_ledger_and_csv;

pub(crate) use setup::*;

/// Absolute path to `crates/admin/templates`.
pub(crate) fn templates_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("admin")
        .join("templates")
}
