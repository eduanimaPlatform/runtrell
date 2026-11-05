// SPDX-License-Identifier: Apache-2.0
use runtrell::*;
use runtrell_postgres::PgStore;
use runtrell_testkit::{owner_contracts as contracts, settlement_contracts};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{sync::Arc, time::Duration};

struct Env {
    pool: PgPool,
    store: Arc<PgStore>,
    reopened: Arc<PgStore>,
    schema: String,
}
async fn env() -> Env {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("isolated test database required");
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("owner_{}", uuid::Uuid::new_v4().simple());
    let store = Arc::new(
        PgStore::initialize(pool.clone(), &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let independent = PgPoolOptions::new()
        .max_connections(16)
        .connect(&url)
        .await
        .unwrap();
    let reopened = Arc::new(
        PgStore::connect(independent, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    Env {
        pool,
        store,
        reopened,
        schema,
    }
}
#[tokio::test]
async fn cancelled_resume_is_refused() {
    contracts::cancelled_resume_is_refused(env().await.store).await;
}
#[tokio::test]
async fn resume_preconditions() {
    contracts::resume_preconditions(env().await.store).await;
}
#[tokio::test]
async fn resume_budget() {
    contracts::resume_budget(env().await.store).await;
}
#[tokio::test]
async fn resume_receipts() {
    let e = env().await;
    contracts::resume_receipts(e.store, e.reopened).await;
}
#[tokio::test]
async fn settlement_receipts() {
    let e = env().await;
    contracts::settlement_receipts(e.store, e.reopened).await;
}

#[tokio::test]
async fn cancel_committed_during_run_lock_wait_refuses_resume() {
    for terminal in [false, true] {
        let e = env().await;
        let run = contracts::held(&*e.store, "lock-race", None).await;
        let before = e.store.get(&run).await.unwrap();
        let mut barrier = e.pool.begin().await.unwrap();
        let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *barrier)
            .await
            .unwrap();
        sqlx::query(&format!(
            "SELECT 1 FROM {}.runs WHERE tenant=$1 AND id=$2 FOR UPDATE",
            e.schema
        ))
        .bind(&run.tenant)
        .bind(&run.id)
        .execute(&mut *barrier)
        .await
        .unwrap();
        let name = format!("owner_race_{}", uuid::Uuid::new_v4().simple());
        let options: PgConnectOptions = std::env::var("RUNTRELL_TEST_DATABASE_URL")
            .unwrap()
            .parse()
            .unwrap();
        let resume_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.application_name(&name))
            .await
            .unwrap();
        let store = PgStore::connect(resume_pool, &e.schema, Capacity::default())
            .await
            .unwrap();
        let handle = run.clone();
        let task = tokio::spawn(async move {
            store
                .resume_held(&handle, before.revision, contracts::command("race"), 10)
                .await
        });
        // Observe the exact backend blocked by this barrier, rather than assuming
        // a launched future or an intent INSERT means it reached the Run lock.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND application_name=$1 AND wait_event_type='Lock' AND $2=ANY(pg_blocking_pids(pid)))")
                    .bind(&name).bind(blocker).fetch_one(&e.pool).await.unwrap();
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("resume never reached the blocked runtime row lock");
        let state = if terminal {
            serde_json::json!(RunState::Terminal(Terminal::Cancelled(
                "during-wait".into()
            )))
        } else {
            serde_json::json!(before.state)
        };
        sqlx::query(&format!("UPDATE {}.runs SET cancel='during-wait', state=$1, revision=revision+1 WHERE tenant=$2 AND id=$3", e.schema))
            .bind(state).bind(&run.tenant).bind(&run.id).execute(&mut *barrier).await.unwrap();
        barrier.commit().await.unwrap();
        assert_eq!(task.await.unwrap(), Err(Error::Closed));
        let after = e.store.get(&run).await.unwrap();
        assert_eq!(after.cancel.as_deref(), Some("during-wait"));
        assert_eq!(after.revision, before.revision + 1);
        if !terminal {
            assert_eq!(after.state, RunState::Waiting(OPERATOR_ACTION_WAIT.into()));
        }
        assert_eq!(e.store.owner_receipt("race").await.unwrap(), None);
    }
}

#[tokio::test]
async fn host_transaction_and_faults_keep_receipt_and_effect_atomic() {
    let e = env().await;
    sqlx::query(&format!(
        "CREATE TABLE {}.host_audit (key text PRIMARY KEY)",
        e.schema
    ))
    .execute(&e.pool)
    .await
    .unwrap();
    let run = contracts::held(&*e.store, "atomic", None).await;
    let before = e.store.get(&run).await.unwrap();
    for fail in [false, true] {
        let mut tx = e.pool.begin().await.unwrap();
        if fail {
            e.store.fail_next_write();
        }
        let result = e
            .store
            .resume_held_in(
                &mut tx,
                &run,
                before.revision,
                contracts::command("atomic"),
                10,
            )
            .await;
        if fail {
            assert_eq!(result, Err(Error::Storage));
        } else {
            assert!(result.is_ok());
        }
        assert_eq!(e.reopened.owner_receipt("atomic").await.unwrap(), None);
        assert_eq!(e.reopened.get(&run).await.unwrap(), before);
        // A failed owner call has rolled back its savepoint; outer tx remains usable.
        sqlx::query(&format!(
            "INSERT INTO {}.host_audit VALUES ('atomic')",
            e.schema
        ))
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(e.store.owner_receipt("atomic").await.unwrap(), None);
        assert_eq!(e.store.get(&run).await.unwrap(), before);
    }
    let mut tx = e.pool.begin().await.unwrap();
    let receipt = e
        .store
        .resume_held_in(
            &mut tx,
            &run,
            before.revision,
            contracts::command("atomic"),
            10,
        )
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO {}.host_audit VALUES ('atomic')",
        e.schema
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        e.reopened.owner_receipt("atomic").await.unwrap(),
        Some(receipt)
    );
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}.host_audit", e.schema))
        .fetch_one(&e.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // The host's own audit INSERT can fail after the runtime call returned.
    // Rolling back that outer transaction must remove both effect and receipt.
    let failed_run = contracts::held(&*e.store, "audit-failure", None).await;
    let failed_before = e.store.get(&failed_run).await.unwrap();
    let mut tx = e.pool.begin().await.unwrap();
    e.store
        .resume_held_in(
            &mut tx,
            &failed_run,
            failed_before.revision,
            contracts::command("audit-failure"),
            10,
        )
        .await
        .unwrap();
    let error = sqlx::query(&format!(
        "INSERT INTO {}.host_audit VALUES ('atomic')",
        e.schema
    ))
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23505")
    );
    tx.rollback().await.unwrap();
    assert_eq!(e.reopened.get(&failed_run).await.unwrap(), failed_before);
    assert_eq!(
        e.reopened.owner_receipt("audit-failure").await.unwrap(),
        None
    );

    let (run, operation) = contracts::unsettled(&*e.store, "atomic-settle").await;
    let before = e.store.get(&run).await.unwrap();
    let account = e.store.account(&before.spec.account).await.unwrap();
    let mut tx = e.pool.begin().await.unwrap();
    e.store
        .settle_recovered_in(
            &mut tx,
            &run,
            &operation,
            settlement_contracts::recovery(),
            contracts::command("atomic-settle"),
            10,
        )
        .await
        .unwrap();
    let error = sqlx::query(&format!(
        "INSERT INTO {}.host_audit VALUES ('atomic')",
        e.schema
    ))
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23505")
    );
    tx.rollback().await.unwrap();
    assert_eq!(e.reopened.get(&run).await.unwrap(), before);
    assert_eq!(
        e.reopened.account(&before.spec.account).await.unwrap(),
        account
    );
    assert_eq!(
        e.reopened.owner_receipt("atomic-settle").await.unwrap(),
        None
    );
    for fail in [false, true] {
        let mut tx = e.pool.begin().await.unwrap();
        if fail {
            e.store.fail_next_write();
        }
        let result = e
            .store
            .settle_recovered_in(
                &mut tx,
                &run,
                &operation,
                settlement_contracts::recovery(),
                contracts::command("atomic-settle"),
                10,
            )
            .await;
        if fail {
            assert_eq!(result, Err(Error::Storage));
        } else {
            assert!(result.is_ok());
        }
        assert_eq!(
            e.reopened.owner_receipt("atomic-settle").await.unwrap(),
            None
        );
        tx.rollback().await.unwrap();
        assert_eq!(e.store.get(&run).await.unwrap(), before);
        assert_eq!(
            e.store.account(&before.spec.account).await.unwrap(),
            account
        );
    }
    let mut tx = e.pool.begin().await.unwrap();
    let receipt = e
        .store
        .settle_recovered_in(
            &mut tx,
            &run,
            &operation,
            settlement_contracts::recovery(),
            contracts::command("atomic-settle"),
            10,
        )
        .await
        .unwrap();
    sqlx::query(&format!(
        "INSERT INTO {}.host_audit VALUES ('atomic-settle')",
        e.schema
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        e.reopened.owner_receipt("atomic-settle").await.unwrap(),
        Some(receipt)
    );
}

#[tokio::test]
async fn additive_migration_and_sql_immutability() {
    let e = env().await;
    sqlx::query(&format!("DROP TABLE {}.owner_receipts", e.schema))
        .execute(&e.pool)
        .await
        .unwrap();
    assert_eq!(
        PgStore::connect(e.pool.clone(), &e.schema, Capacity::default())
            .await
            .err(),
        Some(Error::NotFound)
    );
    PgStore::initialize(e.pool.clone(), &e.schema, Capacity::default())
        .await
        .unwrap();
    let run = contracts::held(&*e.store, "immutable", None).await;
    let rev = e.store.get(&run).await.unwrap().revision;
    let receipt = e
        .store
        .resume_held(&run, rev, contracts::command("immutable"), 10)
        .await
        .unwrap();
    for statement in [
        "UPDATE {s}.owner_receipts SET receipt=receipt",
        "DELETE FROM {s}.owner_receipts",
    ] {
        let error = sqlx::query(&statement.replace("{s}", &e.schema))
            .execute(&e.pool)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("55000")
        );
    }
    PgStore::initialize(e.pool.clone(), &e.schema, Capacity::default())
        .await
        .unwrap();
    assert_eq!(
        e.reopened.owner_receipt("immutable").await.unwrap(),
        Some(receipt)
    );
}
