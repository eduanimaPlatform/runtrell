// SPDX-License-Identifier: Apache-2.0
//! Execution receipts do not imply Run success, settlement or physical rediscovery.
use crate::*;
use runtrell::execution::{ExecutionLedger, ExecutionRecord};

impl PgStore {
    async fn execution_write(&self) -> Result<sqlx::Transaction<'_, sqlx::Postgres>> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::Executor::execute(&mut *tx, "SET LOCAL synchronous_commit=on; SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='30s'")
            .await.map_err(storage)?;
        Ok(tx)
    }
    async fn execution_row(
        &self,
        conn: &mut PgConnection,
        owner: &RunHandle,
        operation: &str,
        lock: bool,
    ) -> Result<ExecutionRecord> {
        let value = sqlx::query_scalar::<_, Value>(&format!(
            "SELECT to_jsonb(e) - 'tenant' - 'run_id' - 'reclaimed' FROM {}.executions e WHERE tenant=$1 AND run_id=$2 AND context_operation=$3{}",
            self.schema, if lock { " FOR UPDATE" } else { "" }))
            .bind(&owner.tenant).bind(&owner.id).bind(operation)
            .fetch_optional(conn).await.map_err(storage)?.ok_or(Error::NotFound)?;
        decode(value)
    }
}
impl ExecutionLedger for PgStore {
    async fn insert(&self, record: ExecutionRecord) -> Result<ExecutionRecord> {
        record.validate_intent()?;
        let mut tx = self.execution_write().await?;
        sqlx::query(&format!("INSERT INTO {}.executions(owner,context_operation,request,revision,cancel_requested,observation,resources,reclaimed) VALUES($1,$2,$3,0,false,NULL,$4,false) ON CONFLICT DO NOTHING", self.schema))
            .bind(serde_json::to_value(&record.owner).map_err(storage)?)
            .bind(&record.context_operation)
            .bind(serde_json::to_value(&record.request).map_err(storage)?)
            .bind(serde_json::to_value(&record.resources).map_err(storage)?)
            .execute(&mut *tx).await.map_err(storage)?;
        let saved = self
            .execution_row(&mut tx, &record.owner, &record.context_operation, true)
            .await?;
        if saved.request != record.request {
            return Err(Error::Conflict);
        }
        self.commit(tx).await?;
        Ok(saved)
    }
    async fn get(&self, owner: &RunHandle, operation: &str) -> Result<ExecutionRecord> {
        let mut conn = self.pool.acquire().await.map_err(storage)?;
        self.execution_row(&mut conn, owner, operation, false).await
    }
    async fn cas(&self, expected: u64, record: ExecutionRecord) -> Result<ExecutionRecord> {
        let mut tx = self.execution_write().await?;
        let saved = self
            .execution_row(&mut tx, &record.owner, &record.context_operation, true)
            .await?;
        let next = saved.advance(expected, record)?;
        sqlx::query(&format!("UPDATE {}.executions SET revision=$4::text::numeric,cancel_requested=$5,observation=$6,resources=$7,reclaimed=$8 WHERE tenant=$1 AND run_id=$2 AND context_operation=$3", self.schema))
            .bind(&next.owner.tenant).bind(&next.owner.id).bind(&next.context_operation)
            .bind(next.revision.to_string()).bind(next.cancel_requested)
            .bind(next.observation.as_ref().map(serde_json::to_value).transpose().map_err(storage)?)
            .bind(serde_json::to_value(&next.resources).map_err(storage)?).bind(next.reclaimed())
            .execute(&mut *tx).await.map_err(storage)?;
        self.commit(tx).await?;
        Ok(next)
    }
    async fn pending(&self) -> Result<Vec<ExecutionRecord>> {
        sqlx::query_scalar::<_, Value>(&format!("SELECT to_jsonb(e) - 'tenant' - 'run_id' - 'reclaimed' FROM {}.executions e WHERE NOT reclaimed ORDER BY tenant,run_id,context_operation", self.schema))
            .fetch_all(&self.pool).await.map_err(storage)?.into_iter().map(decode).collect()
    }
}
