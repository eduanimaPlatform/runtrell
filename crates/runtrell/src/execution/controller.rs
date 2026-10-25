// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::data::{Access, Change};

async fn bounded<C: Clock, T>(
    clock: &C,
    deadline: u64,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    use std::{future::Future, task::Poll};
    let mut future = std::pin::pin!(future);
    let mut timer = std::pin::pin!(clock.wait_until(deadline));
    std::future::poll_fn(|cx| {
        if timer.as_mut().poll(cx) == Poll::Ready(()) {
            return Poll::Ready(Err(Error::UsageUnknown));
        }
        future.as_mut().poll(cx)
    })
    .await
}

async fn save_observation<L: ExecutionLedger>(
    ledger: &L,
    c: &ToolContext,
    o: Observation,
) -> Result<ExecutionRecord> {
    loop {
        let mut r = ledger.get(&c.claim.run, &c.operation).await?;
        let rev = r.revision;
        r.observe(o.clone());
        match ledger.cas(rev, r).await {
            Err(Error::Conflict) => continue,
            other => return other,
        }
    }
}
async fn poll_backend<L: ExecutionLedger, B: ExecutionBackend, C: Clock>(
    ledger: &L,
    backend: &B,
    c: &ToolContext,
    clock: &C,
) -> Result<Lookup> {
    let record = ledger.get(&c.claim.run, &c.operation).await?;
    if record.cancel_requested {
        let deadline = clock.now().saturating_add(record.request.limits.stop_ms);
        bounded(clock, deadline, backend.cancel(c)).await?;
        bounded(clock, deadline, backend.stop(c)).await?;
    }
    let cleanup_deadline = clock.now().saturating_add(record.request.limits.cleanup_ms);
    let Some(mut o) = bounded(clock, cleanup_deadline, backend.lookup(c)).await? else {
        return Ok(Lookup::Unknown);
    };
    if o.execution.owner != c.claim.run || o.execution.id != c.operation {
        return Err(Error::PermissionDenied);
    }
    let observed = save_observation(ledger, c, o.clone()).await?;
    let needs_cleanup = [Resource::Disk, Resource::Volume]
        .iter()
        .any(|r| !observed.resources[r].released);
    if matches!(
        o.state,
        ExecutionState::Stopped | ExecutionState::Reclaiming | ExecutionState::Destroyed
    ) && needs_cleanup
    {
        bounded(clock, cleanup_deadline, backend.destroy(c)).await?;
        o = bounded(clock, cleanup_deadline, backend.lookup(c))
            .await?
            .ok_or(Error::UsageUnknown)?;
    }
    let record = save_observation(ledger, c, o.clone()).await?;
    if !record.reclaimed() {
        return Ok(Lookup::Pending);
    }
    let o = record.observation.ok_or(Error::UsageUnknown)?;
    let usage = Usage::measured(0, 0, o.cost_micro_usd.ok_or(Error::UsageUnknown)?);
    Ok(Lookup::Found(if let Some(code) = o.failure {
        ExternalResult::Failed {
            physical_evidence: None,
            code,
            usage,
            retry_after: None,
        }
    } else {
        ExternalResult::Applied {
            physical_evidence: None,
            output: o.result.unwrap_or(Value::Null),
            usage,
            external_id: Some(o.execution.id),
        }
    }))
}
impl<
    S: ControlStore,
    M: ModelPort,
    T: Tool,
    P: Policy,
    C: Clock,
    V: Validator + 'static,
    F: Finalizer + 'static,
> Runtime<S, M, T, P, C, V, F>
{
    /// Explicit code demand only. Search/normal drive never invokes this path.
    /// Repeating the exact request reconciles the original environment, never starts a replacement.
    pub async fn execute_backend<L: ExecutionLedger, B: ExecutionBackend>(
        &self,
        c: &Claim,
        key: &str,
        request: ExecutionRequest,
        reserve: Amount,
        ledger: &L,
        backend: &B,
    ) -> Result<ExternalResult> {
        request.validate_input(&c.run)?;
        backend.supports(&request)?;
        let mut dispatch = DispatchRequest::new(
            key,
            OperationKind::Sandbox,
            &backend.capabilities().runtime_version,
            serde_json::to_value(&request).map_err(|e| Error::Invalid(e.to_string()))?,
            reserve,
        );
        // Backend owns its wall watchdog; dropping dispatch must not stop supervision.
        dispatch.timeout = None;
        let invoke_request = request.clone();
        self.dispatch(
            c,
            dispatch,
            "execute",
            |ctx| async move {
                invoke_request.validate_owner(&ctx)?;
                ledger
                    .insert(ExecutionRecord::intent(&ctx, invoke_request.clone()))
                    .await?;
                // Acquisition can finish after cancellation or after this future is dropped.
                let deadline = self
                    .clock
                    .now()
                    .saturating_add(invoke_request.limits.acquire_ms);
                let deadline = ctx.deadline.map_or(deadline, |d| d.min(deadline));
                let execution = bounded(
                    &self.clock,
                    deadline,
                    backend.acquire(&ctx, &invoke_request),
                )
                .await?;
                let r = self.store.check(c, self.clock.now(), true).await;
                let record = ledger.get(&c.run, &ctx.operation).await?;
                if r.is_err() || record.cancel_requested {
                    request_cancel(ledger, &ctx).await?;
                    let deadline = self
                        .clock
                        .now()
                        .saturating_add(invoke_request.limits.stop_ms);
                    bounded(&self.clock, deadline, backend.cancel(&ctx)).await?;
                    bounded(&self.clock, deadline, backend.stop(&ctx)).await?;
                } else {
                    let run = r?;
                    self.policy
                        .authorize(&run, &execution.runtime_version, "execute")?;
                    backend.start(&ctx, &execution).await?;
                }
                match poll_backend(ledger, backend, &ctx, &self.clock).await? {
                    Lookup::Found(result) => Ok(result),
                    _ => Ok(ExternalResult::Accepted {
                        external_id: execution.id,
                    }),
                }
            },
            |ctx| async move {
                // Discover the original acquisition; never acquire a replacement during recovery.
                match self.store.check(c, self.clock.now(), true).await {
                    Err(_) => request_cancel(ledger, &ctx).await?,
                    Ok(run) => {
                        let row = ledger.get(&c.run, &ctx.operation).await?;
                        if !row.cancel_requested
                            && let Some(o) = backend.lookup(&ctx).await?
                            && o.state == ExecutionState::Ready
                        {
                            self.policy
                                .authorize(&run, &o.execution.runtime_version, "execute")?;
                            self.store.check(c, self.clock.now(), true).await?;
                            backend.start(&ctx, &o.execution).await?;
                        }
                    }
                }
                poll_backend(ledger, backend, &ctx, &self.clock).await
            },
        )
        .await
    }
    /// Cancels the Run and tracks the selected Sandbox separately from other I/O.
    pub async fn cancel_backend<L: ExecutionLedger, B: ExecutionBackend>(
        &self,
        c: &Claim,
        key: &str,
        ledger: &L,
        backend: &B,
    ) -> Result<()> {
        let run = self.store.check(c, self.clock.now(), false).await?;
        let op = self
            .store
            .operation(&c.run, key)
            .await?
            .ok_or(Error::NotFound)?;
        if op.request.kind != OperationKind::Sandbox
            || op.request.target != backend.capabilities().runtime_version
        {
            return Err(Error::Conflict);
        }
        self.policy.authorize(&run, &op.request.target, "cancel")?;
        self.store.cancel(&c.run, "execution cancelled").await?;
        let ctx = ToolContext {
            recovery_proof: None,
            claim: c.clone(),
            principal: run.spec.principal,
            operation: op.id.clone(),
            attempt: op.request.attempt,
            deadline: None,
            permission: "cancel".into(),
            account_binding: op.request.account_binding,
        };
        // An intent can be cancelled before acquire starts; retain it for late discovery.
        if ledger.get(&c.run, &op.id).await == Err(Error::NotFound) {
            let request = serde_json::from_value(op.request.input)
                .map_err(|e| Error::Invalid(e.to_string()))?;
            ledger
                .insert(ExecutionRecord::intent(&ctx, request))
                .await?;
        }
        request_cancel(ledger, &ctx).await?;
        let row = ledger.get(&c.run, &ctx.operation).await?;
        let deadline = self.clock.now().saturating_add(row.request.limits.stop_ms);
        let mut stop = if bounded(&self.clock, deadline, backend.cancel(&ctx))
            .await
            .is_err()
        {
            Progress::Unknown
        } else {
            bounded(&self.clock, deadline, backend.stop(&ctx))
                .await
                .unwrap_or(Progress::Unknown)
        };
        let result = poll_backend(ledger, backend, &ctx, &self.clock)
            .await
            .unwrap_or(Lookup::Unknown);
        if let Lookup::Found(result) = result {
            self.store
                .record(c, &op.id, result, self.clock.now())
                .await?;
        }
        let rows = ledger.pending().await?;
        let mut other_io = false;
        for operation in self.store.list_operations(&c.run).await? {
            if !operation.settled
                && (operation.request.kind != OperationKind::Sandbox
                    || ledger.get(&c.run, &operation.id).await.is_err())
            {
                other_io = true;
            }
        }
        if other_io
            || rows.iter().any(|r| {
                r.owner == c.run
                    && [Resource::Cpu, Resource::Memory, Resource::Pid]
                        .iter()
                        .any(|k| !r.resources[k].released)
            })
        {
            stop = Progress::Unknown;
        }
        let cleanup = if other_io
            || rows.iter().any(|r| {
                r.owner == c.run
                    && r.resources
                        .iter()
                        .any(|(k, v)| *k != Resource::ExternalCost && !v.released)
            }) {
            Progress::Pending
        } else {
            Progress::Confirmed
        };
        self.store.cleanup(c, stop, cleanup, self.clock.now()).await
    }
}
pub async fn request_cancel<L: ExecutionLedger>(ledger: &L, c: &ToolContext) -> Result<()> {
    loop {
        let mut r = ledger.get(&c.claim.run, &c.operation).await?;
        let revision = r.revision;
        r.cancel_requested = true;
        match ledger.cas(revision, r).await {
            Err(Error::Conflict) => continue,
            other => return other.map(|_| ()),
        }
    }
}
/// Use this after backend stop and before declaring file results saved. CAS rejects stale copies.
/// Access is derived by the host; the request cannot grant another Run's namespace.
pub async fn merge_copy<W: WorkspaceStore>(
    store: &W,
    access: &Access,
    record: &ExecutionRecord,
    changes: Vec<Change>,
) -> Result<WorkspaceSnapshot> {
    let base = record.request.input.as_ref().ok_or(Error::NotFound)?;
    if access.actor().owner != record.owner || access.namespace() != &base.namespace {
        return Err(Error::PermissionDenied);
    }
    let stopped = record
        .observation
        .as_ref()
        .is_some_and(|o| matches!(o.state, ExecutionState::Stopped | ExecutionState::Destroyed));
    if !stopped || record.cancel_requested {
        return Err(Error::Closed);
    }
    store.merge(access, base, changes).await
}

/// Fix a Workspace version and read only its declared files through its authorized store.
/// A concurrent change invalidates the entire copy; never accept a mixed-version input.
pub async fn freeze_workspace<W: WorkspaceStore>(
    store: &W,
    access: &Access,
    request: &mut ExecutionRequest,
    chunk: usize,
) -> Result<()> {
    if chunk == 0 || chunk > 65_536 {
        return Err(Error::Invalid("COPY_CHUNK".into()));
    }
    let snapshot = store.snapshot(access).await?;
    if snapshot.files.len() > 4096
        || snapshot
            .files
            .iter()
            .try_fold(0u64, |n, f| n.checked_add(f.bytes))
            .is_none_or(|n| n > 16_777_216)
    {
        return Err(Error::Invalid("COPY_LIMIT".into()));
    }
    let mut files = BTreeMap::new();
    for file in &snapshot.files {
        let mut bytes = Vec::new();
        while (bytes.len() as u64) < file.bytes {
            let part = store
                .read(access, &file.path, file.revision, bytes.len() as u64, chunk)
                .await?;
            if part.bytes.is_empty() || part.total != file.bytes {
                return Err(Error::Conflict);
            }
            bytes.extend_from_slice(&part.bytes);
        }
        if crate::data::content_digest(&bytes) != file.hash {
            return Err(Error::Conflict);
        }
        files.insert(file.path.clone(), bytes);
    }
    if store.snapshot(access).await? != snapshot {
        return Err(Error::Conflict);
    }
    request.input = Some(snapshot);
    request.input_files = files;
    Ok(())
}
/// Import the bounded, explicit output manifest saved before physical scratch deletion.
pub async fn merge_process_copy<W: WorkspaceStore>(
    store: &W,
    access: &Access,
    record: &ExecutionRecord,
) -> Result<WorkspaceSnapshot> {
    let base = record.request.input.as_ref().ok_or(Error::NotFound)?;
    let output = record
        .observation
        .as_ref()
        .and_then(|o| o.result.as_ref())
        .and_then(|v| v.get("files"))
        .ok_or(Error::NotFound)?;
    let files: BTreeMap<String, Option<Vec<u8>>> =
        serde_json::from_value(output.clone()).map_err(|e| Error::Invalid(e.to_string()))?;
    let mut changes = Vec::new();
    for (path, bytes) in files {
        let expected = base
            .files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.revision);
        match bytes {
            Some(bytes) => changes.push(Change::Write {
                path,
                expected,
                bytes,
            }),
            None => {
                if let Some(expected) = expected {
                    changes.push(Change::Delete { path, expected });
                }
            }
        }
    }
    merge_copy(store, access, record, changes).await
}
