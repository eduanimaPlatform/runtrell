// SPDX-License-Identifier: Apache-2.0
use runtrell::*;
use runtrell_postgres::PgStore;
use sqlx::postgres::PgPoolOptions;

async fn open() -> (sqlx::PgPool, String, PgStore) {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated database required");
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let schema = format!("transaction_{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::open(pool.clone(), &schema, Capacity::default())
        .await
        .unwrap();
    sqlx::query(&format!(
        "CREATE TABLE {schema}.host_records(id text PRIMARY KEY, run_id text NOT NULL)"
    ))
    .execute(&pool)
    .await
    .unwrap();
    (pool, schema, store)
}

#[tokio::test]
async fn host_transaction_commit_rollback_and_failure() {
    let (pool, schema, store) = open().await;
    for mode in ["commit", "rollback", "sql_failure", "store_failure"] {
        let mut tx = pool.begin().await.unwrap();
        let account = store
            .create_account_in(&mut tx, "t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let spec = RunSpec::new("t", mode, account.clone(), "input");
        let run = store.start_in(&mut tx, spec.clone()).await.unwrap();
        sqlx::query(&format!("INSERT INTO {schema}.host_records VALUES ($1,$2)"))
            .bind(mode)
            .bind(&run.id)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(store.start_in(&mut tx, spec.clone()).await.unwrap(), run);
        let mut different = spec;
        different.input = "changed".into();
        assert_eq!(
            store.start_in(&mut tx, different).await,
            Err(Error::Conflict)
        );
        match mode {
            "commit" => tx.commit().await.unwrap(),
            "sql_failure" => {
                assert!(sqlx::query("SELECT 1/0").execute(&mut *tx).await.is_err());
                tx.rollback().await.unwrap();
            }
            "store_failure" => {
                store.fail_next_write();
                assert_eq!(
                    store
                        .start_in(
                            &mut tx,
                            RunSpec::new("t", "fault", account.clone(), "input")
                        )
                        .await,
                    Err(Error::Storage)
                );
                tx.rollback().await.unwrap();
            }
            _ => tx.rollback().await.unwrap(),
        }
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {schema}.host_records WHERE id=$1"
        ))
        .bind(mode)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, i64::from(mode == "commit"));
        if mode == "commit" {
            assert_eq!(store.get(&run).await.unwrap().spec.account, account);
        } else {
            assert_eq!(store.get(&run).await, Err(Error::NotFound));
            assert_eq!(store.account(&account).await, Err(Error::NotFound));
        }
    }
    assert_eq!(store.queue_len().await.unwrap(), 1);
    let reserved: i64 =
        sqlx::query_scalar(&format!("SELECT count(*) FROM {schema}.budget_entries"))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reserved, 0);
}

#[tokio::test]
async fn shared_account_child_and_terminal_boundary() {
    let (pool, schema, store) = open().await;
    let mut tx = pool.begin().await.unwrap();
    let account = store
        .create_account_with_policy_in(
            &mut tx,
            "t",
            Amount::new(100, 100),
            None,
            AccountPolicy::default(),
        )
        .await
        .unwrap();
    let mut parent_spec = RunSpec::new("t", "parent", account.clone(), "input");
    parent_spec.deadline = Some(90);
    let parent = store.start_in(&mut tx, parent_spec).await.unwrap();
    let mut child_spec = RunSpec::new("t", "child", account.clone(), "fresh input");
    child_spec.parent = Some(parent.clone());
    let child = store.start_in(&mut tx, child_spec.clone()).await.unwrap();
    assert_eq!(
        store.start_in(&mut tx, child_spec.clone()).await.unwrap(),
        child
    );
    let mut changed_deadline = child_spec.clone();
    changed_deadline.deadline = Some(200);
    assert_eq!(
        store.start_in(&mut tx, changed_deadline).await,
        Err(Error::Conflict)
    );
    tx.commit().await.unwrap();
    assert_eq!(
        store.find_request(&child_spec).await.unwrap(),
        Some(child.clone())
    );
    assert_eq!(store.get(&child).await.unwrap().spec.deadline, Some(90));
    let claim = store.claim(&parent, "worker", 0, 100).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    store
        .terminate_in(&mut tx, &claim, Terminal::Failed("test".into()), 1)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(!matches!(
        store.get(&parent).await.unwrap().state,
        RunState::Terminal(_)
    ));
    let mut tx = pool.begin().await.unwrap();
    store
        .terminate_in(&mut tx, &claim, Terminal::Failed("test".into()), 1)
        .await
        .unwrap();
    let next = store
        .start_in(
            &mut tx,
            RunSpec::new("t", "next", account.clone(), "new conversation"),
        )
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO {schema}.host_records VALUES ('boundary',$1)"
    ))
    .bind(&next.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        store.get(&parent).await.unwrap().state,
        RunState::Terminal(_)
    ));
    assert_eq!(store.get(&child).await.unwrap().spec.account, account);
    assert_eq!(store.get(&next).await.unwrap().spec.account, account);
    // Run this same test binary in a distinct OS process, using only schema/identity.
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "reopen_in_another_process", "--nocapture"])
        .env("RUNTRELL_REOPEN_SCHEMA", &schema)
        .env("RUNTRELL_REOPEN_RUN", &next.id)
        .env("RUNTRELL_REOPEN_ACCOUNT", &account)
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn reopen_in_another_process() {
    let Ok(schema) = std::env::var("RUNTRELL_REOPEN_SCHEMA") else {
        return;
    };
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let store = PgStore::open(pool.clone(), &schema, Capacity::default())
        .await
        .unwrap();
    let run = RunHandle {
        tenant: "t".into(),
        id: std::env::var("RUNTRELL_REOPEN_RUN").unwrap(),
    };
    assert_eq!(
        store.get(&run).await.unwrap().spec.account,
        std::env::var("RUNTRELL_REOPEN_ACCOUNT").unwrap()
    );
    let saved: String = sqlx::query_scalar(&format!(
        "SELECT run_id FROM {schema}.host_records WHERE id='boundary'"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(saved, run.id);
    assert_eq!(store.queue_len().await.unwrap(), 2);
}

#[tokio::test]
async fn successful_finalization_is_staged_fenced_and_validated() {
    let (pool, schema, store) = open().await;
    let account = store
        .create_account("t", Amount::new(100, 100), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new("t", "finalize", account, "input"))
        .await
        .unwrap();
    let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
    let spec = store.get(&run).await.unwrap().spec;
    let output = serde_json::json!({"result":"complete"});
    let completion = Completion {
        id: "completion-1".into(),
        receipt: ValidationReceipt {
            repairable: false,
            hash: digest(&output),
            basis_revision: spec.basis_revision,
            config_version: spec.config_version,
            validator: spec.validator,
            validator_version: spec.validator_version,
            passed: true,
            diagnostics: vec![],
        },
        output,
    };
    let mut tx = pool.begin().await.unwrap();
    let mut invalid = completion.clone();
    invalid.receipt.hash = "wrong".into();
    assert_eq!(
        store.finalize_in(&mut tx, &claim, invalid, 1).await,
        Err(Error::Conflict)
    );
    assert!(matches!(
        store
            .terminate_in(&mut tx, &claim, Terminal::Succeeded, 1)
            .await,
        Err(Error::Invalid(_))
    ));
    store
        .finalize_in(&mut tx, &claim, completion.clone(), 1)
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO {schema}.host_records VALUES ('complete',$1)"
    ))
    .bind(&run.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(store.get(&run).await.unwrap().state, RunState::Running);
    let mut tx = pool.begin().await.unwrap();
    store
        .finalize_in(&mut tx, &claim, completion.clone(), 1)
        .await
        .unwrap();
    store
        .finalize_in(&mut tx, &claim, completion.clone(), 1)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        store.get(&run).await.unwrap().completion,
        Some(completion.id.clone())
    );
    store.claim(&run, "new-worker", 101, 100).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        store.finalize_in(&mut tx, &claim, completion, 101).await,
        Err(Error::OwnershipLost)
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn failed_call_releases_partial_permits_and_preserves_host_transaction() {
    let (pool, schema, store) = open().await;
    let mut tx = pool.begin().await.unwrap();
    let account = store
        .create_account_in(&mut tx, "t", Amount::new(100, 100), None)
        .await
        .unwrap();
    store.fail_next_write();
    let failed = RunSpec::new("t", "failed", account.clone(), "input");
    assert_eq!(
        store.start_in(&mut tx, failed.clone()).await,
        Err(Error::Storage)
    );
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {schema}.runs"))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let held: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.capacity_slots WHERE holder IS NOT NULL"
    ))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(held, 0);
    let run = store.start_in(&mut tx, failed).await.unwrap();
    sqlx::query(&format!(
        "INSERT INTO {schema}.host_records VALUES ('retry',$1)"
    ))
    .bind(&run.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(store.queue_len().await.unwrap(), 1);
    assert_eq!(store.get(&run).await.unwrap().spec.account, account);
}
