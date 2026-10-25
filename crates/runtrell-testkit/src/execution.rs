// SPDX-License-Identifier: Apache-2.0
//! Test-only lifecycle simulator. Never executes the request's program.
use runtrell::{execution::*, *};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
struct Entry {
    request: ExecutionRequest,
    observation: Observation,
}
/// Simulated external inventory, independent of a backend client's lifetime.
#[derive(Clone, Default)]
pub struct FakeExecutionRegistry(Arc<Mutex<BTreeMap<String, Entry>>>);

#[derive(Clone, Default)]
pub struct FakeExecutionBackend {
    pub registry: FakeExecutionRegistry,
    pub lose: Arc<AtomicBool>,
    pub delay: Arc<AtomicBool>,
    pub unknown_cost: Arc<AtomicBool>,
    pub fail_cleanup: Arc<AtomicBool>,
    pub create_late: Arc<AtomicBool>,
    pub hold_running: Arc<AtomicBool>,
}
fn key(c: &ToolContext) -> String {
    serde_json::to_string(&json!([c.claim.run, c.operation])).unwrap()
}
async fn gate(flag: &AtomicBool) {
    std::future::poll_fn(|cx| {
        if flag.load(Ordering::SeqCst) {
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await
}
impl ExecutionBackend for FakeExecutionBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            isolation: Isolation::Trusted,
            cpu: false,
            ram: false,
            pids: true,
            disk: true,
            file: true,
            network_deny: true,
            runtime_version: "testkit-fake-1".into(),
        }
    }
    async fn acquire(&self, c: &ToolContext, r: &ExecutionRequest) -> Result<ExecutionRef> {
        self.supports(r)?;
        r.validate_owner(c)?;
        gate(&self.create_late).await;
        let e = {
            let mut entries = self.registry.0.lock().map_err(|_| Error::Storage)?;
            let entry = entries.entry(key(c)).or_insert_with(|| Entry {
                request: r.clone(),
                observation: Observation {
                    execution: ExecutionRef {
                        owner: c.claim.run.clone(),
                        id: c.operation.clone(),
                        runtime_version: self.capabilities().runtime_version,
                    },
                    state: ExecutionState::Ready,
                    result: None,
                    failure: None,
                    cost_micro_usd: Some(0),
                    released_resources: Default::default(),
                },
            });
            if entry.request != *r {
                return Err(Error::Conflict);
            }
            entry.observation.execution.clone()
        };
        gate(&self.delay).await;
        if self.lose.swap(false, Ordering::SeqCst) {
            Err(Error::UsageUnknown)
        } else {
            Ok(e)
        }
    }
    async fn start(&self, c: &ToolContext, e: &ExecutionRef) -> Result<()> {
        {
            let mut entries = self.registry.0.lock().map_err(|_| Error::Storage)?;
            let entry = entries.get_mut(&key(c)).ok_or(Error::NotFound)?;
            if entry.observation.execution != *e {
                return Err(Error::PermissionDenied);
            }
            if entry.observation.state != ExecutionState::Ready {
                return Ok(());
            }
            entry.observation.state = ExecutionState::Running;
        }
        loop {
            let mut entries = self.registry.0.lock().map_err(|_| Error::Storage)?;
            let o = &mut entries.get_mut(&key(c)).ok_or(Error::NotFound)?.observation;
            if o.state != ExecutionState::Running {
                return Ok(());
            }
            if !self.hold_running.load(Ordering::SeqCst) {
                o.state = ExecutionState::Stopped;
                o.result = Some(json!({"answer":42}));
                o.released_resources
                    .extend([Resource::Cpu, Resource::Memory, Resource::Pid]);
                return Ok(());
            }
            drop(entries);
            std::thread::yield_now();
        }
    }
    async fn lookup(&self, c: &ToolContext) -> Result<Option<Observation>> {
        let mut o = self
            .registry
            .0
            .lock()
            .map_err(|_| Error::Storage)?
            .get(&key(c))
            .map(|e| e.observation.clone());
        if self.unknown_cost.load(Ordering::SeqCst)
            && let Some(o) = &mut o
        {
            o.cost_micro_usd = None;
        }
        Ok(o)
    }
    async fn cancel(&self, c: &ToolContext) -> Result<()> {
        if let Some(e) = self
            .registry
            .0
            .lock()
            .map_err(|_| Error::Storage)?
            .get_mut(&key(c))
            && matches!(
                e.observation.state,
                ExecutionState::Ready | ExecutionState::Running
            )
        {
            e.observation.state = ExecutionState::Stopped;
            e.observation.failure = Some("closed".into());
            e.observation.released_resources.extend([
                Resource::Cpu,
                Resource::Memory,
                Resource::Pid,
            ]);
        }
        Ok(())
    }
    async fn stop(&self, c: &ToolContext) -> Result<Progress> {
        self.cancel(c).await?;
        Ok(match self.lookup(c).await? {
            Some(o) if matches!(o.state, ExecutionState::Stopped | ExecutionState::Destroyed) => {
                Progress::Confirmed
            }
            _ => Progress::Unknown,
        })
    }
    async fn destroy(&self, c: &ToolContext) -> Result<Progress> {
        if self.fail_cleanup.swap(false, Ordering::SeqCst) {
            return Err(Error::Storage);
        }
        if self.stop(c).await? != Progress::Confirmed {
            return Ok(Progress::Unknown);
        }
        let mut entries = self.registry.0.lock().map_err(|_| Error::Storage)?;
        let o = &mut entries.get_mut(&key(c)).ok_or(Error::NotFound)?.observation;
        o.state = ExecutionState::Destroyed;
        o.released_resources
            .extend([Resource::Disk, Resource::Volume]);
        Ok(Progress::Confirmed)
    }
}
