// SPDX-License-Identifier: Apache-2.0
use runtrell::*;
use runtrell_postgres::PgStore;
use runtrell_testkit::{
    BusinessTool, DecisionFinalizer, DecisionValidator, FakeModel, FakePolicy, TestClock,
};
use sqlx::postgres::PgPoolOptions;
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
            let mut spec = RunSpec::new("t", "child", account, "child");
            spec.parent = Some(parent.clone());
            if phase == "sent" {
                let claim = store.claim(&parent, "first", 0, 100).await.unwrap();
                let op = store
                    .begin(
                        &claim,
                        DispatchRequest::new(
                            "child-op",
                            OperationKind::ChildRun,
                            "child",
                            serde_json::to_value(&spec).unwrap(),
                            Amount::default(),
                        ),
                        0,
                    )
                    .await
                    .unwrap();
                store.mark_sent(&claim, &op.id, 0).await.unwrap();
                assert_eq!(store.find_request(&spec).await.unwrap(), None);
            } else {
                let claim = store.claim(&parent, "second", 101, 100).await.unwrap();
                let clock = TestClock::default();
                clock.set(101);
                let runtime = Runtime {
                    store: store.clone(),
                    model: FakeModel::default(),
                    tools: Vec::<BusinessTool>::new(),
                    policy: FakePolicy::default(),
                    clock,
                    validator: Arc::new(DecisionValidator::default()),
                    finalizer: Arc::new(DecisionFinalizer),
                };
                let child = runtime
                    .spawn_child(&claim, "child-op", spec.clone())
                    .await
                    .unwrap();
                assert_eq!(
                    store.find_request(&spec).await.unwrap(),
                    Some(child.clone())
                );
                assert_eq!(store.start(spec).await.unwrap(), child);
            }
        } else {
            let account = store
                .create_account("t", Amount::new(1000, 1000), None)
                .await
                .unwrap();
            let parent = store
                .start(RunSpec::new("t", "parent", account.clone(), "parent"))
                .await
                .unwrap();
            let mut pids = std::collections::BTreeSet::new();
            for phase in ["sent", "resume"] {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "child_restart_across_processes", "--nocapture"])
                    .env("RUNTRELL_CHILD_SCHEMA", &schema)
                    .env("RUNTRELL_CHILD_PHASE", phase)
                    .env("RUNTRELL_CHILD_PARENT", &parent.id)
                    .env("RUNTRELL_CHILD_ACCOUNT", &account)
                    .spawn()
                    .unwrap();
                assert!(pids.insert(child.id()));
                assert!(child.wait().unwrap().success());
            }
            let mut spec = RunSpec::new("t", "child", account, "child");
            spec.parent = Some(parent.clone());
            let child = store.find_request(&spec).await.unwrap().unwrap();
            assert_eq!(store.start(spec).await.unwrap(), child);
            assert!(
                !store
                    .operation(&parent, "child-op")
                    .await
                    .unwrap()
                    .unwrap()
                    .settled
            );
        }
    });
}
