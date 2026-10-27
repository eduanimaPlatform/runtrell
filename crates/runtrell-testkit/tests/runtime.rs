use runtrell::{memory::MemoryStore, *};
use runtrell_testkit::*;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};
async fn started(s: &MemoryStore, key: &str, limit: u64) -> (RunHandle, Claim, String) {
    let a = s
        .create_account("tenant", Amount::new(limit, limit), None)
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("tenant", key, a.clone(), "judge the case"))
        .await
        .unwrap();
    let c = s.claim(&h, "worker", 0, 100).await.unwrap();
    (h, c, a)
}
fn req(key: &str, tokens: u64, cost: u64) -> DispatchRequest {
    DispatchRequest::new(
        key,
        OperationKind::Tool,
        "business",
        json!({}),
        Amount::new(tokens, cost),
    )
}
fn applied(tokens: u64, cost: u64) -> ExternalResult {
    ExternalResult::Applied {
        physical_evidence: None,
        output: json!({"ok":true}),
        usage: Usage::measured(tokens, 0, cost),
        external_id: None,
    }
}
#[test]
fn u01_structured_judgment_no_workspace_or_runner_and_insufficient_is_non_success() {
    block_on(async {
        for decision in ["no_action", "keep_separate", "insufficient"] {
            let s = Arc::new(MemoryStore::default());
            let (h, c, _) = started(&s, decision, 1000).await;
            let rt = runtime(
                s.clone(),
                FakeModel::scripted(vec![FakeModel::decision(json!({"decision":decision}))]),
                vec![],
            );
            let r = rt.drive(&c).await.unwrap();
            assert_eq!(
                r.state == RunState::Terminal(Terminal::Succeeded),
                decision != "insufficient"
            );
            assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
            assert_eq!(rt.tools.len(), 0);
            assert_eq!(r.handle, h);
            assert_eq!(rt.model.requests.lock().unwrap().len(), 1);
        }
    });
}
#[test]
fn u02_search_business_query_then_validated_answer() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (_, c, _) = started(&s, "support", 1000).await;
        let tools = vec![BusinessTool::new("search"), BusinessTool::new("case_read")];
        let replies = vec![
            FakeModel::calls(vec![ToolCall {
                id: "a".into(),
                name: "search".into(),
                arguments: json!({}),
            }]),
            FakeModel::calls(vec![ToolCall {
                id: "b".into(),
                name: "case_read".into(),
                arguments: json!({}),
            }]),
            FakeModel::decision(json!({"decision":"answered"})),
        ];
        let rt = runtime(s.clone(), FakeModel::scripted(replies), tools);
        assert_eq!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(s.operations(&c.run).await.unwrap().len(), 5);
        for tool in rt.tools {
            assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
        }
    });
}
#[test]
fn u03_lost_tool_reply_recovers_original_operation_without_resend() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (h, c, a) = started(&s, "effect", 1000).await;
        let tool = BusinessTool::new("reply");
        tool.lose_response.store(true, Ordering::SeqCst);
        let model = FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "send".into(),
                name: "reply".into(),
                arguments: json!({"effect_key":"case:reply"}),
            }]),
            FakeModel::decision(json!({"decision":"answered"})),
        ]);
        let rt = runtime(s.clone(), model.clone(), vec![tool.clone()]);
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(s.account(&a).await.unwrap().unknown, 1);
        let next = s.claim(&h, "restart", 101, 100).await.unwrap();
        let restarted = runtime(s.clone(), model, vec![tool.clone()]);
        restarted.clock.set(101);
        assert_eq!(
            restarted.drive(&next).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
        assert_eq!(s.account(&a).await.unwrap().unknown, 0);
    });
}
#[test]
fn u03_accepted_is_not_applied() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (_, c, _) = started(&s, "accepted", 1000).await;
        let tool = BusinessTool::new("action");
        tool.accepted.store(true, Ordering::SeqCst);
        let rt = runtime(
            s.clone(),
            FakeModel::scripted(vec![FakeModel::calls(vec![ToolCall {
                id: "x".into(),
                name: "action".into(),
                arguments: json!({}),
            }])]),
            vec![tool.clone()],
        );
        let r = rt.drive(&c).await.unwrap();
        assert!(matches!(r.state, RunState::Waiting(_)));
        assert!(matches!(r.delivery, Delivery::Accepted(_)));
        assert_eq!(rt.model.requests.lock().unwrap().len(), 1);
    });
}
#[test]
fn u04_duplicate_start_and_cross_run_effect_key_converge() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let tool = BusinessTool::new("reply");
        for n in 0..2 {
            let (h, c, _) = started(&s, &format!("r{n}"), 1000).await;
            let old = s.get(&h).await.unwrap();
            assert_eq!(s.start(old.spec).await.unwrap(), h);
            let rt = runtime(
                s.clone(),
                FakeModel::scripted(vec![
                    FakeModel::calls(vec![ToolCall {
                        id: "x".into(),
                        name: "reply".into(),
                        arguments: json!({"effect_key":"case:1"}),
                    }]),
                    FakeModel::decision(json!({"decision":"answered"})),
                ]),
                vec![tool.clone()],
            );
            assert_eq!(
                rt.drive(&c).await.unwrap().state,
                RunState::Terminal(Terminal::Succeeded)
            );
            assert_eq!(
                rt.drive(&c).await.unwrap().state,
                RunState::Terminal(Terminal::Succeeded)
            );
        }
        assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    });
}
#[test]
fn u05_context_budget_permission_and_tenant_are_separate() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (h, c, a) = started(&s, "a", 1000).await;
        let foreign = RunHandle {
            tenant: "other".into(),
            id: h.id.clone(),
        };
        assert_eq!(s.get(&foreign).await, Err(Error::NotFound));
        assert_eq!(
            s.start(RunSpec::new("other", "steal", a, "secret")).await,
            Err(Error::PermissionDenied)
        );
        let rt = runtime(
            s.clone(),
            FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"no_action"}))]),
            vec![],
        );
        rt.policy.revoked.store(true, Ordering::SeqCst);
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::Failed(_))
        ));
        assert!(rt.model.requests.lock().unwrap().is_empty());
        let (_, other, b) = started(&s, "b", 200).await;
        assert_eq!(s.account(&b).await.unwrap().used, Amount::default());
        assert!(s.get(&other.run).await.unwrap().checkpoint.is_none());
    });
}
#[test]
fn u06_stale_evidence_and_modified_output_refuse_finalize() {
    block_on(async {
        let s = MemoryStore::default();
        let (h, c, _) = started(&s, "stale", 1000).await;
        let run = s.get(&h).await.unwrap();
        let value = json!({"decision":"no_action"});
        let receipt = DecisionValidator::default().validate(&run, &value).unwrap();
        let completion = DecisionFinalizer.finalize(&run, value, receipt).unwrap();
        s.revise_basis(&h, 0, 1).await.unwrap();
        assert_eq!(
            s.finalize(&c, completion.clone(), 0).await,
            Err(Error::Conflict)
        );
        let mut changed = completion;
        changed.output = json!({"decision":"different"});
        assert_eq!(s.finalize(&c, changed, 0).await, Err(Error::Conflict));
    });
}
#[test]
fn u07_triggers_new_runs_input_order_and_session_cas() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let a = s
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let mut handles = vec![];
        for (n, trigger) in [
            Trigger::Api,
            Trigger::Event,
            Trigger::Schedule,
            Trigger::CaseAppend,
        ]
        .into_iter()
        .enumerate()
        {
            let mut spec = RunSpec::new("t", &n.to_string(), a.clone(), "input");
            spec.trigger = trigger;
            spec.session = Some("case".into());
            let h = s.start(spec).await.unwrap();
            assert_eq!(s.get(&h).await.unwrap().sequence, n as u64 + 1);
            handles.push(h);
        }
        let rt = runtime(
            s.clone(),
            FakeModel::scripted(vec![
                FakeModel::decision(json!({"decision":"answered"})),
                FakeModel::decision(json!({"decision":"answered"})),
            ]),
            vec![],
        );
        let first = s.claim(&handles[0], "a", 0, 100).await.unwrap();
        assert_eq!(
            rt.drive(&first).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        let next = s.claim(&handles[1], "b", 0, 100).await.unwrap();
        assert!(
            matches!(rt.drive(&next).await.unwrap().state,RunState::Terminal(Terminal::Failed(ref reason)) if reason.contains("CONTEXT_STALE"))
        );
        assert_eq!(
            s.get(&handles[0]).await.unwrap().output,
            Some(json!({"decision":"answered"}))
        );
    });
}
#[test]
fn u09_parallel_ancestor_reservations_do_not_double_allocate() {
    let s = Arc::new(MemoryStore::default());
    let (a, c1, c2) = block_on(async {
        let root = s
            .create_account("*", Amount::new(100, 100), None)
            .await
            .unwrap();
        let a = s
            .create_account("t", Amount::new(100, 100), Some(root))
            .await
            .unwrap();
        let h1 = s
            .start(RunSpec::new("t", "1", a.clone(), "x"))
            .await
            .unwrap();
        let h2 = s
            .start(RunSpec::new("t", "2", a.clone(), "y"))
            .await
            .unwrap();
        (
            a,
            s.claim(&h1, "a", 0, 100).await.unwrap(),
            s.claim(&h2, "b", 0, 100).await.unwrap(),
        )
    });
    let s1 = s.clone();
    let s2 = s.clone();
    let x = std::thread::spawn(move || block_on(s1.begin(&c1, req("a", 70, 70), 0)));
    let y = std::thread::spawn(move || block_on(s2.begin(&c2, req("b", 70, 70), 0)));
    let results = [x.join().unwrap(), y.join().unwrap()];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        block_on(s.account(&a)).unwrap().reserved,
        Amount::new(70, 70)
    );
}
#[test]
fn default_policy_warning_thresholds_and_duplicate_receipts() {
    block_on(async {
        let s = MemoryStore::default();
        let (_, c, a) = started(&s, "budget", 100).await;
        for (key, tokens, expected) in [("a", 50, 50), ("b", 30, 80), ("c", 15, 95)] {
            let op = s.begin(&c, req(key, tokens, tokens), 0).await.unwrap();
            s.record(&c, &op.id, applied(tokens, tokens), 0)
                .await
                .unwrap();
            s.record(&c, &op.id, applied(tokens, tokens), 0)
                .await
                .unwrap();
            assert_eq!(
                s.record(&c, &op.id, applied(tokens, 0), 0).await,
                Err(Error::Conflict)
            );
            let w = s.warnings(&c, 0).await.unwrap();
            assert_eq!(
                (w[0].token_percent, w[0].cost_percent),
                (Some(expected), Some(expected))
            );
            assert!(s.warnings(&c, 0).await.unwrap().is_empty());
        }
        assert_eq!(s.account(&a).await.unwrap().used, Amount::new(95, 95));
        let op = s.begin(&c, req("last", 5, 0), 0).await.unwrap();
        s.record(&c, &op.id, applied(5, 0), 0).await.unwrap();
        assert_eq!(
            s.begin(&c, req("denied", 0, 1), 0).await,
            Err(Error::BudgetAtLimit)
        );
        assert_eq!(
            s.get(&c.run)
                .await
                .unwrap()
                .checkpoint
                .unwrap()
                .messages
                .len(),
            3
        );
    });
}
#[test]
fn invalid_partial_usage_and_unknown_hold_reservation() {
    block_on(async {
        let s = MemoryStore::default();
        let (_, c, a) = started(&s, "unknown", 100).await;
        let op = s.begin(&c, req("a", 30, 20), 0).await.unwrap();
        s.record(&c, &op.id, ExternalResult::Unknown, 0)
            .await
            .unwrap();
        for usage in [
            Usage::default(),
            Usage {
                input: None,
                ..Usage::measured(10, 5, 1)
            },
            Usage {
                cached: Some(11),
                ..Usage::measured(10, 5, 1)
            },
        ] {
            assert!(
                s.record(
                    &c,
                    &op.id,
                    ExternalResult::Applied {
                        physical_evidence: None,
                        output: Value::Null,
                        usage,
                        external_id: None
                    },
                    0
                )
                .await
                .is_err()
            );
            assert_eq!(s.account(&a).await.unwrap().reserved, Amount::new(30, 20));
        }
        assert_eq!(s.release_slot(&op.id).await, Err(Error::UsageUnknown));
        s.record(&c, &op.id, applied(10, 1), 0).await.unwrap();
        assert_eq!(s.account(&a).await.unwrap().unknown, 0);
    });
}
#[test]
fn price_cap_integer_rounding_subsets_and_no_fixed_output() {
    let limits = ModelLimits {
        model: "m".into(),
        context: 100,
        max_output: 90,
        min_max_output_tokens: 1,
        timeout: 30,
        price: Some(Price {
            model: "m".into(),
            input: 1_000_000,
            cached: 0,
            output: 2_000_000,
            reasoning: 2_000_000,
            image_input: 1_000_000,
            cached_image_input: 0,
            image_output: 2_000_000,
        }),
    };
    assert_eq!(
        limits.cap(10, Amount::new(100, 50)).unwrap(),
        (20, Amount::new(30, 50))
    );
    assert_eq!(limits.cap(10, Amount::new(20, 1000)).unwrap().0, 10);
    assert_eq!(
        limits
            .price
            .as_ref()
            .unwrap()
            .cost(&Usage {
                cached: Some(5),
                reasoning: Some(2),
                ..Usage::measured(10, 5, 0)
            })
            .unwrap(),
        15
    );
    let mut missing = limits;
    missing.price = None;
    assert_eq!(
        missing.cap(1, Amount::new(100, 100)),
        Err(Error::PriceNotConfigured)
    );
}
#[test]
fn cancellation_does_not_start_business_but_reconciliation_settlement_cleanup_continue() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (h, c, a) = started(&s, "cancel", 1000).await;
        let op = s.begin(&c, req("effect", 20, 20), 0).await.unwrap();
        s.mark_sent(&c, &op.id, 0).await.unwrap();
        s.cancel(&h, "stop").await.unwrap();
        assert_eq!(s.begin(&c, req("new", 1, 1), 0).await, Err(Error::Closed));
        assert_eq!(s.mark_sent(&c, &op.id, 0).await, Err(Error::Closed));
        let r = s.get(&h).await.unwrap();
        assert_eq!(r.stop, Progress::Pending);
        assert_eq!(r.cleanup, Progress::Pending);
        assert_eq!(r.settlement, Progress::Pending);
        s.record(&c, &op.id, applied(5, 3), 0).await.unwrap();
        assert_eq!(s.account(&a).await.unwrap().used, Amount::new(5, 3));
        assert_eq!(s.get(&h).await.unwrap().cleanup, Progress::Pending);
        s.cleanup(&c, Progress::Confirmed, Progress::Unknown, 0)
            .await
            .unwrap();
        let r = s.get(&h).await.unwrap();
        assert_eq!(r.stop, Progress::Confirmed);
        assert_eq!(r.cleanup, Progress::Unknown);
        assert_eq!(r.settlement, Progress::Confirmed);
        assert!(matches!(
            r.state,
            RunState::Terminal(Terminal::Cancelled(_))
        ));
    });
}
#[test]
fn deadline_retry_worker_move_does_not_reset_original_contract() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let a = s
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let mut spec = RunSpec::new("t", "deadline", a.clone(), "x");
        spec.deadline = Some(5);
        let h = s.start(spec).await.unwrap();
        let c = s.claim(&h, "old", 0, 4).await.unwrap();
        let op = s.begin(&c, req("first", 20, 20), 0).await.unwrap();
        s.record(&c, &op.id, applied(10, 10), 0).await.unwrap();
        let next = s.claim(&h, "new", 5, 100).await.unwrap();
        assert_eq!(
            s.begin(&next, req("retry", 1, 1), 5).await,
            Err(Error::Deadline)
        );
        assert_eq!(
            s.begin(&c, req("stale", 1, 1), 5).await,
            Err(Error::OwnershipLost)
        );
        assert_eq!(s.account(&a).await.unwrap().used, Amount::new(10, 10));
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        rt.clock.set(5);
        assert_eq!(
            rt.drive(&next).await.unwrap().state,
            RunState::Terminal(Terminal::DeadlineReached)
        );
    });
}
#[test]
fn storage_barriers_prevent_io_and_preserve_atomic_result_and_checkpoint() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (_, c, a) = started(&s, "storage", 100).await;
        s.fail_next_write();
        assert_eq!(s.begin(&c, req("x", 50, 50), 0).await, Err(Error::Storage));
        assert_eq!(s.account(&a).await.unwrap().reserved, Amount::default());
        assert!(s.operation(&c.run, "x").await.unwrap().is_none());
        let op = s.begin(&c, req("x", 50, 50), 0).await.unwrap();
        s.fail_next_write();
        assert_eq!(
            s.record(&c, &op.id, applied(10, 10), 0).await,
            Err(Error::Storage)
        );
        assert_eq!(s.account(&a).await.unwrap().used, Amount::default());
        let r = s.get(&c.run).await.unwrap();
        let cp = Checkpoint {
            schema: 1,
            revision: r.revision,
            payload: json!({"old":"valid"}),
            messages: vec![],
            cursor: 0,
        };
        s.checkpoint(&c, r.revision, cp.clone(), 0).await.unwrap();
        let mut changed = cp.clone();
        changed.payload = json!({"new":"summary"});
        assert_eq!(
            s.checkpoint(&c, r.revision, changed, 0).await,
            Err(Error::Conflict)
        );
        assert_eq!(s.get(&c.run).await.unwrap().checkpoint, Some(cp));
    });
}
#[test]
fn queue_backpressure_timeout_round_robin_and_priority_are_bounded() {
    block_on(async {
        let cap = Capacity {
            queue: 5,
            tenant_queue: 3,
            ..Capacity::default()
        };
        let s = MemoryStore::new(cap);
        for (key, tenant, fast) in [
            ("a1", "a", true),
            ("a2", "a", true),
            ("a3", "a", false),
            ("b1", "b", true),
            ("b2", "b", true),
        ] {
            s.enqueue(QueueEntry {
                key: key.into(),
                tenant: tenant.into(),
                scheduling: SchedulingPolicy {
                    priority: i32::from(fast),
                    ..SchedulingPolicy::default()
                },
                deadline: 10,
            })
            .await
            .unwrap();
        }
        assert_eq!(
            s.enqueue(QueueEntry {
                key: "full".into(),
                tenant: "c".into(),
                scheduling: SchedulingPolicy {
                    priority: 0,
                    ..SchedulingPolicy::default()
                },
                deadline: 10
            })
            .await,
            Err(Error::Backpressure)
        );
        let mut order = vec![];
        while let Some(e) = s.dequeue(0).await.unwrap() {
            order.push(e.key);
        }
        assert_eq!(order, vec!["a1", "b1", "a3", "b2", "a2"]);
        s.enqueue(QueueEntry {
            key: "expired".into(),
            tenant: "a".into(),
            scheduling: SchedulingPolicy {
                priority: 0,
                ..SchedulingPolicy::default()
            },
            deadline: 1,
        })
        .await
        .unwrap();
        assert!(matches!(s.dequeue(1).await, Err(Error::QueueTimeout)));
        assert_eq!(s.queue_len().await, 0);
    });
}
#[test]
fn shared_slots_enforce_kind_tenant_and_run_independently() {
    block_on(async {
        let mut cap = Capacity::default();
        cap.global.insert(OperationKind::Tool, 2);
        cap.tenant.insert(OperationKind::Tool, 1);
        let s = MemoryStore::new(cap);
        let slot = |key: &str, tenant: &str, kind| SlotRequest {
            key: key.into(),
            tenant: tenant.into(),
            run: "r".into(),
            kind,
        };
        s.acquire(slot("a", "a", OperationKind::Tool))
            .await
            .unwrap();
        assert_eq!(
            s.acquire(slot("a2", "a", OperationKind::Tool)).await,
            Err(Error::Backpressure)
        );
        s.acquire(slot("b", "b", OperationKind::Tool))
            .await
            .unwrap();
        assert_eq!(
            s.acquire(slot("c", "c", OperationKind::Tool)).await,
            Err(Error::Backpressure)
        );
        s.acquire(slot("model", "a", OperationKind::Model))
            .await
            .unwrap();
        s.release_slot("a").await.unwrap();
        s.acquire(slot("c", "c", OperationKind::Tool))
            .await
            .unwrap();
    });
}
#[test]
fn all_paid_paths_use_dispatch_and_cannot_reset_parent_budget_or_deadline() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (h, c, a) = started(&s, "paths", 100).await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        for (n, kind) in [
            OperationKind::Model,
            OperationKind::Tool,
            OperationKind::Compaction,
            OperationKind::Retry,
        ]
        .into_iter()
        .enumerate()
        {
            let d = DispatchRequest::new(
                &format!("path-{n}"),
                kind,
                "host",
                json!({}),
                Amount::new(5, 5),
            );
            rt.dispatch(
                &c,
                d,
                "allowed",
                |_| async { Ok(applied(3, 2)) },
                |_| async { Ok(Lookup::Unknown) },
            )
            .await
            .unwrap();
        }
        assert_eq!(s.account(&a).await.unwrap().used, Amount::new(12, 8));
        assert_eq!(s.operations(&h).await.unwrap().len(), 4);
        let ch = rt
            .spawn_child(
                &c,
                "child",
                RunSpec::new("tenant", "child", a.clone(), "child"),
            )
            .await
            .unwrap();
        assert_eq!(s.get(&ch).await.unwrap().spec.account, a);
        let cc = s.claim(&ch, "child-worker", 0, 100).await.unwrap();
        rt.dispatch(
            &cc,
            DispatchRequest::new(
                "paid-child-tool",
                OperationKind::Tool,
                "host",
                json!({}),
                Amount::new(5, 5),
            ),
            "allowed",
            |_| async { Ok(applied(3, 2)) },
            |_| async { Ok(Lookup::Unknown) },
        )
        .await
        .unwrap();
        assert_eq!(s.account(&a).await.unwrap().used, Amount::new(15, 10));
        assert_eq!(s.account(&a).await.unwrap().reserved, Amount::default());
        assert_eq!(s.operations(&h).await.unwrap().len(), 5);
        assert_eq!(s.operations(&ch).await.unwrap().len(), 1);
        s.cancel(&h, "parent stopped").await.unwrap();
        assert!(matches!(
            s.get(&ch).await.unwrap().state,
            RunState::Terminal(Terminal::Cancelled(_))
        ));
    });
}
#[test]
fn duplicate_conflicting_request_and_operation_payload_are_rejected() {
    block_on(async {
        let s = MemoryStore::default();
        let (h, c, _) = started(&s, "conflict", 100).await;
        let mut spec = s.get(&h).await.unwrap().spec;
        spec.input = "different".into();
        assert_eq!(s.start(spec).await, Err(Error::Conflict));
        let op = s.begin(&c, req("a", 1, 1), 0).await.unwrap();
        let mut altered = op.request;
        altered.input = json!({"different":1});
        assert_eq!(s.begin(&c, altered, 0).await, Err(Error::Conflict));
    });
}
#[test]
fn default_policy_either_axis_exact_limit_refuses_finalization() {
    block_on(async {
        for amount in [
            Amount::new(100, 25),
            Amount::new(25, 100),
            Amount::new(100, 100),
        ] {
            let s = MemoryStore::default();
            let (h, c, a) = started(&s, "exact", 100).await;
            let op = s
                .begin(&c, req("x", amount.tokens, amount.micro_usd), 0)
                .await
                .unwrap();
            s.record(&c, &op.id, applied(amount.tokens, amount.micro_usd), 0)
                .await
                .unwrap();
            let run = s.get(&h).await.unwrap();
            let output = json!({"decision":"no_action"});
            let receipt = DecisionValidator::default()
                .validate(&run, &output)
                .unwrap();
            let complete = DecisionFinalizer.finalize(&run, output, receipt).unwrap();
            assert_eq!(s.finalize(&c, complete, 0).await, Err(Error::BudgetAtLimit));
            assert_eq!(s.account(&a).await.unwrap().used, amount);
        }
    });
}
#[test]
fn warning_survives_run_switch_and_is_in_next_model_request() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (_, c, a) = started(&s, "first", 100).await;
        let op = s.begin(&c, req("usage", 50, 50), 0).await.unwrap();
        s.record(&c, &op.id, applied(50, 50), 0).await.unwrap();
        let second = s
            .start(RunSpec::new("tenant", "second", a.clone(), "answer"))
            .await
            .unwrap();
        let next = s.claim(&second, "next", 0, 100).await.unwrap();
        let rt = runtime(
            s.clone(),
            FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"answered"}))]),
            vec![],
        );
        rt.drive(&next).await.unwrap();
        {
            let requests = rt.model.requests.lock().unwrap();
            assert!(
                requests[0]
                    .messages
                    .iter()
                    .any(|m| m.role == "system" && m.text.contains("50"))
            );
        }
        assert!(s.warnings(&c, 0).await.unwrap().is_empty());
    });
}
#[test]
fn stream_partial_never_executes_tools_or_finalizes() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (_, c, _) = started(&s, "partial", 1000).await;
        let tool = BusinessTool::new("action");
        let mut reply = FakeModel::calls(vec![ToolCall {
            id: "x".into(),
            name: "action".into(),
            arguments: json!({}),
        }]);
        reply.complete = false;
        let rt = runtime(
            s.clone(),
            FakeModel::scripted(vec![reply]),
            vec![tool.clone()],
        );
        let r = rt.drive(&c).await.unwrap();
        assert!(matches!(r.state, RunState::Terminal(Terminal::Failed(_))));
        assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
        assert!(r.completion.is_none());
    });
}
#[test]
fn durable_result_before_failed_checkpoint_recovers_without_resampling() {
    block_on(async {
        struct FailValidator(Arc<MemoryStore>);
        impl Validator for FailValidator {
            fn validate(&self, r: &RunRecord, v: &Value) -> Result<ValidationReceipt> {
                self.0.fail_next_write();
                DecisionValidator::default().validate(r, v)
            }
        }
        let s = Arc::new(MemoryStore::default());
        let (h, c, a) = started(&s, "finalize_storage", 1000).await;
        let model = FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"no_action"}))]);
        let rt = Runtime {
            store: s.clone(),
            model: model.clone(),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: std::sync::Arc::new(FailValidator(s.clone())),
            finalizer: std::sync::Arc::new(DecisionFinalizer),
        };
        assert_eq!(rt.drive(&c).await, Err(Error::Storage));
        let next = s.claim(&h, "new", 101, 100).await.unwrap();
        let rt = runtime(s.clone(), model.clone(), vec![]);
        rt.clock.set(101);
        assert_eq!(
            rt.drive(&next).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(model.requests.lock().unwrap().len(), 1);
        assert_eq!(s.account(&a).await.unwrap().used.tokens, 4);
    });
}
#[test]
fn no_removed_turn_or_tool_count_limits_are_reintroduced() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (_, c, _) = started(&s, "long", 1_000_000).await;
        let mut replies = vec![];
        for turn in 0..65 {
            replies.push(FakeModel::calls(
                (0..4)
                    .map(|n| ToolCall {
                        id: format!("{turn}-{n}"),
                        name: "read".into(),
                        arguments: json!({}),
                    })
                    .collect(),
            ));
        }
        replies.push(FakeModel::decision(json!({"decision":"answered"})));
        let rt = runtime(
            s.clone(),
            FakeModel::scripted(replies),
            vec![BusinessTool::new("read")],
        );
        assert_eq!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(rt.model.requests.lock().unwrap().len(), 66);
        assert_eq!(
            s.operations(&c.run)
                .await
                .unwrap()
                .iter()
                .filter(|op| op.request.kind == OperationKind::Tool)
                .count(),
            260
        );
    });
}
#[test]
fn child_runs_keep_capacity_and_inherit_budget_deadline_cancel() {
    block_on(async {
        let mut capacity = Capacity::default();
        capacity.global.insert(OperationKind::ChildRun, 1);
        let s = Arc::new(MemoryStore::new(capacity));
        let a = s
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let mut parent = RunSpec::new("t", "parent", a.clone(), "parent");
        parent.deadline = Some(20);
        let h = s.start(parent).await.unwrap();
        let c = s.claim(&h, "p", 0, 100).await.unwrap();
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let child = rt
            .spawn_child(
                &c,
                "child-op",
                RunSpec::new("t", "child", "ignored".into(), "child"),
            )
            .await
            .unwrap();
        assert_eq!(
            rt.spawn_child(
                &c,
                "child-op",
                RunSpec::new("t", "child", "ignored".into(), "child")
            )
            .await
            .unwrap(),
            child
        );
        let record = s.get(&child).await.unwrap();
        assert_eq!(record.spec.account, a);
        assert_eq!(record.spec.deadline, Some(20));
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 1);
        assert_eq!(
            rt.spawn_child(
                &c,
                "other-op",
                RunSpec::new("t", "other", "ignored".into(), "child")
            )
            .await,
            Err(Error::Backpressure)
        );
        s.cancel(&h, "cancel").await.unwrap();
        assert!(matches!(
            s.get(&child).await.unwrap().state,
            RunState::Terminal(Terminal::Cancelled(_))
        ));
        rt.reconcile(&c).await.unwrap();
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 0);
        assert_eq!(s.account(&a).await.unwrap().reserved, Amount::default());
    });
}
#[test]
fn bounded_tool_timeout_keeps_unknown_cost_and_records_separate_stop() {
    block_on(async {
        struct Hang(TestClock);
        impl Tool for Hang {
            fn descriptor(&self) -> ToolDescriptor {
                let mut d = BusinessTool::new("hang").descriptor();
                d.reserve = Amount::new(2, 2);
                d.timeout = 1;
                d
            }
            async fn invoke(&self, _: &ToolContext, _: Value) -> Result<ExternalResult> {
                self.0.set(2);
                std::future::pending().await
            }
            async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
                Ok(Lookup::Unknown)
            }
        }
        let s = Arc::new(MemoryStore::default());
        let (_, c, a) = started(&s, "hang", 100).await;
        let clock = TestClock::default();
        let rt = Runtime {
            store: s.clone(),
            model: FakeModel::scripted(vec![FakeModel::calls(vec![ToolCall {
                id: "x".into(),
                name: "hang".into(),
                arguments: json!({}),
            }])]),
            tools: vec![Hang(clock.clone())],
            policy: FakePolicy::default(),
            clock,
            validator: std::sync::Arc::new(DecisionValidator::default()),
            finalizer: std::sync::Arc::new(DecisionFinalizer),
        };
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(s.account(&a).await.unwrap().reserved, Amount::new(2, 2));
        assert_eq!(s.account(&a).await.unwrap().unknown, 1);
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::OutcomeUnknown(_))
        ));
    });
}

#[test]
fn child_admission_is_atomic_and_fenced() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (parent, claim, account) = started(&s, "atomic-child", 1000).await;
        let spec = RunSpec::new("tenant", "child", account.clone(), "child");
        let barrier = std::sync::Barrier::new(2);
        let run = || {
            barrier.wait();
            block_on(s.admit_child(&claim, "child-op", spec.clone(), 0))
        };
        let (a, b) = std::thread::scope(|scope| {
            let a = scope.spawn(run);
            let b = scope.spawn(run);
            (a.join().unwrap(), b.join().unwrap())
        });
        let child = a.unwrap();
        assert_eq!(b.unwrap(), child);
        assert_eq!(
            s.admit_child(&claim, "child-op", spec.clone(), 0)
                .await
                .unwrap(),
            child
        );
        let op = s.operation(&parent, "child-op").await.unwrap().unwrap();
        assert_ne!(child.id, op.id, "the handle must contain the child Run ID");
        assert_eq!(
            op.result,
            Some(ExternalResult::Accepted {
                external_id: child.id.clone()
            })
        );
        assert_eq!(op.send_count, 0);
        assert!(!op.settled);
        assert_eq!(s.queue_len().await, 1);
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 1);
        for field in ["input", "config", "account", "deadline"] {
            let mut changed = spec.clone();
            match field {
                "input" => changed.input = "different".into(),
                "config" => changed.config_version = "different".into(),
                "account" => changed.account = "different".into(),
                _ => changed.deadline = Some(50),
            }
            assert_eq!(
                s.admit_child(&claim, "child-op", changed, 0).await,
                Err(Error::Conflict)
            );
        }
        let next = s.claim(&parent, "restart", 101, 100).await.unwrap();
        for key in ["child-op", "stale-new"] {
            assert_eq!(
                s.admit_child(&claim, key, spec.clone(), 101).await,
                Err(Error::OwnershipLost)
            );
        }
        assert_eq!(s.queue_len().await, 1);
        assert_eq!(
            s.admit_child(&next, "child-op", spec.clone(), 101)
                .await
                .unwrap(),
            child
        );
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        rt.clock.set(101);
        rt.reconcile(&next).await.unwrap();
        assert!(
            !s.operation(&parent, "child-op")
                .await
                .unwrap()
                .unwrap()
                .settled
        );
        s.cancel(&child, "done").await.unwrap();
        rt.reconcile(&next).await.unwrap();
        rt.reconcile(&next).await.unwrap();
        assert!(
            s.operation(&parent, "child-op")
                .await
                .unwrap()
                .unwrap()
                .settled
        );
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 0);
        assert_eq!(s.account(&account).await.unwrap().used, Amount::default());
        assert_eq!(
            s.admit_child(&next, "child-op", spec, 101).await.unwrap(),
            child
        );
    });
}

#[test]
fn child_admission_rejects_closed_expired_and_unauthorized_parents() {
    block_on(async {
        for reason in ["cancel", "terminal", "deadline", "lease", "tenant"] {
            let s = Arc::new(MemoryStore::default());
            let account = s
                .create_account("tenant", Amount::new(100, 100), None)
                .await
                .unwrap();
            let mut parent = RunSpec::new("tenant", reason, account.clone(), "parent");
            parent.deadline = Some(10);
            let parent = s.start(parent).await.unwrap();
            let claim = s.claim(&parent, "worker", 0, 20).await.unwrap();
            let mut spec = RunSpec::new("tenant", "child", account, "child");
            let (now, expected) = match reason {
                "cancel" => {
                    s.cancel(&parent, "stop").await.unwrap();
                    (0, Error::Closed)
                }
                "terminal" => {
                    s.terminate(&claim, Terminal::Failed("stop".into()), 0)
                        .await
                        .unwrap();
                    (0, Error::Closed)
                }
                "deadline" => (10, Error::Deadline),
                "lease" => (20, Error::OwnershipLost),
                _ => {
                    spec.tenant = "other".into();
                    (0, Error::PermissionDenied)
                }
            };
            assert_eq!(
                s.admit_child(&claim, "child-op", spec, now).await,
                Err(expected)
            );
            assert_eq!(s.queue_len().await, 0);
            assert_eq!(s.slot_count(OperationKind::ChildRun).await, 0);
            assert!(s.list_operations(&parent).await.unwrap().is_empty());
        }
    });
}

#[test]
fn child_admission_rolls_back_and_scopes_identity_to_parent_and_operation() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (parent, claim, account) = started(&s, "parent-a", 1000).await;
        let spec = RunSpec::new("tenant", "same-request", account.clone(), "child");
        s.fail_next_write();
        assert_eq!(
            s.admit_child(&claim, "child-op", spec.clone(), 0).await,
            Err(Error::Storage)
        );
        assert_eq!(s.queue_len().await, 0);
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 0);
        assert!(s.list_operations(&parent).await.unwrap().is_empty());
        let first = s
            .admit_child(&claim, "child-op", spec.clone(), 0)
            .await
            .unwrap();
        let other_key = s
            .admit_child(&claim, "other-op", spec.clone(), 0)
            .await
            .unwrap();
        assert_ne!(first, other_key);
        let other_parent = s
            .start(RunSpec::new("tenant", "parent-b", account, "parent"))
            .await
            .unwrap();
        let other_claim = s.claim(&other_parent, "worker-b", 0, 100).await.unwrap();
        let other_child = s
            .admit_child(&other_claim, "child-op", spec, 0)
            .await
            .unwrap();
        assert_ne!(first, other_child);
        assert_eq!(s.queue_len().await, 3);
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 3);
    });
}

#[test]
fn spawn_child_refences_after_policy_and_requires_current_authorization() {
    struct Handoff {
        store: Arc<MemoryStore>,
        clock: TestClock,
    }
    impl Policy for Handoff {
        fn authorize(&self, run: &RunRecord, _: &str, _: &str) -> Result<()> {
            self.clock.set(101);
            block_on(self.store.claim(&run.handle, "successor", 101, 100))?;
            Ok(())
        }
    }
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (parent, claim, account) = started(&s, "handoff", 1000).await;
        let spec = RunSpec::new("tenant", "child", account, "child");
        let denied = runtime(s.clone(), FakeModel::default(), vec![]);
        denied.policy.revoked.store(true, Ordering::SeqCst);
        assert_eq!(
            denied.spawn_child(&claim, "child-op", spec.clone()).await,
            Err(Error::PermissionDenied)
        );
        let clock = TestClock::default();
        let rt = Runtime {
            store: s.clone(),
            model: FakeModel::default(),
            tools: Vec::<BusinessTool>::new(),
            policy: Handoff {
                store: s.clone(),
                clock: clock.clone(),
            },
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        assert_eq!(
            rt.spawn_child(&claim, "child-op", spec).await,
            Err(Error::OwnershipLost)
        );
        assert_eq!(s.queue_len().await, 0);
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 0);
        assert!(s.list_operations(&parent).await.unwrap().is_empty());
    });
}

#[test]
fn child_join_yields_single_run_permit_and_resumes_parent_with_shared_paid_budget() {
    struct PaidChildTool(Arc<std::sync::atomic::AtomicU64>);
    impl Tool for PaidChildTool {
        fn descriptor(&self) -> ToolDescriptor {
            let mut descriptor = BusinessTool::new("read").descriptor();
            descriptor.reserve = Amount::new(2, 3);
            descriptor
        }
        async fn invoke(&self, _: &ToolContext, _: Value) -> Result<ExternalResult> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(applied(2, 3))
        }
        async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
            Ok(Lookup::Unknown)
        }
    }
    block_on(async {
        let mut cap = Capacity::default();
        for limits in [&mut cap.global, &mut cap.tenant, &mut cap.run] {
            for kind in [
                OperationKind::Run,
                OperationKind::ChildRun,
                OperationKind::Model,
                OperationKind::Tool,
            ] {
                limits.insert(kind, 1);
            }
        }
        cap.queue = 1;
        cap.tenant_queue = 1;
        let s = Arc::new(MemoryStore::new(cap));
        let (parent, claim, account) = started(&s, "single-permit", 1000).await;
        let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let rt = Runtime {
            store: s.clone(),
            model: FakeModel::scripted(vec![
                FakeModel::calls(vec![ToolCall {
                    id: "tool".into(),
                    name: "read".into(),
                    arguments: json!({}),
                }]),
                FakeModel::decision(json!({"decision":"no_action"})),
                FakeModel::decision(json!({"decision":"no_action"})),
            ]),
            tools: vec![PaidChildTool(calls.clone())],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let child = rt
            .spawn_child(
                &claim,
                "child-op",
                RunSpec::new("tenant", "child", account.clone(), "child"),
            )
            .await
            .unwrap();
        assert!(matches!(
            rt.join_child(&claim, "child-op").await,
            Err(Error::Pending(_))
        ));
        assert!(matches!(
            s.get(&parent).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(s.slot_count(OperationKind::Run).await, 0);
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 1);
        assert!(matches!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(rt.model.requests.lock().unwrap().len(), 0);
        assert_eq!(s.slot_count(OperationKind::Run).await, 0);
        let queued = s.dequeue(0).await.unwrap().unwrap();
        assert_eq!(queued.key, format!("{}:{}", child.tenant, child.id));
        let child_claim = s.claim(&child, "same-worker", 0, 100).await.unwrap();
        assert_eq!(
            rt.drive(&child_claim).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        let adopted = rt.join_child(&claim, "child-op").await.unwrap();
        assert_eq!(adopted.handle, child);
        assert!(adopted.output.is_some());
        assert_eq!(s.account(&account).await.unwrap().used, Amount::new(10, 11));
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 0);
        let finished = rt.drive(&claim).await.unwrap();
        assert_eq!(finished.state, RunState::Terminal(Terminal::Succeeded));
        assert_eq!(
            rt.drive(&claim).await.unwrap().completion,
            finished.completion
        );
        assert_eq!(s.account(&account).await.unwrap().used, Amount::new(14, 15));
        assert_eq!(
            s.account(&account).await.unwrap().reserved,
            Amount::default()
        );
        assert_eq!(s.slot_count(OperationKind::Run).await, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn child_sessions_are_independent_and_ancestor_session_cycles_are_rejected() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let account = s
            .create_account("tenant", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let mut spec = RunSpec::new("tenant", "parent-session", account.clone(), "parent");
        spec.session = Some("conversation".into());
        let parent = s.start(spec).await.unwrap();
        let claim = s.claim(&parent, "worker", 0, 100).await.unwrap();
        let rt = runtime(
            s.clone(),
            FakeModel::scripted(vec![
                FakeModel::decision(json!({"decision":"no_action"})),
                FakeModel::decision(json!({"decision":"no_action"})),
            ]),
            vec![],
        );
        let mut same = RunSpec::new("tenant", "same", account.clone(), "child");
        same.session = Some("conversation".into());
        assert_eq!(
            rt.spawn_child(&claim, "same", same.clone()).await,
            Err(Error::Invalid(
                "child session belongs to an ancestor".into()
            ))
        );
        assert!(s.operation(&parent, "same").await.unwrap().is_none());
        let child = rt
            .spawn_child(
                &claim,
                "child",
                RunSpec::new("tenant", "child", account.clone(), "child"),
            )
            .await
            .unwrap();
        assert_eq!(s.get(&child).await.unwrap().spec.session, None);
        let child_claim = s.claim(&child, "child-worker", 0, 100).await.unwrap();
        assert_eq!(
            rt.spawn_child(&child_claim, "cycle", same).await,
            Err(Error::Invalid(
                "child session belongs to an ancestor".into()
            ))
        );
        assert!(s.operation(&child, "cycle").await.unwrap().is_none());
        assert_eq!(
            rt.drive(&child_claim).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(rt.join_child(&claim, "child").await.unwrap().handle, child);
        assert_eq!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(s.get(&parent).await.unwrap().spec.context_revision, 0);
        let mut next = RunSpec::new("tenant", "next", account, "next input");
        next.session = Some("conversation".into());
        next.context_revision = 1;
        assert!(s.start(next).await.is_ok());
    });
}

#[test]
fn child_existing_results_remain_accessible_after_cancel_or_deadline() {
    block_on(async {
        for cancelled in [false, true] {
            let s = Arc::new(MemoryStore::default());
            let account = s
                .create_account("tenant", Amount::new(100, 100), None)
                .await
                .unwrap();
            let mut parent_spec = RunSpec::new("tenant", "parent", account.clone(), "parent");
            parent_spec.deadline = Some(10);
            let parent = s.start(parent_spec).await.unwrap();
            let claim = s.claim(&parent, "worker", 0, 100).await.unwrap();
            let rt = runtime(s.clone(), FakeModel::default(), vec![]);
            let spec = RunSpec::new("tenant", "child", account, "child");
            let child = rt.spawn_child(&claim, "child", spec.clone()).await.unwrap();
            if cancelled {
                s.cancel(&parent, "stop").await.unwrap();
            } else {
                rt.clock.set(10);
                let child_claim = s.claim(&child, "child-worker", 10, 100).await.unwrap();
                assert_eq!(
                    rt.drive(&child_claim).await.unwrap().state,
                    RunState::Terminal(Terminal::DeadlineReached)
                );
            }
            assert_eq!(
                rt.spawn_child(&claim, "child", spec.clone()).await.unwrap(),
                child
            );
            assert_eq!(
                rt.spawn_child(&claim, "new", spec).await,
                Err(if cancelled {
                    Error::Closed
                } else {
                    Error::Deadline
                })
            );
            let result = rt.join_child(&claim, "child").await.unwrap();
            assert_eq!(result.handle, child);
            assert!(!matches!(
                result.state,
                RunState::Terminal(Terminal::Succeeded)
            ));
            assert_eq!(s.slot_count(OperationKind::ChildRun).await, 0);
            rt.policy.revoked.store(true, Ordering::SeqCst);
            assert_eq!(
                rt.join_child(&claim, "child").await,
                Err(Error::PermissionDenied)
            );
        }
    });
}

#[test]
fn child_cancel_keeps_unknown_paid_work_and_lifetime_slot_until_recovered() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (_, claim, account) = started(&s, "cancel-paid-child", 1000).await;
        let model = FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"no_action"}))]);
        model.lose_response.store(true, Ordering::SeqCst);
        let rt = runtime(s.clone(), model, vec![]);
        let child = rt
            .spawn_child(
                &claim,
                "child",
                RunSpec::new("tenant", "child", account.clone(), "child"),
            )
            .await
            .unwrap();
        let cc = s.claim(&child, "child-worker", 0, 100).await.unwrap();
        assert!(matches!(
            rt.drive(&cc).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(s.account(&account).await.unwrap().unknown, 1);
        let reserved = s.account(&account).await.unwrap().reserved;
        assert_ne!(reserved, Amount::default());
        rt.cancel(&claim, "cancel parent").await.unwrap();
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 1);
        assert!(matches!(
            rt.join_child(&claim, "child").await,
            Err(Error::Pending(_))
        ));
        assert_eq!(s.account(&account).await.unwrap().reserved, reserved);
        rt.reconcile(&cc).await.unwrap();
        rt.reconcile(&cc).await.unwrap();
        assert_eq!(s.account(&account).await.unwrap().used, Amount::new(4, 4));
        assert_eq!(s.account(&account).await.unwrap().unknown, 0);
        assert_eq!(rt.model.requests.lock().unwrap().len(), 1);
        assert!(matches!(
            rt.join_child(&claim, "child").await,
            Err(Error::Pending(_))
        ));
        // Host confirmation of stop/cleanup is separate from a cost receipt.
        s.cleanup(&cc, Progress::Confirmed, Progress::Confirmed, 0)
            .await
            .unwrap();
        let result = rt.join_child(&claim, "child").await.unwrap();
        assert!(matches!(
            result.state,
            RunState::Terminal(Terminal::Cancelled(_))
        ));
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 0);
        assert_eq!(s.slot_count(OperationKind::Model).await, 0);
        assert_eq!(
            s.account(&account).await.unwrap().reserved,
            Amount::default()
        );
        assert_eq!(rt.join_child(&claim, "child").await.unwrap(), result);
    });
}

#[test]
fn child_join_preserves_failed_outcome_instead_of_promoting_admission_to_success() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (_, claim, account) = started(&s, "failed-child", 1000).await;
        let rt = runtime(
            s.clone(),
            FakeModel::scripted(vec![FakeModel::decision(
                json!({"decision":"insufficient"}),
            )]),
            vec![],
        );
        let child = rt
            .spawn_child(
                &claim,
                "child",
                RunSpec::new("tenant", "child", account.clone(), "child"),
            )
            .await
            .unwrap();
        let cc = s.claim(&child, "worker-child", 0, 100).await.unwrap();
        let failed = rt.drive(&cc).await.unwrap();
        assert!(matches!(
            failed.state,
            RunState::Terminal(Terminal::Failed(_))
        ));
        let adopted = rt.join_child(&claim, "child").await.unwrap();
        assert_eq!(adopted, failed);
        assert!(adopted.completion.is_none());
        assert_eq!(s.account(&account).await.unwrap().used, Amount::new(4, 4));
        assert_eq!(s.slot_count(OperationKind::ChildRun).await, 0);
    });
}

#[test]
fn child_admission_cannot_use_external_intent_dispatch_or_send() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (parent, claim, account) = started(&s, "child-no-external-path", 1000).await;
        let rt = runtime(s.clone(), FakeModel::default(), vec![]);
        let spec = RunSpec::new("tenant", "child", account, "child");
        let request = DispatchRequest::new(
            "child-op",
            OperationKind::ChildRun,
            "child",
            serde_json::to_value(&spec).unwrap(),
            Amount::default(),
        );
        let expected = Error::Invalid("child admission requires admit_child".into());
        assert_eq!(
            s.begin(&claim, request.clone(), 0).await,
            Err(expected.clone())
        );
        assert_eq!(
            rt.dispatch(
                &claim,
                request,
                "child",
                |_| async { panic!("ChildRun must never invoke an external adapter") },
                |_| async { panic!("ChildRun must never use provider lookup") }
            )
            .await,
            Err(expected)
        );
        assert!(s.list_operations(&parent).await.unwrap().is_empty());
        let child = rt.spawn_child(&claim, "child-op", spec).await.unwrap();
        let op = s.operation(&parent, "child-op").await.unwrap().unwrap();
        assert_eq!(op.child_handle().unwrap(), child);
        assert_eq!(s.mark_sent(&claim, &op.id, 0).await, Err(Error::Conflict));
        assert_eq!(
            s.operation(&parent, "child-op")
                .await
                .unwrap()
                .unwrap()
                .send_count,
            0
        );
    });
}
