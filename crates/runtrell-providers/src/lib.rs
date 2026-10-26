// SPDX-License-Identifier: Apache-2.0
#![forbid(unsafe_code)]
//! Optional, single-send ModelPort implementations. See docs/design/providers.md.
use runtrell::{ModelLimits, RunHandle};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub image_input: bool,
    pub counters: CounterSupport,
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
    pub fn image_generation(&self) -> bool {
        false
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
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestOptions {
    pub json_schema: Option<Value>,
    #[serde(default)]
    pub images: Vec<ImageInput>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageInput {
    pub mime_type: String,
    pub data_base64: String,
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
pub use adapter::*;
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
