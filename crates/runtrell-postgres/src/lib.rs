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
mod observation;
mod persistence;
pub use observation::storage_error;
use observation::storage_error as storage;

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
    requests: BTreeMap<String, RunSpec>,
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

/// Tables the schema-4 migration creates; `connect` requires every one.
const SCHEMA_TABLES: [&str; 13] = [
    "schema_config",
    "runs",
    "sessions",
    "budget_accounts",
    "attempts",
    "attempt_accounts",
    "slots",
    "queue_tenants",
    "queue_entries",
    "capacity_slots",
    "late_results",
    "queue_policy",
    "executions",
];
const SCHEMA_VERSION: i32 = 4;

impl PgStore {
    fn new(pool: PgPool, schema: &str, capacity: Capacity) -> Result<Self> {
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
        Ok(Self {
            pool,
            schema: format!("\"{schema}\""),
            capacity,
            fail_next: Arc::default(),
        })
    }

    /// Explicitly creates or completes the host-owned schema: runs the migration
    /// DDL, writes `schema_config` and seeds the global capacity permits. Idempotent;
    /// a repeat call with a different `Capacity` returns `Conflict`. Run it once at
    /// deployment with a role that may create and alter the schema. Processes that
    /// only operate on an existing runtime use [`PgStore::connect`] instead.
    pub async fn initialize(pool: PgPool, schema: &str, capacity: Capacity) -> Result<Self> {
        let store = Self::new(pool, schema, capacity)?;
        let mut tx = store
            .pool
            .begin()
            .await
            .map_err(|e| storage("PgStore::initialize", e))?;
        sqlx::Executor::execute(&mut *tx, "SET LOCAL synchronous_commit = on; SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '30s'").await.map_err(|e| storage("PgStore::initialize", e))?;
        // Serialize concurrent initializers, including CREATE SCHEMA/INDEX catalog writes.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("runtrell-schema:{}", store.schema))
            .execute(&mut *tx)
            .await
            .map_err(|e| storage("PgStore::initialize", e))?;
        sqlx::Executor::execute(
            &mut *tx,
            format!("CREATE SCHEMA IF NOT EXISTS {}", store.schema).as_str(),
        )
        .await
        .map_err(|e| storage("PgStore::initialize", e))?;
        sqlx::Executor::execute(
            &mut *tx,
            include_str!("../migrations/0001.sql")
                .replace("{{schema}}", &store.schema)
                .as_str(),
        )
        .await
        .map_err(|e| storage("PgStore::initialize", e))?;
        let config = capacity_value(&store.capacity);
        sqlx::query(&format!("INSERT INTO {}.schema_config(singleton,version,capacity) VALUES(true,{SCHEMA_VERSION},$1) ON CONFLICT DO NOTHING", store.schema))
            .bind(&config).execute(&mut *tx).await.map_err(|e| storage("PgStore::initialize", e))?;
        let (version, saved): (i32, Value) = sqlx::query_as(&format!(
            "SELECT version,capacity FROM {}.schema_config WHERE singleton",
            store.schema
        ))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| storage("PgStore::initialize", e))?;
        if version != SCHEMA_VERSION {
            return Err(Error::Unsupported("postgres schema".into()));
        }
        if saved != config {
            return Err(Error::Conflict);
        }
        store.initialize_capacity(&mut tx).await?;
        tx.commit()
            .await
            .map_err(|e| storage("PgStore::initialize", e))?;
        Ok(store)
    }

    /// Attaches to a runtime that [`PgStore::initialize`] already created, without
    /// changing it: one `READ ONLY` transaction, no DDL, and no write to
    /// `schema_config` or the capacity permits. Intended for least-privilege roles.
    ///
    /// Required privileges for this call: `USAGE` on the schema and `SELECT` on
    /// `schema_config` (existence of the other tables is checked through the
    /// catalog and needs nothing more). Operations performed on the returned store
    /// need the DML privileges of the tables they touch; see `docs/design/postgres.md`.
    ///
    /// Errors: `NotFound` when the schema, a table or the config row is missing;
    /// `PermissionDenied` when the role lacks the privileges above;
    /// `Unsupported("postgres schema")` for another schema version; `Conflict`
    /// when `capacity` differs from the stored configuration.
    pub async fn connect(pool: PgPool, schema: &str, capacity: Capacity) -> Result<Self> {
        let store = Self::new(pool, schema, capacity)?;
        let mut tx = store
            .pool
            .begin()
            .await
            .map_err(|e| storage("PgStore::connect", e))?;
        sqlx::Executor::execute(
            &mut *tx,
            "SET TRANSACTION READ ONLY; SET LOCAL lock_timeout = '5s'; SET LOCAL statement_timeout = '30s'",
        )
        .await
        .map_err(|e| storage("PgStore::connect", e))?;
        let row: Option<(i32, Value)> = sqlx::query_as(&format!(
            "SELECT version,capacity FROM {}.schema_config WHERE singleton",
            store.schema
        ))
        .fetch_optional(&mut *tx)
        .await
        .map_err(connect_error)?;
        let (version, saved) = row.ok_or(Error::NotFound)?;
        if version != SCHEMA_VERSION {
            return Err(Error::Unsupported("postgres schema".into()));
        }
        for table in SCHEMA_TABLES {
            let present: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
                .bind(format!("{}.{table}", store.schema))
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| storage("PgStore::connect", e))?;
            if !present {
                return Err(Error::NotFound);
            }
        }
        // Columns added after the first schema 4 release; an older runtime must be
        // re-initialized by its owner before a least-privilege process can attach.
        let has_recovery: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_attribute WHERE attrelid = to_regclass($1) \
             AND attname = 'recovery' AND NOT attisdropped)",
        )
        .bind(format!("{}.attempts", store.schema))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| storage("PgStore::connect", e))?;
        if !has_recovery {
            return Err(Error::NotFound);
        }
        if saved != capacity_value(&store.capacity) {
            return Err(Error::Conflict);
        }
        // Nothing was written; the read-only transaction ends by rollback.
        tx.rollback()
            .await
            .map_err(|e| storage("PgStore::connect", e))?;
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
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| storage("PgStore::retain_late_result", e))?;
        sqlx::query("SET LOCAL synchronous_commit = on")
            .execute(&mut *tx)
            .await
            .map_err(|e| storage("PgStore::retain_late_result", e))?;
        let op: Option<(Value,)> = sqlx::query_as(&format!(
            "SELECT to_jsonb(a) FROM {}.attempts a WHERE id=$1",
            self.schema
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| storage("PgStore::retain_late_result", e))?;
        let op: Operation = decode(op.ok_or(Error::NotFound)?.0)?;
        if op.run != c.run {
            return Err(Error::PermissionDenied);
        }
        let value =
            serde_json::to_value(result).map_err(|e| storage("PgStore::retain_late_result", e))?;
        sqlx::query(&format!("INSERT INTO {}.late_results(operation,generation,worker,digest,observed_at,result) VALUES($1,$2,$3,$4,$5::text::numeric,$6) ON CONFLICT DO NOTHING", self.schema))
            .bind(id).bind(c.generation.to_string()).bind(&c.worker).bind(digest(&value)).bind(now.to_string()).bind(value)
            .execute(&mut *tx).await.map_err(|e| storage("PgStore::retain_late_result", e))?;
        tx.commit()
            .await
            .map_err(|e| storage("PgStore::retain_late_result", e))
    }
    /// Current owner can inspect observations without treating them as authoritative results.
    pub async fn late_results(&self, c: &Claim, now: u64) -> Result<Vec<LateResult>> {
        self.check(c, now, false).await?;
        let rows = sqlx::query(&format!("SELECT l.operation,l.generation,l.worker,l.observed_at::text AS at,l.result FROM {}.late_results l JOIN {}.attempts r ON r.id=l.operation WHERE r.run=$1 ORDER BY l.observed_at,l.digest", self.schema,self.schema))
            .bind(serde_json::to_value(&c.run).map_err(|e| storage("PgStore::late_results", e))?).fetch_all(&self.pool).await.map_err(|e| storage("PgStore::late_results", e))?;
        rows.into_iter()
            .map(|r| {
                Ok(LateResult {
                    operation: r
                        .try_get("operation")
                        .map_err(|e| storage("PgStore::late_results", e))?,
                    generation: r
                        .try_get::<String, _>("generation")
                        .map_err(|e| storage("PgStore::late_results", e))?
                        .parse()
                        .map_err(|e| storage("PgStore::late_results", e))?,
                    worker: r
                        .try_get("worker")
                        .map_err(|e| storage("PgStore::late_results", e))?,
                    observed_at: r
                        .try_get::<String, _>("at")
                        .map_err(|e| storage("PgStore::late_results", e))?
                        .parse()
                        .map_err(|e| storage("PgStore::late_results", e))?,
                    result: decode(
                        r.try_get("result")
                            .map_err(|e| storage("PgStore::late_results", e))?,
                    )?,
                })
            })
            .collect()
    }
}
fn decode<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| storage("PgStore::decode", e))
}
fn capacity_value(c: &Capacity) -> Value {
    json!({"global":c.global,"tenant":c.tenant,"run":c.run,"queue":c.queue,"tenant_queue":c.tenant_queue})
}
/// Maps the definite "absent" and "not permitted" SQLSTATEs of the config read.
fn connect_error(error: sqlx::Error) -> Error {
    match error
        .as_database_error()
        .and_then(|db| db.code())
        .as_deref()
    {
        Some("3F000" | "42P01") => Error::NotFound,
        Some("42501") => Error::PermissionDenied,
        _ => storage("PgStore::connect", error),
    }
}
