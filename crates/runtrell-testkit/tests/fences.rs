use runtrell::{memory::MemoryStore, *};
use runtrell_testkit::*;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
async fn setup() -> (Arc<MemoryStore>, Claim, String) {
    let s = Arc::new(MemoryStore::default());
    let a = s
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("t", "request", a.clone(), "input"))
        .await
        .unwrap();
    let c = s.claim(&h, "worker", 0, 100).await.unwrap();
    (s, c, a)
}
fn request() -> DispatchRequest {
    let mut d = DispatchRequest::new(
        "effect",
        OperationKind::Tool,
        "action",
        json!({}),
        Amount::new(10, 10),
    );
    d.scope = Some("scope".into());
    d.recovery_until = Some(100);
    d
}
fn applied() -> ExternalResult {
    ExternalResult::Applied {
        output: json!({"ok":true}),
        usage: Usage::measured(1, 0, 1),
        external_id: None,
    }
}
#[test]
fn lookup_then_cancellation_prevents_resend_and_retains_reservation() {
    block_on(async {
        let (s, c, a) = setup().await;
        let op = s.begin(&c, request(), 0).await.unwrap();
        s.mark_sent(&c, &op.id, 0).await.unwrap();
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let sends = AtomicU64::new(0);
        let result = rt
            .dispatch(
                &c,
                request(),
                "action",
                |_| async {
                    sends.fetch_add(1, Ordering::SeqCst);
                    Ok(applied())
                },
                |_| async {
                    s.cancel(&c.run, "stop during lookup").await?;
                    Ok(Lookup::NotFound {
                        scope: "scope".into(),
                    })
                },
            )
            .await;
        assert_eq!(result, Err(Error::Closed));
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        assert_eq!(s.account(&a).await.unwrap().reserved, Amount::new(10, 10));
    });
}
#[test]
fn lookup_then_revocation_prevents_resend_and_cached_results_require_policy() {
    block_on(async {
        let (s, c, _) = setup().await;
        let op = s.begin(&c, request(), 0).await.unwrap();
        s.mark_sent(&c, &op.id, 0).await.unwrap();
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let sends = AtomicU64::new(0);
        let result = rt
            .dispatch(
                &c,
                request(),
                "action",
                |_| async {
                    sends.fetch_add(1, Ordering::SeqCst);
                    Ok(applied())
                },
                |_| async {
                    rt.policy.revoked.store(true, Ordering::SeqCst);
                    Ok(Lookup::NotFound {
                        scope: "scope".into(),
                    })
                },
            )
            .await;
        assert_eq!(result, Err(Error::PermissionDenied));
        assert_eq!(sends.load(Ordering::SeqCst), 0);
        s.record(&c, &op.id, applied(), 0).await.unwrap();
        assert_eq!(
            rt.dispatch(
                &c,
                request(),
                "action",
                |_| async { panic!("cached") },
                |_| async { panic!("cached") }
            )
            .await,
            Err(Error::PermissionDenied)
        );
    });
}
#[test]
fn intent_save_failure_is_before_any_external_io() {
    block_on(async {
        let (s, c, _) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        s.fail_next_write();
        let sends = AtomicU64::new(0);
        assert_eq!(
            rt.dispatch(
                &c,
                request(),
                "action",
                |_| async {
                    sends.fetch_add(1, Ordering::SeqCst);
                    Ok(applied())
                },
                |_| async { Ok(Lookup::Unknown) }
            )
            .await,
            Err(Error::Storage)
        );
        assert_eq!(sends.load(Ordering::SeqCst), 0);
    });
}
#[test]
fn stale_result_and_checkpoint_are_fenced_after_lease_renew_and_handoff() {
    block_on(async {
        let (s, c, _) = setup().await;
        let op = s.begin(&c, request(), 0).await.unwrap();
        let renewed = s.renew(&c, 90, 100).await.unwrap();
        assert_eq!(renewed.generation, c.generation);
        assert_eq!(
            s.claim(&c.run, "other", 101, 100).await,
            Err(Error::OwnershipLost)
        );
        let next = s.claim(&c.run, "other", 191, 100).await.unwrap();
        assert_eq!(
            s.record(&c, &op.id, applied(), 191).await,
            Err(Error::OwnershipLost)
        );
        let revision = s.get(&c.run).await.unwrap().revision;
        assert_eq!(
            s.checkpoint(
                &c,
                revision,
                Checkpoint {
                    schema: 1,
                    revision,
                    payload: Value::Null,
                    messages: vec![],
                    cursor: 0
                },
                191
            )
            .await,
            Err(Error::OwnershipLost)
        );
        s.record(&next, &op.id, applied(), 191).await.unwrap();
    });
}
#[test]
fn result_save_failure_after_external_effect_recovers_by_lookup() {
    block_on(async {
        struct ToolWithFault {
            business: BusinessTool,
            store: Arc<MemoryStore>,
            once: AtomicBool,
        }
        impl Tool for ToolWithFault {
            fn descriptor(&self) -> ToolDescriptor {
                self.business.descriptor()
            }
            async fn invoke(&self, c: &ToolContext, input: Value) -> Result<ExternalResult> {
                let result = self.business.invoke(c, input).await?;
                if self.once.swap(false, Ordering::SeqCst) {
                    self.store.fail_next_write();
                }
                Ok(result)
            }
            async fn lookup(&self, c: &ToolContext, input: Value) -> Result<Lookup> {
                self.business.lookup(c, input).await
            }
        }
        let (s, c, a) = setup().await;
        let business = BusinessTool::new("reply");
        let rt = Runtime {
            store: s.clone(),
            model: FakeModel::scripted(vec![
                FakeModel::calls(vec![ToolCall {
                    id: "x".into(),
                    name: "reply".into(),
                    arguments: json!({"effect_key":"reply:case"}),
                }]),
                FakeModel::decision(json!({"decision":"answered"})),
            ]),
            tools: vec![ToolWithFault {
                business: business.clone(),
                store: s.clone(),
                once: AtomicBool::new(true),
            }],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: DecisionValidator::default(),
            finalizer: DecisionFinalizer,
        };
        assert_eq!(rt.drive(&c).await, Err(Error::Storage));
        let next = s.claim(&c.run, "restart", 101, 100).await.unwrap();
        rt.clock.set(101);
        assert_eq!(
            rt.drive(&next).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(business.calls.load(Ordering::SeqCst), 1);
        assert_eq!(s.account(&a).await.unwrap().used.tokens, 8);
    });
}
#[test]
fn catalog_change_cannot_route_saved_call_to_different_account() {
    block_on(async {
        let (s, c, _) = setup().await;
        let tool = BusinessTool::new("reply");
        tool.lose_response.store(true, Ordering::SeqCst);
        let model = FakeModel::scripted(vec![FakeModel::calls(vec![ToolCall {
            id: "x".into(),
            name: "reply".into(),
            arguments: json!({}),
        }])]);
        let rt = runtime(s.clone(), model.clone(), vec![tool.clone()]);
        rt.drive(&c).await.unwrap();
        let mut replacement = tool;
        replacement.name = "different".into();
        let next = s.claim(&c.run, "restart", 101, 100).await.unwrap();
        let restarted = runtime(s.clone(), model, vec![replacement]);
        restarted.clock.set(101);
        assert!(
            matches!(restarted.drive(&next).await.unwrap().state,RunState::Terminal(Terminal::Failed(ref reason)) if reason.contains("configuration"))
        );
    });
}
#[test]
fn cancellation_between_validation_and_finalize_wins() {
    block_on(async {
        struct CancelValidator(Arc<MemoryStore>);
        impl Validator for CancelValidator {
            fn validate(&self, r: &RunRecord, v: &Value) -> Result<ValidationReceipt> {
                block_on(self.0.cancel(&r.handle, "cancel before commit"))?;
                DecisionValidator::default().validate(r, v)
            }
        }
        let (s, c, _) = setup().await;
        let rt = Runtime {
            store: s.clone(),
            model: FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"answered"}))]),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: CancelValidator(s.clone()),
            finalizer: DecisionFinalizer,
        };
        let r = rt.drive(&c).await.unwrap();
        assert!(matches!(
            r.state,
            RunState::Terminal(Terminal::Cancelled(_))
        ));
        assert!(r.completion.is_none());
    });
}
#[test]
fn price_not_configured_never_calls_model() {
    block_on(async {
        struct Unpriced;
        impl ModelPort for Unpriced {
            fn limits(&self) -> ModelLimits {
                let mut l = FakeModel::default().limits();
                l.price = None;
                l
            }
            fn input_upper_bound(&self, _: &ModelRequest) -> Result<u64> {
                Ok(5)
            }
            fn recovery(&self) -> Option<(String, u64)> {
                None
            }
            async fn invoke(&self, _: &ToolContext, _: ModelRequest) -> Result<ExternalResult> {
                panic!("unpriced I/O")
            }
            async fn lookup(&self, _: &ToolContext, _: ModelRequest) -> Result<Lookup> {
                panic!("unpriced lookup")
            }
        }
        let (s, c, _) = setup().await;
        let rt = Runtime {
            store: s,
            model: Unpriced,
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: DecisionValidator::default(),
            finalizer: DecisionFinalizer,
        };
        assert!(
            matches!(rt.drive(&c).await.unwrap().state,RunState::Terminal(Terminal::BudgetReached(ref reason)) if reason.contains("price"))
        );
    });
}
#[test]
fn session_order_waits_before_any_business_io() {
    block_on(async {
        let (s, c, a) = setup().await;
        let mut first = RunSpec::new("t", "session1", a.clone(), "first");
        first.session = Some("case".into());
        let h1 = s.start(first).await.unwrap();
        let mut second = RunSpec::new("t", "session2", a, "second");
        second.session = Some("case".into());
        let h2 = s.start(second).await.unwrap();
        let owner = s.claim(&h2, "second", 0, 100).await.unwrap();
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        assert!(
            matches!(rt.drive(&owner).await.unwrap().state,RunState::Waiting(ref reason) if reason.contains("predecessor"))
        );
        assert!(rt.model.requests.lock().unwrap().is_empty());
        s.cancel(&h1, "discard predecessor").await.unwrap();
        assert!(s.check(&owner, 0, true).await.is_ok());
        s.cancel(&c.run, "done").await.unwrap();
    });
}
#[test]
fn replay_proof_cannot_survive_worker_handoff_without_new_lookup() {
    block_on(async {
        let (s, c, a) = setup().await;
        let op = s.begin(&c, request(), 0).await.unwrap();
        s.mark_sent(&c, &op.id, 0).await.unwrap();
        s.proven_unsent(&c, &op.id, "scope", 1).await.unwrap();
        let next = s.claim(&c.run, "successor", 101, 100).await.unwrap();
        assert_eq!(
            s.mark_sent(&next, &op.id, 101).await,
            Err(Error::OwnershipLost)
        );
        assert_eq!(
            s.proven_unsent(&next, &op.id, "scope", 101).await,
            Err(Error::UsageUnknown)
        );
        assert_eq!(s.account(&a).await.unwrap().reserved, Amount::new(10, 10));
    });
}
#[test]
fn tenant_round_robin_visits_third_tenant_under_refill() {
    block_on(async {
        let s = MemoryStore::default();
        for (key, tenant) in [
            ("a1", "a"),
            ("a2", "a"),
            ("b1", "b"),
            ("b2", "b"),
            ("c1", "c"),
        ] {
            s.enqueue(QueueEntry {
                key: key.into(),
                tenant: tenant.into(),
                scheduling: SchedulingPolicy {
                    priority: 1,
                    ..SchedulingPolicy::default()
                },
                deadline: 100,
            })
            .await
            .unwrap();
        }
        let mut tenants = vec![];
        for _ in 0..3 {
            tenants.push(s.dequeue(0).await.unwrap().unwrap().tenant);
        }
        assert_eq!(tenants, vec!["a", "b", "c"]);
    });
}
#[test]
fn partial_usage_keeps_result_evidence_and_resolves_only_by_lookup() {
    block_on(async {
        let (s, c, a) = setup().await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let partial = ExternalResult::Applied {
            output: json!({"receipt":"applied-but-unmetered"}),
            usage: Usage::default(),
            external_id: Some("external".into()),
        };
        assert_eq!(
            rt.dispatch(
                &c,
                request(),
                "action",
                |_| async { Ok(partial.clone()) },
                |_| async { Ok(Lookup::Unknown) }
            )
            .await
            .unwrap(),
            ExternalResult::Unknown
        );
        let op = s.operation(&c.run, "effect").await.unwrap().unwrap();
        assert_eq!(op.unsettled_result, Some(partial));
        assert_eq!(s.account(&a).await.unwrap().unknown, 1);
        assert_eq!(s.account(&a).await.unwrap().used, Amount::default());
        rt.dispatch(
            &c,
            request(),
            "action",
            |_| async { panic!("no resend") },
            |_| async { Ok(Lookup::Found(applied())) },
        )
        .await
        .unwrap();
        assert_eq!(s.account(&a).await.unwrap().unknown, 0);
        assert_eq!(s.account(&a).await.unwrap().used, Amount::new(1, 1));
    });
}
#[test]
fn default_policy_unsafe_next_model_reservation_exhausts_shared_request_account() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let a = s
            .create_account("t", Amount::new(5, 1000), None)
            .await
            .unwrap();
        let h = s
            .start(RunSpec::new("t", "small", a.clone(), "answer"))
            .await
            .unwrap();
        let c = s.claim(&h, "worker", 0, 100).await.unwrap();
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::BudgetReached(_))
        ));
        assert!(s.account(&a).await.unwrap().exhausted);
        let h2 = s
            .start(RunSpec::new("t", "other", a, "answer"))
            .await
            .unwrap();
        let c2 = s.claim(&h2, "other", 0, 100).await.unwrap();
        assert_eq!(s.begin(&c2, request(), 0).await, Err(Error::BudgetAtLimit));
        assert!(rt.model.requests.lock().unwrap().is_empty());
    });
}
