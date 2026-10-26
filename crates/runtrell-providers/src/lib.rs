// SPDX-License-Identifier: Apache-2.0
#![forbid(unsafe_code)]
//! Optional, single-send ModelPort implementations. See docs/design/providers.md.
use runtrell::attachments::AttachmentResolver;
pub use runtrell::images::ImageLimits;
use runtrell::{ModelLimits, RunHandle};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// Kept out of Debug, Display and all adapter error messages.
#[derive(Clone)]
pub struct ApiKey(String);
impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}
impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = &self.0;
        f.write_str("ApiKey([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterSupport {
    pub input: bool,
    pub cached: bool,
    pub output: bool,
    pub reasoning: bool,
    pub image_input: bool,
    pub image_output: bool,
    pub cached_image_input: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub limits: ModelLimits,
    pub json_schema: bool,
    pub tool_calling: bool,
    pub parallel_tool_calls: bool,
    pub images: ImageCapabilities,
    pub documents: DocumentCapabilities,
    pub counters: CounterSupport,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageCapabilities {
    pub input: bool,
    pub generation: bool,
    pub reference_edit: bool,
    pub mask_edit: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentTransports {
    pub url: bool,
    pub inline_bytes: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentCapabilities {
    pub pdf: DocumentTransports,
    pub image: DocumentTransports,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLimits {
    pub max_inputs: usize,
    pub max_total_bytes: u64,
}
impl Default for DocumentLimits {
    fn default() -> Self {
        Self {
            max_inputs: 4,
            max_total_bytes: 8 * 1024 * 1024,
        }
    }
}
/// Counter relationships are runtime Usage invariants, independent of the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageRelation {
    SubsetOfInput,
    SubsetOfOutput,
    IntersectionOfCachedAndImageInput,
}
impl Capabilities {
    pub fn usage_relations(&self) -> [(&'static str, UsageRelation); 5] {
        [
            ("cached", UsageRelation::SubsetOfInput),
            ("image_input", UsageRelation::SubsetOfInput),
            ("reasoning", UsageRelation::SubsetOfOutput),
            ("image_output", UsageRelation::SubsetOfOutput),
            (
                "cached_image_input",
                UsageRelation::IntersectionOfCachedAndImageInput,
            ),
        ]
    }
    pub fn result_lookup(&self) -> Option<&'static str> {
        None
    }
    pub fn idempotency_scope(&self) -> Option<&'static str> {
        None
    }
}

/// The host pins all operational values. Endpoint is the full POST URL, including
/// the model path for generateContent. It must have no credentials/query/fragment.
#[derive(Clone)]
pub struct Config {
    pub endpoint: String,
    pub key: ApiKey,
    pub capabilities: Capabilities,
    pub run: RunHandle,
    pub session: Option<String>,
    pub input_token_upper_bound: u64,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub image_limits: ImageLimits,
    pub document_limits: DocumentLimits,
    pub attachment_resolver: Option<Arc<dyn AttachmentResolver>>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestOptions {
    pub json_schema: Option<Value>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Completed,
    MaxTokens,
    ContentFilter,
    ToolCall,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    Absent,
    Partial,
    TotalsReported,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    Authentication,
    InvalidRequest,
    RateLimit,
    SpendingLimit,
    Unclassified,
    Redirect,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub class: ErrorClass,
    pub status: u16,
    pub retry_after: Option<String>,
}

#[cfg(any(feature = "openai", feature = "gemini", feature = "openrouter"))]
mod adapter;
#[cfg(any(feature = "openai", feature = "gemini", feature = "openrouter"))]
mod documents;
#[cfg(any(feature = "openai", feature = "gemini", feature = "openrouter"))]
pub use adapter::*;
#[cfg(feature = "gemini")]
mod images;
#[cfg(feature = "gemini")]
pub use images::GeminiImages;
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("endpoint", &"[HOST ENDPOINT]")
            .field("key", &self.key)
            .field("run", &self.run)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn api_key_debug_redacts_even_in_containers_and_clones() {
        let key = ApiKey::new("FAKE-SECRET-NEVER-PRINT");
        let output = format!("{:?}", Some(vec![key.clone(), key]));
        assert!(!output.contains("FAKE-SECRET-NEVER-PRINT"));
        assert!(output.contains("[REDACTED]"));
    }
}
