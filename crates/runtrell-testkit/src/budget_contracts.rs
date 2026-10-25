// SPDX-License-Identifier: Apache-2.0
//! Adapter-independent budget guarantees and selectable policy contracts.
use runtrell::*;
use serde_json::json;

pub async fn policies<S: ControlStore>(s: &S) {
    let policy = AccountPolicy {
        tokens: false,
        continue_with_unknown: true,
        finalize_after_stop: true,
        warning_percentages: vec![25, 75],
        quantity_limits: [("jobs".into(), 2)].into(),
        ..AccountPolicy::default()
    };
    let a = s
        .create_account_with_policy("t", Amount::new(0, 100), None, policy)
        .await
        .unwrap();
    let mut spec = RunSpec::new("t", "policies", a.clone(), "judge");
    spec.budget = ReservationPolicy::EndRun;
    let h = s.start(spec).await.unwrap();
    let c = s.claim(&h, "w", 0, 100).await.unwrap();
    let mut req = DispatchRequest::new(
        "unknown",
        OperationKind::Tool,
        "job",
        json!({}),
        Amount::new(0, 60),
    );
    req.quantities.insert("jobs".into(), 1);
    let op = s.begin(&c, req, 0).await.unwrap();
    s.record(&c, &op.id, ExternalResult::Unknown, 0)
        .await
        .unwrap();
    let run = s.get(&h).await.unwrap();
    let value = json!({"decision":"done"});
    let receipt = crate::DecisionValidator::default()
        .validate(&run, &value)
        .unwrap();
    let completion = crate::DecisionFinalizer
        .finalize(&run, value, receipt)
        .unwrap();
    assert_eq!(
        s.finalize(&c, completion, 0).await,
        Err(Error::UsageUnknown)
    );
    assert_eq!(s.remaining(&c, 0).await.unwrap(), Amount::new(u64::MAX, 40));
    let mut next = DispatchRequest::new(
        "next",
        OperationKind::Tool,
        "job",
        json!({}),
        Amount::new(0, 40),
    );
    next.quantities.insert("jobs".into(), 1);
    let second = s.begin(&c, next, 0).await.unwrap();
    let mut usage = Usage::measured(0, 0, 30);
    usage.quantities.insert("jobs".into(), 1);
    let result = ExternalResult::Applied {
        output: json!({}),
        usage,
        external_id: None,
    };
    s.record(&c, &second.id, result.clone(), 0).await.unwrap();
    s.record(&c, &second.id, result, 0).await.unwrap();
    assert_eq!(s.account(&a).await.unwrap().used, Amount::new(0, 30));
    assert_eq!(s.warnings(&c, 0).await.unwrap()[0].cost_percent, Some(25));
    assert!(s.warnings(&c, 0).await.unwrap().is_empty());
    let third = DispatchRequest::new(
        "denied",
        OperationKind::Tool,
        "job",
        json!({}),
        Amount::new(0, 11),
    );
    assert_eq!(s.begin(&c, third, 0).await, Err(Error::ReservationDenied));
    assert!(!s.account(&a).await.unwrap().exhausted);
    assert_eq!(s.account(&a).await.unwrap().reserved, Amount::new(0, 60));
}

pub fn image_price() {
    let p = Price {
        model: "m".into(),
        input: 1,
        cached: 2,
        image_input: 3,
        cached_image_input: 5,
        output: 5,
        image_output: 6,
        reasoning: 7,
    };
    let mut u = Usage::measured(10, 10, 1);
    u.cached = Some(6);
    u.image_input = Some(7);
    u.cached_image_input = Some(4);
    u.image_output = Some(2);
    u.reasoning = Some(3);
    // Reasoning and image output both carry their own rate and their overlap is
    // unreported, so the attempt cannot be priced.
    assert_eq!(p.cost(&u), Err(Error::UsageUnknown));
    u.reasoning = Some(0);
    assert_eq!(p.cost(&u), Ok(1)); // round once across all disjoint lines
    u.cached_image_input = Some(2);
    assert!(matches!(p.cost(&u), Err(Error::Invalid(_))));
    u.cached_image_input = None;
    assert_eq!(p.cost(&u), Err(Error::UsageUnknown));
    assert_eq!(
        p.cost(&Usage::measured(10, 10, 0)),
        Err(Error::UsageUnknown)
    );
    // Equal rates require only measured totals; a known crossing still validates.
    let uniform = Price {
        model: "m".into(),
        input: 1,
        cached: 1,
        image_input: 1,
        cached_image_input: 1,
        output: 1,
        reasoning: 1,
        image_output: 1,
    };
    let mut crossing_only = Usage::measured(10, 10, 1);
    crossing_only.cached_image_input = Some(4);
    assert_eq!(uniform.cost(&crossing_only), Ok(1));
    let physical = ModelLimits {
        model: "m".into(),
        context: 100,
        max_output: 10,
        min_max_output_tokens: 3,
        price: Some(uniform),
        timeout: 1,
    };
    assert_eq!(physical.cap(1, Amount::new(100, 1)).unwrap().0, 10); // no premature input rounding
    assert_eq!(
        physical.cap(1, Amount::new(3, 1)),
        Err(Error::ReservationDenied)
    );
    assert_eq!(Usage::default().amount(), Err(Error::UsageUnknown));
    assert_eq!(Usage::measured(0, 0, 0).amount(), Ok(Amount::default()));
    assert_eq!(Usage::measured(0, 0, 7).amount(), Ok(Amount::new(0, 7)));
}

pub async fn settlement_and_finalization<S: ControlStore>(s: &S) {
    for allow in [false, true] {
        let a = s
            .create_account_with_policy(
                "t",
                Amount::new(10, 10),
                None,
                AccountPolicy {
                    finalize_after_stop: allow,
                    ..AccountPolicy::default()
                },
            )
            .await
            .unwrap();
        let h = s
            .start(RunSpec::new(
                "t",
                &format!("finalize-{allow}"),
                a.clone(),
                "judge",
            ))
            .await
            .unwrap();
        let c = s.claim(&h, "w", 0, 100).await.unwrap();
        let op = s
            .begin(
                &c,
                DispatchRequest::new(
                    "last",
                    OperationKind::Tool,
                    "job",
                    json!({}),
                    Amount::new(10, 10),
                ),
                0,
            )
            .await
            .unwrap();
        s.record(
            &c,
            &op.id,
            ExternalResult::Applied {
                output: json!({}),
                usage: Usage::measured(10, 0, 10),
                external_id: None,
            },
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            s.finalization_allowed(&c, 0).await,
            if allow {
                Ok(())
            } else {
                Err(Error::BudgetAtLimit)
            }
        );
        let run = s.get(&h).await.unwrap();
        let value = json!({"decision":"done"});
        let receipt = crate::DecisionValidator::default()
            .validate(&run, &value)
            .unwrap();
        let completion = crate::DecisionFinalizer
            .finalize(&run, value, receipt)
            .unwrap();
        assert_eq!(
            s.finalize(&c, completion, 0).await,
            if allow {
                Ok(())
            } else {
                Err(Error::BudgetAtLimit)
            }
        );
        assert_eq!(
            s.remaining(&c, 0).await,
            if allow {
                Err(Error::Closed)
            } else {
                Err(Error::BudgetAtLimit)
            }
        );
    }
    let a = s
        .create_account("t", Amount::new(100, 100), None)
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("t", "price-unknown", a.clone(), "judge"))
        .await
        .unwrap();
    let c = s.claim(&h, "w", 0, 100).await.unwrap();
    let mut req = DispatchRequest::new(
        "price",
        OperationKind::Model,
        "m",
        json!({}),
        Amount::new(20, 10),
    );
    req.price = Some(Price {
        model: "m".into(),
        input: 1,
        cached: 2,
        image_input: 3,
        cached_image_input: 5,
        output: 6,
        reasoning: 7,
        image_output: 8,
    });
    let op = s.begin(&c, req, 0).await.unwrap();
    let mut usage = Usage::measured(10, 10, 1);
    usage.model = "m".into();
    let result = |usage| ExternalResult::Applied {
        output: json!({}),
        usage,
        external_id: Some("one-bill".into()),
    };
    let mut unmeasured_cost = Usage::measured(0, 0, 0);
    unmeasured_cost.model = "m".into();
    unmeasured_cost.micro_usd = None;
    assert_eq!(
        s.record(&c, &op.id, result(unmeasured_cost), 0).await,
        Err(Error::UsageUnknown)
    );
    assert_eq!(
        s.record(&c, &op.id, result(usage.clone()), 0).await,
        Err(Error::UsageUnknown)
    );
    assert!(!s.operation(&h, "price").await.unwrap().unwrap().settled);
    assert_eq!(s.account(&a).await.unwrap().reserved, Amount::new(20, 10));
    usage.cached = Some(6);
    usage.image_input = Some(7);
    usage.cached_image_input = Some(4);
    usage.reasoning = Some(1);
    usage.image_output = Some(2);
    // Overlap between reasoning and image output is unreported; with both billed
    // at their own rates the attempt cannot be priced and stays reserved.
    assert_eq!(
        s.record(&c, &op.id, result(usage.clone()), 0).await,
        Err(Error::UsageUnknown)
    );
    assert_eq!(s.account(&a).await.unwrap().reserved, Amount::new(20, 10));
    usage.reasoning = Some(0);
    s.record(&c, &op.id, result(usage.clone()), 0)
        .await
        .unwrap();
    s.record(&c, &op.id, result(usage), 0).await.unwrap();
    assert_eq!(s.account(&a).await.unwrap().used, Amount::new(20, 1));
    assert_eq!(s.account(&a).await.unwrap().unknown, 0);
    // The logical key remains fixed, but each charged failed attempt is a new bill.
    let request = DispatchRequest::new(
        "retry",
        OperationKind::Tool,
        "job",
        json!({}),
        Amount::new(0, 5),
    );
    let first = s.begin(&c, request.clone(), 0).await.unwrap();
    let failed = ExternalResult::Failed {
        code: "busy".into(),
        usage: Usage::measured(0, 0, 3),
        retry_after: Some(1),
    };
    s.record(&c, &first.id, failed.clone(), 0).await.unwrap();
    s.record(&c, &first.id, failed.clone(), 0).await.unwrap();
    let second = s
        .begin(
            &c,
            DispatchRequest {
                attempt: 2,
                ..request
            },
            0,
        )
        .await
        .unwrap();
    s.record(&c, &second.id, failed, 0).await.unwrap();
    assert_eq!(s.account(&a).await.unwrap().used, Amount::new(20, 7));
}

pub async fn driver_policies<S: ControlStore>(s: std::sync::Arc<S>) {
    for (i, policy) in [
        ReservationPolicy::EndAccount,
        ReservationPolicy::EndRun,
        ReservationPolicy::Shrink {
            min_output_budget_tokens: 1,
        },
        ReservationPolicy::Shrink {
            min_output_budget_tokens: 6,
        },
        ReservationPolicy::Shrink {
            min_output_budget_tokens: 2,
        },
        ReservationPolicy::Wait { until: 10 },
    ]
    .into_iter()
    .enumerate()
    {
        let a = s
            .create_account("t", Amount::new(10, 10), None)
            .await
            .unwrap();
        let mut spec = RunSpec::new("t", &format!("driver-{i}"), a.clone(), "judge");
        spec.budget = policy.clone();
        let h = s.start(spec).await.unwrap();
        let c = s.claim(&h, "w", 0, 100).await.unwrap();
        let model = crate::FakeModel::scripted(vec![crate::FakeModel::decision(
            json!({"decision":"done"}),
        )]);
        let rt = Runtime {
            store: s.clone(),
            model: model.clone(),
            tools: Vec::<crate::BusinessTool>::new(),
            policy: crate::FakePolicy::default(),
            clock: crate::TestClock::default(),
            validator: crate::DecisionValidator::default(),
            finalizer: crate::DecisionFinalizer,
        };
        let r = rt.drive(&c).await.unwrap();
        match policy {
            ReservationPolicy::Shrink {
                min_output_budget_tokens: 1 | 2,
            } => {
                assert_eq!(r.state, RunState::Terminal(Terminal::Succeeded));
                assert_eq!(model.requests.lock().unwrap()[0].max_output, 5);
                // Actual output is one token, valid even with a quality floor of two.
                assert_eq!(s.account(&a).await.unwrap().used, Amount::new(4, 4));
            }
            ReservationPolicy::Wait { .. } => {
                assert_eq!(r.state, RunState::Waiting("budget".into()));
                // This suite uses default capacity. Every per-run slot must be
                // available while waiting; idempotent reacquisition of the active
                // key alone would not prove that its original slot was released.
                let mut probes = Vec::new();
                for index in 0..Capacity::default().run[&OperationKind::Run] {
                    let key = format!("probe:{}:{}:{index}", c.run.tenant, c.run.id);
                    s.acquire(SlotRequest {
                        key: key.clone(),
                        tenant: c.run.tenant.clone(),
                        run: c.run.id.clone(),
                        kind: OperationKind::Run,
                    })
                    .await
                    .unwrap();
                    probes.push(key);
                }
                for key in probes {
                    s.release_slot(&key).await.unwrap();
                }
                assert_eq!(s.account(&a).await.unwrap().reserved, Amount::default());
                assert!(s.list_operations(&h).await.unwrap().is_empty());
                rt.clock.set(10);
                assert!(matches!(
                    rt.drive(&c).await.unwrap().state,
                    RunState::Terminal(Terminal::BudgetReached(_))
                ));
                assert!(model.requests.lock().unwrap().is_empty());
            }
            _ => {
                assert!(matches!(
                    r.state,
                    RunState::Terminal(Terminal::BudgetReached(_))
                ));
                assert!(model.requests.lock().unwrap().is_empty());
            }
        }
        assert_eq!(
            s.account(&a).await.unwrap().exhausted,
            policy == ReservationPolicy::EndAccount
        );
    }
}

pub async fn scheduling<S: ControlStore>(s: &S) {
    for (key, tenant, priority, weight) in [
        ("low", "a", 0, 1),
        ("high1", "a", 10, 1),
        ("high2", "a", 10, 1),
        ("other", "b", -1, 1),
    ] {
        s.enqueue(QueueEntry {
            key: key.into(),
            tenant: tenant.into(),
            scheduling: SchedulingPolicy { priority, weight },
            deadline: 100,
        })
        .await
        .unwrap();
    }
    assert_eq!(s.dequeue(0).await.unwrap().unwrap().key, "high1");
    assert_eq!(s.dequeue(0).await.unwrap().unwrap().key, "other");
    // Tenant rotation remains primary; exhausted priority weight admits oldest low entry.
    assert_eq!(s.dequeue(0).await.unwrap().unwrap().key, "low");
    assert_eq!(s.dequeue(0).await.unwrap().unwrap().key, "high2");
}

pub async fn axes<S: ControlStore>(s: &S) {
    for (i, (tokens, cost)) in [(true, false), (false, true), (false, false)]
        .into_iter()
        .enumerate()
    {
        let a = s
            .create_account_with_policy(
                "t",
                Amount::new(10, 10),
                None,
                AccountPolicy {
                    tokens,
                    cost,
                    warning_percentages: vec![],
                    ..AccountPolicy::default()
                },
            )
            .await
            .unwrap();
        let h = s
            .start(RunSpec::new("t", &format!("axes-{i}"), a.clone(), "judge"))
            .await
            .unwrap();
        let c = s.claim(&h, "w", 0, 100).await.unwrap();
        let reserve = Amount::new(if tokens { 5 } else { 20 }, if cost { 5 } else { 20 });
        let op = s
            .begin(
                &c,
                DispatchRequest::new("one", OperationKind::Tool, "job", json!({}), reserve),
                0,
            )
            .await
            .unwrap();
        s.record(
            &c,
            &op.id,
            ExternalResult::Applied {
                output: json!({}),
                usage: Usage::measured(reserve.tokens, 0, reserve.micro_usd),
                external_id: None,
            },
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            s.remaining(&c, 0).await.unwrap(),
            Amount::new(
                if tokens { 5 } else { u64::MAX },
                if cost { 5 } else { u64::MAX }
            )
        );
        assert!(s.warnings(&c, 0).await.unwrap().is_empty());
    }
    let parent = s
        .create_account_with_policy(
            "*",
            Amount::new(0, 0),
            None,
            AccountPolicy {
                tokens: false,
                cost: false,
                quantity_limits: [("jobs".into(), 1)].into(),
                ..AccountPolicy::default()
            },
        )
        .await
        .unwrap();
    let a = s
        .create_account("t", Amount::new(100, 100), Some(parent.clone()))
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("t", "quantity-parent", a.clone(), "judge"))
        .await
        .unwrap();
    let c = s.claim(&h, "w", 0, 100).await.unwrap();
    let mut req = DispatchRequest::new(
        "quantity",
        OperationKind::Tool,
        "job",
        json!({}),
        Amount::default(),
    );
    req.quantities.insert("jobs".into(), 2);
    assert_eq!(
        s.begin(&c, req.clone(), 0).await,
        Err(Error::ReservationDenied)
    );
    assert_eq!(
        s.account(&parent)
            .await
            .unwrap()
            .quantity_reserved
            .get("jobs"),
        None
    );
    // Use a fresh request account because the default denial policy stops the first.
    let a = s
        .create_account("t", Amount::new(100, 100), Some(parent.clone()))
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("t", "quantity-success", a, "judge"))
        .await
        .unwrap();
    let c = s.claim(&h, "w", 0, 100).await.unwrap();
    req.quantities.insert("jobs".into(), 1);
    let op = s.begin(&c, req, 0).await.unwrap();
    let incomplete = ExternalResult::Applied {
        output: json!({}),
        usage: Usage::measured(0, 0, 0),
        external_id: None,
    };
    assert_eq!(
        s.record(&c, &op.id, incomplete, 0).await,
        Err(Error::UsageUnknown)
    );
    assert_eq!(
        s.account(&parent).await.unwrap().quantity_reserved["jobs"],
        1
    );
    let mut usage = Usage::measured(0, 0, 0);
    usage.quantities.insert("jobs".into(), 1);
    s.record(
        &c,
        &op.id,
        ExternalResult::Applied {
            output: json!({}),
            usage,
            external_id: None,
        },
        0,
    )
    .await
    .unwrap();
    assert_eq!(s.account(&parent).await.unwrap().quantity_used["jobs"], 1);
}

struct QuantityJob;
impl Tool for QuantityJob {
    fn descriptor(&self) -> ToolDescriptor {
        let mut d = crate::BusinessTool::new("quantity_job").descriptor();
        d.reserve = Amount::new(0, 7);
        d.quantities.insert("jobs".into(), 1);
        d
    }
    async fn invoke(&self, _: &ToolContext, _: serde_json::Value) -> Result<ExternalResult> {
        let mut usage = Usage::measured(0, 0, 7);
        usage.quantities.insert("jobs".into(), 1);
        Ok(ExternalResult::Applied {
            output: json!({}),
            usage,
            external_id: None,
        })
    }
    async fn lookup(&self, _: &ToolContext, _: serde_json::Value) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}

pub async fn quantity_tool<S: ControlStore>(s: std::sync::Arc<S>) {
    let a = s
        .create_account_with_policy(
            "t",
            Amount::new(0, 100),
            None,
            AccountPolicy {
                tokens: false,
                quantity_limits: [("jobs".into(), 2)].into(),
                ..AccountPolicy::default()
            },
        )
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("t", "quantity-tool", a.clone(), "judge"))
        .await
        .unwrap();
    let c = s.claim(&h, "w", 0, 100).await.unwrap();
    let rt = Runtime {
        store: s.clone(),
        model: crate::FakeModel::scripted(vec![
            crate::FakeModel::calls(vec![ToolCall {
                id: "job".into(),
                name: "quantity_job".into(),
                arguments: json!({}),
            }]),
            crate::FakeModel::decision(json!({"decision":"done"})),
        ]),
        tools: vec![QuantityJob],
        policy: crate::FakePolicy::default(),
        clock: crate::TestClock::default(),
        validator: crate::DecisionValidator::default(),
        finalizer: crate::DecisionFinalizer,
    };
    assert_eq!(
        rt.drive(&c).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    let balance = s.account(&a).await.unwrap();
    assert_eq!(balance.used, Amount::new(8, 15));
    assert_eq!(balance.quantity_used["jobs"], 1);
    assert_eq!(balance.quantity_reserved["jobs"], 0);
    let op = s.operation(&h, "tool:1:job").await.unwrap().unwrap();
    assert_eq!(op.request.quantities["jobs"], 1);
}
