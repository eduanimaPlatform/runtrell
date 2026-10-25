// SPDX-License-Identifier: Apache-2.0
#![forbid(unsafe_code)]
//! Shared durable control store. See `docs/design/postgres.md` for transaction scope.
use runtrell::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
mod engine;
mod execution;
mod persistence;

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
    schema: String,
    capacity: Capacity,
    fail_next: Arc<AtomicBool>,
}
#[derive(Clone, Default)]
struct Ledger {
    runs: BTreeMap<String, RunRecord>,
    accounts: BTreeMap<String, Account>,
    operations: BTreeMap<String, Operation>,
    sessions: BTreeMap<String, (u64, u64)>,
    slots: BTreeMap<String, SlotRequest>,
    queue: VecDeque<QueueEntry>,
}
#[derive(Serialize, Deserialize)]
struct SlotWire {
    key: String,
    tenant: String,
    run: String,
    kind: OperationKind,
}
#[derive(Serialize, Deserialize)]
struct QueueWire {
    key: String,
    tenant: String,
    scheduling: SchedulingPolicy,
    deadline: u64,
}
/// An immutable observation; never authorizes settlement or releases a slot.
#[derive(Clone, Debug)]
pub struct LateResult {
    pub operation: String,
    pub generation: u64,
    pub worker: String,
    pub observed_at: u64,
    pub result: ExternalResult,
}

impl PgStore {
    /// Initializes an empty host-owned namespace. Capacity must agree across all workers.
    pub async fn open(pool: PgPool, schema: &str, capacity: Capacity) -> Result<Self> {
        if schema.is_empty()
            || schema.len() > 63
            || schema.to_ascii_lowercase().starts_with("pg_")
            || !schema
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !schema.as_bytes()[0].is_ascii_alphabetic() && schema.as_bytes()[0] != b'_'
        {
            return Err(Error::Invalid("schema identifier".into()));
        }
        let store = Self {
            pool,
            schema: format!("\"{schema}\""),
            capacity,
            fail_next: Arc::default(),
        };
        let mut tx = store.pool.begin().await.map_err(storage)?;
        sqlx::Executor::execute(&mut *tx, "SET LOCAL synchronous_commit = on; SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '30s'").await.map_err(storage)?;
        // Serialize concurrent initializers, including CREATE SCHEMA/INDEX catalog writes.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("runtrell-schema:{}", store.schema))
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::Executor::execute(
            &mut *tx,
            format!("CREATE SCHEMA IF NOT EXISTS {}", store.schema).as_str(),
        )
        .await
        .map_err(storage)?;
        sqlx::Executor::execute(
            &mut *tx,
            include_str!("../migrations/0001.sql")
                .replace("{{schema}}", &store.schema)
                .as_str(),
        )
        .await
        .map_err(storage)?;
        let config = capacity_value(&store.capacity);
        sqlx::query(&format!("INSERT INTO {}.schema_config(singleton,version,capacity) VALUES(true,3,$1) ON CONFLICT DO NOTHING", store.schema))
            .bind(&config).execute(&mut *tx).await.map_err(storage)?;
        let (version, saved): (i32, Value) = sqlx::query_as(&format!(
            "SELECT version,capacity FROM {}.schema_config WHERE singleton",
            store.schema
        ))
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        if version != 3 {
            return Err(Error::Unsupported("postgres schema".into()));
        }
        if saved != config {
            return Err(Error::Conflict);
        }
        store.initialize_capacity(&mut tx).await?;
        tx.commit().await.map_err(storage)?;
        Ok(store)
    }

    /// Inject an actual SQL failure after all staged writes, before COMMIT.
    /// Reads do not consume this process-local test fault.
    pub fn fail_next_write(&self) {
        self.fail_next.store(true, Ordering::SeqCst);
    }

    async fn retain_late_result(
        &self,
        c: &Claim,
        id: &str,
        result: &ExternalResult,
        now: u64,
    ) -> Result<()> {
        // A separate durable observation cannot mutate the operational ledger.
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SET LOCAL synchronous_commit = on")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let op: Option<(Value,)> = sqlx::query_as(&format!(
            "SELECT to_jsonb(a) FROM {}.attempts a WHERE id=$1",
            self.schema
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let op: Operation = decode(op.ok_or(Error::NotFound)?.0)?;
        if op.run != c.run {
            return Err(Error::PermissionDenied);
        }
        let value = serde_json::to_value(result).map_err(storage)?;
        sqlx::query(&format!("INSERT INTO {}.late_results(operation,generation,worker,digest,observed_at,result) VALUES($1,$2,$3,$4,$5::text::numeric,$6) ON CONFLICT DO NOTHING", self.schema))
            .bind(id).bind(c.generation.to_string()).bind(&c.worker).bind(digest(&value)).bind(now.to_string()).bind(value)
            .execute(&mut *tx).await.map_err(storage)?;
        tx.commit().await.map_err(storage)
    }
    /// Current owner can inspect observations without treating them as authoritative results.
    pub async fn late_results(&self, c: &Claim, now: u64) -> Result<Vec<LateResult>> {
        self.check(c, now, false).await?;
        let rows = sqlx::query(&format!("SELECT l.operation,l.generation,l.worker,l.observed_at::text AS at,l.result FROM {}.late_results l JOIN {}.attempts r ON r.id=l.operation WHERE r.run=$1 ORDER BY l.observed_at,l.digest", self.schema,self.schema))
            .bind(serde_json::to_value(&c.run).map_err(storage)?).fetch_all(&self.pool).await.map_err(storage)?;
        rows.into_iter()
            .map(|r| {
                Ok(LateResult {
                    operation: r.try_get("operation").map_err(storage)?,
                    generation: r
                        .try_get::<String, _>("generation")
                        .map_err(storage)?
                        .parse()
                        .map_err(storage)?,
                    worker: r.try_get("worker").map_err(storage)?,
                    observed_at: r
                        .try_get::<String, _>("at")
                        .map_err(storage)?
                        .parse()
                        .map_err(storage)?,
                    result: decode(r.try_get("result").map_err(storage)?)?,
                })
            })
            .collect()
    }
}
fn storage(_: impl std::fmt::Display) -> Error {
    Error::Storage
}
fn decode<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(storage)
}
fn capacity_value(c: &Capacity) -> Value {
    json!({"global":c.global,"tenant":c.tenant,"run":c.run,"queue":c.queue,"tenant_queue":c.tenant_queue})
}
