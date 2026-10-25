// SPDX-License-Identifier: Apache-2.0
// State transitions adapted from runtrell 2a MemoryStore; persistence is PostgreSQL.
use crate::persistence::{Mode, Scope};
use crate::*;
impl PgStore {
    /// Test fixture only: updates evidence revision without changing a completed result.
    pub async fn revise_basis(&self, run: &RunHandle, expected: u64, new: u64) -> Result<()> {
        self.tx(Scope::Run(run, Mode::Bare), |s| {
            let r = get_mut(s, run)?;
            if r.spec.basis_revision != expected {
                return Err(Error::Conflict);
            }
            r.spec.basis_revision = new;
            r.revision += 1;
            Ok(())
        })
        .await
    }
    /// Test inspection of the independently recorded lifetime states.
    pub async fn operations(&self, run: &RunHandle) -> Result<Vec<Operation>> {
        self.read(Scope::Run(run, Mode::Ops), |s| {
            get(s, run)?;
            Ok(s.operations
                .values()
                .filter(|o| o.run == *run)
                .cloned()
                .collect())
        })
        .await
    }
}
pub(crate) fn key(h: &RunHandle) -> String {
    format!("{}:{}", h.tenant, h.id)
}
pub(crate) fn session_key(tenant: &str, id: &str) -> String {
    format!("{}:{tenant}{id}", tenant.len())
}
pub(crate) fn get<'a>(s: &'a Ledger, h: &RunHandle) -> Result<&'a RunRecord> {
    s.runs
        .get(&key(h))
        .filter(|r| r.handle == *h)
        .ok_or(Error::NotFound)
}
fn get_mut<'a>(s: &'a mut Ledger, h: &RunHandle) -> Result<&'a mut RunRecord> {
    s.runs
        .get_mut(&key(h))
        .filter(|r| r.handle == *h)
        .ok_or(Error::NotFound)
}
fn fence(s: &Ledger, c: &Claim, now: u64, new_work: bool) -> Result<RunRecord> {
    let r = get(s, &c.run)?;
    if !r.owner.as_ref().is_some_and(|owner| {
        owner.worker == c.worker && owner.generation == c.generation && now < owner.lease_until
    }) {
        return Err(Error::OwnershipLost);
    }
    if r.schema != 1 {
        return Err(Error::Unsupported("run schema".into()));
    }
    if new_work {
        if r.cancel.is_some() || matches!(r.state, RunState::Terminal(_)) {
            return Err(Error::Closed);
        }
        if r.spec.deadline.is_some_and(|d| now >= d) {
            return Err(Error::Deadline);
        }
        if let Some(session) = &r.spec.session {
            let current = s
                .sessions
                .get(&session_key(&r.spec.tenant, session))
                .ok_or(Error::Conflict)?;
            if current.1 != r.spec.context_revision {
                return Err(Error::Invalid("CONTEXT_STALE".into()));
            }
            if s.runs.values().any(|prior| {
                prior.spec.tenant == r.spec.tenant
                    && prior.spec.session.as_ref() == Some(session)
                    && prior.sequence < r.sequence
                    && !matches!(prior.state, RunState::Terminal(_))
            }) {
                return Err(Error::Pending("session predecessor".into()));
            }
        }
    }
    Ok(r.clone())
}
pub(crate) fn ancestors(s: &Ledger, id: &str) -> Result<Vec<String>> {
    let mut out = vec![];
    let mut at = Some(id.to_owned());
    while let Some(id) = at {
        if out.contains(&id) {
            return Err(Error::Conflict);
        }
        let a = s.accounts.get(&id).ok_or(Error::NotFound)?;
        at = a.parent.clone();
        out.push(id);
    }
    Ok(out)
}
fn budget_open(s: &Ledger, id: &str) -> Result<()> {
    for id in ancestors(s, id)? {
        let a = &s.accounts[&id];
        if a.unknown > 0 {
            return Err(Error::UsageUnknown);
        }
        if a.exhausted || a.used.reaches(a.limit) {
            return Err(Error::BudgetAtLimit);
        }
    }
    Ok(())
}
fn acquire(s: &mut Ledger, req: SlotRequest) -> Result<()> {
    if let Some(old) = s.slots.get(&req.key) {
        return if old == &req {
            Ok(())
        } else {
            Err(Error::Conflict)
        };
    }
    s.slots.insert(req.key.clone(), req);
    Ok(())
}
fn refresh_settlement(s: &mut Ledger, h: &RunHandle) {
    let pending = s.operations.values().any(|o| o.run == *h && !o.settled);
    let unknown = s
        .operations
        .values()
        .any(|o| o.run == *h && o.unknown_usage);
    if let Ok(r) = get_mut(s, h) {
        r.settlement = if unknown {
            Progress::Unknown
        } else if pending {
            Progress::Pending
        } else {
            Progress::Confirmed
        };
    }
}
impl RunStore for PgStore {
    async fn renew(&self, c: &Claim, now: u64, lease: u64) -> Result<Claim> {
        self.tx(Scope::Run(&c.run, Mode::Bare), |s| {
            fence(s, c, now, false)?;
            if lease == 0 {
                return Err(Error::Invalid("zero lease".into()));
            }
            let r = get_mut(s, &c.run)?;
            let owner = r.owner.as_mut().ok_or(Error::OwnershipLost)?;
            owner.lease_until = owner
                .lease_until
                .max(now.checked_add(lease).ok_or(Error::Conflict)?);
            let renewed = owner.clone();
            r.revision += 1;
            Ok(renewed)
        })
        .await
    }
    async fn find_request(&self, spec: &RunSpec) -> Result<Option<RunHandle>> {
        self.read(Scope::Request(spec), |s| {
            let old = s.runs.values().find(|r| {
                r.spec.tenant == spec.tenant
                    && r.spec.principal == spec.principal
                    && r.spec.request_key == spec.request_key
            });
            match old {
                Some(r) if r.spec == *spec => Ok(Some(r.handle.clone())),
                Some(_) => Err(Error::Conflict),
                None => Ok(None),
            }
        })
        .await
    }
    async fn start(&self, mut spec: RunSpec) -> Result<RunHandle> {
        self.tx(Scope::Start(&spec.clone()), |s| {
            if spec.tenant.is_empty() || spec.request_key.is_empty() {
                return Err(Error::Invalid("tenant/request key".into()));
            }
            if let Some(old) = s.runs.values().find(|r| {
                r.spec.tenant == spec.tenant
                    && r.spec.principal == spec.principal
                    && r.spec.request_key == spec.request_key
            }) {
                return if old.spec == spec {
                    Ok(old.handle.clone())
                } else {
                    Err(Error::Conflict)
                };
            }
            let a = s.accounts.get(&spec.account).ok_or(Error::NotFound)?;
            if a.tenant != spec.tenant {
                return Err(Error::PermissionDenied);
            }
            if let Some(parent) = &spec.parent {
                let p = get(s, parent)?;
                if p.handle.tenant != spec.tenant
                    || p.cancel.is_some()
                    || matches!(p.state, RunState::Terminal(_))
                {
                    return Err(Error::Closed);
                }
                if spec.account != p.spec.account
                    && !ancestors(s, &spec.account)?.contains(&p.spec.account)
                {
                    return Err(Error::PermissionDenied);
                }
                spec.deadline = match (spec.deadline, p.spec.deadline) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            }
            let sequence = if let Some(id) = &spec.session {
                let session = s.sessions.entry(session_key(&spec.tenant, id)).or_default();
                if session.1 != spec.context_revision {
                    return Err(Error::Conflict);
                }
                session.0 += 1;
                session.0
            } else {
                0
            };
            let h = RunHandle {
                tenant: spec.tenant.clone(),
                id: uuid::Uuid::new_v4().to_string(),
            };
            s.queue.push_back(QueueEntry {
                key: key(&h),
                tenant: spec.tenant.clone(),
                low_latency: spec.purpose == "reply",
                deadline: spec.deadline.unwrap_or(u64::MAX),
            });
            s.runs.insert(
                key(&h),
                RunRecord {
                    handle: h.clone(),
                    spec,
                    schema: 1,
                    revision: 0,
                    sequence,
                    owner: None,
                    generation: 0,
                    state: RunState::Queued,
                    checkpoint: None,
                    cancel: None,
                    stop: Progress::NotRequired,
                    cleanup: Progress::NotRequired,
                    settlement: Progress::NotRequired,
                    delivery: Delivery::None,
                    output: None,
                    receipt: None,
                    completion: None,
                },
            );
            Ok(h)
        })
        .await
    }
    async fn get(&self, h: &RunHandle) -> Result<RunRecord> {
        self.read(Scope::Run(h, Mode::Bare), |s| Ok(get(s, h)?.clone()))
            .await
    }
    async fn claim(&self, h: &RunHandle, worker: &str, now: u64, lease: u64) -> Result<Claim> {
        self.tx(Scope::Run(h, Mode::Claim), |s| {
            if let Some(i) = s.queue.iter().position(|e| e.key == key(h)) {
                s.queue.remove(i);
            }
            acquire(
                s,
                SlotRequest {
                    key: format!("active:{}:{}", h.tenant, h.id),
                    tenant: h.tenant.clone(),
                    run: h.id.clone(),
                    kind: OperationKind::Run,
                },
            )?;
            let r = get_mut(s, h)?;
            if lease == 0 {
                return Err(Error::Invalid("zero lease".into()));
            }
            if r.owner.as_ref().is_some_and(|c| now < c.lease_until) {
                return Err(Error::OwnershipLost);
            }
            r.generation = r.generation.checked_add(1).ok_or(Error::Conflict)?;
            let c = Claim {
                run: h.clone(),
                worker: worker.into(),
                generation: r.generation,
                lease_until: now.checked_add(lease).ok_or(Error::Conflict)?,
            };
            r.owner = Some(c.clone());
            if !matches!(r.state, RunState::Terminal(_)) {
                r.state = RunState::Running;
            }
            r.revision += 1;
            Ok(c)
        })
        .await
    }
    async fn check(&self, c: &Claim, now: u64, new: bool) -> Result<RunRecord> {
        self.read(Scope::Run(&c.run, Mode::Fence), |s| fence(s, c, now, new))
            .await
    }
    async fn checkpoint(&self, c: &Claim, expected: u64, cp: Checkpoint, now: u64) -> Result<()> {
        self.tx(Scope::Run(&c.run, Mode::Fence), |s| {
            fence(s, c, now, true)?;
            let r = get_mut(s, &c.run)?;
            if cp.schema != 1 {
                return Err(Error::Unsupported("checkpoint schema".into()));
            }
            if r.revision != expected {
                return Err(Error::Conflict);
            }
            r.checkpoint = Some(cp);
            r.revision += 1;
            Ok(())
        })
        .await
    }
    async fn cancel(&self, h: &RunHandle, reason: &str) -> Result<()> {
        self.tx(Scope::Run(h, Mode::Tree), |s| {
            if matches!(get(s, h)?.state, RunState::Terminal(Terminal::Cancelled(_))) {
                return Ok(());
            }
            if matches!(get(s, h)?.state, RunState::Terminal(_)) {
                return Err(Error::Closed);
            }
            let mut pending = vec![h.clone()];
            while let Some(h) = pending.pop() {
                pending.extend(
                    s.runs
                        .values()
                        .filter(|r| {
                            r.spec.parent.as_ref() == Some(&h)
                                && !matches!(r.state, RunState::Terminal(_))
                        })
                        .map(|r| r.handle.clone()),
                );
                let has_ops = s.operations.values().any(|o| o.run == h && !o.settled);
                let r = get_mut(s, &h)?;
                r.cancel = Some(reason.into());
                r.state = RunState::Terminal(Terminal::Cancelled(reason.into()));
                r.stop = if has_ops {
                    Progress::Pending
                } else {
                    Progress::NotRequired
                };
                r.cleanup = if has_ops {
                    Progress::Pending
                } else {
                    Progress::NotRequired
                };
                r.revision += 1;
                s.queue.retain(|e| e.key != key(&h));
            }
            Ok(())
        })
        .await
    }
    async fn terminate(&self, c: &Claim, t: Terminal, now: u64) -> Result<()> {
        self.tx(
            Scope::Run(
                &c.run,
                if matches!(t, Terminal::DeadlineReached | Terminal::BudgetReached(_)) {
                    Mode::Tree
                } else {
                    Mode::Close
                },
            ),
            |s| {
                fence(s, c, now, false)?;
                if t == Terminal::Succeeded {
                    return Err(Error::Invalid("requires finalizer".into()));
                }
                let cascade = matches!(t, Terminal::DeadlineReached | Terminal::BudgetReached(_));
                let mut pending = vec![c.run.clone()];
                while let Some(h) = pending.pop() {
                    if cascade {
                        pending.extend(
                            s.runs
                                .values()
                                .filter(|r| {
                                    r.spec.parent.as_ref() == Some(&h)
                                        && !matches!(r.state, RunState::Terminal(_))
                                })
                                .map(|r| r.handle.clone()),
                        );
                    }
                    let outstanding = s.operations.values().any(|op| op.run == h && !op.settled);
                    let r = get_mut(s, &h)?;
                    if !matches!(r.state, RunState::Terminal(_)) {
                        r.state = RunState::Terminal(t.clone());
                        r.revision += 1;
                    }
                    if outstanding {
                        r.stop = Progress::Pending;
                        r.cleanup = Progress::Pending;
                    }
                    s.queue.retain(|e| e.key != key(&h));
                }
                Ok(())
            },
        )
        .await
    }
    async fn finalize(&self, c: &Claim, completion: Completion, now: u64) -> Result<()> {
        self.tx(Scope::Run(&c.run, Mode::Finalize), |s| {
            let r = fence(s, c, now, false)?;
            if r.completion.as_ref() == Some(&completion.id)
                && r.output.as_ref() == Some(&completion.output)
                && r.receipt.as_ref() == Some(&completion.receipt)
            {
                return Ok(());
            }
            let r = fence(s, c, now, true)?;
            budget_open(s, &r.spec.account)?;
            if s.operations.values().any(|o| {
                o.run == c.run
                    && (!o.settled
                        || matches!(o.state, OperationState::Unknown | OperationState::Accepted)
                        || (o.state == OperationState::Failed
                            && !s.operations.values().any(|later| {
                                later.run == o.run
                                    && later.request.key == o.request.key
                                    && later.request.attempt > o.request.attempt
                            })))
            }) {
                return Err(Error::UsageUnknown);
            }
            let v = &completion.receipt;
            if !v.passed
                || v.hash != digest(&completion.output)
                || v.basis_revision != r.spec.basis_revision
                || v.config_version != r.spec.config_version
                || v.validator != r.spec.validator
                || v.validator_version != r.spec.validator_version
            {
                return Err(Error::Conflict);
            }
            if let Some(id) = &r.spec.session {
                let session = s
                    .sessions
                    .get_mut(&session_key(&r.spec.tenant, id))
                    .ok_or(Error::Conflict)?;
                if session.1 != r.spec.context_revision {
                    return Err(Error::Conflict);
                }
                session.1 += 1;
            }
            let r = get_mut(s, &c.run)?;
            r.state = RunState::Terminal(Terminal::Succeeded);
            r.output = Some(completion.output);
            r.receipt = Some(completion.receipt);
            r.completion = Some(completion.id);
            r.revision += 1;
            Ok(())
        })
        .await
    }
    async fn wait(&self, c: &Claim, reason: &str, now: u64) -> Result<()> {
        self.tx(Scope::Run(&c.run, Mode::Bare), |s| {
            let r = fence(s, c, now, false)?;
            if matches!(r.state, RunState::Terminal(_)) {
                return Err(Error::Closed);
            }
            let r = get_mut(s, &c.run)?;
            r.state = RunState::Waiting(reason.into());
            r.revision += 1;
            Ok(())
        })
        .await
    }
    async fn cleanup(&self, c: &Claim, stop: Progress, cleanup: Progress, now: u64) -> Result<()> {
        self.tx(Scope::Run(&c.run, Mode::Bare), |s| {
            fence(s, c, now, false)?;
            let r = get_mut(s, &c.run)?;
            r.stop = stop;
            r.cleanup = cleanup;
            r.revision += 1;
            Ok(())
        })
        .await
    }
}
impl BudgetAuthority for PgStore {
    async fn exhaust(&self, c: &Claim, now: u64) -> Result<()> {
        self.tx(Scope::Run(&c.run, Mode::Budget), |s| {
            let r = fence(s, c, now, false)?;
            s.accounts
                .get_mut(&r.spec.account)
                .ok_or(Error::NotFound)?
                .exhausted = true;
            Ok(())
        })
        .await
    }
    async fn create_account(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
    ) -> Result<String> {
        self.tx(Scope::Account(parent.clone().as_deref()), |s| {
            if limit.tokens == 0 || limit.micro_usd == 0 {
                return Err(Error::Invalid("zero budget".into()));
            }
            if let Some(id) = &parent {
                let a = s.accounts.get(id).ok_or(Error::NotFound)?;
                if a.tenant != tenant && a.tenant != "*" {
                    return Err(Error::PermissionDenied);
                }
            }
            let id = uuid::Uuid::new_v4().to_string();
            s.accounts.insert(
                id.clone(),
                Account {
                    id: id.clone(),
                    tenant: tenant.into(),
                    parent,
                    limit,
                    reserved: Amount::default(),
                    used: Amount::default(),
                    unknown: 0,
                    exhausted: false,
                    warnings: [0, 0],
                },
            );
            Ok(id)
        })
        .await
    }
    async fn account(&self, id: &str) -> Result<Account> {
        self.read(Scope::Account(Some(id)), |s| {
            s.accounts.get(id).cloned().ok_or(Error::NotFound)
        })
        .await
    }
    async fn remaining(&self, c: &Claim, now: u64) -> Result<Amount> {
        self.read(Scope::Run(&c.run, Mode::Budget), |s| {
            let r = fence(s, c, now, true)?;
            budget_open(s, &r.spec.account)?;
            let mut remaining = Amount::new(u64::MAX, u64::MAX);
            for id in ancestors(s, &r.spec.account)? {
                let a = &s.accounts[&id];
                let total = a.used.checked_add(a.reserved)?;
                let left = Amount::new(
                    a.limit.tokens.saturating_sub(total.tokens),
                    a.limit.micro_usd.saturating_sub(total.micro_usd),
                );
                remaining.tokens = remaining.tokens.min(left.tokens);
                remaining.micro_usd = remaining.micro_usd.min(left.micro_usd);
            }
            Ok(remaining)
        })
        .await
    }
    async fn warnings(&self, c: &Claim, now: u64) -> Result<Vec<Warning>> {
        self.tx(Scope::Run(&c.run, Mode::Budget), |s| {
            let r = fence(s, c, now, true)?;
            let mut out = vec![];
            for id in ancestors(s, &r.spec.account)? {
                let a = s.accounts.get_mut(&id).unwrap();
                let mut reached = [None, None];
                for (axis, (used, limit)) in [
                    (a.used.tokens, a.limit.tokens),
                    (a.used.micro_usd, a.limit.micro_usd),
                ]
                .into_iter()
                .enumerate()
                {
                    for threshold in [50, 80, 95] {
                        if u128::from(used) * 100 >= u128::from(limit) * u128::from(threshold)
                            && threshold > a.warnings[axis]
                        {
                            reached[axis] = Some(threshold);
                        }
                    }
                    if let Some(t) = reached[axis] {
                        a.warnings[axis] = t;
                    }
                }
                if reached.iter().any(Option::is_some) {
                    out.push(Warning {
                        account: id,
                        token_percent: reached[0],
                        cost_percent: reached[1],
                        used: a.used,
                        limit: a.limit,
                    });
                }
            }
            if !out.is_empty() {
                let run = get_mut(s, &c.run)?;
                let cp = run.checkpoint.get_or_insert(Checkpoint {
                    schema: 1,
                    revision: 0,
                    payload: Value::Null,
                    messages: vec![],
                    cursor: 0,
                });
                for w in &out {
                    cp.messages.push(Message::text(
                        "system",
                        format!(
                            "budget {} tokens {:?} cost {:?}; used {:?}, limit {:?}",
                            w.account, w.token_percent, w.cost_percent, w.used, w.limit
                        ),
                    ));
                }
                run.revision += 1;
            }
            Ok(out)
        })
        .await
    }
}
impl OperationStore for PgStore {
    async fn observe_result(
        &self,
        c: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> Result<()> {
        let observation = result.clone();
        let status = self.tx(Scope::Attempt(&c.run, id, false), |s| {
            fence(s, c, now, false)?;
            let op = s.operations.get_mut(id).ok_or(Error::NotFound)?;
            if op.run != c.run {
                return Err(Error::PermissionDenied);
            }
            if let ExternalResult::Applied { usage, .. } | ExternalResult::Failed { usage, .. } =
                &result
                && !op.usage_observations.contains(usage)
            {
                op.usage_observations.push(usage.clone());
            }
            op.unsettled_result = Some(result);
            Ok(())
        }).await;
        if status == Err(Error::OwnershipLost) {
            self.retain_late_result(c, id, &observation, now).await?;
        }
        status
    }
    async fn proven_unsent(&self, c: &Claim, id: &str, scope: &str, now: u64) -> Result<()> {
        self.tx(Scope::Attempt(&c.run, id, false), |s| {
            fence(s, c, now, true)?;
            let op = s.operations.get_mut(id).ok_or(Error::NotFound)?;
            if op.run != c.run {
                return Err(Error::PermissionDenied);
            }
            if op.request.scope.as_deref() != Some(scope)
                || !op.request.recovery_until.is_some_and(|t| now < t)
            {
                return Err(Error::UsageUnknown);
            }
            if op.settled {
                return Err(Error::Conflict);
            }
            op.state = OperationState::Intent;
            op.send_generation = None;
            op.replay_proof_generation = Some(c.generation);
            Ok(())
        })
        .await
    }
    async fn begin(&self, c: &Claim, request: DispatchRequest, now: u64) -> Result<Operation> {
        // Reservation denial exhausts the request account; atomic intent itself rolls back.
        let result = self
            .tx(Scope::Begin(&c.run, &request.clone()), |s| {
                let r = fence(s, c, now, true)?;
                let id = format!("{}:{}:{}", key(&c.run), request.key, request.attempt);
                if let Some(op) = s.operations.get(&id) {
                    return if op.request == request {
                        Ok(op.clone())
                    } else {
                        Err(Error::Conflict)
                    };
                }
                if request.attempt == 0 {
                    return Err(Error::Invalid("attempt starts at one".into()));
                }
                if request.attempt > 1 {
                    let previous = s
                        .operations
                        .values()
                        .find(|op| {
                            op.run == c.run
                                && op.request.key == request.key
                                && op.request.attempt == request.attempt - 1
                        })
                        .ok_or(Error::Conflict)?;
                    if !previous.settled
                        || !matches!(
                            previous.result,
                            Some(ExternalResult::Failed {
                                retry_after: Some(_),
                                ..
                            })
                        )
                        || previous.request.target != request.target
                        || previous.request.version != request.version
                        || previous.request.account_binding != request.account_binding
                    {
                        return Err(Error::Conflict);
                    }
                }
                budget_open(s, &r.spec.account)?;
                let accounts = ancestors(s, &r.spec.account)?;
                for id in &accounts {
                    let a = &s.accounts[id];
                    if a.used
                        .checked_add(a.reserved)?
                        .checked_add(request.reserve)?
                        .exceeds(a.limit)
                    {
                        return Err(Error::ReservationDenied);
                    }
                }
                acquire(
                    s,
                    SlotRequest {
                        key: id.clone(),
                        tenant: c.run.tenant.clone(),
                        run: c.run.id.clone(),
                        kind: request.kind,
                    },
                )?;
                for id in accounts {
                    let a = s.accounts.get_mut(&id).unwrap();
                    a.reserved = a.reserved.checked_add(request.reserve)?;
                }
                let op = Operation {
                    id: id.clone(),
                    run: c.run.clone(),
                    request,
                    state: OperationState::Intent,
                    result: None,
                    settled: false,
                    unknown_usage: false,
                    send_generation: None,
                    send_count: 0,
                    replay_proof_generation: None,
                    created_at: now,
                    finished_at: None,
                    usage_observations: vec![],
                    unsettled_result: None,
                };
                s.operations.insert(id, op.clone());
                get_mut(s, &c.run)?.settlement = Progress::Pending;
                Ok(op)
            })
            .await;
        if result == Err(Error::ReservationDenied) {
            self.tx(Scope::Run(&c.run, Mode::Budget), |s| {
                let r = fence(s, c, now, true)?;
                s.accounts
                    .get_mut(&r.spec.account)
                    .ok_or(Error::NotFound)?
                    .exhausted = true;
                Ok(())
            })
            .await?;
        }
        result
    }
    async fn list_operations(&self, h: &RunHandle) -> Result<Vec<Operation>> {
        self.operations(h).await
    }
    async fn operation(&self, h: &RunHandle, k: &str) -> Result<Option<Operation>> {
        self.read(Scope::Logical(h, k), |s| {
            get(s, h)?;
            Ok(s.operations
                .values()
                .filter(|o| o.run == *h && o.request.key == k)
                .max_by_key(|o| o.request.attempt)
                .cloned())
        })
        .await
    }
    async fn mark_sent(&self, c: &Claim, id: &str, now: u64) -> Result<()> {
        self.tx(Scope::Attempt(&c.run, id, false), |s| {
            fence(s, c, now, true)?;
            let op = s.operations.get_mut(id).ok_or(Error::NotFound)?;
            if op.run != c.run {
                return Err(Error::PermissionDenied);
            }
            if op.state != OperationState::Intent {
                return Err(Error::Conflict);
            }
            if op.send_count > 0 {
                if op.replay_proof_generation != Some(c.generation) {
                    return Err(Error::OwnershipLost);
                }
                if !op.request.recovery_until.is_some_and(|until| now < until) {
                    return Err(Error::UsageUnknown);
                }
            }
            op.state = OperationState::Sent;
            op.send_generation = Some(c.generation);
            op.send_count = op.send_count.checked_add(1).ok_or(Error::Conflict)?;
            Ok(())
        })
        .await
    }
    async fn record(&self, c: &Claim, id: &str, result: ExternalResult, now: u64) -> Result<()> {
        let observation = result.clone();
        let status = self
            .tx(Scope::Attempt(&c.run, id, true), |s| {
                let r = fence(s, c, now, false)?;
                let old = s.operations.get(id).cloned().ok_or(Error::NotFound)?;
                if old.run != c.run {
                    return Err(Error::PermissionDenied);
                }
                if old.result.as_ref() == Some(&result) {
                    return Ok(());
                }
                if old.settled {
                    return Err(Error::Conflict);
                }
                let usage = match &result {
                    ExternalResult::Applied { usage, .. }
                    | ExternalResult::Failed { usage, .. } => Some(usage),
                    _ => None,
                };
                let amount = usage.map(Usage::amount).transpose()?;
                if let (Some(price), Some(usage)) = (&old.request.price, usage)
                    && (price.model != usage.model || Some(price.cost(usage)?) != usage.micro_usd)
                {
                    return Err(Error::Invalid("actual price/model mismatch".into()));
                }
                let state = match &result {
                    ExternalResult::Applied { .. } => OperationState::Applied,
                    ExternalResult::Accepted { .. } => OperationState::Accepted,
                    ExternalResult::Failed { .. } => OperationState::Failed,
                    ExternalResult::Unknown => OperationState::Unknown,
                };
                let unknown = matches!(result, ExternalResult::Unknown)
                    || (amount.is_none() && old.request.reserve != Amount::default());
                for id in ancestors(s, &r.spec.account)? {
                    let a = s.accounts.get_mut(&id).unwrap();
                    if old.unknown_usage {
                        a.unknown -= 1;
                    }
                    if let Some(actual) = amount {
                        a.reserved = a.reserved.subtract(old.request.reserve);
                        a.used = a.used.checked_add(actual)?;
                        if a.used.reaches(a.limit) {
                            a.exhausted = true;
                        }
                    } else if unknown {
                        a.unknown += 1;
                    }
                }
                let op = s.operations.get_mut(id).unwrap();
                op.state = state;
                op.result = Some(result.clone());
                op.settled = amount.is_some();
                op.unknown_usage = unknown;
                op.finished_at = Some(now);
                if amount.is_some() {
                    s.slots.remove(id);
                }
                if let ExternalResult::Accepted { external_id } = result {
                    get_mut(s, &c.run)?.delivery = Delivery::Accepted(external_id);
                } else if let ExternalResult::Applied {
                    external_id: Some(id),
                    ..
                } = result
                {
                    get_mut(s, &c.run)?.delivery = Delivery::Confirmed(id);
                }
                refresh_settlement(s, &c.run);
                Ok(())
            })
            .await;
        if status == Err(Error::OwnershipLost) {
            self.retain_late_result(c, id, &observation, now).await?;
        }
        status
    }
}
impl Scheduler for PgStore {
    async fn acquire(&self, r: SlotRequest) -> Result<()> {
        self.tx(Scope::Slot(&r.key.clone()), |s| acquire(s, r))
            .await
    }
    async fn release_slot(&self, key: &str) -> Result<()> {
        self.tx(Scope::Slot(key), |s| {
            if s.operations.get(key).is_some_and(|o| !o.settled) {
                return Err(Error::UsageUnknown);
            }
            s.slots.remove(key);
            Ok(())
        })
        .await
    }
    async fn enqueue(&self, e: QueueEntry) -> Result<()> {
        self.tx(Scope::Empty, |s| {
            s.queue.push_back(e);
            Ok(())
        })
        .await
    }
    async fn dequeue(&self, now: u64) -> Result<Option<QueueEntry>> {
        self.take_queue(now).await
    }
}
