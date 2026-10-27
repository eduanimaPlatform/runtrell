// SPDX-License-Identifier: Apache-2.0
// Example-only host storage. No changes to the runtime schema or API.
use runtrell::*;
use runtrell_postgres::{PgStore, storage_error as storage};
use runtrell_testkit::{
    BusinessTool, DecisionFinalizer, DecisionValidator, FakeModel, FakePolicy, TestClock,
};
use serde_json::{Value, json};
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::str::FromStr;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::Notify;

pub fn block_on(f: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(f);
}

pub fn processes(args: &[&str]) {
    // The parent never opens a pool. A exits before B starts, including its reactor.
    let schema = format!("chat_{}", uuid::Uuid::new_v4().simple());
    let mut pids = std::collections::BTreeSet::new();
    for phase in ["a", "b", "admit", "recover", "contracts"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(args)
            .env("RUNTRELL_CHAT_SCHEMA", &schema)
            .env("RUNTRELL_CHAT_PHASE", phase)
            .spawn()
            .unwrap();
        assert!(pids.insert(child.id()));
        assert!(child.wait().unwrap().success(), "chat phase {phase}");
    }
}

struct Host {
    pool: PgPool,
    store: Arc<PgStore>,
    schema: String,
}
impl Host {
    async fn open(schema: &str) -> Self {
        let url = std::env::var("RUNTRELL_TEST_DATABASE_URL")
            .expect("disposable PostgreSQL required; chat contracts never skip");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(
                PgConnectOptions::from_str(&url)
                    .unwrap()
                    .disable_statement_logging(),
            )
            .await
            .unwrap();
        // PgStore validates the identifier before the example interpolates it.
        let store = Arc::new(
            PgStore::open(pool.clone(), schema, Capacity::default())
                .await
                .unwrap(),
        );
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS {schema}.chat_messages (
            id text PRIMARY KEY, ordinal bigserial UNIQUE, body text NOT NULL,
            run jsonb NOT NULL, response jsonb)"
        ))
        .execute(&pool)
        .await
        .unwrap();
        Self {
            pool,
            store,
            schema: schema.into(),
        }
    }

    async fn accept(&self, id: &str, body: &str, authorized: bool) -> Result<RunHandle> {
        // Fixed fixture principal/conversation. Real hosts resolve these from current
        // authentication and check reference/configuration permissions on every call.
        if !authorized {
            return Err(Error::PermissionDenied);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| storage("chat_host::accept", e))?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("chat-host:{}", self.schema))
            .execute(&mut *tx)
            .await
            .map_err(|e| storage("chat_host::accept", e))?;
        let old: Option<(String, Value)> = sqlx::query_as(&format!(
            "SELECT body,run FROM {}.chat_messages WHERE id=$1",
            self.schema
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| storage("chat_host::accept", e))?;
        if let Some((saved, run)) = old {
            if saved != body {
                return Err(Error::Conflict);
            }
            return serde_json::from_value(run).map_err(|e| storage("chat_host::accept", e));
        }
        let saved: Vec<(String, Value)> = sqlx::query_as(&format!(
            "SELECT body,response FROM {}.chat_messages WHERE response IS NOT NULL ORDER BY ordinal", self.schema))
            .fetch_all(&mut *tx).await.map_err(|e| storage("chat_host::accept", e))?;
        let revision = saved.len() as u64;
        // Only safe, host-owned text crosses Run boundaries. Never copy the
        // previous Run's checkpoint, protocol or opaque provider envelopes.
        let history: Vec<Value> = saved
            .into_iter()
            .flat_map(|(body, response)| {
                [
                    json!({"role":"user","text":body}),
                    json!({"role":"assistant","text":response["decision"]}),
                ]
            })
            .collect();
        let input = json!({"history":history,"message":body}).to_string();
        let account = self
            .store
            .create_account_in(&mut tx, "chat", Amount::new(1000, 1000), None)
            .await?;
        let mut spec = RunSpec::new("chat", id, account, &input);
        spec.session = Some("conversation".into());
        spec.context_revision = revision;
        let run = self.store.start_in(&mut tx, spec).await?;
        sqlx::query(&format!(
            "INSERT INTO {}.chat_messages(id,body,run) VALUES($1,$2,$3)",
            self.schema
        ))
        .bind(id)
        .bind(body)
        .bind(serde_json::to_value(&run).unwrap())
        .execute(&mut *tx)
        .await
        .map_err(|e| storage("chat_host::accept", e))?;
        tx.commit()
            .await
            .map_err(|e| storage("chat_host::accept", e))?;
        Ok(run)
    }

    async fn publish(&self, run: &RunHandle) -> Result<Value> {
        let record = self.store.get(run).await?;
        if record.state != RunState::Terminal(Terminal::Succeeded) {
            return Err(Error::Conflict);
        }
        let output = record.output.ok_or(Error::Conflict)?;
        let changed = sqlx::query(&format!(
            "UPDATE {}.chat_messages SET response=$1
            WHERE run=$2 AND (response IS NULL OR response=$1)",
            self.schema
        ))
        .bind(&output)
        .bind(serde_json::to_value(run).unwrap())
        .execute(&self.pool)
        .await
        .map_err(|e| storage("chat_host::publish", e))?
        .rows_affected();
        if changed != 1 {
            return Err(Error::Conflict);
        }
        Ok(output)
    }

    async fn idle(&self) {
        assert_eq!(self.store.slot_count(OperationKind::Run).await.unwrap(), 0);
        assert_eq!(
            self.store.slot_count(OperationKind::Model).await.unwrap(),
            0
        );
        assert_eq!(self.store.slot_count(OperationKind::Tool).await.unwrap(), 0);
        assert_eq!(self.store.queue_len().await.unwrap(), 0);
        // No per-conversation runner exists at this point. Closing the pool below
        // also closes idle DB connections; deployment owns overall process scaling.
    }
    async fn close(self) {
        drop(self.store);
        self.pool.close().await;
        assert!(self.pool.is_closed());
    }
}

#[derive(Clone)]
struct ChatModel {
    fake: FakeModel,
    gate: Option<Arc<(Notify, Notify)>>,
    calls: Arc<AtomicU64>,
}
impl ChatModel {
    fn reply(label: &str) -> Self {
        Self {
            fake: FakeModel::scripted(vec![FakeModel::decision(json!({"decision":label}))]),
            gate: None,
            calls: Arc::default(),
        }
    }
}
impl ModelPort for ChatModel {
    fn limits(&self) -> ModelLimits {
        self.fake.limits()
    }
    fn input_upper_bound(&self, r: &PreparedModelRequest) -> Result<u64> {
        self.fake.input_upper_bound(r)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        self.fake.recovery()
    }
    async fn invoke(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<ExternalResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.0.notify_one();
            gate.1.notified().await;
        }
        self.fake.invoke(c, r).await
    }
    async fn lookup(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<Lookup> {
        self.fake.lookup(c, r).await
    }
}
fn runtime(
    store: Arc<PgStore>,
    model: ChatModel,
    now: u64,
) -> Runtime<
    PgStore,
    ChatModel,
    BusinessTool,
    FakePolicy,
    TestClock,
    DecisionValidator,
    DecisionFinalizer,
> {
    let clock = TestClock::default();
    clock.set(now);
    Runtime {
        store,
        model,
        tools: vec![],
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    }
}
async fn reply(host: &Host, run: &RunHandle, label: &str, expected_history: usize) {
    let model = ChatModel::reply(label);
    let claim = host.store.claim(run, label, 0, 100).await.unwrap();
    let rt = runtime(host.store.clone(), model.clone(), 0);
    let record = rt.drive(&claim).await.unwrap();
    assert_eq!(record.state, RunState::Terminal(Terminal::Succeeded));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    {
        let requests = model.fake.requests.lock().unwrap();
        let input: Value = serde_json::from_str(&requests[0].messages[0].text).unwrap();
        assert_eq!(input["message"], format!("hello {label}"));
        assert_eq!(input["history"].as_array().unwrap().len(), expected_history);
        for (index, previous) in ["A", "B"].iter().take(expected_history / 2).enumerate() {
            assert_eq!(
                input["history"][index * 2],
                json!({"role":"user","text":format!("hello {previous}")})
            );
            assert_eq!(
                input["history"][index * 2 + 1],
                json!({"role":"assistant","text":previous})
            );
        }
        assert!(requests[0].messages.iter().all(|m| m.role != "provider"));
    }
    drop(rt);
    // Normal return frees the slot, but does not erase the durable lease record.
    assert!(record.owner.is_some());
    host.idle().await;
    tokio::task::yield_now().await;
    // No empty drive, conversation task or model/tool call is scheduled in idle.
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

pub async fn phase(schema: &str, phase: &str) {
    let host = Host::open(schema).await;
    match phase {
        "a" => {
            let run = host.accept("message-a", "hello A", true).await.unwrap();
            reply(&host, &run, "A", 0).await;
            host.publish(&run).await.unwrap();
            host.idle().await;
        }
        "b" => {
            let a = host.accept("message-a", "hello A", true).await.unwrap();
            let before = host.store.list_operations(&a).await.unwrap();
            assert_eq!(host.publish(&a).await.unwrap(), json!({"decision":"A"}));
            assert_eq!(host.store.list_operations(&a).await.unwrap(), before);
            assert_eq!(
                host.accept("message-a", "changed", true).await,
                Err(Error::Conflict)
            );
            assert_eq!(
                host.accept("message-a", "hello A", false).await,
                Err(Error::PermissionDenied)
            );
            let b = host.accept("message-b", "hello B", true).await.unwrap();
            assert_ne!(a, b);
            assert_eq!(
                host.store.get(&a).await.unwrap().state,
                RunState::Terminal(Terminal::Succeeded)
            );
            reply(&host, &b, "B", 2).await;
            host.publish(&b).await.unwrap();
        }
        "admit" => {
            // Commit admission and exit without any local wake or runner.
            host.accept("message-c", "hello C", true).await.unwrap();
            assert_eq!(host.store.queue_len().await.unwrap(), 1);
        }
        "recover" => {
            let c = host.accept("message-c", "hello C", true).await.unwrap();
            let queued = host.store.dequeue(0).await.unwrap().unwrap();
            assert_eq!(queued.key, format!("{}:{}", c.tenant, c.id));
            reply(&host, &c, "C", 4).await;
            // Exit after runtime completion, before host publication.
        }
        "contracts" => {
            let c = host.accept("message-c", "hello C", true).await.unwrap();
            let before = host.store.list_operations(&c).await.unwrap();
            assert_eq!(before.len(), 1);
            assert_eq!(before[0].request.attempt, 1);
            assert!(before[0].settled);
            host.publish(&c).await.unwrap();
            host.publish(&c).await.unwrap();
            assert_eq!(host.store.list_operations(&c).await.unwrap(), before);
            host.idle().await;
            concurrency(&host).await;
            handoff_and_disconnect(&host).await;
        }
        _ => panic!("unknown chat phase"),
    }
    host.close().await;
}

async fn concurrency(host: &Host) {
    // Actual overlapping input transactions: stable duplicate converges, changed
    // input conflicts; two different IDs receive a durable order at the same basis.
    let (a, duplicate, b) = tokio::join!(
        host.accept("parallel-a", "input A", true),
        host.accept("parallel-a", "input A", true),
        host.accept("parallel-b", "input B", true)
    );
    let a = a.unwrap();
    assert_eq!(a, duplicate.unwrap());
    let b = b.unwrap();
    let mut records = [
        host.store.get(&a).await.unwrap(),
        host.store.get(&b).await.unwrap(),
    ];
    records.sort_by_key(|r| r.sequence);
    assert_eq!(records[1].sequence, records[0].sequence + 1);
    let model = ChatModel::reply("parallel");
    let rt = runtime(host.store.clone(), model.clone(), 0);
    let later = host
        .store
        .claim(&records[1].handle, "later", 0, 100)
        .await
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            rt.drive(&later).await.unwrap().state,
            RunState::Waiting("session predecessor".into())
        );
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    let first = host
        .store
        .claim(&records[0].handle, "first", 0, 100)
        .await
        .unwrap();
    assert_eq!(
        rt.drive(&first).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    let original = host.publish(&first.run).await.unwrap();
    assert!(matches!(rt.drive(&later).await.unwrap().state,
        RunState::Terminal(Terminal::Failed(ref e)) if e.contains("CONTEXT_STALE")));
    assert_eq!(host.publish(&later.run).await, Err(Error::Conflict));
    assert_eq!(host.publish(&first.run).await.unwrap(), original);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert!(
        host.store
            .list_operations(&later.run)
            .await
            .unwrap()
            .is_empty()
    );
    // Repeating the rejected input retrieves its original diagnostic, not a new Run.
    let stale = &records[1];
    assert_eq!(
        host.accept(
            &stale.spec.request_key,
            if stale.spec.request_key == "parallel-a" {
                "input A"
            } else {
                "input B"
            },
            true
        )
        .await
        .unwrap(),
        stale.handle
    );
    host.idle().await;
}

async fn handoff_and_disconnect(host: &Host) {
    let run = host
        .accept("autonomous", "continue without subscriber", true)
        .await
        .unwrap();
    let old = host.store.claim(&run, "old", 0, 10).await.unwrap();
    host.store.wait(&old, "worker handoff", 0).await.unwrap();
    host.store
        .release_slot(&format!("active:{}:{}", run.tenant, run.id))
        .await
        .unwrap();
    let current = host.store.claim(&run, "new", 11, 100).await.unwrap();
    assert!(current.generation > old.generation);
    let mut model = ChatModel::reply("autonomous");
    let gate = Arc::new((Notify::new(), Notify::new()));
    model.gate = Some(gate.clone());
    let model_count = model.calls.clone();
    let rt = runtime(host.store.clone(), model, 11);
    let rt = Arc::new(rt);
    assert_eq!(rt.drive(&old).await, Err(Error::OwnershipLost));
    let (sender, receiver) = tokio::sync::oneshot::channel::<Value>();
    let worker = {
        let rt = rt.clone();
        let claim = current.clone();
        tokio::spawn(async move {
            let record = rt.drive(&claim).await.unwrap();
            assert_eq!(record.state, RunState::Terminal(Terminal::Succeeded));
            assert!(sender.send(record.output.unwrap()).is_err());
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.0.notified())
        .await
        .unwrap();
    // Subscriber disconnects while inference is in flight; worker is independent.
    drop(receiver);
    assert!(host.store.get(&run).await.unwrap().cancel.is_none());
    gate.1.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap();
    let completed = host.store.get(&run).await.unwrap();
    for _ in 0..2 {
        // Duplicate wake after completion, including a replacement worker.
        assert_eq!(
            rt.drive(&current).await.unwrap().completion,
            completed.completion
        );
    }
    let next = host
        .store
        .claim(&run, "replacement", 112, 100)
        .await
        .unwrap();
    let next_rt = runtime(host.store.clone(), ChatModel::reply("must not run"), 112);
    assert_eq!(
        next_rt.drive(&next).await.unwrap().completion,
        completed.completion
    );
    assert_eq!(next_rt.model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(model_count.load(Ordering::SeqCst), 1);
    assert_eq!(host.store.list_operations(&run).await.unwrap().len(), 1);
    assert_eq!(
        host.publish(&run).await.unwrap(),
        json!({"decision":"autonomous"})
    );
    host.idle().await;
}
