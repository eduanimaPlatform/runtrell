// SPDX-License-Identifier: Apache-2.0
use crate::*;

/// Token exhaustion must settle before a fresh turn, without invoking partial tools.
pub async fn token_continuation<S: ControlStore>(store: Arc<S>) {
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("t", "token-continuation", account.clone(), "finish");
    spec.continuation.instruction = "Finish with a complete decision".into();
    let run = store.start(spec).await.unwrap();
    let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
    let tool = BusinessTool::new("partial");
    let mut partial = FakeModel::calls(vec![ToolCall {
        id: "partial".into(),
        name: "partial".into(),
        arguments: json!({}),
    }]);
    partial.text = "unfinished".into();
    partial.metadata = json!({"stop_reason":"max_tokens"});
    let model = FakeModel::scripted(vec![
        partial,
        FakeModel::decision(json!({"decision":"keep"})),
    ]);
    let rt = Runtime {
        store: store.clone(),
        model: model.clone(),
        tools: vec![tool.clone()],
        policy: FakePolicy::default(),
        clock: TestClock::default(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        rt.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
    let operations = store.list_operations(&run).await.unwrap();
    assert_eq!(operations.len(), 2);
    assert!(
        operations
            .iter()
            .all(|o| o.settled && o.request.reserve.tokens > 0)
    );
    assert_ne!(operations[0].request.key, operations[1].request.key);
    assert_eq!(store.account(&account).await.unwrap().used.tokens, 8);
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|m| m.text == "Finish with a complete decision")
    );
    assert!(
        !requests[1]
            .messages
            .iter()
            .any(|m| m.call_id.as_deref() == Some("partial"))
    );
}

pub async fn continuation_stops<S: ControlStore>(store: Arc<S>) {
    for mode in ["budget", "limit", "unlimited", "shrink"] {
        let account = store
            .create_account(
                "t",
                Amount::new(
                    if matches!(mode, "budget" | "shrink") {
                        22
                    } else {
                        1000
                    },
                    1000,
                ),
                None,
            )
            .await
            .unwrap();
        let mut spec = RunSpec::new("t", mode, account.clone(), "finish");
        spec.budget = if mode == "shrink" {
            ReservationPolicy::Shrink {
                min_output_budget_tokens: 1,
            }
        } else {
            ReservationPolicy::EndRun
        };
        spec.continuation.max_continuations = (mode == "limit").then_some(0);
        let run = store.start(spec).await.unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        let partial = ModelReply {
            metadata: json!({"stop_reason":"max_tokens"}),
            ..FakeModel::decision(json!({"partial":true}))
        };
        let mut replies = vec![partial; if mode == "unlimited" { 5 } else { 1 }];
        replies.push(FakeModel::decision(json!({"decision":"keep"})));
        let model = FakeModel::scripted(replies);
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let result = rt.drive(&claim).await.unwrap();
        match mode {
            "budget" => assert!(matches!(
                result.state,
                RunState::Terminal(Terminal::BudgetReached(_))
            )),
            "limit" => assert!(
                matches!(result.state, RunState::Terminal(Terminal::Failed(ref code)) if code.contains("MODEL_CONTINUATION_LIMIT"))
            ),
            _ => assert_eq!(result.state, RunState::Terminal(Terminal::Succeeded)),
        }
        let count = {
            let requests = model.requests.lock().unwrap();
            assert_eq!(
                requests.len(),
                match mode {
                    "unlimited" => 6,
                    "shrink" => 2,
                    _ => 1,
                }
            );
            if mode == "shrink" {
                assert!(requests[1].max_output < requests[0].max_output);
            }
            requests.len()
        };
        assert_eq!(
            store.account(&account).await.unwrap().used.tokens,
            count as u64 * 4
        );
    }
}

#[derive(Clone)]
struct ContextModel {
    main: FakeModel,
    summary: FakeModel,
}

struct DeadlineModel {
    inner: FakeModel,
    clock: TestClock,
    estimates: AtomicU64,
}
impl ModelPort for DeadlineModel {
    fn limits(&self) -> ModelLimits {
        self.inner.limits()
    }
    fn input_upper_bound(&self, request: &PreparedModelRequest) -> Result<u64> {
        if self.estimates.fetch_add(1, Ordering::SeqCst) > 0 {
            self.clock.set(10);
        }
        self.inner.input_upper_bound(request)
    }
    async fn invoke(
        &self,
        context: &ToolContext,
        request: PreparedModelRequest,
    ) -> Result<ExternalResult> {
        self.inner.invoke(context, request).await
    }
    async fn lookup(&self, context: &ToolContext, request: PreparedModelRequest) -> Result<Lookup> {
        self.inner.lookup(context, request).await
    }
}
pub async fn continuation_deadline<S: ControlStore>(store: Arc<S>) {
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("t", "continuation-deadline", account.clone(), "finish");
    spec.deadline = Some(10);
    let run = store.start(spec).await.unwrap();
    let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
    let inner = FakeModel::scripted(vec![ModelReply {
        metadata: json!({"stop_reason":"max_tokens"}),
        ..FakeModel::decision(Value::Null)
    }]);
    let clock = TestClock::default();
    let model = DeadlineModel {
        inner: inner.clone(),
        clock: clock.clone(),
        estimates: AtomicU64::new(0),
    };
    let rt = Runtime {
        store: store.clone(),
        model,
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        rt.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::DeadlineReached)
    );
    assert_eq!(inner.requests.lock().unwrap().len(), 1);
    assert_eq!(store.account(&account).await.unwrap().used.tokens, 4);
}
impl ModelPort for ContextModel {
    fn limits(&self) -> ModelLimits {
        ModelLimits {
            context: 100,
            ..self.main.limits()
        }
    }
    fn compaction_limits(&self) -> ModelLimits {
        let mut limits = self.summary.limits();
        limits.model = "summary".into();
        limits.price.as_mut().unwrap().model = "summary".into();
        limits
    }
    fn input_upper_bound(&self, request: &PreparedModelRequest) -> Result<u64> {
        Ok(if request.model == "summary" {
            10
        } else if request
            .messages
            .iter()
            .any(|m| m.text.contains("Continuation summary"))
        {
            20
        } else if request.messages.len() > 1 {
            90
        } else {
            20
        })
    }
    fn recovery(&self) -> Option<(String, u64)> {
        self.main.recovery()
    }
    async fn invoke(
        &self,
        context: &ToolContext,
        request: PreparedModelRequest,
    ) -> Result<ExternalResult> {
        if request.model == "summary" {
            let result = self.summary.invoke(context, request).await?;
            Ok(summary_usage(result))
        } else {
            self.main.invoke(context, request).await
        }
    }
    async fn lookup(&self, context: &ToolContext, request: PreparedModelRequest) -> Result<Lookup> {
        if request.model == "summary" {
            Ok(match self.summary.lookup(context, request).await? {
                Lookup::Found(result) => Lookup::Found(summary_usage(result)),
                other => other,
            })
        } else {
            self.main.lookup(context, request).await
        }
    }
}
fn summary_usage(mut result: ExternalResult) -> ExternalResult {
    if let ExternalResult::Applied { usage, .. } = &mut result {
        usage.model = "summary".into();
    }
    result
}

struct HostValidator {
    mode: &'static str,
}
impl Validator for HostValidator {
    fn validate(&self, run: &RunRecord, candidate: &Value) -> Result<ValidationReceipt> {
        match self.mode {
            "storage" => return Err(Error::Storage),
            "denied" => return Err(Error::PermissionDenied),
            "conflict" => return Err(Error::Conflict),
            _ => (),
        }
        let passed = candidate["approved"] == true;
        let mut diagnostics = schema_diagnostics(
            candidate,
            &json!({"properties":{"approved":{"const":true}},"required":["approved"]}),
        )?;
        for diagnostic in &mut diagnostics {
            diagnostic.code = "HOST_APPROVAL_NEEDED".into();
        }
        Ok(ValidationReceipt {
            hash: digest(candidate),
            basis_revision: run.spec.basis_revision,
            config_version: run.spec.config_version.clone(),
            validator: run.spec.validator.clone(),
            validator_version: run.spec.validator_version.clone(),
            passed,
            repairable: !passed && self.mode == "repair",
            diagnostics,
        })
    }
}
pub async fn receipt_classification<S: ControlStore>(store: Arc<S>) {
    for mode in ["repair", "unrepairable", "storage", "denied", "conflict"] {
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new(
                "t",
                &format!("host-{mode}"),
                account,
                "approve",
            ))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        // Keep exercising host classification while candidates make progress.
        let mut replies: Vec<_> = (0..5)
            .map(|attempt| FakeModel::decision(json!({"approved":false,"attempt":attempt})))
            .collect();
        replies.push(FakeModel::decision(json!({"approved":true})));
        let model = FakeModel::scripted(replies);
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(HostValidator { mode }),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let result = rt.drive(&claim).await;
        match mode {
            "repair" => {
                assert_eq!(
                    result.unwrap().state,
                    RunState::Waiting("validation repair".into())
                );
                for _ in 0..4 {
                    assert_eq!(
                        rt.drive(&claim).await.unwrap().state,
                        RunState::Waiting("validation repair".into())
                    );
                }
                let done = rt.drive(&claim).await.unwrap();
                assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
                let requests = model.requests.lock().unwrap();
                assert_eq!(requests.len(), 6);
                assert!(requests[1].messages.iter().any(
                    |m| m.text.contains("HOST_APPROVAL_NEEDED")
                        && m.text.contains("/approved")
                        && m.text.contains("hint")
                ));
            }
            "unrepairable" | "denied" => assert!(matches!(
                result.unwrap().state,
                RunState::Terminal(Terminal::Failed(_))
            )),
            "storage" => assert_eq!(result.unwrap_err(), Error::Storage),
            "conflict" => assert_eq!(result.unwrap_err(), Error::Conflict),
            _ => unreachable!(),
        }
        if mode != "repair" {
            assert_eq!(model.requests.lock().unwrap().len(), 1);
            let saved = store.get(&run).await.unwrap();
            assert!(saved.checkpoint.unwrap().payload["repair"].is_null());
        }
    }
}

struct TypedTool(BusinessTool);
impl Tool for TypedTool {
    fn descriptor(&self) -> ToolDescriptor {
        let mut descriptor = self.0.descriptor();
        descriptor.input_schema =
            json!({"type":"object","properties":{"value":{"type":"integer"}},"required":["value"]});
        descriptor
    }
    async fn invoke(&self, context: &ToolContext, input: Value) -> Result<ExternalResult> {
        self.0.invoke(context, input).await
    }
    async fn lookup(&self, context: &ToolContext, input: Value) -> Result<Lookup> {
        self.0.lookup(context, input).await
    }
}
pub async fn tool_repairs<S: ControlStore>(store: Arc<S>) {
    for limited in [false, true] {
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new(
                "t",
                &format!("tool-repair-{limited}"),
                account,
                "use typed tool",
            ))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        let mut replies: Vec<_> = (0..5)
            .map(|attempt| {
                FakeModel::calls(vec![ToolCall {
                    id: format!("bad-{attempt}"),
                    name: "typed".into(),
                    arguments: json!({"value":format!("bad-{attempt}")}),
                }])
            })
            .collect();
        replies.push(FakeModel::calls(vec![ToolCall {
            id: "malformed".into(),
            name: "typed".into(),
            arguments: json!([1]),
        }]));
        replies.push(FakeModel::calls(vec![ToolCall {
            id: "good".into(),
            name: "typed".into(),
            arguments: json!({"value":1}),
        }]));
        replies.push(FakeModel::decision(json!({"decision":"keep"})));
        let model = FakeModel::scripted(replies);
        let tool = BusinessTool::new("typed");
        let validator = runtrell::assembly::ValidationChain {
            validators: vec![],
            schema: json!({}),
            repair: RepairPolicy {
                max_repairs: limited.then_some(0),
                max_elapsed: None,
                ..RepairPolicy::default()
            },
            repairable_codes: Default::default(),
        };
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: vec![TypedTool(tool.clone())],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(validator),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let result = rt.drive(&claim).await.unwrap();
        if limited {
            assert!(
                matches!(result.state, RunState::Terminal(Terminal::Failed(ref code)) if code.contains("VALIDATION_REPAIR_LIMIT"))
            );
            assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
            assert_eq!(model.requests.lock().unwrap().len(), 1);
        } else {
            assert_eq!(result.state, RunState::Terminal(Terminal::Succeeded));
            assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
            let requests = model.requests.lock().unwrap();
            let feedback = requests[1]
                .messages
                .iter()
                .find(|m| m.role == "tool")
                .unwrap();
            assert_eq!(feedback.data["diagnostics"][0]["path"], "/value");
            assert!(
                !feedback.data["diagnostics"][0]["cause"]
                    .as_str()
                    .unwrap()
                    .is_empty()
            );
            assert!(
                !feedback.data["diagnostics"][0]["hint"]
                    .as_str()
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

/// Both stores refer to the same durable state; Pg supplies independent pools.
pub async fn automatic_compaction<S: ControlStore>(store: Arc<S>, reopened: Arc<S>) {
    use runtrell::assembly::ValidationChain;
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new(
        "t",
        "automatic-context",
        account.clone(),
        "preserve the task",
    );
    spec.compaction = Some(CompactionPolicy {
        min_remaining_tokens: 20,
    });
    let run = store.start(spec).await.unwrap();
    let claim = store.claim(&run, "first", 0, 100).await.unwrap();
    let mut calls = FakeModel::calls(vec![ToolCall {
        id: "lookup".into(),
        name: "lookup".into(),
        arguments: json!({}),
    }]);
    calls.metadata = json!({"continuation":{"opaque":"retained"}});
    let main = FakeModel::scripted(vec![
        calls,
        FakeModel::decision(json!({"answer":0})),
        FakeModel::decision(json!({"answer":2})),
    ]);
    let summary = FakeModel::scripted(vec![ModelReply {
        text: "Lookup completed; correct answer still needed".into(),
        ..FakeModel::decision(Value::Null)
    }]);
    summary.lose_response.store(true, Ordering::SeqCst);
    let model = ContextModel {
        main: main.clone(),
        summary: summary.clone(),
    };
    let validator = Arc::new(ValidationChain {
        validators: vec![],
        schema: json!({"type":"object","properties":{"answer":{"type":"integer","minimum":1}},"required":["answer"]}),
        repair: RepairPolicy::default(),
        repairable_codes: Default::default(),
    });
    let rt = Runtime {
        store: store.clone(),
        model: model.clone(),
        tools: vec![BusinessTool::new("lookup")],
        policy: FakePolicy::default(),
        clock: TestClock::default(),
        validator: validator.clone(),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let pending = rt.drive(&claim).await.unwrap();
    assert_eq!(
        pending.state,
        RunState::Waiting("external reconciliation".into())
    );
    assert!(pending.checkpoint.unwrap().payload["context"]["summary"].is_null());
    let waiting = rt.drive(&claim).await.unwrap();
    assert_eq!(waiting.state, RunState::Waiting("validation repair".into()));
    let cp = waiting.checkpoint.unwrap();
    assert!(
        cp.payload["context"]["summary"]
            .as_str()
            .unwrap()
            .contains("Lookup completed")
    );
    assert!(
        cp.payload["context"]["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["value"]["call_id"] == "lookup")
    );
    assert!(
        cp.payload["context"]["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["value"]["text"] == "preserve the task")
    );
    assert!(
        cp.messages
            .iter()
            .any(|m| m.role == "provider" && m.data["continuation"]["opaque"] == "retained")
    );
    assert!(
        cp.messages
            .iter()
            .any(|m| m.role == "tool" && m.call_id.as_deref() == Some("lookup"))
    );
    assert_eq!(
        cp.payload["repair"]["receipt"]["diagnostics"][0]["path"],
        "/answer"
    );
    assert_eq!(summary.requests.lock().unwrap()[0].model, "summary");
    drop(rt);
    let claim = reopened.claim(&run, "second", 101, 100).await.unwrap();
    let clock = TestClock::default();
    clock.set(101);
    let rt = Runtime {
        store: reopened.clone(),
        model,
        tools: vec![BusinessTool::new("lookup")],
        policy: FakePolicy::default(),
        clock,
        validator,
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        rt.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    assert!(
        main.requests.lock().unwrap()[2]
            .messages
            .iter()
            .any(|m| m.text.contains("Continuation summary"))
    );
    assert_eq!(summary.requests.lock().unwrap().len(), 1);
    let operations = reopened.list_operations(&run).await.unwrap();
    assert_eq!(
        operations
            .iter()
            .filter(|o| o.request.kind == OperationKind::Compaction)
            .count(),
        1
    );
    assert!(operations.iter().all(|o| o.settled));
    assert_eq!(reopened.account(&account).await.unwrap().used.tokens, 16);
    let other = reopened
        .start(RunSpec::new("t", "separate-run", account, "new task"))
        .await
        .unwrap();
    assert!(reopened.get(&other).await.unwrap().checkpoint.is_none());
    let other_claim = reopened.claim(&other, "other", 101, 100).await.unwrap();
    let original = reopened.get(&run).await.unwrap().checkpoint.unwrap();
    let revision = reopened.get(&other).await.unwrap().revision;
    reopened
        .checkpoint(&other_claim, revision, original, 101)
        .await
        .unwrap();
    assert!(
        matches!(rt.drive(&other_claim).await.unwrap().state, RunState::Terminal(Terminal::Failed(ref code)) if code.contains("permission denied"))
    );
    assert_eq!(main.requests.lock().unwrap().len(), 3);
}
