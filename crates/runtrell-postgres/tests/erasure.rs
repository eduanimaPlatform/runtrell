// SPDX-License-Identifier: Apache-2.0
use runtrell::{
    data::*,
    execution::{ExecutionRecord, ExecutionRequest},
    *,
};
use runtrell_postgres::{PgStore, RunErasureState};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const BODY: &str = "RT_ERASE_PRIVATE_ANSWER_9b74";
struct Env {
    pool: PgPool,
    store: PgStore,
    schema: String,
    data: DataStore,
}
async fn env() -> Env {
    let pool = PgPoolOptions::new()
        .max_connections(12)
        .connect(&std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("isolated test database"))
        .await
        .unwrap();
    let schema = format!("erase_{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::initialize(pool.clone(), &schema, Capacity::default())
        .await
        .unwrap();
    Env {
        pool,
        store,
        schema,
        data: DataStore::memory(StoragePolicy::default()),
    }
}
async fn run(e: &Env, key: &str) -> (RunSpec, RunHandle, Claim) {
    let account = e
        .store
        .create_account("t", Amount::new(10_000, 10_000), None)
        .await
        .unwrap();
    let spec = RunSpec::new("t", key, account, BODY);
    let h = e.store.start(spec.clone()).await.unwrap();
    let claim = e.store.claim(&h, "worker", 0, 100).await.unwrap();
    (spec, h, claim)
}
fn access(h: &RunHandle, agent: &str) -> Access {
    Access::owner(Namespace {
        owner: h.clone(),
        agent: agent.into(),
    })
}
async fn finish(e: &Env, c: &Claim) {
    e.store
        .terminate(c, Terminal::Failed(BODY.into()), 5)
        .await
        .unwrap();
}
async fn scan(e: &Env) -> Vec<Value> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname=$1 ORDER BY tablename",
    )
    .bind(&e.schema)
    .fetch_all(&e.pool)
    .await
    .unwrap();
    let mut rows = vec![];
    for table in tables {
        rows.extend(
            sqlx::query_scalar::<_, Value>(&format!(
                "SELECT to_jsonb(t) FROM {}.\"{}\" t",
                e.schema, table
            ))
            .fetch_all(&e.pool)
            .await
            .unwrap(),
        );
    }
    rows
}

#[tokio::test]
async fn erases_all_database_bodies_and_payloads_preserving_accounting_and_refusing_replay() {
    let e = env().await;
    let (spec, h, c) = run(&e, "complete").await;
    let cp = Checkpoint {
        schema: 1,
        revision: 0,
        payload: json!({"private": BODY}),
        messages: vec![Message::text("tool", BODY)],
        cursor: 0,
    };
    e.store
        .checkpoint(&c, e.store.get(&h).await.unwrap().revision, cp, 1)
        .await
        .unwrap();
    let op = e
        .store
        .begin(
            &c,
            DispatchRequest::new(
                "tool",
                OperationKind::Tool,
                BODY,
                json!({"answer": BODY}),
                Amount::new(20, 20),
            ),
            2,
        )
        .await
        .unwrap();
    e.store.mark_sent(&c, &op.id, 3).await.unwrap();
    e.store
        .record(
            &c,
            &op.id,
            ExternalResult::Applied {
                physical_evidence: None,
                output: json!({"result": BODY}),
                usage: Usage::measured(2, 3, 4),
                external_id: Some("provider-receipt".into()),
            },
            4,
        )
        .await
        .unwrap();
    for agent in ["first", "second"] {
        e.data
            .write(&access(&h, agent), "answer.txt", None, BODY.as_bytes())
            .await
            .unwrap();
    }
    let execution_context = ToolContext {
        recovery_proof: None,
        claim: c.clone(),
        principal: "host".into(),
        operation: "reclaimed-exec".into(),
        attempt: 1,
        deadline: None,
        permission: "execute".into(),
        account_binding: "host".into(),
    };
    let mut execution = execution::ExecutionLedger::insert(
        &e.store,
        ExecutionRecord::intent(
            &execution_context,
            ExecutionRequest::process("test-only", vec![BODY.into()]),
        ),
    )
    .await
    .unwrap();
    execution.observe(execution::Observation {
        execution: ExecutionRef {
            owner: h.clone(),
            id: "reclaimed-exec".into(),
            runtime_version: "test".into(),
        },
        state: execution::ExecutionState::Destroyed,
        result: Some(json!(BODY)),
        failure: Some(BODY.into()),
        cost_micro_usd: Some(0),
        released_resources: [
            execution::Resource::Cpu,
            execution::Resource::Memory,
            execution::Resource::Pid,
            execution::Resource::Disk,
            execution::Resource::Volume,
            execution::Resource::ExternalCost,
        ]
        .into_iter()
        .collect(),
    });
    assert!(execution.reclaimed());
    execution::ExecutionLedger::cas(&e.store, 0, execution)
        .await
        .unwrap();
    // A stale worker observation is another independently durable body copy.
    let mut stale = c.clone();
    stale.generation += 1;
    assert!(
        e.store
            .record(
                &stale,
                &op.id,
                ExternalResult::Applied {
                    physical_evidence: None,
                    output: json!({"late": BODY}),
                    usage: Usage::measured(2, 3, 4),
                    external_id: None
                },
                4
            )
            .await
            .is_err()
    );
    finish(&e, &c).await;
    let account = e.store.account(&spec.account).await.unwrap();
    assert!(scan(&e).await.iter().any(|v| v.to_string().contains(BODY)));
    assert_eq!(e.store.erase_run(&h, &e.data, 200).await, Ok(()));
    assert_eq!(
        e.store.erasure_status(&h).await.unwrap(),
        Some(RunErasureState::Done)
    );
    assert!(scan(&e).await.iter().all(|v| !v.to_string().contains(BODY)));
    assert_eq!(e.store.account(&spec.account).await.unwrap(), account);
    let entries: Vec<Value> = sqlx::query_scalar(&format!(
        "SELECT to_jsonb(b) FROM {}.budget_entries b WHERE attempt_id=$1",
        e.schema
    ))
    .bind(&op.id)
    .fetch_all(&e.pool)
    .await
    .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["usage"]["input"], 2);
    assert_eq!(entries[0]["usage"]["output"], 3);
    assert_eq!(entries[0]["usage"]["micro_usd"], 4);
    assert_eq!(entries[0]["settled"], true);
    let before = scan(&e).await;
    assert_eq!(e.store.erase_run(&h, &e.data, 300).await, Ok(()));
    assert_eq!(scan(&e).await, before);
    assert_eq!(e.store.get(&h).await, Err(Error::Closed));
    assert_eq!(e.store.start(spec.clone()).await, Err(Error::Closed));
    assert_eq!(e.store.find_request(&spec).await, Err(Error::Closed));
    assert_eq!(
        e.store.claim(&h, "replay", 300, 100).await,
        Err(Error::Closed)
    );
    assert_eq!(
        e.store
            .resume_held(
                &h,
                0,
                OwnerCommand {
                    key: "resume-erased".into(),
                    request_digest: "digest".into()
                },
                300
            )
            .await,
        Err(Error::Closed)
    );
    assert_eq!(
        e.data.snapshot(&access(&h, "third")).await.unwrap_err(),
        Error::Closed
    );
    // Delayed observations and independent execution paths cannot reintroduce data.
    assert_eq!(
        e.store
            .record(
                &c,
                &op.id,
                ExternalResult::Applied {
                    physical_evidence: None,
                    output: json!({"late": BODY}),
                    usage: Usage::measured(2, 3, 4),
                    external_id: None
                },
                300
            )
            .await,
        Err(Error::Closed)
    );
    let context = ToolContext {
        recovery_proof: None,
        claim: c,
        principal: "host".into(),
        operation: "exec".into(),
        attempt: 1,
        deadline: None,
        permission: "execute".into(),
        account_binding: "host".into(),
    };
    let record = ExecutionRecord::intent(
        &context,
        ExecutionRequest::process("test-only", vec![BODY.into()]),
    );
    assert_eq!(
        execution::ExecutionLedger::insert(&e.store, record)
            .await
            .unwrap_err(),
        Error::Closed
    );
    assert!(scan(&e).await.iter().all(|v| !v.to_string().contains(BODY)));
}

#[tokio::test]
async fn active_reserved_unknown_and_live_owner_runs_are_unchanged_on_refusal() {
    for mode in ["running", "reserved", "unknown", "live-owner", "execution"] {
        let e = env().await;
        let (spec, h, c) = run(&e, mode).await;
        if mode == "reserved" || mode == "unknown" {
            let op = e
                .store
                .begin(
                    &c,
                    DispatchRequest::new(
                        "pending",
                        OperationKind::Tool,
                        "tool",
                        json!({"answer": BODY}),
                        Amount::new(20, 20),
                    ),
                    1,
                )
                .await
                .unwrap();
            if mode == "unknown" {
                e.store.mark_sent(&c, &op.id, 2).await.unwrap();
                e.store
                    .record(&c, &op.id, ExternalResult::Unknown, 3)
                    .await
                    .unwrap();
            }
            finish(&e, &c).await;
            // Isolate the attempt safety check from the aggregate cleanup checks.
            e.store
                .cleanup(&c, Progress::Confirmed, Progress::Confirmed, 6)
                .await
                .unwrap();
        } else if mode != "running" {
            finish(&e, &c).await;
        }
        if mode == "execution" {
            let context = ToolContext {
                recovery_proof: None,
                claim: c.clone(),
                principal: "host".into(),
                operation: "exec".into(),
                attempt: 1,
                deadline: None,
                permission: "execute".into(),
                account_binding: "host".into(),
            };
            execution::ExecutionLedger::insert(
                &e.store,
                ExecutionRecord::intent(
                    &context,
                    ExecutionRequest::process("test-only", vec![BODY.into()]),
                ),
            )
            .await
            .unwrap();
        }
        let before = scan(&e).await;
        assert!(
            matches!(
                e.store
                    .erase_run(&h, &e.data, if mode == "live-owner" { 10 } else { 200 })
                    .await,
                Err(Error::Pending(_))
            ),
            "{mode}"
        );
        assert_eq!(scan(&e).await, before, "{mode}");
        assert_eq!(e.store.get(&h).await.unwrap().spec, spec);
    }
}

struct FailPayload {
    data: DataStore,
    fail: AtomicBool,
}
impl RunPayloadStore for FailPayload {
    async fn erase_payloads(&self, h: &RunHandle) -> Result<()> {
        self.data.erase_run(h)?;
        if self.fail.swap(false, Ordering::SeqCst) {
            Err(Error::Storage)
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn sql_rollback_and_external_failure_retry_preserve_durable_erasure_fence() {
    let e = env().await;
    let (_, h, c) = run(&e, "retry").await;
    finish(&e, &c).await;
    e.data
        .write(&access(&h, "agent"), "answer", None, BODY.as_bytes())
        .await
        .unwrap();
    let before = scan(&e).await;
    e.store.fail_next_write();
    assert_eq!(
        e.store.erase_run(&h, &e.data, 200).await,
        Err(Error::Storage)
    );
    assert_eq!(scan(&e).await, before);
    let payloads = FailPayload {
        data: e.data.clone(),
        fail: AtomicBool::new(true),
    };
    assert_eq!(
        e.store.erase_run(&h, &payloads, 200).await,
        Err(Error::Storage)
    );
    assert_eq!(
        e.store.erasure_status(&h).await.unwrap(),
        Some(RunErasureState::Pending)
    );
    let reopened = PgStore::connect(e.pool.clone(), &e.schema, Capacity::default())
        .await
        .unwrap();
    assert_eq!(reopened.get(&h).await, Err(Error::Closed));
    assert_eq!(reopened.erase_run(&h, &payloads, 300).await, Ok(()));
    assert_eq!(
        reopened.erasure_status(&h).await.unwrap(),
        Some(RunErasureState::Done)
    );
}

struct CrashPayload {
    data: DataStore,
}
impl RunPayloadStore for CrashPayload {
    async fn erase_payloads(&self, h: &RunHandle) -> Result<()> {
        use std::io::Write;
        self.data.erase_run(h)?;
        println!("RT_ERASE_CRASH_READY");
        std::io::stdout().flush().unwrap();
        std::future::pending().await
    }
}
#[tokio::test]
#[ignore = "test-only OS child, invoked and killed by crash_parent"]
async fn crash_child() {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let schema = std::env::var("RT_ERASE_SCHEMA").unwrap();
    let store = PgStore::connect(pool, &schema, Capacity::default())
        .await
        .unwrap();
    let run: RunHandle = serde_json::from_str(&std::env::var("RT_ERASE_RUN").unwrap()).unwrap();
    let data = DataStore::local(
        std::path::Path::new(&std::env::var("RT_ERASE_DATA").unwrap()),
        StoragePolicy::default(),
    )
    .unwrap();
    store
        .erase_run(&run, &CrashPayload { data }, 200)
        .await
        .unwrap();
}
#[tokio::test]
async fn crash_parent() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    let e = env().await;
    let (_, h, c) = run(&e, "process-crash").await;
    finish(&e, &c).await;
    let dir = std::env::temp_dir().join(format!("rt-erase-data-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let data = DataStore::local(&dir, StoragePolicy::default()).unwrap();
    data.write(&access(&h, "agent"), "answer", None, BODY.as_bytes())
        .await
        .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--ignored", "--nocapture"])
        .env("RT_ERASE_SCHEMA", &e.schema)
        .env("RT_ERASE_RUN", serde_json::to_string(&h).unwrap())
        .env("RT_ERASE_DATA", &dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, recv) = tokio::sync::oneshot::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.unwrap().contains("RT_ERASE_CRASH_READY") {
                let _ = send.send(());
                return;
            }
        }
    });
    let ready = tokio::time::timeout(Duration::from_secs(20), recv).await;
    child.kill().unwrap();
    child.wait().unwrap();
    reader.join().unwrap();
    ready
        .expect("child did not reach durable payload erasure")
        .unwrap();
    assert_eq!(
        e.store.erasure_status(&h).await.unwrap(),
        Some(RunErasureState::Pending)
    );
    let reopened = DataStore::local(&dir, StoragePolicy::default()).unwrap();
    assert_eq!(
        reopened.snapshot(&access(&h, "agent")).await.unwrap_err(),
        Error::Closed
    );
    assert_eq!(e.store.get(&h).await, Err(Error::Closed));
    e.store.erase_run(&h, &reopened, 300).await.unwrap();
    assert_eq!(
        e.store.erasure_status(&h).await.unwrap(),
        Some(RunErasureState::Done)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn tree_erasure_waits_for_children_then_erases_every_owned_run() {
    let e = env().await;
    let (_, root, parent) = run(&e, "tree").await;
    let account = e.store.get(&root).await.unwrap().spec.account;
    let child = e
        .store
        .admit_child(
            &parent,
            "child-op",
            RunSpec::new("t", "child", account, BODY),
            1,
        )
        .await
        .unwrap();
    let child_claim = e.store.claim(&child, "child-worker", 2, 50).await.unwrap();
    e.data
        .write(&access(&child, "solver"), "answer", None, BODY.as_bytes())
        .await
        .unwrap();
    finish(&e, &parent).await;
    assert!(matches!(
        e.store.erase_run(&root, &e.data, 200).await,
        Err(Error::Pending(_))
    ));
    assert_eq!(e.store.erasure_status(&root).await.unwrap(), None);
    finish(&e, &child_claim).await;
    let op = e.store.operation(&root, "child-op").await.unwrap().unwrap();
    e.store
        .record(
            &parent,
            &op.id,
            ExternalResult::Applied {
                physical_evidence: None,
                output: json!({"child": child}),
                usage: Usage::measured(0, 0, 0),
                external_id: None,
            },
            7,
        )
        .await
        .unwrap();
    e.store
        .cleanup(&parent, Progress::Confirmed, Progress::Confirmed, 8)
        .await
        .unwrap();
    e.store.erase_run(&root, &e.data, 200).await.unwrap();
    for h in [&root, &child] {
        assert_eq!(
            e.store.erasure_status(h).await.unwrap(),
            Some(RunErasureState::Done)
        );
        assert_eq!(e.store.get(h).await, Err(Error::Closed));
        assert_eq!(
            e.data.snapshot(&access(h, "new-agent")).await.unwrap_err(),
            Error::Closed
        );
    }
    assert!(scan(&e).await.iter().all(|v| !v.to_string().contains(BODY)));
}

#[tokio::test]
async fn independent_scheduler_reservation_blocks_erasure() {
    let e = env().await;
    let (_, h, c) = run(&e, "slot").await;
    finish(&e, &c).await;
    e.store
        .acquire(SlotRequest {
            key: "sandbox-reservation".into(),
            tenant: h.tenant.clone(),
            run: h.id.clone(),
            kind: OperationKind::Sandbox,
        })
        .await
        .unwrap();
    assert!(matches!(
        e.store.erase_run(&h, &e.data, 200).await,
        Err(Error::Pending(_))
    ));
    assert_eq!(e.store.erasure_status(&h).await.unwrap(), None);
    e.store.release_slot("sandbox-reservation").await.unwrap();
    e.store.erase_run(&h, &e.data, 200).await.unwrap();
    assert_eq!(
        e.store
            .acquire(SlotRequest {
                key: "resurrection".into(),
                tenant: h.tenant,
                run: h.id,
                kind: OperationKind::Sandbox
            })
            .await,
        Err(Error::Closed)
    );
}

struct PausePayload {
    entered: tokio::sync::Notify,
    proceed: tokio::sync::Notify,
    data: DataStore,
}
impl RunPayloadStore for PausePayload {
    async fn erase_payloads(&self, h: &RunHandle) -> Result<()> {
        self.entered.notify_one();
        self.proceed.notified().await;
        self.data.erase_run(h)
    }
}
#[tokio::test]
async fn pending_deletion_fences_late_writers_before_external_erasure_completes() {
    let e = env().await;
    let (_, h, c) = run(&e, "pending-fence").await;
    let op = e
        .store
        .begin(
            &c,
            DispatchRequest::new(
                "tool",
                OperationKind::Tool,
                "tool",
                json!(BODY),
                Amount::new(2, 2),
            ),
            1,
        )
        .await
        .unwrap();
    e.store
        .record(
            &c,
            &op.id,
            ExternalResult::Applied {
                physical_evidence: None,
                output: json!(BODY),
                usage: Usage::measured(1, 1, 1),
                external_id: None,
            },
            2,
        )
        .await
        .unwrap();
    finish(&e, &c).await;
    let payloads = std::sync::Arc::new(PausePayload {
        entered: tokio::sync::Notify::new(),
        proceed: tokio::sync::Notify::new(),
        data: e.data.clone(),
    });
    let store = e.store.clone();
    let handle = h.clone();
    let p = payloads.clone();
    let eraser = tokio::spawn(async move { store.erase_run(&handle, &*p, 200).await });
    tokio::time::timeout(Duration::from_secs(5), payloads.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        e.store.erasure_status(&h).await.unwrap(),
        Some(RunErasureState::Pending)
    );
    assert_eq!(
        e.store
            .record(
                &c,
                &op.id,
                ExternalResult::Applied {
                    physical_evidence: None,
                    output: json!(BODY),
                    usage: Usage::measured(1, 1, 1),
                    external_id: None
                },
                201
            )
            .await,
        Err(Error::Closed)
    );
    assert_eq!(e.store.claim(&h, "late", 201, 10).await, Err(Error::Closed));
    payloads.proceed.notify_one();
    assert_eq!(eraser.await.unwrap(), Ok(()));
    assert!(scan(&e).await.iter().all(|v| !v.to_string().contains(BODY)));
}

#[tokio::test]
async fn opaque_tenant_identity_can_be_erased_even_without_data_namespaces() {
    let e = env().await;
    let tenant = "organization:大学/部門";
    let account = e
        .store
        .create_account(tenant, Amount::new(100, 100), None)
        .await
        .unwrap();
    let h = e
        .store
        .start(RunSpec::new(tenant, "opaque-identity", account, BODY))
        .await
        .unwrap();
    e.store.cancel(&h, "deleted").await.unwrap();
    e.store.erase_run(&h, &e.data, 200).await.unwrap();
    assert_eq!(
        e.store.erasure_status(&h).await.unwrap(),
        Some(RunErasureState::Done)
    );
    assert!(scan(&e).await.iter().all(|v| !v.to_string().contains(BODY)));
}

fn svg_usage() -> Box<SvgUsage> {
    Box::new(SvgUsage {
        model: Some("svg-author".into()),
        input: Some(1),
        output: Some(1),
        reasoning: Some(0),
        total: Some(2),
        reported: json!({"model":"svg-author", "input_tokens":1, "output_tokens":1, "reasoning_tokens":0, "total_tokens":2, "extra":BODY}),
        micro_usd: Some(0),
        price: Some(SvgAuthorPrice {
            model: "svg-author".into(),
            input: 0,
            output: 0,
            reasoning_in_output: true,
        }),
        unknown_reason: None,
    })
}

#[tokio::test]
async fn erases_raw_svg_usage_from_recovered_attempts_and_owner_receipts_only() {
    use runtrell_testkit::settlement_contracts::{rates, refined, stored_usage};
    let e = env().await;
    let account = e
        .store
        .create_account("owner", Amount::new(100_000, 1_000_000), None)
        .await
        .unwrap();
    let h = e
        .store
        .start(RunSpec::new(
            "owner",
            "svg-accounting",
            account.clone(),
            BODY,
        ))
        .await
        .unwrap();
    let c = e.store.claim(&h, "worker", 0, 10).await.unwrap();
    let mut request = DispatchRequest::new(
        "image",
        OperationKind::Tool,
        "draw",
        json!({}),
        Amount::new(1_000, 50_000),
    );
    request.price = Some(rates());
    let op = e.store.begin(&c, request, 0).await.unwrap();
    let mut usage = stored_usage();
    usage.svg_usage = Some(svg_usage());
    assert_eq!(
        e.store
            .record(
                &c,
                &op.id,
                ExternalResult::Applied {
                    physical_evidence: None,
                    output: json!(BODY),
                    usage: usage.clone(),
                    external_id: None
                },
                0
            )
            .await,
        Err(Error::UsageUnknown)
    );
    e.store.wait(&c, OPERATOR_ACTION_WAIT, 0).await.unwrap();
    let receipt = e
        .store
        .settle_recovered(
            &h,
            &op.id,
            SettlementRecovery {
                basis: "svg-accounting-rule".into(),
                usage: refined(usage),
                price: rates(),
            },
            OwnerCommand {
                key: "svg-recovery-command".into(),
                request_digest: "svg-request-digest".into(),
            },
            11,
        )
        .await
        .unwrap();
    let mut observations = vec![];
    for malformed in [json!(999), json!(BODY), json!(false), json!(-1), json!(1.0)] {
        let mut usage = Usage::measured(1, 1, 1);
        let mut svg = svg_usage();
        svg.reported["input_tokens"] = malformed;
        usage.svg_usage = Some(svg);
        assert_eq!(usage.amount(), Err(Error::UsageUnknown));
        observations.push(usage);
    }
    sqlx::query(&format!(
        "UPDATE {}.attempts SET usage_observations=$2 WHERE id=$1",
        e.schema
    ))
    .bind(&op.id)
    .bind(json!(observations))
    .execute(&e.pool)
    .await
    .unwrap();
    let saved_account = e.store.account(&account).await.unwrap();
    assert!(serde_json::to_string(&receipt).unwrap().contains(BODY));
    // Normal writes remain immutable even when an update would just remove raw JSON.
    assert!(sqlx::query(&format!("UPDATE {}.owner_receipts SET receipt=jsonb_set(receipt,'{{at}}','99') WHERE key='svg-recovery-command'", e.schema)).execute(&e.pool).await.is_err());
    e.store.cancel(&h, "deleted").await.unwrap();
    e.store.erase_run(&h, &e.data, 200).await.unwrap();
    assert_eq!(e.store.account(&account).await.unwrap(), saved_account);
    let after = e
        .store
        .owner_receipt("svg-recovery-command")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.command, receipt.command);
    assert_eq!(after.at, receipt.at);
    let (OwnerResult::Settled(before), OwnerResult::Settled(after_result)) =
        (&receipt.result, &after.result)
    else {
        panic!("settlement receipt required")
    };
    assert_eq!(after_result.amount, before.amount);
    assert_eq!(after_result.settled.amount(), before.settled.amount());
    assert_eq!(after_result.settled.total(), before.settled.total());
    let retained: Value = sqlx::query_scalar(&format!(
        "SELECT usage_observations FROM {}.attempts WHERE id=$1",
        e.schema
    ))
    .bind(&op.id)
    .fetch_one(&e.pool)
    .await
    .unwrap();
    let retained: Vec<Usage> = serde_json::from_value(retained).unwrap();
    assert_eq!(retained.len(), observations.len());
    for (old, new) in observations.iter().zip(retained.iter()) {
        assert_eq!(new.amount(), old.amount());
        let sql_redacted: Value =
            sqlx::query_scalar(&format!("SELECT {}.erase_svg_reported($1)", e.schema))
                .bind(json!(old.svg_usage))
                .fetch_one(&e.pool)
                .await
                .unwrap();
        assert_eq!(new.svg_usage.as_ref().unwrap().reported, sql_redacted);
    }

    assert_eq!(after_result.price, before.price);
    assert_eq!(
        after_result.settled.svg_usage.as_ref().unwrap().input,
        Some(1)
    );
    assert!(scan(&e).await.iter().all(|v| !v.to_string().contains(BODY)));
    assert!(sqlx::query(&format!("UPDATE {}.owner_receipts SET receipt=jsonb_set(receipt,'{{at}}','99') WHERE key='svg-recovery-command'", e.schema)).execute(&e.pool).await.is_err());
    assert!(
        sqlx::query(&format!(
            "DELETE FROM {}.owner_receipts WHERE key='svg-recovery-command'",
            e.schema
        ))
        .execute(&e.pool)
        .await
        .is_err()
    );
}
