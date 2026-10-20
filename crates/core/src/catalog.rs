//! Model catalog, discovery lifecycle, normalization, surrogate model_pk, and exposure rules.
//!
//! Spec: Section 6.6, Section 10.6, Section 14.2.2, Section 19.1 (ADR-7),
//! Requirements: MDL-1, MDL-2, MDL-3, MDL-4, MDL-5, MDL-6, MDL-7.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ids::{ModelPk, ProviderId, UpstreamKeyId, UserId};
use crate::settings::GlobalExposureMode;
use crate::strategy::RoutingStrategy;

pub const MAX_PERSONAL_MODELS_PER_USER: usize = 2000;

/// Errors that occur during model identifier normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizeError {
    Empty,
    TooLong,
    InvalidCharacters,
}

impl std::fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "model ID cannot be empty"),
            Self::TooLong => write!(f, "model ID exceeds 256 characters"),
            Self::InvalidCharacters => write!(f, "model ID contains invalid characters"),
        }
    }
}

impl std::error::Error for NormalizeError {}

/// Collapse consecutive hyphens into a single hyphen and trim leading/trailing hyphens.
pub fn collapse_hyphens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_hyphen = false;
    for c in s.chars() {
        if c == '-' {
            if !last_was_hyphen {
                out.push('-');
                last_was_hyphen = true;
            }
        } else {
            out.push(c);
            last_was_hyphen = false;
        }
    }
    out.trim_matches('-').to_string()
}

/// Pure deterministic model ID normalizer preserving dots between ASCII digits.
///
/// Implements SRDD Section 10.6 specification:
/// 1. Trim whitespace, check 1..=256 bytes printable ASCII.
/// 2. Lowercase and strip vendor prefix (up to last '/').
/// 3. Strip or rewrite colon tag suffixes (:free, :thinking, :extended, :beta, etc.).
/// 4. Replace '_' and ' ' with '-'.
/// 5. Preserve '.' iff immediately preceded and followed by an ASCII digit.
/// 6. Collapse consecutive '-' and trim.
pub fn normalize_model_id(raw: &str) -> Result<String, NormalizeError> {
    let s = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    if s.is_empty() {
        return Err(NormalizeError::Empty);
    }
    if s.len() > 256 {
        return Err(NormalizeError::TooLong);
    }
    if !s.bytes().all(|b| (0x21..=0x7E).contains(&b) || b == b' ') {
        return Err(NormalizeError::InvalidCharacters);
    }

    let mut s = s.to_ascii_lowercase();
    if let Some(idx) = s.rfind('/') {
        s = s[idx + 1..].to_string();
    }
    if let Some(idx) = s.rfind(':') {
        let (base, tag) = (&s[..idx], &s[idx + 1..]);
        s = match tag {
            "free" => format!("{base}-free"),
            "thinking" => format!("{base}-thinking"),
            "nitro" | "floor" => base.to_string(),
            "extended" => format!("{base}-extended"),
            "beta" => format!("{base}-beta"),
            other => format!("{base}-{other}"),
        };
    }
    let replaced: String = s
        .chars()
        .map(|c| if c == '_' || c == ' ' { '-' } else { c })
        .collect();

    let bytes = replaced.as_bytes();
    let mut dotted = String::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'.' {
            let prev_digit = i > 0 && bytes[i - 1].is_ascii_digit();
            let next_digit = i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit();
            dotted.push(if prev_digit && next_digit { '.' } else { '-' });
        } else {
            dotted.push(b as char);
        }
    }
    let collapsed = collapse_hyphens(&dotted);
    if collapsed.is_empty() {
        return Err(NormalizeError::Empty);
    }
    Ok(collapsed)
}

/// Normalizes raw model ID while preserving vendor prefix using `--`.
pub fn normalize_keep_vendor(raw: &str) -> Result<String, NormalizeError> {
    let s = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    if s.is_empty() {
        return Err(NormalizeError::Empty);
    }
    if s.len() > 256 {
        return Err(NormalizeError::TooLong);
    }
    if !s.bytes().all(|b| (0x21..=0x7E).contains(&b) || b == b' ') {
        return Err(NormalizeError::InvalidCharacters);
    }

    let s = s.to_ascii_lowercase();
    if let Some(idx) = s.rfind('/') {
        let (vendor, model) = (&s[..idx], &s[idx + 1..]);
        let norm_vendor = collapse_hyphens(&vendor.replace(['_', ' '], "-"));
        let norm_model = normalize_model_id(model)?;
        return Ok(format!("{norm_vendor}--{norm_model}"));
    }
    normalize_model_id(&s)
}

/// Catalog model origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelOrigin {
    Global,
    Personal,
}

/// Merge confirmation review state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    Confirmed,
    PendingReview,
}

/// Model route status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStatus {
    Active,
    Missing,
    Retired,
}

/// Canonical catalog model record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRecord {
    pub model_pk: ModelPk,
    pub canonical_id: String,
    pub origin: ModelOrigin,
    pub owner_user_id: Option<UserId>,
    pub exposed: Option<bool>,
    pub merge_state: MergeState,
    pub routing_strategy: Option<RoutingStrategy>,
}

/// Discovered model route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRoute {
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: ModelPk,
    pub upstream_model_id: String,
    pub vendor_prefix: Option<String>,
    pub status: RouteStatus,
    pub missing_since_ms: Option<i64>,
}

/// Errors returned during model request resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveModelError {
    NotFound,
    AmbiguousUpstreamId(String),
    KillSwitched,
}

/// In-memory catalog projection.
#[derive(Debug, Clone, Default)]
pub struct CatalogState {
    pub models: HashMap<ModelPk, ModelRecord>,
    pub models_by_canonical: HashMap<String, ModelPk>,
    pub model_aliases: HashMap<String, ModelPk>,
    pub model_redirects: HashMap<String, ModelPk>,
    pub routes: Vec<ModelRoute>,
}

impl CatalogState {
    /// Resolves an inbound model identifier through the 5-step resolution chain (MDL-6).
    pub fn resolve_model(&self, requested: &str) -> Result<ModelPk, ResolveModelError> {
        let norm = normalize_model_id(requested).map_err(|_| ResolveModelError::NotFound)?;

        // 1. Check confirmed model_aliases
        if let Some(&pk) = self
            .model_aliases
            .get(&norm)
            .or_else(|| self.model_aliases.get(requested))
        {
            return self.check_exposed(pk);
        }

        // 2. Check model_redirects
        if let Some(&pk) = self
            .model_redirects
            .get(&norm)
            .or_else(|| self.model_redirects.get(requested))
        {
            return self.check_exposed(pk);
        }

        // 3. Check models.canonical_id
        if let Some(&pk) = self.models_by_canonical.get(&norm) {
            return self.check_exposed(pk);
        }

        // 4. Fallback: exact match on upstream_model_id
        let mut matching_pks = Vec::new();
        for route in &self.routes {
            if route.upstream_model_id == requested && !matching_pks.contains(&route.model_pk) {
                matching_pks.push(route.model_pk);
            }
        }
        if matching_pks.len() == 1 {
            return self.check_exposed(matching_pks[0]);
        } else if matching_pks.len() > 1 {
            return Err(ResolveModelError::AmbiguousUpstreamId(
                requested.to_string(),
            ));
        }

        Err(ResolveModelError::NotFound)
    }

    fn check_exposed(&self, pk: ModelPk) -> Result<ModelPk, ResolveModelError> {
        if let Some(m) = self.models.get(&pk) {
            if m.exposed == Some(false) {
                return Err(ResolveModelError::KillSwitched);
            }
            Ok(pk)
        } else {
            Err(ResolveModelError::NotFound)
        }
    }

    /// Merges source model into target model without cascading deletions (MDL-4).
    pub fn merge_models(&mut self, source_pk: ModelPk, target_pk: ModelPk) {
        if source_pk == target_pk {
            return;
        }
        if let Some(source_model) = self.models.remove(&source_pk) {
            self.models_by_canonical.remove(&source_model.canonical_id);
            self.model_redirects
                .insert(source_model.canonical_id, target_pk);

            for route in &mut self.routes {
                if route.model_pk == source_pk {
                    route.model_pk = target_pk;
                }
            }
            for pk in self.model_aliases.values_mut() {
                if *pk == source_pk {
                    *pk = target_pk;
                }
            }
        }
    }
}

/// Evaluates global model exposure under union or intersection mode (MDL-3).
pub fn is_global_model_exposed(
    model: &ModelRecord,
    mode: GlobalExposureMode,
    provider_has_usable_keys: &HashMap<ProviderId, bool>,
    model_providers: &[ProviderId],
) -> bool {
    if let Some(exp) = model.exposed {
        return exp;
    }

    match mode {
        GlobalExposureMode::Union => {
            model.iter_serving_usable(provider_has_usable_keys, model_providers)
        }
        GlobalExposureMode::Intersection => {
            let active_providers: Vec<&ProviderId> = model_providers
                .iter()
                .filter(|pid| provider_has_usable_keys.contains_key(pid))
                .collect();
            if active_providers.is_empty() {
                false
            } else {
                active_providers
                    .iter()
                    .all(|pid| provider_has_usable_keys.get(pid).copied().unwrap_or(false))
            }
        }
    }
}

trait ServingUsableExt {
    fn iter_serving_usable(
        &self,
        usable: &HashMap<ProviderId, bool>,
        serving: &[ProviderId],
    ) -> bool;
}

impl ServingUsableExt for ModelRecord {
    fn iter_serving_usable(
        &self,
        usable: &HashMap<ProviderId, bool>,
        serving: &[ProviderId],
    ) -> bool {
        serving
            .iter()
            .any(|pid| usable.get(pid).copied().unwrap_or(false))
    }
}

/// Upgrades personal model to global when discovered on a global provider (MDL-5).
pub fn upgrade_personal_to_global(model: &mut ModelRecord) {
    if model.origin == ModelOrigin::Personal {
        model.origin = ModelOrigin::Global;
        model.owner_user_id = None;
    }
}

#[cfg(test)]
pub mod tests;
