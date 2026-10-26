// SPDX-License-Identifier: Apache-2.0
use runtrell::*;
use runtrell_postgres::PgStore;
use runtrell_testkit::{
    BusinessTool, DecisionFinalizer, DecisionValidator, FakeModel, FakePolicy, TestClock,
};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn open() -> (PgPool, String, Arc<PgStore>) {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("dedicated database required");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("storage_{}", uuid::Uuid::new_v4().simple());
    let store = Arc::new(
        PgStore::open(pool.clone(), &schema, Capacity::default())
            .await
            .unwrap(),
    );
    (pool, schema, store)
}
async fn reopen(schema: &str) -> Arc<PgStore> {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    Arc::new(
        PgStore::open(pool, schema, Capacity::default())
            .await
            .unwrap(),
    )
}
fn request() -> DispatchRequest {
    DispatchRequest::new(
        "effect",
        OperationKind::Sandbox,
        "external",
        json!({"input":1}),
        Amount::new(10, 10),
    )
}
fn paid() -> ExternalResult {
    ExternalResult::Applied {
        physical_evidence: None,
        output: json!({"receipt":"saved"}),
        usage: Usage::measured(2, 3, 7),
        external_id: Some("paid-1".into()),
    }
}
async fn run(s: &PgStore, tenant: &str, account: &str, key: &str) -> Claim {
    let h = s
        .start(RunSpec::new(tenant, key, account.into(), "input"))
        .await
        .unwrap();
    s.claim(&h, "first", 0, 100).await.unwrap()
}
async fn entries(
    pool: &PgPool,
    schema: &str,
    id: &str,
) -> Vec<(String, Value, Option<Value>, bool, bool)> {
    sqlx::query_as(&format!("SELECT account_id,reservation,usage,unknown,settled FROM {schema}.budget_entries WHERE attempt_id=$1 ORDER BY account_id"))
        .bind(id).fetch_all(pool).await.unwrap()
}
async fn assert_balance(s: &PgStore, ids: &[String], reserved: Amount, used: Amount, unknown: u64) {
    for id in ids {
        let a = s.account(id).await.unwrap();
        assert_eq!((a.reserved, a.used, a.unknown), (reserved, used, unknown));
    }
}

#[tokio::test]
async fn attempt_identity_retry_fencing_and_constraints() {
    let (pool, schema, s) = open().await;
    let a = s
        .create_account("t", Amount::new(100, 100), None)
        .await
        .unwrap();
    let c = run(&s, "t", &a, "identity").await;
    assert_eq!(
        s.start(RunSpec::new("other", "foreign", a.clone(), "input"))
            .await,
        Err(Error::PermissionDenied)
    );
    let d = request();
    let (left, right) = tokio::join!(s.begin(&c, d.clone(), 0), s.begin(&c, d.clone(), 0));
    let op = left.unwrap();
    assert_eq!(right.unwrap(), op);
    let mut changed = d.clone();
    changed.input = json!({"input":2});
    assert_eq!(s.begin(&c, changed, 0).await, Err(Error::Conflict));
    let mut retry = d.clone();
    retry.attempt = 2;
    assert_eq!(s.begin(&c, retry.clone(), 0).await, Err(Error::Conflict));
    let (left, right) = tokio::join!(s.mark_sent(&c, &op.id, 0), s.mark_sent(&c, &op.id, 0));
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    assert_eq!(
        s.operation(&c.run, "effect")
            .await
            .unwrap()
            .unwrap()
            .send_count,
        1
    );
    s.record(&c, &op.id, ExternalResult::Unknown, 0)
        .await
        .unwrap();
    assert_eq!(s.begin(&c, retry.clone(), 0).await, Err(Error::Conflict));
    let failed = ExternalResult::Failed {
        physical_evidence: None,
        code: "retry".into(),
        usage: Usage::measured(1, 1, 2),
        retry_after: Some(1),
    };
    s.record(&c, &op.id, failed.clone(), 0).await.unwrap();
    s.record(&c, &op.id, failed, 0).await.unwrap();
    for field in ["target", "version", "account"] {
        let mut changed = retry.clone();
        match field {
            "target" => changed.target = "different".into(),
            "version" => changed.version = "different".into(),
            _ => changed.account_binding = "different".into(),
        }
        assert_eq!(s.begin(&c, changed, 0).await, Err(Error::Conflict));
    }
    let second = s.begin(&c, retry, 0).await.unwrap();
    assert_ne!(op.id, second.id);
    assert_eq!(op.request.key, second.request.key);
    let next = s.claim(&c.run, "second", 101, 100).await.unwrap();
    assert_eq!(
        s.mark_sent(&c, &second.id, 101).await,
        Err(Error::OwnershipLost)
    );
    let reopened = reopen(&schema).await;
    assert_eq!(reopened.list_operations(&c.run).await.unwrap().len(), 2);
    assert_eq!(
        reopened.operation(&c.run, "effect").await.unwrap(),
        Some(second.clone())
    );
    reopened.mark_sent(&next, &second.id, 101).await.unwrap();
    reopened
        .record(&next, &second.id, paid(), 101)
        .await
        .unwrap();
    let mut third = second.request.clone();
    third.attempt = 3;
    assert_eq!(
        reopened.begin(&next, third, 101).await,
        Err(Error::Conflict)
    );
    assert_eq!(reopened.account(&a).await.unwrap().used, Amount::new(7, 9));
    assert_eq!(
        reopened.account(&a).await.unwrap().reserved,
        Amount::default()
    );
    let b = s
        .create_account("other", Amount::new(100, 100), None)
        .await
        .unwrap();
    let other = run(&s, "other", &b, "identity").await;
    let other_op = s.begin(&other, d, 0).await.unwrap();
    assert_ne!(op.id, other_op.id);
    assert_eq!(
        s.record(&other, &op.id, paid(), 0).await,
        Err(Error::PermissionDenied)
    );
    // Exercise database uniqueness and Run referential integrity, not just API checks.
    for (id, run_value, code) in [
        ("duplicate", json!(c.run), "23505"),
        ("orphan", json!({"tenant":"t","id":"absent"}), "23503"),
    ] {
        let error = sqlx::query(&format!("INSERT INTO {schema}.attempts(id,run,request,state,settled,unknown_usage,send_count,created_at,usage_observations) SELECT $1,$2,request,state,settled,unknown_usage,send_count,created_at,usage_observations FROM {schema}.attempts WHERE id=$3"))
            .bind(id).bind(run_value).bind(&op.id).execute(&pool).await.unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some(code)
        );
    }
}

#[tokio::test]
async fn ancestor_attribution_rollback_unknown_reopen_and_delete_restriction() {
    let (pool, schema, s) = open().await;
    let root = s
        .create_account("*", Amount::new(100, 100), None)
        .await
        .unwrap();
    let parent = s
        .create_account("*", Amount::new(100, 100), Some(root.clone()))
        .await
        .unwrap();
    let policy = AccountPolicy {
        tokens: false,
        warning_percentages: vec![40, 90],
        ..AccountPolicy::default()
    };
    let leaf = s
        .create_account_with_policy(
            "t",
            Amount::new(50, 50),
            Some(parent.clone()),
            policy.clone(),
        )
        .await
        .unwrap();
    let sibling = s
        .create_account("other", Amount::new(50, 50), Some(parent.clone()))
        .await
        .unwrap();
    let c = run(&s, "t", &leaf, "attribution").await;
    s.fail_next_write();
    assert_eq!(s.begin(&c, request(), 0).await, Err(Error::Storage));
    assert!(s.list_operations(&c.run).await.unwrap().is_empty());
    assert_balance(
        &s,
        &[root.clone(), parent.clone(), leaf.clone(), sibling.clone()],
        Amount::default(),
        Amount::default(),
        0,
    )
    .await;
    let op = s.begin(&c, request(), 0).await.unwrap();
    let error = sqlx::query(&format!("DELETE FROM {schema}.attempts WHERE id=$1"))
        .bind(&op.id)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
    let mut ids = vec![root.clone(), parent.clone(), leaf.clone()];
    ids.sort();
    assert_eq!(
        entries(&pool, &schema, &op.id).await,
        ids.iter()
            .map(|id| (id.clone(), json!(request().reserve), None, false, false))
            .collect::<Vec<_>>()
    );
    s.mark_sent(&c, &op.id, 0).await.unwrap();
    s.record(&c, &op.id, ExternalResult::Unknown, 0)
        .await
        .unwrap();
    assert_balance(&s, &ids, Amount::new(10, 10), Amount::default(), 1).await;
    assert!(
        entries(&pool, &schema, &op.id)
            .await
            .iter()
            .all(|row| row.3 && !row.4)
    );
    let reopened = reopen(&schema).await;
    assert_eq!(reopened.account(&leaf).await.unwrap().policy, policy);
    let next = reopened.claim(&c.run, "next", 101, 100).await.unwrap();
    reopened.fail_next_write();
    assert_eq!(
        reopened.record(&next, &op.id, paid(), 101).await,
        Err(Error::Storage)
    );
    assert_balance(&reopened, &ids, Amount::new(10, 10), Amount::default(), 1).await;
    reopened.record(&next, &op.id, paid(), 101).await.unwrap();
    reopened.record(&next, &op.id, paid(), 101).await.unwrap();
    assert_eq!(
        reopened
            .record(&next, &op.id, ExternalResult::Unknown, 101)
            .await,
        Err(Error::Conflict)
    );
    assert_balance(&reopened, &ids, Amount::default(), Amount::new(5, 7), 0).await;
    assert_balance(
        &reopened,
        &[sibling],
        Amount::default(),
        Amount::default(),
        0,
    )
    .await;
    assert_eq!(
        entries(&pool, &schema, &op.id).await,
        ids.iter()
            .map(|id| (
                id.clone(),
                json!(request().reserve),
                Some(json!(Usage::measured(2, 3, 7))),
                false,
                true
            ))
            .collect::<Vec<_>>()
    );
    // A future GC cannot remove a referenced account or Run and leave dangling attribution.
    for (table, column, id) in [
        ("attempts", "id", op.id.as_str()),
        ("budget_accounts", "id", leaf.as_str()),
        ("budget_accounts", "id", parent.as_str()),
        ("runs", "id", c.run.id.as_str()),
    ] {
        let error = sqlx::query(&format!("DELETE FROM {schema}.{table} WHERE {column}=$1"))
            .bind(id)
            .execute(&pool)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23503")
        );
    }
}

#[tokio::test]
async fn saved_paid_result_recovers_without_lookup_or_resend() {
    let (_, schema, s) = open().await;
    let a = s
        .create_account("t", Amount::new(100, 100), None)
        .await
        .unwrap();
    let c = run(&s, "t", &a, "saved").await;
    let op = s.begin(&c, request(), 0).await.unwrap();
    s.mark_sent(&c, &op.id, 0).await.unwrap();
    s.observe_result(&c, &op.id, paid(), 0).await.unwrap();
    drop(s); // The response is durable, but record/settlement has not run.
    let reopened = reopen(&schema).await;
    let next = reopened.claim(&c.run, "successor", 101, 100).await.unwrap();
    let clock = TestClock::default();
    clock.set(101);
    let rt = Runtime {
        store: reopened.clone(),
        model: FakeModel::default(),
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    // A current owner may not replace a complete saved result with different evidence.
    assert_eq!(
        reopened
            .observe_result(&next, &op.id, ExternalResult::Unknown, 101)
            .await,
        Err(Error::Conflict)
    );
    let mut different = paid();
    if let ExternalResult::Applied { output, .. } = &mut different {
        *output = json!({"receipt":"different"});
    }
    assert_eq!(
        reopened
            .observe_result(&next, &op.id, different.clone(), 101)
            .await,
        Err(Error::Conflict)
    );
    assert_eq!(
        reopened.record(&next, &op.id, different, 101).await,
        Err(Error::Conflict)
    );
    assert_eq!(
        reopened
            .record(&next, &op.id, ExternalResult::Unknown, 101)
            .await,
        Err(Error::Conflict)
    );
    reopened.fail_next_write();
    assert_eq!(
        reopened.record(&next, &op.id, paid(), 101).await,
        Err(Error::Storage)
    );
    assert_balance(
        &reopened,
        std::slice::from_ref(&a),
        Amount::new(10, 10),
        Amount::default(),
        0,
    )
    .await;
    assert_eq!(
        reopened
            .operation(&c.run, "effect")
            .await
            .unwrap()
            .unwrap()
            .unsettled_result,
        Some(paid())
    );
    let lookups = AtomicUsize::new(0);
    let result = rt
        .dispatch(
            &next,
            request(),
            "action",
            |_| async { panic!("saved result must never resend") },
            |_| async {
                lookups.fetch_add(1, Ordering::SeqCst);
                Ok(Lookup::Unknown)
            },
        )
        .await;
    assert_eq!(result, Ok(paid()));
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
    assert_balance(&reopened, &[a], Amount::default(), Amount::new(5, 7), 0).await;
    let saved = reopened.operation(&c.run, "effect").await.unwrap().unwrap();
    assert_eq!(saved.result, Some(paid()));
    assert!(saved.unsettled_result.is_none());
    assert!(saved.usage_observations.is_empty());
    assert_eq!(saved.send_count, 1);
    assert!(saved.settled);
}

#[tokio::test]
async fn saved_observations_recover_once_without_provider_lookup() {
    let (_, schema, s) = open().await;
    runtrell_testkit::recovery_contracts::saved_observations(s, reopen(&schema).await).await;
}

#[tokio::test]
async fn final_catalog() {
    let (pool, schema, _) = open().await;
    let objects: Vec<(String,String)> = sqlx::query_as("SELECT c.relname,c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relname IN ('operations','budget_entries') ORDER BY c.relname")
        .bind(&schema).fetch_all(&pool).await.unwrap();
    assert_eq!(objects, vec![("budget_entries".into(), "v".into())]);
    let version: i32 = sqlx::query_scalar(&format!("SELECT version FROM {schema}.schema_config"))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 4);
    for (label, query) in [
        (
            "relations",
            "SELECT c.relname||':'||c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relkind IN ('r','v','m') ORDER BY c.relname",
        ),
        (
            "columns",
            "SELECT c.relname||'.'||a.attname||':'||format_type(a.atttypid,a.atttypmod)||':'||a.attnotnull::text||':'||a.attgenerated::text FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relkind IN ('r','v','m') AND a.attnum>0 AND NOT a.attisdropped ORDER BY c.relname,a.attnum",
        ),
        (
            "indexes",
            "SELECT indexname||':'||indexdef FROM pg_indexes WHERE schemaname=$1 ORDER BY indexname",
        ),
        (
            "constraints",
            "SELECT c.relname||':'||p.conname||':'||pg_get_constraintdef(p.oid) FROM pg_constraint p JOIN pg_class c ON c.oid=p.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 ORDER BY c.relname,p.conname",
        ),
        (
            "functions",
            "SELECT p.proname||':'||pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=$1 ORDER BY p.proname",
        ),
        (
            "triggers",
            "SELECT t.tgname||':'||pg_get_triggerdef(t.oid) FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND NOT t.tgisinternal ORDER BY t.tgname",
        ),
        (
            "views",
            "SELECT viewname||':'||definition FROM pg_views WHERE schemaname=$1 ORDER BY viewname",
        ),
    ] {
        let rows: Vec<String> = sqlx::query_scalar(query)
            .bind(&schema)
            .fetch_all(&pool)
            .await
            .unwrap();
        println!("CATALOG {label} {}", rows.len());
        for row in rows {
            println!("{label}\t{}", row.replace(&schema, "SCHEMA"));
        }
    }
}
