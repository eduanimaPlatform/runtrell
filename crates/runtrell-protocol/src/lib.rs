// SPDX-License-Identifier: Apache-2.0
//! Host-driven decision protocol. `run::AgentRunStep::Done` is a judgment-level
//! termination candidate; a host Finalizer must establish business completion.
//! Persistence, operations, budgets and execution belong to the next stage.
pub mod completion;
pub mod conversation;
pub mod error;
pub mod id;
pub mod json_utils;
pub mod run;
pub mod streaming;
pub mod structured_output;
pub mod tool;
pub use completion::message;
pub use schemars;

#[cfg(test)]
mod test_utils;
/// Current wall-clock milliseconds used by conversation records.
pub fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
