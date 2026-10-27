// SPDX-License-Identifier: Apache-2.0
use runtrell::*;
use runtrell_postgres::PgStore;
use sqlx::postgres::PgPoolOptions;
use std::{future::Future, ops::Deref, sync::OnceLock};

pub fn block_on<F: Future>(f: F) -> F::Output {
    // Background I/O reactor keeps running even for synchronous Validator callbacks
    // and contract tests that poll a fake model manually.
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
    });
    let _enter = rt.enter();
    runtrell_testkit::block_on(f)
}
pub struct TestPg(pub PgStore, pub Option<(String, sqlx::PgPool)>);
impl Default for TestPg {
    fn default() -> Self {
        Self::new(Capacity::default())
    }
}
impl TestPg {
    pub fn new(capacity: Capacity) -> Self {
        block_on(async {
            let url = std::env::var("RUNTRELL_TEST_DATABASE_URL")
                .expect("RUNTRELL_TEST_DATABASE_URL is required; Pg tests never skip");
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .connect(&url)
                .await
                .unwrap();
            Self(
                PgStore::open(
                    pool,
                    &format!("contract_{}", uuid::Uuid::new_v4().simple()),
                    capacity,
                )
                .await
                .unwrap(),
                None,
            )
        })
    }
    pub async fn slot_count(&self, kind: OperationKind) -> usize {
        self.0.slot_count(kind).await.unwrap()
    }
    pub async fn queue_len(&self) -> usize {
        self.0.queue_len().await.unwrap()
    }
}
impl Deref for TestPg {
    type Target = PgStore;
    fn deref(&self) -> &PgStore {
        &self.0
    }
}

impl RunStore for TestPg {
    async fn renew(&self, claim: &Claim, now: u64, lease: u64) -> Result<Claim> {
        self.0.renew(claim, now, lease).await
    }
    async fn find_request(&self, spec: &RunSpec) -> Result<Option<RunHandle>> {
        self.0.find_request(spec).await
    }
    async fn start(&self, spec: RunSpec) -> Result<RunHandle> {
        self.0.start(spec).await
    }
    async fn admit_child(
        &self,
        c: &Claim,
        key: &str,
        spec: RunSpec,
        now: u64,
    ) -> Result<RunHandle> {
        if self.1.as_ref().map(|(phase, _)| phase.as_str()) == Some("before") {
            child_stop(None);
        }
        if self.1.as_ref().map(|(phase, _)| phase.as_str()) == Some("uncommitted") {
            let mut tx = self
                .1
                .as_ref()
                .unwrap()
                .1
                .begin()
                .await
                .map_err(|_| Error::Storage)?;
            let child = self.0.admit_child_in(&mut tx, c, key, spec, now).await?;
            child_stop(Some(&child));
        }
        let child = self.0.admit_child(c, key, spec, now).await?;
        if self.1.as_ref().map(|(phase, _)| phase.as_str()) == Some("created") {
            child_stop(Some(&child));
        }
        Ok(child)
    }
    async fn get(&self, run: &RunHandle) -> Result<RunRecord> {
        self.0.get(run).await
    }
    async fn claim(&self, run: &RunHandle, worker: &str, now: u64, lease: u64) -> Result<Claim> {
        self.0.claim(run, worker, now, lease).await
    }
    async fn check(&self, claim: &Claim, now: u64, new_work: bool) -> Result<RunRecord> {
        self.0.check(claim, now, new_work).await
    }
    async fn checkpoint(
        &self,
        claim: &Claim,
        expected: u64,
        checkpoint: Checkpoint,
        now: u64,
    ) -> Result<()> {
        self.0.checkpoint(claim, expected, checkpoint, now).await
    }
    async fn cancel(&self, run: &RunHandle, reason: &str) -> Result<()> {
        self.0.cancel(run, reason).await
    }
    async fn terminate(&self, claim: &Claim, terminal: Terminal, now: u64) -> Result<()> {
        self.0.terminate(claim, terminal, now).await
    }
    async fn finalize(&self, claim: &Claim, completion: Completion, now: u64) -> Result<()> {
        self.0.finalize(claim, completion, now).await
    }
    async fn wait(&self, claim: &Claim, reason: &str, now: u64) -> Result<()> {
        self.0.wait(claim, reason, now).await
    }
    async fn cleanup(
        &self,
        claim: &Claim,
        stop: Progress,
        cleanup: Progress,
        now: u64,
    ) -> Result<()> {
        self.0.cleanup(claim, stop, cleanup, now).await
    }
}
impl BudgetAuthority for TestPg {
    async fn create_account_with_policy(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
        policy: AccountPolicy,
    ) -> Result<String> {
        self.0
            .create_account_with_policy(tenant, limit, parent, policy)
            .await
    }
    async fn finalization_allowed(&self, claim: &Claim, now: u64) -> Result<()> {
        self.0.finalization_allowed(claim, now).await
    }
    async fn exhaust(&self, claim: &Claim, now: u64) -> Result<()> {
        self.0.exhaust(claim, now).await
    }
    async fn create_account(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
    ) -> Result<String> {
        self.0.create_account(tenant, limit, parent).await
    }
    async fn account(&self, id: &str) -> Result<Account> {
        self.0.account(id).await
    }
    async fn remaining(&self, claim: &Claim, now: u64) -> Result<Amount> {
        self.0.remaining(claim, now).await
    }
    async fn warnings(&self, claim: &Claim, now: u64) -> Result<Vec<Warning>> {
        self.0.warnings(claim, now).await
    }
}
impl OperationStore for TestPg {
    async fn observe_result(
        &self,
        claim: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> Result<()> {
        self.0.observe_result(claim, id, result, now).await
    }
    async fn proven_unsent(&self, claim: &Claim, id: &str, scope: &str, now: u64) -> Result<()> {
        self.0.proven_unsent(claim, id, scope, now).await
    }
    async fn begin(&self, claim: &Claim, request: DispatchRequest, now: u64) -> Result<Operation> {
        self.0.begin(claim, request, now).await
    }
    async fn list_operations(&self, run: &RunHandle) -> Result<Vec<Operation>> {
        self.0.list_operations(run).await
    }
    async fn operation(&self, run: &RunHandle, key: &str) -> Result<Option<Operation>> {
        self.0.operation(run, key).await
    }
    async fn mark_sent(&self, claim: &Claim, id: &str, now: u64) -> Result<()> {
        self.0.mark_sent(claim, id, now).await
    }
    async fn record(
        &self,
        claim: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> Result<()> {
        if self.1.as_ref().map(|(phase, _)| phase.as_str()) == Some("terminal")
            && let Some(op) = self
                .0
                .list_operations(&claim.run)
                .await?
                .into_iter()
                .find(|op| op.id == id && op.request.kind == OperationKind::ChildRun)
        {
            child_stop(Some(&op.child_handle()?));
        }
        self.0.record(claim, id, result, now).await
    }
}
impl Scheduler for TestPg {
    async fn acquire(&self, request: SlotRequest) -> Result<()> {
        self.0.acquire(request).await
    }
    async fn release_slot(&self, key: &str) -> Result<()> {
        self.0.release_slot(key).await
    }
    async fn enqueue(&self, entry: QueueEntry) -> Result<()> {
        self.0.enqueue(entry).await
    }
    async fn dequeue(&self, now: u64) -> Result<Option<QueueEntry>> {
        self.0.dequeue(now).await
    }
}

// Test-only boundaries inside spawn_child and before parent settlement. The
// supervisor kills the OS process after this receipt; production has no hooks.
fn child_stop(child: Option<&RunHandle>) -> ! {
    use std::io::Write;
    println!(
        "RUNTRELL_CHILD_READY {}",
        child.map_or("-", |h| h.id.as_str())
    );
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}
