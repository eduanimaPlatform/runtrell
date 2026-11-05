// SPDX-License-Identifier: Apache-2.0
//! Owner-only persistence and transitions; no provider I/O.
use crate::*;
use engine::{ancestors, get, get_mut, refresh_settlement};
use persistence::{Mode, Scope};

impl PgStore {
    pub(crate) async fn save_owner(
        &self,
        conn: &mut PgConnection,
        before: &Ledger,
        state: &Ledger,
    ) -> Result<()> {
        for (key, receipt) in &state.owner_receipts {
            if let Some(old) = before.owner_receipts.get(key) {
                if old != receipt {
                    return Err(Error::Conflict);
                }
            } else {
                sqlx::query(&format!(
                    "INSERT INTO {}.owner_receipts(key,receipt) VALUES($1,$2)",
                    self.schema
                ))
                .bind(key)
                .bind(json!(receipt))
                .execute(&mut *conn)
                .await
                .map_err(|e| storage("PgStore::save_owner_receipt", e))?;
            }
        }
        Ok(())
    }
    pub(crate) async fn load_owner(
        &self,
        conn: &mut PgConnection,
        run: &RunHandle,
        command_key: &str,
        write: bool,
    ) -> Result<Ledger> {
        if write {
            // Serialize even when the key has no row yet. Lock order is command ->
            // session -> Run -> accounts, shared by both owner methods.
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("{}:owner:{command_key}", self.schema))
                .execute(&mut *conn)
                .await
                .map_err(|e| storage("PgStore::load_owner", e))?;
        }
        let receipt: Option<Value> = sqlx::query_scalar(&format!(
            "SELECT receipt FROM {}.owner_receipts WHERE key=$1",
            self.schema
        ))
        .bind(command_key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| storage("PgStore::load_owner", e))?;
        if let Some(value) = receipt {
            let mut state = Ledger::default();
            state
                .owner_receipts
                .insert(command_key.into(), decode(value)?);
            return Ok(state);
        }
        Box::pin(self.load(conn, Scope::Run(run, Mode::Work), write)).await
    }
    /// Same transition as `OperationStore::resume_held`, staged in the host's
    /// transaction. Failure rolls back to a savepoint; the host owns COMMIT.
    pub async fn resume_held_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        run: &RunHandle,
        expected_revision: u64,
        command: OwnerCommand,
        now: u64,
    ) -> Result<OwnerReceipt> {
        command.validate()?;
        self.tx_in(tx, Scope::Owner(run, &command.key.clone()), |s| {
            resume(s, run, expected_revision, command, now)
        })
        .await
    }
    /// Same settlement and receipt as `OperationStore::settle_recovered`, staged
    /// in the host's transaction. No second implementation or provider call.
    pub async fn settle_recovered_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        run: &RunHandle,
        id: &str,
        recovery: SettlementRecovery,
        command: OwnerCommand,
        now: u64,
    ) -> Result<OwnerReceipt> {
        command.validate()?;
        self.tx_in(tx, Scope::Owner(run, &command.key.clone()), |s| {
            settle(s, run, id, recovery, command, now)
        })
        .await
    }
}

fn resume(
    s: &mut Ledger,
    run: &RunHandle,
    expected_revision: u64,
    command: OwnerCommand,
    now: u64,
) -> Result<OwnerReceipt> {
    let request = OwnerRequest::Resume { expected_revision };
    if let Some(receipt) = s.owner_receipts.get(&command.key) {
        receipt.check_request(&command, run, &request)?;
        return Ok(receipt.clone());
    }
    get(s, run)?.check_resume_held(expected_revision, now)?;
    if s.operations
        .values()
        .any(|o| o.run == *run && o.awaits_operator())
    {
        return Err(Error::Conflict);
    }
    for id in ancestors(s, &get(s, run)?.spec.account)? {
        s.accounts[&id].resume_admission()?;
    }
    let r = get_mut(s, run)?;
    r.state = RunState::Queued;
    r.owner = None;
    r.revision = r.revision.checked_add(1).ok_or(Error::Conflict)?;
    let receipt = OwnerReceipt {
        command,
        run: run.clone(),
        request,
        result: OwnerResult::Resumed {
            revision: r.revision,
        },
        at: now,
    };
    s.owner_receipts
        .insert(receipt.command.key.clone(), receipt.clone());
    Ok(receipt)
}
fn settle(
    s: &mut Ledger,
    run: &RunHandle,
    id: &str,
    recovery: SettlementRecovery,
    command: OwnerCommand,
    now: u64,
) -> Result<OwnerReceipt> {
    let request = OwnerRequest::Settle {
        operation: id.into(),
        recovery: Box::new(recovery.clone()),
    };
    if let Some(receipt) = s.owner_receipts.get(&command.key) {
        receipt.check_request(&command, run, &request)?;
        return Ok(receipt.clone());
    }
    let old = s.operations.get(id).cloned().ok_or(Error::NotFound)?;
    if old.run != *run {
        return Err(Error::NotFound);
    }
    let account = get(s, run)?.spec.account.clone();
    let Some((result, record)) = old.plan_recovery(&recovery, now)? else {
        return Err(Error::Conflict);
    };
    for id in ancestors(s, &account)? {
        let a = s.accounts.get_mut(&id).unwrap();
        a.unknown = a.unknown.checked_sub(1).ok_or(Error::Conflict)?;
        a.reserved = a.reserved.subtract(old.request.reserve);
        a.used = a.used.checked_add(record.amount)?;
        a.settle_quantities(&old.request, &record.settled)?;
        if a.reached() {
            a.exhausted = true;
        }
    }
    let op = s.operations.get_mut(id).unwrap();
    op.result = Some(result);
    op.settled = true;
    op.unknown_usage = false;
    op.unsettled_result = None;
    op.usage_observations.clear();
    op.recovery = Some(Box::new(record.clone()));
    s.slots.remove(id);
    let r = get_mut(s, run)?;
    if !matches!(r.state, RunState::Terminal(_)) {
        r.state = RunState::Waiting(OPERATOR_ACTION_WAIT.into());
        r.owner = None;
        r.revision = r.revision.checked_add(1).ok_or(Error::Conflict)?;
        s.slots.remove(&format!("active:{}:{}", run.tenant, run.id));
    }
    refresh_settlement(s, run);
    let receipt = OwnerReceipt {
        command,
        run: run.clone(),
        request,
        result: OwnerResult::Settled(Box::new(record)),
        at: now,
    };
    s.owner_receipts
        .insert(receipt.command.key.clone(), receipt.clone());
    Ok(receipt)
}
