// SPDX-License-Identifier: Apache-2.0
use crate::{PgStore, storage};
use runtrell::{
    Error, Result,
    models::{AllowanceDebit, AllowanceLedger},
};
use sqlx::PgConnection;

impl AllowanceLedger for PgStore {
    async fn consume_allowance(&self, debit: &AllowanceDebit) -> Result<u64> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| storage("PgStore::consume_allowance", e))?;
        let included = self.consume_allowance_in(&mut tx, debit, false).await?;
        self.commit(tx).await?;
        Ok(included)
    }
}

impl PgStore {
    pub(crate) async fn consume_allowance_in(
        &self,
        conn: &mut PgConnection,
        debit: &AllowanceDebit,
        require_new: bool,
    ) -> Result<u64> {
        const OP: &str = "PgStore::consume_allowance_in";
        debit.validate()?;
        let periods = format!("{}.allowance_periods", self.schema);
        let receipts = format!("{}.allowance_receipts", self.schema);
        sqlx::query(&format!(
            "INSERT INTO {periods}(allowance, scope, period, capacity) VALUES($1,$2,$3,$4::text::numeric) ON CONFLICT DO NOTHING"
        ))
        .bind(&debit.allowance).bind(&debit.scope).bind(&debit.period).bind(debit.limit.to_string())
        .execute(&mut *conn).await.map_err(|e| storage(OP, e))?;
        let (limit, used): (String, String) = sqlx::query_as(&format!(
            "SELECT capacity::text, consumed::text FROM {periods} WHERE allowance=$1 AND scope=$2 AND period=$3 FOR UPDATE"
        ))
        .bind(&debit.allowance).bind(&debit.scope).bind(&debit.period)
        .fetch_one(&mut *conn).await.map_err(|e| storage(OP, e))?;
        if limit.parse::<u64>().map_err(|_| Error::Storage)? != debit.limit {
            return Err(Error::Conflict);
        }
        let saved: Option<(String, String)> = sqlx::query_as(&format!(
            "SELECT quantity::text, included::text FROM {receipts} WHERE allowance=$1 AND scope=$2 AND period=$3 AND receipt=$4"
        ))
        .bind(&debit.allowance).bind(&debit.scope).bind(&debit.period).bind(&debit.receipt)
        .fetch_optional(&mut *conn).await.map_err(|e| storage(OP, e))?;
        if let Some((quantity, included)) = saved {
            if require_new || quantity.parse::<u64>().map_err(|_| Error::Storage)? != debit.quantity
            {
                return Err(Error::Conflict);
            }
            return included.parse().map_err(|_| Error::Storage);
        }
        let used: u64 = used.parse().map_err(|_| Error::Storage)?;
        let remaining = debit.limit.checked_sub(used).ok_or(Error::Storage)?;
        let included = debit.quantity.min(remaining);
        sqlx::query(&format!(
            "UPDATE {periods} SET consumed=consumed+$4::text::numeric WHERE allowance=$1 AND scope=$2 AND period=$3"
        ))
        .bind(&debit.allowance).bind(&debit.scope).bind(&debit.period).bind(included.to_string())
        .execute(&mut *conn).await.map_err(|e| storage(OP, e))?;
        sqlx::query(&format!(
            "INSERT INTO {receipts}(allowance,scope,period,receipt,quantity,included) VALUES($1,$2,$3,$4,$5::text::numeric,$6::text::numeric)"
        ))
        .bind(&debit.allowance).bind(&debit.scope).bind(&debit.period).bind(&debit.receipt)
        .bind(debit.quantity.to_string()).bind(included.to_string())
        .execute(&mut *conn).await.map_err(|e| storage(OP, e))?;
        Ok(included)
    }
}
