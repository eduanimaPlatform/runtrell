// SPDX-License-Identifier: Apache-2.0
//! Run-tree erasure: fence and scrub SQL first, then erase host payloads.
use crate::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunErasureState {
    Pending,
    Done,
}

impl PgStore {
    /// Read only erasure state; a missing run returns `NotFound`.
    pub async fn erasure_status(&self, run: &RunHandle) -> Result<Option<RunErasureState>> {
        let marker: Option<Option<String>> = sqlx::query_scalar(&format!(
            "SELECT erasure FROM {}.runs WHERE tenant=$1 AND id=$2",
            self.schema
        ))
        .bind(&run.tenant)
        .bind(&run.id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| storage("PgStore::erasure_status", e))?;
        match marker.ok_or(Error::NotFound)?.as_deref() {
            None => Ok(None),
            Some("pending") => Ok(Some(RunErasureState::Pending)),
            Some("done") => Ok(Some(RunErasureState::Done)),
            _ => Err(Error::Storage),
        }
    }

    /// Erase a terminal run tree. The caller authorizes the handle and supplies
    /// every run-owned payload backend through `payloads`.
    pub async fn erase_run<P: RunPayloadStore>(
        &self,
        run: &RunHandle,
        payloads: &P,
        now: u64,
    ) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| storage("PgStore::erase_run", e))?;
        sqlx::Executor::execute(&mut *tx, "SET LOCAL synchronous_commit=on; SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='30s'")
            .await.map_err(|e| storage("PgStore::erase_run", e))?;
        let rows = self.lock_tree(&mut tx, run).await?;
        if rows
            .iter()
            .all(|(_, marker)| marker.as_deref() == Some("done"))
        {
            tx.rollback()
                .await
                .map_err(|e| storage("PgStore::erase_run", e))?;
            return Ok(());
        }
        // The first attempt must validate the entire tree before changing any row.
        for (record, marker) in &rows {
            if marker.is_some() {
                continue;
            }
            if !matches!(record.state, RunState::Terminal(_))
                || record
                    .owner
                    .as_ref()
                    .is_some_and(|owner| now < owner.lease_until)
                || !matches!(record.stop, Progress::Confirmed | Progress::NotRequired)
                || !matches!(record.cleanup, Progress::Confirmed | Progress::NotRequired)
                || !matches!(
                    record.settlement,
                    Progress::Confirmed | Progress::NotRequired
                )
            {
                return Err(Error::Pending(
                    "run not settled or physically cleaned".into(),
                ));
            }
            let attempts: Vec<Value> = sqlx::query_scalar(&format!(
                "SELECT to_jsonb(a) FROM {}.attempts a WHERE tenant=$1 AND run_id=$2 ORDER BY id FOR UPDATE", self.schema
            ))
            .bind(&record.handle.tenant).bind(&record.handle.id)
            .fetch_all(&mut *tx).await.map_err(|e| storage("PgStore::erase_run", e))?;
            for value in attempts {
                let op: Operation = decode(value)?;
                if !op.settled
                    || op.unknown_usage
                    || !matches!(op.state, OperationState::Applied | OperationState::Failed)
                {
                    return Err(Error::Pending("operation not settled".into()));
                }
            }
            let unreclaimed: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS(SELECT 1 FROM {}.executions WHERE tenant=$1 AND run_id=$2 AND NOT reclaimed)", self.schema
            ))
            .bind(&record.handle.tenant).bind(&record.handle.id)
            .fetch_one(&mut *tx).await.map_err(|e| storage("PgStore::erase_run", e))?;
            if unreclaimed {
                return Err(Error::Pending("execution not reclaimed".into()));
            }
            // A terminal Run can still have a stale lifetime permit. An operation
            // permit without a settled matching attempt may represent live work.
            let slots: Vec<(String, Value)> = sqlx::query_as(&format!(
                "SELECT key,kind FROM {}.slots WHERE tenant=$1 AND run=$2 ORDER BY key FOR UPDATE",
                self.schema
            ))
            .bind(&record.handle.tenant)
            .bind(&record.handle.id)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| storage("PgStore::erase_run", e))?;
            for (key, kind) in slots {
                let kind: OperationKind = decode(kind)?;
                let safe = match kind {
                    OperationKind::Run => {
                        key == format!("active:{}:{}", record.handle.tenant, record.handle.id)
                    }
                    OperationKind::ChildRun => {
                        sqlx::query_scalar::<_, bool>(&format!(
                            "SELECT EXISTS(SELECT 1 FROM {}.attempts WHERE id=$1 AND tenant=$2 AND run_id=$3 AND settled AND request->>'kind'='ChildRun')",
                            self.schema
                        ))
                        .bind(&key)
                        .bind(&record.handle.tenant)
                        .bind(&record.handle.id)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(|e| storage("PgStore::erase_run", e))?
                    }
                    _ => false,
                };
                if !safe {
                    return Err(Error::Pending("operation slot still held".into()));
                }
            }
        }
        for (record, marker) in &rows {
            if marker.is_some() {
                continue;
            }
            let h = &record.handle;
            let spec = scrub_spec(&record.spec);
            let state = RunState::Terminal(Terminal::Cancelled("erased".into()));
            sqlx::query(&format!("UPDATE {}.runs SET erasure='pending',spec=$3,request_spec=$3,owner=NULL,state=$4,checkpoint=NULL,cancel=NULL,delivery=$5,output=NULL,receipt=NULL,completion=NULL WHERE tenant=$1 AND id=$2",self.schema))
                .bind(&h.tenant).bind(&h.id).bind(json!(spec)).bind(json!(state)).bind(json!(Delivery::None))
                .execute(&mut *tx).await.map_err(|e| storage("PgStore::erase_run", e))?;
            // Receipt identity and numeric settlement evidence stay immutable;
            // only the opaque SVG provider JSON is removed by the SQL guard.
            sqlx::query(&format!(
                "UPDATE {}.owner_receipts SET receipt={}.erase_receipt_payload(receipt) WHERE receipt->'run'->>'tenant'=$1 AND receipt->'run'->>'id'=$2 AND receipt<>{}.erase_receipt_payload(receipt)",
                self.schema, self.schema, self.schema
            ))
            .bind(&h.tenant)
            .bind(&h.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| storage("PgStore::erase_run", e))?;
            let attempts: Vec<Value> = sqlx::query_scalar(&format!(
                "SELECT to_jsonb(a) FROM {}.attempts a WHERE tenant=$1 AND run_id=$2 ORDER BY id FOR UPDATE", self.schema
            ))
            .bind(&h.tenant).bind(&h.id)
            .fetch_all(&mut *tx).await.map_err(|e| storage("PgStore::erase_run", e))?;
            for value in attempts {
                let mut op: Operation = decode(value)?;
                op.request.input = Value::Null;
                op.request.target.clear();
                op.request.version.clear();
                op.request.account_binding.clear();
                op.request.scope = None;
                op.request.recovery_proof = None;
                op.request.recovery_until = None;
                op.result = op.result.map(scrub_result);
                for usage in &mut op.usage_observations {
                    scrub_usage(usage);
                }
                if let Some(recovery) = &mut op.recovery {
                    scrub_usage(&mut recovery.previous);
                    scrub_usage(&mut recovery.settled);
                }
                op.unsettled_result = None;
                sqlx::query(&format!(
                    "UPDATE {}.attempts SET request=$2,result=$3,usage_observations=$4,recovery=$5,unsettled_result=NULL WHERE id=$1",
                    self.schema
                ))
                .bind(&op.id)
                .bind(json!(op.request))
                .bind(json!(op.result))
                .bind(json!(op.usage_observations))
                .bind(json!(op.recovery))
                .execute(&mut *tx)
                .await
                .map_err(|e| storage("PgStore::erase_run", e))?;
            }
            sqlx::query(&format!("DELETE FROM {}.late_results WHERE operation IN (SELECT id FROM {}.attempts WHERE tenant=$1 AND run_id=$2)",self.schema,self.schema))
                .bind(&h.tenant).bind(&h.id).execute(&mut *tx).await.map_err(|e| storage("PgStore::erase_run", e))?;
            sqlx::query(&format!(
                "DELETE FROM {}.executions WHERE tenant=$1 AND run_id=$2",
                self.schema
            ))
            .bind(&h.tenant)
            .bind(&h.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| storage("PgStore::erase_run", e))?;
            let slots: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT key FROM {}.slots WHERE tenant=$1 AND run=$2 ORDER BY key FOR UPDATE",
                self.schema
            ))
            .bind(&h.tenant)
            .bind(&h.id)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| storage("PgStore::erase_run", e))?;
            for key in slots {
                sqlx::query(&format!("DELETE FROM {}.slots WHERE key=$1", self.schema))
                    .bind(&key)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| storage("PgStore::erase_run", e))?;
                self.free(&mut tx, &format!("slot:{key}")).await?;
            }
            self.remove_queue(&mut tx, &engine::key(h)).await?;
        }
        self.commit(tx).await?;

        for (record, _) in &rows {
            payloads.erase_payloads(&record.handle).await?;
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| storage("PgStore::erase_run", e))?;
        sqlx::Executor::execute(&mut *tx, "SET LOCAL synchronous_commit=on; SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='30s'")
            .await.map_err(|e| storage("PgStore::erase_run", e))?;
        // Re-lock the retained tree before finalizing; parent locks fence child admission.
        let final_rows = self.lock_tree(&mut tx, run).await?;
        if final_rows.iter().any(|(_, marker)| marker.is_none()) {
            return Err(Error::Pending(
                "run tree changed during payload erasure".into(),
            ));
        }
        for (record, marker) in final_rows {
            if marker.as_deref() == Some("pending") {
                sqlx::query(&format!(
                    "UPDATE {}.runs SET erasure='done' WHERE tenant=$1 AND id=$2",
                    self.schema
                ))
                .bind(&record.handle.tenant)
                .bind(&record.handle.id)
                .execute(&mut *tx)
                .await
                .map_err(|e| storage("PgStore::erase_run", e))?;
            }
        }
        self.commit(tx).await
    }

    async fn lock_tree(
        &self,
        conn: &mut PgConnection,
        root: &RunHandle,
    ) -> Result<Vec<(RunRecord, Option<String>)>> {
        let mut pending = VecDeque::from([root.clone()]);
        let mut rows = Vec::new();
        while let Some(h) = pending.pop_front() {
            let value: Value = sqlx::query_scalar(&format!(
                "SELECT to_jsonb(r) FROM {}.runs r WHERE tenant=$1 AND id=$2 FOR UPDATE",
                self.schema
            ))
            .bind(&h.tenant)
            .bind(&h.id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| storage("PgStore::lock_tree", e))?
            .ok_or(Error::NotFound)?;
            let marker: Option<String> = decode(value["erasure"].clone())?;
            let record: RunRecord = decode(value)?;
            let children: Vec<Value> = sqlx::query_scalar(&format!(
                "SELECT handle FROM {}.runs WHERE parent_tenant=$1 AND parent_id=$2 ORDER BY tenant,id", self.schema
            ))
            .bind(&h.tenant).bind(&h.id)
            .fetch_all(&mut *conn).await.map_err(|e| storage("PgStore::lock_tree", e))?;
            for child in children {
                pending.push_back(decode(child)?);
            }
            rows.push((record, marker));
        }
        Ok(rows)
    }
}

fn scrub_spec(spec: &RunSpec) -> RunSpec {
    let mut clean = RunSpec::new(&spec.tenant, &spec.request_key, spec.account.clone(), "");
    clean.principal = spec.principal.clone();
    clean.parent = spec.parent.clone();
    clean.session = spec.session.clone();
    clean.config_version.clear();
    clean.purpose.clear();
    clean.validator.clear();
    clean.validator_version.clear();
    clean.continuation.instruction.clear();
    clean
}

fn scrub_usage(usage: &mut Usage) {
    if let Some(svg) = &mut usage.svg_usage {
        let reported = &svg.reported;
        svg.reported = if reported.is_object() {
            let mut clean = serde_json::Map::new();
            clean.insert(
                "model".into(),
                match (&svg.model, reported.get("model")) {
                    (Some(model), Some(Value::String(actual))) if actual == model => json!(model),
                    _ => Value::Bool(false),
                },
            );
            for field in [
                "input_tokens",
                "output_tokens",
                "reasoning_tokens",
                "total_tokens",
            ] {
                let value = match reported.get(field) {
                    None | Some(Value::Null) => Value::Null,
                    Some(value) if value.as_u64().is_some() => value.clone(),
                    _ => Value::Bool(false),
                };
                clean.insert(field.into(), value);
            }
            Value::Object(clean)
        } else {
            Value::Null
        };
    }
}

fn scrub_result(result: ExternalResult) -> ExternalResult {
    match result {
        ExternalResult::Applied { mut usage, .. } => {
            scrub_usage(&mut usage);
            ExternalResult::Applied {
                physical_evidence: None,
                output: Value::Null,
                usage,
                external_id: None,
            }
        }
        ExternalResult::Failed { mut usage, .. } => {
            scrub_usage(&mut usage);
            ExternalResult::Failed {
                physical_evidence: None,
                code: "erased".into(),
                usage,
                retry_after: None,
                repair_hint: None,
                provider_diagnostic: None,
            }
        }
        _ => ExternalResult::Unknown,
    }
}
