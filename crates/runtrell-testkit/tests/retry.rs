use runtrell::{memory::MemoryStore, *};
use runtrell_testkit::*;
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
struct RetryModel {
    calls: AtomicU64,
    success: bool,
    failure_usage: FailureUsage,
    failure_delay: u64,
    deadlines: Mutex<Vec<Option<u64>>>,
}
#[derive(Clone, Copy)]
enum FailureUsage {
    Known,
    Partial,
    Absent,
}
impl ModelPort for RetryModel {
    fn limits(&self) -> ModelLimits {
        FakeModel::default().limits()
    }
    fn input_upper_bound(&self, r: &PreparedModelRequest) -> Result<u64> {
        FakeModel::default().input_upper_bound(r)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        Some(("retry".into(), 100))
    }
    async fn lookup(&self, _: &ToolContext, _: PreparedModelRequest) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
    async fn invoke(
        &self,
        context: &ToolContext,
        _: PreparedModelRequest,
    ) -> Result<ExternalResult> {
        self.deadlines.lock().unwrap().push(context.deadline);
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let mut usage = Usage::measured(3, 1, 4);
        usage.model = "fake".into();
        usage.cached = Some(0);
        if n == 0 || !self.success {
            match self.failure_usage {
                FailureUsage::Known => {}
                FailureUsage::Partial => usage.micro_usd = None,
                FailureUsage::Absent => {
                    usage.input = None;
                    usage.output = None;
                    usage.cached = None;
                    usage.micro_usd = None;
                }
            }
            Ok(ExternalResult::Failed {
                repair_hint: None,
                physical_evidence: None,
                code: "TRANSIENT".into(),
                usage,
                retry_after: Some(self.failure_delay),
                provider_diagnostic: None,
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
}

struct PreparationDeadlineModel(TestClock);
impl ModelPort for PreparationDeadlineModel {
    fn limits(&self) -> ModelLimits {
        FakeModel::default().limits()
    }
    fn input_upper_bound(&self, request: &PreparedModelRequest) -> Result<u64> {
        FakeModel::default().input_upper_bound(request)
    }
    fn requires_recovery_preparation(&self) -> bool {
        true
    }
    async fn prepare_recovery(
        &self,
        _: &PreparationContext,
        _: &serde_json::Value,
    ) -> Result<Option<RecoveryProof>> {
        self.0.set(60_000);
        Ok(Some(RecoveryProof {
            bytes: vec![1],
            generation: "prepared".into(),
            expires_at: 70_000,
        }))
    }
    async fn invoke(&self, _: &ToolContext, _: PreparedModelRequest) -> Result<ExternalResult> {
        panic!("expired retry must not invoke provider")
    }
    async fn lookup(&self, _: &ToolContext, _: PreparedModelRequest) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}

#[test]
fn retry_expiring_during_recovery_preparation_never_reserves() {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        let account = store
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new(
                "t",
                "prepare-expiry",
                account.clone(),
                "answer",
            ))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100_000).await.unwrap();
        let request = DispatchRequest::new(
            "model",
            OperationKind::Model,
            "fake",
            json!({}),
            Amount::new(10, 10),
        );
        let prior = store.begin(&claim, request.clone(), 0).await.unwrap();
        store.mark_sent(&claim, &prior.id, 0).await.unwrap();
        store
            .record(
                &claim,
                &prior.id,
                ExternalResult::Failed {
                    repair_hint: None,
                    physical_evidence: None,
                    code: "TRANSIENT".into(),
                    usage: Usage::measured(1, 1, 2),
                    retry_after: Some(1),
                    provider_diagnostic: None,
                },
                0,
            )
            .await
            .unwrap();
        let clock = TestClock::default();
        clock.set(59_000);
        let rt = Runtime {
            store: store.clone(),
            model: PreparationDeadlineModel(clock.clone()),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let mut retry = request;
        retry.attempt = 2;
        assert!(
            matches!(rt.dispatch(&claim, retry, "model", |_| async { panic!("send") }, |_| async { panic!("lookup") }).await, Err(Error::Invalid(reason)) if reason.contains("RETRY_EXHAUSTED"))
        );
        assert_eq!(store.operations(&run).await.unwrap().len(), 1);
        assert_eq!(
            store.account(&account).await.unwrap().reserved,
            Amount::default()
        );
    });
}
#[test]
fn charged_retry_is_settled_before_next_attempt_and_uses_shared_budget() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let a = s
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let h = s
            .start(RunSpec::new("t", "retry", a.clone(), "answer"))
            .await
            .unwrap();
        let c = s.claim(&h, "worker", 0, 100_000).await.unwrap();
        let clock = TestClock::default();
        let policy = FakePolicy::default();
        let rt = Runtime {
            store: s.clone(),
            model: RetryModel {
                calls: AtomicU64::new(0),
                success: true,
                failure_usage: FailureUsage::Known,
                failure_delay: 1_000,
                deadlines: Mutex::default(),
            },
            tools: Vec::<BusinessTool>::new(),
            policy,
            clock: clock.clone(),
            validator: std::sync::Arc::new(DecisionValidator::default()),
            finalizer: std::sync::Arc::new(DecisionFinalizer),
        };
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(s.account(&a).await.unwrap().used.tokens, 4);
        assert_eq!(s.account(&a).await.unwrap().reserved.tokens, 0);
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(rt.model.calls.load(Ordering::SeqCst), 1);
        // retry_after is 1_000 ms: still backing off one millisecond before it.
        clock.set(999);
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(rt.model.calls.load(Ordering::SeqCst), 1);
        clock.set(1_000);
        assert_eq!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(s.account(&a).await.unwrap().used.tokens, 8);
        assert_eq!(s.operations(&h).await.unwrap().len(), 2);
    });
}
#[test]
fn retry_bound_and_authorization_are_rechecked_after_restart() {
    block_on(async {
        for revoke in [false, true] {
            let s = Arc::new(MemoryStore::default());
            let a = s
                .create_account("t", Amount::new(1000, 1000), None)
                .await
                .unwrap();
            let h = s
                .start(RunSpec::new("t", "retry", a, "answer"))
                .await
                .unwrap();
            let c = s.claim(&h, "worker", 0, 100_000).await.unwrap();
            let clock = TestClock::default();
            let policy = FakePolicy::default();
            let rt = Runtime {
                store: s.clone(),
                model: RetryModel {
                    calls: AtomicU64::new(0),
                    success: false,
                    failure_usage: FailureUsage::Known,
                    failure_delay: 1_000,
                    deadlines: Mutex::default(),
                },
                tools: Vec::<BusinessTool>::new(),
                policy: policy.clone(),
                clock: clock.clone(),
                validator: std::sync::Arc::new(DecisionValidator::default()),
                finalizer: std::sync::Arc::new(DecisionFinalizer),
            };
            rt.drive(&c).await.unwrap();
            if revoke {
                policy.revoked.store(true, Ordering::SeqCst);
            }
            for time in 1..=3 {
                clock.set(time * 1_000);
                rt.drive(&c).await.unwrap();
            }
            assert_eq!(
                rt.model.calls.load(Ordering::SeqCst),
                if revoke { 1 } else { 3 }
            );
            assert!(matches!(
                s.get(&h).await.unwrap().state,
                RunState::Terminal(Terminal::Failed(_))
            ));
        }
    });
}

#[test]
fn retryable_failed_unknown_usage_keeps_reservation_then_recovers_original_attempt() {
    block_on(async {
        for (index, mode) in [FailureUsage::Partial, FailureUsage::Absent]
            .into_iter()
            .enumerate()
        {
            let store = Arc::new(MemoryStore::default());
            let account = store
                .create_account_with_policy(
                    "t",
                    Amount::new(100, 100),
                    None,
                    AccountPolicy {
                        continue_with_unknown: true,
                        ..AccountPolicy::default()
                    },
                )
                .await
                .unwrap();
            let run = store
                .start(RunSpec::new(
                    "t",
                    &format!("unknown-{index}"),
                    account.clone(),
                    "answer",
                ))
                .await
                .unwrap();
            let claim = store.claim(&run, "worker", 0, 100_000).await.unwrap();
            let clock = TestClock::default();
            let rt = Runtime {
                store: store.clone(),
                model: RetryModel {
                    calls: AtomicU64::new(0),
                    success: true,
                    failure_usage: mode,
                    failure_delay: 1_000,
                    deadlines: Mutex::default(),
                },
                tools: Vec::<BusinessTool>::new(),
                policy: FakePolicy::default(),
                clock: clock.clone(),
                validator: Arc::new(DecisionValidator::default()),
                finalizer: Arc::new(DecisionFinalizer),
            };
            assert!(matches!(
                rt.drive(&claim).await.unwrap().state,
                RunState::Waiting(_)
            ));
            let first = store.operations(&run).await.unwrap().remove(0);
            assert!(matches!(first.result, Some(ExternalResult::Failed { .. })));
            assert!(first.unknown_usage);
            let before = store.account(&account).await.unwrap();
            assert_eq!(before.unknown, 1);
            assert!(before.reserved.tokens > 0 && before.reserved.micro_usd > 0);
            clock.set(1_000);
            assert_eq!(
                rt.drive(&claim).await.unwrap().state,
                RunState::Terminal(Terminal::Succeeded)
            );
            assert_eq!(rt.model.calls.load(Ordering::SeqCst), 2);
            let ops = store.operations(&run).await.unwrap();
            assert_eq!(ops.len(), 2);
            assert_eq!(ops[0].id, first.id);
            assert!(ops[0].unknown_usage);
            assert_eq!(store.account(&account).await.unwrap().unknown, 1);

            let mut recovered = match first.result.unwrap() {
                ExternalResult::Failed { usage, .. } => usage,
                _ => panic!("failed evidence"),
            };
            if matches!(mode, FailureUsage::Absent) {
                recovered.input = Some(0);
                recovered.output = Some(0);
                recovered.cached = Some(0);
            }
            let recovery = SettlementRecovery {
                basis: "fake-provider-usage-receipt".into(),
                usage: recovered,
                price: first.request.price.clone().unwrap(),
            };
            let command = owner_contracts::command(&format!("recover-{index}"));
            let receipt = store
                .settle_recovered(&run, &first.id, recovery.clone(), command.clone(), 1_001)
                .await
                .unwrap();
            assert_eq!(
                store
                    .settle_recovered(&run, &first.id, recovery, command, 1_002)
                    .await
                    .unwrap(),
                receipt
            );
            assert_eq!(store.account(&account).await.unwrap().unknown, 0);
            assert_eq!(
                store.get(&run).await.unwrap().state,
                RunState::Terminal(Terminal::Succeeded)
            );
            assert!(matches!(
                store.operations(&run).await.unwrap()[0].result,
                Some(ExternalResult::Failed { .. })
            ));
        }
    });
}

#[test]
fn retry_after_beyond_total_elapsed_never_sends_again() {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        let account = store
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new("t", "long-delay", account, "answer"))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100_000).await.unwrap();
        let rt = Runtime {
            store: store.clone(),
            model: RetryModel {
                calls: AtomicU64::new(0),
                success: true,
                failure_usage: FailureUsage::Known,
                failure_delay: 90_000,
                deadlines: Mutex::default(),
            },
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        assert!(matches!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert!(matches!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Terminal(Terminal::Failed(_))
        ));
        assert_eq!(rt.model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.operations(&run).await.unwrap().len(), 1);
    });
}

#[test]
fn late_retry_is_bounded_by_original_total_elapsed_deadline() {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        let account = store
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new("t", "late-retry", account, "answer"))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100_000).await.unwrap();
        let clock = TestClock::default();
        let rt = Runtime {
            store: store.clone(),
            model: RetryModel {
                calls: AtomicU64::new(0),
                success: true,
                failure_usage: FailureUsage::Known,
                failure_delay: 1_000,
                deadlines: Mutex::default(),
            },
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: clock.clone(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        assert!(matches!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Waiting(_)
        ));
        clock.set(59_000);
        assert_eq!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(
            *rt.model.deadlines.lock().unwrap(),
            vec![Some(30_000), Some(60_000)]
        );
        let operations = store.operations(&run).await.unwrap();
        assert_eq!(operations[1].request.timeout, Some(1_000));
    });
}

#[test]
fn unknown_reservation_counts_against_retry_budget() {
    block_on(async {
        let store = Arc::new(MemoryStore::default());
        let parent = store
            .create_account_with_policy(
                "t",
                Amount::new(25, 25),
                None,
                AccountPolicy {
                    continue_with_unknown: true,
                    ..AccountPolicy::default()
                },
            )
            .await
            .unwrap();
        let account = store
            .create_account_with_policy(
                "t",
                Amount::new(100, 100),
                Some(parent.clone()),
                AccountPolicy {
                    continue_with_unknown: true,
                    ..AccountPolicy::default()
                },
            )
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new(
                "t",
                "budget-boundary",
                account.clone(),
                "answer",
            ))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100_000).await.unwrap();
        let clock = TestClock::default();
        let rt = Runtime {
            store: store.clone(),
            model: RetryModel {
                calls: AtomicU64::new(0),
                success: true,
                failure_usage: FailureUsage::Absent,
                failure_delay: 1_000,
                deadlines: Mutex::default(),
            },
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: clock.clone(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        assert!(matches!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Waiting(_)
        ));
        let first = store.account(&account).await.unwrap();
        assert_eq!(first.unknown, 1);
        assert!(first.reserved.tokens > 0 && first.reserved.micro_usd > 0);
        let ancestor = store.account(&parent).await.unwrap();
        assert_eq!(ancestor.unknown, 1);
        assert_eq!(ancestor.reserved, first.reserved);
        clock.set(1_000);
        assert!(matches!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Terminal(Terminal::BudgetReached(_))
        ));
        assert_eq!(rt.model.calls.load(Ordering::SeqCst), 1);
        assert!(store.operations(&run).await.unwrap()[0].unknown_usage);
    });
}
