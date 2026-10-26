use runtrell::*;
use runtrell_postgres::PgStore;
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
async fn driver_token_continuation() {
    let (_, _, store, _, _) = setup().await;
    runtrell_testkit::driver_contracts::token_continuation(std::sync::Arc::new(store)).await;
}
#[tokio::test]
async fn driver_continuation_budget_and_host_limit() {
    let (_, _, store, _, _) = setup().await;
    runtrell_testkit::driver_contracts::continuation_stops(std::sync::Arc::new(store)).await;
}
#[tokio::test]
async fn driver_continuation_deadline() {
    let (_, _, store, _, _) = setup().await;
    runtrell_testkit::driver_contracts::continuation_deadline(std::sync::Arc::new(store)).await;
}
#[tokio::test]
async fn driver_host_receipt_classification() {
    let (_, _, store, _, _) = setup().await;
    runtrell_testkit::driver_contracts::receipt_classification(std::sync::Arc::new(store)).await;
}
#[tokio::test]
async fn driver_tool_schema_repairs() {
    let (_, _, store, _, _) = setup().await;
    runtrell_testkit::driver_contracts::tool_repairs(std::sync::Arc::new(store)).await;
}
#[tokio::test]
async fn driver_automatic_context_reopens_with_independent_pool() {
    use std::sync::Arc;
    let (pool, schema, store, _, _) = setup().await;
    let store = Arc::new(store);
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let independent = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let reopened = Arc::new(
        PgStore::open(independent, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    runtrell_testkit::driver_contracts::automatic_compaction(store, reopened).await;
    pool.close().await;
}

#[tokio::test]
async fn empty_database_initializes_and_reopens() {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL")
        .expect("RUNTRELL_TEST_DATABASE_URL must name the dedicated disposable database");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let (version,): (String,) = sqlx::query_as("SELECT current_setting('server_version')")
        .fetch_one(&pool)
        .await
        .unwrap();
    println!("PostgreSQL server_version={version}");
    let schema = format!("test_{}", uuid::Uuid::new_v4().simple());
    PgStore::open(pool.clone(), &schema, Capacity::default())
        .await
        .unwrap();
    PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
}

async fn setup() -> (sqlx::PgPool, String, PgStore, String, Claim) {
    let url =
        std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("RUNTRELL_TEST_DATABASE_URL required");
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("postgres_test_{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::open(pool.clone(), &schema, Capacity::default())
        .await
        .unwrap();
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let h = store
        .start(RunSpec::new("t", "request", account.clone(), "input"))
        .await
        .unwrap();
    let claim = store.claim(&h, "first", 0, 100).await.unwrap();
    (pool, schema, store, account, claim)
}
fn request(key: &str, reserve: u64) -> DispatchRequest {
    DispatchRequest::new(
        key,
        OperationKind::Sandbox,
        "external",
        serde_json::json!({}),
        Amount::new(reserve, reserve),
    )
}
fn applied() -> ExternalResult {
    ExternalResult::Applied {
        physical_evidence: None,
        output: serde_json::json!({"receipt":"done"}),
        usage: Usage::measured(1, 1, 2),
        external_id: Some("external-1".into()),
    }
}

#[tokio::test]
async fn stale_response_is_durable_observation_without_settlement() {
    let (pool, schema, s, a, c) = setup().await;
    let op = s.begin(&c, request("effect", 10), 0).await.unwrap();
    s.mark_sent(&c, &op.id, 0).await.unwrap();
    let next = s.claim(&c.run, "second", 101, 100).await.unwrap();
    assert_eq!(
        s.record(&c, &op.id, applied(), 101).await,
        Err(Error::OwnershipLost)
    );
    assert_eq!(
        s.record(&c, &op.id, applied(), 101).await,
        Err(Error::OwnershipLost)
    );
    let reopened = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    let late = reopened.late_results(&next, 101).await.unwrap();
    assert_eq!(late.len(), 1);
    assert_eq!(late[0].generation, c.generation);
    assert_eq!(late[0].result, applied());
    assert_eq!(
        reopened.account(&a).await.unwrap().reserved,
        Amount::new(10, 10)
    );
    assert_eq!(
        reopened.slot_count(OperationKind::Sandbox).await.unwrap(),
        1
    );
    assert!(
        !reopened
            .operation(&c.run, "effect")
            .await
            .unwrap()
            .unwrap()
            .settled
    );
    reopened
        .record(&next, &op.id, applied(), 101)
        .await
        .unwrap();
    assert_eq!(reopened.account(&a).await.unwrap().used, Amount::new(2, 2));
}

#[tokio::test]
async fn failed_intent_transaction_rolls_back_all_ancestors_and_slot() {
    let (pool, schema, s, a, c) = setup().await;
    // Use an additional account ancestry in the same durable namespace.
    let child = s
        .create_account("t", Amount::new(1000, 1000), Some(a.clone()))
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("t", "child", child.clone(), "input"))
        .await
        .unwrap();
    let owner = s.claim(&h, "child", 0, 100).await.unwrap();
    s.fail_next_write();
    assert_eq!(
        s.begin(&owner, request("rollback", 10), 0).await,
        Err(Error::Storage)
    );
    let reopened = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    assert_eq!(
        reopened.account(&a).await.unwrap().reserved,
        Amount::default()
    );
    assert_eq!(
        reopened.account(&child).await.unwrap().reserved,
        Amount::default()
    );
    assert!(reopened.operations(&h).await.unwrap().is_empty());
    assert_eq!(
        reopened.slot_count(OperationKind::Sandbox).await.unwrap(),
        0
    );
    assert!(reopened.check(&c, 0, true).await.is_ok());
}

#[tokio::test]
async fn failed_result_transaction_preserves_unknown_reservation_for_recovery() {
    let (pool, schema, s, a, c) = setup().await;
    let op = s.begin(&c, request("effect", 10), 0).await.unwrap();
    s.record(&c, &op.id, ExternalResult::Unknown, 0)
        .await
        .unwrap();
    s.fail_next_write();
    assert_eq!(
        s.record(&c, &op.id, applied(), 0).await,
        Err(Error::Storage)
    );
    let reopened = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    let b = reopened.account(&a).await.unwrap();
    assert_eq!(b.reserved, Amount::new(10, 10));
    assert_eq!(b.used, Amount::default());
    assert_eq!(b.unknown, 1);
    assert_eq!(
        reopened.slot_count(OperationKind::Sandbox).await.unwrap(),
        1
    );
    assert_eq!(
        reopened
            .operation(&c.run, "effect")
            .await
            .unwrap()
            .unwrap()
            .result,
        Some(ExternalResult::Unknown)
    );
    reopened.record(&c, &op.id, applied(), 0).await.unwrap();
    reopened.record(&c, &op.id, applied(), 0).await.unwrap();
    assert_eq!(reopened.account(&a).await.unwrap().used, Amount::new(2, 2));
    assert_eq!(
        reopened
            .record(&c, &op.id, ExternalResult::Unknown, 0)
            .await,
        Err(Error::Conflict)
    );
}

#[tokio::test]
async fn independent_connections_compete_for_all_ancestor_balances() {
    let (pool, schema, s, _, _) = setup().await;
    let root = s
        .create_account("*", Amount::new(100, 100), None)
        .await
        .unwrap();
    let tenant = s
        .create_account("t", Amount::new(100, 100), Some(root.clone()))
        .await
        .unwrap();
    let a = s
        .create_account("t", Amount::new(100, 100), Some(tenant.clone()))
        .await
        .unwrap();
    let b = s
        .create_account("t", Amount::new(100, 100), Some(tenant.clone()))
        .await
        .unwrap();
    let h1 = s
        .start(RunSpec::new("t", "a", a.clone(), "input"))
        .await
        .unwrap();
    let h2 = s
        .start(RunSpec::new("t", "b", b.clone(), "input"))
        .await
        .unwrap();
    let c1 = s.claim(&h1, "one", 0, 100).await.unwrap();
    let c2 = s.claim(&h2, "two", 0, 100).await.unwrap();
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let second_pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let second = PgStore::open(second_pool, &schema, Capacity::default())
        .await
        .unwrap();
    let (x, y) = tokio::join!(
        s.begin(&c1, request("one", 70), 0),
        second.begin(&c2, request("two", 70), 0)
    );
    assert_eq!(usize::from(x.is_ok()) + usize::from(y.is_ok()), 1);
    assert!(x == Err(Error::ReservationDenied) || y == Err(Error::ReservationDenied));
    let reopened = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    assert_eq!(
        reopened.account(&root).await.unwrap().reserved,
        Amount::new(70, 70)
    );
    assert_eq!(
        reopened.account(&tenant).await.unwrap().reserved,
        Amount::new(70, 70)
    );
    assert_eq!(
        reopened.account(&a).await.unwrap().reserved.tokens
            + reopened.account(&b).await.unwrap().reserved.tokens,
        70
    );
    assert_eq!(
        reopened.slot_count(OperationKind::Sandbox).await.unwrap(),
        1
    );
}

#[tokio::test]
async fn parallel_claim_has_one_winner_and_failed_handoff_has_no_generation_change() {
    let (pool, schema, s, _, c) = setup().await;
    let other = PgStore::open(pool.clone(), &schema, Capacity::default())
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        s.claim(&c.run, "a", 101, 100),
        other.claim(&c.run, "b", 101, 100)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let winner = a.or(b).unwrap();
    assert_eq!(winner.generation, 2);
    assert_eq!(s.renew(&c, 101, 100).await, Err(Error::OwnershipLost));
    s.fail_next_write();
    assert_eq!(
        s.claim(&c.run, "failed", 202, 100).await,
        Err(Error::Storage)
    );
    assert_eq!(other.get(&c.run).await.unwrap().owner, Some(winner));
    assert_eq!(
        other
            .claim(&c.run, "success", 202, 100)
            .await
            .unwrap()
            .generation,
        3
    );
}

#[tokio::test]
async fn queue_slots_fairness_and_cleanup_survive_all_connections_being_discarded() {
    let (pool, schema, s, a, c) = setup().await;
    for (key, tenant) in [("a1", "a"), ("a2", "a"), ("b1", "b"), ("c1", "c")] {
        s.enqueue(QueueEntry {
            key: key.into(),
            tenant: tenant.into(),
            scheduling: SchedulingPolicy {
                priority: 1,
                ..SchedulingPolicy::default()
            },
            deadline: 1000,
        })
        .await
        .unwrap();
    }
    assert_eq!(s.dequeue(0).await.unwrap().unwrap().tenant, "a");
    let op = s.begin(&c, request("sandbox", 10), 0).await.unwrap();
    s.record(&c, &op.id, ExternalResult::Unknown, 0)
        .await
        .unwrap();
    s.cancel(&c.run, "stop").await.unwrap();
    s.cleanup(&c, Progress::Unknown, Progress::Pending, 0)
        .await
        .unwrap();
    drop(s);
    pool.close().await;
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let new_pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let s = PgStore::open(new_pool, &schema, Capacity::default())
        .await
        .unwrap();
    assert_eq!(s.queue_len().await.unwrap(), 3);
    assert_eq!(s.dequeue(0).await.unwrap().unwrap().tenant, "b");
    assert_eq!(s.dequeue(0).await.unwrap().unwrap().tenant, "c");
    assert_eq!(s.account(&a).await.unwrap().reserved, Amount::new(10, 10));
    assert_eq!(s.account(&a).await.unwrap().unknown, 1);
    assert_eq!(s.slot_count(OperationKind::Sandbox).await.unwrap(), 1);
    assert_eq!(s.recovery_candidates().await.unwrap(), vec![c.run.clone()]);
    let next = s.claim(&c.run, "reclaimer", 101, 100).await.unwrap();
    s.record(&next, &op.id, applied(), 101).await.unwrap();
    s.cleanup(&next, Progress::Confirmed, Progress::Confirmed, 101)
        .await
        .unwrap();
    s.cleanup(&next, Progress::Confirmed, Progress::Confirmed, 101)
        .await
        .unwrap();
    // The Run lease slot also survived the crash; terminal status alone does not
    // prove that the worker completed its post-drive release barrier.
    assert_eq!(s.recovery_candidates().await.unwrap(), vec![c.run.clone()]);
    let slots = s.held_slots(&c.run).await.unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].kind, OperationKind::Run);
    s.release_slot(&slots[0].key).await.unwrap();
    assert!(s.recovery_candidates().await.unwrap().is_empty());
    assert_eq!(s.slot_count(OperationKind::Sandbox).await.unwrap(), 0);
}

#[tokio::test]
async fn different_tokio_runtimes_share_one_durable_reservation_authority() {
    let (pool, schema, s, a, c) = setup().await;
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let mut threads = vec![];
    for n in 0..8 {
        let (url, schema, c) = (url.clone(), schema.clone(), c.clone());
        threads.push(std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let pool = PgPoolOptions::new()
                        .max_connections(1)
                        .connect(&url)
                        .await
                        .unwrap();
                    let s = PgStore::open(pool, &schema, Capacity::default())
                        .await
                        .unwrap();
                    s.begin(
                        &c,
                        DispatchRequest::new(
                            &format!("r{n}"),
                            OperationKind::Model,
                            "fake",
                            serde_json::json!({}),
                            Amount::new(300, 300),
                        ),
                        0,
                    )
                    .await
                })
        }));
    }
    let results = tokio::task::spawn_blocking(move || {
        threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap();
    assert!(results.iter().filter(|r| r.is_ok()).count() <= 3);
    assert!(results.iter().any(|r| r.is_ok()));
    assert!(s.account(&a).await.unwrap().reserved.tokens <= 900);
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(e, Error::ReservationDenied | Error::BudgetAtLimit))
    );
    drop(s);
    pool.close().await;
}

#[tokio::test]
async fn invalid_schema_and_capacity_disagreement_are_rejected() {
    let (pool, schema, _, _, _) = setup().await;
    for name in [
        "",
        "pg_reserved",
        "x;DROP SCHEMA public",
        "a.b",
        "1abc",
        "a\"b",
    ] {
        assert!(matches!(
            PgStore::open(pool.clone(), name, Capacity::default()).await,
            Err(Error::Invalid(_))
        ));
    }
    let mut cap = Capacity::default();
    cap.queue += 1;
    assert!(matches!(
        PgStore::open(pool, &schema, cap).await,
        Err(Error::Conflict)
    ));
}

fn runtime(
    store: std::sync::Arc<PgStore>,
    model: runtrell_testkit::FakeModel,
) -> Runtime<
    PgStore,
    runtrell_testkit::FakeModel,
    runtrell_testkit::BusinessTool,
    runtrell_testkit::FakePolicy,
    runtrell_testkit::TestClock,
    runtrell_testkit::DecisionValidator,
    runtrell_testkit::DecisionFinalizer,
> {
    use runtrell_testkit::*;
    Runtime {
        store,
        model,
        tools: vec![],
        policy: FakePolicy::default(),
        clock: TestClock::default(),
        validator: std::sync::Arc::new(DecisionValidator::default()),
        finalizer: std::sync::Arc::new(DecisionFinalizer),
    }
}

#[tokio::test]
async fn separate_agent_runtimes_cannot_double_reserve_and_loser_never_invokes() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (pool, schema, s, _, _) = setup().await;
    let a = s
        .create_account("t", Amount::new(100, 100), None)
        .await
        .unwrap();
    let h1 = s
        .start(RunSpec::new("t", "runtime-one", a.clone(), "input"))
        .await
        .unwrap();
    let h2 = s
        .start(RunSpec::new("t", "runtime-two", a.clone(), "input"))
        .await
        .unwrap();
    let c1 = s.claim(&h1, "one", 0, 100).await.unwrap();
    let c2 = s.claim(&h2, "two", 0, 100).await.unwrap();
    let second = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    let rt1 = runtime(Arc::new(s), runtrell_testkit::FakeModel::default());
    let rt2 = runtime(Arc::new(second), runtrell_testkit::FakeModel::default());
    let sends = AtomicUsize::new(0);
    let (x, y) = tokio::join!(
        rt1.dispatch(
            &c1,
            request("one", 70),
            "external",
            |_| async {
                sends.fetch_add(1, Ordering::SeqCst);
                Ok(ExternalResult::Accepted {
                    external_id: "one".into(),
                })
            },
            |_| async { Ok(Lookup::Unknown) }
        ),
        rt2.dispatch(
            &c2,
            request("two", 70),
            "external",
            |_| async {
                sends.fetch_add(1, Ordering::SeqCst);
                Ok(ExternalResult::Accepted {
                    external_id: "two".into(),
                })
            },
            |_| async { Ok(Lookup::Unknown) }
        )
    );
    assert_eq!(usize::from(x.is_ok()) + usize::from(y.is_ok()), 1);
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    assert_eq!(
        rt1.store.account(&a).await.unwrap().reserved,
        Amount::new(70, 70)
    );
}

#[tokio::test]
async fn fresh_pool_and_agent_runtime_recover_lost_reply_without_second_model_send() {
    use runtrell_testkit::FakeModel;
    use std::sync::{Arc, atomic::Ordering};
    let (pool, schema, s, a, c) = setup().await;
    let model = FakeModel::scripted(vec![FakeModel::decision(
        serde_json::json!({"decision":"answered"}),
    )]);
    model.lose_response.store(true, Ordering::SeqCst);
    let rt = runtime(Arc::new(s), model.clone());
    let first = rt.drive(&c).await.unwrap();
    assert!(!matches!(
        first.state,
        RunState::Terminal(Terminal::Succeeded)
    ));
    let original = rt
        .store
        .operation(&c.run, "model:1")
        .await
        .unwrap()
        .unwrap();
    drop(rt);
    pool.close().await;
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let new_pool = PgPoolOptions::new().connect(&url).await.unwrap();
    let store = Arc::new(
        PgStore::open(new_pool, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let next = store.claim(&c.run, "restart", 101, 100).await.unwrap();
    let restarted = runtime(store.clone(), model.clone());
    restarted.clock.set(101);
    assert_eq!(
        restarted.drive(&next).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert_eq!(
        store
            .operation(&c.run, "model:1")
            .await
            .unwrap()
            .unwrap()
            .id,
        original.id
    );
    assert_eq!(store.account(&a).await.unwrap().used.tokens, 4);
    let receipt = store.get(&c.run).await.unwrap().completion;
    assert_eq!(restarted.drive(&next).await.unwrap().completion, receipt);
    assert_eq!(model.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn sql_failure_at_start_checkpoint_cancel_and_finalize_is_not_success() {
    use runtrell_testkit::{DecisionFinalizer, DecisionValidator};
    let (pool, schema, s, a, c) = setup().await;
    let spec = RunSpec::new("t", "failed-start", a, "new");
    s.fail_next_write();
    assert_eq!(s.start(spec.clone()).await, Err(Error::Storage));
    assert_eq!(s.find_request(&spec).await.unwrap(), None);
    let before = s.get(&c.run).await.unwrap();
    s.fail_next_write();
    assert_eq!(
        s.checkpoint(
            &c,
            before.revision,
            Checkpoint {
                schema: 1,
                revision: 0,
                payload: serde_json::json!({}),
                messages: vec![],
                cursor: 0
            },
            0
        )
        .await,
        Err(Error::Storage)
    );
    assert_eq!(s.get(&c.run).await.unwrap(), before);
    s.fail_next_write();
    assert_eq!(s.cancel(&c.run, "failed-cancel").await, Err(Error::Storage));
    assert_eq!(s.get(&c.run).await.unwrap(), before);
    let output = serde_json::json!({"decision":"answered"});
    let receipt = DecisionValidator::default()
        .validate(&before, &output)
        .unwrap();
    let completion = DecisionFinalizer
        .finalize(&before, output, receipt)
        .unwrap();
    s.fail_next_write();
    assert_eq!(
        s.finalize(&c, completion.clone(), 0).await,
        Err(Error::Storage)
    );
    let reopened = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    assert_eq!(reopened.get(&c.run).await.unwrap(), before);
    reopened.finalize(&c, completion.clone(), 0).await.unwrap();
    reopened.finalize(&c, completion.clone(), 0).await.unwrap();
    assert_eq!(
        reopened.get(&c.run).await.unwrap().completion,
        Some(completion.id)
    );
}

#[tokio::test]
async fn concurrent_start_deduplicates_and_rejects_different_digest() {
    let (pool, schema, s, a, _) = setup().await;
    let second = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    let spec = RunSpec::new("t", "concurrent", a, "input");
    let (x, y) = tokio::join!(s.start(spec.clone()), second.start(spec.clone()));
    assert_eq!(x.unwrap(), y.unwrap());
    let mut changed = spec;
    changed.input = "different".into();
    assert_eq!(second.start(changed).await, Err(Error::Conflict));
    assert_eq!(s.queue_len().await.unwrap(), 1);
}

#[tokio::test]
async fn dropped_save_request_does_not_acknowledge_or_partially_accept_run() {
    let (pool, schema, s, a, _) = setup().await;
    let session = "save-barrier";
    sqlx::query(&format!(
        "INSERT INTO \"{schema}\".sessions(key) VALUES($1)"
    ))
    .bind(format!("1:t{session}"))
    .execute(&pool)
    .await
    .unwrap();
    let mut blocker = pool.begin().await.unwrap();

    sqlx::query(&format!(
        "SELECT sequence FROM \"{schema}\".sessions FOR UPDATE"
    ))
    .fetch_one(&mut *blocker)
    .await
    .unwrap();
    let mut spec = RunSpec::new("t", "aborted-save", a, "input");
    spec.session = Some(session.into());
    let pending = s.clone();
    let input = spec.clone();
    let (sent, received) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        sent.send(()).unwrap();
        pending.start(input).await
    });
    received.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(!task.is_finished());
    assert_eq!(s.find_request(&spec).await.unwrap(), None);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    blocker.rollback().await.unwrap();
    let reopened = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    assert_eq!(reopened.find_request(&spec).await.unwrap(), None);
    assert_eq!(reopened.queue_len().await.unwrap(), 0);
    let h = reopened.start(spec.clone()).await.unwrap();
    assert_eq!(reopened.find_request(&spec).await.unwrap(), Some(h));
}

#[tokio::test]
async fn cancel_and_finalize_use_one_transaction_order_across_pools() {
    use runtrell_testkit::{DecisionFinalizer, DecisionValidator};
    let (pool, schema, s, _, c) = setup().await;
    let second = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    let before = s.get(&c.run).await.unwrap();
    let output = serde_json::json!({"decision":"answered"});
    let receipt = DecisionValidator::default()
        .validate(&before, &output)
        .unwrap();
    let completion = DecisionFinalizer
        .finalize(&before, output, receipt)
        .unwrap();
    let (cancel, finalize) = tokio::join!(
        s.cancel(&c.run, "race"),
        second.finalize(&c, completion.clone(), 0)
    );
    let r = s.get(&c.run).await.unwrap();
    match r.state {
        RunState::Terminal(Terminal::Succeeded) => {
            assert_eq!(finalize, Ok(()));
            assert_eq!(cancel, Err(Error::Closed));
            assert_eq!(r.completion, Some(completion.id));
        }
        RunState::Terminal(Terminal::Cancelled(_)) => {
            assert_eq!(cancel, Ok(()));
            assert_eq!(finalize, Err(Error::Closed));
            assert!(r.completion.is_none());
        }
        other => panic!("unexpected state {other:?}"),
    }
}

#[tokio::test]
async fn every_slot_kind_enforces_global_tenant_and_run_limits() {
    let (pool, _, _, _, _) = setup().await;
    let mut capacity = Capacity::default();
    let kinds = [
        OperationKind::Run,
        OperationKind::Model,
        OperationKind::Tool,
        OperationKind::Sandbox,
        OperationKind::ChildRun,
        OperationKind::Retry,
        OperationKind::Compaction,
    ];
    for kind in kinds {
        capacity.global.insert(kind, 3);
        capacity.tenant.insert(kind, 2);
        capacity.run.insert(kind, 1);
    }
    let schema = format!("slot_test_{}", uuid::Uuid::new_v4().simple());
    let s = PgStore::open(pool.clone(), &schema, capacity.clone())
        .await
        .unwrap();
    let second = PgStore::open(pool, &schema, capacity).await.unwrap();
    for kind in kinds {
        let slot = |key: &str, tenant: &str, run: &str| SlotRequest {
            key: format!("{kind:?}:{key}"),
            tenant: tenant.into(),
            run: run.into(),
            kind,
        };
        s.acquire(slot("one", "a", "one")).await.unwrap();
        assert_eq!(
            second.acquire(slot("same-run", "a", "one")).await,
            Err(Error::Backpressure)
        );
        second.acquire(slot("two", "a", "two")).await.unwrap();
        assert_eq!(
            s.acquire(slot("tenant", "a", "three")).await,
            Err(Error::Backpressure)
        );
        s.acquire(slot("three", "b", "one")).await.unwrap();
        assert_eq!(
            second.acquire(slot("global", "c", "one")).await,
            Err(Error::Backpressure)
        );
        assert_eq!(second.slot_count(kind).await.unwrap(), 3);
    }
}

#[tokio::test]
async fn locked_free_permits_wait_instead_of_reporting_backpressure() {
    let (pool, _, _, _, _) = setup().await;
    let mut cap = Capacity::default();
    cap.global.insert(OperationKind::Tool, 1);
    cap.tenant.insert(OperationKind::Tool, 1);
    cap.run.insert(OperationKind::Tool, 1);
    let schema = format!("permit_test_{}", uuid::Uuid::new_v4().simple());
    let s = PgStore::open(pool.clone(), &schema, cap).await.unwrap();
    let slot = SlotRequest {
        key: "waiting".into(),
        tenant: "t".into(),
        run: "r".into(),
        kind: OperationKind::Tool,
    };
    // Initialize tenant/Run scopes, then leave every permit free.
    s.acquire(slot.clone()).await.unwrap();
    s.release_slot(&slot.key).await.unwrap();
    for scope in ["g:Tool", "t:\"t\":Tool", "r:[\"t\",\"r\"]:Tool"] {
        for commit in [false, true] {
            let mut blocker = pool.begin().await.unwrap();
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(&mut *blocker)
                .await
                .unwrap();
            let locked: Vec<i64> = sqlx::query_scalar(&format!(
                "SELECT ordinal FROM \"{schema}\".capacity_slots WHERE scope=$1 AND holder IS NULL FOR UPDATE"
            )).bind(scope).fetch_all(&mut *blocker).await.unwrap();
            assert_eq!(locked.len(), 1);
            let store = s.clone();
            let request = slot.clone();
            let task = tokio::spawn(async move { store.acquire(request).await });
            // Observe an actual database lock wait, rather than relying on a sleep.
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    assert!(!task.is_finished(), "locked free {scope} was treated as unavailable");
                    let waiting: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))"
                    ).bind(pid).fetch_one(&pool).await.unwrap();
                    if waiting { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }).await.unwrap();
            if commit {
                blocker.commit().await.unwrap();
            } else {
                blocker.rollback().await.unwrap();
            }
            tokio::time::timeout(std::time::Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(s.slot_count(OperationKind::Tool).await.unwrap(), 1);
            s.release_slot(&slot.key).await.unwrap();
        }
    }
}

#[tokio::test]
async fn permit_lock_timeout_is_storage_and_rolls_back_partial_allocation() {
    let (pool, _, _, _, _) = setup().await;
    let mut cap = Capacity::default();
    cap.global.insert(OperationKind::Tool, 1);
    cap.tenant.insert(OperationKind::Tool, 1);
    cap.run.insert(OperationKind::Tool, 1);
    let schema = format!("timeout_test_{}", uuid::Uuid::new_v4().simple());
    let s = PgStore::open(pool.clone(), &schema, cap).await.unwrap();
    let slot = SlotRequest {
        key: "timeout".into(),
        tenant: "t".into(),
        run: "r".into(),
        kind: OperationKind::Tool,
    };
    s.acquire(slot.clone()).await.unwrap();
    s.release_slot(&slot.key).await.unwrap();
    let mut blocker = pool.begin().await.unwrap();
    let ordinal: i64 = sqlx::query_scalar(&format!(
        "SELECT ordinal FROM \"{schema}\".capacity_slots WHERE scope='r:[\"t\",\"r\"]:Tool' AND holder IS NULL FOR UPDATE"
    )).fetch_one(&mut *blocker).await.unwrap();
    assert_eq!(ordinal, 1);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(10), s.acquire(slot.clone()))
            .await
            .unwrap(),
        Err(Error::Storage)
    );
    // The failed transaction must release earlier scopes before returning,
    // even while the original blocker remains active on the Run permit.
    let mut check = pool.begin().await.unwrap();
    let free: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT scope FROM \"{schema}\".capacity_slots WHERE scope IN ('g:Tool','t:\"t\":Tool') AND holder IS NULL FOR UPDATE NOWAIT"
    )).fetch_all(&mut *check).await.unwrap();
    assert_eq!(free.len(), 2);
    check.rollback().await.unwrap();
    assert_eq!(s.slot_count(OperationKind::Tool).await.unwrap(), 0);
    blocker.rollback().await.unwrap();
    s.acquire(slot).await.unwrap();
    assert_eq!(s.slot_count(OperationKind::Tool).await.unwrap(), 1);
}

#[tokio::test]
async fn concurrent_slot_acquisitions_never_exceed_any_scope_limit() {
    let (pool, _, _, _, _) = setup().await;
    for limits in [[3, 64, 64], [64, 3, 64], [64, 64, 3]] {
        let mut cap = Capacity::default();
        cap.global.insert(OperationKind::Tool, limits[0]);
        cap.tenant.insert(OperationKind::Tool, limits[1]);
        cap.run.insert(OperationKind::Tool, limits[2]);
        let schema = format!("limit_test_{}", uuid::Uuid::new_v4().simple());
        let s = PgStore::open(pool.clone(), &schema, cap).await.unwrap();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(48));
        let mut tasks = Vec::new();
        for i in 0..48 {
            let (store, barrier) = (s.clone(), barrier.clone());
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                store
                    .acquire(SlotRequest {
                        key: format!("slot-{i}"),
                        tenant: "t".into(),
                        run: "r".into(),
                        kind: OperationKind::Tool,
                    })
                    .await
            }));
        }
        let mut accepted = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(()) => accepted += 1,
                Err(Error::Backpressure) => {}
                other => panic!("unexpected slot outcome: {other:?}"),
            }
        }
        assert_eq!(accepted, 3, "limits={limits:?}");
        assert_eq!(s.slot_count(OperationKind::Tool).await.unwrap(), 3);
        let counts: Vec<(String, i64)> = sqlx::query_as(&format!(
            "SELECT scope,count(*) FROM \"{schema}\".capacity_slots WHERE holder IS NOT NULL GROUP BY scope ORDER BY scope"
        )).fetch_all(&pool).await.unwrap();
        assert_eq!(counts.len(), 3);
        assert!(counts.iter().all(|(_, held)| *held == 3));
    }
}

#[tokio::test]
async fn terminal_run_slot_left_by_crashed_worker_is_rediscoverable() {
    use runtrell_testkit::{DecisionFinalizer, DecisionValidator};
    let (pool, schema, s, _, c) = setup().await;
    let before = s.get(&c.run).await.unwrap();
    let output = serde_json::json!({"decision":"answered"});
    let receipt = DecisionValidator::default()
        .validate(&before, &output)
        .unwrap();
    let completion = DecisionFinalizer
        .finalize(&before, output, receipt)
        .unwrap();
    s.finalize(&c, completion, 0).await.unwrap();
    let reopened = PgStore::open(pool, &schema, Capacity::default())
        .await
        .unwrap();
    assert_eq!(
        reopened.recovery_candidates().await.unwrap(),
        vec![c.run.clone()]
    );
    let slots = reopened.held_slots(&c.run).await.unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].kind, OperationKind::Run);
    reopened.release_slot(&slots[0].key).await.unwrap();
    reopened.release_slot(&slots[0].key).await.unwrap();
    assert!(reopened.recovery_candidates().await.unwrap().is_empty());
}

#[tokio::test]
async fn unrelated_run_update_finishes_while_another_run_row_is_locked() {
    let (pool, schema, s, _, c) = setup().await;
    let a = s
        .create_account("other", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("other", "independent", a, "input"))
        .await
        .unwrap();
    let other = s.claim(&h, "other", 0, 100).await.unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query(&format!(
        "SELECT revision FROM \"{schema}\".runs WHERE tenant=$1 AND id=$2 FOR UPDATE"
    ))
    .bind(&c.run.tenant)
    .bind(&c.run.id)
    .fetch_one(&mut *held)
    .await
    .unwrap();
    let blocked_store = s.clone();
    let blocked_claim = c.clone();
    let blocked = tokio::spawn(async move { blocked_store.renew(&blocked_claim, 1, 100).await });
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(1), s.renew(&other, 1, 100)).await;
    assert!(result.unwrap().is_ok());
    assert!(!blocked.is_finished());
    held.rollback().await.unwrap();
    blocked.await.unwrap().unwrap();
}

async fn parallel_store() -> (sqlx::PgPool, String, PgStore) {
    let url =
        std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated test database required");
    let pool = PgPoolOptions::new()
        .max_connections(24)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("parallel_test_{}", uuid::Uuid::new_v4().simple());
    let mut cap = Capacity {
        queue: 128,
        tenant_queue: 64,
        ..Capacity::default()
    };
    for kind in [
        OperationKind::Run,
        OperationKind::Model,
        OperationKind::Tool,
    ] {
        cap.global.insert(kind, 64);
        cap.tenant.insert(kind, 64);
        cap.run.insert(kind, 64);
    }
    let store = PgStore::open(pool.clone(), &schema, cap).await.unwrap();
    (pool, schema, store)
}

#[tokio::test]
async fn many_runs_reserve_shared_ancestors_without_double_spend_or_deadlock() {
    let (_, _, s) = parallel_store().await;
    let root = s
        .create_account("*", Amount::new(400, 400), None)
        .await
        .unwrap();
    let branches = [
        s.create_account("*", Amount::new(400, 400), Some(root.clone()))
            .await
            .unwrap(),
        s.create_account("*", Amount::new(400, 400), Some(root.clone()))
            .await
            .unwrap(),
    ];
    let mut claims = Vec::new();
    let mut leaves = Vec::new();
    for i in 0..48 {
        let tenant = format!("tenant-{i}");
        let leaf = s
            .create_account(
                &tenant,
                Amount::new(100, 100),
                Some(branches[i % 2].clone()),
            )
            .await
            .unwrap();
        let h = s
            .start(RunSpec::new(&tenant, "parallel", leaf.clone(), "input"))
            .await
            .unwrap();
        claims.push(s.claim(&h, "worker", 0, 100).await.unwrap());
        leaves.push(leaf);
    }
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(48));
    let start = std::time::Instant::now();
    let mut tasks = Vec::new();
    for claim in claims {
        let (store, barrier) = (s.clone(), barrier.clone());
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .begin(
                    &claim,
                    DispatchRequest::new(
                        "reserve",
                        OperationKind::Model,
                        "fake",
                        serde_json::json!({}),
                        Amount::new(20, 20),
                    ),
                    0,
                )
                .await
        }));
    }
    let mut accepted = 0;
    for task in tasks {
        match tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
        {
            Ok(_) => accepted += 1,
            Err(Error::ReservationDenied) => {}
            other => panic!("unexpected reservation outcome: {other:?}"),
        }
    }
    assert_eq!(accepted, 20);
    assert_eq!(
        s.account(&root).await.unwrap().reserved,
        Amount::new(400, 400)
    );
    let mut total = Amount::default();
    for id in leaves {
        total = total
            .checked_add(s.account(&id).await.unwrap().reserved)
            .unwrap();
    }
    assert_eq!(total, Amount::new(400, 400));
    let sum = s
        .account(&branches[0])
        .await
        .unwrap()
        .reserved
        .checked_add(s.account(&branches[1]).await.unwrap().reserved)
        .unwrap();
    assert_eq!(sum, total);
    println!(
        "schema2 shared ancestors: requests=48 accepted={accepted} denied=28 elapsed_ms={}",
        start.elapsed().as_millis()
    );
}

#[tokio::test]
async fn concurrent_queue_workers_take_each_entry_once_and_skip_locked_entry() {
    let (pool, schema, s) = parallel_store().await;
    for i in 0..64 {
        s.enqueue(QueueEntry {
            key: format!("entry-{i}"),
            tenant: format!("t{}", i % 8),
            scheduling: SchedulingPolicy {
                priority: i32::from(i % 3 != 0),
                ..SchedulingPolicy::default()
            },
            deadline: 1000,
        })
        .await
        .unwrap();
    }
    // A claim/dequeue transaction that already owns one entry must not stall others.
    let mut held = pool.begin().await.unwrap();
    sqlx::query(&format!(
        "SELECT key FROM \"{schema}\".queue_entries WHERE key='entry-8' FOR UPDATE"
    ))
    .fetch_one(&mut *held)
    .await
    .unwrap();
    let other = tokio::time::timeout(std::time::Duration::from_secs(1), s.dequeue(0))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_ne!(other.key, "entry-8");
    held.rollback().await.unwrap();
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let store = s.clone();
        tasks.push(tokio::spawn(async move {
            let mut keys = Vec::new();
            while let Some(e) = store.dequeue(0).await.unwrap() {
                keys.push(e.key);
            }
            keys
        }));
    }
    let mut all = std::collections::BTreeSet::from([other.key]);
    let mut count = 1;
    for task in tasks {
        for k in task.await.unwrap() {
            count += 1;
            assert!(all.insert(k), "duplicate dequeue");
        }
    }
    assert_eq!(count, 64);
    assert_eq!(all.len(), 64);
    assert_eq!(s.queue_len().await.unwrap(), 0);
    println!("schema2 queue: workers=16 entries=64 unique=64");
}

#[tokio::test]
async fn parallel_run_update_throughput_measurement() {
    let (_, _, s) = parallel_store().await;
    let mut claims = Vec::new();
    for i in 0..24 {
        let tenant = format!("bench-{i}");
        let account = s
            .create_account(&tenant, Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let h = s
            .start(RunSpec::new(&tenant, "bench", account, "input"))
            .await
            .unwrap();
        claims.push(s.claim(&h, "worker", 0, 100).await.unwrap());
    }
    for concurrency in [1, 8, 24] {
        let start = std::time::Instant::now();
        for chunk in claims.chunks(concurrency) {
            let mut tasks = Vec::new();
            for c in chunk {
                let (store, claim) = (s.clone(), c.clone());
                tasks.push(tokio::spawn(async move {
                    for _ in 0..20 {
                        store.renew(&claim, 1, 100).await.unwrap();
                    }
                }));
            }
            for t in tasks {
                t.await.unwrap();
            }
        }
        let seconds = start.elapsed().as_secs_f64();
        println!(
            "schema2 throughput: runs=24 updates=480 concurrency={concurrency} elapsed_ms={:.3} updates_per_second={:.2}",
            seconds * 1000.0,
            480.0 / seconds
        );
    }
    for c in claims {
        assert_eq!(s.get(&c.run).await.unwrap().revision, 61);
    }
}

#[tokio::test]
async fn unrelated_budget_and_capacity_progress_while_other_rows_are_locked() {
    let (pool, schema, s) = parallel_store().await;
    let mut pairs = Vec::new();
    for tenant in ["isolated-a", "isolated-b"] {
        let account = s
            .create_account(tenant, Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let h = s
            .start(RunSpec::new(tenant, "work", account.clone(), "input"))
            .await
            .unwrap();
        let claim = s.claim(&h, "worker", 0, 100).await.unwrap();
        pairs.push((account, claim));
    }
    let op = s
        .begin(
            &pairs[0].1,
            DispatchRequest::new(
                "first",
                OperationKind::Model,
                "fake",
                serde_json::json!({}),
                Amount::new(10, 10),
            ),
            0,
        )
        .await
        .unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query(&format!(
        "SELECT id FROM \"{schema}\".budget_accounts WHERE id=$1 FOR UPDATE"
    ))
    .bind(&pairs[0].0)
    .fetch_one(&mut *held)
    .await
    .unwrap();
    sqlx::query(&format!("SELECT ordinal FROM \"{schema}\".capacity_slots WHERE scope='g:Model' AND holder=$1 FOR UPDATE"))
        .bind(format!("slot:{}",op.id)).fetch_one(&mut *held).await.unwrap();
    let b = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        s.begin(
            &pairs[1].1,
            DispatchRequest::new(
                "second",
                OperationKind::Model,
                "fake",
                serde_json::json!({}),
                Amount::new(10, 10),
            ),
            0,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(op.id, b.id);
    assert_eq!(
        s.account(&pairs[1].0).await.unwrap().reserved,
        Amount::new(10, 10)
    );
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn queue_cursor_preserves_original_rotation_and_burst_after_restart() {
    let (pool, schema, s) = parallel_store().await;
    for (key, tenant, fast) in [
        ("b1", "b", true),
        ("a1", "a", true),
        ("c1", "c", true),
        ("a2", "a", false),
        ("b2", "b", true),
    ] {
        s.enqueue(QueueEntry {
            key: key.into(),
            tenant: tenant.into(),
            scheduling: SchedulingPolicy {
                priority: i32::from(fast),
                ..SchedulingPolicy::default()
            },
            deadline: 1000,
        })
        .await
        .unwrap();
    }
    assert_eq!(s.dequeue(0).await.unwrap().unwrap().key, "b1");
    drop(s);
    // Match the parallel_store configuration without changing it.
    let config: serde_json::Value =
        sqlx::query_scalar(&format!("SELECT capacity FROM \"{schema}\".schema_config"))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(config["queue"], 128);
    let mut cap = Capacity {
        queue: 128,
        tenant_queue: 64,
        ..Capacity::default()
    };
    for kind in [
        OperationKind::Run,
        OperationKind::Model,
        OperationKind::Tool,
    ] {
        cap.global.insert(kind, 64);
        cap.tenant.insert(kind, 64);
        cap.run.insert(kind, 64);
    }
    let s = PgStore::open(pool, &schema, cap).await.unwrap();
    let mut keys = Vec::new();
    while let Some(e) = s.dequeue(0).await.unwrap() {
        keys.push(e.key);
    }
    assert_eq!(keys, vec!["c1", "a2", "b2", "a1"]);
}

#[test]
fn repair_checkpoint_reopens_with_independent_pool_and_runtime() {
    use runtrell::assembly::ValidationChain;
    use runtrell_testkit::{BusinessTool, DecisionFinalizer, FakeModel, FakePolicy, TestClock};
    use serde_json::json;
    use std::{collections::BTreeSet, sync::Arc};
    fn validator() -> ValidationChain {
        ValidationChain {
            validators: vec![],
            schema: json!({"type":"object","required":["answer"],"properties":{"answer":{"type":"integer","minimum":1}}}),
            repair: RepairPolicy::default(),
            repairable_codes: BTreeSet::from(["SCHEMA_VALIDATION".into()]),
        }
    }
    fn executor() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }
    let schema = format!("repair_{}", uuid::Uuid::new_v4().simple());
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("disposable database required");
    let first = executor();
    let (h, account, checkpoint) = first.block_on(async {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap();
        let store = Arc::new(
            PgStore::open(pool.clone(), &schema, Capacity::default())
                .await
                .unwrap(),
        );
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let h = store
            .start(RunSpec::new(
                "t",
                "repair-reopen",
                account.clone(),
                "answer",
            ))
            .await
            .unwrap();
        let claim = store.claim(&h, "first", 0, 100).await.unwrap();
        let rt = Runtime {
            store: store.clone(),
            model: FakeModel::scripted(vec![FakeModel::decision(json!({"answer":0}))]),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(validator()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let waiting = rt.drive(&claim).await.unwrap();
        assert_eq!(waiting.state, RunState::Waiting("validation repair".into()));
        let cp = waiting.checkpoint.unwrap();
        assert_eq!(cp.payload["repair"]["candidate"], json!({"answer":0}));
        assert_eq!(
            cp.payload["repair"]["receipt"]["diagnostics"][0]["path"],
            "/answer"
        );
        drop(rt);
        drop(store);
        pool.close().await;
        (h, account, cp)
    });
    drop(first);
    let second = executor();
    second.block_on(async {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap();
        let store = Arc::new(
            PgStore::open(pool.clone(), &schema, Capacity::default())
                .await
                .unwrap(),
        );
        assert_eq!(store.get(&h).await.unwrap().checkpoint, Some(checkpoint));
        let claim = store.claim(&h, "second", 101, 100).await.unwrap();
        let clock = TestClock::default();
        clock.set(101);
        let model = FakeModel::scripted(vec![FakeModel::decision(json!({"answer":2}))]);
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(validator()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let done = rt.drive(&claim).await.unwrap();
        assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
        assert_eq!(done.output, Some(json!({"answer":2})));
        assert!(done.receipt.unwrap().passed);
        let ops = store.list_operations(&h).await.unwrap();
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|op| op.settled));
        assert_eq!(store.account(&account).await.unwrap().used.tokens, 8);
        assert_eq!(model.requests.lock().unwrap().len(), 1);
        assert!(
            model.requests.lock().unwrap()[0]
                .messages
                .iter()
                .any(|m| m.text.contains("SCHEMA_VALIDATION"))
        );
        drop(rt);
        drop(store);
        pool.close().await;
    });
}

#[test]
fn json_syntax_repair_reopens_with_independent_pool_and_runtime() {
    use runtrell::assembly::ValidationChain;
    use runtrell_testkit::{BusinessTool, DecisionFinalizer, FakeModel, FakePolicy, TestClock};
    use serde_json::json;
    use std::{collections::BTreeSet, sync::Arc};
    fn validator() -> ValidationChain {
        ValidationChain {
            validators: vec![],
            schema: json!({"type":"object","required":["answer"],"properties":{"answer":{"type":"integer","minimum":1}}}),
            repair: RepairPolicy::default(),
            repairable_codes: BTreeSet::from(["JSON_SYNTAX".into()]),
        }
    }
    fn executor() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }
    let schema = format!("repair_{}", uuid::Uuid::new_v4().simple());
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("disposable database required");
    let first = executor();
    let (h, account, checkpoint) = first.block_on(async {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap();
        let store = Arc::new(
            PgStore::open(pool.clone(), &schema, Capacity::default())
                .await
                .unwrap(),
        );
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let h = store
            .start(RunSpec::new(
                "t",
                "repair-reopen",
                account.clone(),
                "answer",
            ))
            .await
            .unwrap();
        let claim = store.claim(&h, "first", 0, 100).await.unwrap();
        let rt = Runtime {
            store: store.clone(),
            model: FakeModel::scripted(vec![ModelReply {
                text: "{\n  \"answer\": }".into(),
                ..FakeModel::decision(serde_json::Value::Null)
            }]),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(validator()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let waiting = rt.drive(&claim).await.unwrap();
        assert_eq!(waiting.state, RunState::Waiting("validation repair".into()));
        let cp = waiting.checkpoint.unwrap();
        assert_eq!(
            cp.payload["repair"]["candidate"],
            json!("{\n  \"answer\": }")
        );
        assert_eq!(
            cp.payload["repair"]["receipt"]["diagnostics"][0]["path"],
            ""
        );
        assert_eq!(store.account(&account).await.unwrap().used.tokens, 4);
        let operations = store.list_operations(&h).await.unwrap();
        assert_eq!(operations.len(), 1);
        assert!(operations[0].settled);
        assert_eq!(
            cp.payload["repair"]["receipt"]["diagnostics"][0]["position"],
            json!({"line":2,"column":13,"byte_offset":14})
        );
        drop(rt);
        drop(store);
        pool.close().await;
        (h, account, cp)
    });
    drop(first);
    let second = executor();
    second.block_on(async {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap();
        let store = Arc::new(
            PgStore::open(pool.clone(), &schema, Capacity::default())
                .await
                .unwrap(),
        );
        assert_eq!(store.get(&h).await.unwrap().checkpoint, Some(checkpoint));
        assert_eq!(store.account(&account).await.unwrap().used.tokens, 4);
        let claim = store.claim(&h, "second", 101, 100).await.unwrap();
        let clock = TestClock::default();
        clock.set(101);
        let model = FakeModel::scripted(vec![FakeModel::decision(json!({"answer":2}))]);
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(validator()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let done = rt.drive(&claim).await.unwrap();
        assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
        assert_eq!(done.output, Some(json!({"answer":2})));
        assert!(done.receipt.as_ref().unwrap().passed);
        assert_eq!(rt.drive(&claim).await.unwrap(), done);
        let ops = store.list_operations(&h).await.unwrap();
        assert_eq!(ops.len(), 2);
        assert!(
            ops.iter()
                .all(|op| op.settled && op.request.reserve.tokens > 0)
        );
        assert_eq!(store.account(&account).await.unwrap().used.tokens, 8);
        assert_eq!(model.requests.lock().unwrap().len(), 1);
        assert!(
            model.requests.lock().unwrap()[0]
                .messages
                .iter()
                .any(|m| m.text.contains("JSON_SYNTAX"))
        );
        drop(rt);
        drop(store);
        pool.close().await;
    });
}
