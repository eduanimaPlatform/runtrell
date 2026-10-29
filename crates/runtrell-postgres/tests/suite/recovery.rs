// SPDX-License-Identifier: Apache-2.0
// Shared contract fixture from runtrell-2a testkit (2d31acf).
use super::{MemoryStore, block_on};
use runtrell::*;
use runtrell_testkit::*;
use serde_json::json;
use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};
struct RestartModel {
    case: &'static str,
    resumed: AtomicBool,
    requests: Mutex<Vec<ModelRequest>>,
    sends: AtomicU64,
    lookups: AtomicU64,
    store: Arc<MemoryStore>,
    clock: TestClock,
    successor: Mutex<Option<Claim>>,
}
impl ModelPort for RestartModel {
    fn limits(&self) -> ModelLimits {
        FakeModel::default().limits()
    }
    fn input_upper_bound(&self, r: &PreparedModelRequest) -> Result<u64> {
        FakeModel::default().input_upper_bound(r)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        if self.case == "no_policy" {
            None
        } else {
            Some((
                if self.case == "caller_changed" && self.resumed.load(Ordering::SeqCst) {
                    "other"
                } else {
                    "caller"
                }
                .into(),
                if self.case == "expired" { 0 } else { 1000 },
            ))
        }
    }
    async fn invoke(&self, _: &ToolContext, r: PreparedModelRequest) -> Result<ExternalResult> {
        self.requests.lock().unwrap().push(r.request().clone());
        if !self.resumed.load(Ordering::SeqCst) {
            return std::future::pending().await;
        }
        self.sends.fetch_add(1, Ordering::SeqCst);
        let mut usage = Usage::measured(
            if self.case == "failed" { 9 } else { 3 },
            1,
            if self.case == "failed" { 10 } else { 4 },
        );
        usage.model = "fake".into();
        usage.cached = Some(0);
        if self.case == "failed" {
            Ok(ExternalResult::Failed {
 repair_hint: None,
 physical_evidence: None,
                code: "PROVIDER_REJECTED".into(),
                usage,
                retry_after: None,
            })
        } else {
            Ok(ExternalResult::Applied {
 physical_evidence: None,
                output: serde_json::to_value(FakeModel::decision(json!({"decision":"answered"})))
                    .unwrap(),
                usage,
                external_id: None,
            })
        }
    }
    async fn lookup(&self, ctx: &ToolContext, _: PreparedModelRequest) -> Result<Lookup> {
        let n = self.lookups.fetch_add(1, Ordering::SeqCst);
        match self.case {
            "unknown" => Ok(Lookup::Unknown),
            "transport" => Err(Error::UsageUnknown),
            "in_progress" => Ok(Lookup::Pending),
            "handoff" if n == 0 => {
                self.clock.set(202);
                let next = self
                    .store
                    .claim(&ctx.claim.run, "successor", 202, 100)
                    .await?;
                *self.successor.lock().unwrap() = Some(next);
                Ok(Lookup::NotFound {
                    scope: "caller".into(),
                })
            }
            _ => Ok(Lookup::NotFound {
                scope: self.recovery().map_or("unknown".into(), |r| r.0),
            }),
        }
    }
}
async fn restart_case(case: &'static str) {
    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let h = store
        .start(RunSpec::new("t", "request", account.clone(), "judge"))
        .await
        .unwrap();
    let first = store.claim(&h, "crashed", 0, 100).await.unwrap();
    let clock = TestClock::default();
    let rt = Runtime {
        store: store.clone(),
        model: RestartModel {
            case,
            resumed: AtomicBool::new(false),
            requests: Mutex::new(vec![]),
            sends: AtomicU64::new(0),
            lookups: AtomicU64::new(0),
            store: store.clone(),
            clock: clock.clone(),
            successor: Mutex::new(None),
        },
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock: clock.clone(),
        validator: std::sync::Arc::new(DecisionValidator::default()),
        finalizer: std::sync::Arc::new(DecisionFinalizer),
    };
    let mut future = Box::pin(rt.drive(&first));
    let w = Waker::noop();
    let mut cx = Context::from_waker(w);
    // DB barriers can yield before model I/O; stop at the same external-I/O boundary.
    loop {
        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
        if !rt.model.requests.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    drop(future);
    let op = store.operation(&h, "model:1").await.unwrap().unwrap();
    assert!(store.get(&h).await.unwrap().checkpoint.is_some());
    assert_eq!(rt.model.requests.lock().unwrap().len(), 1);
    assert_eq!(
        store.claim(&h, "competing", 0, 100).await,
        Err(Error::OwnershipLost)
    );
    clock.set(101);
    rt.model.resumed.store(true, Ordering::SeqCst);
    let next = store.claim(&h, "restart", 101, 100).await.unwrap();
    let result = if case == "handoff" {
        assert_eq!(rt.drive(&next).await, Err(Error::OwnershipLost));
        assert_eq!(rt.model.sends.load(Ordering::SeqCst), 0);
        let successor = rt.model.successor.lock().unwrap().clone().unwrap();
        rt.drive(&successor).await.unwrap()
    } else {
        rt.drive(&next).await.unwrap()
    };
    let resumes = matches!(case, "resume" | "finalize" | "failed" | "handoff");
    assert_eq!(
        rt.model.sends.load(Ordering::SeqCst),
        u64::from(resumes),
        "{case}"
    );
    if resumes && case != "failed" {
        assert_eq!(result.state, RunState::Terminal(Terminal::Succeeded));
        assert!(result.completion.is_some());
    } else if case == "in_progress" {
        assert!(matches!(result.state, RunState::Waiting(_)));
    } else if case == "failed" {
        assert!(
            matches!(result.state,RunState::Terminal(Terminal::Failed(ref code)) if code.contains("PROVIDER_REJECTED"))
        );
    } else {
        assert!(
            matches!(
                result.state,
                RunState::Terminal(Terminal::OutcomeUnknown(_))
            ),
            "{case}: {:?}",
            result.state
        );
    }
    let after = store.operation(&h, "model:1").await.unwrap().unwrap();
    assert_eq!(op.id, after.id);
    assert_eq!(op.request, after.request);
    assert_eq!(after.request.attempt, 1);
    let balance = store.account(&account).await.unwrap();
    assert_eq!(
        balance.used.tokens,
        if case == "failed" {
            10
        } else if resumes {
            4
        } else {
            0
        }
    );
    assert_eq!(
        balance.reserved,
        if resumes {
            Amount::default()
        } else {
            op.request.reserve
        }
    );
    if resumes {
        let requests = rt.model.requests.lock().unwrap();
        assert_eq!(requests[0], requests[1], "exact request/output cap");
    }
    assert_eq!(store.operations(&h).await.unwrap().len(), 1);
    if resumes {
        let owner = store.get(&h).await.unwrap().owner.unwrap();
        rt.drive(&owner).await.unwrap();
        assert_eq!(store.account(&account).await.unwrap(), balance);
        assert_eq!(rt.model.sends.load(Ordering::SeqCst), 1);
    }
}
macro_rules! recovery {
    ($name:ident,$case:literal) => {
        #[test]
        fn $name() {
            block_on(restart_case($case));
        }
    };
}
recovery!(restart_resume_reuses_request_and_reservation, "resume");
recovery!(restart_finalizes_once, "finalize");
recovery!(restart_charged_failure_settles_once, "failed");
recovery!(restart_unknown_keeps_reservation, "unknown");
recovery!(restart_pending_is_machine_wait, "in_progress");
recovery!(restart_transport_failure_is_unknown, "transport");
recovery!(restart_expired_proof_cannot_resend, "expired");
recovery!(restart_changed_caller_cannot_resend, "caller_changed");
recovery!(restart_without_policy_cannot_resend, "no_policy");
recovery!(restart_lookup_then_old_generation_cannot_resend, "handoff");
