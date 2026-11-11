// SPDX-License-Identifier: Apache-2.0
//! Host-authorized deletion of run-owned recoverable data.
use crate::{Result, RunHandle};

/// The host supplies the complete run-owned payload backend (workspace, artifacts,
/// context and any custom copies). Success means all owned bytes are removed and
/// future reads and writes for this run are durably fenced. Implementations must
/// be idempotent and resumable after process death. References to independent
/// source material do not authorize deleting that material.
///
/// A host with several backends composes them here, retrying every backend after
/// an error. Never acknowledge success while a backend is unavailable or omitted.
pub trait RunPayloadStore: Send + Sync {
    fn erase_payloads(
        &self,
        run: &RunHandle,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}

impl RunPayloadStore for crate::data::DataStore {
    async fn erase_payloads(&self, run: &RunHandle) -> Result<()> {
        self.erase_run(run)
    }
}
