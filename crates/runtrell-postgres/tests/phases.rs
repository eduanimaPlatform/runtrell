// SPDX-License-Identifier: Apache-2.0
use runtrell::*;
use runtrell_postgres::PgStore;
use runtrell_testkit::{DecisionValidator, phase_contracts};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::Arc;

async fn store() -> (PgPool, String, PgStore) {
    let pool = PgPoolOptions::new()
        .connect(&std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("isolated test database"))
        .await
        .unwrap();
    let schema = format!("phase_{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::initialize(pool.clone(), &schema, Capacity::default())
        .await
        .unwrap();
    (pool, schema, store)
}

#[tokio::test]
async fn intermediate_completion_reopens_only_after_purge() {
    let (pool, schema, store) = store().await;
    let reopened = PgStore::connect(pool, &schema, Capacity::default())
        .await
        .unwrap();
    phase_contracts::intermediate_completion_reopens_only_after_purge(
        Arc::new(store),
        Arc::new(reopened),
    )
    .await;
}

#[tokio::test]
async fn seal_is_atomic_and_disallows_late_payloads_and_retired_replay() {
    let (pool, schema, store) = store().await;
    let account = store
        .create_account("t", Amount::new(100, 100), None)
        .await
        .unwrap();
    let secret = "transcript-only";
    let mut spec = RunSpec::new("t", "run", account, secret);
    spec.phases = ["capture", "review"]
        .into_iter()
        .map(|id| PhaseConfig {
            id: id.into(),
            tools: ["read".into()].into(),
            completion: CompletionPolicy::default(),
        })
        .collect();
    let run = store.start(spec.clone()).await.unwrap();
    let old = store.claim(&run, "old", 1, 1).await.unwrap();
    let req = DispatchRequest::new(
        "read",
        OperationKind::Tool,
        "read",
        json!(secret),
        Amount::default(),
    );
    let op = store.begin(&old, req.clone(), 1).await.unwrap();
    store.mark_sent(&old, &op.id, 1).await.unwrap();
    let result = ExternalResult::Applied {
        output: json!(secret),
        external_id: None,
        usage: Usage::measured(0, 0, 0),
        physical_evidence: None,
    };
    store.record(&old, &op.id, result.clone(), 1).await.unwrap();
    let current = store.claim(&run, "current", 2, 100).await.unwrap();
    assert_eq!(
        store.record(&old, &op.id, result.clone(), 2).await,
        Err(Error::OwnershipLost)
    );
    assert_eq!(store.late_results(&current, 2).await.unwrap().len(), 1);
    let record = store.get(&run).await.unwrap();
    let output = json!({"decision":"review"});
    let receipt = DecisionValidator::default()
        .validate(&record, &output)
        .unwrap();
    store.fail_next_write();
    assert!(
        store
            .seal_phase(
                &current,
                record.revision,
                output.clone(),
                receipt.clone(),
                3
            )
            .await
            .is_err()
    );
    let restored = store.get(&run).await.unwrap();
    assert_eq!(restored, record);
    assert_eq!(
        store
            .operation(&run, &req.key)
            .await
            .unwrap()
            .unwrap()
            .result,
        Some(result.clone())
    );
    assert_eq!(store.late_results(&current, 3).await.unwrap().len(), 1);
    let sealed = store
        .seal_phase(&current, record.revision, output, receipt, 4)
        .await
        .unwrap();
    assert!(sealed.pending_phase().is_some());
    let sql = format!("SELECT request_spec FROM \"{schema}\".runs WHERE id=$1");
    let request: Value = sqlx::query_scalar(&sql)
        .bind(&run.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!request.to_string().contains(secret));
    assert_eq!(store.find_request(&spec).await.unwrap(), Some(run.clone()));
    let next = store.claim(&run, "next", 5, 100).await.unwrap();
    assert_eq!(
        store.record(&current, &op.id, result.clone(), 5).await,
        Err(Error::OwnershipLost)
    );
    assert_eq!(
        store.record(&next, &op.id, result.clone(), 5).await,
        Err(Error::Closed)
    );
    assert!(store.late_results(&next, 5).await.unwrap().is_empty());
    let sql = format!("SELECT result FROM \"{schema}\".late_results WHERE operation=$1");
    let retained: Vec<Value> = sqlx::query_scalar(&sql)
        .bind(&op.id)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(retained.len(), 1);
    assert!(!retained[0].to_string().contains(secret));
    let revision = store.get(&run).await.unwrap().revision;
    assert_eq!(
        store
            .open_phase(&current, revision, PhaseInput::default(), 5)
            .await,
        Err(Error::OwnershipLost)
    );
    store
        .open_phase(&next, revision, PhaseInput::default(), 6)
        .await
        .unwrap();
    assert_eq!(store.begin(&next, req.clone(), 6).await, Err(Error::Closed));
    assert!(store.operation(&run, "read").await.unwrap().is_none());
    let next_op = store
        .begin(
            &next,
            DispatchRequest {
                key: "next-read".into(),
                input: Value::Null,
                ..req
            },
            6,
        )
        .await
        .unwrap();
    assert_eq!(
        store.record(&current, &next_op.id, result, 6).await,
        Err(Error::OwnershipLost)
    );
    assert!(store.late_results(&next, 6).await.unwrap().is_empty());
}
