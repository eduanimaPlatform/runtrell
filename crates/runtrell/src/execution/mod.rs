// SPDX-License-Identifier: Apache-2.0
//! Lazy execution. Backend receipts, resource reclamation and cost are independent.
use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
mod controller;
mod linux;
mod process;
pub use linux::{LinuxBackend, LinuxCpuBandwidth, LinuxInventory, LinuxUsage};
#[cfg(feature = "python")]
pub mod python;
pub use controller::{freeze_workspace, merge_copy, merge_process_copy, request_cancel};
pub use process::ProcessBackend;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Isolation {
    Trusted,
    Untrusted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionLimits {
    pub isolation: Isolation,
    pub cpu_millis: Option<u64>,
    pub ram_bytes: Option<u64>,
    pub pids: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub file_bytes: Option<u64>,
    pub wall_ms: u64,
    pub acquire_ms: u64,
    pub stop_ms: u64,
    pub cleanup_ms: u64,
    pub output_bytes: usize,
    /// Empty means deny all network; initial backends reject nonempty allowlists.
    pub network_allow: Vec<String>,
}
impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            isolation: Isolation::Trusted,
            cpu_millis: None,
            ram_bytes: None,
            pids: None,
            disk_bytes: None,
            file_bytes: None,
            wall_ms: 1000,
            acquire_ms: 1000,
            stop_ms: 1000,
            cleanup_ms: 1000,
            output_bytes: 4096,
            network_allow: vec![],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub isolation: Isolation,
    pub cpu: bool,
    pub ram: bool,
    pub pids: bool,
    pub disk: bool,
    pub file: bool,
    pub network_deny: bool,
    pub runtime_version: String,
}
impl Capabilities {
    pub fn check(&self, r: &ExecutionRequest) -> Result<()> {
        let l = &r.limits;
        if (l.isolation == Isolation::Untrusted && self.isolation != Isolation::Untrusted)
            || (l.cpu_millis.is_some() && !self.cpu)
            || (l.ram_bytes.is_some() && !self.ram)
            || (l.pids.is_some() && !self.pids)
            || (l.disk_bytes.is_some() && !self.disk)
            || (l.file_bytes.is_some() && !self.file)
            || !l.network_allow.is_empty()
            || !self.network_deny
        {
            return Err(Error::Unsupported(
                "required isolation/resource/network capability".into(),
            ));
        }
        if r.interactive || !r.credential_scopes.is_empty() {
            return Err(Error::Unsupported(
                "interactive installer or credentials".into(),
            ));
        }
        if l.wall_ms == 0
            || l.wall_ms > 86_400_000
            || l.output_bytes == 0
            || l.output_bytes > 16_777_216
            || [l.acquire_ms, l.stop_ms, l.cleanup_ms]
                .iter()
                .any(|n| *n == 0 || *n > 86_400_000)
            || [
                l.cpu_millis,
                l.ram_bytes,
                l.pids,
                l.disk_bytes,
                l.file_bytes,
            ]
            .contains(&Some(0))
        {
            return Err(Error::Invalid("EXECUTION_LIMITS".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Program {
    Process {
        executable: String,
        args: Vec<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRequest {
    /// Require an isolation backend that can stop a process tree.
    pub requires_descendants: bool,
    pub limits: ExecutionLimits,
    pub program: Program,
    /// Optional host-pinned executable SHA-256, checked again by OS backends.
    pub executable_hash: Option<String>,
    pub input: Option<WorkspaceSnapshot>,
    pub input_files: BTreeMap<String, Vec<u8>>,
    pub output_paths: Vec<String>,
    /// Setup is a managed execution too; this records its purpose, not a bypass.
    pub setup: bool,
    pub interactive: bool,
    pub credential_scopes: Vec<String>,
}
impl ExecutionRequest {
    pub fn process(executable: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            requires_descendants: false,
            limits: ExecutionLimits::default(),
            executable_hash: None,
            program: Program::Process {
                executable: executable.into(),
                args,
            },
            input: None,
            input_files: BTreeMap::new(),
            output_paths: vec![],
            setup: false,
            interactive: false,
            credential_scopes: vec![],
        }
    }
    pub fn validate_owner(&self, c: &ToolContext) -> Result<()> {
        self.validate_input(&c.claim.run)
    }
    pub fn validate_input(&self, owner: &RunHandle) -> Result<()> {
        if self
            .input
            .as_ref()
            .is_some_and(|s| &s.namespace.owner != owner)
        {
            return Err(Error::PermissionDenied);
        }
        if let Some(snapshot) = &self.input {
            if snapshot.files.len() > 4096
                || snapshot.files.len() != self.input_files.len()
                || snapshot
                    .files
                    .iter()
                    .map(|f| &f.path)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != snapshot.files.len()
            {
                return Err(Error::Conflict);
            }
            let mut total = 0usize;
            for file in &snapshot.files {
                crate::data::logical_path(&file.path)?;
                let bytes = self.input_files.get(&file.path).ok_or(Error::Conflict)?;
                total = total.checked_add(bytes.len()).ok_or(Error::Conflict)?;
                if total > 16_777_216
                    || bytes.len() as u64 != file.bytes
                    || crate::data::content_digest(bytes) != file.hash
                {
                    return Err(Error::Conflict);
                }
            }
        } else if !self.input_files.is_empty() {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRef {
    pub owner: RunHandle,
    pub id: String,
    pub runtime_version: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionState {
    Acquiring,
    Ready,
    Running,
    Collecting,
    Stopped,
    Reclaiming,
    Destroyed,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub execution: ExecutionRef,
    pub state: ExecutionState,
    pub result: Option<Value>,
    pub failure: Option<String>,
    /// None is unknown, not zero. Includes setup and teardown charges.
    pub cost_micro_usd: Option<u64>,
    /// Explicit per-resource receipts. Destroyed alone does not prove volume deletion.
    pub released_resources: std::collections::BTreeSet<Resource>,
}
impl Observation {
    fn local_stop(&mut self) {
        self.released_resources
            .extend([Resource::Cpu, Resource::Memory, Resource::Pid]);
    }
    // Initial local backends allocate no persistent volume; their only disk is scratch.
    fn local_destroy(&mut self) {
        self.released_resources
            .extend([Resource::Disk, Resource::Volume]);
    }
}
/// Stable key = dispatch operation ID. Repeated acquire/start must not duplicate work.
/// stop/destroy require physical evidence; absence after a transport loss is Unknown.
pub trait ExecutionBackend: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    fn supports(&self, request: &ExecutionRequest) -> Result<()> {
        self.capabilities().check(request)
    }
    fn acquire(
        &self,
        context: &ToolContext,
        request: &ExecutionRequest,
    ) -> impl std::future::Future<Output = Result<ExecutionRef>> + Send;
    fn start(
        &self,
        context: &ToolContext,
        execution: &ExecutionRef,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn lookup(
        &self,
        context: &ToolContext,
    ) -> impl std::future::Future<Output = Result<Option<Observation>>> + Send;
    fn cancel(&self, context: &ToolContext)
    -> impl std::future::Future<Output = Result<()>> + Send;
    fn stop(
        &self,
        context: &ToolContext,
    ) -> impl std::future::Future<Output = Result<Progress>> + Send;
    fn destroy(
        &self,
        context: &ToolContext,
    ) -> impl std::future::Future<Output = Result<Progress>> + Send;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Resource {
    Cpu,
    Memory,
    Pid,
    Disk,
    Volume,
    ExternalCost,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReleaseCondition {
    StopConfirmed,
    DeletionConfirmed,
    CostSettled,
}
impl Resource {
    fn condition(self) -> ReleaseCondition {
        match self {
            Self::Cpu | Self::Memory | Self::Pid => ReleaseCondition::StopConfirmed,
            Self::Disk | Self::Volume => ReleaseCondition::DeletionConfirmed,
            Self::ExternalCost => ReleaseCondition::CostSettled,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceEntry {
    pub condition: ReleaseCondition,
    pub released: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub context_operation: String,
    pub owner: RunHandle,
    pub request: ExecutionRequest,
    pub revision: u64,
    pub cancel_requested: bool,
    pub observation: Option<Observation>,
    pub resources: BTreeMap<Resource, ResourceEntry>,
}
impl ExecutionRecord {
    /// Both adapters enforce the same fixed identity, input and resource schema.
    pub fn validate_intent(&self) -> Result<()> {
        self.request.validate_input(&self.owner)?;
        if self.revision != 0
            || self.cancel_requested
            || self.observation.is_some()
            || self.resources.len() != 6
            || self
                .resources
                .iter()
                .any(|(k, v)| v.released || v.condition != k.condition())
        {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    /// Validate a CAS transition and advance its revision without discarding evidence.
    pub fn advance(&self, expected: u64, mut next: Self) -> Result<Self> {
        if self.revision != expected
            || next.revision != expected
            || self.owner != next.owner
            || self.context_operation != next.context_operation
            || self.request != next.request
            || (self.cancel_requested && !next.cancel_requested)
            || next.resources.len() != self.resources.len()
            || self.resources.iter().any(|(k, v)| {
                next.resources
                    .get(k)
                    .is_none_or(|n| n.condition != v.condition || (v.released && !n.released))
            })
            || next.observation.as_ref().is_some_and(|o| {
                o.execution.owner != next.owner || o.execution.id != next.context_operation
            })
        {
            return Err(Error::Conflict);
        }
        let mut normalized = self.clone();
        normalized.cancel_requested = next.cancel_requested;
        if let Some(o) = next.observation.clone() {
            normalized.observe(o);
        }
        if normalized != next {
            return Err(Error::Conflict);
        }
        next.revision = expected.checked_add(1).ok_or(Error::Conflict)?;
        Ok(next)
    }
    pub fn intent(c: &ToolContext, request: ExecutionRequest) -> Self {
        Self {
            context_operation: c.operation.clone(),
            owner: c.claim.run.clone(),
            request,
            revision: 0,
            cancel_requested: false,
            observation: None,
            resources: [
                Resource::Cpu,
                Resource::Memory,
                Resource::Pid,
                Resource::Disk,
                Resource::Volume,
                Resource::ExternalCost,
            ]
            .into_iter()
            .map(|r| {
                (
                    r,
                    ResourceEntry {
                        condition: r.condition(),
                        released: false,
                    },
                )
            })
            .collect(),
        }
    }
    pub fn reclaimed(&self) -> bool {
        self.resources.values().all(|r| r.released)
    }
    pub fn observe(&mut self, mut o: Observation) {
        if let Some(old) = &self.observation {
            fn rank(s: ExecutionState) -> u8 {
                match s {
                    ExecutionState::Acquiring => 0,
                    ExecutionState::Ready => 1,
                    ExecutionState::Running => 2,
                    ExecutionState::Unknown => 2,
                    ExecutionState::Collecting => 3,
                    ExecutionState::Stopped => 4,
                    ExecutionState::Reclaiming => 5,
                    ExecutionState::Destroyed => 6,
                }
            }
            if rank(o.state) < rank(old.state) {
                return;
            }
            if o.cost_micro_usd.is_none() {
                o.cost_micro_usd = old.cost_micro_usd;
            }
            if o.result.is_none() {
                o.result = old.result.clone();
            }
            if o.failure.is_none() {
                o.failure = old.failure.clone();
            }
        }
        for (resource, entry) in &mut self.resources {
            entry.released |= match entry.condition {
                ReleaseCondition::StopConfirmed => {
                    o.released_resources.contains(resource)
                        && matches!(
                            o.state,
                            ExecutionState::Collecting
                                | ExecutionState::Stopped
                                | ExecutionState::Reclaiming
                                | ExecutionState::Destroyed
                        )
                }
                ReleaseCondition::DeletionConfirmed => o.released_resources.contains(resource),
                ReleaseCondition::CostSettled => o.cost_micro_usd.is_some(),
            };
        }
        self.observation = Some(o);
    }
}
/// A durable host can implement this without changing any existing control-store trait.
/// CAS must preserve cancel and receipts. Never delete unknown acquisitions.
pub trait ExecutionLedger: Send + Sync {
    fn insert(
        &self,
        record: ExecutionRecord,
    ) -> impl std::future::Future<Output = Result<ExecutionRecord>> + Send;
    fn get(
        &self,
        owner: &RunHandle,
        operation: &str,
    ) -> impl std::future::Future<Output = Result<ExecutionRecord>> + Send;
    fn cas(
        &self,
        expected: u64,
        record: ExecutionRecord,
    ) -> impl std::future::Future<Output = Result<ExecutionRecord>> + Send;
    fn pending(&self) -> impl std::future::Future<Output = Result<Vec<ExecutionRecord>>> + Send;
}
#[derive(Clone, Default)]
pub struct MemoryExecutionLedger(Arc<Mutex<BTreeMap<String, ExecutionRecord>>>);
fn key(owner: &RunHandle, id: &str) -> String {
    digest(&json!([owner, id]))
}
impl ExecutionLedger for MemoryExecutionLedger {
    async fn insert(&self, r: ExecutionRecord) -> Result<ExecutionRecord> {
        r.validate_intent()?;
        let mut rows = self.0.lock().map_err(|_| Error::Storage)?;
        let old = rows
            .entry(key(&r.owner, &r.context_operation))
            .or_insert_with(|| r.clone());
        if old.request != r.request {
            return Err(Error::Conflict);
        }
        Ok(old.clone())
    }
    async fn get(&self, owner: &RunHandle, id: &str) -> Result<ExecutionRecord> {
        self.0
            .lock()
            .map_err(|_| Error::Storage)?
            .get(&key(owner, id))
            .cloned()
            .ok_or(Error::NotFound)
    }
    async fn cas(&self, expected: u64, r: ExecutionRecord) -> Result<ExecutionRecord> {
        let mut rows = self.0.lock().map_err(|_| Error::Storage)?;
        let old = rows
            .get_mut(&key(&r.owner, &r.context_operation))
            .ok_or(Error::NotFound)?;
        let r = old.advance(expected, r)?;
        *old = r.clone();
        Ok(r)
    }
    async fn pending(&self) -> Result<Vec<ExecutionRecord>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| Error::Storage)?
            .values()
            .filter(|r| !r.reclaimed())
            .cloned()
            .collect())
    }
}

/// Convert the absolute Run deadline to the remaining physical watchdog budget.
fn with_deadline(mut request: ExecutionRequest, c: &ToolContext) -> Result<ExecutionRequest> {
    if let Some(deadline) = c.deadline {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Deadline)?
            .as_millis();
        let remaining = (deadline as u128)
            .checked_sub(now)
            .filter(|v| *v > 0)
            .ok_or(Error::Deadline)?;
        request.limits.wall_ms = request.limits.wall_ms.min(remaining as u64);
    }
    Ok(request)
}
