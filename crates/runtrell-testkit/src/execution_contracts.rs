// SPDX-License-Identifier: Apache-2.0
//! Shared Memory/Pg ledger contracts. Reopen must create a fresh adapter client.
use crate::FakeExecutionBackend;
use runtrell::{execution::*, *};
use std::sync::atomic::Ordering;

#[allow(async_fn_in_trait)]
pub trait LedgerHarness {
    type Ledger: ExecutionLedger;
    async fn open() -> Self;
    fn ledger(&self) -> &Self::Ledger;
    async fn reopen(&self) -> Self::Ledger;
}
pub struct MemoryLedgerHarness(MemoryExecutionLedger);
impl LedgerHarness for MemoryLedgerHarness {
    type Ledger = MemoryExecutionLedger;
    async fn open() -> Self {
        Self(MemoryExecutionLedger::default())
    }
    fn ledger(&self) -> &Self::Ledger {
        &self.0
    }
    async fn reopen(&self) -> Self::Ledger {
        self.0.clone()
    }
}
fn context() -> ToolContext {
    ToolContext {
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
    }
}
fn intent() -> ExecutionRecord {
    ExecutionRecord::intent(&context(), ExecutionRequest::process("test-only", vec![]))
}
pub async fn identity<H: LedgerHarness>() {
    let h = H::open().await;
    let l = h.ledger();
    let mut r = intent();
    // Fixed workspace contents, not merely an opaque snapshot identifier.
    use runtrell::data::*;
    let w = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner: r.owner.clone(),
        agent: "main".into(),
    });
    w.write(&access, "input", None, b"fixed").await.unwrap();
    freeze_workspace(&w, &access, &mut r.request, 2)
        .await
        .unwrap();
    assert_eq!(l.insert(r.clone()).await.unwrap(), r);
    assert_eq!(l.insert(r.clone()).await.unwrap(), r);
    let reopened = h.reopen().await;
    assert_eq!(
        reopened.get(&r.owner, &r.context_operation).await.unwrap(),
        r
    );
    let mut changed = r.clone();
    changed
        .request
        .input_files
        .insert("input".into(), b"other".to_vec());
    assert!(l.insert(changed).await.is_err());
    let mut other = r.owner.clone();
    other.tenant = "other".into();
    assert_eq!(
        l.get(&other, &r.context_operation).await.unwrap_err(),
        Error::NotFound
    );
}
pub async fn cas_fences<H: LedgerHarness>() {
    let h = H::open().await;
    let l = h.ledger();
    let mut r = l.insert(intent()).await.unwrap();
    r.cancel_requested = true;
    let saved = l.cas(0, r.clone()).await.unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(l.cas(0, r).await.unwrap_err(), Error::Conflict);
    let other = h.reopen().await;
    let mut current = other
        .get(&saved.owner, &saved.context_operation)
        .await
        .unwrap();
    assert!(current.cancel_requested);
    current.cancel_requested = false;
    assert_eq!(other.cas(1, current).await.unwrap_err(), Error::Conflict);
    let mut changed = saved.clone();
    changed.request.setup = true;
    assert_eq!(l.cas(1, changed).await.unwrap_err(), Error::Conflict);
    let mut changed = saved.clone();
    changed.resources.remove(&Resource::Volume);
    assert_eq!(l.cas(1, changed).await.unwrap_err(), Error::Conflict);
    let mut changed = saved;
    changed
        .resources
        .get_mut(&Resource::Volume)
        .unwrap()
        .condition = ReleaseCondition::StopConfirmed;
    assert_eq!(l.cas(1, changed).await.unwrap_err(), Error::Conflict);
}
pub async fn resource_receipts<H: LedgerHarness>() {
    let h = H::open().await;
    let l = h.ledger();
    let c = context();
    let mut r = l.insert(intent()).await.unwrap();
    let mut o = Observation {
        execution: ExecutionRef {
            owner: c.claim.run,
            id: c.operation,
            runtime_version: "test".into(),
        },
        state: ExecutionState::Destroyed,
        result: None,
        failure: None,
        cost_micro_usd: None,
        released_resources: [
            Resource::Cpu,
            Resource::Memory,
            Resource::Pid,
            Resource::Disk,
        ]
        .into_iter()
        .collect(),
    };
    r.observe(o.clone());
    r = l.cas(r.revision, r).await.unwrap();
    assert!(r.resources[&Resource::Disk].released);
    assert!(!r.resources[&Resource::Volume].released);
    assert!(!r.resources[&Resource::ExternalCost].released);
    let mut rollback = r.clone();
    rollback
        .resources
        .get_mut(&Resource::Disk)
        .unwrap()
        .released = false;
    assert_eq!(
        l.cas(r.revision, rollback).await.unwrap_err(),
        Error::Conflict
    );
    assert_eq!(h.reopen().await.pending().await.unwrap(), vec![r.clone()]);
    o.released_resources.insert(Resource::Volume);
    r.observe(o.clone());
    r = l.cas(r.revision, r).await.unwrap();
    assert!(!r.reclaimed());
    o.cost_micro_usd = Some(7);
    r.observe(o);
    r = l.cas(r.revision, r).await.unwrap();
    assert!(r.reclaimed());
    assert!(h.reopen().await.pending().await.unwrap().is_empty());
}
pub async fn worker_restart<H: LedgerHarness>() {
    let h = H::open().await;
    let c = context();
    let b = FakeExecutionBackend::default();
    let mut r = h.ledger().insert(intent()).await.unwrap();
    let e = b.acquire(&c, &r.request).await.unwrap();
    b.start(&c, &e).await.unwrap();
    b.fail_cleanup.store(true, Ordering::SeqCst);
    assert_eq!(b.destroy(&c).await.unwrap_err(), Error::Storage);
    r.observe(b.lookup(&c).await.unwrap().unwrap());
    r = h.ledger().cas(0, r).await.unwrap();
    let registry = b.registry.clone();
    drop(b);
    let ledger = h.reopen().await;
    drop(h);
    let backend = FakeExecutionBackend {
        registry,
        ..Default::default()
    };
    assert_eq!(ledger.pending().await.unwrap(), vec![r.clone()]);
    assert!(r.resources[&Resource::Cpu].released);
    assert!(!r.resources[&Resource::Disk].released);
    let mut foreign = c.clone();
    foreign.claim.run.id = "another".into();
    assert!(backend.lookup(&foreign).await.unwrap().is_none());
    assert_eq!(
        backend.start(&foreign, &e).await.unwrap_err(),
        Error::NotFound
    );
    for _ in 0..2 {
        assert_eq!(backend.destroy(&c).await.unwrap(), Progress::Confirmed);
        r.observe(backend.lookup(&c).await.unwrap().unwrap());
        r = ledger.cas(r.revision, r).await.unwrap();
    }
    assert!(r.reclaimed());
    assert!(ledger.pending().await.unwrap().is_empty());
}
pub async fn lost_registry<H: LedgerHarness>() {
    let h = H::open().await;
    let c = context();
    let b = FakeExecutionBackend::default();
    let r = h.ledger().insert(intent()).await.unwrap();
    b.acquire(&c, &r.request).await.unwrap();
    drop(b);
    let ledger = h.reopen().await;
    drop(h);
    let backend = FakeExecutionBackend::default();
    assert!(backend.lookup(&c).await.unwrap().is_none());
    assert_eq!(backend.stop(&c).await.unwrap(), Progress::Unknown);
    assert_eq!(backend.destroy(&c).await.unwrap(), Progress::Unknown);
    assert_eq!(ledger.pending().await.unwrap(), vec![r]);
}

#[macro_export]
macro_rules! execution_ledger_contract_tests {
    ($h:ty) => {
        #[test]
        fn execution_identity() {
            block_on($crate::execution_contracts::identity::<$h>());
        }
        #[test]
        fn execution_cas_fences() {
            block_on($crate::execution_contracts::cas_fences::<$h>());
        }
        #[test]
        fn execution_resource_receipts() {
            block_on($crate::execution_contracts::resource_receipts::<$h>());
        }
        #[test]
        fn execution_worker_restart() {
            block_on($crate::execution_contracts::worker_restart::<$h>());
        }
        #[test]
        fn execution_lost_registry() {
            block_on($crate::execution_contracts::lost_registry::<$h>());
        }
        #[test]
        fn execution_dispatch_restart() {
            block_on($crate::execution_contracts::dispatch_restart::<$h>());
        }
    };
}

/// Exercise dispatch settlement as well as inventory recovery using either ledger.
pub async fn dispatch_restart<H: LedgerHarness>() {
    use crate::{FakeModel, runtime};
    use std::sync::Arc;
    let h = H::open().await;
    let store = Arc::new(runtrell::memory::MemoryStore::default());
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new("t", "restart", account.clone(), "test"))
        .await
        .unwrap();
    let claim = store.claim(&run, "worker", 0, 10000).await.unwrap();
    let rt = runtime(store.clone(), FakeModel::default(), vec![]);
    let b = FakeExecutionBackend::default();
    b.fail_cleanup.store(true, Ordering::SeqCst);
    let request = ExecutionRequest::process("test-only", vec![]);
    assert_eq!(
        rt.execute_backend(
            &claim,
            "code",
            request.clone(),
            Amount::new(0, 100),
            h.ledger(),
            &b
        )
        .await
        .unwrap(),
        ExternalResult::Unknown
    );
    assert_eq!(
        store.account(&account).await.unwrap().reserved.micro_usd,
        100
    );
    let before = h.ledger().pending().await.unwrap();
    assert_eq!(before.len(), 1);
    let registry = b.registry.clone();
    drop(b);
    drop(rt);
    let ledger = h.reopen().await;
    drop(h);
    assert_eq!(ledger.pending().await.unwrap(), before);
    let backend = FakeExecutionBackend {
        registry,
        ..Default::default()
    };
    let rt = runtime(store.clone(), FakeModel::default(), vec![]);
    for _ in 0..2 {
        assert!(matches!(
            rt.execute_backend(
                &claim,
                "code",
                request.clone(),
                Amount::new(0, 100),
                &ledger,
                &backend
            )
            .await
            .unwrap(),
            ExternalResult::Applied { .. }
        ));
    }
    assert!(ledger.pending().await.unwrap().is_empty());
    assert_eq!(store.slot_count(OperationKind::Sandbox).await, 0);
    assert_eq!(store.account(&account).await.unwrap().reserved.micro_usd, 0);
    assert_eq!(store.list_operations(&run).await.unwrap().len(), 1);
}
