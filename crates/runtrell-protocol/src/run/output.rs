// Derived from Rig v0.45.0 (98ed06d7f1c911e6e4c7fdb5d690093bc964c514), crates/rig-agent/src/run/output.rs.
// Copyright (c) 2024, Playgrounds Analytics Inc.
// Modified for runtrell; inherited code: MIT (LICENSES/MIT-Rig.txt); changes: Apache-2.0.
//! Structured-output enforcement modes for agent requests.
//!
//! ```
//! use runtrell_protocol::run::output::OutputMode;
//! let mode = OutputMode::default();
//! assert_eq!(mode, OutputMode::Auto);
//! ```

use serde::{Deserialize, Serialize};

/// Select how requests convey an agent's output schema.
/// Native enforcement depends on provider support. Tool and prompted modes are
/// best-effort; callers must validate returned values before relying on the schema.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputMode {
    /// Resolve to tool output when a schema and function tools are present,
    /// tool choice permits it, and native output cannot compose with tools.
    /// Otherwise use native output.
    #[default]
    Auto,
    /// Offer the schema as a synthetic final-answer tool instead of a native
    /// constraint. Calling it is best-effort; callers must validate the result.
    Tool,
    /// Send a native structured-output constraint on every turn.
    /// Enforcement and compatibility with tool calls depend on provider capabilities.
    Native,
    /// Put the schema in the system prompt and return final text verbatim.
    /// Callers must extract and validate JSON; prose and markdown may be present.
    Prompted,
}
