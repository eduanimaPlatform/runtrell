// SPDX-License-Identifier: Apache-2.0
//! Resolution and accounting for the bundled model catalog.
mod allowance;
mod pricing;
mod snapshot;
mod validate;

#[doc(hidden)]
pub use allowance::AllowanceCharge;
pub use allowance::{AllowanceDebit, AllowanceLedger, ProviderAllowanceEvidence};
pub use pricing::{BillOn, Measured, Scope, Unit};
pub use snapshot::{CACHE_WRITE, CACHE_WRITE_12H, CACHE_WRITE_30M, PricingSnapshot};
#[doc(hidden)]
pub use validate::generate;

use pricing::{Condition, Map, PriceProfile};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Completion,
    RasterGenerate,
    RasterEdit,
    ImageUpscale,
    SvgGenerate,
    SpeechSynthesize,
    Transcribe,
    AudioTranslate,
    VoiceSession,
    MusicGenerate,
    VideoGenerate,
    VideoEdit,
    VideoExtend,
    Ocr,
    Parse,
    Search,
    Fetch,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ResolveError {
    #[error("unknown catalog model")]
    UnknownModel,
    #[error("unsupported catalog operation")]
    UnsupportedOperation,
    #[error("unknown catalog binding")]
    UnknownBinding,
    #[error("no verified adapter and reservation rule for the selected binding")]
    ReservationUnavailable,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Catalog {
    pub catalog_revision: String,
    pub profiles: Map<Profile>,
    pub models: Map<Model>,
    pub operations: Map<Operation>,
    pub price_profiles: Map<PriceProfile>,
    #[serde(default)]
    pub allowances: Map<pricing::Allowance>,
    #[serde(default)]
    pub time_windows: Map<validate::TimeWindow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Profile {
    pub api_family: String,
    pub provider: String,
    pub origin: String,
    pub path: String,
    pub method: String,
    pub auth: String,
    pub transport: String,
    pub wire_version: String,
    pub region: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Model {
    pub operations: Vec<String>,
    pub lifecycle: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Operation {
    pub kind: OperationKind,
    pub default_binding: String,
    pub bindings: Map<Binding>,
    pub input: Vec<String>,
    pub output: Vec<String>,
    pub capabilities: Capabilities,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Binding {
    pub profile: String,
    pub request_model: Option<String>,
    pub status: String,
    pub requested_tier: String,
    pub plan: String,
    pub residency: String,
    pub region: Option<String>,
    pub usage_rule: String,
    pub bounds_rule: String,
    pub image_reservation: Option<ImageReservation>,
    pub cache_policy: Option<CachePolicy>,
    pub effects: Map<Effect>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub response_aliases: Vec<String>,
    #[serde(default)]
    pub constrains: Condition,
    #[serde(default)]
    pub allow_fallbacks: bool,
    #[serde(default)]
    pub background: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Effect {
    pub effective_tier: String,
    pub mode: String,
    pub prices: Vec<String>,
    pub endpoint_tag: Option<String>,
    pub upstream_provider: Option<String>,
    pub region: Option<String>,
    #[serde(default)]
    pub tool_prices: Map<String>,
    #[serde(default)]
    pub co_billed: Map<CoBilled>,
    pub cache_lifecycle: Option<CacheLifecycle>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct CoBilled {
    pub model: String,
    pub operation: String,
    pub binding: String,
    pub effect: String,
    #[serde(default)]
    pub applies: Condition,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct CacheLifecycle {
    pub profile: String,
    pub price: String,
    pub usage_rule: String,
    pub bounds_rule: String,
    pub max_tokens: u64,
    pub max_lifetime_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CachePolicy {
    pub kind: CachePolicyKind,
    pub evidence: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CachePolicyKind {
    #[serde(rename = "inherited_direct_image_2_2_5_zero_when_unreported")]
    InheritedDirectImage2And25ZeroWhenUnreported,
}

/// Reservation parameters for requests through the direct raster Images API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageReservation {
    pub calculator_output_tokens: Map<Map<u64>>,
    pub output_multiplier: u64,
    pub prompt_bytes: usize,
    pub text_tokens: u64,
    pub reference_long_edge: u32,
    pub reference_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Limits {
    pub context_tokens: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub request_bytes: Option<u64>,
    pub response_bytes: Option<u64>,
    pub attachment_count: Option<u64>,
    pub attachment_bytes: Option<u64>,
    pub images_in: Option<u64>,
    #[serde(default)]
    pub image_output_tokens_by_size: Map<u64>,
    pub outputs: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub structured_output: bool,
    #[serde(default)]
    pub tool_choice: bool,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub reasoning_modes: Vec<String>,
    #[serde(default)]
    pub image_qualities: Vec<String>,
    #[serde(default)]
    pub image_sizes: Vec<String>,
    #[serde(default)]
    pub video_resolutions: Vec<String>,
    #[serde(default)]
    pub video_audio: Vec<bool>,
    #[serde(default)]
    pub media_modes: Vec<String>,
    #[serde(default)]
    pub duration_ms_options: Vec<u64>,
    #[serde(default)]
    pub formats: Vec<String>,
    #[serde(default)]
    pub cache_write_seconds: Vec<u64>,
    #[serde(default)]
    pub auto_size: bool,
}

#[derive(Deserialize)]
struct Index {
    digest: String,
    catalog: Catalog,
}

static INDEX: LazyLock<Index> = LazyLock::new(|| {
    serde_json::from_str(include_str!("models/index.json")).expect("generated model index")
});

/// A verified catalog entry. Its wire destination, capabilities, and pricing
/// come from the same catalog revision.
#[derive(Clone, Debug)]
pub struct ResolvedModel {
    id: String,
    operation: &'static Operation,
    binding: &'static Binding,
    binding_id: String,
    profile: &'static Profile,
}

pub fn resolve(id: &str, kind: OperationKind) -> Result<ResolvedModel, ResolveError> {
    resolve_binding(id, kind, None)
}

pub fn resolve_binding(
    id: &str,
    kind: OperationKind,
    binding_name: Option<&str>,
) -> Result<ResolvedModel, ResolveError> {
    let catalog = &INDEX.catalog;
    let model = catalog.models.get(id).ok_or(ResolveError::UnknownModel)?;
    let operation = model
        .operations
        .iter()
        .find_map(|op| {
            let operation = catalog.operations.get(op)?;
            (operation.kind == kind).then_some(operation)
        })
        .ok_or(ResolveError::UnsupportedOperation)?;
    let binding_id = binding_name.unwrap_or(&operation.default_binding);
    let binding = operation
        .bindings
        .get(binding_id)
        .ok_or(ResolveError::UnknownBinding)?;
    if binding.status != "verified" || model.lifecycle == "retired" {
        return Err(ResolveError::ReservationUnavailable);
    }
    Ok(ResolvedModel {
        id: id.into(),
        operation,
        binding,
        binding_id: binding_id.into(),
        profile: &catalog.profiles[&binding.profile],
    })
}

impl ResolvedModel {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn kind(&self) -> OperationKind {
        self.operation.kind
    }
    pub fn capabilities(&self) -> &Capabilities {
        &self.operation.capabilities
    }
    pub fn limits(&self) -> &Limits {
        &self.binding.limits
    }
    pub fn image_reservation(&self) -> Option<&ImageReservation> {
        self.binding.image_reservation.as_ref()
    }
    pub fn inputs(&self) -> &[String] {
        &self.operation.input
    }
    pub fn outputs(&self) -> &[String] {
        &self.operation.output
    }
    pub fn api_family(&self) -> &str {
        &self.profile.api_family
    }
    pub fn wire_model(&self) -> &str {
        self.binding.request_model.as_deref().unwrap_or_default()
    }
    pub fn endpoint(&self) -> String {
        format!(
            "{}{}",
            self.profile.origin,
            self.profile.path.replace("{model}", self.wire_model())
        )
    }
    pub fn endpoint_tags(&self) -> Vec<&str> {
        self.binding
            .effects
            .values()
            .filter_map(|effect| effect.endpoint_tag.as_deref())
            .collect()
    }
    pub fn billing_mode(&self) -> Option<&str> {
        self.binding
            .effects
            .values()
            .next()
            .map(|effect| effect.mode.as_str())
    }
    pub fn response_aliases(&self) -> &[String] {
        &self.binding.response_aliases
    }
    pub fn background(&self) -> bool {
        self.binding.background
    }
    pub fn cache_retention_seconds(&self) -> Option<u64> {
        match self.binding.constrains.get("retention_seconds") {
            Some(pricing::Predicate::Range { min, max_exclusive })
                if *max_exclusive == min.checked_add(1) =>
            {
                Some(*min)
            }
            _ => None,
        }
    }
    pub fn catalog_revision(&self) -> &str {
        &INDEX.catalog.catalog_revision
    }
    pub fn catalog_digest(&self) -> &str {
        &INDEX.digest
    }
    pub fn pricing(&self) -> PricingSnapshot {
        PricingSnapshot::new(self, &INDEX)
    }
    pub fn cost(&self, usage: &crate::Usage) -> crate::Result<crate::Amount> {
        self.pricing().cost(usage)
    }
}

#[cfg(test)]
mod tests;
