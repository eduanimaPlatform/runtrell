// SPDX-License-Identifier: Apache-2.0
use runtrell::{assembly::ValidationChain, *};
use runtrell_postgres::PgStore;
use runtrell_testkit::{BusinessTool, DecisionFinalizer, FakeModel, FakePolicy, TestClock};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;

async fn drive(store: Arc<PgStore>, claim: &Claim, answer: i32, now: u64) -> RunRecord {
    let clock = TestClock::default();
    clock.set(now);
    Runtime {
        store,
        model: FakeModel::scripted(vec![
            ModelReply { text: "progress summary".into(), ..FakeModel::decision(serde_json::Value::Null) },
            FakeModel::decision(json!({"answer":answer})),
        ]),
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(ValidationChain {
            validators: vec![],
            schema: json!({"type":"object","properties":{"answer":{"type":"integer","minimum":1}},"required":["answer"]}),
            repair: RepairPolicy::default(),
        }),
        finalizer: Arc::new(DecisionFinalizer),
    }.drive(claim).await.unwrap()
}

#[tokio::test]
async fn context_prefix_survives_process_restart() {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated database required");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let schema = format!("context_{}", uuid::Uuid::new_v4().simple());
    let store = Arc::new(
        PgStore::open(pool, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("t", "resume", account, "original occurrence");
    spec.compaction = Some(CompactionPolicy {
        min_remaining_tokens: 2000,
    });
    let run = store.start(spec).await.unwrap();
    let claim = store.claim(&run, "first", 0, 100).await.unwrap();
    let pending = drive(store.clone(), &claim, 0, 0).await;
    assert_eq!(pending.state, RunState::Waiting("validation repair".into()));
    let before = pending.checkpoint.unwrap();
    let originals = before.payload["context"]["facts"].as_array().unwrap();
    assert_eq!(originals.len(), 1);
    assert!(before.payload["context"]["summary"].as_str().is_some());
    let archived = before.payload["context_protocol_len"].as_u64().unwrap() as usize;
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "resume_context_in_child_process", "--nocapture"])
        .env("RUNTRELL_CONTEXT_SCHEMA", &schema)
        .env("RUNTRELL_CONTEXT_RUN", &run.id)
        .status()
        .unwrap();
    assert!(status.success());
    let final_run = store.get(&run).await.unwrap();
    assert_eq!(final_run.state, RunState::Terminal(Terminal::Succeeded));
    let cp = final_run.checkpoint.unwrap();
    let facts = cp.payload["context"]["facts"].as_array().unwrap();
    assert_eq!(&facts[..originals.len()], originals);
    assert_eq!(
        facts
            .iter()
            .filter(|item| item["value"]["text"] == "original occurrence")
            .count(),
        1
    );
    assert_eq!(
        facts.len(),
        originals.len() + before.messages.len() - archived
    );
    assert_eq!(
        store
            .list_operations(&run)
            .await
            .unwrap()
            .iter()
            .filter(|op| op.request.kind == OperationKind::Compaction)
            .count(),
        2
    );
    for (fact, message) in facts[originals.len()..]
        .iter()
        .zip(&before.messages[archived..])
    {
        assert_eq!(fact["value"], serde_json::to_value(message).unwrap());
    }
}

#[tokio::test]
async fn no_progress_survives_process_restart_and_compaction() {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated database required");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let schema = format!("repair_{}", uuid::Uuid::new_v4().simple());
    let store = Arc::new(
        PgStore::open(pool, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("t", "repair-process", account, "repair");
    spec.compaction = Some(CompactionPolicy {
        min_remaining_tokens: 2000,
    });
    let run = store.start(spec).await.unwrap();
    let claim = store.claim(&run, "first", 0, 100).await.unwrap();
    let pending = drive(store.clone(), &claim, 0, 0).await;
    assert_eq!(
        pending.checkpoint.unwrap().payload["repair"]["no_progress"]["occurrences"],
        1
    );
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "resume_context_in_child_process", "--nocapture"])
        .env("RUNTRELL_CONTEXT_SCHEMA", &schema)
        .env("RUNTRELL_CONTEXT_RUN", &run.id)
        .env("RUNTRELL_CONTEXT_NO_PROGRESS", "1")
        .status()
        .unwrap();
    assert!(status.success());
    let stopped = store.get(&run).await.unwrap();
    assert!(
        matches!(stopped.state, RunState::Terminal(Terminal::Failed(ref reason)) if reason.contains("VALIDATION_NO_PROGRESS"))
    );
    assert_eq!(
        stopped.checkpoint.unwrap().payload["repair"]["no_progress"]["occurrences"],
        3
    );
    let operations = store.list_operations(&run).await.unwrap();
    assert_eq!(
        operations
            .iter()
            .filter(|op| op.request.kind == OperationKind::Compaction)
            .count(),
        3
    );
    assert!(operations.iter().all(|op| op.settled));
}

#[tokio::test]
async fn resume_context_in_child_process() {
    let Ok(schema) = std::env::var("RUNTRELL_CONTEXT_SCHEMA") else {
        return;
    };
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let store = Arc::new(
        PgStore::open(pool, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let run = RunHandle {
        tenant: "t".into(),
        id: std::env::var("RUNTRELL_CONTEXT_RUN").unwrap(),
    };
    if std::env::var("RUNTRELL_CONTEXT_NO_PROGRESS").is_ok() {
        for turn in 1..=2 {
            let now = turn * 101;
            let claim = store.claim(&run, "child", now, 100).await.unwrap();
            let result = drive(store.clone(), &claim, 0, now).await;
            if turn == 1 {
                assert_eq!(result.state, RunState::Waiting("validation repair".into()));
            } else {
                assert!(
                    matches!(result.state, RunState::Terminal(Terminal::Failed(ref reason)) if reason.contains("VALIDATION_NO_PROGRESS"))
                );
            }
        }
        return;
    }
    let claim = store.claim(&run, "second", 101, 100).await.unwrap();
    let completed = drive(store, &claim, 2, 101).await;
    assert_eq!(completed.state, RunState::Terminal(Terminal::Succeeded));
}
