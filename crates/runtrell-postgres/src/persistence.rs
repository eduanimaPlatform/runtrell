// SPDX-License-Identifier: Apache-2.0
//! Scoped relational persistence for the synchronous contract transitions.
use crate::*;
use engine::{ancestors, key, session_key};

#[derive(Clone, Copy)]
pub(crate) enum Mode {
    Bare,
    Fence,
    Budget,
    Work,
    Finalize,
    Ops,
    Claim,
    Tree,
    Close,
}
pub(crate) enum Scope<'a> {
    Run(&'a RunHandle, Mode),
    Start(&'a RunSpec),
    Request(&'a RunSpec),
    Account(Option<&'a str>),
    LockedAccount(&'a str),
    Slot(&'a str),
    Empty,
    Attempt(&'a RunHandle, &'a str, bool),
    Begin(&'a RunHandle, &'a DispatchRequest),
    Logical(&'a RunHandle, &'a str),
}
#[derive(Clone, Copy)]
enum Selection<'a> {
    All,
    Exact(&'a str),
    Outstanding(&'a str),
    Logical(&'a str),
}

impl PgStore {
    pub(crate) async fn read<R>(
        &self,
        scope: Scope<'_>,
        f: impl FnOnce(&Ledger) -> Result<R>,
    ) -> Result<R> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let state = self.load(&mut tx, scope, false).await?;
        let value = f(&state)?;
        tx.commit().await.map_err(storage)?;
        Ok(value)
    }
    pub(crate) async fn tx<R>(
        &self,
        scope: Scope<'_>,
        f: impl FnOnce(&mut Ledger) -> Result<R>,
    ) -> Result<R> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let value = self.tx_in(&mut tx, scope, f).await?;
        self.commit(tx).await?;
        Ok(value)
    }
    pub(crate) async fn tx_in<R>(
        &self,
        outer: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: Scope<'_>,
        f: impl FnOnce(&mut Ledger) -> Result<R>,
    ) -> Result<R> {
        // A failed call must not leave partial permits or writes in a host transaction.
        let mut tx = sqlx::Acquire::begin(&mut *outer).await.map_err(storage)?;
        let result = async {
            sqlx::Executor::execute(&mut *tx, "SET LOCAL synchronous_commit=on; SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='30s'")
                .await.map_err(storage)?;
            let mut state = self.load(&mut tx, scope, true).await?;
            let before = state.clone();
            let value = f(&mut state)?;
            self.save(&mut tx, &before, &state).await?;
            if self.fail_next.swap(false, Ordering::SeqCst) {
                sqlx::query("SELECT 1/0").execute(&mut *tx).await.map_err(storage)?;
            }
            Ok(value)
        }.await;
        match result {
            Ok(value) => {
                // RELEASE SAVEPOINT only; the caller owns the outer COMMIT.
                tx.commit().await.map_err(storage)?;
                Ok(value)
            }
            Err(error) => {
                tx.rollback().await.map_err(storage)?;
                Err(error)
            }
        }
    }
    pub(crate) async fn commit(&self, mut tx: sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            let fault = sqlx::query("SELECT 1/0").execute(&mut *tx).await;
            tx.rollback().await.map_err(storage)?;
            fault.map_err(storage)?;
            return Err(Error::Storage);
        }
        tx.commit().await.map_err(storage)
    }
    async fn rows(
        &self,
        conn: &mut PgConnection,
        table: &str,
        filter: &str,
        args: &[&str],
        lock: bool,
    ) -> Result<Vec<Value>> {
        let sql = format!(
            "SELECT to_jsonb(t) FROM {}.{table} t WHERE {filter}{}",
            self.schema,
            if lock { " FOR UPDATE" } else { "" }
        );
        let mut q = sqlx::query_scalar::<_, Value>(&sql);
        for arg in args {
            q = q.bind(*arg);
        }
        q.fetch_all(conn).await.map_err(storage)
    }
    async fn session(
        &self,
        conn: &mut PgConnection,
        s: &mut Ledger,
        tenant: &str,
        id: Option<&str>,
        write: bool,
    ) -> Result<()> {
        if let Some(id) = id {
            let k = session_key(tenant, id);
            if write {
                sqlx::query(&format!(
                    "INSERT INTO {}.sessions(key) VALUES($1) ON CONFLICT DO NOTHING",
                    self.schema
                ))
                .bind(&k)
                .execute(&mut *conn)
                .await
                .map_err(storage)?;
            }
            for v in self.rows(conn, "sessions", "key=$1", &[&k], write).await? {
                s.sessions.insert(
                    k.clone(),
                    (
                        decode(v["sequence"].clone())?,
                        decode(v["context_revision"].clone())?,
                    ),
                );
            }
        }
        Ok(())
    }
    async fn accounts(
        &self,
        conn: &mut PgConnection,
        s: &mut Ledger,
        id: &str,
        write: bool,
    ) -> Result<()> {
        // Topology is immutable. Discover IDs first, then lock *all* accounts in ID order.
        let ids: Vec<String> = sqlx::query_scalar(&format!("WITH RECURSIVE chain AS (SELECT id,parent FROM {}.budget_accounts WHERE id=$1 UNION SELECT a.id,a.parent FROM {}.budget_accounts a JOIN chain c ON a.id=c.parent) SELECT id FROM chain",self.schema,self.schema))
            .bind(id).fetch_all(&mut *conn).await.map_err(storage)?;
        let sql = format!(
            "SELECT to_jsonb(a) FROM {}.budget_accounts a WHERE id=ANY($1) ORDER BY id{}",
            self.schema,
            if write { " FOR UPDATE" } else { "" }
        );
        for v in sqlx::query_scalar::<_, Value>(&sql)
            .bind(ids)
            .fetch_all(&mut *conn)
            .await
            .map_err(storage)?
        {
            let a: Account = decode(v)?;
            s.accounts.insert(a.id.clone(), a);
        }
        Ok(())
    }
    async fn load(&self, conn: &mut PgConnection, scope: Scope<'_>, write: bool) -> Result<Ledger> {
        let mut s = Ledger::default();
        let (scope, selection, selected_slot) = match scope {
            Scope::Attempt(h, id, budget) => (
                Scope::Run(h, if budget { Mode::Work } else { Mode::Ops }),
                if budget {
                    Selection::Outstanding(id)
                } else {
                    Selection::Exact(id)
                },
                Some(id.to_owned()),
            ),
            Scope::Begin(h, request) => (
                Scope::Run(h, Mode::Work),
                Selection::Logical(&request.key),
                Some(format!("{}:{}:{}", key(h), request.key, request.attempt)),
            ),
            Scope::Logical(h, k) => (Scope::Run(h, Mode::Ops), Selection::Logical(k), None),
            scope => (scope, Selection::All, None),
        };
        match scope {
            Scope::Attempt(..) | Scope::Begin(..) | Scope::Logical(..) => {
                unreachable!("normalized scope")
            }

            Scope::Empty => {}
            Scope::LockedAccount(id) => {
                self.accounts(conn, &mut s, id, write).await?;
            }
            Scope::Account(id) => {
                if let Some(id) = id {
                    for v in self
                        .rows(conn, "budget_accounts", "id=$1", &[id], false)
                        .await?
                    {
                        let a: Account = decode(v)?;
                        s.accounts.insert(a.id.clone(), a);
                    }
                }
            }
            Scope::Request(spec) => {
                for v in self
                    .rows(
                        conn,
                        "runs",
                        "tenant=$1 AND principal=$2 AND request_key=$3",
                        &[&spec.tenant, &spec.principal, &spec.request_key],
                        false,
                    )
                    .await?
                {
                    let r = load_run(&mut s, v)?;
                    s.runs.insert(key(&r.handle), r);
                }
            }
            Scope::Start(spec) => {
                // Only duplicate reception of the same identity contends on this lock.
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                    .bind(format!(
                        "{}:request:{}",
                        self.schema,
                        json!([spec.tenant, spec.principal, spec.request_key])
                    ))
                    .execute(&mut *conn)
                    .await
                    .map_err(storage)?;
                for v in self
                    .rows(
                        conn,
                        "runs",
                        "tenant=$1 AND principal=$2 AND request_key=$3",
                        &[&spec.tenant, &spec.principal, &spec.request_key],
                        false,
                    )
                    .await?
                {
                    let r = load_run(&mut s, v)?;
                    s.runs.insert(key(&r.handle), r);
                }
                // An existing receipt requires no new admission or parent/session mutation.
                if s.runs.is_empty() {
                    self.session(conn, &mut s, &spec.tenant, spec.session.as_deref(), write)
                        .await?;
                    if let Some(h) = &spec.parent {
                        for v in self
                            .rows(
                                conn,
                                "runs",
                                "tenant=$1 AND id=$2",
                                &[&h.tenant, &h.id],
                                write,
                            )
                            .await?
                        {
                            let r = load_run(&mut s, v)?;
                            s.runs.insert(key(h), r);
                        }
                    }
                    self.accounts(conn, &mut s, &spec.account, false).await?;
                }
            }
            Scope::Run(h, mode) => {
                if !matches!(mode, Mode::Bare | Mode::Claim | Mode::Tree | Mode::Close) {
                    // Session precedes Run locks. Run identity/session membership are immutable.
                    let session: Option<Option<String>> = sqlx::query_scalar(&format!(
                        "SELECT session_id FROM {}.runs WHERE tenant=$1 AND id=$2",
                        self.schema
                    ))
                    .bind(&h.tenant)
                    .bind(&h.id)
                    .fetch_optional(&mut *conn)
                    .await
                    .map_err(storage)?;
                    if let Some(Some(id)) = session {
                        self.session(conn, &mut s, &h.tenant, Some(&id), write)
                            .await?;
                    }
                }
                let mut pending = VecDeque::from([h.clone()]);
                while let Some(at) = pending.pop_front() {
                    for v in self
                        .rows(
                            conn,
                            "runs",
                            "tenant=$1 AND id=$2",
                            &[&at.tenant, &at.id],
                            write,
                        )
                        .await?
                    {
                        let r = load_run(&mut s, v)?;
                        s.runs.insert(key(&at), r);
                        if matches!(mode, Mode::Tree) {
                            // Discover children only after locking their parent: child admission
                            // takes the same parent lock, so newly committed descendants cannot escape.
                            for v in self
                                .rows(
                                    conn,
                                    "runs",
                                    "parent_tenant=$1 AND parent_id=$2 ORDER BY tenant,id",
                                    &[&at.tenant, &at.id],
                                    false,
                                )
                                .await?
                            {
                                let child = load_run(&mut s, v)?;
                                pending.push_back(child.handle);
                            }
                        }
                    }
                }
                if let Some(r) = s.runs.get(&key(h)).cloned() {
                    if !matches!(mode, Mode::Bare | Mode::Claim | Mode::Tree | Mode::Close)
                        && let Some(id) = &r.spec.session
                    {
                        // Only one active predecessor is needed to enforce the input order.
                        for v in self.rows(conn,"runs","tenant=$1 AND session_id=$2 AND sequence < $3::numeric AND NOT (state ? 'Terminal') ORDER BY sequence LIMIT 1", &[&h.tenant,id,&r.sequence.to_string()],false).await? {
                            let prior = load_run(&mut s, v)?; s.runs.insert(key(&prior.handle),prior);
                        }
                    }
                    if matches!(mode, Mode::Budget | Mode::Work | Mode::Finalize) {
                        self.accounts(conn, &mut s, &r.spec.account, write).await?;
                    }
                }
                let handles: Vec<_> = if matches!(mode, Mode::Tree) {
                    s.runs.values().map(|r| r.handle.clone()).collect()
                } else {
                    vec![h.clone()]
                };
                for at in handles {
                    if matches!(
                        mode,
                        Mode::Ops | Mode::Work | Mode::Tree | Mode::Finalize | Mode::Close
                    ) {
                        let mut args = vec![at.tenant.as_str(), at.id.as_str()];
                        let filter = match selection {
                            Selection::All if matches!(mode, Mode::Tree | Mode::Close) => {
                                "tenant=$1 AND run_id=$2 AND NOT settled"
                            }
                            Selection::All => "tenant=$1 AND run_id=$2",
                            Selection::Exact(id) => {
                                args.push(id);
                                "id=$3"
                            }
                            Selection::Outstanding(id) => {
                                args.push(id);
                                "(tenant=$1 AND run_id=$2 AND NOT settled) OR id=$3"
                            }
                            Selection::Logical(k) => {
                                args.push(k);
                                "tenant=$1 AND run_id=$2 AND operation_key=$3"
                            }
                        };
                        // All placeholders have a declared type even for exact-ID lookups.
                        let filter = if matches!(selection, Selection::Exact(_)) {
                            "id=$3 AND $1::text IS NOT NULL AND $2::text IS NOT NULL"
                        } else {
                            filter
                        };
                        for v in self.rows(conn, "attempts", filter, &args, false).await? {
                            let op: Operation = decode(v)?;
                            s.operations.insert(op.id.clone(), op);
                        }
                    }
                    if matches!(mode, Mode::Claim | Mode::Work) {
                        if let Some(k) = &selected_slot {
                            for v in self.rows(conn, "slots", "key=$1", &[k], false).await? {
                                add_slot(&mut s, v)?;
                            }
                        } else {
                            for v in self
                                .rows(
                                    conn,
                                    "slots",
                                    "tenant=$1 AND run=$2",
                                    &[&at.tenant, &at.id],
                                    false,
                                )
                                .await?
                            {
                                add_slot(&mut s, v)?;
                            }
                        }
                    }
                    if matches!(mode, Mode::Claim | Mode::Tree | Mode::Close) {
                        for v in self
                            .rows(conn, "queue_entries", "key=$1", &[&key(&at)], false)
                            .await?
                        {
                            add_queue(&mut s, v)?;
                        }
                    }
                }
            }
            Scope::Slot(k) => {
                // Same-key scheduler calls serialize; unrelated slot identities do not.
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                    .bind(format!("{}:slot:{k}", self.schema))
                    .execute(&mut *conn)
                    .await
                    .map_err(storage)?;
                for v in self.rows(conn, "slots", "key=$1", &[k], write).await? {
                    add_slot(&mut s, v)?;
                }
                for v in self.rows(conn, "attempts", "id=$1", &[k], false).await? {
                    let op: Operation = decode(v)?;
                    s.operations.insert(op.id.clone(), op);
                }
            }
        }
        Ok(s)
    }
    /// The column list is internal and fixed. Compound protocol values are JSONB;
    /// entity identity, lifetime and ownership each have their own relational row.
    async fn put(
        &self,
        conn: &mut PgConnection,
        table: &str,
        columns: &str,
        conflict: &str,
        value: Value,
    ) -> Result<()> {
        let cols: Vec<_> = columns.split(',').collect();
        let select = cols
            .iter()
            .map(|c| format!("x.{c}"))
            .collect::<Vec<_>>()
            .join(",");
        let update = cols
            .iter()
            .map(|c| format!("{c}=EXCLUDED.{c}"))
            .collect::<Vec<_>>()
            .join(",");
        sqlx::query(&format!("INSERT INTO {}.{table} ({columns}) SELECT {select} FROM jsonb_populate_record(NULL::{}.{table},$1) x ON CONFLICT ({conflict}) DO UPDATE SET {update}",self.schema,self.schema))
            .bind(value).execute(conn).await.map_err(storage)?;
        Ok(())
    }
    async fn save(&self, conn: &mut PgConnection, before: &Ledger, s: &Ledger) -> Result<()> {
        for (k, r) in &s.runs {
            if before.runs.get(k) != Some(r) {
                let mut value = json!(r);
                value["request_spec"] = json!(s.requests.get(k).ok_or(Error::Storage)?);
                self.put(conn,"runs","handle,spec,request_spec,schema,revision,sequence,owner,generation,state,checkpoint,cancel,stop,cleanup,settlement,delivery,output,receipt,completion","tenant,id",value).await?;
            }
        }
        for (k, a) in &s.accounts {
            if before.accounts.get(k) != Some(a) {
                self.put(
                    conn,
                    "budget_accounts",
                    "id,tenant,parent,\"limit\",reserved,used,unknown,exhausted,warnings,policy,quantity_reserved,quantity_used,quantity_warnings",
                    "id",
                    json!(a),
                )
                .await?;
            }
        }
        for (k, (sequence, revision)) in &s.sessions {
            if before.sessions.get(k) != s.sessions.get(k) {
                self.put(
                    conn,
                    "sessions",
                    "key,sequence,context_revision",
                    "key",
                    json!({"key":k,"sequence":sequence,"context_revision":revision}),
                )
                .await?;
            }
        }
        for (k, o) in &s.operations {
            if before.operations.get(k) != Some(o) {
                self.put(conn,"attempts","id,run,request,state,result,settled,unknown_usage,send_generation,send_count,replay_proof_generation,created_at,finished_at,usage_observations,unsettled_result","id",json!(o)).await?;
                if !before.operations.contains_key(k) {
                    let run = s.runs.get(&key(&o.run)).ok_or(Error::Storage)?;
                    for account in ancestors(s, &run.spec.account)? {
                        sqlx::query(&format!(
                            "INSERT INTO {}.attempt_accounts(attempt_id,account_id) VALUES($1,$2)",
                            self.schema
                        ))
                        .bind(&o.id)
                        .bind(account)
                        .execute(&mut *conn)
                        .await
                        .map_err(storage)?;
                    }
                }
            }
        }
        for (k, v) in &s.slots {
            if !before.slots.contains_key(k) {
                self.insert_slot(conn, v).await?;
            }
        }
        for k in before.slots.keys().filter(|k| !s.slots.contains_key(*k)) {
            sqlx::query(&format!("DELETE FROM {}.slots WHERE key=$1", self.schema))
                .bind(k)
                .execute(&mut *conn)
                .await
                .map_err(storage)?;
            self.free(conn, &format!("slot:{k}")).await?;
        }
        for e in &s.queue {
            if !before.queue.iter().any(|old| old.key == e.key) {
                self.insert_queue(conn, e).await?;
            }
        }
        for e in before
            .queue
            .iter()
            .filter(|e| !s.queue.iter().any(|new| new.key == e.key))
        {
            self.remove_queue(conn, &e.key).await?;
        }
        Ok(())
    }
    pub(crate) async fn initialize_capacity(&self, conn: &mut PgConnection) -> Result<()> {
        // Seed shared scopes before admitting any Run. Otherwise first use could
        // wait on another transaction's uncommitted initialization of free permits.
        for (kind, limit) in &self.capacity.global {
            self.seed_permits(conn, &format!("g:{kind:?}"), *limit)
                .await?;
        }
        self.seed_permits(conn, "q:global", self.capacity.queue)
            .await
    }
    async fn seed_permits(&self, conn: &mut PgConnection, scope: &str, limit: usize) -> Result<()> {
        sqlx::query(&format!("INSERT INTO {}.capacity_slots(scope,ordinal) SELECT $1,n FROM generate_series(1,$2::bigint) n WHERE NOT EXISTS (SELECT 1 FROM {}.capacity_slots p WHERE p.scope=$1 AND p.ordinal=n) ON CONFLICT DO NOTHING",self.schema,self.schema))
            .bind(scope).bind(i64::try_from(limit).map_err(storage)?).execute(conn).await.map_err(storage)?;
        Ok(())
    }
    async fn permits(
        &self,
        conn: &mut PgConnection,
        holder: &str,
        scopes: Vec<(String, usize)>,
    ) -> Result<()> {
        for (scope, limit) in scopes {
            self.seed_permits(conn, &scope, limit).await?;
            let mut ordinal: Option<i64> = sqlx::query_scalar(&format!("SELECT ordinal FROM {}.capacity_slots WHERE scope=$1 AND holder IS NULL ORDER BY ordinal FOR UPDATE SKIP LOCKED LIMIT 1",self.schema))
                .bind(&scope).fetch_optional(&mut *conn).await.map_err(storage)?;
            if ordinal.is_none() {
                // A skipped free row is contention, not proof of exhaustion.
                // PostgreSQL rechecks holder after waiting for a concurrent
                // updater; lock_timeout bounds the wait and maps to Storage.
                ordinal = sqlx::query_scalar(&format!("SELECT ordinal FROM {}.capacity_slots WHERE scope=$1 AND holder IS NULL ORDER BY ordinal FOR UPDATE LIMIT 1",self.schema))
                    .bind(&scope).fetch_optional(&mut *conn).await.map_err(storage)?;
            }
            let ordinal = ordinal.ok_or(Error::Backpressure)?;
            sqlx::query(&format!(
                "UPDATE {}.capacity_slots SET holder=$1 WHERE scope=$2 AND ordinal=$3",
                self.schema
            ))
            .bind(holder)
            .bind(scope)
            .bind(ordinal)
            .execute(&mut *conn)
            .await
            .map_err(storage)?;
        }
        Ok(())
    }
    async fn free(&self, conn: &mut PgConnection, holder: &str) -> Result<()> {
        // Stable ordering also covers release of multiple permit scopes.
        sqlx::query(&format!("SELECT ordinal FROM {}.capacity_slots WHERE holder=$1 ORDER BY scope,ordinal FOR UPDATE",self.schema))
            .bind(holder).fetch_all(&mut *conn).await.map_err(storage)?;
        sqlx::query(&format!(
            "UPDATE {}.capacity_slots SET holder=NULL WHERE holder=$1",
            self.schema
        ))
        .bind(holder)
        .execute(conn)
        .await
        .map_err(storage)?;
        Ok(())
    }
    async fn insert_slot(&self, conn: &mut PgConnection, v: &SlotRequest) -> Result<()> {
        let kind = json!(v.kind);
        let inserted = sqlx::query(&format!(
            "INSERT INTO {}.slots(key,tenant,run,kind) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING",
            self.schema
        ))
        .bind(&v.key)
        .bind(&v.tenant)
        .bind(&v.run)
        .bind(kind)
        .execute(&mut *conn)
        .await
        .map_err(storage)?
        .rows_affected();
        if inserted == 0 {
            let old = self.rows(conn, "slots", "key=$1", &[&v.key], false).await?;
            let mut state = Ledger::default();
            for v in old {
                add_slot(&mut state, v)?;
            }
            return if state.slots.get(&v.key) == Some(v) {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        self.permits(
            conn,
            &format!("slot:{}", v.key),
            vec![
                (
                    format!("g:{:?}", v.kind),
                    *self.capacity.global.get(&v.kind).unwrap_or(&0),
                ),
                (
                    format!("t:{}:{:?}", json!(v.tenant), v.kind),
                    *self.capacity.tenant.get(&v.kind).unwrap_or(&0),
                ),
                (
                    format!("r:{}:{:?}", json!([v.tenant, v.run]), v.kind),
                    *self.capacity.run.get(&v.kind).unwrap_or(&0),
                ),
            ],
        )
        .await
    }
    async fn insert_queue(&self, conn: &mut PgConnection, e: &QueueEntry) -> Result<()> {
        sqlx::query(&format!(
            "INSERT INTO {}.queue_tenants(tenant) VALUES($1) ON CONFLICT DO NOTHING",
            self.schema
        ))
        .bind(&e.tenant)
        .execute(&mut *conn)
        .await
        .map_err(storage)?;
        let n=sqlx::query(&format!("INSERT INTO {}.queue_entries(key,tenant,deadline,scheduling) VALUES($1,$2,$3::text::numeric,$4) ON CONFLICT DO NOTHING",self.schema))
            .bind(&e.key).bind(&e.tenant).bind(e.deadline.to_string()).bind(json!(e.scheduling)).execute(&mut *conn).await.map_err(storage)?.rows_affected();
        if n == 0 {
            let mut state = Ledger::default();
            for v in self
                .rows(conn, "queue_entries", "key=$1", &[&e.key], false)
                .await?
            {
                add_queue(&mut state, v)?;
            }
            return if state.queue.front().is_some_and(|old| {
                old.tenant == e.tenant
                    && old.scheduling == e.scheduling
                    && old.deadline == e.deadline
            }) {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        self.permits(
            conn,
            &format!("queue:{}", e.key),
            vec![
                ("q:global".into(), self.capacity.queue),
                (format!("q:{}", json!(e.tenant)), self.capacity.tenant_queue),
            ],
        )
        .await
    }
    async fn remove_queue(&self, conn: &mut PgConnection, k: &str) -> Result<()> {
        sqlx::query(&format!(
            "DELETE FROM {}.queue_entries WHERE key=$1",
            self.schema
        ))
        .bind(k)
        .execute(&mut *conn)
        .await
        .map_err(storage)?;
        self.free(conn, &format!("queue:{k}")).await
    }
    pub(crate) async fn take_queue(&self, now: u64) -> Result<Option<QueueEntry>> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::Executor::execute(&mut *tx,"SET LOCAL synchronous_commit=on; SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='30s'").await.map_err(storage)?;
        let expired: Option<String>=sqlx::query_scalar(&format!("SELECT key FROM {}.queue_entries WHERE deadline <= $1::text::numeric ORDER BY position FOR UPDATE SKIP LOCKED LIMIT 1",self.schema))
            .bind(now.to_string()).fetch_optional(&mut *tx).await.map_err(storage)?;
        if let Some(k) = expired {
            self.remove_queue(&mut tx, &k).await?;
            self.commit(tx).await?;
            return Err(Error::QueueTimeout);
        }
        // Only dispatch ordering is coordinated. SKIP LOCKED means an idle worker
        // can poll again; no Run, reservation or slot call acquires this row.
        let policy: Option<(Option<String>, i32)> = sqlx::query_as(&format!(
            "SELECT last_tenant,priority_streak FROM {}.queue_policy WHERE singleton FOR UPDATE SKIP LOCKED",
            self.schema
        ))
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let Some((last_tenant, streak)) = policy else {
            return Ok(None);
        };
        let tenant: Option<String> = sqlx::query_scalar(&format!(
            "SELECT tenant FROM {}.queue_tenants t WHERE EXISTS (SELECT 1 FROM {}.queue_entries e WHERE e.tenant=t.tenant) ORDER BY CASE WHEN $1::text IS NULL OR tenant>$1 THEN 0 ELSE 1 END, CASE WHEN $1::text IS NULL THEN (SELECT min(position) FROM {}.queue_entries e WHERE e.tenant=t.tenant) END, tenant FOR UPDATE OF t SKIP LOCKED LIMIT 1",self.schema,self.schema,self.schema))
            .bind(last_tenant).fetch_optional(&mut *tx).await.map_err(storage)?;
        let Some(tenant) = tenant else {
            return Ok(None);
        };
        let values: Vec<Value> = sqlx::query_scalar(&format!("SELECT to_jsonb(e) FROM {}.queue_entries e WHERE tenant=$1 ORDER BY position FOR UPDATE SKIP LOCKED", self.schema))
            .bind(&tenant).fetch_all(&mut *tx).await.map_err(storage)?;
        let entries: Vec<QueueEntry> = values
            .into_iter()
            .map(|v| {
                let w: QueueWire = decode(v)?;
                Ok(QueueEntry {
                    key: w.key,
                    tenant: w.tenant,
                    scheduling: w.scheduling,
                    deadline: w.deadline,
                })
            })
            .collect::<Result<_>>()?;
        let Some(index) = runtrell::budget::queue_index(&entries, streak as usize) else {
            return Ok(None);
        };
        let e = entries[index].clone();
        let high = entries.iter().map(|e| e.scheduling.priority).max().unwrap();
        self.remove_queue(&mut tx, &e.key).await?;
        sqlx::query(&format!(
            "UPDATE {}.queue_policy SET last_tenant=$1,priority_streak=$2 WHERE singleton",
            self.schema
        ))
        .bind(&e.tenant)
        .bind(if e.scheduling.priority == high {
            streak.saturating_add(1).min(i32::from(u16::MAX))
        } else {
            0
        })
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        self.commit(tx).await?;
        Ok(Some(QueueEntry {
            key: e.key,
            tenant: e.tenant,
            scheduling: e.scheduling,
            deadline: e.deadline,
        }))
    }
    pub async fn slot_count(&self, kind: OperationKind) -> Result<usize> {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {}.slots WHERE kind=$1",
            self.schema
        ))
        .bind(json!(kind))
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        usize::try_from(n).map_err(storage)
    }
    pub async fn queue_len(&self) -> Result<usize> {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {}.queue_entries",
            self.schema
        ))
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        usize::try_from(n).map_err(storage)
    }
    pub async fn recovery_candidates(&self) -> Result<Vec<RunHandle>> {
        let values:Vec<Value>=sqlx::query_scalar(&format!("SELECT handle FROM {}.runs r WHERE NOT (state ? 'Terminal') OR stop IN ('\"Pending\"'::jsonb,'\"Unknown\"'::jsonb) OR cleanup IN ('\"Pending\"'::jsonb,'\"Unknown\"'::jsonb) OR settlement IN ('\"Pending\"'::jsonb,'\"Unknown\"'::jsonb) OR EXISTS(SELECT 1 FROM {}.slots s WHERE s.tenant=r.tenant AND s.run=r.id) ORDER BY tenant,id",self.schema,self.schema)).fetch_all(&self.pool).await.map_err(storage)?;
        values.into_iter().map(decode).collect()
    }
    pub async fn held_slots(&self, run: &RunHandle) -> Result<Vec<SlotRequest>> {
        self.read(Scope::Run(run, Mode::Claim), |s| {
            engine::get(s, run)?;
            Ok(s.slots.values().cloned().collect())
        })
        .await
    }
}
fn add_slot(s: &mut Ledger, v: Value) -> Result<()> {
    let w: SlotWire = decode(v)?;
    s.slots.insert(
        w.key.clone(),
        SlotRequest {
            key: w.key,
            tenant: w.tenant,
            run: w.run,
            kind: w.kind,
        },
    );
    Ok(())
}
fn add_queue(s: &mut Ledger, v: Value) -> Result<()> {
    let w: QueueWire = decode(v)?;
    s.queue.push_back(QueueEntry {
        key: w.key,
        tenant: w.tenant,
        scheduling: w.scheduling,
        deadline: w.deadline,
    });
    Ok(())
}

fn load_run(s: &mut Ledger, value: Value) -> Result<RunRecord> {
    let request = decode(value["request_spec"].clone())?;
    let run: RunRecord = decode(value)?;
    s.requests.insert(key(&run.handle), request);
    Ok(run)
}
