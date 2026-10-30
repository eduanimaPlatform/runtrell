// SPDX-License-Identifier: Apache-2.0
use crate::*;
use runtrell::{data::*, file_tools::*};
use std::{future::Future, sync::atomic::AtomicUsize, task::Poll};

struct ClassifiedFailure {
    mode: &'static str,
    clock: TestClock,
}
impl Tool for ClassifiedFailure {
    fn descriptor(&self) -> ToolDescriptor {
        let mut descriptor = BusinessTool::new("failure").descriptor();
        if self.mode == "output_limit" {
            descriptor.max_output_bytes = 1;
        }
        descriptor
    }
    async fn invoke(&self, _: &ToolContext, _: Value) -> Result<ExternalResult> {
        if self.mode == "unknown" {
            return Ok(ExternalResult::Unknown);
        }
        if self.mode == "deadline" {
            self.clock.set(1);
        }
        Ok(ExternalResult::Failed {
            physical_evidence: None,
            code: self.mode.into(),
            usage: Usage::measured(0, 0, 0),
            retry_after: (self.mode == "retry").then_some(1),
            repair_hint: matches!(self.mode, "retry" | "output_limit" | "deadline")
                .then(|| "Safe correction".into()),
        })
    }
    async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}

pub async fn failure_boundaries<S: ControlStore>(store: Arc<S>) {
    for mode in [
        "storage",
        "permission",
        "unclassified",
        "unknown",
        "retry",
        "output_limit",
        "deadline",
    ] {
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let mut spec = RunSpec::new("t", &format!("tool-boundary-{mode}"), account, "act");
        if mode == "deadline" {
            spec.deadline = Some(1);
        }
        let run = store.start(spec).await.unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        let clock = TestClock::default();
        let model = FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "failure".into(),
                name: "failure".into(),
                arguments: json!({}),
            }]),
            FakeModel::decision(json!({"decision":"keep"})),
        ]);
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: vec![ClassifiedFailure {
                mode,
                clock: clock.clone(),
            }],
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let done = rt.drive(&claim).await.unwrap();
        match mode {
            "unknown" | "retry" => assert!(matches!(done.state, RunState::Waiting(_))),
            "deadline" => assert_eq!(done.state, RunState::Terminal(Terminal::DeadlineReached)),
            _ => assert!(matches!(
                done.state,
                RunState::Terminal(Terminal::Failed(_))
            )),
        }
        assert_eq!(model.requests.lock().unwrap().len(), 1, "{mode}");
        assert!(
            done.checkpoint.unwrap().payload["tool_repair"].is_null(),
            "{mode}"
        );
    }
}

struct SaveThenPause<S> {
    store: Arc<S>,
    inner: FileTool,
    saved: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}
impl<S: ControlStore> Tool for SaveThenPause<S> {
    fn descriptor(&self) -> ToolDescriptor {
        self.inner.descriptor()
    }
    async fn invoke(&self, context: &ToolContext, input: Value) -> Result<ExternalResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = self.inner.invoke(context, input).await?;
        if !self.saved.load(Ordering::SeqCst) {
            self.store
                .record(&context.claim, &context.operation, result, 0)
                .await?;
            self.saved.store(true, Ordering::SeqCst);
            // Model a worker disappearing after durable record, before protocol answer.
            return std::future::pending().await;
        }
        Ok(result)
    }
    async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
        panic!("a settled file operation must use its saved result")
    }
}

/// Pg passes an independent connection pool; Memory shares the ledger.
pub async fn saved_failure_recovery<S: ControlStore>(store: Arc<S>, reopened: Arc<S>) {
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new("t", "saved-tool-failure", account, "edit"))
        .await
        .unwrap();
    let claim = store.claim(&run, "first", 0, 100).await.unwrap();
    let data = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner: run.clone(),
        agent: "main".into(),
    });
    let file = data
        .write(&access, "a", None, b"alpha beta alpha")
        .await
        .unwrap();
    let call = |old: &str| {
        FakeModel::calls(vec![ToolCall {
            id: old.into(),
            name: "files.exact_replace".into(),
            arguments: json!({"expected_revision":1,"edits":[{"path":"a","expected_digest":file.hash,"old_text":old,"new_text":"gamma"}]}),
        }])
    };
    let model = FakeModel::scripted(vec![
        call("alpha"),
        call("alpha beta"),
        FakeModel::decision(json!({"decision":"keep"})),
    ]);
    let saved = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut rt = Runtime {
        store: store.clone(),
        model,
        tools: vec![SaveThenPause {
            store: store.clone(),
            inner: FileTool {
                store: data.clone(),
                access: access.clone(),
                action: FileAction::ExactReplace,
            },
            saved: saved.clone(),
            calls: calls.clone(),
        }],
        policy: FakePolicy::default(),
        clock: TestClock::default(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let mut driving = Box::pin(rt.drive(&claim));
    std::future::poll_fn(|cx| {
        assert!(driving.as_mut().poll(cx).is_pending());
        if saved.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    drop(driving);
    let before = reopened.get(&run).await.unwrap();
    assert!(before.checkpoint.as_ref().unwrap().payload["tool_repair"].is_null());
    let output = json!({"decision":"keep"});
    let receipt = rt.validator.validate(&before, &output).unwrap();
    let completion = rt.finalizer.finalize(&before, output, receipt).unwrap();
    // A hint alone cannot bypass the completion guard before model delivery.
    assert_eq!(
        reopened.finalize(&claim, completion, 0).await.unwrap_err(),
        Error::UsageUnknown
    );
    rt.store = reopened.clone();
    let recovered = rt.drive(&claim).await.unwrap();
    assert_eq!(recovered.state, RunState::Terminal(Terminal::Succeeded));
    assert_eq!(
        recovered.checkpoint.unwrap().payload["tool_repair"]["count"],
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(data.snapshot(&access).await.unwrap().revision, 2);
    assert_eq!(
        rt.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let operations = reopened.list_operations(&run).await.unwrap();
    assert!(operations.iter().all(|op| op.settled && op.send_count == 1));
}

/// The same deterministic repair scenario runs against both control stores.
pub async fn file_repairs<S: ControlStore>(store: Arc<S>) {
    for code in [
        "MULTIPLE_MATCHES",
        "NO_MATCH",
        "CONFLICT_REVISION",
        "CONFLICT_DIGEST",
        "NOT_FOUND",
        "SEARCH_LIMIT",
        "SEARCH_BYTES",
    ] {
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new("t", code, account, "edit a file"))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        let data = DataStore::memory(StoragePolicy::default());
        let access = Access::owner(Namespace {
            owner: run.clone(),
            agent: "main".into(),
        });
        let file = data
            .write(&access, "a", None, b"alpha beta alpha")
            .await
            .unwrap();
        let search = code.starts_with("SEARCH_");
        let good = if search {
            json!({"query":"alpha","limit":50,"max_bytes":15000})
        } else {
            json!({"expected_revision":1,"edits":[{"path":"a","expected_digest":file.hash,"old_text":"alpha beta","new_text":"gamma"}]})
        };
        let mut bad = good.clone();
        match code {
            "MULTIPLE_MATCHES" => bad["edits"][0]["old_text"] = json!("alpha"),
            "NO_MATCH" => bad["edits"][0]["old_text"] = json!("private-missing-text"),
            "CONFLICT_REVISION" => bad["expected_revision"] = json!(0),
            "CONFLICT_DIGEST" => bad["edits"][0]["expected_digest"] = json!("stale"),
            "NOT_FOUND" => bad["edits"][0]["path"] = json!("missing"),
            "SEARCH_LIMIT" => bad["query"] = json!(""),
            "SEARCH_BYTES" => bad["query"] = json!("é".repeat(MAX_SEARCH_QUERY_BYTES / 2 + 1)),
            _ => unreachable!(),
        }
        let name = if search {
            "files.search"
        } else {
            "files.exact_replace"
        };
        let model = FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "bad".into(),
                name: name.into(),
                arguments: bad,
            }]),
            // Reuse the provider call ID on another turn: operation keys must differ.
            FakeModel::calls(vec![ToolCall {
                id: "bad".into(),
                name: name.into(),
                arguments: good,
            }]),
            FakeModel::decision(json!({"decision":"keep"})),
        ]);
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: vec![FileTool {
                store: data.clone(),
                access: access.clone(),
                action: if search {
                    FileAction::Search
                } else {
                    FileAction::ExactReplace
                },
            }],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let done = rt.drive(&claim).await.unwrap();
        assert_eq!(
            done.state,
            RunState::Terminal(Terminal::Succeeded),
            "{code}"
        );
        assert_eq!(
            data.snapshot(&access).await.unwrap().revision,
            if search { 1 } else { 2 }
        );
        let feedback = model.requests.lock().unwrap()[1]
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .unwrap()
            .data
            .clone();
        assert_eq!(
            feedback["code"],
            if code == "SEARCH_LIMIT" {
                "TOOL_INPUT_INVALID"
            } else if code == "SEARCH_BYTES" {
                "SEARCH_LIMIT"
            } else if code.starts_with("CONFLICT_") {
                "CONFLICT"
            } else {
                code
            }
        );
        if code == "SEARCH_LIMIT" {
            assert_eq!(feedback["diagnostics"][0]["path"], "/query");
            assert_eq!(feedback["diagnostics"][0]["code"], "SCHEMA_VALIDATION");
        } else {
            assert_eq!(feedback["repairable"], true);
            assert!(!feedback["hint"].as_str().unwrap().is_empty());
        }
        assert!(!feedback.to_string().contains("private-missing-text"));
        assert!(!feedback.to_string().contains("alpha"));
        let cp = done.checkpoint.unwrap();
        assert_eq!(cp.payload["tool_repair"]["count"], 1);
        let operations = store.list_operations(&run).await.unwrap();
        let tools: Vec<_> = operations
            .iter()
            .filter(|op| op.request.kind == OperationKind::Tool)
            .collect();
        assert_eq!(tools.len(), if code == "SEARCH_LIMIT" { 1 } else { 2 });
        assert!(
            tools
                .iter()
                .all(|op| op.settled && op.send_count == 1 && op.request.attempt == 1)
        );
        assert_eq!(
            tools
                .iter()
                .filter(|op| op.state == OperationState::Failed)
                .count(),
            usize::from(code != "SEARCH_LIMIT")
        );
    }
}

pub async fn repair_stops<S: ControlStore>(store: Arc<S>) {
    for mode in [
        "count",
        "elapsed",
        "mixed",
        "budget",
        "input_limit",
        "no_progress",
        "no_progress_schema",
        "no_progress_cycle",
    ] {
        let account = store
            .create_account(
                "t",
                Amount::new(if mode == "budget" { 21 } else { 1000 }, 1000),
                None,
            )
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new(
                "t",
                &format!("tool-stop-{mode}"),
                account,
                "edit",
            ))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        let mut policy = StoragePolicy::default();
        if mode == "input_limit" {
            policy.max_edit_bytes = 128;
        }
        let data = DataStore::memory(policy);
        let access = Access::owner(Namespace {
            owner: run.clone(),
            agent: "main".into(),
        });
        let file = data
            .write(&access, "a", None, b"alpha alpha")
            .await
            .unwrap();
        let bad = json!({"expected_revision":1,"edits":[{"path":"a","expected_digest":file.hash,"old_text":"alpha","new_text":"gamma"}]});
        let first = match mode {
            "mixed" | "no_progress_schema" => json!({"expected_revision":"bad","edits":[]}),
            _ => bad.clone(),
        };
        let call = |id: &str, arguments| {
            FakeModel::calls(vec![ToolCall {
                id: id.into(),
                name: "files.exact_replace".into(),
                arguments,
            }])
        };
        let repeated = if mode == "no_progress_schema" {
            first.clone()
        } else {
            bad.clone()
        };
        let mut replies = vec![
            call("first", first),
            call("second", repeated.clone()),
            call("third", repeated.clone()),
            FakeModel::decision(json!({"decision":"keep"})),
        ];
        if mode == "no_progress_cycle" {
            let mut alternate = bad.clone();
            alternate["edits"][0]["new_text"] = json!("delta");
            replies = vec![
                call("a1", bad.clone()),
                call("b1", alternate.clone()),
                call("a2", bad.clone()),
                call("b2", alternate),
                call("a3", bad),
            ];
        }
        let model = FakeModel::scripted(replies);
        let validator = runtrell::assembly::ValidationChain {
            validators: vec![],
            schema: json!({}),
            repair: RepairPolicy {
                max_repairs: matches!(mode, "count" | "mixed").then_some(1),
                max_elapsed: (mode == "elapsed").then_some(0),
                ..RepairPolicy::default()
            },
        };
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: vec![FileTool {
                store: data.clone(),
                access: access.clone(),
                action: FileAction::ExactReplace,
            }],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(validator),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let done = rt.drive(&claim).await.unwrap();
        match mode {
            "no_progress" | "no_progress_schema" | "no_progress_cycle" => assert!(
                matches!(done.state, RunState::Terminal(Terminal::Failed(ref code)) if code.contains("VALIDATION_NO_PROGRESS"))
            ),
            "budget" => assert!(matches!(
                done.state,
                RunState::Terminal(Terminal::BudgetReached(_))
            )),
            "input_limit" => assert!(
                matches!(done.state, RunState::Terminal(Terminal::Failed(ref code)) if code.contains("INPUT_LIMIT"))
            ),
            _ => assert!(
                matches!(done.state, RunState::Terminal(Terminal::Failed(ref code)) if code.contains("VALIDATION_REPAIR_LIMIT"))
            ),
        }
        assert_eq!(
            model.requests.lock().unwrap().len(),
            if mode == "no_progress_cycle" {
                5
            } else if mode.starts_with("no_progress") {
                3
            } else if matches!(mode, "count" | "mixed") {
                2
            } else {
                1
            }
        );
        assert_eq!(data.snapshot(&access).await.unwrap().revision, 1);
        let operations = store.list_operations(&run).await.unwrap();
        assert!(operations.iter().all(|op| op.settled && op.send_count == 1));
        assert_eq!(
            done.checkpoint.unwrap().payload["tool_repair"]["count"]
                .as_u64()
                .unwrap_or(0),
            if mode == "no_progress_cycle" {
                4
            } else if mode.starts_with("no_progress") {
                2
            } else if matches!(mode, "count" | "mixed" | "budget") {
                1
            } else {
                0
            }
        );
    }
}
