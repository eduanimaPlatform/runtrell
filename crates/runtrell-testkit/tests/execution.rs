// SPDX-License-Identifier: Apache-2.0
use runtrell::{execution::*, memory::MemoryStore, *};
use runtrell_testkit::*;
use serde_json::json;
use std::sync::{Arc, atomic::Ordering};
async fn setup() -> (Arc<MemoryStore>, Claim, String) {
    let s = Arc::new(MemoryStore::default());
    let a = s
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("t", "u08", a.clone(), "reproduce"))
        .await
        .unwrap();
    let c = s.claim(&h, "w", 0, 10000).await.unwrap();
    (s, c, a)
}
fn request() -> ExecutionRequest {
    ExecutionRequest::process("test-only", vec![])
}
#[test]
fn a04_never_weakens_required_isolation_or_hard_memory() {
    let backend = FakeExecutionBackend::default();
    let mut r = request();
    assert!(backend.supports(&r).is_ok());
    r.limits.isolation = Isolation::Untrusted;
    assert!(backend.supports(&r).is_err());
    r.limits.isolation = Isolation::Trusted;
    r.limits.ram_bytes = Some(1024);
    assert!(backend.supports(&r).is_err());
}
#[test]
fn u08_reserve_only_on_code_demand_and_keep_unknown_cost() {
    block_on(async {
        let (s, c, a) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let ledger = MemoryExecutionLedger::default();
        let b = FakeExecutionBackend::default();
        assert!(ledger.pending().await.unwrap().is_empty());
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
        b.unknown_cost.store(true, Ordering::SeqCst);
        let result = rt
            .execute_backend(&c, "code", request(), Amount::new(0, 100), &ledger, &b)
            .await
            .unwrap();
        assert!(matches!(result, ExternalResult::Accepted { .. }));
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 1);
        let r = ledger.pending().await.unwrap().pop().unwrap();
        assert!(r.resources[&Resource::Disk].released);
        assert!(!r.resources[&Resource::ExternalCost].released);
        assert_eq!(s.account(&a).await.unwrap().reserved.micro_usd, 100);
        b.unknown_cost.store(false, Ordering::SeqCst);
        assert!(matches!(
            rt.execute_backend(&c, "code", request(), Amount::new(0, 100), &ledger, &b)
                .await
                .unwrap(),
            ExternalResult::Applied { .. }
        ));
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
        assert!(ledger.pending().await.unwrap().is_empty());
    })
}
#[test]
fn a08_cancel_during_acquisition_and_late_environment_is_never_started() {
    block_on(async {
        let (s, c, _) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let ledger = MemoryExecutionLedger::default();
        let b = FakeExecutionBackend::default();
        b.delay.store(true, Ordering::SeqCst);
        let mut running = std::pin::pin!(rt.execute_backend(
            &c,
            "code",
            request(),
            Amount::new(0, 100),
            &ledger,
            &b
        ));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(running.as_mut().poll(&mut cx).is_pending());
        rt.cancel_backend(&c, "code", &ledger, &b).await.unwrap();
        b.delay.store(false, Ordering::SeqCst);
        let _ = running.await;
        let op = s.operation(&c.run, "code").await.unwrap().unwrap();
        let r = ledger.get(&c.run, &op.id).await.unwrap();
        assert!(r.cancel_requested);
        assert!(r.reclaimed());
        assert!(r.observation.unwrap().result.is_none());
    })
}
#[test]
fn a14_cleanup_worker_restart_and_duplicate_reconciliation() {
    block_on(async {
        let (s, c, _) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let ledger = MemoryExecutionLedger::default();
        let b = FakeExecutionBackend::default();
        b.fail_cleanup.store(true, Ordering::SeqCst);
        assert_eq!(
            rt.execute_backend(&c, "code", request(), Amount::new(0, 100), &ledger, &b)
                .await
                .unwrap(),
            ExternalResult::Unknown
        );
        let before = ledger.pending().await.unwrap();
        assert!(!before[0].resources[&Resource::Disk].released);
        assert!(before[0].resources[&Resource::Cpu].released);
        drop(rt);
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        for _ in 0..2 {
            assert!(matches!(
                rt.execute_backend(&c, "code", request(), Amount::new(0, 100), &ledger, &b)
                    .await
                    .unwrap(),
                ExternalResult::Applied { .. }
            ));
        }
        assert!(ledger.pending().await.unwrap().is_empty());
    })
}
#[test]
fn lost_acquisition_response_is_found_and_cancelled_without_second_execution() {
    block_on(async {
        let (s, c, _) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let ledger = MemoryExecutionLedger::default();
        let b = FakeExecutionBackend::default();
        b.lose.store(true, Ordering::SeqCst);
        assert_eq!(
            rt.execute_backend(&c, "code", request(), Amount::new(0, 100), &ledger, &b)
                .await
                .unwrap(),
            ExternalResult::Unknown
        );
        rt.cancel_backend(&c, "code", &ledger, &b).await.unwrap();
        let op = s.operation(&c.run, "code").await.unwrap().unwrap();
        assert!(
            ledger
                .get(&c.run, &op.id)
                .await
                .unwrap()
                .observation
                .unwrap()
                .result
                .is_none()
        );
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
    })
}
#[test]
fn a22_stale_workspace_copy_and_other_run_are_rejected() {
    block_on(async {
        use runtrell::data::*;
        let (s, c, _) = setup().await;
        let ns = Namespace {
            owner: c.run.clone(),
            agent: "main".into(),
        };
        let a = Access::owner(ns);
        let w = DataStore::memory(StoragePolicy::default());
        let file = w.write(&a, "input", None, b"old").await.unwrap();
        let mut req = request();
        req.input = Some(w.snapshot(&a).await.unwrap());
        req.input_files.insert("input".into(), b"old".to_vec());
        let ctx = ToolContext {
            claim: c.clone(),
            principal: "host".into(),
            operation: "copy".into(),
            attempt: 1,
            deadline: None,
            permission: "execute".into(),
            account_binding: "host".into(),
        };
        let b = FakeExecutionBackend::default();
        let e = b.acquire(&ctx, &req).await.unwrap();
        b.start(&ctx, &e).await.unwrap();
        let mut r = ExecutionRecord::intent(&ctx, req);
        r.observe(b.lookup(&ctx).await.unwrap().unwrap());
        w.write(&a, "input", Some(file.revision), b"new")
            .await
            .unwrap();
        assert_eq!(
            merge_copy(
                &w,
                &a,
                &r,
                vec![Change::Write {
                    path: "input".into(),
                    expected: Some(file.revision),
                    bytes: b"stale".to_vec()
                }]
            )
            .await
            .unwrap_err(),
            Error::Conflict
        );
        let mut foreign = ctx.clone();
        foreign.claim.run.id = "other".into();
        assert_eq!(
            b.acquire(&foreign, &r.request).await.unwrap_err(),
            Error::PermissionDenied
        );
        assert!(s.operation(&c.run, "unused").await.unwrap().is_none());
    })
}
#[test]
fn a28_setup_requires_same_limits_and_refuses_tty_and_credentials() {
    let b = FakeExecutionBackend::default();
    let mut r = request();
    r.setup = true;
    r.interactive = true;
    assert!(b.supports(&r).is_err());
    r.interactive = false;
    r.credential_scopes.push("admin".into());
    assert!(b.supports(&r).is_err());
    r.credential_scopes.clear();
    r.limits.network_allow.push("example.test".into());
    assert!(b.supports(&r).is_err());
    assert_eq!(json!({"setup":r.setup})["setup"], true);
}

#[test]
fn lost_acquisition_response_resumes_original_environment() {
    block_on(async {
        let (s, c, _) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let ledger = MemoryExecutionLedger::default();
        let b = FakeExecutionBackend::default();
        b.lose.store(true, Ordering::SeqCst);
        assert_eq!(
            rt.execute_backend(&c, "code", request(), Amount::new(0, 100), &ledger, &b)
                .await
                .unwrap(),
            ExternalResult::Unknown
        );
        assert!(matches!(
            rt.execute_backend(&c, "code", request(), Amount::new(0, 100), &ledger, &b)
                .await
                .unwrap(),
            ExternalResult::Applied { .. }
        ));
        assert!(ledger.pending().await.unwrap().is_empty());
    })
}
#[test]
fn a18_cancel_is_observed_while_execution_is_running() {
    block_on(async {
        let (_, c, _) = setup().await;
        let ctx = ToolContext {
            claim: c,
            principal: "host".into(),
            operation: "busy".into(),
            attempt: 1,
            deadline: None,
            permission: "execute".into(),
            account_binding: "host".into(),
        };
        let b = FakeExecutionBackend::default();
        let req = request();
        b.hold_running.store(true, Ordering::SeqCst);
        let e = b.acquire(&ctx, &req).await.unwrap();
        let copy = b.clone();
        let cc = ctx.clone();
        let running =
            std::thread::spawn(move || block_on(async { copy.start(&cc, &e).await.unwrap() }));
        let wait = std::time::Instant::now();
        while b.lookup(&ctx).await.unwrap().unwrap().state == ExecutionState::Ready {
            assert!(wait.elapsed().as_secs() < 2);
            std::thread::yield_now();
        }
        b.cancel(&ctx).await.unwrap();
        running.join().unwrap();
        let result = b.lookup(&ctx).await.unwrap().unwrap();
        assert_eq!(result.state, ExecutionState::Stopped);
        assert!(result.failure.unwrap().contains("closed"));
        assert!(wait.elapsed().as_secs() < 2);
    })
}

#[test]
fn acquisition_timeout_preserves_reservation_and_late_environment() {
    block_on(async {
        let (s, c, a) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let ledger = MemoryExecutionLedger::default();
        let b = FakeExecutionBackend::default();
        b.delay.store(true, Ordering::SeqCst);
        let mut request = request();
        request.limits.acquire_ms = 5;
        let mut running = std::pin::pin!(rt.execute_backend(
            &c,
            "code",
            request.clone(),
            Amount::new(0, 100),
            &ledger,
            &b
        ));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(running.as_mut().poll(&mut cx).is_pending());
        rt.clock.set(5);
        assert_eq!(running.await.unwrap(), ExternalResult::Unknown);
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 1);
        assert_eq!(s.account(&a).await.unwrap().reserved.micro_usd, 100);
        b.delay.store(false, Ordering::SeqCst);
        assert!(matches!(
            rt.execute_backend(&c, "code", request, Amount::new(0, 100), &ledger, &b)
                .await
                .unwrap(),
            ExternalResult::Applied { .. }
        ));
    })
}

#[test]
fn a18_reclaimed_resources_and_unknown_cost_have_distinct_run_states() {
    block_on(async {
        let (s, c, a) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let ledger = MemoryExecutionLedger::default();
        let b = FakeExecutionBackend::default();
        b.unknown_cost.store(true, Ordering::SeqCst);
        rt.execute_backend(&c, "code", request(), Amount::new(0, 100), &ledger, &b)
            .await
            .unwrap();
        rt.cancel_backend(&c, "code", &ledger, &b).await.unwrap();
        let run = s.get(&c.run).await.unwrap();
        assert_eq!(run.stop, Progress::Confirmed);
        assert_eq!(run.cleanup, Progress::Confirmed);
        assert_ne!(run.settlement, Progress::Confirmed);
        assert_eq!(s.account(&a).await.unwrap().reserved.micro_usd, 100);
    })
}
#[test]
fn a08_environment_created_after_cancellation_is_discovered_and_destroyed() {
    block_on(async {
        let (s, c, _) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let ledger = MemoryExecutionLedger::default();
        let b = FakeExecutionBackend::default();
        b.create_late.store(true, Ordering::SeqCst);
        let mut running = std::pin::pin!(rt.execute_backend(
            &c,
            "code",
            request(),
            Amount::new(0, 100),
            &ledger,
            &b
        ));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(running.as_mut().poll(&mut cx).is_pending());
        rt.cancel_backend(&c, "code", &ledger, &b).await.unwrap();
        assert_eq!(s.get(&c.run).await.unwrap().stop, Progress::Unknown);
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 1);
        b.create_late.store(false, Ordering::SeqCst);
        let _ = running.await;
        rt.cancel_backend(&c, "code", &ledger, &b).await.unwrap();
        let op = s.operation(&c.run, "code").await.unwrap().unwrap();
        let record = ledger.get(&c.run, &op.id).await.unwrap();
        assert!(record.reclaimed());
        assert_eq!(
            record.observation.as_ref().unwrap().state,
            ExecutionState::Destroyed
        );
        assert!(record.observation.unwrap().result.is_none());
    })
}

#[test]
fn workspace_copy_is_fixed_and_collected_files_merge_with_same_base() {
    block_on(async {
        use runtrell::data::*;
        let (_, c, _) = setup().await;
        let a = Access::owner(Namespace {
            owner: c.run.clone(),
            agent: "main".into(),
        });
        let w = DataStore::memory(StoragePolicy::default());
        let old = w.write(&a, "input", None, b"before").await.unwrap();
        let mut req = request();
        freeze_workspace(&w, &a, &mut req, 2).await.unwrap();
        assert_eq!(req.input_files["input"], b"before");
        req.output_paths.push("generated".into());
        let ctx = ToolContext {
            claim: c,
            principal: "host".into(),
            operation: "files".into(),
            attempt: 1,
            deadline: None,
            permission: "execute".into(),
            account_binding: "host".into(),
        };
        let mut record = ExecutionRecord::intent(&ctx, req);
        record.observe(Observation {
            execution: ExecutionRef {
                owner: ctx.claim.run.clone(),
                id: ctx.operation.clone(),
                runtime_version: "test".into(),
            },
            state: ExecutionState::Destroyed,
            result: Some(json!({"files":{"input":null,"generated":b"after".to_vec()}})),
            failure: None,
            cost_micro_usd: Some(0),
            released_resources: [
                Resource::Cpu,
                Resource::Memory,
                Resource::Pid,
                Resource::Disk,
                Resource::Volume,
            ]
            .into_iter()
            .collect(),
        });
        let snapshot = merge_process_copy(&w, &a, &record).await.unwrap();
        assert!(w.read(&a, "input", old.revision, 0, 2).await.is_err());
        assert_eq!(
            w.read(&a, "generated", snapshot.revision, 0, 20)
                .await
                .unwrap()
                .bytes,
            b"after"
        );
        assert_eq!(
            merge_process_copy(&w, &a, &record).await.unwrap_err(),
            Error::Conflict
        );
    })
}
