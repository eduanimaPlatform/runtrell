// Derived from Rig v0.45.0 (98ed06d7f1c911e6e4c7fdb5d690093bc964c514), crates/rig-core/src/streaming/mod.rs.
// Copyright (c) 2024, Playgrounds Analytics Inc.
// Modified for runtrell; inherited code: MIT (LICENSES/MIT-Rig.txt); changes: Apache-2.0.
//! Streaming items and ordered part events, independent of I/O.
mod event;
pub use crate::json_utils::parse_partial_arguments;
pub use event::*;
use serde::{Deserialize, Serialize};
/// Unmodeled JSON payload with content-redacted `Debug` output.
/// Serialization preserves the payload; [`Self::value`] explicitly exposes it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnknownPayload(serde_json::Value);

impl UnknownPayload {
    /// Wrap a raw unmodeled payload.
    pub fn new(value: serde_json::Value) -> Self {
        Self(value)
    }

    /// The raw payload, for consumers who opt in to the content.
    pub fn value(&self) -> &serde_json::Value {
        &self.0
    }
}

impl std::fmt::Debug for UnknownPayload {
    /// Reports serialized size without exposing payload content.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes = serde_json::to_vec(&self.0).map_or(0, |json| json.len());
        write!(f, "UnknownPayload({bytes} bytes redacted)")
    }
}

impl From<serde_json::Value> for UnknownPayload {
    fn from(value: serde_json::Value) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod unknown_payload_tests;
