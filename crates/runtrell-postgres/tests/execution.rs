// SPDX-License-Identifier: Apache-2.0
#[allow(dead_code)]
mod support;
use runtrell::{Capacity, Claim, Error, RunHandle, ToolContext, execution::*};
use runtrell_postgres::PgStore;
use runtrell_testkit::execution_contracts::LedgerHarness;
use sqlx::{PgPool, postgres::PgPoolOptions};
use support::block_on;
struct PgHarness {
    ledger: PgStore,
    pool: PgPool,
    schema: String,
}
impl LedgerHarness for PgHarness {
    type Ledger = PgStore;
    async fn open() -> Self {
        let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("test DB required");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("execution_{}", uuid::Uuid::new_v4().simple());
        let ledger = PgStore::open(pool.clone(), &schema, Capacity::default())
            .await
            .unwrap();
        Self {
            ledger,
            pool,
            schema,
        }
    }
    fn ledger(&self) -> &PgStore {
        &self.ledger
    }
    async fn reopen(&self) -> PgStore {
        PgStore::open(self.pool.clone(), &self.schema, Capacity::default())
            .await
            .unwrap()
    }
}
runtrell_testkit::execution_ledger_contract_tests!(PgHarness);

#[test]
fn execution_sql_failure_rolls_back_and_two_clients_cas_once() {
    block_on(async {
        let h = PgHarness::open().await;
        let c = ToolContext {
            claim: Claim {
                run: RunHandle {
                    tenant: "t".into(),
                    id: "r".into(),
                },
                worker: "w".into(),
                generation: 1,
                lease_until: 1000,
            },
            principal: "host".into(),
            operation: "op".into(),
            attempt: 1,
            deadline: None,
            permission: "execute".into(),
            account_binding: "host".into(),
        };
        let intent = ExecutionRecord::intent(&c, ExecutionRequest::process("test-only", vec![]));
        h.ledger.fail_next_write();
        assert_eq!(
            h.ledger.insert(intent.clone()).await.unwrap_err(),
            Error::Storage
        );
        assert_eq!(
            h.ledger.get(&c.claim.run, &c.operation).await.unwrap_err(),
            Error::NotFound
        );
        let mut r = h.ledger.insert(intent).await.unwrap();
        r.cancel_requested = true;
        h.ledger.fail_next_write();
        assert_eq!(
            h.ledger.cas(0, r.clone()).await.unwrap_err(),
            Error::Storage
        );
        assert!(
            !h.reopen()
                .await
                .get(&r.owner, &r.context_operation)
                .await
                .unwrap()
                .cancel_requested
        );
        let other = h.reopen().await;
        let (a, b) = tokio::join!(h.ledger.cas(0, r.clone()), other.cas(0, r));
        assert!(matches!(
            (a, b),
            (Ok(_), Err(Error::Conflict)) | (Err(Error::Conflict), Ok(_))
        ));
        assert_eq!(other.pending().await.unwrap()[0].revision, 1);
    });
}
