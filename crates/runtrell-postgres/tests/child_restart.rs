// SPDX-License-Identifier: Apache-2.0
use runtrell::*;
use runtrell_postgres::PgStore;
use runtrell_testkit::{
    BusinessTool, DecisionFinalizer, DecisionValidator, FakeModel, FakePolicy, TestClock,
};
use sqlx::postgres::PgPoolOptions;
use std::io::{BufRead, BufReader};
mod support;
use std::sync::Arc;

#[test]
fn child_restart_across_processes() {
    let phase = std::env::var("RUNTRELL_CHILD_PHASE").ok();
    let schema = std::env::var("RUNTRELL_CHILD_SCHEMA")
        .unwrap_or_else(|_| format!("child_{}", uuid::Uuid::new_v4().simple()));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let pool = PgPoolOptions::new()
            .connect(
                &std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated database required"),
            )
            .await
            .unwrap();
        let store_pool = pool.clone();
        let store = Arc::new(
            PgStore::open(pool, &schema, Capacity::default())
                .await
                .unwrap(),
        );
        if let Some(phase) = phase {
            let parent = RunHandle {
                tenant: "t".into(),
                id: std::env::var("RUNTRELL_CHILD_PARENT").unwrap(),
            };
            let account = std::env::var("RUNTRELL_CHILD_ACCOUNT").unwrap();
            let spec = RunSpec::new("t", &parent.id, account.clone(), "child");
            let now = if phase == "resume" { 101 } else { 0 };
            let claim = store.claim(&parent, &phase, now, 100).await.unwrap();
            let clock = TestClock::default();
            clock.set(now);
            let runtime = Runtime {
                store: Arc::new(support::TestPg(
                    store.as_ref().clone(),
                    Some((phase.clone(), store_pool.clone())),
                )),
                model: FakeModel::scripted(vec![
                    FakeModel::decision(serde_json::json!({"decision":"no_action"})),
                    FakeModel::decision(serde_json::json!({"decision":"no_action"})),
                ]),
                tools: Vec::<BusinessTool>::new(),
                policy: FakePolicy::default(),
                clock,
                validator: Arc::new(DecisionValidator::default()),
                finalizer: Arc::new(DecisionFinalizer),
            };
            // "before" and "created" pause inside this actual spawn_child call.
            let child = runtime
                .spawn_child(&claim, "child-op", spec.clone())
                .await
                .unwrap();
            let op = store.operation(&parent, "child-op").await.unwrap().unwrap();
            assert_ne!(child.id, op.id);
            assert_eq!(op.send_count, 0);
            assert_eq!(
                runtime.spawn_child(&claim, "child-op", spec).await.unwrap(),
                child
            );
            if !matches!(
                store.get(&child).await.unwrap().state,
                RunState::Terminal(_)
            ) {
                assert!(matches!(
                    runtime.join_child(&claim, "child-op").await,
                    Err(Error::Pending(_))
                ));
                let child_claim = store.claim(&child, "child-worker", now, 100).await.unwrap();
                assert_eq!(
                    runtime.drive(&child_claim).await.unwrap().state,
                    RunState::Terminal(Terminal::Succeeded)
                );
            }
            // "terminal" pauses inside join before parent adoption/settlement.
            let adopted = runtime.join_child(&claim, "child-op").await.unwrap();
            assert_eq!(adopted.handle, child);
            assert!(adopted.completion.is_some());
            assert_eq!(
                runtime.join_child(&claim, "child-op").await.unwrap(),
                adopted
            );
            let finished = runtime.drive(&claim).await.unwrap();
            assert_eq!(finished.state, RunState::Terminal(Terminal::Succeeded));
            assert_eq!(
                runtime.drive(&claim).await.unwrap().completion,
                finished.completion
            );
            assert!(
                store
                    .operation(&parent, "child-op")
                    .await
                    .unwrap()
                    .unwrap()
                    .settled
            );
            assert_eq!(
                store.account(&account).await.unwrap().used,
                Amount::new(8, 8)
            );
            assert_eq!(runtime.store.slot_count(OperationKind::ChildRun).await, 0);
            assert_eq!(runtime.store.queue_len().await, 0);
        } else {
            for phase in ["before", "uncommitted", "created", "terminal"] {
                let account = store
                    .create_account("t", Amount::new(1000, 1000), None)
                    .await
                    .unwrap();
                let parent = store
                    .start(RunSpec::new("t", phase, account.clone(), "parent"))
                    .await
                    .unwrap();
                let mut original_child = None;
                let mut original_completion = None;
                let mut aborted_child = None;
                let mut pids = std::collections::BTreeSet::new();
                for step in [phase, "resume"] {
                    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", "child_restart_across_processes", "--nocapture"])
                        .env("RUNTRELL_CHILD_SCHEMA", &schema)
                        .env("RUNTRELL_CHILD_PHASE", step)
                        .env("RUNTRELL_CHILD_PARENT", &parent.id)
                        .env("RUNTRELL_CHILD_ACCOUNT", &account)
                        .stdout(if step == "resume" {
                            std::process::Stdio::inherit()
                        } else {
                            std::process::Stdio::piped()
                        })
                        .spawn()
                        .unwrap();
                    assert!(pids.insert(child.id()));
                    if step != "resume" {
                        let mut reader = BufReader::new(child.stdout.take().unwrap());
                        let mut line = String::new();
                        loop {
                            line.clear();
                            assert_ne!(
                                reader.read_line(&mut line).unwrap(),
                                0,
                                "worker exited before stop point"
                            );
                            if let Some(receipt) = line.strip_prefix("RUNTRELL_CHILD_READY ") {
                                let id = receipt.trim();
                                if id != "-" {
                                    let handle = RunHandle {
                                        tenant: "t".into(),
                                        id: id.into(),
                                    };
                                    if phase == "uncommitted" {
                                        aborted_child = Some(handle);
                                    } else {
                                        original_child = Some(handle);
                                    }
                                }
                                break;
                            }
                        }
                        let op = store.operation(&parent, "child-op").await.unwrap();
                        if phase == "before" || phase == "uncommitted" {
                            assert!(op.is_none());
                            assert_eq!(store.slot_count(OperationKind::ChildRun).await.unwrap(), 0);
                        } else {
                            let op = op.unwrap();
                            assert!(!op.settled);
                            assert_eq!(op.send_count, 0);
                            let Some(ExternalResult::Accepted { external_id }) = op.result else {
                                panic!("child receipt missing");
                            };
                            let handle = RunHandle {
                                tenant: "t".into(),
                                id: external_id,
                            };
                            let record = store.get(&handle).await.unwrap();
                            assert_eq!(Some(&handle), original_child.as_ref());
                            original_completion = record.completion.clone();
                            assert_eq!(record.spec.parent.as_ref(), Some(&parent));
                            assert_eq!(
                                matches!(record.state, RunState::Terminal(Terminal::Succeeded)),
                                phase == "terminal"
                            );
                            assert_eq!(store.slot_count(OperationKind::ChildRun).await.unwrap(), 1);
                        }
                        child.kill().unwrap();
                        let status = child.wait().unwrap();
                        assert!(!status.success());
                        #[cfg(unix)]
                        {
                            use std::os::unix::process::ExitStatusExt;
                            assert_eq!(status.signal(), Some(9));
                        }
                        if phase == "before" || phase == "uncommitted" {
                            // Lock acquisition is the synchronization receipt that the killed
                            // connection's open transaction has rolled back; no sleeps/polling.
                            let mut tx = store_pool.begin().await.unwrap();
                            sqlx::query(&format!(
                                "SELECT id FROM {schema}.runs WHERE id=$1 FOR UPDATE"
                            ))
                            .bind(&parent.id)
                            .execute(&mut *tx)
                            .await
                            .unwrap();
                            tx.rollback().await.unwrap();
                            assert!(
                                store
                                    .operation(&parent, "child-op")
                                    .await
                                    .unwrap()
                                    .is_none()
                            );
                            assert_eq!(store.queue_len().await.unwrap(), 0);
                            assert_eq!(store.slot_count(OperationKind::ChildRun).await.unwrap(), 0);
                            let count: i64 = sqlx::query_scalar(&format!(
                                "SELECT count(*) FROM {schema}.runs WHERE parent_id=$1"
                            ))
                            .bind(&parent.id)
                            .fetch_one(&store_pool)
                            .await
                            .unwrap();
                            assert_eq!(count, 0);
                            if let Some(h) = &aborted_child {
                                assert_eq!(store.get(h).await, Err(Error::NotFound));
                            }
                        }
                    } else {
                        assert!(child.wait().unwrap().success());
                    }
                }
                let op = store.operation(&parent, "child-op").await.unwrap().unwrap();
                assert!(op.settled);
                let Some(ExternalResult::Applied { output, .. }) = op.result else {
                    panic!("child settlement missing");
                };
                let child: RunHandle = serde_json::from_value(output).unwrap();
                assert_ne!(child.id, op.id);
                if let Some(original) = &original_child {
                    assert_eq!(&child, original);
                }
                if let Some(aborted) = &aborted_child {
                    assert_ne!(&child, aborted);
                }
                if let Some(completion) = original_completion {
                    assert_eq!(
                        store.get(&child).await.unwrap().completion,
                        Some(completion)
                    );
                }
                assert_eq!(
                    store.get(&parent).await.unwrap().state,
                    RunState::Terminal(Terminal::Succeeded)
                );
                let parent_ops = store.list_operations(&parent).await.unwrap();
                assert_eq!(parent_ops.len(), 2);
                assert!(parent_ops.iter().all(|op| op.settled));
                assert_eq!(
                    store.get(&child).await.unwrap().state,
                    RunState::Terminal(Terminal::Succeeded)
                );
                assert_eq!(
                    store.account(&account).await.unwrap().used,
                    Amount::new(8, 8)
                );
                assert_eq!(
                    store.account(&account).await.unwrap().reserved,
                    Amount::default()
                );
                assert_eq!(store.slot_count(OperationKind::ChildRun).await.unwrap(), 0);
                assert_eq!(store.slot_count(OperationKind::Model).await.unwrap(), 0);
                let children: i64 = sqlx::query_scalar(&format!(
                    "SELECT count(*) FROM {schema}.runs WHERE parent_id=$1"
                ))
                .bind(&parent.id)
                .fetch_one(&store_pool)
                .await
                .unwrap();
                assert_eq!(children, 1);
                let attempts = store.list_operations(&child).await.unwrap();
                assert_eq!(attempts.len(), 1);
                assert!(attempts[0].settled);
            }
        }
    });
}
